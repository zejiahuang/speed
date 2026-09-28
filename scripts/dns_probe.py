#!/usr/bin/env python3
"""Ask one DNS server one question and print the A records it answers with.

The daemon smoke test uses this to observe what the running kernel believes, which
is the only way to see a rule reload take effect without watching a real
connection. It reuses the query builder and the parser from the smoke client, so
the two halves of the harness cannot disagree about the wire format.
"""

import argparse
import os
import socket
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tun_smoke_client import build_query, parse_answers  # noqa: E402  (path set above)

TRANSACTION_ID = 0x5741


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, help="address of the resolver to ask")
    parser.add_argument("--domain", required=True)
    parser.add_argument("--port", type=int, default=53)
    parser.add_argument("--timeout", type=float, default=5.0)
    args = parser.parse_args()

    query = build_query(args.domain, TRANSACTION_ID)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(args.timeout)
            sock.sendto(query, (args.server, args.port))
            data, peer = sock.recvfrom(4096)
    except OSError as err:
        print(f"DNS FAIL {args.domain} {err}", flush=True)
        return 1

    if peer[0] != args.server:
        print(f"DNS FAIL {args.domain} reply came from {peer[0]}", flush=True)
        return 1

    flags = struct.unpack(">H", data[2:4])[0]
    if not flags & 0x8000:
        print(f"DNS FAIL {args.domain} reply has no QR flag", flush=True)
        return 1
    if flags & 0x000F:
        print(f"DNS FAIL {args.domain} rcode={flags & 0x000F}", flush=True)
        return 1

    try:
        answers = parse_answers(data)
    except (IndexError, struct.error) as err:
        print(f"DNS FAIL {args.domain} unparsable reply: {err}", flush=True)
        return 1

    print(f"DNS ANSWERS {args.domain} {' '.join(answers) if answers else '(none)'}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
