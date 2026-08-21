#!/usr/bin/env bash
# Uninstall the local CaPilot .deb, rebuild it, and install the new package.
# User data (~/CaPilot, CAPILOT_HOME) is left alone.
#
# Usage (repo root or anywhere):
#   ./reinstall-deb.sh
#   ./reinstall-deb.sh --skip-build    # reinstall the newest already-built .deb
set -euo pipefail

pkg=ca-pilot
bin=capilot-ide

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_dir"

skip_build=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) skip_build=1 ;;
    -h|--help)
      sed -n '2,8p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      echo "usage: $0 [--skip-build]" >&2
      exit 2
      ;;
  esac
done

# Local updater signing key (minisign). CI injects the same via
# TAURI_SIGNING_PRIVATE_KEY; without it `createUpdaterArtifacts` aborts
# after the .deb is already written.
if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ -f "$HOME/.tauri/capilot.key" ]; then
  TAURI_SIGNING_PRIVATE_KEY=$(cat "$HOME/.tauri/capilot.key")
  export TAURI_SIGNING_PRIVATE_KEY
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
fi

# `sudo` needs a TTY for a password prompt. Claude Code `!` commands and
# the agent sandbox have none — fall back to pkexec (desktop polkit dialog).
root() {
  local cmd abs
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
    return
  fi
  if sudo -n true >/dev/null 2>&1; then
    sudo "$@"
    return
  fi
  if [ -t 0 ] && [ -t 1 ]; then
    sudo "$@"
    return
  fi
  if command -v pkexec >/dev/null 2>&1; then
    cmd=$1
    shift
    abs=$(command -v "$cmd") || {
      echo "cannot resolve $cmd" >&2
      exit 1
    }
    echo "==> no sudo TTY; using pkexec (graphical auth)" >&2
    pkexec "$abs" "$@"
    return
  fi
  echo "need root, but no TTY for sudo and pkexec is missing" >&2
  exit 1
}

stop_running() {
  if ! command -v pgrep >/dev/null 2>&1; then
    return 0
  fi
  if pgrep -x "$bin" >/dev/null 2>&1; then
    echo "==> stopping running $bin"
    pkill -x "$bin" || true
    # Give the PTY daemon a moment to drop file locks on the binary.
    for _ in 1 2 3 4 5; do
      pgrep -x "$bin" >/dev/null 2>&1 || return 0
      sleep 0.4
    done
    if pgrep -x "$bin" >/dev/null 2>&1; then
      echo "==> $bin still running, sending SIGKILL"
      pkill -9 -x "$bin" || true
    fi
  fi
}

uninstall_if_present() {
  if dpkg-query -W -f='${Status}\n' "$pkg" 2>/dev/null | grep -q 'install ok installed'; then
    local ver
    ver=$(dpkg-query -W -f='${Version}' "$pkg")
    echo "==> uninstalling $pkg $ver"
    root dpkg -r "$pkg"
  else
    echo "==> $pkg is not installed, skip uninstall"
  fi
}

newest_deb() {
  # Tauri writes src-tauri/target/release/bundle/deb/<name>_<ver>_amd64.deb
  local dir="$script_dir/src-tauri/target/release/bundle/deb"
  if [ ! -d "$dir" ]; then
    echo "no deb bundle dir: $dir" >&2
    return 1
  fi
  # Prefer a real installer .deb, not nested data tarballs.
  local found
  found=$(find "$dir" -maxdepth 2 -type f -name '*.deb' -printf '%T@ %p\n' \
    | sort -nr | awk 'NR==1 { $1=""; sub(/^ /,""); print }')
  if [ -z "${found:-}" ]; then
    echo "no .deb under $dir" >&2
    return 1
  fi
  printf '%s\n' "$found"
}

stop_running
uninstall_if_present

if [ "$skip_build" -eq 0 ]; then
  echo "==> building .deb (pnpm tauri build -b deb)"
  pnpm tauri build -b deb
else
  echo "==> --skip-build: using existing bundle"
fi

deb=$(newest_deb)
echo "==> installing $deb"
root apt-get install --reinstall -y "$deb"

echo "==> installed:"
dpkg-query -W -f='${Package} ${Version} ${Architecture}\n' "$pkg"
command -v "$bin"
echo "done. launch with: $bin"
