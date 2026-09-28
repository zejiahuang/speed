#!/usr/bin/env bash
#
# Does the *path* to an address work right now, with and without the tunnel?
#
#   bash scripts/app-path.sh <ip> [rounds]
#
# Why this exists next to `app-ab.sh`. That script compares a hostname, and a
# hostname compounds two different questions: "does DNS give me a usable
# address" and "is that address reachable". When both sides answer 000 the
# result cannot say which question failed, and the failures here have been
# read as a kernel fault three times when they were a dead route.
#
# This one asks the second question alone. `curl --resolve` pins the
# hostname to one address so no resolver — the emulator's, the tunnel's, or
# a poisoned one — is in the loop, and `-k` skips the certificate so the
# server's identity is not the variable either. What is left is the path.
#
# The comparison is then meaningful in a way the hostname version never was:
# direct-works and tunnel-fails is the kernel's fault; both-fail is the
# network's; both-work with the tunnel slower is the kernel's tax.
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"

IP="${1:?usage: app-path.sh <ip> [rounds]}"
ROUNDS="${2:-10}"
# The SNI the pinned address should expect. A github Azure edge serves
# github.com; override when probing something else.
HOST="${HOST:-github.com}"

control() {
  "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1
}

# Switch state and wait for the report, not the request. See app-ab.sh for why
# the quiet period is load-bearing rather than polite.
set_state() {
  local want="$1" tries=30 line
  if [ "$want" = "on" ]; then
    control --es cmd mode --es value vpn
    control --es cmd connect
  else
    control --es cmd disconnect
  fi
  for _ in $(seq "$tries"); do
    control --es cmd status
    sleep 1
    line="$("$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' \
      | grep " $TAG: " | tail -1 | sed 's/.*'"$TAG"': //')"
    case "$want:$line" in
      on:*'"running":true'*)   sleep "${SETTLE:-2}"; return 0 ;;
      off:*'"running":false'*) sleep "${SETTLE:-2}"; return 0 ;;
    esac
  done
  echo "could not switch to $want" >&2
  return 1
}

# One request to the pinned path. Returns "<code> <seconds>".
probe() {
  "$ADB" -s "$DEVICE" shell \
    "curl -sS -k -o /dev/null -w '%{http_code} %{time_total}' --max-time 12 --resolve $HOST:443:$IP https://$HOST/" \
    2>/dev/null | tr -d '\r' | tail -1
}

d_ok=0; t_ok=0
d_codes=""; t_codes=""
d_time=0; t_time=0

echo "path to $IP as $HOST, x$ROUNDS, interleaved (direct, tunnel, ...)"
for i in $(seq "$ROUNDS"); do
  set_state off || exit 2
  dr="$(probe)"; dc="${dr%% *}"; dt="${dr##* }"

  set_state on || exit 2
  tr_="$(probe)"; tc="${tr_%% *}"; tt="${tr_##* }"

  printf '  %2d  direct=%s (%ss)   tunnel=%s (%ss)\n' "$i" "$dc" "${dt:-?}" "$tc" "${tt:-?}"

  [ "$dc" = "301" ] || [ "$dc" = "200" ] && d_ok=$((d_ok + 1))
  [ "$tc" = "301" ] || [ "$tc" = "200" ] && t_ok=$((t_ok + 1))
  [[ "$dt" =~ ^[0-9.]+$ ]] && d_time="$(awk -v a="$d_time" -v b="$dt" 'BEGIN{printf "%.3f", a+b}')"
  [[ "$tt" =~ ^[0-9.]+$ ]] && t_time="$(awk -v a="$t_time" -v b="$tt" 'BEGIN{printf "%.3f", a+b}')"
  d_codes="$d_codes ${dc:0:1}"
  t_codes="$t_codes ${tc:0:1}"
done

echo
echo "ip=$IP host=$HOST rounds=$ROUNDS  (a 301 is the edge answering: the path works)"
awk -v d="$d_ok" -v t="$t_ok" -v n="$ROUNDS" 'BEGIN{
  printf "  direct reachable = %d/%d = %.0f%%\n", d, n, 100*d/n
  printf "  tunnel reachable = %d/%d = %.0f%%\n", t, n, 100*t/n
}'
awk -v d="$d_time" -v t="$t_time" -v n="$ROUNDS" 'BEGIN{
  printf "  mean direct time = %.2fs\n", d/n
  printf "  mean tunnel time = %.2fs\n", t/n
}'
echo "  direct codes: $d_codes   (3/2=ok, 0=no conn)"
echo "  tunnel codes: $t_codes"
echo
echo "reading: both columns failing = the route is dead, not the kernel."
echo "         direct ok + tunnel 0 = the kernel did not carry it."
