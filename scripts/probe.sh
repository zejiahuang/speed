#!/usr/bin/env bash
# Resolve dependency versions from the registry rather than guessing them, then
# print the parts of the dependency API this crate actually depends on.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR/core-rs"

if [ "${1:-}" = "add" ]; then
  shift
  cargo add -p watt-stack "$@"
  exit 0
fi

if [ "${1:-}" = "inspect" ]; then
  shift
  CRATE="${1:?usage: probe.sh inspect <crate>}"
  VER="$(cargo metadata --format-version 1 --no-deps >/dev/null 2>&1; cargo tree -p watt-stack --depth 1 2>/dev/null | grep -oE "${CRATE} v[0-9.]+" | head -n 1 | awk '{print $2}')"
  SRC="$HOME/.cargo/registry/src"
  DIR="$(find "$SRC" -maxdepth 2 -type d -name "${CRATE}-${VER}" | head -n 1)"
  echo "crate: $CRATE $VER"
  echo "source: $DIR"
  find "$DIR/src" -maxdepth 2 -name '*.rs' | sort
  exit 0
fi

echo "usage: probe.sh add <crate>... | probe.sh inspect <crate>" >&2
exit 2
