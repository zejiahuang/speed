#!/usr/bin/env bash
#
# Do the tunnel and the kernel behave the same for hosts that have NO rule?
#
# The rule-driven path and the direct path are different code. A host that
# matches a rule has its connection rewritten to a rule address and carried
# through the candidate chain; a host with no rule is passed straight out
# (`flows_direct`), which should cost nothing and look exactly like no tunnel at
# all.
#
# This measures a mixed host list in ONE window, three ways per host:
#
#   direct    -- tunnel down, straight out
#   tunnel    -- tunnel up
#   tunnel#2  -- tunnel again immediately (did the first pass warm anything?)
#
# and reads the kernel counters on both sides of the tunnel arm, so the claim
# "this host went direct" is backed by `flows_direct` moving rather than by
# assumption.
#
# Why one window: connectivity on this emulator drifts on a scale of minutes, so
# a direct sweep and a tunnel sweep taken ten minutes apart compare two different
# networks. Every host is measured in all three modes back to back.
#
# Usage:
#   bash scripts/app-host-matrix.sh [--hosts "a b c"] [--rounds N]

set -u

DEVICE="${DEVICE:-emulator-5554}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG=dev.detour
WORK="${WORK:-tmp}"

# A deliberate mix:
#   - rule carriers       (github.com, flathub.org, apache.org, dl.google.com)
#   - no-rule mainstream  (bing.com, baidu.com, bilibili.com, 4399.com)
#   - no-rule reachable   (www.qq.com, www.163.com)
#   - no-rule blocked     (google.com, www.youtube.com) -- should behave like
#     direct, i.e. fail, because there is no rule to rescue them
HOSTS="${HOSTS:-github.com flathub.org apache.org dl.google.com bing.com baidu.com bilibili.com www.4399.com google.com www.qq.com www.163.com}"
ROUNDS="${ROUNDS:-3}"

while [ $# -gt 0 ]; do
  case "$1" in
    --hosts)  HOSTS="$2"; shift 2 ;;
    --rounds) ROUNDS="$2"; shift 2 ;;
    *) echo "matrix: unknown option $1" >&2; exit 2 ;;
  esac
done

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }
mkdir -p "$WORK"

to_win() {
  local path="$1" abs
  abs="$(cd "$(dirname "$path")" 2>/dev/null && pwd)/$(basename "$path")"
  case "$abs" in
    /[a-zA-Z]/*) printf '%s' "$(echo "${abs:1:1}" | tr 'a-z' 'A-Z'):${abs:2}" ;;
    *) printf '%s' "$abs" ;;
  esac
}

control() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd $1" >/dev/null 2>&1
}

pid_of() {
  "$ADB" "${ADB_ARGS[@]}" shell 'pidof dev.detour' 2>/dev/null | tr -d '\r' | head -1
}

read_stats() {
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd status" >/dev/null 2>&1
  sleep 1
  "$ADB" "${ADB_ARGS[@]}" logcat -d -s DetourControl:I 2>/dev/null \
    | tr -d '\r' | grep -o '{"phase".*}' | tail -1
}

# Pull one numeric field out of the status JSON. A status line that failed to
# arrive must not silently read as zero -- that turns "unknown" into "no traffic
# went direct", which is the opposite conclusion.
stat_field() {
  printf '%s' "$1" | sed -n "s/.*\"$2\":\([0-9-]*\).*/\1/p" | head -1
}

# One host, `rounds` requests, on the device; prints "code dns connect tls ttfb total".
# Component timings are the point: they say WHERE the cost is.
cat >"$WORK/matrix-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = host, $2 = rounds, $3 = timeout
host="$1"
rounds="$2"
tmo="$3"
url="https://$host/"
r=1
while [ "$r" -le "$rounds" ]; do
  curl -sS -o /dev/null \
    -w "%{http_code} %{time_namelookup} %{time_connect} %{time_appconnect} %{time_starttransfer} %{time_total}\n" \
    --max-time "$tmo" "$url" 2>/dev/null || printf '000 0 0 0 0 0\n'
  r=$((r + 1))
done
RUNNER

[ -x "$ADB" ] || { say "matrix: adb not executable at $ADB"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/matrix-runner.sh")" /data/local/tmp/watt/matrix-runner.sh >/dev/null 2>&1 \
  || { say "matrix: FAILED to push the runner"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/matrix-runner.sh' >/dev/null 2>&1

run_one() {
  "$ADB" "${ADB_ARGS[@]}" shell \
    "sh /data/local/tmp/watt/matrix-runner.sh $1 $ROUNDS 20" 2>/dev/null | tr -d '\r'
}

summarise() {
  awk -v L="$1" '
    {
      code=$1; dns=$2+0; con=$3+0; tls=$4+0; ttfb=$5+0; tot=$6+0
      n++
      # ANY three-digit status means the host was reached and answered -- a 404
      # or a 403 is a successful connection, not a failure.
      if (code ~ /^[0-9]{3}$/ && code != "000") reached++
      s_dns+=dns; s_con+=con; s_tls+=tls; s_ttfb+=ttfb; s_tot+=tot
      if (tot > mx) mx=tot
    }
    END {
      if (n == 0) { printf "%-9s no data", L; exit }
      printf "%-9s reached=%d/%d  total=%.2fs  tls=%.2fs  ttfb=%.2fs  max=%.2fs",
        L, reached+0, n, s_tot/n, s_tls/n, s_ttfb/n, mx
    }
  '
}

OUT="$WORK/host-matrix.tsv"
: >"$OUT"

if [ -z "$(pid_of)" ]; then
  say "matrix: the app is not running; start it first"
  exit 1
fi
PID="$(pid_of)"
say "matrix: app pid=$PID  rounds=$ROUNDS  hosts=$(printf '%s' "$HOSTS" | wc -w)"
say ""

for host in $HOSTS; do
  D="$WORK/matrix-direct.$host.txt"
  T="$WORK/matrix-tunnel.$host.txt"
  R="$WORK/matrix-repeat.$host.txt"

  # --- direct arm: tunnel down ---
  control disconnect >/dev/null 2>&1
  sleep 3
  run_one "$host" > "$D"

  # --- tunnel arm ---
  control connect >/dev/null 2>&1
  sleep 5
  SB="$(read_stats)"
  FD_BEFORE="$(stat_field "$SB" flows_direct)"
  TM_BEFORE="$(stat_field "$SB" flows_matched_rules)"
  run_one "$host" > "$T"
  # A warmup request so the *second* tunnel pass is the one that shows whether
  # the first paid a one-time candidate-chain cost.
  run_one "$host" > "$R"
  SA="$(read_stats)"
  FD_AFTER="$(stat_field "$SA" flows_direct)"
  TM_AFTER="$(stat_field "$SA" flows_matched_rules)"

  DSUM="$(summarise direct < "$D")"
  TSUM="$(summarise tunnel < "$T")"
  RSUM="$(summarise 'tunnel#2' < "$R")"

  printf '%s\n' "$DSUM"
  printf '%s\n' "$TSUM"
  printf '%s\n' "$RSUM"

  # Which path did this host actually take? A rule host moves
  # `flows_matched_rules`; a no-rule host moves `flows_direct`.
  #
  # Absent counters must not read as zero: "unknown" would then look identical
  # to "no traffic went direct", which is the opposite conclusion. Report `?`
  # unless both readings arrived.
  if [ -n "$FD_BEFORE" ] && [ -n "$FD_AFTER" ] && [ -n "$TM_BEFORE" ] && [ -n "$TM_AFTER" ]; then
    DR=$(( FD_AFTER - FD_BEFORE ))
    MR=$(( TM_AFTER - TM_BEFORE ))
    if [ "$MR" -gt 0 ] && [ "$DR" -le 0 ]; then
      PATHKIND="rule"
    elif [ "$DR" -gt 0 ]; then
      PATHKIND="direct"
    else
      PATHKIND="neither"
    fi
    say "          path=$PATHKIND  (flows_direct +$DR, flows_matched_rules +$MR)"
  else
    PATHKIND="?"
    say "          path=?  (kernel counters unavailable)"
  fi

  printf '%s\t%s\t%s\t%s\t%s\n' "$host" "$PATHKIND" "$DSUM" "$TSUM" "$RSUM" >>"$OUT"
  echo ""
done

say "matrix: raw per-host bytes in $WORK/matrix-{direct,tunnel,repeat}.<host>.txt"
say "matrix: table in $OUT"
