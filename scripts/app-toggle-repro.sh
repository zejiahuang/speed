#!/usr/bin/env bash
# Isolate the rapid connect/disconnect toggle as the crash trigger.
#
# Evidence so far:
#   * `app-crash-repro.sh` (sweep + `disconnect; connect; disconnect`) crashed on
#     round 1, twice, both times at ~49-51s of process uptime.
#   * `app-binder-ab.sh` (identical sweep, but only a single `connect; disconnect`
#     pair per round) never crashed in 6 rounds.
#   * The abort is `decStrong() called on <ptr> too many times` from
#     `BinderProxy_destroy`, on the `ReferenceQueueD` daemon — an ART refcount
#     underflow on a binder proxy, not a native free.
#
# The one thing that differs is the *toggling*: bringing the tunnel down and
# straight back up without settling. The tunnel is a VpnService, and
# establish/teardown of a VpnService goes through the framework's binder
# plumbing. A rapid down-up-down is a teardown racing a fresh establishment.
#
# So this script runs the churn *without* any sweep: no kernel load, no rule
# recompile, no request traffic. If it still crashes, the load is irrelevant and
# the toggle itself is the trigger — which is the hypothesis worth testing,
# because it points at the app's own start/stop path rather than at the kernel.
#
# Usage: bash scripts/app-toggle-repro.sh [rounds] [toggles-per-round]
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
ROUNDS="${1:-4}"
TOGGLES="${2:-3}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

abort_count() {
  "$ADB" "${ADB_ARGS[@]}" shell \
    'dumpsys dropbox --print 2>/dev/null | grep -c "called on 0x"' 2>/dev/null | tr -d '\r' | tail -1
}

# By pid, not by package name: a round that crashed while the *next* `am start`
# was already racing the crash dump shows a live process and reads as a
# survivor. That is exactly how this script once reported "round 3 survived"
# for a round whose app had died of a renderer SIGSEGV.
alive() {
  local want="$1" out
  [ -z "$want" ] && return 1
  out="$("$ADB" "${ADB_ARGS[@]}" shell "ps -A | awk '{print \$2}' | grep -c '^${want}\$'" 2>/dev/null | tr -d '\r')"
  [ "${out:-0}" != "0" ]
}

control() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd $1" >/dev/null 2>&1
}

pid_of() {
  "$ADB" "${ADB_ARGS[@]}" shell 'ps -A | grep "[d]ev.detour" | awk "{print \$2}"' 2>/dev/null | tr -d '\r' | head -1
}

BASE="$(abort_count)"
say "toggle-repro: baseline abort count = ${BASE:-?}"
say "toggle-repro: $ROUNDS rounds x $TOGGLES toggles, NO sweep, NO traffic"

DIED=0
DIED_AT=""
START="$(date +%s)"

for round in $(seq 1 "$ROUNDS"); do
  "$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
  sleep 2

  PID="$(pid_of)"
  say "toggle-repro: round $round — pid $PID, toggling"

  for t in $(seq 1 "$TOGGLES"); do
    control connect
    sleep 1
    control disconnect
    # The distinguishing detail: come straight back up with no settle.
    control connect
    control disconnect
    sleep 1
    if ! alive "$PID"; then
      say "toggle-repro: round $round toggle $t — APP DIED (pid $PID gone)"
      DIED=1
      DIED_AT="$round/$t"
      break
    fi
  done
  [ "$DIED" = "1" ] && break
  say "toggle-repro: round $round — survived (pid $PID)"
done

ELAPSED=$(( $(date +%s) - START ))
AFTER="$(abort_count)"

say ""
say "toggle-repro: elapsed ${ELAPSED}s, abort count baseline=$BASE after=$AFTER"

"$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
sleep 2

if [ "$DIED" = "1" ]; then
  say "toggle-repro: ATTRIBUTION — the toggle alone reproduces at $DIED_AT; load is irrelevant"
  exit 1
fi

say "toggle-repro: ATTRIBUTION — the toggle alone did not reproduce in $ROUNDS rounds; load is part of the trigger"
