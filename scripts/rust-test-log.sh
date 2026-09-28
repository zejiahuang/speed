#!/usr/bin/env bash
# Run the Rust workspace tests and capture the summary where the host can read it.
#
# `scripts/cargo.sh` streams to the terminal, which is awkward to inspect from the
# Windows side and loses the tail through a redirected wsl.exe pipe. This writes a
# UTF-8 log on the Windows filesystem instead.
set -uo pipefail

cd /mnt/d/4 || exit 1
LOG=/mnt/d/4/rust-test.log
bash scripts/cargo.sh test --workspace >"$LOG" 2>&1
echo "EXIT=$?" >>"$LOG"
