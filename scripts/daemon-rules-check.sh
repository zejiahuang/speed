#!/usr/bin/env bash
#
# Load the real upstream rule document through watt-daemon, both from disk and
# over the network, and check what the index resolves.
#
# This is the only place the real 1 MiB document meets the real fetcher: the unit
# tests use a fixture, and the smoke test uses a two-line document it writes
# itself. Neither would notice a fetcher that mishandles a redirect, a large body,
# or a document whose groups use the placeholder form.
#
# Every stage asserts on the output rather than only on the exit status. `--check`
# exits zero whatever the rules turn out to be, so a script that merely ran it and
# checked `$?` would pass on a daemon that loaded nothing at all.
#
#   bash scripts/daemon-rules-check.sh
#
# Environment:
#   HOSTS_URL_1  first live source (default: the UsbEAm short path `/1`)
#   HOSTS_URL_2  second live source (default: the S302 short path `/2`)
#   CACHE_DIR    where a JSON fetch would be cached (hosts writes none)
#
# No root needed: `--check` never opens a tunnel.
#
# --- why the network stages use `--hosts-url`, not `--rules-url` -------------
#
# The old `/rules` endpoint is gone (nginx 404) and no public replacement serves
# its shape. `/1` and `/2` serve *hosts text*; their `?format=json` is a different
# schema (`{"entries":[{"ip":..,"domain":..}]}`) that the daemon's `--rules-url`
# path (`RuleCache::store` -> `parse_document` -> `serde_json`, the `groups`
# shape) cannot read. Measured 2026-09-26:
#
#   watt-daemon --check --rules-url "https://abhuang.dpdns.org/1?format=json"
#     rules    origin=builtin                 (28 entries)
#     refresh  failed, keeping builtin: rule document has no usable entries
#     (and no cache directory is created -- `RuleSet::from_document` refuses the
#      zero-entry document with `NoUsableEntries`)
#
# So the JSON lane cannot be the live path, and this script now exercises the
# hosts path the daemon actually uses. The daemon's own default is the single
# source `/1` (`DEFAULT_HOSTS_URLS`); it is named explicitly below so this test
# does not silently follow a future change to the default.

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOSTS_URL_1="${HOSTS_URL_1:-https://abhuang.dpdns.org/1}"
# Not a default. `/2` is the S302 loopback-hijack set and is only ever named
# explicitly (see phase 2); it is defined here so the negative assertion can
# reference the same literal the app uses.
HOSTS_URL_2="${HOSTS_URL_2:-https://abhuang.dpdns.org/2}"
LOCAL_DOC="$REPO_DIR/tmp/rules.json"
CACHE_DIR="${CACHE_DIR:-/tmp/watt-rules-cache}"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
DAEMON="$CARGO_TARGET_DIR/debug/watt-daemon"

PASSED=0
FAILURES=0

check() { # <name> <ok: 0/1> <detail>
  if [ "$2" -eq 0 ]; then
    PASSED=$(( PASSED + 1 ))
    echo "CHECK PASS $1 ($3)"
  else
    FAILURES=$(( FAILURES + 1 ))
    echo "CHECK FAIL $1 ($3)"
  fi
}

# Run the daemon, capturing both streams into OUT and the status into RC.
#
# Deliberately not a pipeline or a subshell assignment at each call site: the
# output has to be asserted on, and a failing command must not trip `set -e`
# before the assertion gets to say why.
RC=0
OUT=""
run() {
  RC=0
  OUT="$("$@" 2>&1)" || RC=$?
}

if [ ! -x "$DAEMON" ]; then
  echo "daemon-rules-check: building the daemon"
  bash "$REPO_DIR/scripts/cargo.sh" build --quiet -p watt-daemon
fi

if [ ! -f "$LOCAL_DOC" ]; then
  echo "daemon-rules-check: no fixture at $LOCAL_DOC; running fetch-rules.sh"
  # NOTE: as of the endpoint change this cannot succeed against the public
  # endpoints (no live JSON `groups` shape exists) -- fetch-rules.sh explains and
  # fails loudly rather than writing a zero-entry fixture. The fixture already in
  # the tree is used when present, which is the normal case.
  bash "$REPO_DIR/scripts/fetch-rules.sh" || true
fi

if [ ! -f "$LOCAL_DOC" ]; then
  echo "daemon-rules-check: FAIL no local document at $LOCAL_DOC" >&2
  exit 1
fi

PROBES=(github.com)
# Domains the old `/rules` document deliberately did not cover.
#
# That document's top-level `filtered` list excluded twelve groups with the
# reason `PC_ONLY`, and Steam was one of them, so a Steam domain resolving to
# `direct` used to be the correct answer rather than a miss. The replacement
# endpoints changed this fact: `/2` IS the Steamcommunity 302 block and covers
# `steamcommunity.com` directly. This list is retained for the `--rules-file`
# fixture path (which still uses the old `groups` JSON), but it no longer
# describes what the live `/1` + `/2` sources do -- see the header. Under the new
# sources a `steamcommunity.com` probe yielding an address is correct, not a
# regression.
EXCLUDED_PROBES=(steamcommunity.com api.steampowered.com)
probe_flags=()
for domain in "${PROBES[@]}" "${EXCLUDED_PROBES[@]}"; do
  probe_flags+=(--probe "$domain")
done

# --- 1. a pinned JSON document, compiled from disk --------------------------
#
# This is the last place the old JSON `groups` shape is exercised: a pinned file
# isolates from the network, so `--rules-file` must compile exactly that document
# and fetch nothing (the defaults are dropped -- see `Options::parse`).
echo "=== 1. a pinned JSON document, compiled from disk ==="
run "$DAEMON" --check --rules-file "$LOCAL_DOC" "${probe_flags[@]}"
echo "$OUT"

DOC_BYTES="$(stat -c %s "$LOCAL_DOC")"
if [ "$RC" -eq 0 ] && grep -q 'origin=provided' <<<"$OUT"; then
  check "the local document was compiled" 0 "bytes=$DOC_BYTES"
else
  check "the local document was compiled" 1 "rc=$RC"
fi

# A pinned run must not also phone home for the default hosts sources.
if ! grep -q 'hosts    origin=https://' <<<"$OUT"; then
  check "a pinned file fetches no hosts source" 0 "no network sources"
else
  check "a pinned file fetches no hosts source" 1 "a hosts source was fetched anyway"
fi

# The document is the full historical JSON, so it has to be substantially larger
# than any fixture. A fetcher that truncated the body would still produce a valid file.
if [ "$DOC_BYTES" -gt 500000 ]; then
  check "the document is the whole published one" 0 "bytes=$DOC_BYTES"
else
  check "the document is the whole published one" 1 "bytes=$DOC_BYTES (expected > 500000)"
fi

# A covered domain has to resolve to something concrete. `direct` would mean the
# index missed a domain the document definitely lists.
COVERED_DIRECT="$(grep '^\[.*\] probe ' <<<"$OUT" | grep '^.*github\.com ' | grep -c 'strategy=direct' || true)"
COVERED_LINES="$(grep -c '^\[.*\] probe .*github\.com ' <<<"$OUT" || true)"
COVERED_RESOLVED=$(( COVERED_LINES - COVERED_DIRECT ))
if [ "$COVERED_LINES" -gt 0 ] && [ "$COVERED_RESOLVED" -eq "$COVERED_LINES" ]; then
  check "a covered domain resolved through the rules" 0 \
    "probes=$COVERED_LINES resolved=$COVERED_RESOLVED"
else
  check "a covered domain resolved through the rules" 1 \
    "probes=$COVERED_LINES resolved=$COVERED_RESOLVED"
fi

# A concrete address has to come out of it, not just a non-direct strategy: a rule
# whose entries are all placeholders resolves to `family-fallback` with nothing in
# hand, which is not what a domain with 39 published addresses should do.
COVERED_ADDRESSES="$(grep '^\[.*\] probe ' <<<"$OUT" | grep 'entry=GitHub.com' | grep -c 'addresses=\[' || true)"
if [ "$COVERED_ADDRESSES" -gt 0 ]; then
  check "a covered domain produced concrete addresses" 0 \
    "lines with addresses=$COVERED_ADDRESSES"
else
  check "a covered domain produced concrete addresses" 1 "no addresses in any probe line"
fi

# And the domains the document filters out have to stay unfiltered-in, so to speak:
# `direct` is the documented answer for them.
EXCLUDED_NOT_DIRECT=0
for domain in "${EXCLUDED_PROBES[@]}"; do
  LINES="$(grep '^\[.*\] probe ' <<<"$OUT" | grep -c " $domain " || true)"
  DIRECT="$(grep '^\[.*\] probe ' <<<"$OUT" | grep " $domain " | grep -c 'strategy=direct' || true)"
  if [ "$LINES" -eq 0 ] || [ "$LINES" -ne "$DIRECT" ]; then
    EXCLUDED_NOT_DIRECT=$(( EXCLUDED_NOT_DIRECT + 1 ))
  fi
done
if [ "$EXCLUDED_NOT_DIRECT" -eq 0 ]; then
  check "the PC_ONLY groups stay excluded, as the document says" 0 \
    "domains=${EXCLUDED_PROBES[*]} all direct"
else
  check "the PC_ONLY groups stay excluded, as the document says" 1 \
    "$EXCLUDED_NOT_DIRECT of ${#EXCLUDED_PROBES[@]} resolved when the document excludes them"
fi

# --- 2. the live hosts source fetched over the network, through curl --------
#
# This used to fetch the JSON document with `--rules-url`. That endpoint is
# retired and its replacement (`/1`) serves hosts text, so the same coverage --
# the real fetcher meeting the real, large, multi-group upstream payload -- now
# goes through `--hosts-url`.
#
# Only `/1` is fetched here, because `/1` alone is the daemon's default
# (`DEFAULT_HOSTS_URLS`). `/2` is a *different rule set*, not a second half of
# the same one: its addresses are all `127.0.0.1`, which the planner refuses to
# relay, so merging it into the default would turn 675 domains that work today
# into RSTs. It stays opt-in (`--hosts-url "$HOSTS_URL_2"`), and asserting it
# alongside `/1` here would bless exactly the regression we removed.
echo
echo "=== 2. the live hosts source fetched over the network, through curl ==="
run "$DAEMON" --check --hosts-url "$HOSTS_URL_1" "${probe_flags[@]}"
echo "$OUT"

NET_LINES="$(grep -c 'hosts    origin=https://' <<<"$OUT" || true)"
if [ "$RC" -eq 0 ] && [ "$NET_LINES" -eq 1 ]; then
  check "the default hosts source was fetched over the network" 0 "sources=$NET_LINES"
else
  check "the default hosts source was fetched over the network" 1 "rc=$RC sources=$NET_LINES"
fi

# `/2` must not be reachable from the default path at all. If a future change
# re-adds it to `DEFAULT_HOSTS_URLS`, this catches it without needing the
# network: the default run above would report two origins.
if grep -q "hosts    origin=$HOSTS_URL_2" <<<"$OUT"; then
  check "the loopback-hijack source is not in the default set" 1 \
    "$HOSTS_URL_2 appeared in a default run"
else
  check "the loopback-hijack source is not in the default set" 0 \
    "$HOSTS_URL_2 absent unless named explicitly"
fi

# A fetched rule set has to be substantially larger than any fixture: a fetcher
# that truncated the body would still produce a plausible-looking line count.
FETCHED_LINES="$(grep 'hosts    origin=https://' <<<"$OUT" | grep -o 'lines=[0-9]*' | head -1 | cut -d= -f2)"
if [ -n "$FETCHED_LINES" ] && [ "$FETCHED_LINES" -gt 10000 ]; then
  check "the fetched rules are the whole published set" 0 "lines=$FETCHED_LINES"
else
  check "the fetched rules are the whole published set" 1 "lines=${FETCHED_LINES:-none} (expected > 10000)"
fi

# --- 3. was anything cached? ------------------------------------------------
#
# Nothing should be: hosts sources are one-shot (see `Options::hosts_url`), so a
# hosts fetch deliberately writes no cache. This asserts the negative rather than
# leaving it implicit.
echo
echo "=== 3. the fetched copy leaves no JSON cache (hosts are one-shot) ==="
rm -rf "$CACHE_DIR"
run "$DAEMON" --check --hosts-url "$HOSTS_URL_1" --cache-dir "$CACHE_DIR"
echo "$OUT"

if [ ! -e "$CACHE_DIR/rules.json" ]; then
  check "a hosts fetch writes no JSON cache" 0 "no $CACHE_DIR/rules.json"
else
  check "a hosts fetch writes no JSON cache" 1 "unexpected $CACHE_DIR/rules.json"
fi

# --- 4. a second run fetches again (hosts are not cached) -------------------
echo
echo "=== 4. a second run fetches the hosts source again ==="
run "$DAEMON" --check --hosts-url "$HOSTS_URL_1" --cache-dir "$CACHE_DIR"
echo "$OUT"

if [ "$RC" -eq 0 ] && grep -q "hosts    origin=$HOSTS_URL_1" <<<"$OUT"; then
  check "a second run re-fetches the hosts source" 0 "origin=$HOSTS_URL_1"
else
  check "a second run re-fetches the hosts source" 1 "rc=$RC"
fi

# --- 5. no cache and no network, which must fall back to the built-in set ---
echo
echo "=== 5. no cache and no network, which must fall back to the built-in set ==="
rm -rf "$CACHE_DIR.builtin"
run "$DAEMON" --check --offline --cache-dir "$CACHE_DIR.builtin"
echo "$OUT"

if [ "$RC" -eq 0 ] && grep -q 'origin=builtin' <<<"$OUT"; then
  check "offline with no cache falls back to the built-in document" 0 "origin=builtin"
else
  check "offline with no cache falls back to the built-in document" 1 "rc=$RC"
fi

# --- 6. a placeholder-only rule, which must degrade instead of inventing ----
echo
echo "=== 6. a placeholder-only rule, which must degrade instead of inventing ==="
# The built-in document carries no concrete addresses at all — every entry is a
# placeholder — so a probe against it shows what the second strategy level does
# when it has nothing to hand out: it falls back, and the query goes to a real
# resolver rather than the kernel making an address up.
run "$DAEMON" --check --offline --cache-dir "$CACHE_DIR.builtin" \
  --probe github.com --probe raw.githubusercontent.com
echo "$OUT"

FALLBACKS="$(grep -c 'strategy=placeholder-fallback' <<<"$OUT" || true)"
INVENTED="$(grep -c 'addresses=[0-9]' <<<"$OUT" || true)"
if [ "$FALLBACKS" -gt 0 ] && [ "$INVENTED" -eq 0 ]; then
  check "a placeholder rule degrades instead of inventing an address" 0 \
    "fallbacks=$FALLBACKS invented=$INVENTED"
else
  check "a placeholder rule degrades instead of inventing an address" 1 \
    "fallbacks=$FALLBACKS invented=$INVENTED"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "RESULT PASS checks=$PASSED"
  echo "daemon-rules-check: PASS checks=$PASSED"
  exit 0
fi

echo "RESULT FAIL failures=$FAILURES passed=$PASSED"
echo "daemon-rules-check: FAIL failures=$FAILURES" >&2
exit 1
