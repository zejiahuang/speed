#!/usr/bin/env bash
# Workspace-wide lint, warnings as errors.
set -euo pipefail

cd /mnt/d/4
exec bash scripts/cargo.sh clippy --workspace --all-targets -- -D warnings
