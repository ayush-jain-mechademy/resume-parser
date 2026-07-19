#!/usr/bin/env bash
# Convenience wrapper: build (release) and run the parser on a folder of resumes.
# Usage: ./run.sh <resume-folder> [extra flags...]
set -euo pipefail

FOLDER="${1:-sample_resumes}"
shift || true

if [[ -z "${GEMINI_API_KEY:-}" && ! -f .env ]]; then
  echo "GEMINI_API_KEY is not set and no .env file found." >&2
  echo "  export GEMINI_API_KEY=...   (or copy .env.example to .env)" >&2
  exit 1
fi

cargo build --release
exec ./target/release/resume-parser "$FOLDER" -o candidates.xlsx "$@"
