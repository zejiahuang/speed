#!/usr/bin/env bash
#
# Ask, for each "<domain> <ip>" pair on stdin, whether that address actually
# serves that domain: fetch it with the address pinned, and report the HTTP
# status, the TLS verification result and the time.
#
# This is the cheap way to find out whether a rule's address is the right one,
# without building a tunnel. It answers the same question the kernel will face
# when it forwards a flow to that address.
#
#   scripts/precheck-rule-ip.sh pairs.tsv
#
# Output: "<domain> <ip> -> <http_code> <ssl_verify_result> <seconds>"

set -uo pipefail

TIMEOUT="${TIMEOUT:-12}"

if [ $# -lt 1 ]; then
  echo "usage: precheck-rule-ip.sh <domain-ip-pairs.tsv>" >&2
  exit 2
fi
PAIRS="$1"

exec 3<"$PAIRS"
while read -r domain ip <&3; do
  [ -z "${domain:-}" ] && continue
  case "$domain" in \#*) continue ;; esac

  # Files written on the Windows side of this repo arrive with CRLF endings.
  # A trailing CR silently hides inside the `--resolve` argument and makes curl
  # fail instantly with status 000, which looks exactly like a network problem.
  domain="${domain%$'\r'}"
  ip="${ip%$'\r'}"
  [ -z "${ip:-}" ] && continue

  result="$(curl --silent --show-error --max-time "$TIMEOUT" \
    --resolve "$domain:443:$ip" \
    --output /dev/null \
    --write-out '%{http_code} %{ssl_verify_result} %{time_total}' \
    "https://$domain/" 2>>"${ERROR_LOG:-/dev/null}")"

  if [ -z "$result" ]; then
    result="curl-failed"
  fi
  echo "$domain $ip -> $result"
done
