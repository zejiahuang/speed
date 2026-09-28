#!/usr/bin/env bash
#
# Probe every domain the rule document lists, and report which ones the router
# matches and to which addresses.
#
# This answers "does the rule set cover its own domains" — a question that is
# easy to get wrong by sampling, because a miss is invisible until the domain
# you wanted is the one that falls through to `direct`.
#
#   scripts/probe-all-domains.sh [-o <out.tsv>]
#
# Environment:
#   DAEMON_BIN   path to watt-daemon            (default: built debug binary)
#   BATCH        domains per daemon invocation  (default: 400)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$REPO_DIR/tmp"
RULES="$TMP_DIR/rules.json"
DOMAINS="$TMP_DIR/rule-domains.txt"
OUT="${1:-$TMP_DIR/probe-all-domains.tsv}"

while [ $# -gt 0 ]; do
  case "$1" in
    -o|--out) OUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "probe-all-domains: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ ! -f "$DOMAINS" ]; then
  echo "probe-all-domains: $DOMAINS is missing" >&2
  exit 1
fi
if [ ! -f "$RULES" ]; then
  echo "probe-all-domains: $RULES is missing; run scripts/daemon-rules-check.sh first" >&2
  exit 1
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
DAEMON_BIN="${DAEMON_BIN:-$CARGO_TARGET_DIR/debug/watt-daemon}"
BATCH="${BATCH:-400}"

if [ ! -x "$DAEMON_BIN" ]; then
  echo "probe-all-domains: building the daemon"
  bash "$REPO_DIR/scripts/cargo.sh" build --quiet -p watt-daemon || exit 1
fi

WORK_DIR="$TMP_DIR/probe-all"
mkdir -p "$WORK_DIR"
RAW="$WORK_DIR/raw.log"
: >"$RAW"

total="$(grep -c . "$DOMAINS")"
echo "probe-all-domains: probing $total domains in batches of $BATCH"

batch_num=0
while read -r chunk_file; do
  batch_num=$((batch_num + 1))
  # Build the argv from the chunk. `--probe` is repeatable; one daemon run per
  # chunk keeps the argument list well inside the kernel's limit.
  mapfile -t names <"$chunk_file"
  args=()
  for name in "${names[@]}"; do
    args+=(--probe "$name")
  done
  echo "probe-all-domains: batch $batch_num (${#names[@]} domains)"
  "$DAEMON_BIN" --check --rules-file "$RULES" "${args[@]}" >>"$RAW" 2>&1
done < <(split -l "$BATCH" -d -a 4 "$DOMAINS" "$WORK_DIR/chunk-" && printf '%s\n' "$WORK_DIR"/chunk-*)

echo "probe-all-domains: parsed $(grep -c '\] probe ' "$RAW") probe results"

python3 - "$RAW" "$OUT" "$TMP_DIR/rule-domains.tsv" <<'PY'
import re, sys, csv

raw_path, out_path, meta_path = sys.argv[1:4]

meta = {}
with open(meta_path, encoding="utf-8") as f:
    next(f)
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 5:
            meta[parts[0]] = parts[1:]

# v4 and v6 are reported separately; a domain counts as matched if either
# family produced concrete addresses.
best = {}
# `port` is `-` when nothing matched, so it cannot be narrowed to digits.
line_re = re.compile(
    r"^\s*\[\s*[0-9.]+s\]\s+probe\s+(\S+)\s+(v4|v6)\s+strategy=(\S+)\s+"
    r"entry=(.*?)\s+addresses=\[([^\]]*)\]\s+port=(\S+)"
)
with open(raw_path, encoding="utf-8", errors="replace") as f:
    for line in f:
        m = line_re.match(line)
        if not m:
            continue
        domain, family, strategy, entry, addresses, port = m.groups()
        addrs = [a.strip() for a in addresses.split(",") if a.strip()]
        # "+N more" is the daemon eliding a long list; the count is still real
        # but the addresses are not, so keep the ones that were printed.
        rec = best.get(domain)
        if rec is None:
            rec = {"domain": domain, "strategy": strategy, "entry": entry,
                   "port": port, "addrs": [], "fams": []}
            best[domain] = rec
        rec["fams"].append(family)

        # v4 and v6 are reported separately and can disagree: an IPv6-only rule
        # answers `family-fallback` for v4 and `rule-addresses` for v6. The
        # family that actually produced addresses is the one that describes
        # what would happen on the wire, so it wins over the order of arrival.
        rank = {"rule-addresses": 3, "placeholder-fallback": 2,
                "family-fallback": 1, "direct": 0}
        better = rank.get(strategy, 0) > rank.get(rec["strategy"], 0)
        if better:
            rec["strategy"] = strategy
            rec["entry"] = entry
            rec["port"] = port
            rec["addrs"] = addrs
        elif addrs and not rec["addrs"]:
            rec["addrs"] = addrs

with open(out_path, "w", encoding="utf-8", newline="\n") as f:
    w = csv.writer(f, delimiter="\t")
    w.writerow(["domain", "group", "entry_expected", "entry_matched",
                "strategy", "family", "addresses", "matched", "klass",
                "placeholder"])
    klass_by_strategy = {
        "rule-addresses": "redirect",        # the rule supplies real addresses
        "placeholder-fallback": "upstream-dns",  # matched, but only a CDN token
        "family-fallback": "other-family",   # addresses exist, not in this family
        "direct": "unmatched",
    }
    for domain in sorted(best):
        r = best[domain]
        group, exp, port, ph = meta.get(domain, ["", "", "", ""])
        # "matched" means the rule set knows the domain. It does not mean the
        # traffic is redirected — a placeholder rule deliberately hands the
        # client back to its own resolver.
        matched = "0" if r["strategy"] == "direct" else "1"
        w.writerow([domain, group, exp, r["entry"], r["strategy"],
                    "+".join(sorted(set(r["fams"]))), ",".join(r["addrs"][:4]),
                    matched, klass_by_strategy.get(r["strategy"], r["strategy"]),
                    ph])

classes = {}
for r in best.values():
    k = klass_by_strategy.get(r["strategy"], r["strategy"])
    classes[k] = classes.get(k, 0) + 1
for k in sorted(classes, key=lambda k: -classes[k]):
    print(f"probe-all-domains: {k:<14} {classes[k]:>5}")
print(f"probe-all-domains: {len(best)} domains probed")
print(f"probe-all-domains: wrote {out_path}")
PY
