#!/usr/bin/env bash
#
# Why does one request take 3-5 seconds through the tunnel?
#
# The concurrency sweep found mean latency of ~4-6s at concurrency 1, where there
# is no contention at all, and every request still returned 200. A single HTTPS
# GET to a reachable host should be well under a second. So the cost is on the
# *path*, not in the load.
#
# This measures the same host, sequentially, three ways in one window:
#
#   direct   -- straight out, no tunnel
#   tunnel   -- through the VPN
#   repeat   -- tunnel again, right after (does the first one warm anything?)
#
# Timing all three back to back is the only way to compare them: connectivity
# drifts on a scale of minutes, so a direct run and a tunnel run taken ten
# minutes apart are two different networks.

set -u

DEVICE="${DEVICE:-emulator-5554}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG=dev.detour
WORK="${WORK:-tmp}"

HOSTS="${HOSTS:-github.com flathub.org apache.org dl.google.com}"
ROUNDS="${ROUNDS:-3}"

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

# One host, one request, on the device, printing "code time_total dns connect
# tls first_byte". The component timings are the whole point: if connect or tls
# dominates, the cost is in the candidate chain, not in throughput.
cat >"$WORK/timing-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = host, $2 = rounds, $3 = timeout
host="$1"
rounds="$2"
tmo="$3"
url="https://$host/"
r=1
while [ "$r" -le "$rounds" ]; do
  curl -sS -o /dev/null \
    -w "%{http_code} %{time_total} %{time_namelookup} %{time_connect} %{time_appconnect} %{time_starttransfer}\n" \
    --max-time "$tmo" "$url" 2>/dev/null || printf '000 0 0 0 0 0\n'
  r=$((r + 1))
done
RUNNER

[ -x "$ADB" ] || { say "timing: adb not executable at $ADB"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/timing-runner.sh")" /data/local/tmp/watt/timing-runner.sh >/dev/null 2>&1 \
  || { say "timing: FAILED to push the runner"; exit 1; }
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/timing-runner.sh' >/dev/null 2>&1

run_one() {
  local host="$1" which="$2"
  "$ADB" "${ADB_ARGS[@]}" shell \
    "sh /data/local/tmp/watt/timing-runner.sh $host $ROUNDS 25" 2>/dev/null | tr -d '\r'
}

summarise() {
  local label="$1" file="$2"
  awk -v L="$label" '
    {
      code=$1; tot=$2+0; dns=$3+0; con=$4+0; tls=$5+0; ttfb=$6+0
      n++
      # Any 3-digit status is a REACHED host: the server answered, whether that
      # answer was 200 or 404. Crediting only 2xx/3xx reads a working 404 as a
      # failure, which is how a healthy dl.google.com got reported as ok=0 on
      # every arm -- including direct.
      if (code ~ /^[0-9]{3}$/ && code != "000") ok++
      s_tot+=tot; s_dns+=dns; s_con+=con; s_tls+=tls; s_ttfb+=ttfb
    }
    END {
      if (n == 0) { printf "  %-10s no data\n", L; exit }
      printf "  %-10s n=%-3d reached=%-3d  total=%.2fs  dns=%.2fs  connect=%.2fs  tls=%.2fs  ttfb=%.2fs\n",
        L, n, ok+0, s_tot/n, s_dns/n, s_con/n, s_tls/n, s_ttfb/n
    }
  ' "$file"
}

OUT="$WORK/timing-compare.tsv"
: >"$OUT"

say "timing: hosts=$HOSTS  rounds=$ROUNDS each"
say "timing: all three arms run back to back per host, so the window is shared"
say ""

for host in $HOSTS; do
  say "== $host =="

  # Per-host raw files. Reusing one filename per arm meant only the LAST host's
  # bytes survived the loop -- the github.com rows were gone by the time anything
  # wanted to re-read them.
  D="$WORK/timing-direct.$host.txt"
  T="$WORK/timing-tunnel.$host.txt"
  R="$WORK/timing-repeat.$host.txt"

  # Tunnel down for the direct arm, up again for the tunnel arm.
  control disconnect >/dev/null 2>&1
  sleep 3
  run_one "$host" direct > "$D"

  control connect >/dev/null 2>&1
  sleep 5
  # A warmup request so the first timed one is not paying the candidate chain.
  "$ADB" "${ADB_ARGS[@]}" shell "curl -sS -o /dev/null --max-time 25 https://$host/ >/dev/null 2>&1" >/dev/null 2>&1
  run_one "$host" tunnel > "$T"

  # Same again immediately: if this is much faster, the first pass paid a
  # one-time cost (verdict recording, dial-name resolution) rather than a
  # per-request cost.
  run_one "$host" repeat > "$R"

  summarise "direct" "$D"
  summarise "tunnel" "$T"
  summarise "tunnel#2" "$R"

  awk -v h="$host" -v f="$T" '
    {t=$2+0; n++; s+=t} END {printf "%s\t%d\t%.3f\n", h, n, (n?s/n:0)}' "$T" >>"$OUT"
  echo ""
done

say "timing: raw component timings in $WORK/timing-{direct,tunnel,repeat}.<host>.txt"
