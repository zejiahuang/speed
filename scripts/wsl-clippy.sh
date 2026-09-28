#!/usr/bin/env bash
# Lint the crates changed this round, with warnings as errors.
set -euo pipefail

cd /mnt/d/4
exec bash scripts/cargo.sh clippy -p watt-stack -p watt-ffi --all-targets -- -D warnings
