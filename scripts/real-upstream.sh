#!/usr/bin/env bash
#
# Real internet traffic through the tunnel, with real certificate verification.
#
# Every other end-to-end test here rewrites its destination to a local server,
# which is what makes them hermetic. That shortcut leaves one thing unproven: that
# the kernel can carry traffic to somewhere it has to reach for real, over the
# host's actual network, through its own userspace TCP stack.
#
# This script removes the shortcut. The destinations are real hosts on the
# internet, reached by their real addresses, and curl verifies their real
# certificates — there is no `-k` here, so a single corrupted byte anywhere in a
# handshake fails the run. Each response body is hashed and compared against the
# same URL fetched directly, which makes this a differential test rather than a
# set of hardcoded expectations.
#
# It needs the socket-mark exemption, because without it the kernel's upstream
# traffic is routed back into the tunnel and the two relay each other forever.
# `scripts/probe-selfloop.sh` demonstrates exactly that, in about a second.
#
#   scripts/real-upstream.sh
#
# Environment:
#   HOSTS         hosts to fetch, space separated    (default: three unrelated ones)
#   TUN_NAME      interface to create                (default: watt3)
#   PROTECT_MARK  socket mark to exempt the kernel   (default: 0x5754)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/upstream"

# Three unrelated operators on purpose. One host proves the path works; several
# prove it keeps working across different destinations, which exercises the
# per-destination listener pool and the router's multi-entry index rather than a
# single warm entry.
HOSTS="${HOSTS:-example.com www.iana.org www.cloudflare.com}"
TUN_NAME="${TUN_NAME:-watt3}"
TUN_ADDRESS="198.21.0.1"
TUN_PREFIX="15"
PROTECT_MARK="${PROTECT_MARK:-0x5754}"
MARK_TABLE="${MARK_TABLE:-5754}"
DNS_ADDRESS="203.0.113.53"

DAEMON_LOG="$TMP_DIR/upstream-daemon.log"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
DAEMON_BIN=""

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --daemon-bin) DAEMON_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "real-upstream: unknown argument: $1" >&2; exit 2 ;;
  esac
done

for tool in ip curl python3 sha256sum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "real-upstream: $tool is required" >&2
    exit 1
  fi
done

# Resolve the binary before elevating and carry it across, because sudo does not
# preserve HOME: a path built from `$HOME` in the parent becomes `/root/...` in
# the child, and the script then reports a missing binary it built itself.
if [ -z "$DAEMON_BIN" ]; then
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
  if [ ! -x "$DAEMON_BIN" ]; then
    echo "real-upstream: building the daemon"
    bash "$SCRIPTS_DIR/cargo.sh" build --quiet -p watt-daemon
  fi
fi
if [ ! -x "$DAEMON_BIN" ]; then
  echo "real-upstream: daemon binary not found: $DAEMON_BIN" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "real-upstream: needs root, re-executing under sudo"
  exec sudo env "PATH=$PATH" "HOSTS=$HOSTS" "TUN_NAME=$TUN_NAME" \
    "PROTECT_MARK=$PROTECT_MARK" "MARK_TABLE=$MARK_TABLE" \
    bash "$0" --daemon-bin "$DAEMON_BIN"
fi

mkdir -p "$TMP_DIR" "$WORK_DIR"
rm -f "$DAEMON_LOG" "$WORK_DIR"/baseline-*.bin "$WORK_DIR"/baseline-*.err
rm -f "$WORK_DIR"/tunnel-*.bin "$WORK_DIR"/tunnel-*.err

DAEMON_PID=""
MARK_RULE=0

cleanup() {
  set +e
  [ -n "$DAEMON_PID" ] && kill -TERM "$DAEMON_PID" 2>/dev/null
  sleep 0.3
  [ -n "$DAEMON_PID" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  for address in "${RESOLVED_ADDRESSES[@]:-}"; do
    [ -n "$address" ] && ip route del "$address/32" dev "$TUN_NAME" 2>/dev/null
  done
  ip route del "$DNS_ADDRESS/32" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  if [ "$MARK_RULE" -eq 1 ]; then
    ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null
    ip route flush table "$MARK_TABLE" 2>/dev/null
  fi
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

# --- 1. Resolve, and fetch every host directly for the baseline. ------------
RESOLVED_HOSTS=()
RESOLVED_ADDRESSES=()
BASELINE_HASHES=()
BASELINE_BYTES=()

for host in $HOSTS; do
  address="$(getent ahostsv4 "$host" 2>/dev/null | awk 'NR == 1 {print $1}')"
  if [ -z "$address" ]; then
    echo "real-upstream: could not resolve $host; this test needs working DNS" >&2
    exit 1
  fi

  index="${#RESOLVED_HOSTS[@]}"
  body="$WORK_DIR/baseline-$index.bin"
  echo "real-upstream: $host -> $address, fetching directly for the baseline"
  if ! curl --silent --show-error --max-time 30 --output "$body" \
    "https://$host/" 2>"$WORK_DIR/baseline-$index.err"; then
    echo "real-upstream: the direct fetch of $host failed, so there is nothing to compare against" >&2
    cat "$WORK_DIR/baseline-$index.err" >&2
    exit 1
  fi

  RESOLVED_HOSTS+=( "$host" )
  RESOLVED_ADDRESSES+=( "$address" )
  BASELINE_HASHES+=( "$(sha256sum "$body" | awk '{print $1}')" )
  BASELINE_BYTES+=( "$(stat -c %s "$body")" )
  echo "real-upstream:   ${BASELINE_BYTES[$index]} bytes, sha256=${BASELINE_HASHES[$index]}"
done

# --- 2. Keep the kernel's own traffic out of the tunnel. --------------------
#
# A private table holding only the real default route, and a rule sending marked
# packets to it. Without both halves the kernel's upstream connections come
# straight back, which `probe-selfloop.sh` demonstrates in about a second.
echo
echo "real-upstream: exempting mark $PROTECT_MARK via table $MARK_TABLE"
ip route flush table "$MARK_TABLE" 2>/dev/null || true
ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null || true

DEFAULT_ROUTE="$(ip route show default | head -1)"
GATEWAY="$(sed -n 's/.*via \([0-9.]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
DEVICE="$(sed -n 's/.*dev \([^ ]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
if [ -z "$DEVICE" ]; then
  echo "real-upstream: no default route to copy; cannot exempt the kernel" >&2
  exit 1
fi
if [ -n "$GATEWAY" ]; then
  ip route add default via "$GATEWAY" dev "$DEVICE" table "$MARK_TABLE"
else
  ip route add default dev "$DEVICE" table "$MARK_TABLE"
fi
ip rule add fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" pref 100
MARK_RULE=1
echo "real-upstream: table $MARK_TABLE mirrors '$DEFAULT_ROUTE'"

# --- 3. A rule document that owns every host. -------------------------------
RULES="$WORK_DIR/rules.json"
{
  printf '{\n'
  printf '  "meta": { "version": "real-upstream", "update_time": "2026-01-01T00:00:00Z" },\n'
  printf '  "groups": [\n    {\n      "group": "Upstream",\n      "entries": [\n'
  for index in "${!RESOLVED_HOSTS[@]}"; do
    printf '        {\n'
    printf '          "id": "upstream-%s",\n' "$index"
    printf '          "name": "%s",\n' "${RESOLVED_HOSTS[$index]}"
    printf '          "nameZh": "真实上游",\n'
    printf '          "domains": ["%s"],\n' "${RESOLVED_HOSTS[$index]}"
    printf '          "ips": ["%s"],\n' "${RESOLVED_ADDRESSES[$index]}"
    printf '          "port": "443",\n'
    printf '          "isPlaceholder": false,\n'
    printf '          "ipCountry": "ZZ",\n'
    printf '          "ipCountryName": "Nowhere"\n'
    printf '        }%s\n' "$([ "$index" -lt $(( ${#RESOLVED_HOSTS[@]} - 1 )) ] && echo ,)"
  done
  printf '      ]\n    }\n  ]\n}\n'
} >"$RULES"

# --- 4. The kernel, with its own traffic exempted. --------------------------
echo "real-upstream: starting the daemon on $TUN_NAME"
"$DAEMON_BIN" \
  --tun "$TUN_NAME" \
  --address "$TUN_ADDRESS/$TUN_PREFIX" \
  --rules-file "$RULES" \
  --protect-mark "$PROTECT_MARK" \
  --stats-interval 10 \
  --tick 3600 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

if ! wait_for_line "$DAEMON_LOG" '\] tun ' 20; then
  echo "real-upstream: the daemon did not start" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi
grep -E '\] (rules|protect|tun) ' "$DAEMON_LOG" || true

echo "real-upstream: addressing $TUN_NAME and routing each host into it"
ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
# One host route per destination rather than a block: the tunnel takes exactly
# those addresses out of the real internet, so nothing else on the host changes.
for address in "${RESOLVED_ADDRESSES[@]}"; do
  ip route add "$address/32" dev "$TUN_NAME"
done
# The DNS leg needs its own route. Without it the query leaves by the front door
# and is answered by the host's resolver, which would prove nothing about the
# kernel while still looking like a successful lookup.
ip route add "$DNS_ADDRESS/32" dev "$TUN_NAME"

# --- 5. DNS through the tunnel, then the real fetches. ---------------------
echo
echo "--- leg 1: DNS through the tunnel ---"
for index in "${!RESOLVED_HOSTS[@]}"; do
  host="${RESOLVED_HOSTS[$index]}"
  DNS_OUT=""
  DNS_RC=0
  DNS_OUT="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" --domain "$host" 2>&1)" || DNS_RC=$?
  echo "$DNS_OUT"
  if [ "$DNS_RC" -eq 0 ] && [ "${DNS_OUT#DNS ANSWERS $host }" = "${RESOLVED_ADDRESSES[$index]}" ]; then
    check "the rule set answered DNS for $host" 0 "${RESOLVED_ADDRESSES[$index]}"
  else
    check "the rule set answered DNS for $host" 1 "rc=$DNS_RC out=$DNS_OUT"
  fi
done

echo
echo "--- leg 2: HTTPS with real certificate verification ---"
for index in "${!RESOLVED_HOSTS[@]}"; do
  host="${RESOLVED_HOSTS[$index]}"
  body="$WORK_DIR/tunnel-$index.bin"

  TUNNEL_OUT=""
  TUNNEL_RC=0
  # No `--insecure`: the certificate is the real one and curl checks it against
  # the system trust store. `--resolve` supplies the address so the connection
  # goes to the address the tunnel is steering, without depending on the host's
  # resolver — the DNS leg above is what tests that path.
  TUNNEL_OUT="$(curl --silent --show-error --max-time 45 \
    --resolve "$host:443:${RESOLVED_ADDRESSES[$index]}" \
    --write-out 'CURL %{http_code} %{size_download} %{time_total} %{ssl_verify_result}\n' \
    --output "$body" "https://$host/" 2>"$WORK_DIR/tunnel-$index.err")" || TUNNEL_RC=$?

  TUNNEL_HASH="$(sha256sum "$body" 2>/dev/null | awk '{print $1}')"
  TUNNEL_BYTES="$(stat -c %s "$body" 2>/dev/null || echo 0)"

  printf '%-20s %s\n' "$host" "${TUNNEL_OUT:-curl exit $TUNNEL_RC: $(cat "$WORK_DIR/tunnel-$index.err")}"

  if [ "$TUNNEL_RC" -eq 0 ] && [ "$TUNNEL_HASH" = "${BASELINE_HASHES[$index]}" ]; then
    check "$host returned the same bytes as a direct fetch" 0 \
      "bytes=$TUNNEL_BYTES sha256=$TUNNEL_HASH"
  else
    check "$host returned the same bytes as a direct fetch" 1 \
      "rc=$TUNNEL_RC bytes=$TUNNEL_BYTES expected=${BASELINE_HASHES[$index]} actual=$TUNNEL_HASH"
  fi

  # ssl_verify_result is 0 when the chain validated. Read from the write-out
  # rather than inferred from the exit status, because the two can disagree.
  VERIFY_RESULT="$(sed -n 's/.*CURL [0-9]* [0-9]* [0-9.]* \([0-9]*\)$/\1/p' <<<"$TUNNEL_OUT")"
  if [ "${VERIFY_RESULT:-1}" -eq 0 ]; then
    check "$host had its certificate chain validated" 0 "ssl_verify_result=$VERIFY_RESULT"
  else
    check "$host had its certificate chain validated" 1 "ssl_verify_result=${VERIFY_RESULT:-?}"
  fi
done

# --- 6. Stop and read what the kernel did with it. --------------------------
echo
echo "real-upstream: stopping the daemon"
kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""

echo
echo "===================== kernel ========================="
cat "$DAEMON_LOG"
echo "======================================================"

COUNTERS="$(grep -E '\] stop ' "$DAEMON_LOG" | tail -1)"
MATCHED="$(sed -n 's/.*matched=\([0-9]*\).*/\1/p' <<<"$COUNTERS")"
DIRECT="$(sed -n 's/.*direct=\([0-9]*\).*/\1/p' <<<"$COUNTERS")"
TCP_OPEN="$(sed -n 's/.*tcp_open=\([0-9]*\).*/\1/p' <<<"$COUNTERS")"
UP_BYTES="$(sed -n 's/.*u2c=\([0-9]*\).*/\1/p' <<<"$COUNTERS")"
LOCAL_DNS="$(sed -n 's/.*dns_local=\([0-9]*\).*/\1/p' <<<"$COUNTERS")"

# The number of hosts plus a little slack, not hundreds: the kernel's own sockets
# have to be leaving by the front door rather than coming back in through the
# tunnel. A cascade shows up here as a count in the hundreds.
HOST_COUNT="${#RESOLVED_HOSTS[@]}"
if [ "${TCP_OPEN:-0}" -ge "$HOST_COUNT" ] && [ "${TCP_OPEN:-0}" -le $(( HOST_COUNT + 5 )) ]; then
  check "the kernel opened one flow per host, not a cascade" 0 \
    "tcp_open=$TCP_OPEN (hosts=$HOST_COUNT)"
else
  check "the kernel opened one flow per host, not a cascade" 1 \
    "tcp_open=${TCP_OPEN:-?} (expected $HOST_COUNT..$(( HOST_COUNT + 5 )))"
fi

if [ "${MATCHED:-0}" -ge "$HOST_COUNT" ] && [ "${DIRECT:-1}" -eq 0 ]; then
  check "every connection was steered by the rules, none direct" 0 \
    "matched=$MATCHED direct=$DIRECT (hosts=$HOST_COUNT)"
else
  check "every connection was steered by the rules, none direct" 1 \
    "matched=${MATCHED:-?} direct=${DIRECT:-?}"
fi

BASELINE_TOTAL=0
for bytes in "${BASELINE_BYTES[@]}"; do
  BASELINE_TOTAL=$(( BASELINE_TOTAL + bytes ))
done
if [ "${UP_BYTES:-0}" -ge "$BASELINE_TOTAL" ]; then
  check "the bytes came back through the kernel" 0 "u2c=$UP_BYTES (>= $BASELINE_TOTAL)"
else
  check "the bytes came back through the kernel" 1 "u2c=${UP_BYTES:-?} (>= $BASELINE_TOTAL expected)"
fi

if [ "${LOCAL_DNS:-0}" -ge "$HOST_COUNT" ]; then
  check "every DNS answer came from the rule set" 0 "dns_local=$LOCAL_DNS (hosts=$HOST_COUNT)"
else
  check "every DNS answer came from the rule set" 1 \
    "dns_local=${LOCAL_DNS:-?} (expected >= $HOST_COUNT)"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "RESULT PASS checks=$PASSED"
  echo "real-upstream: PASS checks=$PASSED"
  exit 0
fi

echo "RESULT FAIL failures=$FAILURES passed=$PASSED"
echo "real-upstream: FAIL failures=$FAILURES" >&2
exit 1
