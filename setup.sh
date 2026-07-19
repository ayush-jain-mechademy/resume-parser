#!/usr/bin/env bash
#
# End-to-end setup for resume-parser. Idempotent and safe to re-run.
#   1. Installs the Rust toolchain (rustup) if missing
#   2. Installs OS build dependencies (compiler, OpenSSL, pkg-config)
#   3. Compiles the optimized release binary
#   4. Seeds a .env for your GEMINI_API_KEY
#
# Usage:  ./setup.sh
#
set -euo pipefail
cd "$(dirname "$0")"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }

OS="$(uname -s)"

# --- 1. Rust toolchain -------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  # rustup may already be installed but not on PATH in this shell
  if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
fi
if ! command -v cargo >/dev/null 2>&1; then
  say "Installing the Rust toolchain via rustup (non-interactive)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
else
  say "Rust toolchain found: $(cargo --version)"
fi

# --- 2. OS build dependencies -----------------------------------------------
case "$OS" in
  Linux)
    say "Ensuring Linux build dependencies (C compiler, OpenSSL, pkg-config)…"
    if command -v apt-get >/dev/null 2>&1; then
      sudo apt-get update -y && sudo apt-get install -y build-essential pkg-config libssl-dev curl
    elif command -v dnf >/dev/null 2>&1; then
      sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config openssl-devel curl
    elif command -v yum >/dev/null 2>&1; then
      sudo yum install -y gcc gcc-c++ make pkgconfig openssl-devel curl
    elif command -v pacman >/dev/null 2>&1; then
      sudo pacman -Sy --needed --noconfirm base-devel openssl pkgconf curl
    else
      warn "Unknown package manager — ensure a C compiler, OpenSSL dev headers and pkg-config are installed."
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
