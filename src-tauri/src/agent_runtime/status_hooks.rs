use crate::persistence::status_dir;

/// Environment names the status hook script reads. `CAPILOT_AGENT_ID` is set
/// per-session on the spawned agent process; the script writes the agent's
/// lifecycle state to `<CAPILOT_STATUS_DIR>/<agent_id>.json`. Absent → no-op, so
/// an agent run not spawned by CaPilot is never touched.
pub const HOOK_ENV_AGENT: &str = "CAPILOT_AGENT_ID";
pub const HOOK_ENV_DIR: &str = "CAPILOT_STATUS_DIR";

/// The status hook script (`~/CaPilot/status/hook.sh`). Reads the hook event
/// payload on stdin, maps `hook_event_name` to a CaPilot status, and atomically
/// writes the per-agent sidecar. Pure POSIX sh — no runtime dependency, and it
/// is written by the app itself (never injected into the user's own config).
///
/// The event names are the Claude-compatible lifecycle names both claude
/// (`--settings` hook payload) and codex (config-profile TOML hooks) emit, so a
/// single script serves both runtimes unchanged.
pub const STATUS_HOOK_SCRIPT: &str = r#"#!/bin/sh
# CaPilot status hook — reports agent lifecycle state to the IDE sidecar.
# Env is injected per-session by CaPilot (CAPILOT_AGENT_ID / CAPILOT_STATUS_DIR);
# when absent this hook is a no-op (e.g. an agent run not spawned by the IDE).
id="${CAPILOT_AGENT_ID:-}"
dir="${CAPILOT_STATUS_DIR:-}"
[ -n "$id" ] && [ -n "$dir" ] || exit 0
payload=$(cat)
event=$(printf '%s' "$payload" | sed -n 's/.*"hook_event_name":"\([^"]*\)".*/\1/p')
case "$event" in
  SessionStart) st="idle" ;;
  UserPromptSubmit|PreToolUse|PostToolUse|PostToolUseFailure|PostToolBatch) st="working" ;;
  PermissionRequest) st="waiting_input" ;;
  Stop|StopFailure) st="idle" ;;
  SessionEnd) st="dormant" ;;
  *) exit 0 ;;
esac
tmp="$dir/$id.tmp"
printf '{"status":"%s","ts":%s}\n' "$st" "$(date +%s)" > "$tmp"
mv "$tmp" "$dir/$id.json"
"#;

/// The lifecycle events each runtime wires to the status script. `claude`
/// carries a few events codex has no equivalent for (`PostToolUseFailure`,
/// `PostToolBatch`, `StopFailure`) — those stay claude-only, but hook.sh is
/// shared and simply never sees them from codex.
pub const CLAUDE_HOOK_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "PermissionRequest",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// Events codex supports as TOML hooks (codex's `HookEventsToml`). Subset of
/// `CLAUDE_HOOK_EVENTS` minus the claude-only events.
pub const CODEX_HOOK_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];

/// Write (idempotently) the shared status-reporting hook files into
/// `~/CaPilot/status/`: `hook.sh` (the lifecycle→status script) and
/// `hooks.json` (the claude `--settings` payload wired to it). Claude loads
/// these per-invocation only; the user's global `~/.claude/settings.json` and
/// standalone `claude` usage are untouched. Codex references the same `hook.sh`
/// from its per-session config profile.
pub fn ensure_status_hooks() -> std::io::Result<()> {
    let dir = status_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("hook.sh"), STATUS_HOOK_SCRIPT)?;
    // The `--settings` file must reference the script by absolute path.
    let hook_sh = dir.join("hook.sh");
    let handler = serde_json::json!({
        "type": "command",
        "command": "/bin/sh",
        "args": [hook_sh]
    });
    // Every lifecycle event routes to the same status script; a distinct
    // matcher group per event keeps the wiring explicit.
    let mut events = serde_json::Map::new();
    for name in CLAUDE_HOOK_EVENTS {
        events.insert(name.to_string(), serde_json::json!([{ "hooks": [handler.clone()] }]));
    }
    let hooks = serde_json::json!({ "hooks": events });
    std::fs::write(dir.join("hooks.json"), serde_json::to_string_pretty(&hooks)?)?;
    Ok(())
}
