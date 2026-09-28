#!/usr/bin/env bash
# Build the debug APK and capture the full log where the host can read it.
#
# The plain `wsl-app-build.sh` streams to the terminal, which is awkward to
# inspect after the fact and impossible to read from a redirected pipe (wsl.exe
# writes UTF-16 and the tail is lost). This writes a UTF-8 log on the Windows
# filesystem instead.
set -uo pipefail

cd /mnt/d/4 || exit 1
export SKIP_NATIVE=1
LOG=/mnt/d/4/build-ui.log
bash scripts/app-build.sh assembleDebug >"$LOG" 2>&1
echo "EXIT=$?" >>"$LOG"
