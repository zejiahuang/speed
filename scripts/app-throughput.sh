#!/usr/bin/env bash
#
# Measure tunnel throughput against the direct baseline, in one window.
#
#   bash scripts/app-throughput.sh <host> [rounds]
#
# Reports bytes per second for the same URL twice — once with the tunnel up, once
# with it down — interleaved so both numbers describe the same network. Bytes and
# speed, not status codes: a status code says a request completed, while this
# question is how long the body took. A page that arrives in 6 s and one that
# arrives in 0.5 s are both `200`, and only the second is usable.
#
# The speed comes from curl's own measurement rather than wall-clock around the
# call, so process startup and adb round trips do not inflate the tunnel's number.

set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"

HOST="${1:?usage: app-throughput.sh <host> [rounds]}"
ROUNDS="${2:-5}"

control() { "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1; }

set_state() {
  local want="$1" tries=30 line
  if [ "$want" = "on" ]; then
    control --es cmd mode --es value vpn; control --es cmd connect
  else
    control --es cmd disconnect
  fi
  for _ in $(seq "$tries"); do
    control --es cmd status
    sleep 1
    line="$("$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' \
      | grep " $TAG: " | tail -1 | sed 's/.*'"$TAG"': //')"
    case "$want:$line" in
      on:*'"running":true'*)   sleep 2; return 0 ;;
      off:*'"running":false'*) sleep 2; return 0 ;;
    esac
  done
  echo "could not switch to $want" >&2
  return 1
}

# Returns "<speed> <bytes> <seconds>".
#
# The timeout is 12 s, not the 40 s an earlier version used. Forty seconds looked
# generous and was the opposite: against a flapping path it made every sample a
# full forty-second wait, so four rounds took five minutes and the later rounds
# landed in a different network weather than the earlier ones — the exact
# confound this script exists to avoid. Twelve seconds is longer than any success
# observed here (the worst was ~11 s) and short enough that a round completes
# while the window is still open.
fetch() {
  "$ADB" -s "$DEVICE" shell \
    "curl -sS -o /dev/null -w '%{speed_download} %{size_download} %{time_total}' \
       --max-time 12 'https://$HOST/'" 2>/dev/null | tr -d '\r' | tail -1
}

d_sum=0; t_sum=0; d_n=0; t_n=0

echo "throughput $HOST x$ROUNDS (direct then tunnel, same window)"
for i in $(seq "$ROUNDS"); do
  set_state off || exit 2
  d="$(fetch)"

  set_state on || exit 2
  t="$(fetch)"

  printf '  %2d  direct: %14s B/s  %9s B  %6ss\n' "$i" \
    "$(printf '%s' "$d" | cut -d' ' -f1)" \
    "$(printf '%s' "$d" | cut -d' ' -f2)" \
    "$(printf '%s' "$d" | cut -d' ' -f3)"
  printf '      tunnel: %14s B/s  %9s B  %6ss\n' \
    "$(printf '%s' "$t" | cut -d' ' -f1)" \
    "$(printf '%s' "$t" | cut -d' ' -f2)" \
    "$(printf '%s' "$t" | cut -d' ' -f3)"

  ds="$(printf '%s' "$d" | cut -d' ' -f1)"
  ts="$(printf '%s' "$t" | cut -d' ' -f1)"
  case "$ds" in ''|*[!0-9.]*) ;; *) d_sum="$(awk -v a="$d_sum" -v b="$ds" 'BEGIN{printf "%.0f", a+b}')"; d_n=$((d_n+1));; esac
  case "$ts" in ''|*[!0-9.]*) ;; *) t_sum="$(awk -v a="$t_sum" -v b="$ts" 'BEGIN{printf "%.0f", a+b}')"; t_n=$((t_n+1));; esac
done

echo
awk -v d="$d_sum" -v dn="$d_n" -v t="$t_sum" -v tn="$t_n" 'BEGIN{
  if (dn > 0) printf "  mean direct = %8.0f B/s  (%d usable samples)\n", d/dn, dn
  else        print  "  mean direct = no usable samples"
  if (tn > 0) printf "  mean tunnel = %8.0f B/s  (%d usable samples)\n", t/tn, tn
  else        print  "  mean tunnel = no usable samples"
  if (dn > 0 && tn > 0) printf "  tunnel is %.2fx the direct rate\n", (t/tn)/(d/dn)
}'
