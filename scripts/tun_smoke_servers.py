#!/usr/bin/env python3
"""Local servers the TUN test harness rewrites relayed flows onto.

Both listeners are on loopback, so they are reachable from the engine's upstream
sockets but invisible to the tunnel itself:

* TCP answers ``PING`` with ``PONG PING``, and ``BULK <n>`` with ``n`` bytes of a
  deterministic pattern. Echoing the request back proves the bytes made a full
  round trip rather than the client seeing a canned reply, and the pattern lets
  the client verify a large transfer byte for byte.
* UDP answers every datagram with ``pong:<payload>``.

The ports are ephemeral, so they are printed on one line for the shell script to
parse. The process then runs until it is signalled.

Imported by ``tun_smoke_client.py`` for :func:`pattern`, so the two halves of a
bulk transfer cannot drift apart.
"""

import signal
import socket
import sys
import threading
import time

# How long to wait for both listeners to report their port.
STARTUP_TIMEOUT = 5.0

# One block of the bulk pattern. Built from an affine sequence rather than
# ``bytes(range(256))`` so a shifted or duplicated region is still detectable.
PATTERN_BLOCK = bytes((index * 31 + 7) & 0xFF for index in range(4096))


def pattern(count):
    """Deterministic bytes of length ``count``, reproducible by the client."""
    repeats, remainder = divmod(count, len(PATTERN_BLOCK))
    return PATTERN_BLOCK * repeats + PATTERN_BLOCK[:remainder]


def read_line(conn):
    data = b""
    while not data.endswith(b"\n"):
        try:
            chunk = conn.recv(4096)
        except OSError:
            break
        if not chunk:
            break
        data += chunk
    return data


def handle_tcp(conn):
    with conn:
        conn.settimeout(10)
        line = read_line(conn)
        try:
            if line.startswith(b"BULK "):
                count = int(line.split()[1])
                conn.sendall(pattern(count) + b"\n")
                return
            conn.sendall(b"PONG " + line.strip() + b"\n")
        except (OSError, ValueError):
            pass


def serve_tcp(ready, errors):
    try:
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("127.0.0.1", 0))
        server.listen(64)
    except OSError as err:
        errors.append(f"tcp: {err}")
        return

    ready["tcp"] = server.getsockname()[1]
    while True:
        try:
            conn, _ = server.accept()
        except OSError:
            return
        # One thread per connection: the harness opens many at once on purpose,
        # to exercise the engine's listener pool.
        threading.Thread(target=handle_tcp, args=(conn,), daemon=True).start()


def serve_udp(ready, errors):
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
        sock.bind(("127.0.0.1", 0))
    except OSError as err:
        errors.append(f"udp: {err}")
        return

    ready["udp"] = sock.getsockname()[1]
    while True:
        try:
            data, peer = sock.recvfrom(65535)
        except OSError:
            return
        try:
            sock.sendto(b"pong:" + data, peer)
        except OSError:
            pass


def main():
    ready = {}
    errors = []

    for target in (serve_tcp, serve_udp):
        threading.Thread(target=target, args=(ready, errors), daemon=True).start()

    deadline = time.monotonic() + STARTUP_TIMEOUT
    while time.monotonic() < deadline:
        if errors or ("tcp" in ready and "udp" in ready):
            break
        time.sleep(0.05)

    if errors or "tcp" not in ready or "udp" not in ready:
        print("SERVERS FAILED " + ("; ".join(errors) or "timed out"), flush=True)
        return 1

    print(f"SERVERS tcp={ready['tcp']} udp={ready['udp']}", flush=True)

    stopping = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stopping.set())
    signal.signal(signal.SIGINT, lambda *_: stopping.set())
    stopping.wait()
    return 0


if __name__ == "__main__":
    sys.exit(main())
