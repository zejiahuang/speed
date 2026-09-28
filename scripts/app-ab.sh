#!/usr/bin/env bash
#
# Interleave tunnel and direct samples so the comparison is honest.
#
#   bash scripts/app-ab.sh <host> [rounds]
#
# The problem this solves: connectivity here drifts on a scale of minutes. A
# 30-sample tunnel run followed by a 30-sample direct run measures two different
# networks, and the difference between them gets attributed to the kernel. That
# mistake has been made twice now — once reading a lucky 20/20 direct window as
# "the emulator is fine", once reading a dead-egress window as "the kernel
# regressed".
#
# So alternate: one direct sample, one tunnel sample, one direct sample, ... Each
# pair is taken within a second or two of each other, and only the *difference*
# within a pair is meaningful. Reported as a pair-wise comparison plus a tally.
#
# Cost: two connect/disconnect round trips per round. At three seconds each and
# twenty rounds that is two minutes of state switching, which is the price of a
# result that means something.

set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"

HOST="${1:?usage: app-ab.sh <host> [rounds]}"
ROUNDS="${2:-20}"

control() {
  "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1
}

# Switch state and wait for it to be reported back, not merely requested.
#
# The settle is not politeness, it is required. Tearing a VPN down and bringing
# it back up makes Android remove and re-add the tun interface, and the framework
# unbinds the service in between. Doing that every couple of seconds produced a
# `DeadObjectException` in ActivityManager and left the control channel answering
# `running:false` to a `connect` that had been accepted — the app was fine, the
# framework had not finished with the previous cycle. A second of quiet on each
# end is what keeps the harness from testing Android's bookkeeping instead of the
# kernel.
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

# One request. Returns "<code> <seconds>".
probe() {
  "$ADB" -s "$DEVICE" shell \
    "curl -sS -o /dev/null -w '%{http_code} %{time_total}' --max-time 12 'https://$HOST/'" \
    2>/dev/null | tr -d '\r' | tail -1
}

d_ok=0; t_ok=0
d_time=0; t_time=0
pairs_t=""
pairs_d=""

echo "interleaved $HOST x$ROUNDS (direct, tunnel, direct, tunnel, ...)"
for i in $(seq "$ROUNDS"); do
  set_state off || exit 2
  dr="$(probe)"; dc="${dr%% *}"; dt="${dr##* }"

  set_state on || exit 2
  tr_="$(probe)"; tc="${tr_%% *}"; tt="${tr_##* }"

  printf '  %2d  direct=%s (%ss)   tunnel=%s (%ss)\n' "$i" "$dc" "${dt:-?}" "$tc" "${tt:-?}"

  [ "$dc" = "200" ] && d_ok=$((d_ok + 1))
  [ "$tc" = "200" ] && t_ok=$((t_ok + 1))
  [[ "$dt" =~ ^[0-9.]+$ ]] && d_time="$(awk -v a="$d_time" -v b="$dt" 'BEGIN{printf "%.3f", a+b}')"
  [[ "$tt" =~ ^[0-9.]+$ ]] && t_time="$(awk -v a="$t_time" -v b="$tt" 'BEGIN{printf "%.3f", a+b}')"
  pairs_d="$pairs_d ${dc:0:1}"
  pairs_t="$pairs_t ${tc:0:1}"
done

echo
echo "host=$HOST rounds=$ROUNDS   (interleaved: each pair is one moment)"
awk -v d="$d_ok" -v t="$t_ok" -v n="$ROUNDS" 'BEGIN{
  printf "  direct success = %d/%d = %.0f%%\n", d, n, 100*d/n
  printf "  tunnel success = %d/%d = %.0f%%\n", t, n, 100*t/n
  printf "  difference     = %+.0f points\n", 100*(t-d)/n
}'
awk -v d="$d_time" -v t="$t_time" -v n="$ROUNDS" 'BEGIN{
  printf "  mean direct time = %.2fs\n", d/n
  printf "  mean tunnel time = %.2fs\n", t/n
}'
echo "  direct codes: $pairs_d   (2=200, 0=no conn)"
echo "  tunnel codes: $pairs_t"
