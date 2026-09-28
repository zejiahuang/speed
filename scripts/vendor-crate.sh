#!/usr/bin/env bash
# Copy a vendored dependency's source into tmp/vendor so it can be searched with
# the normal file tools instead of reaching into ~/.cargo over WSL.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE="${1:?usage: vendor-crate.sh <crate-name>}"

cd "$REPO_DIR/core-rs"
cargo fetch >/dev/null 2>&1 || true

VERSION="$(cargo tree --workspace --depth 1 2>/dev/null | grep -oE "\b${CRATE} v[0-9]+\.[0-9]+\.[0-9]+" | head -n 1 | awk '{print $2}' | sed 's/^v//')"
if [ -z "$VERSION" ]; then
  echo "could not resolve a version for ${CRATE} in the workspace" >&2
  exit 1
fi

SRC="$(find "$HOME/.cargo/registry/src" -maxdepth 2 -type d -name "${CRATE}-${VERSION}" | head -n 1)"
if [ -z "$SRC" ]; then
  echo "source for ${CRATE}-${VERSION} not found; run 'cargo fetch' first" >&2
  exit 1
fi

DEST="$REPO_DIR/tmp/vendor/${CRATE}-${VERSION}"
rm -rf "$DEST"
mkdir -p "$(dirname "$DEST")"
cp -r "$SRC" "$DEST"
printf 'vendored %s %s -> %s\n' "$CRATE" "$VERSION" "$DEST"
