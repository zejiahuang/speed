#!/usr/bin/env bash
# Run the focused kernel tests inside WSL: the parts changed this round.
#
# A file rather than a one-liner because the command crosses wsl.exe, which
# strips `$` from the command line. See scripts/run-in-wsl.sh.
set -euo pipefail

cd /mnt/d/4

# One test binary at a time, one thread each: the TCP tests drive a real
# smoltcp interface and two instances will interfere with each other's timing.
exec bash scripts/cargo.sh test -p watt-ffi --lib -- --test-threads=1
