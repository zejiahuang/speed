#!/usr/bin/env bash
#
# Syntax-check every script in this directory.
#
# A file rather than an inline `wsl.exe -- bash -lc "..."`: the Windows argument
# passing eats `$`, so a loop variable arrives empty and the check silently
# "passes" nothing. That is the same trap that makes every inline invocation in
# this project unreliable, and the reason all logic lives in files like this one.
#
#   bash scripts/check-scripts.sh

set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failures=0

for path in "$DIR"/*.sh; do
  name="$(basename "$path")"
  if bash -n "$path" 2>/dev/null; then
    printf 'ok    %s\n' "$name"
  else
    printf 'FAIL  %s\n' "$name"
    bash -n "$path"
    failures=$((failures + 1))
  fi
done

for path in "$DIR"/*.py; do
  name="$(basename "$path")"
  if python3 -m py_compile "$path" 2>/dev/null; then
    printf 'ok    %s\n' "$name"
  else
    printf 'FAIL  %s\n' "$name"
    python3 -m py_compile "$path"
    failures=$((failures + 1))
  fi
done

if [ "$failures" -eq 0 ]; then
  echo "check-scripts: PASS"
  exit 0
fi

echo "check-scripts: FAIL failures=$failures" >&2
exit 1
