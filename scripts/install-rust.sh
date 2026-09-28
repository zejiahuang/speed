#!/usr/bin/env bash
# Ensure a usable Rust toolchain exists and is set as default.
set -euo pipefail

export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export PATH="$CARGO_HOME/bin:$PATH"

log() { printf '\n==> %s\n' "$*"; }

log "state before"
ls -l "$CARGO_HOME/bin" 2>/dev/null || echo "no cargo bin directory"
rustup toolchain list 2>/dev/null || echo "rustup not runnable yet"

if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
  log "installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
  sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable --no-modify-path
fi

log "ensuring stable toolchain"
# An interrupted rustup run leaves a toolchain directory behind that has no
# manifest and cannot run rustc. Detect that and repair it instead of trusting
# `rustup toolchain list`, which still reports the broken toolchain as installed.
toolchain_healthy() {
  "$CARGO_HOME/bin/rustc" --version >/dev/null 2>&1
}

if ! toolchain_healthy; then
  log "stable toolchain is missing or damaged, reinstalling"
  rustup toolchain uninstall stable || true
  rm -rf "$RUSTUP_HOME/tmp"
  rustup toolchain install stable --profile minimal --force
fi
rustup default stable

log "components and targets"
rustup component add clippy rustfmt || true
rustup target add aarch64-linux-android x86_64-linux-android || true

log "state after"
rustup show
rustc --version
cargo --version
