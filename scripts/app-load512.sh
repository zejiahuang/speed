#!/usr/bin/env bash
# Drive the tunnel at high concurrency and watch what the kernel does.
#
# Why a separate harness from `app-sweep.sh`. That script answers "do the rules
# work", so it is deliberately gentle — six at a time, and its header says why:
# the listener pool and flow table are finite, and a wide burst would measure the
# ceiling rather than the rules. This script is the opposite question. It *wants*
# the ceiling, and it wants to know where it is and how the kernel behaves when
# it arrives.
#
# What 512 concurrent requests actually run into, from `StackConfig::default`:
#
#   max_listeners_per_endpoint  64    the binding limit for one hostname. A 512
#                                     way burst at a single host exceeds it by 8x
#                                     by design, so the first thing to read is
#                                     whether the ceiling produces refusals or
#                                     queueing.
#   listener_pool                4    the static water mark. The pool refills to
#                                     this after a burst, not to the burst size.
#   max_tcp_flows             2048   512 fits, but the sweep's own traffic and
#                                     the previous run's TIME_WAIT may not.
#   flow_buffer_limit       512 KiB   per flow, so 512 flows could ask for 256 MB
#                                     against ~900 MB free. Worth watching.
#
# The device has two cores and 2 GB. It will be the bottleneck long before any
# host-side number matters, so every measurement is taken on the device and the
# host only starts the load and reads the counters.
#
# Usage:
#   bash scripts/app-load512.sh [--threads N] [--seconds N] [--target MODE]
#
#   --threads   concurrent curl processes (default 512)
#   --seconds   how long to sustain (default 60)
#   --target    mix (default) | one (single host) | local (device loopback)
#   --warm      let the verdict cache fill first, separately
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
WORK="${WORK:-tmp}"

THREADS=512
SECONDS_TO_RUN=60
TARGET="mix"
TAG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --threads) THREADS="$2"; shift 2 ;;
    --seconds) SECONDS_TO_RUN="$2"; shift 2 ;;
    --target)  TARGET="$2"; shift 2 ;;
    --tag)     TAG="$2"; shift 2 ;;
    *) echo "app-load512: unknown option $1" >&2; exit 2 ;;
  esac
done

# Results land in per-run files. A fixed output path let a later run overwrite an
# earlier run's raw data -- the `one` run's 1493 rows were gone by the time the
# `local` re-run finished, leaving only its summary. The tag defaults to the
# target name so consecutive runs of the same target still collide; pass --tag
# when that matters.
SUFFIX="${TAG:-$TARGET}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

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

# The control plane answers `status` with a JSON counter line in logcat. Reading
# it back is the only way to see the KERNEL's own accounting of the storm: how
# many flows it opened, how many it failed, and whether any were still live
# after the dust settled. Reading it after the storm has passed is not enough --
# a counter can only be attributed to a load if it was read on both sides of it.
read_stats() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd status" >/dev/null 2>&1
  sleep 1
  "$ADB" "${ADB_ARGS[@]}" logcat -d -s DetourControl:I 2>/dev/null \
    | tr -d '\r' | grep -o '{"phase".*}' | tail -1
}

pid_of() {
  "$ADB" "${ADB_ARGS[@]}" shell 'ps -A | grep "[d]ev.detour" | awk "{print \$2}"' 2>/dev/null | tr -d '\r' | head -1
}

##############################################################################
# the device-side load generator
##############################################################################
#
# Written as a file and pushed, never inlined: adb shell eats every `$` on the
# way through Windows, so an inline generator arrives mangled and produces
# silent empty output. This is the lesson `scripts/run-in-wsl.sh` also exists
# for, stated for adb.
#
# The generator runs `$threads` shell processes, each looping for `$seconds` and
# issuing a fresh request every iteration. Each records one line:
#
#   code  exit  seconds  target
#
# which is enough to compute the success rate, the exit-code mix, the latency
# distribution and the achieved request rate without a second pass.
cat > "$WORK/load512-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = threads, $2 = seconds, $3 = target mode, $4 = warmup (0/1)
threads="$1"
duration="$2"
target="$3"

# Every thread writes its OWN file. A shared fd looked fine at low concurrency
# but 512 `printf` calls racing on one descriptor splice their bytes together:
# the previous run produced rows like "13.323316200", "28200", "0307" -- a
# status code, a time and an exit code from three different requests fused into
# one line. One file per thread costs nothing and makes every row well-formed.
OUTDIR=/data/local/tmp/watt/load512-parts
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

# Targets. `mix` spreads across many hosts so the per-endpoint listener ceiling
# is not the story; `one` concentrates on a single host so that it is.
TARGETS_MIX="github.com flathub.org apache.org www.mozilla.org code.jquery.com
addons.mozilla.org s1.pearlcdn.com www.ebay.com login.live.com cfx.re
dl.google.com objects.githubusercontent.com cache-redirector.jetbrains.com
media.githubusercontent.com avatars.githubusercontent.com apache.org github.com"
TARGETS_ONE="github.com"

case "$target" in
  one)   POOL="$TARGETS_ONE" ;;
  local) POOL="127.0.0.1" ;;
  *)     POOL="$TARGETS_MIX" ;;
esac

# Pick a target per thread by round-robin over the pool.
pick() {
  i="$1"
  set -- $POOL
  n=$#
  idx=$(( i % n ))
  for t in $POOL; do
    [ "$idx" -eq 0 ] && { printf '%s' "$t"; return; }
    idx=$((idx - 1))
  done
}

deadline=$(( $(date +%s) + duration ))
t=1
while [ "$t" -le "$threads" ]; do
  (
    host="$(pick "$t")"
    if [ "$target" = "local" ]; then
      url="http://127.0.0.1:8080/"
    else
      url="https://$host/"
    fi
    while [ "$(date +%s)" -lt "$deadline" ]; do
      out="$(curl -sS -o /dev/null -w "%{http_code} %{time_total}" --max-time 15 "$url" 2>/dev/null)"
      rc=$?
      [ -z "$out" ] && out="000 0"
      printf "%s %s\n" "$out" "$rc"
    done > "$OUTDIR/part.$t"
  ) &
  t=$((t + 1))
done
wait
# Fold the per-thread files back into one stream, in thread order, so the
# aggregate file is exactly what the old single-fd version tried to be.
for f in "$OUTDIR"/part.*; do
  [ -f "$f" ] && cat "$f"
done
RUNNER

##############################################################################
# kernel-side sampler, so the load has a witness while it runs
##############################################################################
#
# Without this the only evidence is the client's view, and a stall and a slow
# link look the same from there. The sampler records the app's CPU, its thread
# count, its file descriptors and its RSS once a second, all of which are
# readable without root.
cat > "$WORK/load512-sampler.sh" <<'SAMPLER'
#!/system/bin/sh
# $1 = pid, $2 = samples, $3 = interval seconds
pid="$1"
count="$2"
interval="$3"
i=0
while [ "$i" -lt "$count" ]; do
  ts="$(date +%s)"
  # utime+stime from /proc/<pid>/stat, fields 14 and 15.
  if [ -r "/proc/$pid/stat" ]; then
    stat="$(cat "/proc/$pid/stat" 2>/dev/null)"
    # The comm field can contain spaces and parens, so split after the last ')'.
    rest="${stat#*) }"
    set -- $rest
    utime="$12"
    stime="$13"
  else
    utime="-"; stime="-"
  fi
  fds="$(ls /proc/$pid/fd 2>/dev/null | wc -l | tr -d ' ')"
  threads="$(ls /proc/$pid/task 2>/dev/null | wc -l | tr -d ' ')"
  rss="$(awk '/^VmRSS/{print $2}' /proc/$pid/status 2>/dev/null)"
  printf "%s %s %s %s %s %s\n" "$ts" "$utime" "$stime" "$threads" "$fds" "${rss:-0}"
  i=$((i + 1))
  sleep "$interval"
done
SAMPLER

MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/load512-runner.sh")"  /data/local/tmp/watt/load512-runner.sh  >/dev/null 2>&1
MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/load512-sampler.sh")" /data/local/tmp/watt/load512-sampler.sh >/dev/null 2>&1
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/load512-runner.sh /data/local/tmp/watt/load512-sampler.sh' >/dev/null 2>&1

##############################################################################
# run
##############################################################################

say "load512: threads=$THREADS duration=${SECONDS_TO_RUN}s target=$TARGET"
say "load512: device has $(MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" shell 'nproc' 2>/dev/null | tr -d '\r') cores, $(MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" shell 'awk "/^MemFree/{print \$2}" /proc/meminfo' 2>/dev/null | tr -d '\r') kB free"

# Bring the tunnel up and let it settle: a burst into a tunnel that is still
# establishing measures the establishment.
control disconnect
sleep 2
control connect
sleep 5

PID="$(pid_of)"
if [ -z "$PID" ]; then
  say "load512: the app is not running; start it first"
  exit 1
fi
say "load512: app pid=$PID"

##############################################################################
# sample + warm + load
##############################################################################
#
# The sampler starts BEFORE the warmup, not after. Last time the app died during
# the warmup pass, so the sampler -- which only began once warmup returned --
# found no `/proc/<pid>` and logged nine rows of "- - 0 0 0". The witness must
# already be watching before the thing it is meant to witness can happen.

SAMPLES=$((SECONDS_TO_RUN + 40))
"$ADB" "${ADB_ARGS[@]}" shell \
  "sh /data/local/tmp/watt/load512-sampler.sh $PID $SAMPLES 1 > /data/local/tmp/watt/load512-samples.$SUFFIX.txt 2>&1" \
  >/dev/null 2>&1 &
SAMPLER_JOB=$!

##############################################################################
# warm the verdict cache
##############################################################################
#
# The first connection to each host pays the candidate chain: dial, fail,
# re-dial, and only then record a verdict. At 512 threads every host's *first*
# connection would pay that cost simultaneously, so the run would mostly measure
# cold-start and say nothing about steady state. One sequential pass over the
# target list first, so each host has a verdict, is the difference between
# measuring the relay loop and measuring the resolver.
say "load512: warming the verdict cache (one pass, sequential)"
for host in github.com flathub.org apache.org www.mozilla.org code.jquery.com \
            addons.mozilla.org s1.pearlcdn.com www.ebay.com login.live.com cfx.re \
            dl.google.com objects.githubusercontent.com \
            cache-redirector.jetbrains.com media.githubusercontent.com \
            avatars.githubusercontent.com; do
  "$ADB" "${ADB_ARGS[@]}" shell "curl -sS -o /dev/null --max-time 12 https://$host/ >/dev/null 2>&1" >/dev/null 2>&1
done

# The warmup is a real load in its own right. Ask whether the app survived it
# before claiming the storm was what killed it.
if [ -n "$(pid_of)" ] && [ "$(pid_of)" = "$PID" ]; then
  say "load512: app survived the warmup (pid $PID)"
else
  say "load512: APP DIED DURING WARMUP -- the storm never started"
  say "load512: this is a finding, not a failure of the harness"
fi

##############################################################################
# load
##############################################################################

say "load512: starting $THREADS threads"
START="$(date +%s)"

# The kernel's own view, taken on the near side of the storm. The difference
# between this and the reading after is what the storm actually did to the
# engine, independent of what the clients reported.
STATS_BEFORE="$(read_stats)"
say "load512: kernel before: ${STATS_BEFORE:-<unavailable>}"

"$ADB" "${ADB_ARGS[@]}" shell \
  "sh /data/local/tmp/watt/load512-runner.sh $THREADS $SECONDS_TO_RUN $TARGET > /data/local/tmp/watt/load512-out.$SUFFIX.txt 2>/dev/null" \
  >/dev/null 2>&1

ELAPSED=$(( $(date +%s) - START ))
wait "$SAMPLER_JOB" 2>/dev/null

STATS_AFTER="$(read_stats)"
say "load512: kernel after:  ${STATS_AFTER:-<unavailable>}"

say "load512: load finished after ${ELAPSED}s"
say "load512: pulling results"

"$ADB" "${ADB_ARGS[@]}" shell "cat /data/local/tmp/watt/load512-out.$SUFFIX.txt" 2>/dev/null | tr -d '\r' > "$WORK/load512-out.$SUFFIX.txt"
"$ADB" "${ADB_ARGS[@]}" shell "cat /data/local/tmp/watt/load512-samples.$SUFFIX.txt" 2>/dev/null | tr -d '\r' > "$WORK/load512-samples.$SUFFIX.txt"

OUT="$WORK/load512-out.$SUFFIX.txt"
SAMPLES_FILE="$WORK/load512-samples.$SUFFIX.txt"

TOTAL="$(grep -c . "$OUT" 2>/dev/null || echo 0)"
say "load512: $TOTAL requests completed in ${ELAPSED}s"

if [ "$TOTAL" -gt 0 ] 2>/dev/null; then
  RATE=$((TOTAL / ELAPSED))
  say "load512: $RATE requests/second achieved"
fi

# Liveness, by pid, after the storm has passed. A load test that ends with a
# dead process has a result that is not a performance number.
if [ -n "$(pid_of)" ] && [ "$(pid_of)" = "$PID" ]; then
  say "load512: app pid $PID SURVIVED"
  SURVIVED=1
else
  say "load512: app pid $PID DID NOT SURVIVE the load"
  SURVIVED=0
fi

say ""
say "--- summary ---"
# Field order is: code time_total exit_code (that is what curl -w emits, and the
# runner appends $rc last). An earlier revision read $2 as the exit code and
# therefore reported the latency column as if it were an exit status.
awk '{
  code=$1; sec=$2+0; rc=$3
  n++
  if (code ~ /^[23]/) ok++
  else if (code ~ /^[45]/) answered++
  else if (code == "000" && rc == "60") cert++
  else if (code == "000" && rc == "28") to++
  else if (code == "000") other++
  sum += sec
  if (sec > max) max = sec
} END {
  printf "requests   %d\n", n
  printf "reached    %d (%.1f%%)\n", ok+answered, (n ? 100*(ok+answered)/n : 0)
  printf "  2xx      %d\n", ok
  printf "  4xx/5xx  %d\n", answered
  printf "cert-rej   %d (%.1f%%)\n", cert, (n ? 100*cert/n : 0)
  printf "timed out  %d (%.1f%%)\n", to, (n ? 100*to/n : 0)
  printf "other 000  %d\n", other
  printf "mean       %.3fs\n", (n ? sum/n : 0)
  printf "max        %.3fs\n", max
}' "$OUT"

say ""
say "--- exit code mix (why curl gave up) ---"
awk '{k=$3; n[k]++} END {for (c in n) printf "  exit %-6s %8d\n", (c==""?"<empty>":c), n[c]}' "$OUT" | sort -k3 -rn

say ""
say "--- kernel during the load (ts utime stime threads fds rss_kB) ---"
head -3 "$SAMPLES_FILE"
say "  ..."
tail -3 "$SAMPLES_FILE"

say ""
say "load512: raw data in $OUT and $SAMPLES_FILE"

control disconnect >/dev/null 2>&1

if [ "$SURVIVED" = "1" ]; then
  exit 0
fi
exit 1
