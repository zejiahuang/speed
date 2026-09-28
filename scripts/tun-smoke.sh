#!/usr/bin/env bash
#
# End-to-end smoke test for the full-traffic kernel on a real TUN device.
#
# This is the host stand-in for "install the VPN on a phone and browse". It is
# the only test in the repository that exercises the path Android will use: a
# real TUN descriptor, real kernel routing, real upstream sockets.
#
#   scripts/tun-smoke.sh
#
# It elevates itself, because creating a TUN device needs CAP_NET_ADMIN. The
# build happens before elevation so the target directory stays owned by the
# invoking user. Everything the run creates is removed on exit.
#
# Environment:
#   TUN_NAME        interface to create            (default: watt0)
#   ENGINE_SECONDS  hard limit on the kernel's run (default: 120)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/smoke"

TUN_NAME="${TUN_NAME:-watt0}"
# 198.18.0.0/15 is reserved for benchmarking by RFC 2544: never routable, so it
# is a safe private identity for a tunnel on a machine with a live network.
TUN_ADDRESS="198.18.0.1"
TUN_PREFIX="15"
# TEST-NET-3 (RFC 5737). Routed into the tunnel, so nothing the host does on the
# real network can collide with the test.
TEST_NET="203.0.113.0/24"
ENGINE_SECONDS="${ENGINE_SECONDS:-120}"

ENGINE_LOG="$TMP_DIR/tun-smoke-engine.log"
SERVER_LOG="$TMP_DIR/tun-smoke-servers.log"
CLIENT_LOG="$TMP_DIR/tun-smoke-client.log"
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
    *) echo "tun-smoke: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Build first. The Rust toolchain lives in the invoking user's home, and a root
# owned target directory would be unusable for them afterwards.
if [ -z "$ENGINE_BIN" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
  echo "tun-smoke: building the example"
  bash "$SCRIPTS_DIR/cargo.sh" build --example tun_smoke
  ENGINE_BIN="$CARGO_TARGET_DIR/debug/examples/tun_smoke"
fi

if [ ! -x "$ENGINE_BIN" ]; then
  echo "tun-smoke: engine binary not found: $ENGINE_BIN" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "tun-smoke: creating a TUN device needs root, re-executing under sudo"
  # `env` rather than a plain re-exec: sudo resets the environment, so every
  # override the caller set would silently be lost on the way up and the run
  # would quietly fall back to the defaults.
  exec sudo env \
    "PATH=$PATH" \
    "TUN_NAME=$TUN_NAME" \
    "ENGINE_SECONDS=$ENGINE_SECONDS" \
    bash "$0" --engine-bin "$ENGINE_BIN"
fi

for tool in ip python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "tun-smoke: $tool is required" >&2
    exit 1
  fi
done

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

# A leftover interface from an interrupted run would be attached to rather than
# created, and would carry stale addresses.
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

# --- 1. Local servers the relayed flows are rewritten onto. -----------------
echo "tun-smoke: starting loopback servers"
python3 "$SCRIPTS_DIR/tun_smoke_servers.py" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

if ! wait_for_line "$SERVER_LOG" '^SERVERS ' 10; then
  echo "tun-smoke: the loopback servers did not start" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi

SERVER_PORTS="$(sed -n 's/^SERVERS tcp=\([0-9]*\) udp=\([0-9]*\)$/\1 \2/p' "$SERVER_LOG")"
TCP_PORT="${SERVER_PORTS%% *}"
UDP_PORT="${SERVER_PORTS##* }"
if [ -z "$TCP_PORT" ] || [ -z "$UDP_PORT" ]; then
  echo "tun-smoke: could not read the server ports out of $SERVER_LOG" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
echo "tun-smoke: loopback servers tcp=$TCP_PORT udp=$UDP_PORT"

# --- 2. The kernel, on a real TUN device. ----------------------------------
echo "tun-smoke: starting the kernel on $TUN_NAME"
"$ENGINE_BIN" \
  --tun "$TUN_NAME" \
  --seconds "$ENGINE_SECONDS" \
  --tcp-port "$TCP_PORT" \
  --udp-port "$UDP_PORT" \
  --stop-file "$STOP_FILE" >"$ENGINE_LOG" 2>&1 &
ENGINE_PID=$!

if ! wait_for_line "$ENGINE_LOG" '^TUN_READY ' 15; then
  echo "tun-smoke: the kernel did not bring up $TUN_NAME" >&2
  cat "$ENGINE_LOG" >&2
  exit 1
fi
grep -E '^(RULES|RULE_DOMAIN|LOADED|TUN_READY|SMOKE|REWRITE) ' "$ENGINE_LOG" || true

# --- 3. Hand the interface to the operating system. -------------------------
# The library deliberately does not do this: on Android `VpnService.Builder`
# owns it instead, which is the one part of setup the two platforms do not share.
echo "tun-smoke: addressing $TUN_NAME and routing $TEST_NET into it"
ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$TEST_NET" dev "$TUN_NAME"
ip -brief addr show "$TUN_NAME"
ip route show "$TEST_NET"

# --- 4. Ordinary client traffic, carried by the kernel's own route table. ---
SMOKE_LINE="$(grep -m1 '^SMOKE ' "$ENGINE_LOG")"
DNS_ADDRESS="$(sed -n 's/.*dns=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
RULE_ADDRESS="$(sed -n 's/.*rule=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
UDP_ADDRESS="$(sed -n 's/.*udp=\([0-9.]*\).*/\1/p' <<<"$SMOKE_LINE")"
SMOKE_DOMAIN="$(sed -n 's/.*domain=\([^ ]*\).*/\1/p' <<<"$SMOKE_LINE")"
PARALLEL="$(sed -n 's/.*parallel=\([0-9]*\).*/\1/p' <<<"$SMOKE_LINE")"
EXPECT_ANSWERS="$(sed -n 's/^RULE_DOMAIN [^ ]* ips=//p' "$ENGINE_LOG")"

echo "tun-smoke: client -> dns=$DNS_ADDRESS rule=$RULE_ADDRESS udp=$UDP_ADDRESS domain=$SMOKE_DOMAIN"
CLIENT_STATUS=0
python3 "$SCRIPTS_DIR/tun_smoke_client.py" \
  --dns-address "$DNS_ADDRESS" \
  --rule-address "$RULE_ADDRESS" \
  --udp-address "$UDP_ADDRESS" \
  --udp-port "$UDP_PORT" \
  --domain "$SMOKE_DOMAIN" \
  --expect-answers "$EXPECT_ANSWERS" \
  --parallel "$PARALLEL" >"$CLIENT_LOG" 2>&1 || CLIENT_STATUS=$?
cat "$CLIENT_LOG"

# --- 5. Ask the kernel for its report. -------------------------------------
touch "$STOP_FILE"
if ! wait_for_line "$ENGINE_LOG" '^RESULT ' 30; then
  echo "tun-smoke: the kernel never produced a verdict" >&2
fi
wait "$ENGINE_PID" 2>/dev/null || true
ENGINE_PID=""

echo
echo "===================== kernel log ====================="
cat "$ENGINE_LOG"
echo "======================================================"

if grep -q '^RESULT PASS' "$ENGINE_LOG" && [ "$CLIENT_STATUS" -eq 0 ]; then
  echo "tun-smoke: PASS"
  exit 0
fi

echo "tun-smoke: FAIL (client exit status $CLIENT_STATUS)" >&2
exit 1
