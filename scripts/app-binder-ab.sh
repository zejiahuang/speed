#!/usr/bin/env bash
# Isolate whether the binder refcount abort needs the tunnel, or only the load.
#
# Two runs of the same shape, differing in one thing:
#   idle   — start the app, churn connect/disconnect, no traffic at all
#   loaded — start the app, bring the tunnel up, push a burst of real requests
#
# The abort is `decWeak/decStrong called on <ptr> too many times` on a
# BinderProxy, i.e. an ART-level refcount underflow. Nothing in the app's own
# code touches binder (no bindService, no IBinder, no ServiceConnection), so the
# object is one the framework holds on the app's behalf. If the idle run is
# clean and the loaded run aborts, the trigger is the allocation pressure the
# sweep creates; if both abort, the tunnel itself is implicated and the sweep is
# incidental.
#
# Usage: bash scripts/app-binder-ab.sh [rounds]
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
TMP="${TMP:-tmp}"
ROUNDS="${1:-3}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

# The abort message is the fingerprint; matching it is what distinguishes this
# crash from the double free. Counting occurrences rather than reading a count
# from dumpsys, because dropbox prints history and we want "did a new one land".
abort_count() {
  "$ADB" "${ADB_ARGS[@]}" shell \
    'dumpsys dropbox --print 2>/dev/null | grep -c "called on 0x"' 2>/dev/null | tr -d '\r' | tail -1
}

# By pid, not by package name — see `app-crash-repro.sh` for why a package-name
# check reads a crashed round as a survivor.
alive() {
  local want="$1" out
  [ -z "$want" ] && return 1
  out="$("$ADB" "${ADB_ARGS[@]}" shell "ps -A | awk '{print \$2}' | grep -c '^${want}\$'" 2>/dev/null | tr -d '\r')"
  [ "${out:-0}" != "0" ]
}

pid_of() {
  "$ADB" "${ADB_ARGS[@]}" shell 'ps -A | grep "[d]ev.detour" | awk "{print \$2}"' 2>/dev/null | tr -d '\r' | head -1
}

control() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd $1" >/dev/null 2>&1
}

fresh() {
  "$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
  sleep 2
}

# --- arm 1: idle churn, no traffic -------------------------------------------
BASE="$(abort_count)"
say "binder-ab: baseline abort count = ${BASE:-?}"
say "binder-ab: arm 'idle' — $ROUNDS rounds of connect/disconnect, no traffic"

IDLE_DIED=0
for round in $(seq 1 "$ROUNDS"); do
  fresh
  PID="$(pid_of)"
  control connect
  sleep 2
  control disconnect
  control connect
  control disconnect
  sleep 1
  if alive "$PID"; then
    say "binder-ab: idle round $round — alive (pid $PID)"
  else
    say "binder-ab: idle round $round — DIED (pid $PID gone)"
    IDLE_DIED=1
    break
  fi
done

MID="$(abort_count)"
say "binder-ab: after idle arm, abort count = ${MID:-?}"

# --- arm 2: same churn, with real load ---------------------------------------
say ""
say "binder-ab: arm 'loaded' — $ROUNDS rounds of tunnel + traffic + teardown"

LOAD_DIED=0
for round in $(seq 1 "$ROUNDS"); do
  fresh
  PID="$(pid_of)"
  control connect
  sleep 2

  bash scripts/app-sweep.sh --jobs 6 --timeout 8 --limit 40 \
    --mode tunnel --out "$TMP/binder-ab-$round.tsv" >/dev/null 2>&1

  control disconnect
  sleep 2
  if alive "$PID"; then
    say "binder-ab: loaded round $round — alive (pid $PID)"
  else
    say "binder-ab: loaded round $round — DIED (pid $PID gone)"
    LOAD_DIED=1
    break
  fi
done

FINAL="$(abort_count)"
say ""
say "binder-ab: abort count baseline=$BASE after_idle=$MID after_loaded=$FINAL"
say "binder-ab: idle_died=$IDLE_DIED loaded_died=$LOAD_DIED"

fresh

# The verdict is about which arm reproduced, not about pass/fail of the app:
# this script exists to attribute the crash, so a reproduction is a result.
if [ "$IDLE_DIED" = "0" ] && [ "$LOAD_DIED" = "1" ]; then
  say "binder-ab: ATTRIBUTION — load-triggered (the tunnel is not required to be idle-safe, the traffic is the trigger)"
  exit 0
fi
if [ "$IDLE_DIED" = "1" ] && [ "$LOAD_DIED" = "0" ]; then
  say "binder-ab: ATTRIBUTION — churn-triggered (teardown alone suffices)"
  exit 0
fi
if [ "$IDLE_DIED" = "1" ] && [ "$LOAD_DIED" = "1" ]; then
  say "binder-ab: ATTRIBUTION — both arms reproduce; the tunnel is implicated"
  exit 0
fi
say "binder-ab: ATTRIBUTION — neither arm reproduced in $ROUNDS rounds; the trigger is rarer than this"
