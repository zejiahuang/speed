#!/usr/bin/env python3
"""Summarise a rule document: the groups it defines and how big each one is."""

import json
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "/mnt/d/4/tmp/rules.json"
with open(path, "r", encoding="utf-8") as handle:
    document = json.load(handle)

meta = document.get("meta", {})
print(f"version={meta.get('version')} update_time={meta.get('update_time')}")
filtered = document.get("filtered") or []
if filtered:
    print(f"filtered={len(filtered)}: {', '.join(map(str, filtered[:12]))}")

print()
print(f"{'group':<28} {'entries':>7} {'domains':>8} {'ips':>7} {'placeholders':>12}")
for group in document.get("groups", []):
    entries = group.get("entries", [])
    domains = sum(len(entry.get("domains") or []) for entry in entries)
    ips = sum(len(entry.get("ips") or []) for entry in entries)
    placeholders = sum(1 for entry in entries if entry.get("isPlaceholder"))
    print(
        f"{group.get('group', '?'):<28} {len(entries):>7} {domains:>8} {ips:>7} {placeholders:>12}"
    )
