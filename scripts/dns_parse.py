#!/usr/bin/env python3
"""Parse a raw DNS reply captured from the wire and print what it answered.

The Android end-to-end test needs this because the device has no `dig`, and its
`curl` was built without `--dns-servers`. So the query is built on the host,
sent from the device with netcat, and the reply is pulled back to be read here.

    python3 scripts/dns_parse.py reply.bin [reply.bin ...]

Output: "<file> id=<hex> qr=<0|1> tc=<0|1> rcode=<n> answers=<n> <addr> ..."
"""

import socket
import struct
import sys


def read_name(buf, offset):
    """Read a possibly compressed name, returning (name, offset_after).

    Raises ValueError on a truncated or malformed message rather than reading
    past the end. The caller feeds this whatever came back off the wire, and a
    short read is a normal outcome — a reply cut off mid-name used to surface as
    an IndexError traceback, which says nothing about what actually happened.
    """
    parts = []
    cursor = offset
    seen = set()
    while True:
        if cursor >= len(buf):
            raise ValueError("name runs past the end of the message")
        length = buf[cursor]
        if length & 0xC0 == 0:
            if length == 0:
                cursor += 1
                break
            end = cursor + 1 + length
            if end > len(buf):
                raise ValueError("name runs past the end of the message")
            parts.append(buf[cursor + 1:end].decode("ascii", "replace"))
            cursor = end
        else:
            if cursor + 1 >= len(buf):
                raise ValueError("compression pointer runs past the end")
            second = buf[cursor + 1]
            target = ((length & 0x3F) << 8) | second
            if target in seen or target >= cursor:
                return ".".join(parts), cursor + 2
            seen.add(target)
            cursor = target
    return ".".join(parts), cursor


def parse(buf):
    if len(buf) < 12:
        return None
    ident, flags, qd, an, ns, ar = struct.unpack("!HHHHHH", buf[:12])
    cursor = 12
    addresses = []
    try:
        for _ in range(qd):
            _, cursor = read_name(buf, cursor)
            cursor += 4
        for _ in range(an):
            _, cursor = read_name(buf, cursor)
            if cursor + 10 > len(buf):
                raise ValueError("record header runs past the end")
            rtype, rclass, _ttl, rdlen = struct.unpack("!HHIH", buf[cursor:cursor + 10])
            cursor += 10
            data = buf[cursor:cursor + rdlen]
            cursor += rdlen
            if rtype == 1 and rdlen == 4 and len(data) == 4:
                addresses.append(socket.inet_ntoa(data))
            elif rtype == 28 and rdlen == 16 and len(data) == 16:
                addresses.append(socket.inet_ntop(socket.AF_INET6, data))
    except (ValueError, struct.error) as err:
        # A truncated capture is a normal outcome, not a crash. Report what was
        # recovered and say the rest was cut off.
        return {
            "id": ident,
            "qr": (flags >> 15) & 1,
            "tc": (flags >> 9) & 1,
            "rcode": flags & 0xF,
            "ancount": an,
            "addresses": addresses,
            "truncated": str(err),
            "captured_bytes": len(buf),
        }
    return {
        "id": ident,
        "qr": (flags >> 15) & 1,
        "tc": (flags >> 9) & 1,
        "rcode": flags & 0xF,
        "ancount": an,
        "addresses": addresses,
    }


def main():
    if len(sys.argv) < 2:
        print(__doc__.strip())
        return 2
    for path in sys.argv[1:]:
        with open(path, "rb") as handle:
            buf = handle.read()
        parsed = parse(buf)
        if parsed is None:
            print(f"{path} EMPTY_OR_SHORT bytes={len(buf)}")
            continue
        addrs = " ".join(parsed["addresses"][:8])
        if len(parsed["addresses"]) > 8:
            addrs += f" (+{len(parsed['addresses']) - 8} more)"
        print(f"{path} id=0x{parsed['id']:04x} qr={parsed['qr']} tc={parsed['tc']} "
              f"rcode={parsed['rcode']} answers={parsed['ancount']} {addrs}")


if __name__ == "__main__":
    sys.exit(main())
