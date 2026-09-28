#!/usr/bin/env python3
"""Compare every concrete-placeholder domain through direct access and CONNECT.

The rule document's `{Cloudflare}` / `{Cloudfront}` entries are not concrete IP
rules. They are still legitimate allow-list entries: the proxy resolves them
normally, then connects without decrypting TLS. This test asks every such domain
both ways and keeps only the cases where direct access fails but CONNECT works.

Usage:
    python3 scripts/probe-placeholder-rescue.py --proxy 127.0.0.1:18080

The output is a TSV with one row per placeholder domain. `RESCUED` is the
interesting verdict: the local machine failed while the Android CONNECT path
completed a real TLS handshake.
"""

from __future__ import annotations

import argparse
import csv
import json
import pathlib
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed


def placeholder_domains(rules_path: pathlib.Path):
    doc = json.loads(rules_path.read_text(encoding="utf-8"))
    rows = {}
    for group in doc["groups"]:
        for entry in group["entries"]:
            ips = entry.get("ips") or []
            tokens = [ip for ip in ips if ip.startswith("{")]
            if not tokens:
                continue
            for domain in entry.get("domains") or []:
                rows[domain.lower()] = {
                    "domain": domain.lower(),
                    "group": group["group"],
                    "entry": entry["name"],
                    "placeholder": ",".join(tokens),
                    "port": str(entry.get("port") or "443"),
                }
    return list(rows.values())


def curl(domain: str, proxy: str | None, timeout: int, program: str):
    command = [
        program,
        "--silent",
        "--show-error",
        "--noproxy",
        "",
        "--max-time",
        str(timeout),
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}\\t%{size_download}\\t%{ssl_verify_result}\\t%{time_total}\\t%{remote_ip}",
    ]
    if proxy:
        command += ["-x", proxy]
    command.append(f"https://{domain}/")
    try:
        proc = subprocess.run(command, capture_output=True, text=True, timeout=timeout + 3)
        fields = proc.stdout.strip().split("\t")
        if len(fields) != 5:
            return {"http": "000", "bytes": "0", "ssl": "0", "time": "", "ip": "", "exit": str(proc.returncode), "error": proc.stderr.strip()[:160]}
        return {"http": fields[0], "bytes": fields[1], "ssl": fields[2], "time": fields[3], "ip": fields[4], "exit": str(proc.returncode), "error": ""}
    except Exception as err:  # one dead domain must not stop the batch
        return {"http": "000", "bytes": "0", "ssl": "0", "time": "", "ip": "", "exit": "-1", "error": str(err)[:160]}


def usable(result):
    try:
        return result["exit"] == "0" and int(result["http"]) != 0 and result["ssl"] == "0"
    except (KeyError, ValueError):
        return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rules", default="tmp/rules.json")
    ap.add_argument("--proxy", required=True)
    ap.add_argument("--out", default="tmp/placeholder-rescue.tsv")
    ap.add_argument("--workers", type=int, default=32)
    ap.add_argument("--timeout", type=int, default=10)
    ap.add_argument("--curl", default="curl", help="curl executable; use curl.exe for a Windows adb-forward proxy")
    args = ap.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    rules = root / args.rules
    out = root / args.out
    entries = placeholder_domains(rules)
    print(f"placeholder-rescue: {len(entries)} unique placeholder domains", flush=True)

    def one(entry):
        direct = curl(entry["domain"], None, args.timeout, args.curl)
        proxied = curl(entry["domain"], args.proxy, args.timeout, args.curl)
        if usable(proxied) and not usable(direct):
            verdict = "RESCUED"
        elif usable(proxied):
            verdict = "BOTH_OK"
        elif usable(direct):
            verdict = "PROXY_FAILED"
        else:
            verdict = "BOTH_FAILED"
        return {**entry, "direct": direct, "proxy": proxied, "verdict": verdict}

    results = []
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = [pool.submit(one, entry) for entry in entries]
        for index, future in enumerate(as_completed(futures), 1):
            results.append(future.result())
            if index % 100 == 0:
                print(f"placeholder-rescue: {index}/{len(futures)}", flush=True)

    results.sort(key=lambda row: row["domain"])
    fields = [
        "domain", "group", "entry", "placeholder", "port",
        "direct_http", "direct_bytes", "direct_ssl", "direct_time", "direct_ip", "direct_exit",
        "proxy_http", "proxy_bytes", "proxy_ssl", "proxy_time", "proxy_ip", "proxy_exit",
        "verdict",
    ]
    with out.open("w", encoding="utf-8", newline="\n") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t")
        writer.writeheader()
        for row in results:
            writer.writerow({
                "domain": row["domain"], "group": row["group"], "entry": row["entry"],
                "placeholder": row["placeholder"], "port": row["port"],
                "direct_http": row["direct"]["http"], "direct_bytes": row["direct"]["bytes"],
                "direct_ssl": row["direct"]["ssl"], "direct_time": row["direct"]["time"],
                "direct_ip": row["direct"]["ip"], "direct_exit": row["direct"]["exit"],
                "proxy_http": row["proxy"]["http"], "proxy_bytes": row["proxy"]["bytes"],
                "proxy_ssl": row["proxy"]["ssl"], "proxy_time": row["proxy"]["time"],
                "proxy_ip": row["proxy"]["ip"], "proxy_exit": row["proxy"]["exit"],
                "verdict": row["verdict"],
            })

    from collections import Counter
    counts = Counter(row["verdict"] for row in results)
    print(f"placeholder-rescue: wrote {out}")
    for verdict in ("RESCUED", "BOTH_OK", "PROXY_FAILED", "BOTH_FAILED"):
        print(f"  {verdict:<13} {counts[verdict]}")

    print("placeholder-rescue: rescued examples")
    for row in results:
        if row["verdict"] == "RESCUED":
            print(f"  {row['domain']} direct={row['direct']['http']} proxy={row['proxy']['http']} proxy_ip={row['proxy']['ip']}")


if __name__ == "__main__":
    main()
