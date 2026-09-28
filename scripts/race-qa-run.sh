#!/usr/bin/env bash
# QA runner for the racing feature. Runs one named test per cargo invocation with
# `--exact`, because a prefix filter runs the whole tcp::tests module and one
# test in it is known to hang when the module is driven as a whole.
#
# Usage (from inside WSL): bash scripts/race-qa-run.sh [test-name ...]
set -uo pipefail

cd /mnt/d/4

TESTS=("$@")
if [ "${#TESTS[@]}" -eq 0 ]; then
  TESTS=(
    "tcp::tests::race_picks_the_first_live_candidate"
    "tcp::tests::race_dials_each_candidate_at_most_once"
    "tcp::tests::losers_release_their_fds"
    "tcp::tests::serial_mode_is_unchanged"
    "tcp::tests::client_bytes_only_reach_the_winner"
    "tcp::tests::window_rotates_before_client_budget"
    "tcp::tests::bytes_from_upstream_counts_winner_only"
    "tcp::tests::protect_before_connect"
    "tcp::tests::the_global_dial_ceiling_bounds_the_window"
    "tcp::tests::a_synchronous_connect_wins_over_a_dial_in_flight"
  )
fi

pass=0
fail=0

for test in "${TESTS[@]}"; do
  out="$(timeout 180 bash scripts/cargo.sh test -p watt-stack --lib -- --exact "$test" --test-threads=1 2>&1 | tr -d '\0')"
  if printf '%s' "$out" | grep -q "test result: ok. 1 passed"; then
    echo "  PASS  $test"
    pass=$((pass + 1))
  else
    echo "  FAIL  $test"
    printf '%s\n' "$out" | tail -30
    fail=$((fail + 1))
  fi
done

echo
echo "race QA tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
