#!/usr/bin/env bash
# Download the upstream rule document into tmp/rules.json.
#
# The file is a test fixture and an offline bootstrap snapshot; it is not part of
# the shipped binary, which is why it lives in tmp/ and is git ignored.
#
# --- the endpoint change, and why this script now fails loudly -----------------
#
# The retired endpoints (`/rules`, `/hosts?all=1`) returned an nginx 404. Their
# replacement, `/1`, is UsbEAm host records, and it is served as *hosts text* by
# default. `/1?format=json` exists but is a DIFFERENT SCHEMA from the document
# `watt_rules::parse_document` (and this script's validator) expects:
#
#   endpoint  { "entries": [ { "ip": "..", "domain": "..", "comment": ".." } ] }
#   expected  { "groups":  [ { "entries": [ { "domains": [".."], "ips": [".."] } ] } ] }
#
# Measured 2026-09-26: `curl "https://abhuang.dpdns.org/1?format=json"` returns
# `{"ok":true,...,"entries":[15971 objects]}` with NO `groups` key, so the python
# validator below reports `rule document contains no entries` and exits 1. A
# broken fixture is therefore never published over a good one — that part still
# works.
#
# NOTE: `tmp/rules.json` is consumed by `watt-rules/tests/upstream_fixture.rs`,
# which parses it as JSON. No live endpoint serves the `groups` JSON shape any
# more, so this script CANNOT currently produce a usable fixture from the public
# endpoints — it downloads, then the validator refuses the result. `RULES_URL` is
# left on `?format=json` deliberately: that is the shape the validator and the
# fixture test need, so the failure names the real problem instead of switching
# to hosts text (which the JSON validator would reject as invalid JSON, obscuring
# why). Point RULES_URL at a rules-puller instance that emits the `groups` shape
# to restore it. The hosts-text form the kernel actually reads is /1 and /2 via
# `--hosts-url` (verified: lines=15971 addresses=15952 domains=5225 skipped=19).
# See the report accompanying this change.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# `?format=json` is required by this script's JSON validator and by
# `upstream_fixture.rs`. It is NOT a document the kernel parses — for the kernel,
# use the hosts-text URL (`/1`, `/2`) with `--hosts-url`, not `--rules-url`.
RULES_URL="${RULES_URL:-https://abhuang.dpdns.org/1?format=json}"
OUT="${1:-$REPO_DIR/tmp/rules.json}"

mkdir -p "$(dirname "$OUT")"

printf '==> fetching %s\n' "$RULES_URL"
curl -fsSL --max-time 120 --retry 2 "$RULES_URL" -o "$OUT.partial"

if [ ! -s "$OUT.partial" ]; then
  echo "download produced an empty file" >&2
  rm -f "$OUT.partial"
  exit 1
fi

# Validate before publishing so a bad response never replaces a good snapshot.
python3 - "$OUT.partial" <<'PY'
import json, sys
with open(sys.argv[1], 'rb') as handle:
    doc = json.load(handle)
groups = doc.get('groups') or []
entries = sum(len(g.get('entries') or []) for g in groups)
domains = sum(len(e.get('domains') or []) for g in groups for e in (g.get('entries') or []))
if not entries or not domains:
    raise SystemExit('rule document contains no entries')
print(f"    version={doc.get('meta', {}).get('version')} groups={len(groups)} entries={entries} domains={domains}")
PY

mv "$OUT.partial" "$OUT"
printf '==> wrote %s (%s bytes)\n' "$OUT" "$(wc -c < "$OUT")"
