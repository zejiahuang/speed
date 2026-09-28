#!/usr/bin/env python3
"""A real TLS + HTTP/1.1 server on loopback, for driving the kernel with real clients.

Every other server in this harness speaks a protocol invented for the occasion: a
line of text, or a fixed number of bytes. That proves the relay carries *bytes*.
It does not prove the relay carries *traffic*. This server exists so the kernel
can be pointed at by `curl` instead — a genuine TLS handshake, genuine HTTP/1.1
with keep-alive, genuine chunked encoding, and a body whose hash the client
checks independently.

Being a real server matters for the verdict, not just for realism: the counters
printed here come from code that knows nothing about the kernel, so they cannot
come out in the kernel's favour by mistake.

Routes
------
``GET /hello``            a short fixed body.
``GET /big?n=<count>``    ``count`` bytes of the shared deterministic pattern.
``GET /chunked?n=<n>``    the same bytes, sent chunked, with no Content-Length.
``POST /echo``            replies ``len=<n> sha256=<hex>`` for the body received.

``--hash <count>`` prints the sha256 the client should expect for ``/big`` and
exits, so the two sides cannot drift apart on what the pattern is.
"""

import argparse
import hashlib
import signal
import socket
import ssl
import sys
import threading
import time

from tun_smoke_servers import pattern

# Chunk size for the chunked route: deliberately not a round number, so a server
# that ignored the framing and used Content-Length would produce a body of the
# wrong length rather than one that happens to match.
CHUNK = 8191

# A request line plus headers may not exceed this. Anything larger is not a real
# client, and reading it forever is how a test hangs.
MAX_HEAD = 64 * 1024

# How long a single connection may sit between requests before it is closed.
IDLE_TIMEOUT = 30.0


class Stats:
    """Counters from outside the kernel. The verdict is built on these."""

    def __init__(self):
        self.lock = threading.Lock()
        self.connections = 0
        self.handshakes_failed = 0
        self.requests = 0
        self.max_requests_on_one_connection = 0
        self.bytes_in = 0
        self.bytes_out = 0
        self.status = {}

    def connection_opened(self):
        with self.lock:
            self.connections += 1
            return self.connections

    def handshake_failed(self):
        with self.lock:
            self.handshakes_failed += 1

    def request_served(self, served_on_this_connection, status, sent, received):
        with self.lock:
            self.requests += 1
            self.bytes_out += sent
            self.bytes_in += received
            self.status[status] = self.status.get(status, 0) + 1
            if served_on_this_connection > self.max_requests_on_one_connection:
                self.max_requests_on_one_connection = served_on_this_connection

    def describe(self):
        with self.lock:
            statuses = ",".join(f"{code}:{count}" for code, count in sorted(self.status.items()))
            return (
                f"connections={self.connections} handshakes_failed={self.handshakes_failed} "
                f"requests={self.requests} "
                f"max_requests_on_one_connection={self.max_requests_on_one_connection} "
                f"bytes_in={self.bytes_in} bytes_out={self.bytes_out} "
                f"status={statuses or '-'}"
            )


class Reader:
    """Buffered reader, because a request and its body can arrive in one segment."""

    def __init__(self, sock):
        self.sock = sock
        self.buffer = b""

    def read_until(self, marker):
        while marker not in self.buffer:
            if len(self.buffer) > MAX_HEAD:
                return None
            chunk = self.sock.recv(65536)
            if not chunk:
                return None
            self.buffer += chunk
        end = self.buffer.index(marker) + len(marker)
        found, self.buffer = self.buffer[:end], self.buffer[end:]
        return found

    def read_exactly(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(min(65536, count - len(self.buffer)))
            if not chunk:
                return None
            self.buffer += chunk
        taken, self.buffer = self.buffer[:count], self.buffer[count:]
        return taken


def parse_request(reader):
    """Read one request. Returns ``(method, target, version, headers, body)`` or None."""
    head = reader.read_until(b"\r\n\r\n")
    if head is None:
        return None

    lines = head.split(b"\r\n")
    try:
        method, target, version = lines[0].decode("latin-1").split(" ", 2)
    except ValueError:
        return None

    headers = {}
    for line in lines[1:]:
        if not line:
            continue
        name, _, value = line.decode("latin-1").partition(":")
        headers[name.strip().lower()] = value.strip()

    body = b""
    if "content-length" in headers:
        try:
            length = int(headers["content-length"])
        except ValueError:
            return None
        if length > 0:
            body = reader.read_exactly(length)
            if body is None:
                return None

    return method, target, version, headers, body


def response(status, body=b"", content_type="text/plain", chunked=False, keep_alive=True):
    reason = {200: "OK", 404: "Not Found", 400: "Bad Request"}.get(status, "OK")
    lines = [
        f"HTTP/1.1 {status} {reason}",
        f"Content-Type: {content_type}",
        "Connection: " + ("keep-alive" if keep_alive else "close"),
    ]
    if chunked:
        lines.append("Transfer-Encoding: chunked")
    else:
        lines.append(f"Content-Length: {len(body)}")
    head = ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1")

    if not chunked:
        return head + body

    # Framed properly: each piece carries its own length, and a zero-length chunk
    # ends the body. A client that got this wrong would see a truncated body.
    framed = b""
    for start in range(0, len(body), CHUNK):
        piece = body[start : start + CHUNK]
        framed += f"{len(piece):x}\r\n".encode("latin-1") + piece + b"\r\n"
    framed += b"0\r\n\r\n"
    return head + framed


def route(method, target, body):
    """Return ``(status, response_body, chunked)`` for a request."""
    path, _, query = target.partition("?")
    params = dict(
        pair.split("=", 1) for pair in query.split("&") if "=" in pair
    )

    if method == "GET" and path == "/hello":
        return 200, b"hello through the tunnel\n", False

    if method == "GET" and path in ("/big", "/chunked"):
        try:
            count = int(params.get("n", "0"))
        except ValueError:
            return 400, b"bad n\n", False
        if count < 0 or count > 64 * 1024 * 1024:
            return 400, b"bad n\n", False
        return 200, pattern(count), path == "/chunked"

    if method == "POST" and path == "/echo":
        digest = hashlib.sha256(body).hexdigest()
        return 200, f"len={len(body)} sha256={digest}\n".encode("latin-1"), False

    return 404, b"not found\n", False


def handle(conn, stats):
    served = 0
    try:
        conn.settimeout(IDLE_TIMEOUT)
        reader = Reader(conn)
        while True:
            parsed = parse_request(reader)
            if parsed is None:
                return
            method, target, version, headers, body = parsed
            served += 1

            keep_alive = headers.get("connection", "").lower() != "close"
            if version == "HTTP/1.0":
                keep_alive = headers.get("connection", "").lower() == "keep-alive"

            status, payload, chunked = route(method, target, body)
            wire = response(
                status,
                payload,
                chunked=chunked,
                keep_alive=keep_alive,
            )
            conn.sendall(wire)
            stats.request_served(served, status, len(wire), len(body))

            if not keep_alive:
                return
    except (OSError, ssl.SSLError):
        return
    finally:
        try:
            conn.close()
        except OSError:
            pass


def serve(context, stats, ready, errors):
    try:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen(128)
    except OSError as err:
        errors.append(str(err))
        return

    ready["port"] = listener.getsockname()[1]

    while True:
        try:
            raw, _ = listener.accept()
        except OSError:
            return

        # The handshake happens here, in the accept loop, so a client that fails it
        # is counted as a failed handshake rather than as a connection that
        # silently did nothing.
        try:
            conn = context.wrap_socket(raw, server_side=True)
        except (ssl.SSLError, OSError):
            stats.handshake_failed()
            try:
                raw.close()
            except OSError:
                pass
            continue

        stats.connection_opened()
        threading.Thread(target=handle, args=(conn, stats), daemon=True).start()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cert", help="PEM certificate chain")
    parser.add_argument("--key", help="PEM private key")
    parser.add_argument("--hash", type=int, metavar="COUNT",
                        help="print the sha256 of pattern(COUNT) and exit")
    options = parser.parse_args()

    if options.hash is not None:
        print(hashlib.sha256(pattern(options.hash)).hexdigest())
        return 0

    if not options.cert or not options.key:
        print("TLS_SERVER FAILED --cert and --key are required", flush=True)
        return 1

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(options.cert, options.key)
    # The client is told not to verify, so this is not needed for correctness, but
    # keeping the default floor makes the handshake representative of a real one.
    context.minimum_version = ssl.TLSVersion.TLSv1_2

    stats = Stats()
    ready = {}
    errors = []

    threading.Thread(target=serve, args=(context, stats, ready, errors), daemon=True).start()

    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and not ready and not errors:
        time.sleep(0.05)

    if errors or "port" not in ready:
        print("TLS_SERVER FAILED " + ("; ".join(errors) or "timed out"), flush=True)
        return 1

    print(f"TLS_SERVER tls={ready['port']}", flush=True)

    stopping = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stopping.set())
    signal.signal(signal.SIGINT, lambda *_: stopping.set())
    stopping.wait()

    # Printed on the way out, so the harness can build its verdict on numbers this
    # process produced rather than on the kernel's self-report.
    print(f"TLS_STATS {stats.describe()}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
