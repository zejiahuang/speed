#!/usr/bin/env bash
# Run the stack unit tests inside WSL.
#
# Written as a file rather than a one-liner because the invocation goes through
# wsl.exe, which strips `$` from anything on the command line. See
# scripts/run-in-wsl.sh for the full explanation.
set -euo pipefail

cd /mnt/d/4
exec bash scripts/cargo.sh test -p watt-stack --lib "$@"
