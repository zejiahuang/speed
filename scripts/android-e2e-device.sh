#!/bin/sh
#
# Device side of the Android end-to-end test. Pushed to the device and run as
# root by scripts/android-e2e.sh.
#
# Android routes differently from a desktop Linux, and that is the whole reason
# this file exists. There is no `main` table lookup in the rule set: the rules
# select a per-network table (wlan0, dummy0, ...) by uid and fwmark, and the
# final rule is `unreachable`. A route added to `main` is therefore consulted by
# nobody. The tunnel's routes go into their own table, reached by a rule placed
# ahead of the network ones.
#
# The kernel's own sockets are exempted the same way as on a host — a fwmark
# pointing at a table that does not contain the tunnel — but the mark rule has
# to sit *before* the tunnel rule, or the exemption never gets a chance.

set -u

DIR=/data/local/tmp/watt
TUN=watt0
TUN_ADDR=198.18.0.1/15
DNS_ADDR=203.0.113.53
MARK=0x5754
TUN_TABLE=5754
BYPASS_TABLE=5755
LOG="$DIR/daemon.log"
OUT="$DIR/results.txt"

# Sites: "<domain> <address>" lines, read from $DIR/sites.txt
GATEWAY="$(ip route show table wlan0 2>/dev/null | sed -n 's/.*via \([0-9.]*\).*/\1/p' | head -1)"
DEV="$(ip route show table wlan0 2>/dev/null | sed -n 's/.*dev \([^ ]*\).*/\1/p' | head -1)"

cleanup() {
  [ -n "${DAEMON_PID:-}" ] && kill -TERM "$DAEMON_PID" 2>/dev/null
  sleep 1
  [ -n "${DAEMON_PID:-}" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  ip rule del pref 5000 2>/dev/null
  ip rule del pref 4000 2>/dev/null
  ip route flush table "$TUN_TABLE" 2>/dev/null
  ip route flush table "$BYPASS_TABLE" 2>/dev/null
  ip link del "$TUN" 2>/dev/null
}
trap cleanup EXIT

: >"$OUT"
echo "ANDROID $(getprop ro.build.version.release) sdk=$(getprop ro.build.version.sdk) abi=$(getprop ro.product.cpu.abi)" | tee -a "$OUT"
echo "gateway=$GATEWAY dev=$DEV" | tee -a "$OUT"

ip link del "$TUN" 2>/dev/null
ip route flush table "$TUN_TABLE" 2>/dev/null
ip route flush table "$BYPASS_TABLE" 2>/dev/null
ip rule del pref 5000 2>/dev/null
ip rule del pref 4000 2>/dev/null

# --- 1. The kernel. ---------------------------------------------------------
cd "$DIR" || exit 1
./watt-daemon --tun "$TUN" --address "$TUN_ADDR" --rules-file ./rules.json \
  --protect-mark "$MARK" --stats-interval 2 --tick 3600 >"$LOG" 2>&1 &
DAEMON_PID=$!

for _ in 1 2 3 4 5 6 7 8 9 10; do
  grep -q '\] tun ' "$LOG" 2>/dev/null && break
  sleep 1
done
if ! grep -q '\] tun ' "$LOG" 2>/dev/null; then
  echo "FAIL the daemon did not start" | tee -a "$OUT"
  cat "$LOG" | tee -a "$OUT"
  exit 1
fi
grep -E '\] (rules|protect|tun) ' "$LOG" | tee -a "$OUT"

ip link set "$TUN" up
ip addr add 198.18.0.1/15 dev "$TUN" 2>/dev/null

# --- 2. Keep the kernel's own traffic out of the tunnel. --------------------
ip route add default via "$GATEWAY" dev "$DEV" table "$BYPASS_TABLE"
ip rule add fwmark "$MARK" lookup "$BYPASS_TABLE" pref 4000

# --- 3. Send the tunnel's addresses, and nothing else, into it. -------------
ip route add "$DNS_ADDR/32" dev "$TUN" table "$TUN_TABLE"
while read -r domain address; do
  [ -z "${domain:-}" ] && continue
  ip route add "$address/32" dev "$TUN" table "$TUN_TABLE"
done <"$DIR/sites.txt"
ip rule add from all lookup "$TUN_TABLE" pref 5000

echo "--- routes into $TUN ---" | tee -a "$OUT"
ip route show table "$TUN_TABLE" | tee -a "$OUT"

# --- 4. DNS, answered by the rule set. --------------------------------------
#
# libcurl on this image has no --dns-servers, so the query is built on the host
# and sent with netcat. The reply is pulled back and parsed there.
echo "--- leg 1: DNS ---" | tee -a "$OUT"
while read -r domain address; do
  [ -z "${domain:-}" ] && continue
  nc -u -q 1 -w 3 "$DNS_ADDR" 53 <"$DIR/query-$domain.bin" >"$DIR/reply-$domain.bin" 2>/dev/null
  size=$(wc -c <"$DIR/reply-$domain.bin" 2>/dev/null || echo 0)
  echo "$domain query_sent reply_bytes=$size" | tee -a "$OUT"
done <"$DIR/sites.txt"

# --- 5. HTTPS through the tunnel, certificates verified for real. -----------
echo "--- leg 2: HTTPS ---" | tee -a "$OUT"
while read -r domain address; do
  [ -z "${domain:-}" ] && continue
  result=$(curl -s -o "$DIR/body-$domain.bin" \
    --resolve "$domain:443:$address" --max-time 30 \
    -w '%{http_code} %{size_download} %{time_total} %{ssl_verify_result}' \
    "https://$domain/" 2>&1)
  echo "$domain $address -> $result" | tee -a "$OUT"
done <"$DIR/sites.txt"

# --- 6. What the kernel says it did. ----------------------------------------
echo "--- leg 3: kernel ---" | tee -a "$OUT"
kill -TERM "$DAEMON_PID" 2>/dev/null
wait "$DAEMON_PID" 2>/dev/null
DAEMON_PID=""
cat "$LOG" | tee -a "$OUT"
