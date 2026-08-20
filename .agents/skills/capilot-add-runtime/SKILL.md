---
name: capilot-add-runtime
description: Add a CaPilot IDE agent runtime at v1 (spawn PTY only). Use when adding gemini, cursor, copilot, grok, or any new CLI harness. Do not use for canvas, Git, or Composer TUI automation unless the user asks for v2.
metadata:
  short-description: Fast v1 agent runtime add for CaPilot IDE
---

# Add CaPilot agent runtime (v1)

Ship a new runtime as **a detectable CLI that opens a PTY tab**. Composer types plaintext + Enter. Do not copy claude/codex/dsh/pi adapters.

## v1 is done when

- CLI shows in Settings → Runtimes when on PATH
- Project `+` picker can spawn it
- Tab gets a live PTY; user types in Composer or the TUI
- Unknown ids never spawn `claude`

Explicitly **out of v1** (do not implement unless asked): live permission/model/effort key driving, status hooks, context usage, SGR mouse, `--no-alt-screen` games, resident xterm, resume_key, slash catalogs.

Framebuffer TUIs (OpenCode-like alt-screen) still use v1 spawn. Do not put them in a canvas node. Do not add `isMouseTuiRuntime` unless the user accepts resident-terminal cost.

## Implementation

1. Add a row to `V1_RUNTIMES` in `src-tauri/src/agent_runtime/runtimes/generic.rs` (`id`, display `name`, PATH `binary`, optional extra argv). `known_runtimes()` / `get_adapter()` pick it up. Do not open a 700-line adapter.
2. Add a picker row in `ui/state/store.ts` `DEFAULT_TEMPLATES`. Hide when `available: false` already works.
3. Optional: PNG under `ui/assets/icons/agent-favicons/<id>.png` (Orca pack) or SVG in `brands/`; `DEFAULT_LAUNCH` snippet in Settings. Skip i18n/slash/usage allow-lists.
4. Unknown id → `GenericCliAdapter::for_id` with binary = id. Never Claude.
5. Do not add `if (runtime === "…")` in `Composer.tsx` for v1.

If the CLI needs cwd-only spawn, resume later, or a one-line `preflight`, extend `GenericCliAdapter` or a thin wrapper. If it needs hooks or TUI key scripts, stop and ask — that is v2.

File checklist: `references/v1-checklist.md`.

Composer v1 uses `PLAIN_COMPOSER` in `Composer.tsx` to hide the action bar and `/` catalog globally; do not add per-runtime `if`s there.

## Settings UX (deferred — do not implement in v1)

User design feedback, record here so v2 / a follow-up UI task can ship it:

1. **Version string is short.** Settings → 已安装 already shows a compact token (`5.3.9`, not `GNU bash，版本 5.3.9(1)-release …`). Keep it that way: parse in `executable::short_version` and `shortRuntimeVersion` in SettingsModal. Full banner stays on the `title` tooltip.
2. **Each runtime row can collapse, and has an enable switch to the right of ⚙.**
   - Switch **off** → runtime hidden from the project `+` picker (still listed in Settings so it can be turned back on).
   - Persist the enabled set locally (same pattern as `capilot.termTemplates`).
   - The whole `#settings-runtimes` panel should be collapsible.
   - Do **not** add this in a v1 runtime-add PR; it is a Settings UX task.

## Local install convention

User-local only — never `sudo`, never system npm prefix.

- Node CLIs: `npm i -g --prefix "$HOME/APP/n" <pkg>` (CaPilot `ensure_cli_path` already prepends `~/APP/n/bin`).
- Other user bins: `~/.local/bin` (also on `ensure_cli_path`).
- Inventory + uninstall: `references/local-install.md`.
