#!/usr/bin/env bash
#
# Run the whole verification suite in order and print one verdict.
#
# The individual scripts each answer one question. This one answers "is it
# working?", which is the question actually worth asking before believing
# anything about the kernel. It runs them in ascending order of how much they
# prove, so a failure early on is not buried under output from later stages.
#
#   1. unit tests          — the code does what it says
#   2. clippy              — it says it without warnings
#   3. script syntax       — the harness itself is not broken
#   4. real traffic        — curl over a real TLS handshake, judged from outside
#   5. real upstream       — real websites over the real internet, real certs
#   6. smoke               — the path works on a real TUN device
#   7. daemon              — rule lifecycle, hot reload, clean exit
#   8. upstream rules      — the real 1 MiB document compiles and caches
#
# Stages 4-8 each elevate themselves. Total runtime is a few minutes, most of it
# in stages 4 and 5. Stage 5 needs working internet access; without it that stage
# fails and says so, and the rest still run.
#
#   scripts/verify-all.sh
#
# Environment:
#   REPEAT_REAL_TRAFFIC   how many times to repeat stage 4 (default: 3)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"

REPEAT_REAL_TRAFFIC="${REPEAT_REAL_TRAFFIC:-3}"

mkdir -p "$TMP_DIR"

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) echo "verify-all: unknown argument: $1" >&2; exit 2 ;;
  esac
done

STAGE_NAMES=()
STAGE_RESULTS=()
STAGE_DETAILS=()

record() { # <name> <exit-status> <detail>
  STAGE_NAMES+=( "$1" )
  STAGE_RESULTS+=( "$2" )
  STAGE_DETAILS+=( "$3" )
}

run_stage() { # <name> <log> <command...>
  local name="$1" log="$2"
  shift 2
  echo
  echo "================================================================"
  echo "== $name"
  echo "================================================================"

  "$@" >"$log" 2>&1
  local status=$?

  local summary
  summary="$(summarize "$log")"
  if [ -z "$summary" ]; then
    summary="(no summary line; see $log)"
  fi

  if [ "$status" -eq 0 ]; then
    echo "PASS  $name"
  else
    echo "FAIL  $name (exit $status)"
  fi
  echo "      $summary"
  [ "$status" -eq 0 ] || tail -25 "$log"

  record "$name" "$status" "$summary"
}

# One line describing what a stage's log actually says.
#
# The test run is summed rather than sampled: `cargo test` prints one result line
# per target, so taking the last few shows the doctests and reports "0 passed" for
# a run of a hundred and sixty. A summary that can be read as the opposite of the
# truth is worse than no summary.
summarize() { # <log>
  local log="$1"

  if grep -q '^test result:' "$log"; then
    local passed failed
    passed="$(grep -oE '[0-9]+ passed' "$log" | awk '{sum += $1} END {print sum+0}')"
    failed="$(grep -oE '[0-9]+ failed' "$log" | awk '{sum += $1} END {print sum+0}')"
    printf 'tests passed=%s failed=%s\n' "$passed" "$failed"
    return 0
  fi

  # The harness markers are checked before the clippy heuristic, because those
  # logs also contain a `Finished` line from the build they ran first. Order
  # matters here: the other way round, a passing smoke test reports clippy's
  # result, which is not wrong so much as about the wrong thing.
  local markers
  markers="$(grep -E '^(RESULT |check-scripts:|daemon-smoke:|tun-smoke:|real-traffic:|real-upstream:|daemon-rules-check:)' \
    "$log" | tail -3 | tr '\n' ' ')"
  if [ -n "$markers" ]; then
    echo "$markers"
    return 0
  fi

  # clippy prints nothing when it is happy, so silence is the result.
  if grep -qE '^(warning|error)(\[|:)' "$log"; then
    printf 'clippy reported %s warning/error line(s)\n' "$(grep -cE '^(warning|error)(\[|:)' "$log")"
    return 0
  fi
  if grep -q '^    Finished' "$log"; then
    printf 'no warnings, no errors\n'
    return 0
  fi
}

echo "verify-all: running the full suite, this takes a few minutes"
echo "verify-all: repeat count for real traffic: $REPEAT_REAL_TRAFFIC"

run_stage "unit tests" "$TMP_DIR/verify-all-tests.log" \
  bash "$SCRIPTS_DIR/cargo.sh" test --workspace

run_stage "clippy" "$TMP_DIR/verify-all-clippy.log" \
  bash "$SCRIPTS_DIR/cargo.sh" clippy --workspace --all-targets -- -D warnings

run_stage "script syntax" "$TMP_DIR/verify-all-scripts.log" \
  bash "$SCRIPTS_DIR/check-scripts.sh"

# Repeated because a kernel that works once and not reliably is not usable, and a
# single green run cannot tell the two apart.
for attempt in $(seq 1 "$REPEAT_REAL_TRAFFIC"); do
  run_stage "real traffic (attempt $attempt/$REPEAT_REAL_TRAFFIC)" \
    "$TMP_DIR/verify-all-real-$attempt.log" \
    bash "$SCRIPTS_DIR/real-traffic.sh"
done

# Real internet traffic, which is the only stage that depends on something
# outside this machine. It is here because a kernel that only works against a
# local server has not been shown to work.
run_stage "real upstream traffic" "$TMP_DIR/verify-all-upstream.log" \
  bash "$SCRIPTS_DIR/real-upstream.sh"

run_stage "smoke on a real TUN" "$TMP_DIR/verify-all-smoke.log" \
  bash "$SCRIPTS_DIR/tun-smoke.sh"

run_stage "daemon lifecycle" "$TMP_DIR/verify-all-daemon.log" \
  bash "$SCRIPTS_DIR/daemon-smoke.sh"

run_stage "upstream rule document" "$TMP_DIR/verify-all-rules.log" \
  bash "$SCRIPTS_DIR/daemon-rules-check.sh"

echo
echo "================================================================"
echo "== summary"
echo "================================================================"

FAILED=0
for index in "${!STAGE_NAMES[@]}"; do
  if [ "${STAGE_RESULTS[$index]}" -eq 0 ]; then
    printf 'PASS  %s\n' "${STAGE_NAMES[$index]}"
  else
    printf 'FAIL  %s\n' "${STAGE_NAMES[$index]}"
    FAILED=$(( FAILED + 1 ))
  fi
done

echo
if [ "$FAILED" -eq 0 ]; then
  echo "VERIFY-ALL PASS stages=${#STAGE_NAMES[@]}"
  exit 0
fi

echo "VERIFY-ALL FAIL stages_failed=$FAILED of ${#STAGE_NAMES[@]}" >&2
exit 1
