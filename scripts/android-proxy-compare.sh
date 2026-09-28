#!/usr/bin/env bash
#
# Compare direct access with the CONNECT proxy for a list of domains, keeping
# the proxy's liveness under observation for the whole run.
#
# The reason this exists separately from android-hosts-compare.sh: a proxy that
# dies at sample 80 turns every remaining sample into `exit 7` (connection
# refused) while direct access keeps working, because direct access never
# touches the proxy. The resulting table looks like a catastrophic kernel
# regression when in truth the harness simply outlived its server. A dead server
# and a broken server produce different numbers, and only the harness can tell
# them apart.
#
# So: the run aborts the moment the proxy stops answering, and the partial file
# is annotated rather than being read as a verdict.
#
#   scripts/android-proxy-compare.sh <hosts-file> <domains-file> [out.tsv]
#
# Environment:
#   ADB, DEVICE, PORT, TIMEOUT   as in android-hosts-compare.sh
#   ADB_BIN_NODE                 path to the device binary (default: LDPlayer's)

set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:--s emulator-5554}"
PORT="${PORT:-18080}"
TIMEOUT="${TIMEOUT:-8}"
# The liveness probe deliberately uses a domain that is not in the ruleset.
PROBE_HOST="${PROBE_HOST:-example.com}"

HOSTS_FILE="${1:?usage: android-proxy-compare.sh <hosts-file> <domains-file> [out.tsv]}"
DOMAINS_FILE="${2:?missing domains file}"
OUT="${3:-tmp/android-proxy-compare.tsv}"

# Is the proxy still accepting connections?
#
# The probe uses an *unlisted* domain on purpose, because that is the one
# request whose correct answer is a flat refusal — the proxy replies 403 and
# hangs up without needing the network at all. curl surfaces that as exit 56
# ("recv failure"), which is a refusal, not an outage: it proves a live server
# read our CONNECT and answered it. Only exit 7 (connection refused) means the
# listener is gone.
#
# Probing with a *listed* domain instead would conflate "the proxy is dead" with
# "the upstream is unreachable", and the whole point is to tell those apart.
PROBE_HOST="${PROBE_HOST:-example.com}"
proxy_alive() {
  curl --silent --output /dev/null --noproxy "" --max-time 5 \
    -x "127.0.0.1:$PORT" -w '%{exitcode}' "https://$PROBE_HOST/" 2>/dev/null \
    | grep -qv '^7$'
}

"$ADB" $DEVICE forward --remove "tcp:$PORT" >/dev/null 2>&1 || true
"$ADB" $DEVICE forward "tcp:$PORT" "tcp:$PORT" >/dev/null || exit 1

"$ADB" $DEVICE shell "killall watt-daemon 2>/dev/null; true" >/dev/null 2>&1
"$ADB" $DEVICE push "$HOSTS_FILE" /data/local/tmp/watt/hosts-all.txt >/dev/null 2>&1
# `setsid` detaches from the adb shell's process group. Without it the daemon is
# a child of a shell that exits the moment the command returns, and on Android
# that has been observed to take the daemon down with it — the listener binds,
# then vanishes, and every later sample reads as a kernel failure.
"$ADB" $DEVICE shell \
  "cd /data/local/tmp/watt && setsid ./watt-daemon --proxy-listen 0.0.0.0:$PORT --hosts-file ./hosts-all.txt --stats-interval 3600 </dev/null > compare.log 2>&1 &" \
  >/dev/null 2>&1

# Give the listener a bounded window to come up. `sleep 3` alone is a guess: the
# daemon compiles 2858 rules before it binds, and on a loaded emulator that can
# take longer. Waiting on the condition rather than on a fixed delay is the
# difference between "it is not up yet" and "it will never be up".
ready=0
for _ in $(seq 1 20); do
  if proxy_alive; then ready=1; break; fi
  sleep 1
done
"$ADB" $DEVICE shell "cat /data/local/tmp/watt/compare.log" | tr -d '\r'

if [ "$ready" -ne 1 ]; then
  echo "android-proxy-compare: proxy never answered within 20s of starting" >&2
  "$ADB" $DEVICE shell "ps -A | grep '[w]att-daemon' || echo 'no watt-daemon process'" >&2
  exit 1
fi

printf 'domain\tdirect\tdirect_exit\tproxy\tproxy_ssl\tproxy_exit\tverdict\n' >"$OUT"
printf '\n%-42s %-22s %s\n' DOMAIN DIRECT PROXY

samples=0
aborted=0
while read -r domain; do
  domain="${domain%$'\r'}"
  [ -z "$domain" ] && continue
  case "$domain" in \#*) continue ;; esac

  # Check before each sample: the only way to attribute a failure to the domain
  # rather than to a server that quietly exited is to ask first.
  if ! proxy_alive; then
    echo "android-proxy-compare: proxy stopped answering after $samples samples; aborting" >&2
    aborted=1
    break
  fi

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
  samples=$((samples + 1))
done <"$DOMAINS_FILE"

"$ADB" $DEVICE shell "killall watt-daemon 2>/dev/null; true" >/dev/null 2>&1
echo
if [ "$aborted" -eq 1 ]; then
  echo "android-proxy-compare: PARTIAL ($samples samples) — treat tail as unusable, wrote $OUT"
  exit 2
fi
echo "android-proxy-compare: complete ($samples samples), wrote $OUT"
awk -F '\t' 'NR>1 {c[$7]++} END {for (k in c) printf "  %-14s %d\n", k, c[k]}' "$OUT"
