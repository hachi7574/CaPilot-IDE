# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CaPilot IDE — an **Agentic IDE** (not an ordinary code editor): a Tauri v2 desktop workbench for creating, orchestrating, and monitoring multiple AI coding agents. Each agent runs as a real PTY terminal rendering its CLI's TUI (Claude Code, bash). A bottom **Composer** is the unified smart-input layer. Master/worker orchestration is done via a PATH shim (`capilot` command) + Unix socket. A hardware ESP32 remote (BLE-NUS) is the secondary control surface.

Documentation lives in `docs/` — **read `docs/CaPilot-IDE-RUNBOOK.md` (run/maintenance, known debts, security notes) and `docs/CaPilot-IDE-DevPlan.md` (architecture, module map, interaction spec, milestones) before deep work.** `docs/CaPilot-PRD.md` is the product requirement. Don't duplicate what's already written there.

## Commands

```bash
pnpm install            # frontend deps (repo root)
pnpm tauri dev          # dev mode (needs claude CLI; Linux system deps below)
pnpm tauri build        # package
cargo test              # Rust unit tests — run INSIDE src-tauri/ (currently 24)
cargo test <name>       # single test by name filter (e.g. cargo test settings_kv)
pnpm tsc --noEmit       # TypeScript type check (repo root)
```

- Linux system deps (one-time): `libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`
- Verification loop: `cargo check`/`cargo test` (in `src-tauri/`) + `pnpm tsc --noEmit`. Frontend-only changes still need `cargo check` if they touch the invoke surface.

## Architecture (big picture)

### Two layers: Rust core + React frontend

- **`src-tauri/`** — Tauri v2 (Rust). All `invoke` commands live in `src-tauri/src/lib.rs` (the entire IPC surface: `agent_*`, `sessions_*`, `fs_*`, `git_*`, `setting_*`, `esp_*`, `resource_*`). State: `PtyManager` + `Persistence` + `Dispatcher` are managed as `Arc` state.
- **`ui/`** — React 19 + zustand (`ui/state/store.ts` is the single store). Tauri `Channel` streams PTY bytes to `XTermPanel`.

### Agent runtime abstraction

`src-tauri/src/agent_runtime/adapter.rs` defines `AgentRuntimeAdapter` — **each CLI is one file in `runtimes/`** (`claude.rs`, `bash.rs`; registry in `runtimes/mod.rs`). Adapters only build `(cmd, args)`; **all PTY lifecycle lives in `PtyManager` (`pty.rs`)**, which owns spawn/resume/kill, a natural-exit callback (`on_exit`), and `kill_all` (app-quit cleanup). Key rule: fresh spawn = no auto-resume; resume is explicit and carries the session's stored `resume_key` + `mode/speed/model`.

### Session lifecycle (recently reworked — read `docs/CaPilot-IDE-DevPlan.md` §6.3)

- Persistence: single top-level DB `~/CaPilot/sessions.db` (`sessions` table + `settings` KV table) + per-agent `~/CaPilot/workspaces/<project>/agents/<id>/.agent-meta.json` (dual-written with the DB). `sessions.db` is the source of truth.
- Sessions survive restart: `ui/state/session.ts` `useSessionRestore` re-adds them (skipping `done`), and opening a tab lazily `agent_resume`s via `build_and_spawn`.
- `settings.session_end_mode` (`keep` default / `delete`) controls what happens on natural process exit: `keep` marks `done` (sidebar "已结束" group, recoverable); `delete` removes row + agent dir.
- Composer's permission/speed/model controls follow the **active session** (`agent_set_session_config`), persisted for next resume; they also remember the "next spawn" global default.
- On app quit, `RunEvent::ExitRequested` → `pty.kill_all()` (sessions stay `running` = recoverable).

### Master/worker orchestration

`src-tauri/src/orchestration/` — `dispatcher.rs` (worker pool, `capilot` socket commands), `shim.rs` (installs `~/CaPilot/bin/capilot` on PATH), `smart_return.rs`. Workers are ordinary sessions marked `role: worker`; dispatch injects the prompt into the worker's PTY (`pty_write`). No headless spawn path is implemented.

### Frontend layout

`ui/components/layout/` — `LeftSidebar` (project/terminal tree, pinned Master group, "已结束" groups), `MainArea` (`TabBar` + `ContentArea`), `RightSidebar` (overview/file tree/Git panel, master report), `Composer`, `StatusBar`, `SettingsModal`. `XTermPanel` renders the PTY via xterm.js and is where resume-on-open happens. Git panel is a VSCode-SCM-style panel (`git_*` commands, frontend 2.5s polling — no native file watcher).

## Important constraints & gotchas

- **No agent hooks, no managed home dirs.** Don't install hooks into agent CLIs or create `~/.codex`/`.pi`-style directories. The IDE reads `~/.claude/projects/` read-only; session identity comes from the IDE's own sqlite, not from agent-side hooks. (This is a deliberate divergence from orca.)
- **`cargo test` must run from `src-tauri/`** (Cargo.toml lives there; the repo root has none).
- **`~/.claude/projects/<cwd-encoded>` encoding**: Claude's project dir replaces *every* non-`[a-zA-Z0-9]` char with `-` (leading `/` included) — `claude.rs` `claude_project_key` must match exactly.
- **Terminal perf (WebKitGTK)**: the codebase has hard-won fixes against software-compositor repaint cost (no CSS cursor blink animation, bounded xterm resize loop, no reactive activity timestamps in the store). Keep hot paths cheap.
- **`master` project is pinned/never deletable**; `removeProject`/`sleepProject` group agents by cwd → custom-rooted (git-clone/picked-folder) projects need the `projectRoots` map, not `workspaces/<name>` matching.
- **Wayland limitation (this machine)**: screen-off blocks mss/XTest, so UI automation is unreliable; verify via code reading + CLI/tests.
- **Security**: `agent_write`/`esp_send` are high-privilege; `fs_*`/`git_*` scope tightening is a pre-release checklist item (see `docs/security-review.md`).

## Working style

- The user often runs Claude autonomously at night: don't interrupt with questions, keep the decided architecture/layout unchanged, and leave a **Chinese handover** for the next session.
- Keep changes within the existing mechanisms (sqlite + PtyManager + zustand store) — don't reach for new dependencies or agent-side integration without being asked.
