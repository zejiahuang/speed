#!/usr/bin/env bash
#
# Does the kernel relay its own upstream traffic back into itself?
#
# Every other test in this harness points a rule's address at loopback with a
# static override, which is what makes the relay reachable without the internet.
# That shortcut hides a question worth asking on its own: what happens when a
# relayed destination is *not* overridden, and the host's route table sends it
# back into the tunnel?
#
# The kernel's upstream socket is routed by the same table as everything else. So
# a connection to a relayed address arrives back at the kernel as a new client,
# which relays it again, which arrives again. On Android `VpnService.protect`
# exists precisely to prevent this, and the engine refuses to connect a socket
# that was never offered to a protector. A bare Linux host has no protector, so
# this script finds out what actually happens.
#
# It is a diagnostic, not a test: it reports what it observed rather than
# asserting an expectation, because the honest answer was not known when it was
# written. The watchdog is not optional — if the loop is real it allocates
# descriptors as fast as loopback allows.
#
#   scripts/probe-selfloop.sh
#
# Environment:
#   WATCH_SECONDS   how long to watch before concluding   (default: 6)
#   FD_ABORT        abort early above this descriptor count (default: 300)
#   PROTECT_MARK    socket mark to exempt the kernel's own upstream sockets, in
#                   decimal or 0x form. Empty means no protection, which is the
#                   case this script exists to demonstrate.

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/selfloop"

TUN_NAME="${TUN_NAME:-watt2}"
TUN_ADDRESS="198.20.0.1"
TUN_PREFIX="15"
TEST_NET="203.0.113.0/24"
# A rule address with no override. Nothing is listening on it, and nothing needs
# to be: the point is what the kernel does with the connection attempt.
RELAYED_ADDRESS="203.0.113.90"
RELAYED_PORT=443

WATCH_SECONDS="${WATCH_SECONDS:-6}"
FD_ABORT="${FD_ABORT:-300}"
PROTECT_MARK="${PROTECT_MARK:-}"

# A private routing table holding only the real default route. Marked packets are
# sent here, which is what keeps the kernel's upstream traffic out of the tunnel.
# The same arrangement `wg-quick` uses for WireGuard's own packets.
MARK_TABLE="${MARK_TABLE:-5754}"

DAEMON_LOG="$TMP_DIR/selfloop-daemon.log"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
DAEMON_BIN=""

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --daemon-bin) DAEMON_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "probe-selfloop: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Resolve the binary before elevating and carry it across, because sudo does not
# preserve HOME: a path built from `$HOME` in the parent becomes `/root/...` in the
# child, and the script then reports a missing binary it built itself.
if [ -z "$DAEMON_BIN" ]; then
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
  if [ ! -x "$DAEMON_BIN" ]; then
    echo "probe-selfloop: building the daemon"
    bash "$SCRIPTS_DIR/cargo.sh" build --quiet -p watt-daemon
  fi
fi

if [ ! -x "$DAEMON_BIN" ]; then
  echo "probe-selfloop: daemon binary not found: $DAEMON_BIN" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "probe-selfloop: needs root, re-executing under sudo"
  exec sudo env "PATH=$PATH" "TUN_NAME=$TUN_NAME" \
    "WATCH_SECONDS=$WATCH_SECONDS" "FD_ABORT=$FD_ABORT" \
    "PROTECT_MARK=$PROTECT_MARK" "MARK_TABLE=$MARK_TABLE" \
    bash "$0" --daemon-bin "$DAEMON_BIN"
fi

mkdir -p "$TMP_DIR" "$WORK_DIR"
rm -f "$DAEMON_LOG"

DAEMON_PID=""
CLIENT_PID=""

cleanup() {
  set +e
  [ -n "$CLIENT_PID" ] && kill -KILL "$CLIENT_PID" 2>/dev/null
  [ -n "$DAEMON_PID" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  ip route del "$TEST_NET" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  if [ -n "$PROTECT_MARK" ]; then
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

# A rule the kernel will steer, and no override to point it anywhere else.
cat >"$WORK_DIR/rules.json" <<JSON
{
  "meta": { "version": "selfloop", "update_time": "2026-01-01T00:00:00Z" },
  "groups": [
    {
      "group": "SelfLoop",
      "entries": [
        {
          "id": "selfloop",
          "name": "SelfLoop",
          "nameZh": "自环探测",
          "domains": ["loop.test"],
          "ips": ["$RELAYED_ADDRESS"],
          "port": "$RELAYED_PORT",
          "isPlaceholder": false,
          "ipCountry": "ZZ",
          "ipCountryName": "Nowhere"
        }
      ]
    }
  ]
}
JSON

echo "probe-selfloop: interface $TUN_NAME, relayed address $RELAYED_ADDRESS:$RELAYED_PORT, no override"

DAEMON_ARGS=(
  --tun "$TUN_NAME"
  --address "$TUN_ADDRESS/$TUN_PREFIX"
  --rules-file "$WORK_DIR/rules.json"
  --stats-interval 2
  --tick 3600
)

if [ -n "$PROTECT_MARK" ]; then
  # The mark only means something if the host sends marked traffic somewhere the
  # tunnel's routes are not. Without this rule the daemon would be tagging packets
  # that still come straight back.
  echo "probe-selfloop: exempting mark $PROTECT_MARK via table $MARK_TABLE"
  ip route flush table "$MARK_TABLE" 2>/dev/null || true
  ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null || true

  # Whatever the host already uses to reach the internet, copied into a table of
  # our own. Both forms appear in practice: WSL has a gateway, some setups do not.
  DEFAULT_ROUTE="$(ip route show default | head -1)"
  if [ -z "$DEFAULT_ROUTE" ]; then
    echo "probe-selfloop: no default route to copy; the mark cannot help" >&2
    exit 1
  fi
  GATEWAY="$(sed -n 's/.*via \([0-9.]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
  DEVICE="$(sed -n 's/.*dev \([^ ]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
  if [ -n "$GATEWAY" ]; then
    ip route add default via "$GATEWAY" dev "$DEVICE" table "$MARK_TABLE"
  else
    ip route add default dev "$DEVICE" table "$MARK_TABLE"
  fi
  ip rule add fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" pref 100
  echo "probe-selfloop: table $MARK_TABLE: $DEFAULT_ROUTE"
  ip rule show | head -5

  DAEMON_ARGS+=( --protect-mark "$PROTECT_MARK" )
else
  echo "probe-selfloop: no protect mark, so the kernel's own sockets are unprotected"
fi

"$DAEMON_BIN" "${DAEMON_ARGS[@]}" >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

if ! wait_for_line "$DAEMON_LOG" '\] tun ' 20; then
  echo "probe-selfloop: the daemon did not start" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi

ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$TEST_NET" dev "$TUN_NAME"

BASELINE_FDS="$(ls /proc/"$DAEMON_PID"/fd 2>/dev/null | wc -l)"
echo "probe-selfloop: baseline descriptors = $BASELINE_FDS"

# One connection attempt, in the background, with its own timeout so it cannot
# outlive the watchdog. The timeout is an argument rather than a shell variable
# because the heredoc is quoted, which is what keeps the rest of the program from
# being mangled by the shell.
python3 - "$RELAYED_ADDRESS" "$RELAYED_PORT" "$WATCH_SECONDS" <<'PY' >"$WORK_DIR/client.log" 2>&1 &
import socket, sys, time
addr, port, budget = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
started = time.monotonic()
try:
    with socket.create_connection((addr, port), timeout=budget) as sock:
        print(f"connected in {time.monotonic() - started:.2f}s", flush=True)
        sock.settimeout(2.0)
        try:
            data = sock.recv(64)
            print(f"received {len(data)} bytes", flush=True)
        except OSError as err:
            print(f"no reply: {err}", flush=True)
except OSError as err:
    print(f"failed after {time.monotonic() - started:.2f}s: {err}", flush=True)
PY
CLIENT_PID=$!

echo "probe-selfloop: watching for ${WATCH_SECONDS}s (abort above $FD_ABORT descriptors)"
echo
printf '%6s %8s %10s %10s\n' "t" "fds" "tcp_open" "tcp_failed"

STARTED="$(date +%s)"
PEAK_FDS="$BASELINE_FDS"
ABORTED=0

while :; do
  NOW="$(date +%s)"
  ELAPSED=$(( NOW - STARTED ))
  [ "$ELAPSED" -ge "$WATCH_SECONDS" ] && break

  FDS="$(ls /proc/"$DAEMON_PID"/fd 2>/dev/null | wc -l)"
  LINE="$(grep -E '\] stats ' "$DAEMON_LOG" | tail -1)"
  TCP_OPEN="$(sed -n 's/.*tcp_open=\([0-9]*\).*/\1/p' <<<"$LINE")"
  TCP_FAILED="$(sed -n 's/.*tcp_failed=\([0-9]*\).*/\1/p' <<<"$LINE")"

  [ "$FDS" -gt "$PEAK_FDS" ] && PEAK_FDS="$FDS"
  printf '%6s %8s %10s %10s\n' "$ELAPSED" "$FDS" "${TCP_OPEN:--}" "${TCP_FAILED:--}"

  if [ "$FDS" -gt "$FD_ABORT" ]; then
    ABORTED=1
    echo
    echo "probe-selfloop: aborted at $FDS descriptors, above the $FD_ABORT limit"
    break
  fi

  sleep 0.5
done

kill -KILL "$CLIENT_PID" 2>/dev/null || true
CLIENT_PID=""

echo
echo "probe-selfloop: stopping the daemon"
kill -TERM "$DAEMON_PID" 2>/dev/null || true
sleep 1
kill -KILL "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true

echo
echo "===================== daemon log ====================="
cat "$DAEMON_LOG"
echo "======================================================"

FINAL_FDS="$(ls /proc/"$DAEMON_PID"/fd 2>/dev/null | wc -l)"
echo "client: $(cat "$WORK_DIR/client.log" 2>/dev/null | tail -1)"
echo
echo "baseline descriptors : $BASELINE_FDS"
echo "peak descriptors     : $PEAK_FDS"
echo "watchdog             : $([ "$ABORTED" -eq 1 ] && echo 'tripped' || echo 'not tripped')"

if [ "$ABORTED" -eq 1 ]; then
  echo
  echo "VERDICT: the kernel relays its own upstream traffic back into itself."
  echo "         Descriptors grew from $BASELINE_FDS to $PEAK_FDS in ${WATCH_SECONDS}s"
  echo "         without a client asking for more than one connection."
  exit 1
fi

echo
echo "VERDICT: descriptors stayed bounded (peak $PEAK_FDS, baseline $BASELINE_FDS)."
exit 0
