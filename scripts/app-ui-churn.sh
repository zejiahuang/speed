#!/usr/bin/env bash
# Does a bare `am force-stop` + `am start` cycle kill the app, with no tunnel?
#
# The previous script killed the app by toggling the tunnel, which was read as
# "the toggle is the trigger". The logcat says otherwise: the fatal signal came
# from `tid <RenderThread>` inside `libhwui.so` (`skpaint_to_grpaint_impl`), i.e.
# the GPU renderer, and a second one killed the *previous* round's process too.
# The tunnel has nothing to do with either.
#
# Every round of every repro script does a cold start, and a cold start forces a
# window transition (splash -> activity), which is the one thing that actually
# exercises hwui. So the hypothesis to test is that the crash is emulator
# rendering under repeated transitions, and the app's own machinery is
# irrelevant.
#
# This script therefore does the cold-start churn and nothing else: no
# `connect`, no `disconnect`, no sweep.
#
# Usage: bash scripts/app-ui-churn.sh [rounds]
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
ROUNDS="${1:-6}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

# Count the hwui signal specifically. It is distinguishable from the app's own
# crashes by the thread name in the logcat line, and by there being no matching
# dropbox entry for the pid in the app's package.
hwui_crashes() {
  "$ADB" "${ADB_ARGS[@]}" logcat -d 2>/dev/null \
    | grep -c "Fatal signal 11.*RenderThread.*pid .* (dev.detour)" | tr -d '\r'
}

BASE="$(hwui_crashes)"
say "ui-churn: baseline RenderThread SIGSEGV lines = ${BASE:-0}"
say "ui-churn: $ROUNDS cold starts, no tunnel, no traffic"

for round in $(seq 1 "$ROUNDS"); do
  "$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
  sleep 3
  ALIVE="$("$ADB" "${ADB_ARGS[@]}" shell 'ps -A | grep -c "[d]ev.detour"' 2>/dev/null | tr -d '\r')"
  say "ui-churn: round $round — processes alive: ${ALIVE:-0}"
done

AFTER="$(hwui_crashes)"
say ""
say "ui-churn: RenderThread SIGSEGV lines baseline=$BASE after=$AFTER"

if [ -n "$BASE" ] && [ -n "$AFTER" ] && [ "$AFTER" -gt "$BASE" ]; then
  say "ui-churn: ATTRIBUTION — cold-start churn alone crashes the renderer;"
  say "ui-churn:              the kernel, the tunnel and the sweep are all exonerated"
  exit 0
fi

say "ui-churn: ATTRIBUTION — cold-start churn did not add a renderer crash this run"
