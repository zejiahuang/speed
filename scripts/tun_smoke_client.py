#!/usr/bin/env python3
"""Client half of the TUN test harness.

Every socket here goes through the tunnel without knowing it. The shell script
has routed TEST-NET-3 (RFC 5737) into the TUN device, so an ordinary ``connect``
to 203.0.113.10 is carried by the kernel's own route table into the engine,
relayed by the userspace stack, and answered by a loopback server.

Five legs, each proving something different:

* ``dns``   — a rule owned domain is answered from the rule set, not the network
* ``tcp``   — several parallel connections, repeated in waves; what a browser does
* ``udp``   — the NAT table forwards, re-addresses the reply, and scales
* ``bulk``  — a transfer big enough to exercise backpressure, verified byte for byte
* ``icmp``  — the kernel counts what it cannot relay instead of dropping it

The TCP, UDP and bulk legs run concurrently, because a kernel that only holds up
under one kind of load at a time has not been tested.
"""

import argparse
import os
import socket
import struct
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tun_smoke_servers import pattern  # noqa: E402  (path set above)

# The transaction id the DNS leg uses, so a stray answer cannot be mistaken for
# the reply to this query.
DNS_TRANSACTION_ID = 0x5741
# The echo identifier the ICMP leg uses.
ICMP_IDENTIFIER = 0x7A57
# How long to wait for a reply the kernel should never send.
ICMP_TIMEOUT = 1.5
# The budget for the whole UDP leg, not for each socket in it.
#
# A per-socket timeout multiplies by the number of flows: a client that opened two
# hundred sockets against a kernel that was dropping all of them sat here for
# fifty minutes, which is not a failure signal, it is a hang. The budget has to
# cover the leg so a broken kernel produces a verdict instead of a stall.
UDP_BUDGET = 30.0


def skip_name(data, offset):
    """Return the offset just past a DNS name, following the compression form."""
    while True:
        length = data[offset]
        if length == 0:
            return offset + 1
        if length & 0xC0 == 0xC0:
            return offset + 2
        offset += 1 + length


def build_query(name, qid):
    header = struct.pack(">HHHHHH", qid, 0x0100, 1, 0, 0, 0)
    question = b"".join(
        bytes([len(label)]) + label.encode("ascii") for label in name.split(".")
    )
    return header + question + b"\x00" + struct.pack(">HH", 1, 1)


def parse_answers(data):
    """Return the A records in a DNS response."""
    qdcount, ancount = struct.unpack(">HH", data[4:8])
    offset = 12
    for _ in range(qdcount):
        offset = skip_name(data, offset) + 4

    answers = []
    for _ in range(ancount):
        offset = skip_name(data, offset)
        rtype, _rclass, _ttl, rdlen = struct.unpack(">HHIH", data[offset : offset + 10])
        offset += 10
        rdata = data[offset : offset + rdlen]
        offset += rdlen
        if rtype == 1 and rdlen == 4:
            answers.append(socket.inet_ntoa(rdata))
    return answers


def check_dns(target, domain, expected, results):
    query = build_query(domain, DNS_TRANSACTION_ID)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(5)
            sock.sendto(query, (target, 53))
            data, peer = sock.recvfrom(4096)
    except OSError as err:
        results.append(("FAIL", "dns answered locally", str(err)))
        return

    if peer[0] != target or peer[1] != 53:
        results.append(("FAIL", "dns answered locally", f"reply came from {peer}"))
        return
    if struct.unpack(">H", data[:2])[0] != DNS_TRANSACTION_ID:
        results.append(("FAIL", "dns answered locally", "transaction id mismatch"))
        return
    flags = struct.unpack(">H", data[2:4])[0]
    if not flags & 0x8000:
        results.append(("FAIL", "dns answered locally", "reply has no QR flag"))
        return
    if flags & 0x000F:
        results.append(("FAIL", "dns answered locally", "non-zero rcode"))
        return

    try:
        answers = parse_answers(data)
    except (IndexError, struct.error) as err:
        results.append(("FAIL", "dns answered locally", f"unparsable reply: {err}"))
        return

    missing = expected - set(answers)
    if missing:
        results.append(
            ("FAIL", "dns answered locally", f"answers={answers} missing={sorted(missing)}")
        )
    else:
        results.append(
            ("PASS", "dns answered locally", f"{domain} -> {answers} from {peer[0]}:{peer[1]}")
        )


def one_connection(target, port, barrier, outcomes, index):
    try:
        # All connections in a wave are released together, so the engine really
        # does see them arrive at the same moment.
        barrier.wait(timeout=10)
        with socket.create_connection((target, port), timeout=10) as sock:
            sock.settimeout(10)
            sock.sendall(b"PING\n")
            data = b""
            while not data.endswith(b"\n"):
                chunk = sock.recv(4096)
                if not chunk:
                    break
                data += chunk
        outcomes[index] = data.strip().decode(errors="replace")
    except Exception as err:  # the message itself is the result
        outcomes[index] = f"error: {err}"


def check_tcp(target, port, count, waves, results):
    expected = "PONG PING"
    wrong = []
    started = time.monotonic()

    for wave in range(waves):
        barrier = threading.Barrier(count)
        outcomes = [None] * count
        threads = [
            threading.Thread(
                target=one_connection, args=(target, port, barrier, outcomes, index)
            )
            for index in range(count)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=30)
        wrong.extend(
            f"wave {wave + 1}/{waves}: {outcome}"
            for outcome in outcomes
            if outcome != expected
        )
        if wrong:
            break

    elapsed = time.monotonic() - started
    total = count * waves
    if wrong:
        results.append(("FAIL", f"{total} tcp connections", f"{wrong[:5]}"))
    else:
        results.append(
            (
                "PASS",
                f"{total} tcp connections",
                f"{waves} waves of {count} in {elapsed:.2f}s",
            )
        )


def check_udp(target, port, count, results):
    sockets = []
    started = time.monotonic()
    try:
        for _ in range(count):
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.sendto(b"ping", (target, port))
            sockets.append(sock)
    except OSError as err:
        results.append(("FAIL", f"{count} udp flows", f"send failed after {len(sockets)}: {err}"))
        for sock in sockets:
            sock.close()
        return

    deadline = started + UDP_BUDGET
    received = 0
    wrong_source = 0
    for sock in sockets:
        remaining = deadline - time.monotonic()
        if remaining > 0:
            try:
                sock.settimeout(remaining)
                data, peer = sock.recvfrom(2048)
            except OSError:
                pass
            else:
                if data == b"pong:ping":
                    if peer[0] == target:
                        received += 1
                    else:
                        wrong_source += 1
        sock.close()

    elapsed = time.monotonic() - started
    if received != count or wrong_source:
        results.append(
            (
                "FAIL",
                f"{count} udp flows",
                f"replied={received} wrong_source={wrong_source} in {elapsed:.2f}s"
                f" (budget {UDP_BUDGET:.0f}s)",
            )
        )
    else:
        results.append(
            ("PASS", f"{count} udp flows", f"all replied in {elapsed:.2f}s")
        )


def check_bulk(target, port, count, results):
    if count <= 0:
        return

    expected = pattern(count)
    received = bytearray()
    started = time.monotonic()
    try:
        with socket.create_connection((target, port), timeout=10) as sock:
            sock.settimeout(120)
            sock.sendall(f"BULK {count}\n".encode())
            # The server appends a newline, and a recv can overshoot the byte
            # count, so the comparison is against the first `count` bytes.
            while len(received) < count:
                chunk = sock.recv(262144)
                if not chunk:
                    break
                received += chunk
    except OSError as err:
        results.append(("FAIL", "bulk transfer", f"{err} after {len(received)} bytes"))
        return

    elapsed = max(time.monotonic() - started, 1e-6)
    got = bytes(received[:count])
    if len(got) != count or got != expected:
        mismatch = next(
            (index for index, (a, b) in enumerate(zip(got, expected)) if a != b),
            len(got),
        )
        results.append(
            (
                "FAIL",
                "bulk transfer",
                f"{len(got)} of {count} bytes, first difference at {mismatch}",
            )
        )
        return

    results.append(
        (
            "PASS",
            "bulk transfer",
            f"{count} bytes verified in {elapsed:.2f}s ({count / elapsed / 1024:.0f} KiB/s)",
        )
    )


def internet_checksum(data):
    if len(data) % 2:
        data += b"\x00"
    total = 0
    for index in range(0, len(data), 2):
        total += (data[index] << 8) + data[index + 1]
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def icmp_echo(identifier, sequence, payload=b"watt-smoke"):
    header = struct.pack(">BBHHH", 8, 0, 0, identifier, sequence)
    checksum = internet_checksum(header + payload)
    return struct.pack(">BBHHH", 8, 0, checksum, identifier, sequence) + payload


def check_icmp(target, results):
    """The kernel is a relay, not a router: it must not answer for the target."""
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_ICMP)
    except OSError as err:
        results.append(("WARN", "icmp echo", f"raw socket unavailable: {err}"))
        return

    with sock:
        sock.settimeout(ICMP_TIMEOUT)
        try:
            sock.sendto(icmp_echo(ICMP_IDENTIFIER, 1), (target, 0))
        except OSError as err:
            results.append(("WARN", "icmp echo", f"send failed: {err}"))
            return

        deadline = time.monotonic() + ICMP_TIMEOUT
        while time.monotonic() < deadline:
            try:
                data, peer = sock.recvfrom(2048)
            except TimeoutError:
                break
            except OSError as err:
                results.append(("WARN", "icmp echo", f"receive failed: {err}"))
                return
            if peer[0] != target:
                continue
            # A raw ICMP socket sees the IP header, so byte 20 is the ICMP type.
            if len(data) >= 28 and data[20] == 0:
                results.append(("FAIL", "icmp echo", f"unexpected reply from {peer[0]}"))
                return

    results.append(("PASS", "icmp echo", "no reply, as expected from a relay"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dns-address", required=True, help="address the DNS query is sent to")
    parser.add_argument("--rule-address", required=True, help="address the rule set owns")
    parser.add_argument("--rule-port", type=int, default=80, help="port dialled on the rule address")
    parser.add_argument("--udp-address", required=True, help="address the UDP datagrams are sent to")
    parser.add_argument("--udp-port", type=int, required=True)
    parser.add_argument("--domain", required=True, help="domain the rule set owns")
    parser.add_argument(
        "--expect-answers",
        default="",
        help="comma separated addresses the DNS answer must contain",
    )
    parser.add_argument("--parallel", type=int, default=3, help="connections per wave")
    parser.add_argument("--tcp-waves", type=int, default=1, help="how many waves to run")
    parser.add_argument("--udp-flows", type=int, default=1, help="distinct UDP flows to open")
    parser.add_argument("--bulk-bytes", type=int, default=0, help="bytes to move in one transfer")
    args = parser.parse_args()

    expected = {item.strip() for item in args.expect_answers.split(",") if item.strip()}
    results = []

    check_dns(args.dns_address, args.domain, expected, results)

    # The three load legs overlap on purpose: a kernel that only holds up under
    # one kind of traffic at a time has not been tested.
    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = [
            pool.submit(
                check_tcp,
                args.rule_address,
                args.rule_port,
                args.parallel,
                args.tcp_waves,
                results,
            ),
            pool.submit(check_udp, args.udp_address, args.udp_port, args.udp_flows, results),
            pool.submit(check_bulk, args.rule_address, args.rule_port, args.bulk_bytes, results),
        ]
        for future in futures:
            future.result()

    check_icmp(args.rule_address, results)

    for status, name, detail in results:
        print(f"CLIENT {status} {name} ({detail})", flush=True)

    failures = sum(1 for status, _, _ in results if status == "FAIL")
    if failures:
        print(f"CLIENT RESULT FAIL failures={failures}", flush=True)
        return 1

    print(f"CLIENT RESULT PASS checks={len(results)}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
