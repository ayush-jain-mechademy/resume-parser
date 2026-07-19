#!/usr/bin/env bash
#
# End-to-end setup for resume-parser. Idempotent and safe to re-run.
#   1. Installs the Rust toolchain (rustup) if missing
#   2. Installs OS build dependencies (compiler, OpenSSL, pkg-config)
#   3. Compiles the optimized release binary
#   4. Seeds a .env for your GEMINI_API_KEY
#
# Usage:  ./setup.sh      (also works as: bash setup.sh  or  sh setup.sh)
#
# Ubuntu's /bin/sh is dash, which lacks `set -o pipefail` and other bashisms.
# If we weren't started by bash, re-exec under bash so it works either way.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail
cd "$(dirname "$0")"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }

OS="$(uname -s)"

# --- 1. OS build dependencies (FIRST: rustup + cargo need curl and a C toolchain) ---
case "$OS" in
  Linux)
    say "Ensuring Linux build dependencies (C compiler, OpenSSL, pkg-config, curl)…"
    if command -v apt-get >/dev/null 2>&1; then
      sudo apt-get update -y
      sudo apt-get install -y build-essential pkg-config libssl-dev curl ca-certificates
    elif command -v dnf >/dev/null 2>&1; then
      sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config openssl-devel curl ca-certificates
    elif command -v yum >/dev/null 2>&1; then
      sudo yum install -y gcc gcc-c++ make pkgconfig openssl-devel curl ca-certificates
    elif command -v pacman >/dev/null 2>&1; then
      sudo pacman -Sy --needed --noconfirm base-devel openssl pkgconf curl ca-certificates
    else
      warn "Unknown package manager — ensure a C compiler, OpenSSL dev headers, pkg-config and curl are installed."
    fi
    warn "Note: legacy .doc/.rtf parsing uses macOS 'textutil' and is unavailable on Linux (those files are skipped gracefully; PDF/DOCX/TXT work everywhere)."
    ;;
  Darwin)
    if ! xcode-select -p >/dev/null 2>&1; then
      say "Installing Xcode Command Line Tools (follow the GUI prompt, then re-run this script)…"
      xcode-select --install || true
    else
      say "Xcode Command Line Tools present."
    fi
    ;;
  *)
    warn "Unrecognized OS '$OS' — attempting to build anyway."
    ;;
esac

# --- 2. Rust toolchain (need >= 1.85 for edition 2024) ----------------------
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# True only if a rustc >= 1.85 is on PATH (edition 2024 requirement).
rust_ok() {
  command -v rustc >/dev/null 2>&1 || return 1
  ver="$(rustc --version 2>/dev/null | awk '{print $2}')"
  major="$(printf '%s' "$ver" | cut -d. -f1)"
  minor="$(printf '%s' "$ver" | cut -d. -f2)"
  case "$major" in ''|*[!0-9]*) return 1 ;; esac
  case "$minor" in ''|*[!0-9]*) return 1 ;; esac
  [ "$major" -gt 1 ] && return 0
  [ "$major" -eq 1 ] && [ "$minor" -ge 85 ] && return 0
  return 1
}

if rust_ok; then
  say "Rust $(rustc --version | awk '{print $2}') found."
elif command -v rustup >/dev/null 2>&1; then
  say "Rust is missing or too old for edition 2024 — updating via rustup…"
  rustup update stable && rustup default stable
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
else
  say "Installing the Rust toolchain via rustup (non-interactive)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

if ! rust_ok; then
  warn "Rust >= 1.85 is required (edition 2024); found: $(rustc --version 2>/dev/null || echo none)."
  warn "Fix with:  rustup update stable   (install rustup from https://rustup.rs)"
fi

# --- 3. Build ----------------------------------------------------------------
say "Building the optimized release binary (first build downloads crates; a few minutes)…"
cargo build --release

BIN="./target/release/resume-parser"
[ -x "$BIN" ] && say "Built: $BIN"

# --- 4. .env -----------------------------------------------------------------
if [ ! -f .env ]; then
  cp .env.example .env
  warn "Created .env — open it and set GEMINI_API_KEY (get one at https://aistudio.google.com/apikey)."
else
  say ".env already present."
fi

cat <<EOF

──────────────────────────────────────────────
 Setup complete.

 1) Add your key:      edit .env  ->  GEMINI_API_KEY=...
 2) Run on a folder:   ./run.sh /path/to/resumes
    (or)               $BIN /path/to/resumes -o candidates.xlsx

 Outputs: candidates.xlsx (+ .csv, .json, .metrics.json)
──────────────────────────────────────────────
EOF
