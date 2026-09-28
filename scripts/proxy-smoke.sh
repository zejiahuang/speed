#!/usr/bin/env bash
#
# Real HTTP CONNECT smoke test. It deliberately starts no local TLS server: the
# client owns TLS and the daemon only copies opaque bytes to a real origin.
#
#   scripts/proxy-smoke.sh
#
# Environment:
#   LISTED_HOST   concrete rule domain to fetch (default: nikke-en.com)
#   UNLISTED_HOST domain that must be rejected (default: example.com)
#   PROXY_ADDR    local listener (default: 127.0.0.1:18080)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$REPO_DIR/tmp"
DAEMON="${DAEMON:-$HOME/.cache/watt-target/debug/watt-daemon}"
RULES="${RULES:-$TMP_DIR/rules.json}"
PROXY_ADDR="${PROXY_ADDR:-127.0.0.1:18080}"
LISTED_HOST="${LISTED_HOST:-nikke-en.com}"
UNLISTED_HOST="${UNLISTED_HOST:-example.com}"
LOG="$TMP_DIR/proxy-smoke-daemon.log"
PIDFILE="$TMP_DIR/proxy-smoke.pid"

[ -x "$DAEMON" ] || { echo "proxy-smoke: daemon not found: $DAEMON" >&2; exit 1; }
[ -f "$RULES" ] || { echo "proxy-smoke: rules not found: $RULES" >&2; exit 1; }

cleanup() {
  set +e
  [ -s "$PIDFILE" ] && kill -TERM "$(cat "$PIDFILE")" 2>/dev/null
  rm -f "$PIDFILE"
}
trap cleanup EXIT

"$DAEMON" --proxy-listen "$PROXY_ADDR" --rules-file "$RULES" \
  --stats-interval 3600 >"$LOG" 2>&1 &
echo $! >"$PIDFILE"

for _ in $(seq 1 50); do
  grep -q "proxy listening" "$LOG" 2>/dev/null && break
  sleep 0.1
done
if ! grep -q "proxy listening" "$LOG" 2>/dev/null; then
  echo "proxy-smoke: proxy did not start" >&2
  cat "$LOG" >&2
  exit 1
fi

LISTED_OUT="$TMP_DIR/proxy-listed.bin"
LISTED_META="$(curl --silent --show-error --noproxy "" --max-time 30 \
  -x "$PROXY_ADDR" -o "$LISTED_OUT" \
  -w '%{http_code} %{size_download} %{ssl_verify_result}' \
  "https://$LISTED_HOST/")"
LISTED_RC=$?
UNLISTED_META="$(curl --silent --show-error --noproxy "" --max-time 15 \
  -x "$PROXY_ADDR" -o /dev/null \
  -w '%{http_code}' "https://$UNLISTED_HOST/")"
UNLISTED_RC=$?

PASSED=0
FAILURES=0
check() {
  if [ "$2" -eq 0 ]; then PASSED=$((PASSED + 1)); echo "CHECK PASS $1 ($3)"
  else FAILURES=$((FAILURES + 1)); echo "CHECK FAIL $1 ($3)"
  fi
}

read -r code bytes verify <<<"$LISTED_META"
check "listed domain completed CONNECT and TLS" \
  $([ "$LISTED_RC" -eq 0 ] && [ "$code" != "000" ] && [ "$verify" = "0" ]; echo $?) \
  "host=$LISTED_HOST http=${code:-?} bytes=${bytes:-?} ssl_verify_result=${verify:-?}"
check "unlisted domain was rejected" \
  $([ "$UNLISTED_RC" -ne 0 ] && [ "$UNLISTED_META" = "000" ]; echo $?) \
  "host=$UNLISTED_HOST curl_rc=$UNLISTED_RC proxy_status=${UNLISTED_META:-?}"

if [ "$FAILURES" -eq 0 ]; then
  echo "RESULT PASS checks=$PASSED"
  exit 0
fi
echo "RESULT FAIL failures=$FAILURES passed=$PASSED"
exit 1
