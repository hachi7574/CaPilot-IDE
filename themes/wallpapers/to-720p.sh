#!/usr/bin/env bash
# Convert a video to a wallpaper-friendly H.264 clip:
#   720p (never upscale), no audio, yuv420p, short DPB.
# Usage (Linux / Git Bash / WSL):
#   ./to-720p.sh <video>
# Output is written next to this script, named <stem>-720p.mp4.
set -euo pipefail

usage() {
  echo "usage: ./to-720p.sh <video>" >&2
  echo "  writes <stem>-720p.mp4 next to this script" >&2
  exit 2
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] || [ $# -ne 1 ]; then
  usage
fi

# Resolve this script's directory even when invoked via a relative path.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# Accept Windows paths (B:\foo\bar.mp4) when running under Git Bash / MSYS.
src=${1//\\//}

if [ ! -f "$src" ]; then
  echo "error: not a file: $1" >&2
  exit 1
fi

find_ffmpeg() {
  if command -v ffmpeg >/dev/null 2>&1; then
    command -v ffmpeg
    return 0
  fi
  # Common Windows locations when PATH is a stripped Git-Bash env.
  local candidate
  for candidate in \
    "/c/ffmpeg/bin/ffmpeg.exe" \
    "/c/Program Files/ffmpeg/bin/ffmpeg.exe" \
    "/c/Program Files (x86)/ffmpeg/bin/ffmpeg.exe"
  do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

if ! ffmpeg_bin=$(find_ffmpeg); then
  echo "error: ffmpeg not found on PATH." >&2
  echo "  Linux:  sudo apt install ffmpeg   (or your distro equivalent)" >&2
  echo "  Windows: install ffmpeg and reopen the terminal" >&2
  echo "           https://ffmpeg.org/download.html" >&2
  exit 1
fi

stem=$(basename -- "$src")
stem=${stem%.*}
out="$script_dir/${stem}-720p.mp4"

if [ -e "$out" ]; then
  echo "error: already exists: $out" >&2
  echo "  remove it first if you want to overwrite" >&2
  exit 1
fi

# -2 keeps width even (required by yuv420p). min(720,ih) never upscales.
# -an drops the audio track. -refs 2 shrinks the decoder picture buffer
# a bit versus the H.264 High-profile default.
echo "in : $src"
echo "out: $out"
"$ffmpeg_bin" -hide_banner -nostdin -y \
  -i "$src" \
  -map 0:v:0 \
  -an \
  -vf "scale=-2:'min(720,ih)'" \
  -c:v libx264 -profile:v high -pix_fmt yuv420p \
  -preset medium -crf 20 -refs 2 \
  -movflags +faststart \
  "$out"

echo "done: $out"
