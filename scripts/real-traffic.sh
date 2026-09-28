#!/usr/bin/env bash
#
# Real application traffic through the kernel, driven by curl.
#
# Everything else in this harness talks to the kernel with a protocol invented
# for the occasion — a line of text, or a fixed number of bytes. That proves the
# relay moves bytes. It does not prove it moves *traffic*, because the client was
# written by the same person who wrote the kernel and can be wrong in the same
# direction.
#
# This script removes that doubt. The client is `curl`, the server is a real TLS
# HTTP/1.1 server, and the verdict is built from three sources that know nothing
# about the kernel:
#
#   * curl's exit status and the HTTP status codes it received;
#   * the server's own connection and request counters;
#   * sha256 of the bytes that arrived, compared against a hash computed from the
#     same generator the server used.
#
# The kernel's counters are printed for diagnosis but never decide the outcome.
#
#   scripts/real-traffic.sh
#
# It elevates itself, because creating a TUN device needs CAP_NET_ADMIN.
#
# Environment:
#   TUN_NAME          interface to create              (default: watt1)
#   BIG_BYTES         bytes for the large download     (default: 8388608)
#   CHUNKED_BYTES     bytes for the chunked download   (default: 1000000)
#   UPLOAD_BYTES      bytes for the POST body          (default: 3145728)
#   KEEPALIVE_REQUESTS  requests sharing one connection (default: 5)
#   PARALLEL          simultaneous curl processes      (default: 8)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/real"

# A different interface and address from the other harnesses, so a stray watt0
# left behind by an interrupted run cannot be mistaken for this one's.
TUN_NAME="${TUN_NAME:-watt1}"
TUN_ADDRESS="198.19.0.1"
TUN_PREFIX="15"

# TEST-NET-3 (RFC 5737). Guaranteed never to be a real destination, which is what
# makes it safe to route into a tunnel on a machine with a working network.
TEST_NET="203.0.113.0/24"
RULE_ADDRESS="203.0.113.10"
DNS_ADDRESS="203.0.113.53"
DOMAIN="probe.test"

BIG_BYTES="${BIG_BYTES:-8388608}"
CHUNKED_BYTES="${CHUNKED_BYTES:-1000000}"
UPLOAD_BYTES="${UPLOAD_BYTES:-3145728}"
KEEPALIVE_REQUESTS="${KEEPALIVE_REQUESTS:-5}"
PARALLEL="${PARALLEL:-8}"

DAEMON_LOG="$TMP_DIR/real-daemon.log"
SERVER_LOG="$TMP_DIR/real-server.log"

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

DAEMON_BIN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --daemon-bin) DAEMON_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "real-traffic: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Build before elevating, so the target directory stays owned by the caller.
if [ -z "$DAEMON_BIN" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
  echo "real-traffic: building the daemon"
  # Quiet, because cargo's progress uses carriage returns that overwrite the line
  # the script is writing. Errors still come through.
  bash "$SCRIPTS_DIR/cargo.sh" build --quiet -p watt-daemon
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
fi

if [ ! -x "$DAEMON_BIN" ]; then
  echo "real-traffic: daemon binary not found: $DAEMON_BIN" >&2
  exit 1
fi

for tool in ip curl openssl python3 sha256sum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "real-traffic: $tool is required" >&2
    exit 1
  fi
done

if [ "$(id -u)" -ne 0 ]; then
  echo "real-traffic: creating a TUN device needs root, re-executing under sudo"
  # `env` rather than a plain re-exec: sudo resets the environment, so every
  # override the caller set would silently be lost on the way up.
  exec sudo env \
    "PATH=$PATH" \
    "TUN_NAME=$TUN_NAME" \
    "BIG_BYTES=$BIG_BYTES" \
    "CHUNKED_BYTES=$CHUNKED_BYTES" \
    "UPLOAD_BYTES=$UPLOAD_BYTES" \
    "KEEPALIVE_REQUESTS=$KEEPALIVE_REQUESTS" \
    "PARALLEL=$PARALLEL" \
    bash "$0" --daemon-bin "$DAEMON_BIN"
fi

mkdir -p "$TMP_DIR" "$WORK_DIR"
rm -f "$DAEMON_LOG" "$SERVER_LOG"
rm -f "$WORK_DIR"/{hello.txt,big.bin,chunked.bin,upload.bin,echo.txt,ka1.txt,ka2.txt}
rm -f "$WORK_DIR"/parallel-*.{out,rc,body}

SERVER_PID=""
DAEMON_PID=""

cleanup() {
  set +e
  [ -n "$SERVER_PID" ] && kill -TERM "$SERVER_PID" 2>/dev/null
  [ -n "$DAEMON_PID" ] && kill -TERM "$DAEMON_PID" 2>/dev/null
  sleep 0.3
  [ -n "$SERVER_PID" ] && kill -KILL "$SERVER_PID" 2>/dev/null
  [ -n "$DAEMON_PID" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  ip route del "$TEST_NET" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  return 0
}
trap cleanup EXIT

ip link del "$TUN_NAME" 2>/dev/null || true

wait_for_line() { # <file> <pattern> <seconds>
  local file="$1" pattern="$2" seconds="$3"
  local attempt
  for attempt in $(seq 1 $(( seconds * 10 ))); do
    if grep -q "$pattern" "$file" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

PASSED=0
FAILURES=0

check() { # <name> <ok: 0/1> <detail>
  if [ "$2" -eq 0 ]; then
    PASSED=$(( PASSED + 1 ))
    echo "CHECK PASS $1 ($3)"
  else
    FAILURES=$(( FAILURES + 1 ))
    echo "CHECK FAIL $1 ($3)"
  fi
}

# --- 1. A certificate and a real TLS server. --------------------------------
echo "real-traffic: generating a certificate for $DOMAIN"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$WORK_DIR/key.pem" -out "$WORK_DIR/cert.pem" \
  -days 1 -subj "/CN=$DOMAIN" \
  -addext "subjectAltName=DNS:$DOMAIN" >/dev/null 2>&1

echo "real-traffic: starting the TLS server"
python3 "$SCRIPTS_DIR/real_tls_server.py" \
  --cert "$WORK_DIR/cert.pem" --key "$WORK_DIR/key.pem" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

if ! wait_for_line "$SERVER_LOG" '^TLS_SERVER ' 10; then
  echo "real-traffic: the TLS server did not start" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
TLS_PORT="$(sed -n 's/^TLS_SERVER tls=\([0-9]*\)$/\1/p' "$SERVER_LOG")"
if [ -z "$TLS_PORT" ]; then
  echo "real-traffic: could not read the TLS port out of $SERVER_LOG" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
echo "real-traffic: TLS server on 127.0.0.1:$TLS_PORT"

# --- 2. A rule document that owns the domain. -------------------------------
RULES="$WORK_DIR/probe.json"
cat >"$RULES" <<JSON
{
  "meta": { "version": "real-traffic", "update_time": "2026-01-01T00:00:00Z" },
  "groups": [
    {
      "group": "Probe",
      "entries": [
        {
          "id": "probe",
          "name": "Probe",
          "nameZh": "探测",
          "domains": ["$DOMAIN"],
          "ips": ["$RULE_ADDRESS"],
          "port": "443",
          "isPlaceholder": false,
          "ipCountry": "ZZ",
          "ipCountryName": "Nowhere"
        }
      ]
    }
  ]
}
JSON

# --- 3. The kernel, steering the rule address at the local TLS server. ------
#
# The override is not a test shortcut, it is a necessity of this environment. The
# kernel's own upstream sockets are routed by the same table as everything else,
# so a connection to $RULE_ADDRESS would be captured by this very tunnel. On
# Android `VpnService.protect` is what keeps that from happening; a bare Linux
# host has no equivalent, so the destination is pointed at loopback instead.
echo "real-traffic: starting the daemon on $TUN_NAME"
"$DAEMON_BIN" \
  --tun "$TUN_NAME" \
  --address "$TUN_ADDRESS/$TUN_PREFIX" \
  --rules-file "$RULES" \
  --override "$RULE_ADDRESS:443=127.0.0.1:$TLS_PORT" \
  --stats-interval 5 \
  --tick 3600 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

if ! wait_for_line "$DAEMON_LOG" '\] tun ' 20; then
  echo "real-traffic: the daemon did not bring up $TUN_NAME" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi
grep -E '\] (rules|tun|override) ' "$DAEMON_LOG" || true

echo "real-traffic: addressing $TUN_NAME and routing $TEST_NET into it"
ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$TEST_NET" dev "$TUN_NAME"

# --- 4. curl, over a real TLS handshake. ------------------------------------
#
# `--resolve` supplies the address so the connection does not depend on the
# host's resolver, and `--http1.1` keeps the server's framing honest. The write
# out line is the only thing on stdout; bodies go to files.
CURL=(
  curl --silent --show-error --http1.1 --insecure --max-time 30
  --resolve "$DOMAIN:443:$RULE_ADDRESS"
  --write-out 'CURL %{http_code} %{size_download} %{num_connects} %{time_total}\n'
)

echo
echo "--- leg 1: DNS through the tunnel ---"
DNS_OUT=""
DNS_RC=0
DNS_OUT="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" --domain "$DOMAIN" 2>&1)" || DNS_RC=$?
echo "$DNS_OUT"
if [ "$DNS_RC" -eq 0 ] && [ "${DNS_OUT#DNS ANSWERS $DOMAIN }" = "$RULE_ADDRESS" ]; then
  check "the rule set answered DNS for $DOMAIN" 0 "$DNS_OUT"
else
  check "the rule set answered DNS for $DOMAIN" 1 "rc=$DNS_RC out=$DNS_OUT"
fi

echo
echo "--- leg 2: one HTTPS request ---"
HELLO_OUT=""
HELLO_RC=0
HELLO_OUT="$("${CURL[@]}" -o "$WORK_DIR/hello.txt" "https://$DOMAIN/hello" 2>&1)" || HELLO_RC=$?
echo "$HELLO_OUT"
HELLO_BODY="$(cat "$WORK_DIR/hello.txt" 2>/dev/null || true)"
if [ "$HELLO_RC" -eq 0 ] && [ "$HELLO_BODY" = "hello through the tunnel" ]; then
  check "curl completed a real TLS request" 0 "$HELLO_OUT"
else
  check "curl completed a real TLS request" 1 "rc=$HELLO_RC body=$HELLO_BODY"
fi

echo
echo "--- leg 3: keep-alive, $KEEPALIVE_REQUESTS requests on one connection ---"
KEEPALIVE_ARGS=()
for _ in $(seq 1 "$KEEPALIVE_REQUESTS"); do
  KEEPALIVE_ARGS+=( -o /dev/null "https://$DOMAIN/hello" )
done
KA_OUT=""
KA_RC=0
KA_OUT="$("${CURL[@]}" "${KEEPALIVE_ARGS[@]}" 2>&1)" || KA_RC=$?
echo "$KA_OUT"
KA_TOTAL="$(printf '%s\n' "$KA_OUT" | grep -c '^CURL ' || true)"
# curl reports num_connects=0 when a transfer reused the connection it already had,
# so exactly one of N transfers should have opened one.
KA_NEW="$(printf '%s\n' "$KA_OUT" | awk '/^CURL / && $4 != 0 {count++} END {print count+0}')"
if [ "$KA_RC" -eq 0 ] && [ "$KA_TOTAL" -eq "$KEEPALIVE_REQUESTS" ] && [ "$KA_NEW" -eq 1 ]; then
  check "keep-alive carried every request on one connection" 0 \
    "transfers=$KA_TOTAL new_connections=$KA_NEW"
else
  check "keep-alive carried every request on one connection" 1 \
    "rc=$KA_RC transfers=$KA_TOTAL new_connections=$KA_NEW"
fi

echo
echo "--- leg 4: large download, $BIG_BYTES bytes ---"
BIG_OUT=""
BIG_RC=0
BIG_OUT="$("${CURL[@]}" -o "$WORK_DIR/big.bin" "https://$DOMAIN/big?n=$BIG_BYTES" 2>&1)" || BIG_RC=$?
echo "$BIG_OUT"
BIG_EXPECT="$(python3 "$SCRIPTS_DIR/real_tls_server.py" --hash "$BIG_BYTES")"
BIG_ACTUAL="$(sha256sum "$WORK_DIR/big.bin" | awk '{print $1}')"
BIG_SIZE="$(stat -c %s "$WORK_DIR/big.bin")"
if [ "$BIG_RC" -eq 0 ] && [ "$BIG_ACTUAL" = "$BIG_EXPECT" ] && [ "$BIG_SIZE" -eq "$BIG_BYTES" ]; then
  check "the large download matched byte for byte" 0 "sha256=$BIG_ACTUAL bytes=$BIG_SIZE"
else
  check "the large download matched byte for byte" 1 \
    "rc=$BIG_RC bytes=$BIG_SIZE expected=$BIG_EXPECT actual=$BIG_ACTUAL"
fi

echo
echo "--- leg 5: chunked response, $CHUNKED_BYTES bytes ---"
CHUNKED_OUT=""
CHUNKED_RC=0
CHUNKED_OUT="$("${CURL[@]}" -o "$WORK_DIR/chunked.bin" "https://$DOMAIN/chunked?n=$CHUNKED_BYTES" 2>&1)" || CHUNKED_RC=$?
echo "$CHUNKED_OUT"
CHUNKED_EXPECT="$(python3 "$SCRIPTS_DIR/real_tls_server.py" --hash "$CHUNKED_BYTES")"
CHUNKED_ACTUAL="$(sha256sum "$WORK_DIR/chunked.bin" | awk '{print $1}')"
if [ "$CHUNKED_RC" -eq 0 ] && [ "$CHUNKED_ACTUAL" = "$CHUNKED_EXPECT" ]; then
  check "the chunked response was reassembled correctly" 0 "sha256=$CHUNKED_ACTUAL"
else
  check "the chunked response was reassembled correctly" 1 \
    "rc=$CHUNKED_RC expected=$CHUNKED_EXPECT actual=$CHUNKED_ACTUAL"
fi

echo
echo "--- leg 6: upload, $UPLOAD_BYTES bytes ---"
# Random rather than patterned: a body the server could have predicted would not
# prove the bytes travelled.
head -c "$UPLOAD_BYTES" /dev/urandom >"$WORK_DIR/upload.bin"
UPLOAD_EXPECT="$(sha256sum "$WORK_DIR/upload.bin" | awk '{print $1}')"
ECHO_OUT=""
ECHO_RC=0
ECHO_OUT="$("${CURL[@]}" -X POST --data-binary "@$WORK_DIR/upload.bin" \
  -o "$WORK_DIR/echo.txt" "https://$DOMAIN/echo" 2>&1)" || ECHO_RC=$?
echo "$ECHO_OUT"
ECHO_BODY="$(cat "$WORK_DIR/echo.txt" 2>/dev/null || true)"
if [ "$ECHO_RC" -eq 0 ] && [ "$ECHO_BODY" = "len=$UPLOAD_BYTES sha256=$UPLOAD_EXPECT" ]; then
  check "the uploaded body arrived intact" 0 "$ECHO_BODY"
else
  check "the uploaded body arrived intact" 1 \
    "rc=$ECHO_RC expected=len=$UPLOAD_BYTES sha256=$UPLOAD_EXPECT actual=$ECHO_BODY"
fi

echo
echo "--- leg 7: $PARALLEL simultaneous clients ---"
PARALLEL_PIDS=()
for index in $(seq 1 "$PARALLEL"); do
  (
    "${CURL[@]}" -o "$WORK_DIR/parallel-$index.body" \
      "https://$DOMAIN/big?n=65536" >"$WORK_DIR/parallel-$index.out" 2>&1
    echo $? >"$WORK_DIR/parallel-$index.rc"
  ) &
  PARALLEL_PIDS+=( $! )
done
for pid in "${PARALLEL_PIDS[@]}"; do
  wait "$pid" 2>/dev/null || true
done

PARALLEL_OK=0
PARALLEL_BAD=""
for index in $(seq 1 "$PARALLEL"); do
  rc="$(cat "$WORK_DIR/parallel-$index.rc" 2>/dev/null || echo 1)"
  size="$(stat -c %s "$WORK_DIR/parallel-$index.body" 2>/dev/null || echo 0)"
  if [ "$rc" -eq 0 ] && [ "$size" -eq 65536 ]; then
    PARALLEL_OK=$(( PARALLEL_OK + 1 ))
  else
    PARALLEL_BAD="$PARALLEL_BAD #$index(rc=$rc size=$size)"
  fi
done
echo "simultaneous clients that succeeded: $PARALLEL_OK/$PARALLEL$PARALLEL_BAD"
if [ "$PARALLEL_OK" -eq "$PARALLEL" ]; then
  check "every simultaneous client succeeded" 0 "succeeded=$PARALLEL_OK/$PARALLEL"
else
  check "every simultaneous client succeeded" 1 "succeeded=$PARALLEL_OK/$PARALLEL$PARALLEL_BAD"
fi

# --- 5. Stop both processes and read the server's own counters. -------------
echo
echo "real-traffic: stopping the daemon"
kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""

echo "real-traffic: stopping the TLS server"
kill -TERM "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

echo
echo "===================== TLS server ====================="
cat "$SERVER_LOG"
echo "===================== kernel ========================="
grep -vE '\] (stats|override) ' "$DAEMON_LOG" || true
echo "===================== kernel counters ================"
grep -E '\] (stats|stop) ' "$DAEMON_LOG" || true
echo "======================================================"

# The server's counters, not the kernel's, decide these two. A kernel that dropped
# traffic could still report bytes in both directions; a server that counted the
# requests it answered could not have answered requests it never saw.
TLS_STATS="$(sed -n 's/^TLS_STATS //p' "$SERVER_LOG")"
echo "TLS_STATS $TLS_STATS"

SERVER_REQUESTS="$(sed -n 's/.*requests=\([0-9]*\).*/\1/p' <<<"$TLS_STATS")"
SERVER_CONNECTIONS="$(sed -n 's/.*connections=\([0-9]*\).*/\1/p' <<<"$TLS_STATS")"
SERVER_MAX_KEEPALIVE="$(sed -n 's/.*max_requests_on_one_connection=\([0-9]*\).*/\1/p' <<<"$TLS_STATS")"
SERVER_HANDSHAKE_FAILURES="$(sed -n 's/.*handshakes_failed=\([0-9]*\).*/\1/p' <<<"$TLS_STATS")"
SERVER_ERRORS="$(sed -n 's/.*status=\([^ ]*\).*/\1/p' <<<"$TLS_STATS")"

# Every leg issues at least one request: 1 hello, N keep-alive, 1 big, 1 chunked,
# 1 upload, and PARALLEL more.
EXPECTED_REQUESTS=$(( 1 + KEEPALIVE_REQUESTS + 1 + 1 + 1 + PARALLEL ))

if [ -n "$SERVER_REQUESTS" ] && [ "$SERVER_REQUESTS" -ge "$EXPECTED_REQUESTS" ]; then
  check "the server saw every request" 0 \
    "requests=$SERVER_REQUESTS (>= $EXPECTED_REQUESTS expected)"
else
  check "the server saw every request" 1 \
    "requests=${SERVER_REQUESTS:-?} (>= $EXPECTED_REQUESTS expected)"
fi

if [ "${SERVER_HANDSHAKE_FAILURES:-1}" -eq 0 ]; then
  check "every TLS handshake completed" 0 "handshakes_failed=$SERVER_HANDSHAKE_FAILURES"
else
  check "every TLS handshake completed" 1 "handshakes_failed=$SERVER_HANDSHAKE_FAILURES"
fi

if [ "${SERVER_MAX_KEEPALIVE:-0}" -ge "$KEEPALIVE_REQUESTS" ]; then
  check "the server saw $KEEPALIVE_REQUESTS requests on a single connection" 0 \
    "max_requests_on_one_connection=$SERVER_MAX_KEEPALIVE"
else
  check "the server saw $KEEPALIVE_REQUESTS requests on a single connection" 1 \
    "max_requests_on_one_connection=${SERVER_MAX_KEEPALIVE:-?} (>= $KEEPALIVE_REQUESTS expected)"
fi

if [ "$SERVER_ERRORS" = "200:$SERVER_REQUESTS" ]; then
  check "no response was an error" 0 "status=$SERVER_ERRORS"
else
  check "no response was an error" 1 "status=$SERVER_ERRORS (connections=$SERVER_CONNECTIONS)"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "RESULT PASS checks=$PASSED"
  echo "real-traffic: PASS"
  exit 0
fi

echo "RESULT FAIL failures=$FAILURES passed=$PASSED"
echo "real-traffic: FAIL" >&2
exit 1
