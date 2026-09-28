#!/usr/bin/env python3
"""Join the tunnel and direct sweeps and print a per-group verdict.

Usage: sweep-report.py --list L --tunnel T --direct D --mode M --out O

The classification is the part worth reading, because "did it work" is three
questions and only one of them is the status code:

  OK        2xx/3xx — the server answered and liked the request.
  ANSWERED  4xx/5xx — the server answered. A 403 from Cloudflare or a 404 from
            a path this script guessed is *not* a connectivity failure: TLS
            completed, the origin replied, the kernel carried it. Counting
            these as failures would make every API endpoint in the rules look
            broken, which is exactly the noise that hides a real signal.
  BLOCKED   000 — no answer at all. This is the only real failure.
  TLSErr    a certificate or handshake error: the address was reachable but
            serves the wrong name. Distinct from BLOCKED because it is a
            property of the *rule's address choice*, and the fix is a different
            one.

The headline number is the BLOCKED rate, and it is reported per group rather
than as one figure, because the groups are not the same kind of thing: `For Web`
is what someone opens a browser for, while `In Game` endpoints are largely
undocumented and expected to refuse a bare GET.
"""

import argparse
import sys
from collections import defaultdict


def read_sweep(path):
    """Domain -> (code, seconds, exitcode). Missing file reads as empty."""
    out = {}
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.rstrip("\n")
                if not line:
                    continue
                parts = line.split("\t")
                if len(parts) < 2:
                    continue
                domain = parts[0].strip()
                code = parts[1].strip() or "000"
                try:
                    secs = float(parts[2]) if len(parts) > 2 and parts[2] else 0.0
                except ValueError:
                    secs = 0.0
                exitcode = parts[3].strip() if len(parts) > 3 else ""
                if domain:
                    out[domain] = (code, secs, exitcode)
    except FileNotFoundError:
        pass
    return out


# curl's exit codes, and why each is a *different kind* of failure.
#
# Collapsing them into `000` is the single most misleading thing this report used
# to do. `000` means "curl printed no HTTP status", which is true both when
# nothing answered and when something answered with a certificate the client
# refused — and those are opposite findings. In the 1.0.54 rules the second was
# the larger population, so a report that called it all "BLOCKED" overstated the
# tunnel's damage by an order of magnitude.
CURL_CERT = "60"      # TLS certificate rejected: the address WAS reached
CURL_TIMEOUT = "28"   # no usable answer within the budget
CURL_REFUSED = "7"    # TCP connected, host refused
CURL_RESOLVE = "6"    # name could not be resolved locally
CURL_CONNECT = "35"   # TLS handshake failed for a reason other than the cert


def classify(code, exitcode=""):
    if code != "000":
        if code.startswith("2") or code.startswith("3"):
            return "OK"
        if code.startswith("4") or code.startswith("5"):
            return "ANSWERED"
        return "OTHER"

    # `000`: fall back to why curl gave up. A certificate rejection means bytes
    # flowed both directions and the session ended on the client's decision,
    # which is reachability in every sense that matters for "is this domain
    # broken for the user" — the user's browser would show a certificate warning,
    # not a connection error.
    if exitcode == CURL_CERT:
        return "CERT"
    if exitcode == CURL_TIMEOUT:
        return "BLOCKED"
    if exitcode == CURL_REFUSED:
        return "REFUSED"
    return "BLOCKED"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", required=True)
    ap.add_argument("--tunnel", required=True)
    ap.add_argument("--direct", required=True)
    ap.add_argument("--mode", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rows = []
    with open(args.list, encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 4:
                continue
            rows.append((parts[0], parts[1], parts[2], parts[3]))

    tunnel = read_sweep(args.tunnel)
    direct = read_sweep(args.direct)

    per_group = defaultdict(lambda: defaultdict(int))
    listed = []
    for group, entry, domain, ipcount in rows:
        t_code, t_secs, t_exit = tunnel.get(domain, ("", 0.0, ""))
        d_code, d_secs, d_exit = direct.get(domain, ("", 0.0, ""))
        t_class = classify(t_code, t_exit) if t_code else "UNKNOWN"
        d_class = classify(d_code, d_exit) if d_code else "UNKNOWN"

        listed.append((group, entry, domain, ipcount, t_code, t_class, t_secs,
                       d_code, d_class, d_secs, t_exit))
        per_group[group]["total"] += 1
        per_group[group]["t_" + t_class] += 1
        per_group[group]["d_" + d_class] += 1
        if t_class == "BLOCKED" and d_class == "BLOCKED":
            per_group[group]["both_blocked"] += 1
        if t_class in ("OK", "ANSWERED") and d_class == "BLOCKED":
            per_group[group]["tunnel_only"] += 1
        if t_class == "BLOCKED" and d_class in ("OK", "ANSWERED", "CERT"):
            per_group[group]["direct_only"] += 1

    with open(args.out, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("group\tentry\tdomain\tips\ttunnel_code\ttunnel_class\t"
                     "tunnel_s\tdirect_code\tdirect_class\tdirect_s\ttunnel_exit\n")
        for row in listed:
            handle.write("\t".join(str(v) for v in row) + "\n")

    def band(count, total):
        rate = count / total if total else 0
        if rate <= 0.10:
            return "good"
        if rate <= 0.35:
            return "mixed"
        return "poor"

    print()
    print("=" * 78)
    print(f"sweep report  (mode={args.mode})")
    print("=" * 78)
    print(f"{'group':<22}{'n':>5}{'tun ok':>9}{'tun ans':>9}{'tun BLK':>9}"
          f"{'tun CERT':>10}{'dir BLK':>9}{'tun-only':>10}{'dir-only':>10}")
    print("-" * 88)

    grand = defaultdict(int)
    for group in sorted(per_group):
        s = per_group[group]
        total = s["total"]
        reachable = s["t_OK"] + s["t_ANSWERED"] + s["t_CERT"]
        pct = 100 * reachable / total if total else 0
        print(f"{group[:21]:<22}{total:>5}{s['t_OK']:>9}{s['t_ANSWERED']:>9}"
              f"{s['t_BLOCKED']:>9}{s['t_CERT']:>10}{s['d_BLOCKED']:>9}"
              f"{s['tunnel_only']:>10}{s['direct_only']:>10}"
              f"   {pct:5.1f}% reachable via tunnel  [{band(s['t_BLOCKED'], total)}]")
        for key, value in s.items():
            grand[key] += value

    print("-" * 88)
    total = grand["total"]
    reachable = grand["t_OK"] + grand["t_ANSWERED"] + grand["t_CERT"]
    print(f"{'TOTAL':<22}{total:>5}{grand['t_OK']:>9}{grand['t_ANSWERED']:>9}"
          f"{grand['t_BLOCKED']:>9}{grand['t_CERT']:>10}{grand['d_BLOCKED']:>9}"
          f"{grand['tunnel_only']:>10}{grand['direct_only']:>10}")
    if total:
        print()
        print(f"  tunnel reached      {reachable:>5} / {total}  = {100*reachable/total:.1f}%"
              f"   (OK {grand['t_OK']} + ANSWERED {grand['t_ANSWERED']}"
              f" + CERT {grand['t_CERT']})")
        print(f"  tunnel blocked      {grand['t_BLOCKED']:>5} / {total}"
              f"  = {100*grand['t_BLOCKED']/total:.1f}%   (000, no answer)")
        print(f"  tunnel cert-rejected {grand['t_CERT']:>4} / {total}"
              f"  = {100*grand['t_CERT']/total:.1f}%   (000, exit 60)")
        print(f"  direct blocked      {grand['d_BLOCKED']:>5} / {total}"
              f"  = {100*grand['d_BLOCKED']/total:.1f}%")
        print()
        print("  A certificate rejection is NOT a block. The address answered and")
        print("  the TLS handshake ran; the client refused the certificate because")
        print("  it does not cover the name. That is the boundary watt cannot cross")
        print("  without terminating TLS, and it is a different finding from silence.")
        print()
        print(f"  reached via tunnel only : {grand['tunnel_only']}")
        print(f"  reached direct only     : {grand['direct_only']}")
        print(f"  blocked both ways       : {grand['both_blocked']}")

    # The rows worth a human's attention, longest address lists first: a host
    # the rules support well and that still cannot be reached is the finding.
    print()
    print("--- blocked through the tunnel, widest address lists first ---")
    blocked = [r for r in listed if r[5] == "BLOCKED"]
    blocked.sort(key=lambda r: -int(r[3] or 0))
    for row in blocked[:40]:
        group, entry, domain, ipcount, t_code, t_class, t_secs, d_code, d_class, d_secs, t_exit = row
        note = "also blocked direct" if d_class == "BLOCKED" else f"direct={d_code}"
        print(f"  {domain:<46} ips={ipcount:>4}  exit={t_exit:<3} {note}")
    if len(blocked) > 40:
        print(f"  ... and {len(blocked) - 40} more (see {args.out})")

    print()
    print(f"app-sweep: results written to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
