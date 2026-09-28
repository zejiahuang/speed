#!/usr/bin/env bash
#
# 512 concurrent clients, with a PER-HOST verdict.
#
# The earlier 512-thread runs reported aggregates -- 89% reached, 194 timeouts --
# which answers "did the storm succeed" but not "which sites work". This one asks
# the second question: every host gets its own concurrency share, and the report
# is one line per host.
#
# Reading the result:
#   reached   -- the host answered with a status code (2xx/3xx/4xx/5xx all count;
#                a 403 is a server answering, not a failure)
#   timeout   -- no answer within the budget (curl 28)
#   refused   -- the connection was rejected (curl 7). In a TUN this means the
#                kernel had no listener to land the SYN on -- it is the one
#                client-visible signal that says "the kernel ran out", so it is
#                counted separately rather than folded into "failed".
#
# Why per-host files: 512 shells writing to one descriptor splice their lines
# together (rows like "13.323316200" from three fused requests). Each worker
# writes its own file; the fold happens after the wait.
#
# Usage:
#   bash scripts/app-load512-perhost.sh [--threads N] [--seconds N] [--timeout S]

set -u

DEVICE="${DEVICE:-emulator-5554}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG=dev.detour
WORK="${WORK:-tmp}"

THREADS=512
SECONDS_TO_RUN=40
TIMEOUT=15

while [ $# -gt 0 ]; do
  case "$1" in
    --threads) THREADS="$2"; shift 2 ;;
    --seconds) SECONDS_TO_RUN="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    *) echo "perhost: unknown option $1" >&2; exit 2 ;;
  esac
done

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }
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

read_stats() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd status" >/dev/null 2>&1
  sleep 1
  "$ADB" "${ADB_ARGS[@]}" logcat -d -s DetourControl:I 2>/dev/null \
    | tr -d '\r' | grep -o '{"phase".*}' | tail -1
}

##############################################################################
# the site list
##############################################################################
#
# A mix, so the report says something about each class rather than about one:
#   rule carriers   -- the traffic rule set covers these
#   mainstream no-rule -- domestic sites with no rule entry
#   blocked no-rule -- no rule, and unreachable on this network
# Each host is probed by a share of the threads, round-robin.
HOSTS="${HOSTS:-github.com flathub.org apache.org dl.google.com bing.com baidu.com bilibili.com www.4399.com www.qq.com www.163.com google.com www.youtube.com}"

##############################################################################
# device-side runner
##############################################################################
#
# Every thread is assigned ONE host and hammers it for the duration. Assigning
# a host per thread (rather than picking per request) is what makes the per-host
# numbers meaningful: each host gets exactly `threads/hosts` workers, so the
# load is even and comparable.
cat >"$WORK/perhost-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = threads, $2 = seconds, $3 = timeout, $4 = host list (space separated)
threads="$1"
duration="$2"
tmo="$3"
hosts="$4"

OUTDIR=/data/local/tmp/watt/perhost-parts
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

# Flatten the host list and count it.
set -- $hosts
nhosts=$#
[ "$nhosts" -eq 0 ] && nhosts=1

# Host for thread i: round-robin, so thread 1..N cover the list evenly.
host_for() {
  i="$1"
  idx=$(( (i - 1) % nhosts ))
  shift 1
  for h in "$@"; do
    [ "$idx" -eq 0 ] && { printf '%s' "$h"; return; }
    idx=$((idx - 1))
  done
}

deadline=$(( $(date +%s) + duration ))

t=1
while [ "$t" -le "$threads" ]; do
  (
    h="$(host_for "$t" $hosts)"
    url="https://$h/"
    while [ "$(date +%s)" -lt "$deadline" ]; do
      out="$(curl -sS -o /dev/null -w "%{http_code} %{time_total}" --max-time "$tmo" "$url" 2>/dev/null)"
      rc=$?
      [ -z "$out" ] && out="000 0"
      # The host goes INTO the line. Deriving it later from the part-file index
      # means re-running the same round-robin at read time, and any drift between
      # the two -- a glob that sorts part.10 before part.2, a thread that died
      # without writing -- silently attributes one host's results to another.
      printf "%s %s %s\n" "$h" "$out" "$rc"
    done > "$OUTDIR/p.$t"
  ) &
  t=$((t + 1))
done
wait

for f in "$OUTDIR"/p.*; do
  [ -f "$f" ] && cat "$f"
done
RUNNER

##############################################################################
# run
##############################################################################

[ -x "$ADB" ] || { say "perhost: adb not executable at $ADB"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/perhost-runner.sh")" /data/local/tmp/watt/perhost-runner.sh >/dev/null 2>&1 \
  || { say "perhost: FAILED to push the runner"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/perhost-runner.sh' >/dev/null 2>&1

say "perhost: threads=$THREADS seconds=${SECONDS_TO_RUN}s timeout=${TIMEOUT}s"
say "perhost: hosts = $HOSTS"

control disconnect >/dev/null 2>&1
sleep 2
control connect >/dev/null 2>&1
sleep 5

PID="$(pid_of)"
if [ -z "$PID" ]; then
  say "perhost: the app is not running; start it first"
  exit 1
fi
say "perhost: app pid=$PID"

say "perhost: kernel before: $(read_stats)"

say "perhost: starting $THREADS threads"
"$ADB" "${ADB_ARGS[@]}" shell \
  "sh /data/local/tmp/watt/perhost-runner.sh $THREADS $SECONDS_TO_RUN $TIMEOUT '$HOSTS' > /data/local/tmp/watt/perhost-out.txt 2>/dev/null" \
  >/dev/null 2>&1

say "perhost: kernel after:  $(read_stats)"

OUT="$WORK/perhost-out.txt"
"$ADB" "${ADB_ARGS[@]}" shell 'cat /data/local/tmp/watt/perhost-out.txt' 2>/dev/null | tr -d '\r' > "$OUT"

if [ "$(pid_of)" = "$PID" ]; then
  say "perhost: app pid $PID SURVIVED"
else
  say "perhost: app pid $PID DID NOT SURVIVE (a dead kernel corrupts the verdict)"
fi

say ""

say ""
say "=== aggregate ==="
awk '
  {
    host=$1; code=$2; t=$3+0; rc=$4
    n++
    if (code ~ /^[0-9]{3}$/ && code != "000") reached++
    else if (rc == "28") to++
    else if (rc == "7") refused++
    else other++
    sum+=t
  }
  END {
    printf "requests   %d\n", n
    printf "reached    %d (%.1f%%)\n", reached+0, (n?100*(reached+0)/n:0)
    printf "timeout    %d (%.1f%%)\n", to+0, (n?100*(to+0)/n:0)
    printf "refused    %d (%.1f%%)\n", refused+0, (n?100*(refused+0)/n:0)
    printf "other 000  %d\n", other+0
    printf "mean       %.3fs\n", (n?sum/n:0)
  }
' "$OUT"

##############################################################################
# per-host verdict: the actual question
##############################################################################
#
# Every line carries its own host, so this is a straight group-by -- no
# re-derivation of the round-robin, nothing to drift.
#
# "reached" means the host returned a three-digit status. A 403 or a 404 is a
# host that ANSWERED; counting only 2xx/3xx would report a working site as
# broken, which is the mistake this project has already made twice.
say ""
say "=== per host ==="
printf '%-22s %-9s %-8s %-8s %-8s %-8s %s\n' "host" "requests" "reached" "ok%" "timeout" "refused" "mean_s"
printf '%-22s %-9s %-8s %-8s %-8s %-8s %s\n' "----" "--------" "-------" "---" "-------" "-------" "------"

awk '
  {
    host=$1; code=$2; t=$3+0; rc=$4
    n[host]++
    if (code ~ /^[0-9]{3}$/ && code != "000") reached[host]++
    else if (rc == "28") to[host]++
    else if (rc == "7") refused[host]++
    sum[host]+=t
  }
  END {
    for (h in n)
      printf "%-22s %-9d %-8d %-8.1f %-8d %-8d %.3f\n",
        h, n[h], reached[h]+0, 100*(reached[h]+0)/n[h], to[h]+0, refused[h]+0, sum[h]/n[h]
  }
' "$OUT" | sort

say ""
say "perhost: raw storm data in $OUT"
say "perhost: the per-host split above is the verdict; the aggregate is context"
