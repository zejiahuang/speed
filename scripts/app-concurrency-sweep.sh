#!/usr/bin/env bash
#
# Find the concurrency level at which the tunnel stops delivering on a REAL host.
#
# Why this exists
# ---------------
# The 512-thread runs answered a different question than the one that matters.
# `--target local` (nothing listening on 127.0.0.1:8080) proved the kernel
# refuses cleanly and fast under 512-way concurrency, but a refusal never
# involves an upstream at all, so it says nothing about forwarding.
# `--target one` (512 threads, all on github.com) produced 100% timeouts -- but
# from a 2-core device, so the timeouts are equally consistent with "the kernel
# is slow" and "512 TLS clients cannot be scheduled on 2 cores".
#
# This sweep separates those. It walks the concurrency up in steps, on one host
# whose verdict is already warm, and reports the success rate at each step. The
# question it answers is: at what concurrency does delivery start to fail, and
# does it fail as refusals (the kernel) or as timeouts (the scheduler)?
#
# Usage:
#   bash scripts/app-concurrency-sweep.sh [--host H] [--levels "1 4 16 64 128"] \
#                                        [--rounds N] [--timeout S]

set -u

DEVICE="${DEVICE:-emulator-5554}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG=dev.detour
WORK="${WORK:-tmp}"

HOST="github.com"
LEVELS="1 4 16 64 128 256"
ROUNDS=3
TIMEOUT=20

while [ $# -gt 0 ]; do
  case "$1" in
    --host)    HOST="$2"; shift 2 ;;
    --levels)  LEVELS="$2"; shift 2 ;;
    --rounds)  ROUNDS="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    *) echo "sweep: unknown option $1" >&2; exit 2 ;;
  esac
done

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

# Git Bash already exports TMP=/tmp; a variable named TMP silently becomes a
# POSIX path that adb.exe cannot read. WORK is the name that does not collide.
mkdir -p "$WORK"

to_win() {
  local path="$1" abs
  abs="$(cd "$(dirname "$path")" 2>/dev/null && pwd)/$(basename "$path")"
  case "$abs" in
    /[a-zA-Z]/*) printf '%s' "$(echo "${abs:1:1}" | tr 'a-z' 'A-Z'):${abs:2}" ;;
    *) printf '%s' "$abs" ;;
  esac
}

control() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd $1" >/dev/null 2>&1
}

pid_of() {
  "$ADB" "${ADB_ARGS[@]}" shell 'pidof dev.detour' 2>/dev/null | tr -d '\r' | head -1
}

##############################################################################
# device-side runner: N concurrent curls, all at the SAME host
##############################################################################
#
# One background curl per slot, each doing a single request and writing its own
# result file. A shared fd splices lines together once the shell gets busy -- the
# 512-thread run produced rows like "13.323316200" from three requests fused --
# so every worker gets its own file and the fold happens after the wait.
cat >"$WORK/sweep-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = concurrency, $2 = timeout, $3 = host, $4 = rounds
conc="$1"
timeout_s="$2"
host="$3"
rounds="$4"

DIR=/data/local/tmp/watt/sweep-parts
rm -rf "$DIR"
mkdir -p "$DIR"

url="https://$host/"

i=1
while [ "$i" -le "$conc" ]; do
  (
    r=1
    while [ "$r" -le "$rounds" ]; do
      out="$(curl -sS -o /dev/null -w "%{http_code} %{time_total}" --max-time "$timeout_s" "$url" 2>/dev/null)"
      rc=$?
      [ -z "$out" ] && out="000 0"
      printf "%s %s\n" "$out" "$rc"
      r=$((r + 1))
    done > "$DIR/w.$i"
  ) &
  i=$((i + 1))
done
wait

for f in "$DIR"/w.*; do
  [ -f "$f" ] && cat "$f"
done
RUNNER

say "sweep: host=$HOST timeout=${TIMEOUT}s rounds=$ROUNDS"
say "sweep: levels=$LEVELS"

if [ -z "$(pid_of)" ]; then
  say "sweep: the app is not running; start it first"
  exit 1
fi

PID="$(pid_of)"
say "sweep: app pid=$PID"

# Tunnel up and settled. Establishing during a measurement measures establishing.
control disconnect
sleep 2
control connect
sleep 5

say "sweep: warming $HOST (one sequential request)"
"$ADB" "${ADB_ARGS[@]}" shell "curl -sS -o /dev/null --max-time 20 https://$HOST/ >/dev/null 2>&1" >/dev/null 2>&1

[ -x "$ADB" ] || { say "sweep: adb not executable at $ADB"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/sweep-runner.sh")" /data/local/tmp/watt/sweep-runner.sh >/dev/null 2>&1 \
  || { say "sweep: FAILED to push the runner"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/sweep-runner.sh' >/dev/null 2>&1

OUT="$WORK/concurrency-sweep.tsv"
: >"$OUT"

DIED=0
DEATHS=0

say ""
printf '%-8s %-10s %-9s %-9s %-9s %s\n' "conc" "requests" "ok" "timeout" "refused" "mean_s"
printf '%-8s %-10s %-9s %-9s %-9s %s\n' "----" "--------" "---" "-------" "-------" "------"

for conc in $LEVELS; do
  "$ADB" "${ADB_ARGS[@]}" shell \
    "sh /data/local/tmp/watt/sweep-runner.sh $conc $TIMEOUT $HOST $ROUNDS" \
    2>/dev/null | tr -d '\r' > "$WORK/sweep-$conc.txt"

  # code time exit -- curl -w emits code and time; the runner appends the exit.
  read -r n ok to refused other mean < <(awk '
    {
      code=$1; t=$2+0; rc=$3
      n++
      if (code ~ /^[0-9]{3}$/ && code != "000") ok++
      else if (code == "000" && rc == "28") to++
      else if (code == "000" && rc == "7") refused++
      else other++
      sum += t
    }
    END { printf "%d %d %d %d %d %.3f\n", n, ok+0, to+0, refused+0, other+0, (n?sum/n:0) }
  ' "$WORK/sweep-$conc.txt")

  printf '%-8s %-10s %-9s %-9s %-9s %s\n' "$conc" "$n" "$ok" "$to" "$refused" "$mean"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$conc" "$n" "$ok" "$to" "$refused" "$mean" >>"$OUT"

  # The app is the subject, not a bystander: if it died, this level's numbers
  # describe an absent kernel and must not be recorded as a performance result.
  #
  # But a death is NOT a reason to abandon the sweep. The crashes on this
  # emulator are not load-correlated -- one of them landed at concurrency 1,
  # with three successful requests already in the bag -- so treating the first
  # death as "the load ceiling" would attribute the ROM's instability to the
  # kernel. Restart the app and mark the level instead.
  if [ "$(pid_of)" != "$PID" ]; then
    say ""
    say "sweep: app died at concurrency=$conc -- restarting it and marking the level"
    "$ADB" "${ADB_ARGS[@]}" shell "am start -n dev.detour/.MainActivity" >/dev/null 2>&1
    sleep 7
    NEWPID="$(pid_of)"
    if [ -z "$NEWPID" ]; then
      say "sweep: could not restart the app -- stopping"
      DIED=1
      break
    fi
    PID="$NEWPID"
    say "sweep: restarted as pid=$PID (this level's row is not a kernel measurement)"
    control connect
    sleep 5
    "$ADB" "${ADB_ARGS[@]}" shell "curl -sS -o /dev/null --max-time 20 https://$HOST/ >/dev/null 2>&1" >/dev/null 2>&1
    DEATHS=$((DEATHS + 1))
  fi
  sleep 3
done

say ""
if [ "$DIED" = "0" ]; then
  say "sweep: app SURVIVED the whole sweep (${DEATHS:-0} restarts)"
else
  say "sweep: the sweep could not be completed"
fi
say "sweep: app restarts during the sweep: ${DEATHS:-0}"
say "sweep: raw per-level data in $WORK/sweep-<level>.txt"
say "sweep: table in $OUT"

control disconnect >/dev/null 2>&1
