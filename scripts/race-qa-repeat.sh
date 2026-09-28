#!/usr/bin/env bash
# Repeat the racing tests a few times to shake out flakiness in the two that
# depend on real socket timing (`window_rotates_before_client_budget` and the
# refused-candidate tests).
set -uo pipefail

cd /mnt/d/4

for run in 1 2 3; do
  echo "########## repeat $run ##########"
  out="$(bash scripts/race-qa-run.sh 2>&1)"
  code=$?
  printf '%s\n' "$out" | grep -E "race QA tests|FAIL"
  echo "run $run exit: $code"
done
