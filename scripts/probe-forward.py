#!/usr/bin/env python3
"""Test, domain by domain, whether the rule set's forwarding actually works.

Matching and forwarding are different questions, and a rule set can answer yes
to the first and no to the second. `watt-daemon --probe` answers the first: it
says which rule a domain hits. This script answers the second: it dials the
address the rule supplies and tries to complete a real handshake.

Every domain also gets a control connection to whatever the system resolver
returns for it. Without that control, a failure is ambiguous: a rule address
that refuses connections might be a wrong rule, or it might be a host this
network cannot reach at all. Comparing the two is what separates the two cases.

    python3 scripts/probe-forward.py [-o out.tsv] [--limit N] [--workers N]

Input:  tmp/rules.json  and  tmp/probe-all-domains.tsv
Output: one row per domain:
    domain, group, entry, port, rule_ip, tcp, tls, cert_ok, ms, direct_ip,
    direct_tcp, direct_tls, direct_ms, verdict, error
"""

import argparse
import csv
import json
import socket
import ssl
import sys
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor

REPO = str(__import__("pathlib").Path(__file__).resolve().parent.parent)


def load_rules(path):
    doc = json.load(open(path, encoding="utf-8"))
    by_domain = {}
    for group in doc["groups"]:
        for entry in group["entries"]:
            ips = [a for a in (entry.get("ips") or []) if not a.startswith("{")]
            for name in entry.get("domains") or []:
                name = name.strip().lower()
                if not name:
                    continue
                by_domain[name] = {
                    "group": group["group"],
                    "entry": entry["name"],
                    "port": int(entry.get("port") or 443),
                    "ips": ips,
                    "cert": entry.get("cert") or "",
                }
    return by_domain


def tls_handshake(host, addr, port, timeout):
    """Return (tcp_ok, tls_ok, cert_ok, ms, error)."""
    started = time.monotonic()
    ctx = ssl.create_default_context()
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED
    fam = socket.AF_INET6 if ":" in addr else socket.AF_INET
    try:
        with socket.create_connection((addr, port), timeout=timeout) as sock:
            tcp_ms = (time.monotonic() - started) * 1000
            with ctx.wrap_socket(sock, server_hostname=host) as tls:
                tls.getpeercert()  # forces the handshake to complete
                ms = (time.monotonic() - started) * 1000
                return True, True, True, round(ms, 1), "", round(tcp_ms, 1)
    except ssl.SSLCertVerificationError as err:
        ms = (time.monotonic() - started) * 1000
        return True, False, False, round(ms, 1), f"cert: {err.verify_message}", ""
    except ssl.SSLError as err:
        ms = (time.monotonic() - started) * 1000
        return True, False, False, round(ms, 1), f"tls: {err.reason}", ""
    except OSError as err:
        ms = (time.monotonic() - started) * 1000
        return False, False, False, round(ms, 1), f"tcp: {err.strerror or err}", ""
    except Exception as err:  # noqa: BLE001 - one bad host must not kill the run
        ms = (time.monotonic() - started) * 1000
        return False, False, False, round(ms, 1), f"{type(err).__name__}: {err}", ""


def http_get(host, addr, port, timeout):
    """Return (tcp_ok, http_ok, cert_ok, ms, error) for a plain-HTTP rule."""
    started = time.monotonic()
    fam = socket.AF_INET6 if ":" in addr else socket.AF_INET
    try:
        with socket.create_connection((addr, port), timeout=timeout) as sock:
            sock.sendall(
                f"GET / HTTP/1.1\r\nHost: {host}\r\n"
                f"User-Agent: watt-forward-probe\r\nConnection: close\r\n\r\n".encode()
            )
            sock.settimeout(timeout)
            head = sock.recv(64)
            ms = (time.monotonic() - started) * 1000
            ok = head[:4] == b"HTTP"
            if not ok:
                return True, False, None, round(ms, 1), f"no status line: {head[:24]!r}", ""
            return True, True, None, round(ms, 1), "", round(ms, 1)
    except OSError as err:
        ms = (time.monotonic() - started) * 1000
        return False, False, None, round(ms, 1), f"tcp: {err.strerror or err}", ""
    except Exception as err:  # noqa: BLE001
        ms = (time.monotonic() - started) * 1000
        return False, False, None, round(ms, 1), f"{type(err).__name__}: {err}", ""


def resolve(host):
    try:
        infos = socket.getaddrinfo(host, None, proto=socket.IPPROTO_TCP)
    except OSError:
        return None
    for info in infos:
        if info[0] in (socket.AF_INET, socket.AF_INET6):
            return info[4][0]
    return None


def probe(domain, meta, timeout, max_ips):
    port = meta["port"]
    use_tls = port == 443
    row = {
        "domain": domain, "group": meta["group"], "entry": meta["entry"],
        "port": port, "cert": meta["cert"],
    }

    # The rule's own addresses, in the order the rule lists them. Stopping at
    # the first that answers keeps this cheap; a rule whose early addresses are
    # all dead but whose later ones work is still reported as a failure, which
    # is the honest answer for a client that also stops early.
    rule_ip, best = "", None
    errors = []
    for addr in meta["ips"][:max_ips]:
        if use_tls:
            tcp, app, cert, ms, err, _ = tls_handshake(domain, addr, port, timeout)
        else:
            tcp, app, cert, ms, err, _ = http_get(domain, addr, port, timeout)
        errors.append(f"{addr}: {err}" if err else "")
        if tcp and app:
            rule_ip, best = addr, (tcp, app, cert, ms, err)
            break
        if best is None or (tcp and not best[0]):
            best = (tcp, app, cert, ms, err)
            rule_ip = addr
    if best is None:
        best = (False, False, False, 0.0, "no addresses")
    row.update(rule_ip=rule_ip, tcp=best[0], app=best[1], cert_ok=best[2],
               ms=best[3], error="; ".join(e for e in errors if e)[:200])

    # Control: the same host reached through the system resolver.
    direct = resolve(domain)
    row["direct_ip"] = direct or ""
    if direct:
        if use_tls:
            d = tls_handshake(domain, direct, port, timeout)
        else:
            d = http_get(domain, direct, port, timeout)
        row.update(direct_tcp=d[0], direct_app=d[1], direct_ms=d[3])
    else:
        row.update(direct_tcp=False, direct_app=False, direct_ms=0.0)

    if row["app"]:
        row["verdict"] = "FORWARDED"
    elif row["direct_app"]:
        row["verdict"] = "RULE_IP_DEAD"  # reachable directly, not at the rule's IP
    elif row["tcp"]:
        row["verdict"] = "RULE_IP_REFUSED_TLS"
    else:
        row["verdict"] = "UNREACHABLE_BOTH"
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default=f"{REPO}/tmp/probe-forward.tsv")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--workers", type=int, default=48)
    ap.add_argument("--timeout", type=float, default=5.0)
    ap.add_argument("--max-ips", type=int, default=3)
    ap.add_argument("--group", default="")
    args = ap.parse_args()

    rules = load_rules(f"{REPO}/tmp/rules.json")
    probe_rows = list(csv.DictReader(
        open(f"{REPO}/tmp/probe-all-domains.tsv", encoding="utf-8"), delimiter="\t"))

    # Only domains the router actually redirects are worth dialling: a
    # placeholder rule sends the client to its own resolver by design, so there
    # is no rule address to test.
    targets = [r["domain"] for r in probe_rows if r["strategy"] == "rule-addresses"]
    if args.group:
        targets = [d for d in targets
                   if rules.get(d, {}).get("group", "").lower().startswith(args.group.lower())]
    if args.limit:
        targets = targets[: args.limit]

    print(f"probe-forward: dialling {len(targets)} domains "
          f"({args.workers} workers, {args.timeout}s timeout)", flush=True)

    rows = []
    done = 0
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {pool.submit(probe, d, rules[d], args.timeout, args.max_ips): d
                   for d in targets if d in rules}
        for fut in futures:
            try:
                rows.append(fut.result())
            except Exception as err:  # noqa: BLE001
                rows.append({"domain": futures[fut], "verdict": "ERROR",
                             "error": f"{type(err).__name__}: {err}"})
            done += 1
            if done % 100 == 0:
                print(f"probe-forward: {done}/{len(futures)}", flush=True)

    rows.sort(key=lambda r: r.get("domain", ""))
    fields = ["domain", "group", "entry", "port", "rule_ip", "tcp", "app",
              "cert_ok", "ms", "direct_ip", "direct_tcp", "direct_app",
              "direct_ms", "verdict", "error"]
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        w = csv.DictWriter(f, fieldnames=fields, delimiter="\t", extrasaction="ignore")
        w.writeheader()
        for r in rows:
            w.writerow(r)

    print()
    counts = Counter(r["verdict"] for r in rows)
    total = len(rows)
    print(f"probe-forward: {total} domains")
    for k in ("FORWARDED", "RULE_IP_DEAD", "RULE_IP_REFUSED_TLS", "UNREACHABLE_BOTH", "ERROR"):
        v = counts.get(k, 0)
        print(f"  {k:<22} {v:>5}  ({v * 100 / max(total, 1):.1f}%)")
    print(f"probe-forward: wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
