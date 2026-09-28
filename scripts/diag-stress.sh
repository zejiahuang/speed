#!/usr/bin/env bash
#
# One-shot diagnostic snapshot of a running stress/endurance test.
#
# Written as a file rather than passed inline to `wsl.exe` because Windows
# argument passing mangles quoting: a `^CLIENT RESULT` pattern inside an inline
# `bash -lc '...'` arrives as `RESULT"` and the shell then tries to run `round`
# as a command. Everything that needs quoting lives here instead.
#
#   wsl.exe -d Ubuntu-22.04 -- bash /mnt/d/4/scripts/diag-stress.sh

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$REPO_DIR/tmp"
ENGINE_LOG="$TMP_DIR/tun-stress-engine.log"
CLIENT_LOG="$TMP_DIR/tun-stress-client.log"
ENDURANCE_LOG="$TMP_DIR/tun-endurance.log"

echo "=== clock ==="
date -u '+%Y-%m-%dT%H:%M:%SZ'

echo
echo "=== processes ==="
# Only the fields that carry a signal: the full argument list includes the whole
# inherited PATH, which buries the numbers that matter.
ps -eo pid,stat,etime,pcpu,pmem,rss,comm \
  | grep -E 'tun_stress|tun_smoke|watt-daemon' \
  | grep -v grep

echo
echo "=== thread states of the engine ==="
engine_pid="$(pgrep -x tun_stress | head -n 1)"
if [ -n "$engine_pid" ]; then
  echo "engine pid=$engine_pid threads=$(ls /proc/$engine_pid/task | wc -l)"
  for task in /proc/$engine_pid/task/*; do
    printf '  tid=%s state=%s wchan=%s\n' \
      "$(basename "$task")" \
      "$(awk '{print $3}' "$task/stat" 2>/dev/null)" \
      "$(cat "$task/wchan" 2>/dev/null)"
  done
  echo "  open fds: $(ls /proc/$engine_pid/fd 2>/dev/null | wc -l)"
fi

echo
echo "=== thread states of the client ==="
client_pid="$(pgrep -f tun_smoke_client.py | head -n 1)"
if [ -n "$client_pid" ]; then
  echo "client pid=$client_pid threads=$(ls /proc/$client_pid/task | wc -l)"
  for task in /proc/$client_pid/task/*; do
    printf '  tid=%s state=%s wchan=%s\n' \
      "$(basename "$task")" \
      "$(awk '{print $3}' "$task/stat" 2>/dev/null)" \
      "$(cat "$task/wchan" 2>/dev/null)"
  done
  echo "  open fds: $(ls /proc/$client_pid/fd 2>/dev/null | wc -l)"
fi

echo
echo "=== rounds the shell script has announced ==="
grep -c 'round [0-9]*/' "$ENDURANCE_LOG" 2>/dev/null || echo "(none)"
tail -n 4 "$ENDURANCE_LOG" 2>/dev/null

echo
echo "=== client rounds completed ==="
grep -c '^CLIENT RESULT' "$CLIENT_LOG" 2>/dev/null || echo 0
echo "--- last two client blocks ---"
tail -n 12 "$CLIENT_LOG" 2>/dev/null

echo
echo "=== engine samples (tail) ==="
grep '^SAMPLE ' "$ENGINE_LOG" 2>/dev/null | tail -n 10

echo
echo "=== engine non-sample log ==="
grep -v '^SAMPLE ' "$ENGINE_LOG" 2>/dev/null

echo
echo "=== tun device ==="
ip -brief addr show watt0 2>/dev/null || echo "(watt0 gone)"
ip route show 203.0.113.0/24 2>/dev/null || echo "(no route)"
