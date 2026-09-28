#!/usr/bin/env bash
#
# Sample connectivity through the running app, many times, and print a rate.
#
#   bash scripts/app-sample.sh <host> [trials] [mode]
#
#     host    e.g. github.com
#     trials  default 20
#     mode    "tunnel" (default) or "direct"
#
# Why this exists: a single failed request in this environment is not evidence
# of anything. The emulator's own network fails roughly three times in four.
# The only honest way to say "the kernel works" is to compare a success RATE
# through the tunnel against the same rate without it. See MEMORY.md.
#
# "direct" turns the app off via the control channel first so the emulator uses
# its own resolver and its own route, then turns it back on. That makes the two
# numbers comparable: same device, same moment, same radio.

set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"
PROXY_PORT=1080

HOST="${1:?usage: app-sample.sh <host> [trials] [tunnel|direct]}"
TRIALS="${2:-20}"
MODE="${3:-tunnel}"

control() {
  "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1
}

wait_running() {
  # Poll status until it agrees with the wanted state, or give up.
  #
  # The state we read is `running`, not `phase`: after `connect` the phase is
  # "on" while the engine spins up, and only then does running flip to true.
  # Waiting on phase would return while the tunnel is still dead.
  local want="$1" tries="${2:-30}"
  for _ in $(seq "$tries"); do
    control --es cmd status
    sleep 1
    local line
    line="$("$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' \
      | grep " $TAG: " | tail -1 | sed 's/.*'"$TAG"': //')"
    case "$want:$line" in
      true:*'"running":true'*)  return 0 ;;
      false:*'"running":false'*) return 0 ;;
    esac
  done
  return 1
}

if [ "$MODE" = "direct" ]; then
  echo "disconnecting for the direct baseline"
  control --es cmd disconnect
  if ! wait_running false 30; then
    echo "could not disconnect; baseline would be meaningless" >&2
    exit 2
  fi
else
  echo "connecting (vpn mode)"
  control --es cmd mode --es value vpn
  control --es cmd connect
  if ! wait_running true 45; then
    echo "could not bring the app up" >&2
    exit 2
  fi
fi

# One request, timed, on the device. We ask curl for the status code only, and
# cap it at 12s — anything longer is a failure in this environment anyway, and
# waiting the full default would make 20 trials take forever.
probe_once() {
  local url="https://$HOST/"
  "$ADB" -s "$DEVICE" shell \
    "curl -sS -o /dev/null -w '%{http_code} %{time_total}' --max-time 12 '$url'" \
    2>/dev/null | tr -d '\r' | tail -1
}

ok=0
other=0
fail=0
total_time=0
declare -a codes=()

echo "sampling $HOST x$TRIALS via $MODE ..."
for i in $(seq "$TRIALS"); do
  line="$(probe_once)"
  code="${line%% *}"
  t="${line##* }"
  codes+=("$code")
  case "$code" in
    2*|3*) ok=$((ok + 1)) ;;
    000)   fail=$((fail + 1)) ;;
    *)     other=$((other + 1)) ;;
  esac
  if [[ "$t" =~ ^[0-9.]+$ ]]; then
    total_time="$(awk -v a="$total_time" -v b="$t" 'BEGIN{printf "%.3f", a+b}')"
  fi
  printf '  %2d  code=%s  %ss\n' "$i" "$code" "${t:-?}"
done

echo
echo "mode=$MODE host=$HOST trials=$TRIALS"
echo "  2xx/3xx  = $ok"
echo "  other    = $other   (e.g. 4xx/5xx — reached the server)"
echo "  no conn  = $fail    (000 — never got an answer)"
awk -v ok="$ok" -v n="$TRIALS" \
  'BEGIN{printf "  success rate = %.0f%%\n", 100*ok/n}'
echo "  total time across trials = ${total_time}s"
echo "  codes: ${codes[*]}"
