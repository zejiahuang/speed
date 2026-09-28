#!/usr/bin/env bash
# Try to reproduce the double-free crash that killed the app mid-sweep.
#
# The crash was a heap corruption whose visible victims were scattered —
# `drop_glue<Planner>` inside `nativeFree`, `RawVec<IpAddr>::grow_one`, the rule
# set's `ip_index` rehash. They shared one cause: `Engine.close()` and
# `Proxy.close()` used a plain `var handle: Long` read-then-zeroed, so a
# teardown racing a failing tunnel let two threads call `nativeFree` on the same
# pointer.
#
# The repro therefore has to do the two things at once, and that is the whole
# point of this script: a loop of connect/disconnect alone was already tried
# (twenty iterations, no crash) and a sweep alone is not a teardown race. It is
# the *combination* — a long-running sweep whose engine then has to come down,
# repeatedly — that exercises both sides of the race.
#
# Usage: bash scripts/app-crash-repro.sh [rounds] [domains-per-sweep]
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
TMP="${TMP:-tmp}"

ROUNDS="${1:-6}"
PER_SWEEP="${2:-40}"

# adb.exe is a Windows program and cannot read POSIX paths. The wrapper below
# converts any argument that looks like one. `MSYS_NO_PATHCONV=1` stops the
# shell from helpfully rewriting `/data/local/tmp/x` into a Windows path on the
# way out, so both halves are needed: one for the argument, one for the option.
export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

# Snapshot the crash counters this package has accumulated so far. `dumpsys
# dropbox` is the authoritative place Android records native crashes; the
# tombstone files under /data/tombstones need root, which `adb shell` does not
# have on this image.
crash_count() {
  "$ADB" "${ADB_ARGS[@]}" shell 'dumpsys dropbox --print 2>/dev/null | grep -c "Package: dev.detour"' 2>/dev/null | tr -d '\r' | tail -1
}

# Is the process we started this round still alive?
#
# By pid, not by package name. A `ps | grep dev.detour` in a loop that stops and
# restarts the app sees whatever process exists *now* — and a round that crashed
# while the next `am start` was already racing the crash dump will show a live
# process, so the round reads as a survivor. That is not hypothetical: it hid a
# real renderer crash at round 3 of `app-toggle-repro.sh`.
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

# --- baseline ----------------------------------------------------------------
BEFORE="$(crash_count)"
say "crash-repro: baseline dropbox entries for $PKG = ${BEFORE:-unknown}"
say "crash-repro: $ROUNDS rounds x $PER_SWEEP domains, tunnel mode"

START="$(date +%s)"
FAILED=0
DIED_AT=""

for round in $(seq 1 "$ROUNDS"); do
  # Bring the app up fresh. A cold start each round also re-runs the rule-set
  # compile, which is one of the places the corrupt heap used to surface.
  "$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
  sleep 2

  control connect
  sleep 3

  PID="$(pid_of)"
  if [ -z "$PID" ]; then
    say "crash-repro: round $round — the app was already gone before the sweep"
    FAILED=1
    DIED_AT="$round/start"
    break
  fi

  say "crash-repro: round $round — pid $PID, tunnel up, sweeping"

  # A short burst of real traffic through the tunnel. This is what puts the
  # engine under load at the same moment the next teardown will arrive.
  bash scripts/app-sweep.sh --jobs 6 --timeout 8 --limit "$PER_SWEEP" \
    --mode tunnel --out "$TMP/repro-sweep-$round.tsv" >/dev/null 2>&1

  # Now tear down while that load is still in flight in the kernel's tables.
  # No settle: the race needs the tunnel to still be busy when it is revoked.
  control disconnect
  control connect
  control disconnect
  sleep 2

  # The pid we started, not "a" process. If the app died mid-round and something
  # restarted it, `running()` would have called that a survivor.
  if alive "$PID"; then
    say "crash-repro: round $round — survived (pid $PID alive after teardown churn)"
  else
    say "crash-repro: round $round — APP DIED during teardown churn (pid $PID gone)"
    FAILED=1
    DIED_AT="$round/teardown"
    break
  fi
done

# --- verdict -----------------------------------------------------------------
ELAPSED=$(( $(date +%s) - START ))
AFTER="$(crash_count)"

say ""
say "crash-repro: elapsed ${ELAPSED}s"
say "crash-repro: dropbox entries before=${BEFORE:-?} after=${AFTER:-?}"

# Re-launch so the caller always leaves a live app behind.
"$ADB" "${ADB_ARGS[@]}" shell "am force-stop $PKG; am start -n $PKG/.MainActivity" >/dev/null 2>&1
sleep 2

if [ "$FAILED" = "1" ]; then
  say "crash-repro: RESULT FAIL — the app stopped at $DIED_AT"
  exit 1
fi

if [ -n "$BEFORE" ] && [ -n "$AFTER" ] && [ "$BEFORE" != "$AFTER" ]; then
  say "crash-repro: RESULT FAIL — a new crash was recorded (${BEFORE} -> ${AFTER})"
  exit 1
fi

say "crash-repro: RESULT PASS — $ROUNDS rounds of load + teardown churn, no crash"
