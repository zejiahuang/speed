#!/usr/bin/env bash
# Verify the racing change: ffi settings, config defaults, the candidate/dial
# tests, and the serial handshake regression. Each test is named explicitly and
# wrapped in `timeout`, because one test in `tcp::tests` is known to hang when a
# whole module is run at once.
set -uo pipefail

cd /mnt/d/4

run() {
  local label="$1"; shift
  echo "=================== $label ==================="
  timeout 180 bash scripts/cargo.sh test "$@" 2>&1 | grep -E "test result|panicked|error\[|FAILED|^error|assertion" | head -40
}

run "ffi settings" -p watt-ffi --lib -- --test-threads=1

echo "=================== config ==================="
timeout 120 bash scripts/cargo.sh test -p watt-stack --lib -- \
  --exact config::tests::race_defaults_are_the_ones_the_design_settled_on --test-threads=1 2>&1 \
  | grep -E "test result|panicked|assertion|error" | head -20

echo "=================== handshake regression ==================="
timeout 120 bash scripts/cargo.sh test -p watt-stack --lib -- \
  --exact tcp::tests::completes_a_handshake_and_relays_payload_both_ways --test-threads=1 2>&1 \
  | grep -E "test result|panicked|assertion|error" | head -20
echo "(exit above 124 means it timed out and hung)"

echo "=================== race winner / SO_ERROR ==================="
timeout 120 bash scripts/cargo.sh test -p watt-stack --lib -- \
  --exact \
  tcp::tests::a_dial_still_in_progress_is_never_declared_the_winner \
  tcp::tests::a_live_candidate_wins_while_a_stuck_one_is_still_dialling \
  tcp::tests::a_serial_window_still_completes_a_handshake_and_relays_payload \
  tcp::tests::a_window_with_two_dials_still_has_somewhere_to_go \
  --test-threads=1 2>&1 | grep -E "test result|panicked|assertion|error" | head -20

echo "=================== candidate/dial ==================="
bash scripts/wsl-candidate-test.sh 2>&1 | tail -20
