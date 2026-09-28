#!/usr/bin/env python3
"""Build the domain list the wide test sweeps, grouped and ordered.

Why a generator instead of a checked-in list. The rule document is the source
of truth and it changes on its own schedule (1.0.47 -> 1.0.54 in one week), so a
hand-written list is stale the moment it is written and nobody notices. This
re-derives the list from whatever document is on disk, which means the sweep
always tests the rules that actually ship.

A domain is included when it is a *name* the client could type and the rule
gives it at least one concrete address:

  * wildcards are skipped — `*.githubusercontent.com` is a matching pattern, and
    fetching the pattern asks a question the rules were never meant to answer;
  * entries whose only addresses are CDN placeholders (`{Cloudflare}`) are
    skipped, because the client resolves those itself and there is no host to
    forward to;
  * negation entries (`!foo`) are skipped.

Output is TSV: group, entry name, domain, address count. Sorted by group then by
address count descending, so the widest-supported hosts — which are also the
ones most likely to work — come first and a truncated run still covers the
best cases rather than an alphabetical slice of the alphabet.

Usage: build-sweep-list.py <rules.json> <out.tsv>
"""

import json
import sys
from collections import defaultdict

# Groups in the order they matter to the user. "For Web" and "developer" are
# what someone opens a browser for; "In Game" is the largest but the least
# likely to be diagnosable from a status code alone. The sweep runs in this
# order so an interrupted run has covered the interesting half first.
GROUP_ORDER = [
    "developer",
    "For Web",
    "For Service",
    "For Tools",
    "Microsoft Live",
    "XBOX/Microsoft Store",
    "Other Platforms",
    "In Game",
    "CDN for open-source",
    "Academic",
]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    rules_path, out_path = sys.argv[1], sys.argv[2]

    doc = json.load(open(rules_path, encoding="utf-8"))

    # One row per unique domain. The first entry to claim a domain wins; a
    # domain listed by two entries is the same host either way, and counting it
    # twice would weight the sweep toward whichever group happens to repeat.
    seen: dict[str, tuple[str, str, int]] = {}
    for group in doc["groups"]:
        name = group.get("group", "?")
        for entry in group.get("entries", []):
            ips = [str(a) for a in (entry.get("ips") or []) if not str(a).startswith("{")]
            if not ips:
                continue
            for raw in entry.get("domains") or []:
                domain = raw.strip()
                if not domain or domain.startswith("!") or "*" in domain:
                    continue
                if domain not in seen:
                    seen[domain] = (name, entry.get("name", "?"), len(ips))

    buckets: dict[str, list[tuple[str, str, int]]] = defaultdict(list)
    for domain, (group, entry, count) in seen.items():
        buckets[group].append((domain, entry, count))

    order = {g: i for i, g in enumerate(GROUP_ORDER)}
    rows = []
    for group in sorted(buckets, key=lambda g: (order.get(g, 99), g)):
        # Widest address list first within a group: those are the entries with
        # the most ways to succeed.
        for domain, entry, count in sorted(buckets[group], key=lambda r: (-r[2], r[0])):
            rows.append((group, entry, domain, count))

    with open(out_path, "w", encoding="utf-8", newline="\n") as handle:
        for group, entry, domain, count in rows:
            handle.write(f"{group}\t{entry}\t{domain}\t{count}\n")

    per_group = defaultdict(int)
    for group, _, _, _ in rows:
        per_group[group] += 1
    print(f"build-sweep-list: {len(rows)} testable domains")
    for group in sorted(per_group, key=lambda g: (order.get(g, 99), g)):
        print(f"  {group:<24} {per_group[group]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
