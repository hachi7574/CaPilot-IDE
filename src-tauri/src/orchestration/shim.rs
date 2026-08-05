//! The `capilot` PATH shim — a real executable the orchestrator installs at
//! `~/CaPilot/bin/capilot`. It connects to the Rust orchestrator over a Unix
//! socket and forwards `dispatch/status/report` commands (DevPlan §5.2).

pub const SHIM_SCRIPT: &str = r#"#!/usr/bin/env bash
# capilot — CaPilot orchestration PATH shim.
# Connects to the Rust orchestrator over a Unix socket and forwards commands:
#   capilot dispatch <worker> <prompt...>
#   capilot status
#   capilot report [<worker>] <summary...>
set -euo pipefail

SOCKET_FILE="${HOME}/.capilot/socket"
if [ -f "$SOCKET_FILE" ]; then
  SOCKET="$(cat "$SOCKET_FILE" | tr -d '[:space:]')"
else
  SOCKET="${XDG_RUNTIME_DIR:-${HOME}/.capilot/run}/capilot-orchestrator.sock"
fi

if [ ! -S "$SOCKET" ]; then
  echo "capilot: orchestrator not running (no socket at $SOCKET)" >&2
  exit 1
fi

CMD="$*"

if command -v socat >/dev/null 2>&1; then
  RESP=$(printf '%s\n' "$CMD" | socat - "UNIX-CONNECT:$SOCKET")
elif command -v nc >/dev/null 2>&1 && nc -h 2>&1 | grep -q -- "-U"; then
  RESP=$(printf '%s\n' "$CMD" | nc -U -w 5 "$SOCKET")
else
  RESP=$(python3 - "$SOCKET" "$CMD" <<'PYEOF'
import socket, sys
sock, cmd = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(sock)
s.sendall((cmd + "\n").encode())
s.shutdown(socket.SHUT_WR)
data = b""
while True:
    chunk = s.recv(4096)
    if not chunk:
        break
    data += chunk
s.close()
sys.stdout.write(data.decode())
PYEOF
)
fi

printf '%s\n' "$RESP"
"#;

/// Write the shim to `~/CaPilot/bin/capilot` and make it executable.
pub fn install_shim() -> std::io::Result<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let bin_dir = std::path::PathBuf::from(&home).join("CaPilot").join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let path = bin_dir.join("capilot");
    std::fs::write(&path, SHIM_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms)?;
    }
    Ok(path)
}
