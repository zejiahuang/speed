#!/usr/bin/env bash
# Full verification for the racing change: clippy over the workspace with
# warnings denied, then the whole workspace test suite.
#
# The workspace run is wrapped in `timeout` because the tcp::tests module is
# documented to hang when driven as a whole (see scripts/wsl-candidate-test.sh).
set -uo pipefail

cd /mnt/d/4

echo "=================== clippy (workspace, -D warnings) ==================="
bash scripts/cargo.sh clippy --workspace --all-targets -- -D warnings 2>&1 | tail -25
clippy_code=$?
echo "clippy exit: $clippy_code"

echo
echo "=================== workspace test ==================="
out="$(timeout 900 bash scripts/cargo.sh test --workspace 2>&1)"
code=$?
printf '%s\n' "$out" | grep -E "test result|panicked|FAILED|^error|warning:|Compiling watt" | tail -60
echo "workspace test exit: $code (124 means it timed out)"
