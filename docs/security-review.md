# CaPilot IDE — Security Review

Scope: `src-tauri/tauri.conf.json` (CSP / bundle), `src-tauri/capabilities/default.json`,
and the app-defined IPC surface in `src-tauri/src/lib.rs` (`fs_*`, `git_*`, `agent_*`).
Status: review snapshot — recommendations are **not yet applied** unless noted. No app security
behavior was changed during this pass.

---

## 1. CSP analysis (`app.security.csp`)

Current:

```
default-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; img-src 'self' data: asset: https://asset.localhost; script-src 'self' 'unsafe-eval'
```

| Directive | Assessment |
|---|---|
| `default-src 'self'` | Good baseline — everything not listed falls back to same-origin only. |
| `script-src 'self' 'unsafe-eval'` | **Main looseness.** `'unsafe-eval'` permits `eval()` / `new Function()`. A scan of the production bundle (`dist/`) found **no** `eval`/`new Function` usage; Vite + React production builds do not need it. It is only potentially useful to some dev-time tooling. Removing it hardens the app so that an XSS cannot `eval` attacker-supplied JS even with `'self'`. Tradeoff: you must confirm `pnpm tauri dev` (HMR) still works — Vite uses native ES modules + `import.meta.hot`, so it should; if any dev tool needs it, keep it only in `app.security.devCsp`, not the shipped `csp`. |
| `style-src 'self' 'unsafe-inline' https://fonts.googleapis.com` | `'unsafe-inline'` is needed by React inline styles / CodeMirror — keep. The `https://fonts.googleapis.com` allowance is **unnecessary**: all fonts are self-hosted via `@font-face` in `ui/App.css` (`/fonts/*.ttf`). Drop it. |
| `font-src 'self' https://fonts.gstatic.com` | Same — Google Fonts are not loaded; `https://fonts.gstatic.com` is dead allowance. Drop it. |
| `img-src 'self' data: asset: https://asset.localhost` | Reasonable for an IDE (icons, `data:` URIs, Tauri v2 asset protocol). Keep. |
| Missing directives | `connect-src`, `object-src`, `base-uri`, `form-action`, `frame-src`, `worker-src` all fall back to `default-src 'self'`. Add `object-src 'none'`, `base-uri 'none'` (or `'self'`), `form-action 'self'`, and an explicit `connect-src 'self'`. Note: the updater's HTTP requests run in Rust (`reqwest`), not the webview, so CSP does not constrain them. Dev HMR websocket (`ws://localhost:1420`/`1421`) is the only thing that could be affected by a strict `connect-src` in dev. |

Recommended hardened CSP (for release):

```
default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data: asset: https://asset.localhost; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-src 'none'
```

Verdict: not critically loose (strong `default-src`, no remote script origins), but `'unsafe-eval'`
and the unused Google Fonts origins should be removed before release.

---

## 2. Capability permissions (`capabilities/default.json`)

Current grants: `core:default`, `opener:default`, `updater:default`, `notification:default`,
`dialog:default`, `store:default`, `log:default`, `process:default`.

| Permission | Necessary? | Notes |
|---|---|---|
| `core:default` | Yes | Baseline Tauri core. |
| `updater:default` | Yes (scaffolding) | Exposes `check`/`download`/`install` to the webview. Endpoint is a placeholder today, so calls fail harmlessly. Keep for scaffolding; must be backed by a real endpoint + pubkey before release. |
| `notification:default` | Yes | Required by the `notify` command. |
| `store:default` | Yes | Persistent settings KV store. |
| `log:default` | Yes (dev) | Logger; can be trimmed for release. |
| `process:default` | Low risk | App `exit`/`restart` only; does **not** include `kill-child` (correctly absent). |
| `dialog:default` | Partially | Grants open/save/message/ask. The UI does not call dialogs yet (planned for folder pickers). Slightly over-granted today; scope down to `dialog:allow-open` (+ `dialog:allow-save` if needed) once the feature set is final. |
| `opener:default` | Partially | Broad: opens any `http(s)`/`mailto:`/`tel:` URL plus unscoped `open_path`. If the IDE only needs "reveal in file manager", scope to `opener:allow-reveal-item-in-dir` and an URL allowlist. |

No high-risk permission (shell, broad `fs`, `process:default-kill`) is granted.

**Important architectural note:** app-defined commands (`agent_*`, `fs_*`, `git_*`) are
implicitly available to any window covered by a capability — Tauri does **not** require (or allow)
per-command ACL entries for application commands. The capability file therefore only governs
plugin/core permissions. The real trust boundary is "the bundled frontend is trusted"; an XSS in
the main window equals full app control. This is inherent to the single-window design and is the
biggest risk driver — mitigation is defense-in-depth (strict CSP, no remote content), not
capabilities.

---

## 3. Path whitelisting in `fs_read` / `fs_write` / `fs_list` (lib.rs)

Pattern: `canonicalize()` → `starts_with($HOME)` → operate.

- **Read / list are sound.** `canonicalize` resolves symlinks in every component, and
  `Path::starts_with` is component-wise (no `/home/hachi2` prefix false-positive). A path like
  `$HOME/link-to-etc/...` resolves outside `$HOME` and is rejected.
- **`fs_write` has a concrete symlink-traversal fallback bug.** If the target file does not
  exist, `canonicalize()` fails and the code falls back to the **raw, un-resolved path**
  (`unwrap_or_else(|_| PathBuf::from(&path))`), then only checks whether that raw string
  textually starts with `$HOME`. `std::fs::write` then traverses symlinked parent directories.
  Concretely, `$HOME/Projects/malicious-repo/symlink-to-etc/new.txt` (where `symlink-to-etc`
  → `/etc`) passes the check and writes to `/etc/new.txt`. Because the IDE operates on
  user-cloned repos and agent workspaces, a malicious repo can plant such a symlink and induce
  an escape.
- **TOCTOU.** Between the `canonicalize` check and the read/write there is a race; a local
  attacker able to swap a symlink in that window defeats the check. Lower severity for a
  single-user desktop app, but real.
- **Scope breadth.** `fs_write` can write anywhere under `$HOME`, including dotfiles
  (`~/.bashrc`, `~/.ssh/...`). By design for an IDE, but combined with an XSS it is a privilege
  escalation path worth flagging.
- Suggested fix: canonicalize the deepest existing ancestor (parent dir) and rejoin the final
  component, or open with `O_NOFOLLOW`, or reject paths whose `canonicalize` fails (no
  auto-fallback), or migrate to `tauri-plugin-fs` with a `$HOME` scope which handles this safely.

---

## 4. IPC exposure (app commands)

All commands below are callable by any frontend code in the main window with no per-command
scoping:

| Command | Risk |
|---|---|
| `agent_write(id, data, raw)` | **High.** Injects text / keystrokes into any running agent PTY. With a `bash`-runtime session this is arbitrary command execution in that session; `id` is caller-supplied with no session-ownership check. Mitigation: validate `id` against persisted sessions. |
| `fs_write` / `fs_read` / `fs_list` | Arbitrary files under `$HOME` (see §3). |
| `git_*` (`git_status`, `git_stage`, `git_commit`, `git_pull`, `git_push`, …) | Accept an arbitrary `repo` path, **not** validated to be a known workspace. A compromised frontend can run git in any readable directory (e.g., `git push` from `/etc`). Low–moderate. |
| `agent_spawn` | Spawns new PTY sessions (including `bash`); a compromised frontend can spawn shells or exhaust resources. By design; consider a session cap. |
| `sessions_delete` | Removes an agent workspace dir; path is derived from persisted data — OK. |
| `notify`, `resource_history` | Low risk. |

---

## 5. Hardening checklist (before release)

1. **Updater**: replace the placeholder endpoint + empty `pubkey` with a real HTTPS endpoint
   and signing key; set `bundle.createUpdaterArtifacts` and sign artifacts (`tauri signer`).
   An empty `pubkey` means signature verification is skipped — never ship that.
2. **CSP**: apply the hardened CSP above; remove `'unsafe-eval'` and Google Fonts origins; keep
   any dev-only eval allowance in `devCsp`. Re-test `pnpm tauri dev` HMR.
3. **Code signing**: macOS Developer ID + notarization; Windows Authenticode; Linux GPG-sign
   deb/rpm.
4. **Dev-only surfaces**: disable devtools in release builds; ensure `withGlobalTauri` stays
   `false`; never point `frontendDist` at remote content.
5. **Fix `fs_write`** symlink/TOCTOU issue (§3); prefer `tauri-plugin-fs` with `$HOME` scope.
6. **Scope plugin permissions** down (`dialog:default`, `opener:default`) once the UI feature
   set is final.
7. **Validate command inputs**: session `id` checks in `agent_write`/`agent_kill`/`agent_resize`;
   length/schema checks for `agent_spawn`; restrict `git_*` repos to known
   workspace roots.
8. Keep `dangerousDisableAssetCspModification` unset (CSP stays enforced on assets).

---

## 6. ACP transport threat surface (Phase 3, 2026-08-17)

Dual-track Agent Client Protocol sessions (`runtime` id `acp:<name>`, e.g. `acp:opencode`)
do **not** use portable-pty. They spawn an ordinary child process and speak JSON-RPC 2.0
NDJSON on stdio. New IPC / host surfaces:

| Surface | Risk | Mitigation (implemented) |
| --- | --- | --- |
| `acp_prompt` / `acp_cancel` / `acp_respond_permission` | Medium — drive agent turns and permission outcomes | Same trust model as other app commands (bundled FE trusted). Backend rejects `agent_write` / resize on ACP ids. Cancel is a **notification** (no JSON-RPC `id`) per OpenCode. |
| Agent→client `session/request_permission` | High if auto-allowed | **Default ask:** Host never auto-allows; emits `PermissionRequest` UI event; turn waits until `acp_respond_permission` (`allow` / `reject` / `cancelled`). auto/yolo deferred. |
| Agent→client `fs/read_text_file` | High (path escape) | `fs_sandbox::resolve_under_root`: absolute path required; `canonicalize` + must stay under session `cwd`; symlink escape rejected; 2 MiB size cap; optional line/limit slice. Out-of-root → JSON-RPC error `-32001`. |
| Agent→client `fs/write_text_file` | Critical if enabled | **Disabled:** `clientCapabilities.fs.writeTextFile=false` at `initialize`; Host still answers any write request with error `-32003` (`WriteDisabled`). |
| `terminal/*` | Critical | Not advertised (`terminal: false`); unknown agent→client methods with `id` get `-32601`. |
| Child env / config pollution | Medium | ACP spawn does **not** inject `OPENCODE_TUI_CONFIG` / status-plugin dirs; descriptor env is explicit only. |
| PTY / ACP mix-up | Medium (UX + security confusion) | Runtime ids `acp:*` forked **before** `get_adapter`; UI `isAcpRuntime`; Composer must not `agent_write` on ACP tabs. |

### Tests covering sandbox / permission
- Unit: `agent_runtime::acp::fs_sandbox::*` (relative reject, outside reject, symlink escape, write disabled, line slice).
- Integration (mock agent): permission allow/reject roundtrip; `fsread:<path>` under cwd OK / outside denied.

### Residual / follow-ups
- OpenCode may execute tools **inside the agent process** without client `fs/*` or `request_permission` (observed in Phase 0 probe). Host policy still applies when the agent *does* call client methods; in-process agent tools are bounded by the OS user of the CaPilot process (same as PTY CLIs).
- auto/yolo permission modes not implemented — keep ask until product policy is reviewed.
- Consider per-session audit log of permission outcomes and fs denials.
