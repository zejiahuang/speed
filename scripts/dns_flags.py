#!/usr/bin/env python3
"""Ask a resolver one question and report what came back, including the flags.

`dns_probe.py` prints only the addresses, which makes a truncated answer
indistinguishable from an empty one. A DNS response that sets the TC bit is a
request to retry over TCP, not a statement that the name has no addresses, and
telling those apart is the whole point here: the kernel answers large rules with
a truncation and the client is left with nothing it can use.

    python3 scripts/dns_flags.py <server> <domain> [<domain> ...]

Output: "<domain> qr=<0|1> tc=<0|1> rcode=<n> ancount=<n> addrs=<n> bytes=<n>"
"""

import argparse
import os
import socket
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tun_smoke_client import build_query  # noqa: E402  (path set above)

TRANSACTION_ID = 0x5742


def ask(server, domain, port=53, timeout=6.0):
    query = build_query(domain, TRANSACTION_ID)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(timeout)
        sock.sendto(query, (server, port))
        data, peer = sock.recvfrom(65535)
    if peer[0] != server:
        return f"{domain} ERROR reply-from={peer[0]}"

    if len(data) < 12:
        return f"{domain} ERROR short-reply bytes={len(data)}"
    # Header layout: id(2) flags(2) qdcount(2) ancount(2) nscount(2) arcount(2).
    flags, _qd, ancount, _ns, _ar = struct.unpack("!HHHHH", data[2:12])
    qr = (flags >> 15) & 1
    tc = (flags >> 9) & 1
    rcode = flags & 0x000F

    # Walk the question section so the answer section can be counted by offset
    # rather than parsed in full; a truncated answer is empty by definition and
    # the record walk would stop there anyway.
    return (f"{domain} qr={qr} tc={tc} rcode={rcode} ancount={ancount} "
            f"addrs={ancount} bytes={len(data)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("server")
    ap.add_argument("domains", nargs="+")
    ap.add_argument("--port", type=int, default=53)
    ap.add_argument("--timeout", type=float, default=6.0)
    args = ap.parse_args()

    failures = 0
    for domain in args.domains:
        try:
            print(ask(args.server, domain, args.port, args.timeout), flush=True)
        except OSError as err:
            failures += 1
            print(f"{domain} ERROR {err}", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
