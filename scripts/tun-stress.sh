#!/usr/bin/env bash
#
# Load and endurance test for the full-traffic kernel, on a real TUN device.
#
# Where tun-smoke.sh asks "does the path work", this asks "does it keep working":
# hundreds of connections at once, a bulk transfer big enough to exercise
# backpressure, a wide UDP table, and enough repetition that a descriptor or
# buffer leak has nowhere to hide.
#
#   scripts/tun-stress.sh
#
# It elevates itself, because creating a TUN device needs CAP_NET_ADMIN. The
# build happens before elevation so the target directory stays owned by the
# invoking user. Everything the run creates is removed on exit.
#
# Environment:
#   TUN_NAME              interface to create                 (default: watt0)
#   STRESS_SECONDS        hard limit on the kernel's run      (raised to fit the plan)
#   STRESS_SETTLE         quiet seconds before the verdict    (default: 5)
#   STRESS_SAMPLE_SECONDS gap between memory samples          (default: 1)
#   STRESS_PARALLEL       connections per wave                (default: 25)
#   STRESS_WAVES          waves per round                     (default: 8)
#   STRESS_ROUNDS         rounds of the whole load            (default: 1)
#   STRESS_UDP_FLOWS      distinct UDP flows per round        (default: 200)
#   STRESS_BULK_BYTES     bytes per bulk transfer             (default: 8388608)
#   STRESS_LISTENERS      per-destination listener ceiling    (default: 64)
#   STRESS_UDP_CEILING    UDP flow table ceiling              (default: kernel's own)
#
# An endurance run is just more rounds:
#   STRESS_ROUNDS=60 STRESS_SAMPLE_SECONDS=10 scripts/tun-stress.sh

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/stress"

TUN_NAME="${TUN_NAME:-watt0}"
TUN_ADDRESS="198.18.0.1"
TUN_PREFIX="15"
TEST_NET="203.0.113.0/24"

STRESS_SECONDS="${STRESS_SECONDS:-0}"
STRESS_SETTLE="${STRESS_SETTLE:-5}"
STRESS_SAMPLE_SECONDS="${STRESS_SAMPLE_SECONDS:-1}"
STRESS_PARALLEL="${STRESS_PARALLEL:-25}"
STRESS_WAVES="${STRESS_WAVES:-8}"
STRESS_ROUNDS="${STRESS_ROUNDS:-1}"
STRESS_UDP_FLOWS="${STRESS_UDP_FLOWS:-200}"
STRESS_BULK_BYTES="${STRESS_BULK_BYTES:-8388608}"
# A burst of simultaneous connections to one host needs one listening socket
# each, so this has to clear the widest wave. Left at the kernel's own default on
# purpose: the point of the default run is to prove that default is enough.
STRESS_LISTENERS="${STRESS_LISTENERS:-64}"
# Zero leaves the kernel's own ceiling in place, which is what a run should be
# testing. A small value is how the eviction path gets exercised on purpose.
STRESS_UDP_CEILING="${STRESS_UDP_CEILING:-0}"

# The kernel's lifetime has to outlast every round, and how long a round takes is
# not known until the run is over. A backstop that is too low does not fail
# loudly: the kernel stops, the client sits waiting for replies that will never
# come, and the run ends with a verdict that says nothing about the kernel. So the
# caller's value is treated as a floor to raise rather than a limit to honour.
STRESS_SECONDS_MIN=$(( STRESS_ROUNDS * 60 + 120 ))
if [ "$STRESS_SECONDS" -lt "$STRESS_SECONDS_MIN" ]; then
  if [ "$STRESS_SECONDS" -gt 0 ]; then
    echo "tun-stress: raising STRESS_SECONDS from $STRESS_SECONDS to $STRESS_SECONDS_MIN so the kernel outlives all $STRESS_ROUNDS round(s)"
  fi
  STRESS_SECONDS="$STRESS_SECONDS_MIN"
fi

CONNECTIONS=$(( STRESS_PARALLEL * STRESS_WAVES * STRESS_ROUNDS ))
UDP_FLOWS=$(( STRESS_UDP_FLOWS * STRESS_ROUNDS ))
BULK_TOTAL=$(( STRESS_BULK_BYTES * STRESS_ROUNDS ))

ENGINE_LOG="$TMP_DIR/tun-stress-engine.log"
SERVER_LOG="$TMP_DIR/tun-stress-servers.log"
CLIENT_LOG="$TMP_DIR/tun-stress-client.log"
STOP_FILE="$WORK_DIR/stop"

usage() {
  # Every comment line after the shebang, so the help text cannot drift out of
  # step with the header the way a hard coded line range does.
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

ENGINE_BIN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --engine-bin) ENGINE_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "tun-stress: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Build first. The Rust toolchain lives in the invoking user's home, and a root
# owned target directory would be unusable for them afterwards.
if [ -z "$ENGINE_BIN" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
  echo "tun-stress: building the example"
  bash "$SCRIPTS_DIR/cargo.sh" build --example tun_stress
  ENGINE_BIN="$CARGO_TARGET_DIR/debug/examples/tun_stress"
fi

if [ ! -x "$ENGINE_BIN" ]; then
  echo "tun-stress: engine binary not found: $ENGINE_BIN" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "tun-stress: creating a TUN device needs root, re-executing under sudo"
  # `env` rather than a plain re-exec: sudo resets the environment, so every
  # override the caller set would silently be lost on the way up and the run
  # would quietly fall back to the defaults.
  exec sudo env \
    "PATH=$PATH" \
    "TUN_NAME=$TUN_NAME" \
    "STRESS_SECONDS=$STRESS_SECONDS" \
    "STRESS_SETTLE=$STRESS_SETTLE" \
    "STRESS_SAMPLE_SECONDS=$STRESS_SAMPLE_SECONDS" \
    "STRESS_PARALLEL=$STRESS_PARALLEL" \
    "STRESS_WAVES=$STRESS_WAVES" \
    "STRESS_ROUNDS=$STRESS_ROUNDS" \
    "STRESS_UDP_FLOWS=$STRESS_UDP_FLOWS" \
    "STRESS_BULK_BYTES=$STRESS_BULK_BYTES" \
    "STRESS_LISTENERS=$STRESS_LISTENERS" \
    "STRESS_UDP_CEILING=$STRESS_UDP_CEILING" \
    bash "$0" --engine-bin "$ENGINE_BIN"
fi

for tool in ip python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "tun-stress: $tool is required" >&2
    exit 1
  fi
done

# One descriptor per flow in the kernel and one per flow in the client. The
# default soft limit of 1024 would turn a load test into a limit test.
ulimit -n 16384 2>/dev/null || true

mkdir -p "$TMP_DIR" "$WORK_DIR"
rm -f "$STOP_FILE" "$ENGINE_LOG" "$SERVER_LOG" "$CLIENT_LOG"

SERVER_PID=""
ENGINE_PID=""

cleanup() {
  set +e
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "$ENGINE_PID" ] && kill "$ENGINE_PID" 2>/dev/null
  ip route del "$TEST_NET" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  return 0
}
trap cleanup EXIT

ip link del "$TUN_NAME" 2>/dev/null || true

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

echo "tun-stress: plan = $CONNECTIONS connections ($STRESS_WAVES waves of $STRESS_PARALLEL, $STRESS_ROUNDS round(s)), $UDP_FLOWS udp flows, $BULK_TOTAL bytes bulk, kernel lifetime ${STRESS_SECONDS}s"

# --- 1. Local servers the relayed flows are rewritten onto. -----------------
python3 "$SCRIPTS_DIR/tun_smoke_servers.py" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

if ! wait_for_line "$SERVER_LOG" '^SERVERS ' 10; then
  echo "tun-stress: the loopback servers did not start" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi

SERVER_PORTS="$(sed -n 's/^SERVERS tcp=\([0-9]*\) udp=\([0-9]*\)$/\1 \2/p' "$SERVER_LOG")"
TCP_PORT="${SERVER_PORTS%% *}"
UDP_PORT="${SERVER_PORTS##* }"
if [ -z "$TCP_PORT" ] || [ -z "$UDP_PORT" ]; then
  echo "tun-stress: could not read the server ports out of $SERVER_LOG" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
echo "tun-stress: loopback servers tcp=$TCP_PORT udp=$UDP_PORT"

# --- 2. The kernel, on a real TUN device. ----------------------------------
echo "tun-stress: starting the kernel on $TUN_NAME"
"$ENGINE_BIN" \
  --tun "$TUN_NAME" \
  --seconds "$STRESS_SECONDS" \
  --settle "$STRESS_SETTLE" \
  --sample-seconds "$STRESS_SAMPLE_SECONDS" \
  --tcp-port "$TCP_PORT" \
  --udp-port "$UDP_PORT" \
  --connections "$CONNECTIONS" \
  --udp-flows "$UDP_FLOWS" \
  --bulk-bytes "$BULK_TOTAL" \
  --listener-ceiling "$STRESS_LISTENERS" \
  --udp-ceiling "$STRESS_UDP_CEILING" \
  --stop-file "$STOP_FILE" >"$ENGINE_LOG" 2>&1 &
ENGINE_PID=$!

if ! wait_for_line "$ENGINE_LOG" '^TUN_READY ' 15; then
  echo "tun-stress: the kernel did not bring up $TUN_NAME" >&2
  cat "$ENGINE_LOG" >&2
  exit 1
fi
grep -E '^(RULES|TUN_READY|SMOKE|REWRITE|BASELINE) ' "$ENGINE_LOG" || true

# --- 3. Hand the interface to the operating system. -------------------------
echo "tun-stress: addressing $TUN_NAME and routing $TEST_NET into it"
ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$TEST_NET" dev "$TUN_NAME"

# --- 4. Ordinary client traffic, carried by the kernel's own route table. ---
SMOKE_LINE="$(grep -m1 '^SMOKE ' "$ENGINE_LOG")"
DNS_ADDRESS="$(sed -n 's/.*dns=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
RULE_ADDRESS="$(sed -n 's/.*rule=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
UDP_ADDRESS="$(sed -n 's/.*udp=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
SMOKE_DOMAIN="$(sed -n 's/.*domain=\([^ ]*\).*/\1/p' <<<"$SMOKE_LINE")"

CLIENT_STATUS=0
for round in $(seq 1 "$STRESS_ROUNDS"); do
  echo "tun-stress: round $round/$STRESS_ROUNDS"
  python3 "$SCRIPTS_DIR/tun_smoke_client.py" \
    --dns-address "$DNS_ADDRESS" \
    --rule-address "$RULE_ADDRESS" \
    --udp-address "$UDP_ADDRESS" \
    --udp-port "$UDP_PORT" \
    --domain "$SMOKE_DOMAIN" \
    --expect-answers "203.0.113.10,203.0.113.11" \
    --parallel "$STRESS_PARALLEL" \
    --tcp-waves "$STRESS_WAVES" \
    --udp-flows "$STRESS_UDP_FLOWS" \
    --bulk-bytes "$STRESS_BULK_BYTES" >>"$CLIENT_LOG" 2>&1 || CLIENT_STATUS=$?
  if [ "$CLIENT_STATUS" -ne 0 ]; then
    echo "tun-stress: round $round failed, stopping"
    break
  fi
done

# --- 5. Ask the kernel for its report. -------------------------------------
#
# The kernel settles before it reports: after the stop file appears it keeps
# running for STRESS_SETTLE seconds so that idle flows are reaped and the
# descriptor count can be compared against the baseline. The verdict therefore
# arrives later than the stop file, and a fixed window shorter than the settle
# would announce a missing verdict on a run that is about to print PASS. Derive
# the window from the settle so the message only ever means what it says.
touch "$STOP_FILE"
if ! wait_for_line "$ENGINE_LOG" '^RESULT ' "$(( STRESS_SETTLE + 60 ))"; then
  echo "tun-stress: no verdict after $(( STRESS_SETTLE + 60 ))s (settle ${STRESS_SETTLE}s plus 60s); the kernel is still running" >&2
fi
wait "$ENGINE_PID" 2>/dev/null || true
ENGINE_PID=""

echo
echo "===================== client log ====================="
cat "$CLIENT_LOG"
echo
echo "===================== kernel log ====================="
grep -v '^SAMPLE ' "$ENGINE_LOG" || true
echo "--- samples ---"
grep '^SAMPLE ' "$ENGINE_LOG" || true
echo "======================================================"

if grep -q '^RESULT PASS' "$ENGINE_LOG" && [ "$CLIENT_STATUS" -eq 0 ]; then
  echo "tun-stress: PASS"
  exit 0
fi

echo "tun-stress: FAIL (client exit status $CLIENT_STATUS)" >&2
exit 1
