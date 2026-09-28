#!/usr/bin/env bash
#
# Ask the running kernel, domain by domain, what it answers for a DNS query,
# and print the raw result next to what `--probe` says the rule contains.
#
# Used to reconcile two things that disagree: the daemon's own `dns_local`
# counter (which counts answers it produced) and what a client actually
# receives. When they differ, the answer is being produced but not delivered.
#
#   scripts/debug-dns.sh <domains-file>
#
# Environment:
#   TUN_NAME      interface to create   (default: watt9)
#   PROTECT_MARK  socket mark           (default: 0x5754)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/debug-dns"

TUN_NAME="${TUN_NAME:-watt9}"
TUN_ADDRESS="198.21.0.1"
TUN_PREFIX="15"
PROTECT_MARK="${PROTECT_MARK:-0x5754}"
MARK_TABLE="${MARK_TABLE:-5754}"
DNS_ADDRESS="203.0.113.53"
RULES="$TMP_DIR/rules.json"
DOMAINS_FILE="${1:-$TMP_DIR/rule-sites.txt}"

DAEMON_BIN="${DAEMON_BIN:-}"

# Resolve the binary before elevating and carry it across: sudo does not
# preserve HOME, so a default path built from `$HOME` here becomes
# `/root/.cache/...` there and the script then reports a binary it built itself
# as missing.
if [ -z "$DAEMON_BIN" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
fi

[ -x "$DAEMON_BIN" ] || { echo "debug-dns: daemon not built: $DAEMON_BIN" >&2; exit 1; }
[ -f "$RULES" ] || { echo "debug-dns: no rule document: $RULES" >&2; exit 1; }

if [ "$(id -u)" -ne 0 ]; then
  exec sudo env "PATH=$PATH" "TUN_NAME=$TUN_NAME" "PROTECT_MARK=$PROTECT_MARK" \
    "MARK_TABLE=$MARK_TABLE" "DAEMON_BIN=$DAEMON_BIN" bash "$0" "$DOMAINS_FILE"
fi

mkdir -p "$WORK_DIR"
DAEMON_LOG="$WORK_DIR/daemon.log"
DAEMON_PID=""
MARK_RULE=0

cleanup() {
  set +e
  [ -n "$DAEMON_PID" ] && kill -TERM "$DAEMON_PID" 2>/dev/null
  sleep 0.3
  [ -n "$DAEMON_PID" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  ip route del "$DNS_ADDRESS/32" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  [ "$MARK_RULE" -eq 1 ] && ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null
  [ "$MARK_RULE" -eq 1 ] && ip route flush table "$MARK_TABLE" 2>/dev/null
  return 0
}
trap cleanup EXIT

ip link del "$TUN_NAME" 2>/dev/null || true

DEFAULT_ROUTE="$(ip route show default | head -1)"
GATEWAY="$(sed -n 's/.*via \([0-9.]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
DEVICE="$(sed -n 's/.*dev \([^ ]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
if [ -n "$GATEWAY" ]; then
  ip route add default via "$GATEWAY" dev "$DEVICE" table "$MARK_TABLE" 2>/dev/null
else
  ip route add default dev "$DEVICE" table "$MARK_TABLE" 2>/dev/null
fi
ip rule add fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" pref 100 2>/dev/null
MARK_RULE=1

"$DAEMON_BIN" \
  --tun "$TUN_NAME" \
  --address "$TUN_ADDRESS/$TUN_PREFIX" \
  --rules-file "$RULES" \
  --protect-mark "$PROTECT_MARK" \
  --stats-interval 3600 \
  --tick 3600 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 100); do
  grep -q '\] tun ' "$DAEMON_LOG" && break
  sleep 0.1
done
ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
ip route add "$DNS_ADDRESS/32" dev "$TUN_NAME"
echo "debug-dns: daemon up on $TUN_NAME"

echo
printf '%-30s %-10s %s\n' DOMAIN PROBE RESULT
while read -r domain _rest; do
  domain="${domain%$'\r'}"
  [ -z "${domain:-}" ] && continue
  case "$domain" in \#*) continue ;; esac

  # What the router says it would do, offline.
  planned="$("$DAEMON_BIN" --check --rules-file "$RULES" --probe "$domain" 2>&1 \
    | grep -m1 '\] probe .* v4 ' \
    | sed -n 's/.*strategy=\([^ ]*\).*addresses=\[\([^]]*\)\].*/\1 [\2]/p')"

  answered="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" \
    --domain "$domain" --timeout 6 2>&1)"
  # The flags separate "this name has no addresses" from "the answer did not
  # fit and you must retry over TCP", which look identical when only the
  # addresses are printed.
  flags="$(python3 "$SCRIPTS_DIR/dns_flags.py" "$DNS_ADDRESS" "$domain" 2>&1)"
  printf '%-30s %s\n' '' "flags: $flags"
  count="$(sed -n 's/^DNS ANSWERS [^ ]* //p' <<<"$answered" | wc -w)"
  if grep -q '^(none)$' <<<"$answered"; then count=0; fi
  printf '%-30s %-10s %s\n' "$domain" "answers=$count" "$answered"
done <"$DOMAINS_FILE"

echo
echo "--- daemon ---"
kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null
DAEMON_PID=""
cat "$DAEMON_LOG"
