# CLAUDE.md

This file provides guidance to coding agents working in this repository.

## What this is

CaPilot IDE is a Tauri v2 desktop workspace for interactive AI coding CLI sessions. Each session runs in a real PTY and renders its CLI's TUI. The application also includes a Composer input layer, file editor, Git tools, and ESP32 remote support.

## Commands

```bash
pnpm install
pnpm tauri dev
pnpm tauri build
cd src-tauri && cargo test
pnpm tsc --noEmit
```

Linux requires `libwebkit2gtk-4.1-dev`, `librsvg2-dev`, `libgtk-3-dev`, `libsoup-3.0-dev`, and `libjavascriptcoregtk-4.1-dev`.

## Architecture

- `src-tauri/` contains the Rust core. Tauri commands live in `src-tauri/src/lib.rs` and cover agent sessions, persistence, files, Git, settings, ESP, and resource history.
- `src-tauri/src/agent_runtime/` contains runtime adapters and the shared PTY lifecycle manager.
- `docs/ai-runtime-references.md` maps each agent CLI's official docs and the integration facts CaPilot hard-codes (keybindings, flags, permission cycles, session layout). Consult it before changing runtime adapters.
- `ui/` contains the React 19 frontend. Zustand state lives in `ui/state/store.ts`, and Tauri channels stream PTY bytes to `XTermPanel`.
- Session state is stored in `~/CaPilot/sessions.db` and per-session `.agent-meta.json` files. Opening a restored tab resumes its saved runtime session lazily.

## Constraints

- Do not create managed agent home directories. Hooks may be installed into third-party agent CLIs only for session-status reporting, and only via per-session invocation flags (e.g. `--settings` / environment on the spawned process) that leave the user's global CLI config and standalone usage untouched.
- Run Cargo commands from `src-tauri/` or pass `--manifest-path src-tauri/Cargo.toml`.
- Claude project-directory encoding replaces every non-alphanumeric character with `-`; keep `claude_project_key` compatible.
- Keep terminal rendering hot paths inexpensive on WebKitGTK.
- Treat `agent_write`, `esp_send`, file operations, and Git operations as high-privilege IPC surfaces.
- Preserve unrelated user changes in a dirty worktree.

See `docs/CaPilot-IDE-RUNBOOK.md` and `docs/security-review.md` for operational and security details.
