#!/usr/bin/env bash
# Find the concurrency ceiling of the tunnel, so the sweep can be configured
# below it.
#
# The full 1281-domain sweep through the tunnel reported 1026 blocked (80%),
# while the same domains probed sequentially all succeed in under a second. The
# difference is concurrency: the sweep runs 8 curls at once and the numbers
# degrade even for the domains that do succeed (flathub 0.3s -> 2.25s,
# github.com 200 in 10s against a 10s cap).
#
# So the sweep's "blocked" count is measuring the harness, not the network. This
# script measures where the tunnel stops keeping up: a fixed list of known-good
# domains at 1, 2, 4, 8, 16 concurrent requests.
#
# The runner goes to the device as a *file*, never as an inline `sh -c`: adb
# shell eats every `$` on the way through Windows, so an inline command arrives
# mangled and produces silent empty output. See scripts/run-in-wsl.sh for the
# same lesson stated for wsl.exe.
#
# Usage: bash scripts/app-concurrency.sh [runs-per-level]
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
# `TMP` is already exported by Git Bash (to /tmp), so `${TMP:-tmp}` silently
# resolves to /tmp and every artefact lands outside the project. That is how
# this script's first two runs produced no device files at all: the push looked
# for them in `tmp/` while the heredoc had written them to `/tmp`, and adb.exe
# was handed a POSIX path it cannot read. Same family as `GROUPS` — pick a name
# the shell does not already own.
WORK="${WORK:-tmp}"
RUNS="${1:-2}"
TIMEOUT="${TIMEOUT:-12}"
JOBS_LEVELS="${JOBS_LEVELS:-1 2 4 8 16}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

# adb.exe is a Windows program: it cannot read a POSIX source path. Resolve to
# an absolute path first (so a relative `tmp/x` becomes `D:/4/tmp/x`), then
# convert the drive letter. Without the `cd`/`pwd` step a relative path reaches
# adb unchanged and it reports "cannot stat" against the local file — an error
# that reads like the file is missing when it is really the path format.
to_win() {
  local path="$1" abs
  abs="$(cd "$(dirname "$path")" 2>/dev/null && pwd)/$(basename "$path")"
  case "$abs" in
    /[a-zA-Z]/*) printf '%s' "$(echo "${abs:1:1}" | tr 'a-z' 'A-Z'):${abs:2}" ;;
    *) printf '%s' "$abs" ;;
  esac
}

# Push, and say so when it fails. A silent `push_file` is how the first two runs
# of this script produced "0/0 reachable" with no error to explain it: the push
# failed, the device ran nothing, and every level looked equally broken.
push_file() {
  local src
  src="$(to_win "$1")"
  "$ADB" "${ADB_ARGS[@]}" push "$src" "$2" >/dev/null 2>&1 || {
    say "concurrency: FAILED to push $src -> $2"
    return 1
  }
}

# All of these returned 2xx/3xx through the tunnel when probed one at a time.
DOMAINS="flathub.org apache.org github.com www.mozilla.org code.jquery.com
addons.mozilla.org s1.pearlcdn.com www.ebay.com login.live.com cfx.re"

say "concurrency: $RUNS runs per level, ${TIMEOUT}s per request"
say "concurrency: levels = $JOBS_LEVELS"
say "concurrency: domains = $(printf '%s' "$DOMAINS" | wc -w)"
say ""

# The device-side runner. One curl per domain, `-P` sets the parallelism. The
# `_` after `sh -c` puts the domain in `$1` rather than `$0`.
cat >"$WORK/conc-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = parallelism, $2 = timeout, $3 = list file
jobs="$1"
timeout_s="$2"
list="$3"
CMD="d=\"\$1\"; curl -sS -o /dev/null -w \"%{http_code}\n\" --max-time $timeout_s \"https://\$d/\" 2>/dev/null || printf '000\n'"
xargs -P "$jobs" -n 1 sh -c "$CMD" _ <"$list" 2>/dev/null
printf '\n'
RUNNER

printf '%s\n' $DOMAINS >"$WORK/conc-list.txt"
push_file "$WORK/conc-runner.sh" /data/local/tmp/watt/conc-runner.sh
push_file "$WORK/conc-list.txt" /data/local/tmp/watt/conc-list.txt

"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/conc-runner.sh' >/dev/null 2>&1

for jobs in $JOBS_LEVELS; do
  TOT=0
  OKN=0

  for run in $(seq 1 "$RUNS"); do
    # Keep the newlines: one code per line is what makes them countable. Flatten
    # to spaces first and ten codes become one token, which is how an earlier
    # version of this script reported "reachable=1/1" for every level.
    OUT="$("$ADB" "${ADB_ARGS[@]}" shell \
      "sh /data/local/tmp/watt/conc-runner.sh $jobs $TIMEOUT /data/local/tmp/watt/conc-list.txt" \
      2>/dev/null | tr -d '\r' | grep -E '^[0-9]{3}$')"

    N=0
    O=0
    for code in $OUT; do
      N=$((N + 1))
      [ "$code" != "000" ] && O=$((O + 1))
    done
    TOT=$((TOT + N))
    OKN=$((OKN + O))

    say "  jobs=$jobs run $run: reachable=$O/$N  codes=$(printf '%s' "$OUT" | tr '\n' ' ')"
  done

  if [ "$TOT" -gt 0 ]; then
    PCT=$((OKN * 100 / TOT))
  else
    PCT=0
  fi
  say "concurrency: jobs=$jobs  TOTAL reachable=$OKN/$TOT  (${PCT}%)"
  say ""
done

say "concurrency: pick the sweep's --jobs below the level where the rate first"
say "concurrency: drops. If 1 and 2 are 100% and 4 is not, run the sweep at 2."
