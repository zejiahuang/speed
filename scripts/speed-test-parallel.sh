#!/usr/bin/env bash
# Download one URL with N parallel range requests, and report the aggregate
# rate. Used to find out whether a slow download is a per-connection cap or a
# real bandwidth limit: if eight streams beat one by a wide margin, the fix is
# parallelism rather than patience.
#
#   speed-test-parallel.sh <url> [connections...]

set -uo pipefail

URL="$1"
shift
CONNS=("$@")
[ ${#CONNS[@]} -eq 0 ] && CONNS=(1 4 8)
CHUNK=$((8 * 1024 * 1024))
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

for n in "${CONNS[@]}"; do
  start=$(date +%s.%N)
  # One background curl per range. `xargs -I` cannot be used here: its
  # placeholder is substituted textually, so arithmetic on it would have to run
  # before xargs ever sees it.
  for i in $(seq 0 $((n - 1))); do
    lo=$((i * CHUNK))
    hi=$(((i + 1) * CHUNK - 1))
    curl -s -o /dev/null \
      -r "$lo-$hi" \
      --max-time 30 "$URL" \
      -w '%{size_download}\n' >>"$WORK/out.$n" 2>/dev/null &
  done
  wait
  end=$(date +%s.%N)
  total=$(awk '{s += $1} END {print s + 0}' "$WORK/out.$n")
  elapsed=$(awk -v a="$start" -v b="$end" 'BEGIN {d = b - a; print (d > 0 ? d : 0.001)}')
  awk -v n="$n" -v t="$total" -v e="$elapsed" \
    'BEGIN {printf "  %2d connection(s): %8.0f KB/s  (%d bytes in %.1fs)\n", n, t/e/1024, t, e}'
done
