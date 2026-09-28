#!/usr/bin/env bash
#
# Compare direct access against the kernel's CONNECT proxy, using a hosts file
# as the rule source.
#
# The hosts file upstream publishes is the larger address source: it carries
# concrete addresses where the JSON document has `{Cloudflare}` placeholders, and
# it covers the groups the JSON filters out of the mobile profile. This script
# asks whether that actually changes reachability — for each domain it fetches
# directly and then through the proxy, and prints both.
#
#   scripts/compare-hosts.sh [domains-file]
#
# Environment:
#   HOSTS_FILE   hosts file to load        (default: tmp/hosts-all.txt)
#   PROXY_ADDR   listener                  (default: 127.0.0.1:18081)
#   TIMEOUT      per request, seconds      (default: 8)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$REPO_DIR/tmp"
BIN="${BIN:-$HOME/.cache/watt-target/debug/watt-daemon}"
HOSTS_FILE="${HOSTS_FILE:-$TMP_DIR/hosts-all.txt}"
PROXY_ADDR="${PROXY_ADDR:-127.0.0.1:18081}"
TIMEOUT="${TIMEOUT:-8}"
DOMAINS_FILE="${1:-}"

[ -x "$BIN" ] || { echo "compare-hosts: daemon not built: $BIN" >&2; exit 1; }
[ -f "$HOSTS_FILE" ] || { echo "compare-hosts: no hosts file: $HOSTS_FILE" >&2; exit 1; }

LOG="$TMP_DIR/compare-hosts-daemon.log"
"$BIN" --proxy-listen "$PROXY_ADDR" --hosts-file "$HOSTS_FILE" \
  --stats-interval 3600 >"$LOG" 2>&1 &
DAEMON_PID=$!
cleanup() {
  set +e
  kill -TERM "$DAEMON_PID" 2>/dev/null
  wait "$DAEMON_PID" 2>/dev/null
}
trap cleanup EXIT

for _ in $(seq 1 60); do
  grep -q "proxy listening" "$LOG" 2>/dev/null && break
  sleep 0.1
done
grep -E '\] (hosts|rules|proxy) ' "$LOG" || true

if [ -z "$DOMAINS_FILE" ]; then
  DOMAINS_FILE="$(mktemp)"
  trap 'rm -f "$DOMAINS_FILE"; cleanup' EXIT
  cat >"$DOMAINS_FILE" <<'LIST'
discordapp.com
gateway.discord.gg
store.steampowered.com
steamcommunity.com
api.steampowered.com
epicgames.com
store.epicgames.com
ubisoft.com
github.com
code.jquery.com
apache.org
www.indiegala.com
addons.mozilla.org
login.live.com
nikke-en.com
LIST
fi

probe() { # <domain> <extra curl args...>
  local domain="$1"
  shift
  curl --silent --output /dev/null --noproxy "" --max-time "$TIMEOUT" \
    -w '%{http_code} %{ssl_verify_result} %{exitcode}' "$@" "https://$domain/" 2>/dev/null
}

printf '\n%-40s %-20s %s\n' DOMAIN DIRECT PROXY
while read -r domain; do
  domain="${domain%$'\r'}"
  [ -z "$domain" ] && continue
  case "$domain" in \#*) continue ;; esac
  direct="$(probe "$domain")"
  proxied="$(probe "$domain" -x "$PROXY_ADDR" --max-time "$(( TIMEOUT + 6 ))")"
  marker=""
  if [ "${direct%% *}" = "000" ] && [ "${proxied%% *}" != "000" ]; then
    marker="  <-- rescued"
  fi
  printf '%-40s %-20s %s%s\n' "$domain" "$direct" "$proxied" "$marker"
done <"$DOMAINS_FILE"
