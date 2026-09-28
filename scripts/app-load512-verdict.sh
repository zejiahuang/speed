#!/usr/bin/env bash
#
# 512 concurrent clients, a per-host verdict, and — the part the last version
# got wrong — a verdict that stays honest when the app dies mid-storm.
#
# # What went wrong last time
#
# The previous per-host run reported a beautiful table:
#
#     apache.org   114 requests  38 reached  33.3%  ...
#     github.com   104 requests   0 reached   0.0%  ...
#
# It was wrong. The kernel's own status, read either side of the storm, said:
#
#     before: {"phase":"on","running":true, "tcp_opened":0,   "uptime_ms":4911}
#     after:  {"phase":"off","running":false,"tcp_opened":0,   "uptime_ms":0}
#
# A `uptime_ms` of zero after the storm is not a quiet kernel — it is a
# *different* kernel, and `running:false` says there was not one at all. The app
# had died partway through, and every request after that point was answered by
# nothing. Half the table measured a closed port and blamed the sites.
#
# The cause was not the sites and not the load. It was a use-after-free in the
# teardown path: the step loop ran on a coroutine, `Job.cancel()` does not wait
# for it, and the free that followed raced a `step()` that was still inside the
# protector — `#00 BoxedProtector::protect+7` under `TcpRelay::service`, fault
# address 0x20. That is fixed now (the Rust side refuses to free into a live
# call, and the Kotlin side joins the loop before freeing), but a test that
# cannot tell "the site failed" from "the app died" is not worth re-running
# unimproved. So this one watches.
#
# # The design
#
# The storm is cut into segments. Between segments the supervisor checks that the
# app is alive AND that its kernel says `running:true`. If it is not, it restarts
# the app, reconnects, and continues — and every segment records which one it is,
# so the final table can say how many segments a host's numbers came from and
# whether the kernel changed under them.
#
# A host whose samples all come from one segment with a stable kernel is a real
# verdict. A host whose samples span a restart is marked, and its numbers are
# context rather than proof.
#
# # Reading the result
#
#   reached   the host answered with a status code. 2xx/3xx/4xx/5xx all count;
#             a 403 is a server answering, not a failure. Only "000" is a
#             non-answer, and it is broken out by why (timeout / refused).
#   timeout   no answer within the budget (curl 28) -- genuinely unreachable.
#   refused   the connection was rejected (curl 7). Through the tunnel this
#             means the kernel had no listener for the SYN: the one
#             client-visible signal that the kernel ran out.
#   segs      how many segments this host's samples came from. 1 is clean.
#
# Usage:
#   bash scripts/app-load512-verdict.sh [--threads N] [--seconds N]
#                                       [--segment N] [--timeout S] [--hosts "..."]
#
#   --threads   concurrent curl processes           (default 512)
#   --seconds   total storm duration, split across  (default 40)
#               segments
#   --segment   seconds per segment                 (default 10)
#   --timeout   per-request curl budget             (default 15)

set -u

DEVICE="${DEVICE:-emulator-5554}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG=dev.detour
WORK="${WORK:-tmp}"

THREADS=512
SECONDS_TO_RUN=40
SEGMENT=10
TIMEOUT=15

while [ $# -gt 0 ]; do
  case "$1" in
    --threads) THREADS="$2"; shift 2 ;;
    --seconds) SECONDS_TO_RUN="$2"; shift 2 ;;
    --segment) SEGMENT="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --hosts)   HOSTS="$2"; shift 2 ;;
    *) echo "verdict: unknown option $1" >&2; exit 2 ;;
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

# The kernel's own JSON, or empty. `status` is broadcast, waited on, then read
# back from the control tag: the broadcast is asynchronous, so reading
# immediately after it returns the previous answer.
read_stats() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd status" >/dev/null 2>&1
  sleep 1
  "$ADB" "${ADB_ARGS[@]}" logcat -d -s DetourControl:I 2>/dev/null \
    | tr -d '\r' | grep -o '{"phase".*}' | tail -1
}

# The one field that matters for the survival question. Extracted with a plain
# string split rather than a JSON parser, because the device has neither `jq`
# nor a guaranteed `python3` and this field is always a bare `true`/`false`.
kernel_running() {
  printf '%s' "$1" | grep -o '"running":[a-z]*' | head -1 | cut -d: -f2
}

##############################################################################
# the site list
##############################################################################
#
# Three classes, so the report says something about each rather than about one:
#   rule carriers     -- the traffic rule set covers these
#   mainstream, no rule -- domestic sites with no rule entry
#   blocked, no rule  -- no rule, and unreachable on this network
#
# The blocked ones are in on purpose. A verdict table that only contains sites
# that work says nothing about whether the tunnel is doing its job. `google.com`
# is expected to fail: it has no rule, and it is blocked here. A `000` for it is
# the correct answer, and its presence is what proves the table can tell the two
# apart.
HOSTS="${HOSTS:-github.com flathub.org apache.org dl.google.com bing.com baidu.com bilibili.com www.4399.com www.qq.com www.163.com google.com www.youtube.com}"

##############################################################################
# device-side runner
##############################################################################
#
# Every worker is pinned to ONE host and hammers it for the segment. Pinning is
# what makes the per-host numbers comparable: each host gets exactly
# `threads/hosts` workers, so a host is never judged on a smaller share of the
# load than its neighbours.
#
# Each worker writes its own file. 512 shells sharing one descriptor splice their
# lines together -- rows like "13.323316200" are three fused requests -- and the
# per-host split would then read garbage. The fold happens after the wait.
#
# The host goes INTO each line rather than being derived later from the part-file
# index. Re-deriving the round-robin at read time looks equivalent and is not:
# a glob that sorts part.10 before part.2, or a worker that died without
# writing, silently attributes one host's results to another.
cat >"$WORK/verdict-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = threads, $2 = seconds, $3 = timeout, $4 = host list (space separated)
threads="$1"
duration="$2"
tmo="$3"
hosts="$4"

OUTDIR=/data/local/tmp/watt/verdict-parts
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

set -- $hosts
nhosts=$#
[ "$nhosts" -eq 0 ] && nhosts=1

# Host for worker i, round-robin over the list.
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

[ -x "$ADB" ] || { say "verdict: adb not executable at $ADB"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/verdict-runner.sh")" /data/local/tmp/watt/verdict-runner.sh >/dev/null 2>&1 \
  || { say "verdict: FAILED to push the runner"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/verdict-runner.sh' >/dev/null 2>&1

SEGMENTS=$(( (SECONDS_TO_RUN + SEGMENT - 1) / SEGMENT ))
[ "$SEGMENTS" -lt 1 ] && SEGMENTS=1

say "verdict: threads=$THREADS total=${SECONDS_TO_RUN}s segment=${SEGMENT}s segments=$SEGMENTS timeout=${TIMEOUT}s"
say "verdict: hosts = $HOSTS"

# Bring the tunnel up from a known state. A live kernel from a previous run would
# make "before" meaningless.
control disconnect >/dev/null 2>&1
sleep 2
control connect >/dev/null 2>&1
sleep 5

PID="$(pid_of)"
if [ -z "$PID" ]; then
  say "verdict: the app is not running; start it first"
  exit 1
fi

BEFORE="$(read_stats)"
say "verdict: kernel before: $BEFORE"
if [ "$(kernel_running "$BEFORE")" != "true" ]; then
  say "verdict: the kernel is not running before the storm; nothing to measure"
  exit 1
fi

ALL="$WORK/verdict-all.txt"
: > "$ALL"
SEGLOG="$WORK/verdict-segments.txt"
: > "$SEGLOG"
RESTARTS=0

seg=1
while [ "$seg" -le "$SEGMENTS" ]; do
  # Liveness before the segment, not just after: a kernel that died in the last
  # segment would otherwise be measured again and its failure attributed to the
  # hosts.
  cur_pid="$(pid_of)"
  cur_stats="$(read_stats)"
  cur_running="$(kernel_running "$cur_stats")"

  if [ -z "$cur_pid" ] || [ "$cur_running" != "true" ]; then
    RESTARTS=$((RESTARTS + 1))
    say "verdict: segment $seg -- kernel down (pid='${cur_pid:-none}' running='${cur_running:-none}'), restarting"
    printf 'restart segment=%d pid=%s running=%s uptime=%s\n' \
      "$seg" "${cur_pid:-none}" "${cur_running:-none}" \
      "$(printf '%s' "$cur_stats" | grep -o '"uptime_ms":[0-9]*' | cut -d: -f2)" >> "$SEGLOG"

    # `am start` rather than `monkey`: this emulator has no `monkey`.
    "$ADB" "${ADB_ARGS[@]}" shell "am start -n $PKG/.MainActivity" >/dev/null 2>&1
    sleep 6
    control connect >/dev/null 2>&1
    sleep 6

    cur_pid="$(pid_of)"
    cur_running="$(kernel_running "$(read_stats)")"
    if [ -z "$cur_pid" ] || [ "$cur_running" != "true" ]; then
      say "verdict: segment $seg -- could not bring the kernel back; skipping this segment"
      printf 'failed segment=%d\n' "$seg" >> "$SEGLOG"
      seg=$((seg + 1))
      continue
    fi
  fi

  say "verdict: segment $seg/$SEGMENTS -- pid=$cur_pid kernel=up"

  OUT_DEV="/data/local/tmp/watt/verdict-seg$seg.txt"
  "$ADB" "${ADB_ARGS[@]}" shell \
    "sh /data/local/tmp/watt/verdict-runner.sh $THREADS $SEGMENT $TIMEOUT '$HOSTS' > $OUT_DEV 2>/dev/null" \
    >/dev/null 2>&1

  # Tag every row with its segment, so a host's numbers can be attributed and a
  # reader can see for themselves which segment a line came from.
  "$ADB" "${ADB_ARGS[@]}" shell "cat $OUT_DEV" 2>/dev/null | tr -d '\r' \
    | awk -v s="$seg" 'NF >= 4 { print $1, $2, $3, $4, s }' >> "$ALL"

  after_stats="$(read_stats)"
  after_running="$(kernel_running "$after_stats")"
  rows=$(wc -l < "$ALL" | tr -d ' ')
  printf 'segment=%d pid=%s kernel_after=%s rows_so_far=%s\n' \
    "$seg" "$(pid_of)" "${after_running:-none}" "$rows" >> "$SEGLOG"
  say "verdict: segment $seg done -- kernel after=$after_running rows=$rows"

  seg=$((seg + 1))
done

AFTER="$(read_stats)"
say "verdict: kernel after:  $AFTER"
say "verdict: restarts=$RESTARTS segments=$SEGMENTS"

TOTAL_ROWS=$(wc -l < "$ALL" | tr -d ' ')
if [ "$TOTAL_ROWS" -eq 0 ]; then
  say "verdict: no samples at all; nothing to report"
  exit 1
fi

##############################################################################
# aggregate
##############################################################################
say ""
say "=== aggregate (all segments pooled) ==="
awk '
  {
    code=$2; t=$3+0; rc=$4
    n++
    if (code ~ /^[0-9][0-9][0-9]$/ && code != "000") reached++
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
' "$ALL"

##############################################################################
# per-host verdict: the actual question
##############################################################################
#
# Straight group-by on the host column -- no re-derivation of the round-robin,
# nothing to drift. `segs` is the honest part: a host measured across more than
# one segment had a restart in the middle of its samples, and that is worth
# knowing before trusting the number.
#
# `reached` means the host returned a three-digit status. Counting only 2xx/3xx
# would report a working-but-forbidden site as broken, which this project has
# done twice.
say ""
say "=== per host ==="
printf '%-22s %-9s %-8s %-7s %-8s %-8s %-8s %s\n' \
  "host" "requests" "reached" "ok%" "timeout" "refused" "segs" "mean_s"
printf '%-22s %-9s %-8s %-7s %-8s %-8s %-8s %s\n' \
  "----" "--------" "-------" "---" "-------" "-------" "----" "------"

awk '
  {
    host=$1; code=$2; t=$3+0; rc=$4; seg=$5
    n[host]++
    if (code ~ /^[0-9][0-9][0-9]$/ && code != "000") reached[host]++
    else if (rc == "28") to[host]++
    else if (rc == "7") refused[host]++
    sum[host]+=t
    seen[host, seg]=1
  }
  END {
    for (h in n) {
      s=0
      for (k in seen) {
        split(k, parts, SUBSEP)
        if (parts[1] == h) s++
      }
      printf "%-22s %-9d %-8d %-7.1f %-8d %-8d %-8d %.3f\n",
        h, n[h], reached[h]+0, 100*(reached[h]+0)/n[h],
        to[h]+0, refused[h]+0, s, sum[h]/n[h]
    }
  }
' "$ALL" | sort

say ""
say "=== segment log ==="
cat "$SEGLOG"

say ""
say "verdict: raw samples (host code time rc segment) in $ALL"
say "verdict: a host with segs=1 is a clean verdict; segs>1 means a restart"
say "verdict:   landed in the middle of its samples, so read it as context"
