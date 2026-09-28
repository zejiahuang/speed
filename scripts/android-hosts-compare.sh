#!/usr/bin/env bash
#
# Sample a hosts file and, for each domain, compare direct access with the
# kernel's CONNECT proxy running on a device.
#
# The point is to find domains that are unreachable from this machine but
# reachable once the kernel steers them to the address the hosts file supplies —
# which is the only claim worth making about a rule source. A domain that works
# both ways proves nothing, and a domain that fails both ways is the network's
# answer, not the kernel's.
#
#   scripts/android-hosts-compare.sh <hosts-file> <domains-file>
#
# Environment:
#   ADB        adb executable
#   DEVICE     adb device selector
#   PORT       local port forwarded to the device proxy   (default: 18080)
#   TIMEOUT    direct request timeout, seconds            (default: 8)

set -uo pipefail

ADB="${ADB:-/c/Users/Administrator/Desktop/platform-tools/adb.exe}"
DEVICE="${DEVICE:--s 127.0.0.1:5555}"
PORT="${PORT:-18080}"
TIMEOUT="${TIMEOUT:-8}"

HOSTS_FILE="$1"
DOMAINS_FILE="$2"
OUT="${3:-tmp/android-hosts-compare.tsv}"

"$ADB" $DEVICE forward --remove "tcp:$PORT" >/dev/null 2>&1 || true
"$ADB" $DEVICE forward "tcp:$PORT" "tcp:$PORT" >/dev/null

"$ADB" $DEVICE shell "killall watt-daemon 2>/dev/null || true"
"$ADB" $DEVICE push "$HOSTS_FILE" /data/local/tmp/watt/hosts-all.txt >/dev/null 2>&1
"$ADB" $DEVICE shell \
  "cd /data/local/tmp/watt; ./watt-daemon --proxy-listen 0.0.0.0:$PORT --hosts-file ./hosts-all.txt --stats-interval 3600 > /data/local/tmp/watt/compare.log 2>&1 &"
sleep 3
"$ADB" $DEVICE shell "cat /data/local/tmp/watt/compare.log" | tr -d '\r'

printf 'domain\tdirect\tdirect_exit\tproxy\tproxy_ssl\tproxy_exit\tverdict\n' >"$OUT"
printf '\n%-42s %-22s %s\n' DOMAIN DIRECT PROXY
while read -r domain; do
  domain="${domain%$'\r'}"
  [ -z "$domain" ] && continue
  case "$domain" in \#*) continue ;; esac

  direct="$(curl --silent --output /dev/null --noproxy "" --max-time "$TIMEOUT" \
    -w '%{http_code} %{exitcode}' "https://$domain/" 2>/dev/null)"
  proxied="$(curl --silent --output /dev/null --noproxy "" --max-time "$(( TIMEOUT + 12 ))" \
    -x "127.0.0.1:$PORT" -w '%{http_code} %{ssl_verify_result} %{exitcode}' \
    "https://$domain/" 2>/dev/null)"

  dcode="${direct%% *}"; dexit="${direct##* }"
  pcode="$(cut -d' ' -f1 <<<"$proxied")"
  pssl="$(cut -d' ' -f2 <<<"$proxied")"
  pexit="$(cut -d' ' -f3 <<<"$proxied")"

  verdict="BOTH_FAILED"
  [ "$dcode" != "000" ] && [ "$pcode" != "000" ] && verdict="BOTH_OK"
  [ "$dcode" != "000" ] && [ "$pcode" = "000" ] && verdict="PROXY_FAILED"
  [ "$dcode" = "000" ] && [ "$pcode" != "000" ] && verdict="RESCUED"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$domain" "$dcode" "$dexit" "$pcode" "$pssl" "$pexit" "$verdict" >>"$OUT"
  printf '%-42s %-22s %s %s\n' "$domain" "$direct" "$proxied" \
    "$([ "$verdict" = RESCUED ] && echo '  <-- rescued')"
done <"$DOMAINS_FILE"

"$ADB" $DEVICE shell "killall watt-daemon 2>/dev/null || true"
echo
echo "android-hosts-compare: wrote $OUT"
awk -F '\t' 'NR>1 {c[$7]++} END {for (k in c) printf "  %-14s %d\n", k, c[k]}' "$OUT"
