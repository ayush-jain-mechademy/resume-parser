#!/usr/bin/env bash
# Convenience wrapper: build (release) and run the parser on a folder of resumes.
# Usage: ./run.sh <resume-folder> [extra flags...]
# Re-exec under bash if started by dash/sh (Ubuntu's /bin/sh) so bashisms work.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail
cd "$(dirname "$0")"

# Load cargo/rustup into PATH if this shell hasn't yet (e.g. right after ./setup.sh
# in the same terminal, before ~/.cargo/env has been sourced).
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

FOLDER="${1:-sample_resumes}"
shift || true

if [[ -z "${GEMINI_API_KEY:-}" && ! -f .env ]]; then
  echo "GEMINI_API_KEY is not set and no .env file found." >&2
  echo "  export GEMINI_API_KEY=...   (or copy .env.example to .env and add your key)" >&2
  exit 1
fi

BIN="./target/release/resume-parser"
# Prefer the binary setup.sh already built — no cargo needed to run.
if [ ! -x "$BIN" ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found and no prebuilt binary present." >&2
    echo "  Run ./setup.sh first, then re-run ./run.sh." >&2
    echo "  If you just ran setup.sh in this same terminal, load Rust into PATH first:" >&2
    echo "      source \"\$HOME/.cargo/env\"    (or open a new terminal)" >&2
    exit 1
  fi
  cargo build --release
fi

exec "$BIN" "$FOLDER" -o candidates.xlsx "$@"
