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
  UserPromptSubmit|PostToolUse|PostToolUseFailure|PostToolBatch) st="working" ;;
  PreToolUse)
    # A question tool blocks on the user picking an option — report it as
    # `awaiting_choice` (待选择), distinct from a plain tool run. claude names it
    # AskUserQuestion; codex's is `tool/requestUserInput` (session items call it
    # `item/tool/requestUserInput`) — the `*requestUserInput` glob covers both.
    # Every other tool call is `working`.
    tool=$(printf '%s' "$payload" | sed -n 's/.*"tool_name":"\([^"]*\)".*/\1/p')
    case "$tool" in
      AskUserQuestion|*requestUserInput) st="awaiting_choice" ;;
      *) st="working" ;;
    esac
    ;;
  PermissionRequest)
    # A question tool surfaced through the permission flow must NOT downgrade to
    # `waiting_input` (待确认): claude fires PreToolUse(AskUserQuestion) then a
    # trailing PermissionRequest, and codex likewise gates requestUserInput —
    # without this check the later event would overwrite `awaiting_choice` back
    # to `waiting_input`. The PermissionRequest payload carries `tool_name` (same
    # shape as PreToolUse), so the same question-tool glob applies. Every other
    # permission prompt is `waiting_input`.
    tool=$(printf '%s' "$payload" | sed -n 's/.*"tool_name":"\([^"]*\)".*/\1/p')
    case "$tool" in
      AskUserQuestion|*requestUserInput) st="awaiting_choice" ;;
      *) st="waiting_input" ;;
    esac
    ;;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Execute the real `hook.sh` against a payload, reading back the status it
    /// wrote to the sidecar. Each call uses a unique temp dir, so the tests can
    /// run in parallel without stepping on each other's env/files.
    fn run_hook(payload: &str) -> Option<String> {
        // pid+nanos can collide when two tests in this process call `run_hook`
        // in the same clock tick — a counter makes each temp dir unique so one
        // test's cleanup can never delete another's mid-write.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "capilot_status_hook_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(base.join("dir")).unwrap();
        let script = base.join("hook.sh");
        std::fs::write(&script, STATUS_HOOK_SCRIPT).unwrap();
        let mut child = Command::new("/bin/sh")
            .arg(&script)
            .env("CAPILOT_AGENT_ID", "test-agent")
            .env("CAPILOT_STATUS_DIR", base.join("dir"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let sidecar = base.join("dir").join("test-agent.json");
        let status = std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("status").and_then(serde_json::Value::as_str).map(str::to_owned));
        let _ = std::fs::remove_dir_all(base);
        status
    }

    #[test]
    fn pretooluse_askuserquestion_reports_awaiting_choice() {
        let payload =
            r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"question":"pick one"}}"#;
        assert_eq!(run_hook(payload).as_deref(), Some("awaiting_choice"));
    }

    #[test]
    fn pretooluse_codex_request_user_input_reports_awaiting_choice() {
        // codex names the tool `tool/requestUserInput` (its session item stream
        // spells it `item/tool/requestUserInput`) — both must land in
        // awaiting_choice, not a plain working.
        for tool_name in ["tool/requestUserInput", "item/tool/requestUserInput"] {
            let payload = format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool_name}","tool_input":{{"question":"pick one"}}}}"#
            );
            assert_eq!(run_hook(&payload).as_deref(), Some("awaiting_choice"));
        }
    }

    #[test]
    fn pretooluse_other_tool_reports_working() {
        let payload =
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert_eq!(run_hook(payload).as_deref(), Some("working"));
    }

    #[test]
    fn permission_request_reports_waiting_input() {
        let payload = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#;
        assert_eq!(run_hook(payload).as_deref(), Some("waiting_input"));
    }

    #[test]
    fn permission_request_askuserquestion_reports_awaiting_choice() {
        // claude gates AskUserQuestion behind a PermissionRequest that arrives
        // after the PreToolUse event — the trailing event must not overwrite
        // awaiting_choice back to waiting_input.
        let payload =
            r#"{"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"question":"pick one"}}"#;
        assert_eq!(run_hook(payload).as_deref(), Some("awaiting_choice"));
    }

    #[test]
    fn permission_request_codex_request_user_input_reports_awaiting_choice() {
        for tool_name in ["tool/requestUserInput", "item/tool/requestUserInput"] {
            let payload = format!(
                r#"{{"hook_event_name":"PermissionRequest","tool_name":"{tool_name}","tool_input":{{"question":"pick one"}}}}"#
            );
            assert_eq!(run_hook(&payload).as_deref(), Some("awaiting_choice"));
        }
    }

    #[test]
    fn pretooluse_then_permission_request_question_keeps_awaiting_choice() {
        // The real-world sequence for a question prompt: PreToolUse (→
        // awaiting_choice) followed by PermissionRequest for the same tool. The
        // second event must leave the status at awaiting_choice, not clobber it
        // to waiting_input.
        let base = std::env::temp_dir().join(format!(
            "capilot_status_hook_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("dir")).unwrap();
        let script = base.join("hook.sh");
        std::fs::write(&script, STATUS_HOOK_SCRIPT).unwrap();
        let run = |payload: &str| {
            let mut child = Command::new("/bin/sh")
                .arg(&script)
                .env("CAPILOT_AGENT_ID", "seq-agent")
                .env("CAPILOT_STATUS_DIR", base.join("dir"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
            drop(child.stdin.take());
            assert!(child.wait().unwrap().success());
            let sidecar = base.join("dir").join("seq-agent.json");
            std::fs::read_to_string(&sidecar)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|v| v.get("status").and_then(serde_json::Value::as_str).map(str::to_owned))
        };
        assert_eq!(
            run(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"}"#).as_deref(),
            Some("awaiting_choice")
        );
        // The trailing PermissionRequest for the same question tool must keep
        // awaiting_choice.
        assert_eq!(
            run(r#"{"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion"}"#).as_deref(),
            Some("awaiting_choice")
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
