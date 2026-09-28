#!/usr/bin/env python3
"""Compare a direct sweep against a tunnel sweep, domain by domain.

The interesting number is not "how many domains did the tunnel reach". A tunnel
that reaches 900 of 1281 domains sounds fine until you notice that 700 of them
were already reachable directly, and 600 that *were* reachable directly are no
longer. The comparison that matters is the 2x2:

    direct ok    -> tunnel ok       : no change
    direct ok    -> tunnel blocked  : the tunnel BROKE a working domain
    direct blocked -> tunnel ok     : the tunnel RESCUED a blocked domain
    direct blocked -> tunnel blocked: no change

Only the rescued count is a win and only the broken count is a loss, so the
headline is `rescued - broken`. Both sweeps must cover the same domain list and
must have run close together in time, which is why the sweep script interleaves
them rather than running one to completion and then the other.

Usage:
    python scripts/sweep-compare.py <direct.tsv> <tunnel.tsv> [--report out.md]
"""

from __future__ import annotations

import argparse
import sys
from collections import Counter
from pathlib import Path

# A rule entry with a handful of addresses has no ordering space, so one stale
# address is the whole domain. Below this many addresses a failure is expected
# to be the rule's fault rather than the kernel's, and the report says so.
LOW_DENSITY = 2


def load(path: Path) -> dict[str, tuple[str, float]]:
    """Read a sweep TSV into {domain: (code, seconds)}."""
    out: dict[str, tuple[str, float]] = {}
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 3:
                continue
            domain, code, seconds = parts[0], parts[1], parts[2]
            try:
                elapsed = float(seconds)
            except ValueError:
                continue
            out[domain] = (code, elapsed)
    return out


def blocked(code: str) -> bool:
    """`000` is curl's word for 'no response at all'.

    Every other code — including 403 and 404 — means the TLS handshake finished
    and an origin answered, which is reachability. Counting 4xx as failure makes
    every API endpoint look broken and buries the real signal.
    """
    return code == "000"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("direct", type=Path)
    parser.add_argument("tunnel", type=Path)
    parser.add_argument("--report", type=Path, default=None)
    parser.add_argument("--top", type=int, default=25)
    args = parser.parse_args()

    direct = load(args.direct)
    tunnel = load(args.tunnel)

    if not direct or not tunnel:
        print("sweep-compare: one of the inputs is empty", file=sys.stderr)
        return 1

    shared = sorted(set(direct) & set(tunnel))
    only_direct = set(direct) - set(tunnel)
    only_tunnel = set(tunnel) - set(direct)

    rows: list[tuple[str, str, str, float, float]] = []
    for domain in shared:
        dcode, dsec = direct[domain]
        tcode, tsec = tunnel[domain]
        rows.append((domain, dcode, tcode, dsec, tsec))

    matrix = Counter()
    for _, dcode, tcode, _, _ in rows:
        key = ("blocked" if blocked(dcode) else "ok",
               "blocked" if blocked(tcode) else "ok")
        matrix[key] += 1

    rescued = [(d, dc, tc, ds, ts) for d, dc, tc, ds, ts in rows
               if blocked(dc) and not blocked(tc)]
    broken = [(d, dc, tc, ds, ts) for d, dc, tc, ds, ts in rows
              if not blocked(dc) and blocked(tc)]
    stable_ok = matrix[("ok", "ok")]
    stable_bad = matrix[("blocked", "blocked")]

    # Slowdown among domains that worked both ways. The tunnel adds a hop, so a
    # small constant is expected; a domain that goes from 0.3s to 9s is not.
    both_ok = [(d, ds, ts) for d, dc, tc, ds, ts in rows
               if not blocked(dc) and not blocked(tc)]
    slowdowns = sorted(
        ((d, ds, ts, ts - ds) for d, ds, ts in both_ok if ts > ds + 1.0),
        key=lambda row: row[3], reverse=True,
    )

    lines: list[str] = []

    def emit(text: str = "") -> None:
        lines.append(text)
        print(text)

    emit("# Sweep comparison: direct vs tunnel")
    emit()
    emit(f"- direct sweep: `{args.direct}` ({len(direct)} domains)")
    emit(f"- tunnel sweep: `{args.tunnel}` ({len(tunnel)} domains)")
    emit(f"- compared (present in both): **{len(shared)}**")
    if only_direct:
        emit(f"- only in direct: {len(only_direct)}")
    if only_tunnel:
        emit(f"- only in tunnel: {len(only_tunnel)}")
    emit()

    emit("## The 2x2")
    emit()
    emit("| direct | tunnel | count | meaning |")
    emit("| --- | --- | ---: | --- |")
    emit(f"| ok | ok | {stable_ok} | unchanged |")
    emit(f"| ok | **blocked** | **{len(broken)}** | **tunnel broke a working domain** |")
    emit(f"| **blocked** | ok | **{len(rescued)}** | **tunnel rescued a blocked domain** |")
    emit(f"| blocked | blocked | {stable_bad} | unchanged |")
    emit()
    net = len(rescued) - len(broken)
    verdict = "a net loss" if net < 0 else ("a net gain" if net > 0 else "neutral")
    emit(f"**Net: {net:+d}** — {verdict} on this traffic set.")
    emit()

    if broken:
        emit(f"## Working directly, blocked through the tunnel ({len(broken)})")
        emit()
        emit("Each of these is a regression the tunnel introduces. The time column")
        emit("is the tunnel's own, and a value near the cap means the kernel spent")
        emit("its whole budget before giving up.")
        emit()
        emit("| domain | direct | tunnel | direct s | tunnel s |")
        emit("| --- | --- | --- | ---: | ---: |")
        for domain, dcode, tcode, dsec, tsec in sorted(
                broken, key=lambda r: r[4], reverse=True)[: args.top]:
            emit(f"| {domain} | {dcode} | {tcode} | {dsec:.2f} | {tsec:.2f} |")
        if len(broken) > args.top:
            emit(f"| … | | | | _{len(broken) - args.top} more_ |")
        emit()

    if rescued:
        emit(f"## Blocked directly, reached through the tunnel ({len(rescued)})")
        emit()
        emit("This is the set the tunnel exists for.")
        emit()
        emit("| domain | direct | tunnel | tunnel s |")
        emit("| --- | --- | --- | ---: |")
        for domain, dcode, tcode, _dsec, tsec in sorted(
                rescued, key=lambda r: r[4])[: args.top]:
            emit(f"| {domain} | {dcode} | {tcode} | {tsec:.2f} |")
        if len(rescued) > args.top:
            emit(f"| … | | | _{len(rescued) - args.top} more_ |")
        emit()

    if slowdowns:
        emit(f"## Working both ways, but slower through the tunnel (>1s worse)")
        emit()
        emit("| domain | direct s | tunnel s | delta |")
        emit("| --- | ---: | ---: | ---: |")
        for domain, dsec, tsec, delta in slowdowns[: args.top]:
            emit(f"| {domain} | {dsec:.2f} | {tsec:.2f} | +{delta:.2f} |")
        emit()

    emit("## Reading this")
    emit()
    emit("- A domain that works directly and fails through the tunnel is the most")
    emit("  actionable row. The tunnel is not failing to reach it; it is choosing")
    emit("  an address that does not work, and the direct path is choosing one that")
    emit("  does. That asymmetry is a rule-quality problem, not a routing problem.")
    emit("- The tunnel's own time near the request cap on those rows is the tell:")
    emit("  the kernel tried its candidates, ran out of budget, and reported")
    emit("  nothing. A rule with one address has no second choice to fall back to.")
    emit("- `403` and `404` count as reachable throughout. They are answers.")

    if args.report:
        args.report.write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"\nsweep-compare: wrote {args.report}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
