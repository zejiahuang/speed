#!/usr/bin/env bash
# Run cargo against the workspace from inside WSL.
#
# Two things matter here:
#   * `CARGO_TARGET_DIR` is redirected out of /mnt/d. The Windows drive is mounted
#     over 9p, and writing tens of thousands of small object files there makes
#     linking several times slower.
#   * the workspace lives on /mnt/d so the source stays visible on the Windows
#     side, which is where the editor and the Android project live.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR/core-rs"

exec cargo "$@"
