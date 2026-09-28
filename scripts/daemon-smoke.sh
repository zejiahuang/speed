#!/usr/bin/env bash
#
# End-to-end test for watt-daemon, the command line driver.
#
# The smoke and stress scripts already prove the engine relays traffic. What is
# only true of the daemon is everything around it: which copy of the rules it
# starts from, whether a signal really reloads them, and whether a stop request
# leaves a clean counter line behind. So this drives those, and uses the tunnel
# itself as the witness.
#
# The reload check is the interesting one. A rule document is written to disk
# pointing a domain at one address, the daemon is started on it, and the domain is
# asked for over DNS — which the kernel answers from the rule set, so the answer
# *is* the rule set. The document at the same path is then rewritten to point at a
# different address, the daemon is signalled, and the same question is asked
# again. The answer changing is proof that the signal reached the fetcher, the
# parser, and the running engine's route table, in that order.
#
#   scripts/daemon-smoke.sh
#
# It elevates itself, because the tunnel half needs CAP_NET_ADMIN. The build and
# the --check half happen before elevation, which is also how the script proves
# that --check needs no privileges.
#
# Environment:
#   TUN_NAME       interface to create (default: watt0)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/daemon"

TUN_NAME="${TUN_NAME:-watt0}"
TUN_ADDRESS="198.18.0.1"
TUN_PREFIX="15"
TEST_NET="203.0.113.0/24"
DNS_ADDRESS="203.0.113.53"
PROBE_DOMAIN="probe.daemon.test"
PROBE_FIRST="203.0.113.10"
PROBE_SECOND="203.0.113.20"

# Carried across the elevation below: sudo starts a fresh shell, and a tally that
# resets halfway through would report only half the run.
PASSED="${DAEMON_PASSED:-0}"
FAILURES="${DAEMON_FAILURES:-0}"

RULES="$WORK_DIR/rules.json"
CACHE_DIR="$WORK_DIR/cache"
DAEMON_LOG="$TMP_DIR/daemon-smoke-daemon.log"

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

DAEMON_BIN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --daemon-bin) DAEMON_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "daemon-smoke: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$DAEMON_BIN" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
  echo "daemon-smoke: building the daemon"
  bash "$SCRIPTS_DIR/cargo.sh" build -p watt-daemon
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
fi

if [ ! -x "$DAEMON_BIN" ]; then
  echo "daemon-smoke: daemon binary not found: $DAEMON_BIN" >&2
  exit 1
fi

check() { # <name> <ok: 0 for pass> <detail>
  if [ "$2" -eq 0 ]; then
    echo "CHECK PASS $1 ($3)"
    PASSED=$((PASSED + 1))
  else
    echo "CHECK FAIL $1 ($3)"
    FAILURES=$((FAILURES + 1))
  fi
}

# Run a command, capture its output, and leave its status in RC without tripping
# `set -e`.
RC=0
capture() { # <logfile> <command...>
  set +e
  "${@:2}" >"$1" 2>&1
  RC=$?
  set -e
}

write_rules() { # <path> <address>
  cat >"$1" <<JSON
{
  "meta": { "version": "daemon-smoke", "update_time": "2026-09-19T00:00:00Z" },
  "groups": [
    {
      "group": "Daemon smoke",
      "entries": [
        {
          "id": "daemon-smoke",
          "name": "Daemon Smoke",
          "domains": ["$PROBE_DOMAIN"],
          "ips": ["$2"],
          "port": "443",
          "isPlaceholder": false
        }
      ]
    }
  ]
}
JSON
}

wait_for_line() { # <file> <pattern> <seconds>
  local file="$1" pattern="$2" seconds="$3"
  local attempts=$(( seconds * 10 ))
  local attempt
  for attempt in $(seq 1 "$attempts"); do
    if grep -q "$pattern" "$file" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

count_lines() { # <file> <pattern>
  grep -c "$2" "$1" 2>/dev/null || true
}

mkdir -p "$TMP_DIR"
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"

echo "daemon-smoke: binary $DAEMON_BIN"

# =========================================================================
# Part 1 — the control plane. Runs as the invoking user, which is the point.
# =========================================================================
if [ "$(id -u)" -ne 0 ]; then
  echo
  echo "--- part 1: rule lifecycle and argument handling (no root) ---"

  capture "$WORK_DIR/help.log" "$DAEMON_BIN" --help
  check "--help exits 0" "$RC" "status $RC"
  if grep -q 'SIGUSR1' "$WORK_DIR/help.log"; then
    check "--help documents the signals" 0 "found SIGUSR1"
  else
    check "--help documents the signals" 1 "SIGUSR1 missing from the help text"
  fi

  capture "$WORK_DIR/badflag.log" "$DAEMON_BIN" --not-a-flag
  if [ "$RC" -eq 2 ]; then
    check "an unknown flag is refused" 0 "status 2"
  else
    check "an unknown flag is refused" 1 "status $RC, expected 2"
  fi

  # An empty cache directory and no network: the built-in document is all there is.
  capture "$WORK_DIR/builtin.log" \
    "$DAEMON_BIN" --check --offline --cache-dir "$WORK_DIR/cache-builtin"
  check "--check runs without root" "$RC" "status $RC"
  if grep -q 'origin=builtin' "$WORK_DIR/builtin.log"; then
    check "an empty cache falls back to the built-in rules" 0 "$(grep -m1 ' rules ' "$WORK_DIR/builtin.log")"
  else
    check "an empty cache falls back to the built-in rules" 1 "$(grep -m1 ' rules ' "$WORK_DIR/builtin.log")"
  fi

  write_rules "$RULES" "$PROBE_FIRST"

  capture "$WORK_DIR/pinned.log" \
    "$DAEMON_BIN" --check --rules-file "$RULES" --probe "$PROBE_DOMAIN"
  check "a pinned document compiles" "$RC" "status $RC"
  if grep -q 'origin=provided' "$WORK_DIR/pinned.log"; then
    check "a pinned document is reported as provided" 0 "$(grep -m1 ' rules ' "$WORK_DIR/pinned.log")"
  else
    check "a pinned document is reported as provided" 1 "$(grep -m1 ' rules ' "$WORK_DIR/pinned.log")"
  fi
  if grep -q "$PROBE_FIRST" "$WORK_DIR/pinned.log"; then
    check "--probe resolves through the document" 0 "$(grep -m1 ' probe ' "$WORK_DIR/pinned.log")"
  else
    check "--probe resolves through the document" 1 "$(grep -m1 ' probe ' "$WORK_DIR/pinned.log" || echo 'no probe line')"
  fi

  capture "$WORK_DIR/contradiction.log" \
    "$DAEMON_BIN" --check --rules-file "$RULES" --offline
  if [ "$RC" -eq 2 ]; then
    check "a pinned document plus --offline is refused" 0 "status 2"
  else
    check "a pinned document plus --offline is refused" 1 "status $RC, expected 2"
  fi

  # A path as the URL exercises the file fetcher, so the whole download-and-store
  # path is tested without a network.
  capture "$WORK_DIR/download.log" \
    "$DAEMON_BIN" --check --rules-url "$RULES" --cache-dir "$CACHE_DIR"
  check "a path URL is fetched without a network" "$RC" "status $RC"
  if grep -q 'origin=downloaded' "$WORK_DIR/download.log"; then
    check "a cold cache is reported as downloaded" 0 "$(grep -m1 ' refresh ' "$WORK_DIR/download.log")"
  else
    check "a cold cache is reported as downloaded" 1 "$(grep -m1 ' refresh ' "$WORK_DIR/download.log" || echo 'no refresh line')"
  fi

  capture "$WORK_DIR/cached.log" \
    "$DAEMON_BIN" --check --rules-url "$RULES" --cache-dir "$CACHE_DIR"
  if grep -q 'origin=cache' "$WORK_DIR/cached.log"; then
    check "a warm cache is preferred to the network" 0 "$(grep -m1 ' rules ' "$WORK_DIR/cached.log")"
  else
    check "a warm cache is preferred to the network" 1 "$(grep -m1 ' rules ' "$WORK_DIR/cached.log")"
  fi

  echo
  echo "daemon-smoke: creating a TUN device needs root, re-executing under sudo"
  # `env` rather than a plain re-exec: sudo resets the environment, so every
  # override the caller set — and the tally so far — would be lost on the way up.
  exec sudo env \
    "PATH=$PATH" \
    "TUN_NAME=$TUN_NAME" \
    "DAEMON_PASSED=$PASSED" \
    "DAEMON_FAILURES=$FAILURES" \
    bash "$0" --daemon-bin "$DAEMON_BIN"
fi

# =========================================================================
# Part 2 — the tunnel, which needs root.
# =========================================================================
for tool in ip python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "daemon-smoke: $tool is required" >&2
    exit 1
  fi
done

DAEMON_PID=""
cleanup() {
  set +e
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
  ip route del "$TEST_NET" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  return 0
}
trap cleanup EXIT

echo
echo "--- part 2: the tunnel ---"
ip link del "$TUN_NAME" 2>/dev/null || true
rm -f "$DAEMON_LOG"

# Start from a document that points the domain at the first address.
write_rules "$RULES" "$PROBE_FIRST"
rm -rf "$CACHE_DIR"

"$DAEMON_BIN" \
  --tun "$TUN_NAME" \
  --address "$TUN_ADDRESS/$TUN_PREFIX" \
  --rules-url "$RULES" \
  --cache-dir "$CACHE_DIR" \
  --tick 5 \
  --stats-interval 5 \
  >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

if ! wait_for_line "$DAEMON_LOG" '\] tun ' 15; then
  echo "daemon-smoke: the daemon did not open the tunnel" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi
grep -E ' rules | refresh | tun ' "$DAEMON_LOG" || true

ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$TEST_NET" dev "$TUN_NAME"

# The kernel answers for a rule owned name itself, so asking for it reads the rule
# set back out of the running engine.
before="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" --domain "$PROBE_DOMAIN" || true)"
echo "daemon-smoke: before reload: $before"
if [ "$before" = "DNS ANSWERS $PROBE_DOMAIN $PROBE_FIRST" ]; then
  check "the running kernel answers from the loaded document" 0 "$before"
else
  check "the running kernel answers from the loaded document" 1 "$before"
fi

# Rewrite the document at the same path, then ask for a reload. The document has
# to change *before* the signal, or the refresh reads the copy it already has. The
# start-up refresh means the log already holds a refresh line, so the check counts
# them rather than waiting for one to appear.
write_rules "$RULES" "$PROBE_SECOND"
refreshes_before="$(count_lines "$DAEMON_LOG" '\] refresh ')"
kill -USR1 "$DAEMON_PID"
reloaded=1
for _ in $(seq 1 100); do
  if [ "$(count_lines "$DAEMON_LOG" '\] refresh ')" -gt "$refreshes_before" ]; then
    reloaded=0
    break
  fi
  sleep 0.1
done
if [ "$reloaded" -eq 0 ]; then
  check "SIGUSR1 produced a refresh" 0 "$(grep '\] refresh ' "$DAEMON_LOG" | tail -n 1)"
else
  check "SIGUSR1 produced a refresh" 1 "no new refresh line within 10s"
fi

after="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" --domain "$PROBE_DOMAIN" || true)"
echo "daemon-smoke: after reload:  $after"
if [ "$after" = "DNS ANSWERS $PROBE_DOMAIN $PROBE_SECOND" ]; then
  check "the reload reached the running engine" 0 "$after"
else
  check "the reload reached the running engine" 1 "$after"
fi

# A stop request has to leave a clean counter line behind.
kill -TERM "$DAEMON_PID"
set +e
wait "$DAEMON_PID"
DAEMON_STATUS=$?
set -e
DAEMON_PID=""

if [ "$DAEMON_STATUS" -eq 0 ]; then
  check "SIGTERM stops the daemon cleanly" 0 "status 0"
else
  check "SIGTERM stops the daemon cleanly" 1 "status $DAEMON_STATUS"
fi
if grep -q '\] stop ' "$DAEMON_LOG"; then
  check "the daemon reported its final counters" 0 "$(grep '\] stop ' "$DAEMON_LOG" | tail -n 1)"
else
  check "the daemon reported its final counters" 1 "no stop line"
fi

echo
echo "===================== daemon log ====================="
cat "$DAEMON_LOG"
echo "====================================================="

if [ "$FAILURES" -eq 0 ]; then
  echo "daemon-smoke: PASS checks=$PASSED"
  exit 0
fi

echo "daemon-smoke: FAIL failures=$FAILURES passed=$PASSED" >&2
exit 1
