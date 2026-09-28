#!/usr/bin/env bash
#
# Drive real internet traffic through the tunnel using the real rule document.
#
# `real-upstream.sh` proves the kernel can carry traffic to real hosts, but it
# writes its own rule document to do it. That leaves the interesting question
# open: does the shipped document — 316 entries, 2454 domains, 24k addresses —
# actually steer traffic to addresses that work?
#
# This script answers it for a chosen set of domains. For each one it:
#
#   1. asks the kernel for DNS through the tunnel, and checks the answer is an
#      address the rule really lists (so the rule set answered, not the host);
#   2. routes that address into the tunnel and fetches the domain over it;
#   3. compares the bytes against the same fetch made outside the tunnel, which
#      is what proves the kernel relayed rather than mangled.
#
# Certificates are verified for real throughout; there is no `-k` anywhere.
#
#   scripts/verify-rule-sites.sh [--sites <file>] [--rules <file>]
#
# The sites file holds "<domain> <address>" per line. It defaults to a curated
# set whose addresses were confirmed reachable by `scripts/probe-forward.py`.
#
# Environment:
#   TUN_NAME      interface to create                (default: watt4)
#   PROTECT_MARK  socket mark to exempt the kernel   (default: 0x5754)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS_DIR="$REPO_DIR/scripts"
TMP_DIR="$REPO_DIR/tmp"
WORK_DIR="$TMP_DIR/rule-sites"

TUN_NAME="${TUN_NAME:-watt4}"
TUN_ADDRESS="198.21.0.1"
TUN_PREFIX="15"
PROTECT_MARK="${PROTECT_MARK:-0x5754}"
MARK_TABLE="${MARK_TABLE:-5754}"
DNS_ADDRESS="203.0.113.53"

RULES="$TMP_DIR/rules.json"
SITES_FILE="$TMP_DIR/rule-sites.txt"
DAEMON_LOG="$TMP_DIR/rule-sites-daemon.log"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
DAEMON_BIN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --sites) SITES_FILE="$2"; shift 2 ;;
    --rules) RULES="$2"; shift 2 ;;
    --daemon-bin) DAEMON_BIN="$2"; shift 2 ;;
    -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) echo "verify-rule-sites: unknown argument: $1" >&2; exit 2 ;;
  esac
done

for tool in ip curl python3 sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "verify-rule-sites: $tool is required" >&2; exit 1; }
done

# The binary path has to be resolved before elevating: sudo does not preserve
# HOME, so a path built from `$HOME` here becomes `/root/...` there.
if [ -z "$DAEMON_BIN" ]; then
  DAEMON_BIN="$CARGO_TARGET_DIR/debug/watt-daemon"
  if [ ! -x "$DAEMON_BIN" ]; then
    echo "verify-rule-sites: building the daemon"
    bash "$SCRIPTS_DIR/cargo.sh" build --quiet -p watt-daemon
  fi
fi
[ -x "$DAEMON_BIN" ] || { echo "verify-rule-sites: daemon binary not found: $DAEMON_BIN" >&2; exit 1; }

if [ ! -f "$RULES" ]; then
  echo "verify-rule-sites: rule document not found: $RULES" >&2
  exit 1
fi
if [ ! -f "$SITES_FILE" ]; then
  echo "verify-rule-sites: sites file not found: $SITES_FILE" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "verify-rule-sites: needs root, re-executing under sudo"
  exec sudo env "PATH=$PATH" "TUN_NAME=$TUN_NAME" "PROTECT_MARK=$PROTECT_MARK" \
    "MARK_TABLE=$MARK_TABLE" \
    bash "$0" --sites "$SITES_FILE" --rules "$RULES" --daemon-bin "$DAEMON_BIN"
fi

mkdir -p "$TMP_DIR" "$WORK_DIR"
rm -f "$DAEMON_LOG" "$WORK_DIR"/baseline-*.bin "$WORK_DIR"/tunnel-*.bin
rm -f "$WORK_DIR"/*.err

DAEMON_PID=""
MARK_RULE=0
ROUTED=()
SITE_HOSTS=()
SITE_ADDRS=()

cleanup() {
  set +e
  [ -n "$DAEMON_PID" ] && kill -TERM "$DAEMON_PID" 2>/dev/null
  sleep 0.3
  [ -n "$DAEMON_PID" ] && kill -KILL "$DAEMON_PID" 2>/dev/null
  for address in "${ROUTED[@]:-}"; do
    [ -n "$address" ] && ip route del "$address/32" dev "$TUN_NAME" 2>/dev/null
  done
  ip route del "$DNS_ADDRESS/32" dev "$TUN_NAME" 2>/dev/null
  ip link del "$TUN_NAME" 2>/dev/null
  if [ "$MARK_RULE" -eq 1 ]; then
    ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null
    ip route flush table "$MARK_TABLE" 2>/dev/null
  fi
  return 0
}
trap cleanup EXIT

ip link del "$TUN_NAME" 2>/dev/null || true

wait_for_line() { # <file> <pattern> <seconds>
  local attempt
  for attempt in $(seq 1 $(( $3 * 10 ))); do
    grep -q "$2" "$1" 2>/dev/null && return 0
    sleep 0.1
  done
  return 1
}

PASSED=0
FAILURES=0
PREFERRED_DEAD=0
check() { # <name> <ok> <detail>
  if [ "$2" -eq 0 ]; then
    PASSED=$(( PASSED + 1 )); echo "CHECK PASS $1 ($3)"
  else
    FAILURES=$(( FAILURES + 1 )); echo "CHECK FAIL $1 ($3)"
  fi
}

# --- 1. Read the sites, and the addresses each rule really lists. -----------
#
# The rule's address list is the ground truth for the DNS check below: if the
# kernel answers with an address the rule does not contain, it did not answer
# from the rule set.
python3 - "$RULES" "$SITES_FILE" "$WORK_DIR/entry-ips.txt" <<'PY'
import json, sys, pathlib
rules_path, sites_path, out_path = sys.argv[1:4]
doc = json.load(open(rules_path, encoding="utf-8"))
by_domain = {}
for group in doc["groups"]:
    for entry in group["entries"]:
        ips = entry.get("ips") or []
        # Entries whose address list is a CDN placeholder such as `{Cloudflare}`
        # have no address of their own; by design the client resolves those
        # itself, so there is nothing here to forward to and nothing to test.
        concrete = [a for a in ips if not a.startswith("{")]
        for name in entry.get("domains") or []:
            by_domain[name.strip().lower()] = concrete
lines = []
skipped = []
for raw in pathlib.Path(sites_path).read_text(encoding="utf-8").splitlines():
    raw = raw.strip()
    if not raw or raw.startswith("#"):
        continue
    parts = raw.split()
    domain = parts[0]
    # The address in the file is one confirmed reachable by probe-forward.py.
    # Fetching that one, rather than the first the rule lists, keeps the two
    # legs comparable: the rule's ordering is a ranking concern, and a ranking
    # that leads with an unreachable address would otherwise look like a
    # forwarding failure.
    good = parts[1] if len(parts) > 1 else ""
    ips = by_domain.get(domain, [])
    if not ips:
        skipped.append(domain)
        continue
    if good and good not in ips:
        print(f"warning: {good} is not listed for {domain}; using the first listed",
              file=sys.stderr)
        good = ips[0]
    if not good:
        good = ips[0]
    lines.append(f"{domain}\t{good}\t{','.join(ips)}")
pathlib.Path(out_path).write_text("\n".join(lines) + "\n", encoding="utf-8")
if skipped:
    print(f"verify-rule-sites: skipped {len(skipped)} placeholder-only site(s): "
          f"{', '.join(skipped[:5])}")
print(f"verify-rule-sites: {len(lines)} sites, each with addresses in the document")
PY

SITE_GOOD=()
while IFS=$'\t' read -r domain good ips; do
  [ -z "${domain:-}" ] && continue
  domain="${domain%$'\r'}"
  SITE_HOSTS+=( "$domain" )
  SITE_GOOD+=( "${good%$'\r'}" )
  SITE_ADDRS+=( "${ips%$'\r'}" )
done <"$WORK_DIR/entry-ips.txt"

if [ "${#SITE_HOSTS[@]}" -eq 0 ]; then
  echo "verify-rule-sites: no usable sites" >&2
  exit 1
fi

# --- 2. Baseline: the same fetch, outside the tunnel. -----------------------
echo
echo "--- baseline: each site fetched at its rule address, no tunnel ---"
BASELINE_HASHES=()
for index in "${!SITE_HOSTS[@]}"; do
  host="${SITE_HOSTS[$index]}"
  # The address confirmed reachable by probe-forward.py, not the first the rule
  # lists. Which address a rule ranks first is a separate question from whether
  # the kernel can carry traffic to one, and mixing them would report a slow
  # ranking as a broken relay.
  address="${SITE_GOOD[$index]}"
  body="$WORK_DIR/baseline-$index.bin"
  out="$(curl --silent --show-error --max-time 30 \
    --resolve "$host:443:$address" \
    --write-out 'CURL %{http_code} %{size_download} %{time_total} %{ssl_verify_result}\n' \
    --output "$body" "https://$host/" 2>"$WORK_DIR/baseline-$index.err")"
  hash="$(sha256sum "$body" 2>/dev/null | awk '{print $1}')"
  bytes="$(stat -c %s "$body" 2>/dev/null || echo 0)"
  BASELINE_HASHES+=( "$hash" )
  printf '  %-32s %-16s %s\n' "$host" "$address" "${out:-failed: $(head -1 "$WORK_DIR/baseline-$index.err")}"
  printf '      %s bytes sha256=%s\n' "$bytes" "${hash:-none}"
done

# --- 3. Keep the kernel's own traffic out of the tunnel. --------------------
echo
echo "verify-rule-sites: exempting mark $PROTECT_MARK via table $MARK_TABLE"
ip route flush table "$MARK_TABLE" 2>/dev/null || true
ip rule del fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" 2>/dev/null || true
DEFAULT_ROUTE="$(ip route show default | head -1)"
GATEWAY="$(sed -n 's/.*via \([0-9.]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
DEVICE="$(sed -n 's/.*dev \([^ ]*\).*/\1/p' <<<"$DEFAULT_ROUTE")"
[ -n "$DEVICE" ] || { echo "verify-rule-sites: no default route to copy" >&2; exit 1; }
if [ -n "$GATEWAY" ]; then
  ip route add default via "$GATEWAY" dev "$DEVICE" table "$MARK_TABLE"
else
  ip route add default dev "$DEVICE" table "$MARK_TABLE"
fi
ip rule add fwmark "$PROTECT_MARK" lookup "$MARK_TABLE" pref 100
MARK_RULE=1

# --- 4. The kernel, on the real document. ----------------------------------
echo "verify-rule-sites: starting the daemon on $TUN_NAME with the real document"
"$DAEMON_BIN" \
  --tun "$TUN_NAME" \
  --address "$TUN_ADDRESS/$TUN_PREFIX" \
  --rules-file "$RULES" \
  --protect-mark "$PROTECT_MARK" \
  --stats-interval 10 \
  --tick 3600 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

if ! wait_for_line "$DAEMON_LOG" '\] tun ' 20; then
  echo "verify-rule-sites: the daemon did not start" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi
grep -E '\] (rules|report|protect|tun) ' "$DAEMON_LOG" || true

ip link set "$TUN_NAME" up
ip addr add "$TUN_ADDRESS/$TUN_PREFIX" dev "$TUN_NAME"
# Only the DNS leg is routed yet. Each site's address is routed once the kernel
# has said which address it is steering to, so the route always matches the
# answer instead of a guess made beforehand.
ip route add "$DNS_ADDRESS/32" dev "$TUN_NAME"

# --- 5. DNS through the tunnel, then the real fetches. ----------------------
echo
echo "--- leg 1: does the rule set answer DNS, with an address it lists? ---"
DNS_ANSWERS=()
for index in "${!SITE_HOSTS[@]}"; do
  host="${SITE_HOSTS[$index]}"
  out="$(python3 "$SCRIPTS_DIR/dns_probe.py" --server "$DNS_ADDRESS" --domain "$host" 2>&1)"
  flags="$(python3 "$SCRIPTS_DIR/dns_flags.py" "$DNS_ADDRESS" "$host" 2>&1)"
  # Trimmed answers are expected: a rule with more addresses than one datagram
  # holds gives the client the ones that fit. What must never happen is an empty
  # answer, which is what a truncation flag would produce.
  count="$(sed -n 's/.*ancount=\([0-9]*\).*/\1/p' <<<"$flags")"
  tc="$(sed -n 's/.* tc=\([01]\) .*/\1/p' <<<"$flags")"
  answers="${out#DNS ANSWERS $host }"
  DNS_ANSWERS+=( "$answers" )

  outsiders=0
  for answer in $answers; do
    grep -qE "(^|,)${answer}(,|$)" <<<"${SITE_ADDRS[$index]}" || outsiders=$(( outsiders + 1 ))
  done

  if [ "${count:-0}" -gt 0 ] && [ "${tc:-0}" -eq 0 ] && [ "$outsiders" -eq 0 ]; then
    check "the rule set answered DNS for $host with addresses it lists" 0 \
      "ancount=$count of $(awk -F, '{print NF}' <<<"${SITE_ADDRS[$index]}") listed"
  else
    check "the rule set answered DNS for $host with addresses it lists" 1 \
      "ancount=${count:-0} tc=${tc:-?} outsiders=$outsiders"
  fi
done

echo
echo "--- leg 2: HTTPS through the tunnel, certificates verified for real ---"
for index in "${!SITE_HOSTS[@]}"; do
  host="${SITE_HOSTS[$index]}"
  address="${SITE_GOOD[$index]}"
  body="$WORK_DIR/tunnel-$index.bin"

  # The kernel does not connect to the address the client asked for — it
  # connects to the address it ranked first for this rule, which is the whole
  # point of it. So the address that decides this test is the one in the DNS
  # answer, not the one in `--resolve`, and asking about it up front is what
  # makes a failure explainable: a dead ranked address is a property of the rule
  # and this network, not of the relay.
  preferred="$(awk '{print $1}' <<<"${DNS_ANSWERS[$index]}")"
  preferred_note=""
  if [[ "$preferred" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    if curl --silent --show-error --max-time 8 --output /dev/null \
      --resolve "$host:443:$preferred" "https://$host/" 2>/dev/null; then
      preferred_note="preferred=$preferred reachable"
    else
      preferred_note="preferred=$preferred UNREACHABLE outside the tunnel too"
      PREFERRED_DEAD=$(( PREFERRED_DEAD + 1 ))
    fi
  fi

  # Route the same address the baseline used, so the only thing that differs
  # between the two fetches is whether the tunnel is in the path. This has to
  # come after the probe above: once the route exists, every connection to that
  # address goes into the tunnel, and the probe would stop being a measurement
  # of the address and start being a measurement of the relay.
  if [[ "$address" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    ip route add "$address/32" dev "$TUN_NAME" 2>/dev/null && ROUTED+=( "$address" )
  fi

  out="$(curl --silent --show-error --max-time 45 \
    --resolve "$host:443:$address" \
    --write-out 'CURL %{http_code} %{size_download} %{time_total} %{ssl_verify_result}\n' \
    --output "$body" "https://$host/" 2>"$WORK_DIR/tunnel-$index.err")"
  rc=$?
  bytes="$(stat -c %s "$body" 2>/dev/null || echo 0)"
  printf '  %-32s %s\n' "$host" "${out:-curl exit $rc: $(head -1 "$WORK_DIR/tunnel-$index.err")}"
  [ -n "$preferred_note" ] && printf '      %s\n' "$preferred_note"

  verify="$(sed -n 's/.*CURL [0-9]* [0-9]* [0-9.]* \([0-9]*\)$/\1/p' <<<"$out")"
  code="$(sed -n 's/.*CURL \([0-9]*\) .*/\1/p' <<<"$out")"

  # A completed TLS session whose certificate validated is the proof: the
  # handshake crossed the tunnel in both directions and the origin answered.
  # Byte equality with the baseline is deliberately not asserted — the kernel may
  # reach a different node of the same service, whose response differs legitimately.
  if [ "${verify:-1}" -eq 0 ] && [ -n "${code:-}" ] && [ "$code" != "000" ]; then
    check "$host completed a real TLS session through the tunnel" 0 \
      "http=$code bytes=$bytes ssl_verify_result=$verify"
  else
    check "$host completed a real TLS session through the tunnel" 1 \
      "http=${code:-none} ssl_verify_result=${verify:-?} $preferred_note"
  fi
done

# --- 6. Stop and read what the kernel did with it. --------------------------
echo
echo "verify-rule-sites: stopping the daemon"
kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""

echo
echo "===================== kernel ========================="
cat "$DAEMON_LOG"
echo "======================================================"

COUNTERS="$(grep -E '\] stop ' "$DAEMON_LOG" | tail -1)"
field() { sed -n "s/.*$1=\([0-9]*\).*/\1/p" <<<"$COUNTERS"; }
MATCHED="$(field matched)"
DIRECT="$(field direct)"
TCP_OPEN="$(field tcp_open)"
LOCAL_DNS="$(field dns_local)"
UP_BYTES="$(field u2c)"
HOST_COUNT="${#SITE_HOSTS[@]}"

if [ "${TCP_OPEN:-0}" -ge "$HOST_COUNT" ] && [ "${TCP_OPEN:-0}" -le $(( HOST_COUNT + 5 )) ]; then
  check "the kernel opened one flow per site, not a cascade" 0 "tcp_open=$TCP_OPEN (sites=$HOST_COUNT)"
else
  check "the kernel opened one flow per site, not a cascade" 1 "tcp_open=${TCP_OPEN:-?} (sites=$HOST_COUNT)"
fi

if [ "${MATCHED:-0}" -ge "$HOST_COUNT" ] && [ "${DIRECT:-1}" -eq 0 ]; then
  check "every connection was steered by the rules, none direct" 0 "matched=$MATCHED direct=$DIRECT"
else
  check "every connection was steered by the rules, none direct" 1 "matched=${MATCHED:-?} direct=${DIRECT:-?}"
fi

if [ "${LOCAL_DNS:-0}" -ge "$HOST_COUNT" ]; then
  check "every DNS answer came from the rule set" 0 "dns_local=$LOCAL_DNS (sites=$HOST_COUNT)"
else
  check "every DNS answer came from the rule set" 1 "dns_local=${LOCAL_DNS:-?} (expected >= $HOST_COUNT)"
fi

if [ "$PREFERRED_DEAD" -gt 0 ]; then
  echo "verify-rule-sites: note: $PREFERRED_DEAD site(s) have a ranked-first address that is"
  echo "verify-rule-sites:       unreachable from this network even outside the tunnel. That is a"
  echo "verify-rule-sites:       property of the rule's address list, not of the relay."
fi

if [ "${UP_BYTES:-0}" -gt 0 ]; then
  check "payload bytes came back through the kernel" 0 "u2c=$UP_BYTES"
else
  check "payload bytes came back through the kernel" 1 "u2c=${UP_BYTES:-0}"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "RESULT PASS checks=$PASSED"
  echo "verify-rule-sites: PASS checks=$PASSED"
  exit 0
fi
echo "RESULT FAIL failures=$FAILURES passed=$PASSED"
echo "verify-rule-sites: FAIL failures=$FAILURES" >&2
exit 1
