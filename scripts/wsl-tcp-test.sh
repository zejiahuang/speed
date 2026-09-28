#!/usr/bin/env bash
# Run just the tcp and verify tests inside WSL.
#
# Filtered deliberately. The full `watt-stack` suite drives a real smoltcp
# interface and one of its handshake tests has been observed to sit for over an
# hour when the harness clock is frozen; running everything to check three
# functions wastes the session. The changed surface is the candidate-ordering and
# the dial-timeout selection, both in `tcp`, plus the verdict table in `verify`.
set -euo pipefail

cd /mnt/d/4

# Two invocations because `--` accepts one filter. Single-threaded: these tests
# own an interface each and run their own timers.
bash scripts/cargo.sh test -p watt-stack --lib -- --test-threads=1 tcp::tests:: 2>&1 | tail -40
echo "=============== verify ==============="
bash scripts/cargo.sh test -p watt-stack --lib -- --test-threads=1 verify:: 2>&1 | tail -25
