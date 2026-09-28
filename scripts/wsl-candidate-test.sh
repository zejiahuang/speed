#!/usr/bin/env bash
# Run the candidate-ordering and dial-timeout tests, one at a time.
#
# Written as individual `--exact` filters rather than a `tcp::tests::` prefix.
# The prefix form runs the whole module, and one test in it
# (`completes_a_handshake_and_relays_payload_both_ways`) drives a real smoltcp
# interface whose harness clock advances only on demand — when it is left to run
# alone it has been observed to sit for over an hour without printing anything.
# Naming the tests explicitly is how the suite stays usable.
#
# Each test gets its own cargo invocation because `--` takes one filter. Slow,
# and the only way to get an answer.
set -uo pipefail

cd /mnt/d/4

TESTS=(
  "tcp::tests::a_rejected_candidate_is_stepped_over_rather_than_dialled"
  "tcp::tests::a_list_where_everything_was_rejected_is_never_emptied"
  "tcp::tests::an_unverified_candidate_is_never_skipped"
  "tcp::tests::a_dial_with_somewhere_to_fail_over_to_is_the_one_that_runs_out_of_patience"
  "tcp::tests::the_alternative_is_looked_for_ahead_of_the_cursor_not_behind_it"
  "tcp::tests::the_short_dial_is_never_longer_than_the_full_one"
  "verify::tests::a_probe_in_flight_is_not_a_verdict"
  "verify::tests::an_in_flight_probe_counts_as_pending_so_the_dial_waits_for_it"
  "verify::tests::a_settled_verdict_stops_counting_as_pending"
  "verify::tests::an_unreachable_verdict_is_forgotten_sooner_than_a_wrong_certificate"
)

pass=0
fail=0

for test in "${TESTS[@]}"; do
  out="$(timeout 120 bash scripts/cargo.sh test -p watt-stack --lib -- --exact "$test" --test-threads=1 2>&1 | tr -d '\0')"
  if printf '%s' "$out" | grep -q "test result: ok. 1 passed"; then
    echo "  PASS  $test"
    pass=$((pass + 1))
  else
    echo "  FAIL  $test"
    printf '%s\n' "$out" | tail -20
    fail=$((fail + 1))
  fi
done

echo
echo "candidate/dial tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
