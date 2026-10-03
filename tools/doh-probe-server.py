"""Local backend for doh-probe.html.

A static page cannot do two of the things this tool needs:

  * read the answer from an endpoint that sends no CORS header -- and both
    domestic resolvers are in that group. Alibaba answers the POST with 200 and
    no Access-Control-Allow-Origin, then fails the preflight with 400; Tencent
    fails it with 502. A browser can therefore never read what they said.
  * send a plain UDP/53 query, which is the only way to get the real baseline
    ("what does the resolver this machine actually uses answer").

Both disappear once the request is made from a process instead of a page. This
server does exactly that and nothing else: it serves the page, and answers three
JSON endpoints. No dependencies beyond the standard library.

    python doh-probe-server.py --port 8765
    adb reverse tcp:8765 tcp:8765     # then open http://127.0.0.1:8765 on a device

Binding stays on the loopback address. Reaching it from a device is what
`adb reverse` is for; there is no reason to expose it to the network.
"""

import argparse
import json
import os
import random
import re
import socket
import ssl
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = os.path.join(HERE, "doh-probe.html")
TIMEOUT = 8.0
QTYPE_A = 1
QTYPE_AAAA = 28


# ---------------------------------------------------------------- wire format

def encode_name(name):
    out = b""
    for label in name.split("."):
        raw = label.encode("ascii")
        out += bytes([len(raw)]) + raw
    return out + b"\x00"


def build_query(name, qtype, qid=None):
    if qid is None:
        qid = random.randrange(0x10000)
    header = struct.pack(">HHHHHH", qid, 0x0100, 1, 0, 0, 0)
    return header + encode_name(name) + struct.pack(">HH", qtype, 1), qid


def skip_name(data, off):
    for _ in range(128):  # a compression loop would otherwise spin forever
        if off >= len(data):
            raise ValueError("name runs past end of message")
        length = data[off]
        if length == 0:
            return off + 1
        if length & 0xC0 == 0xC0:
            return off + 2
        off += 1 + length
    raise ValueError("too many labels")


def read_name(data, off):
    """Decode a name, following compression pointers.

    Returns the offset of the byte *after the name as it appears at `off`*,
    which is not the same as where the labels ended once a pointer was followed:
    a pointer occupies two bytes at its own position and the name continues
    somewhere else entirely. Returning the end of the target instead silently
    desynchronises the record walk -- every following rdlen is read from the
    wrong place and the answer section comes back empty while still parsing
    without error. That is exactly what the first version of this did.
    """
    labels = []
    jumps = 0
    resume = None
    while True:
        if off >= len(data):
            raise ValueError("name runs past end of message")
        length = data[off]
        if length == 0:
            return ".".join(labels), (resume if resume is not None else off + 1)
        if length & 0xC0 == 0xC0:
            if jumps > 16:
                raise ValueError("compression pointer loop")
            if resume is None:
                resume = off + 2
            jumps += 1
            off = struct.unpack(">H", data[off:off + 2])[0] & 0x3FFF
            continue
        labels.append(data[off + 1:off + 1 + length].decode("ascii", "replace"))
        off += 1 + length


def parse_response(data):
    """Return the answer section in a shape the page can render directly."""
    if len(data) < 12:
        raise ValueError("shorter than a header")
    _qid, flags, qd, an, _ns, _ar = struct.unpack(">HHHHHH", data[:12])
    off = 12
    for _ in range(qd):
        off = skip_name(data, off) + 4
    answers = []
    for _ in range(an):
        name, off = read_name(data, off)
        rtype, _rclass, ttl, rdlen = struct.unpack(">HHIH", data[off:off + 10])
        off += 10
        rdata = data[off:off + rdlen]
        off += rdlen
        if rtype == QTYPE_A and rdlen == 4:
            answers.append({"name": name, "type": "A", "ttl": ttl,
                            "data": socket.inet_ntoa(rdata)})
        elif rtype == QTYPE_AAAA and rdlen == 16:
            answers.append({"name": name, "type": "AAAA", "ttl": ttl,
                            "data": socket.inet_ntop(socket.AF_INET6, rdata)})
        elif rtype == 5:
            try:
                target, _ = read_name(data, off - rdlen)
            except Exception:  # noqa: BLE001
                target = "?"
            answers.append({"name": name, "type": "CNAME", "ttl": ttl, "data": target})
    addrs = [a["data"] for a in answers if a["type"] in ("A", "AAAA")]
    return {"rcode": flags & 0xF, "answers": answers, "addrs": addrs}


# ------------------------------------------------------------------- probes

def probe_doh(endpoint, name, qtype):
    query, qid = build_query(name, qtype)
    req = urllib.request.Request(
        endpoint, data=query, method="POST",
        headers={"content-type": "application/dns-message",
                 "accept": "application/dns-message"})
    ctx = ssl.create_default_context()
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT, context=ctx) as resp:
            raw = resp.read()
            status = resp.status
            cors = resp.headers.get("Access-Control-Allow-Origin")
        parsed = parse_response(raw)
        return {"ok": True, "ms": round((time.monotonic() - started) * 1000),
                "status": status, "cors": cors, "id": qid, **parsed}
    except urllib.error.HTTPError as exc:
        return {"ok": False, "kind": "http",
                "ms": round((time.monotonic() - started) * 1000),
                "error": f"HTTP {exc.code}"}
    except Exception as exc:  # noqa: BLE001 - the class name is the finding
        return {"ok": False, "kind": classify(exc),
                "ms": round((time.monotonic() - started) * 1000),
                "error": f"{type(exc).__name__}: {exc}"}


def classify(exc):
    """Separate 'the name was cut' from 'nothing answered'.

    A reset right after the ClientHello and a host that never replies are both
    failures, but they are different findings: the first says the address is
    reachable and something above it refused this name, the second says the
    address is not reachable at all. Only the first is an SNI block.
    """
    text = f"{type(exc).__name__}: {exc}"
    if "10054" in text or "reset" in text.lower() or "UNEXPECTED_EOF" in text:
        return "reset"
    if "timed out" in text.lower() or "timeout" in text.lower():
        return "timeout"
    if "SSL" in text or "certificate" in text.lower():
        return "tls"
    return "error"


def probe_udp(resolver, name, qtype):
    query, qid = build_query(name, qtype)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(TIMEOUT)
    started = time.monotonic()
    try:
        sock.sendto(query, (resolver, 53))
        data, _peer = sock.recvfrom(4096)
        parsed = parse_response(data)
        echoed = struct.unpack(">H", data[:2])[0] == qid
        return {"ok": True, "ms": round((time.monotonic() - started) * 1000),
                "id_echoed": echoed, **parsed}
    except Exception as exc:  # noqa: BLE001
        return {"ok": False, "kind": classify(exc),
                "ms": round((time.monotonic() - started) * 1000),
                "error": f"{type(exc).__name__}: {exc}"}
    finally:
        sock.close()


def probe_tcp(resolver, name, qtype):
    """UDP/53 is where the injector lives. TCP/53 is the control for it: when the
    two transports disagree about the same name, that disagreement is the
    injection, and it needs no blocklist to be read."""
    query, _qid = build_query(name, qtype)
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(TIMEOUT)
    started = time.monotonic()
    try:
        sock.connect((resolver, 53))
        sock.sendall(struct.pack(">H", len(query)) + query)
        head = sock.recv(2)
        if len(head) < 2:
            raise ValueError("no length prefix")
        want = struct.unpack(">H", head)[0]
        data = b""
        while len(data) < want:
            chunk = sock.recv(want - len(data))
            if not chunk:
                break
            data += chunk
        parsed = parse_response(data)
        return {"ok": True, "ms": round((time.monotonic() - started) * 1000), **parsed}
    except Exception as exc:  # noqa: BLE001
        return {"ok": False, "kind": classify(exc),
                "ms": round((time.monotonic() - started) * 1000),
                "error": f"{type(exc).__name__}: {exc}"}
    finally:
        sock.close()


# ----------------------------------------------------------------- resolvers

def detect_resolvers():
    """Best effort, and reported as such -- the page lets the user override it.

    Two things make this fiddly, and both are handled by being narrow rather
    than clever. `ipconfig` is localised, so the label is matched loosely. And
    it lists every adapter, including the virtual ones WSL and Hyper-V leave
    behind, whose DNS server is not the one this machine actually uses.

    The first attempt at this scanned any IPv4 near the word "DNS" and returned
    the subnet mask and the default gateway along with the resolver, which is
    the wrong answer delivered confidently. Addresses are now taken only from
    the tail of a DNS line and its continuation lines.
    """
    if os.name != "nt":
        found = []
        try:
            with open("/etc/resolv.conf", encoding="utf-8") as handle:
                for line in handle:
                    match = re.match(r"\s*nameserver\s+(\S+)", line)
                    if match:
                        found.append(match.group(1))
        except Exception:  # noqa: BLE001
            pass
        return found[:4]

    try:
        out = subprocess.run(["ipconfig", "/all"], capture_output=True, text=True,
                             timeout=10, encoding="utf-8", errors="replace").stdout
    except Exception:  # noqa: BLE001
        return []

    def dns_servers(block):
        servers = []
        lines = block.splitlines()
        for index, line in enumerate(lines):
            if not re.search(r"DNS|域名", line) or ":" not in line:
                continue
            servers += re.findall(r"\d{1,3}(?:\.\d{1,3}){3}", line.rsplit(":", 1)[1])
            for follow in lines[index + 1:]:
                stripped = follow.strip()
                if re.fullmatch(r"\d{1,3}(?:\.\d{1,3}){3}", stripped):
                    servers.append(stripped)
                elif stripped:
                    break
        return servers

    def has_gateway(block):
        for line in block.splitlines():
            if re.search(r"网关|Gateway", line) and ":" in line:
                if re.search(r"\d{1,3}(?:\.\d{1,3}){3}", line.rsplit(":", 1)[1]):
                    return True
        return False

    virtual = re.compile(r"Hyper-V|WSL|Virtual|虚拟|Loopback|回环|Bluetooth|蓝牙", re.I)
    blocks = [b for b in re.split(r"\r?\n\s*\r?\n", out) if b.strip()]
    ranked = ([b for b in blocks if has_gateway(b) and not virtual.search(b)]
              + [b for b in blocks if has_gateway(b) and virtual.search(b)]
              + [b for b in blocks if not has_gateway(b) and not virtual.search(b)])

    found = []
    for block in ranked:
        for addr in dns_servers(block):
            if addr in found or addr.startswith("0.") or addr == "255.255.255.255":
                continue
            if addr.endswith(".0") and addr.count(".") == 3:
                continue  # a network address, i.e. a subnet mask read as one
            found.append(addr)
    return found[:4]


# -------------------------------------------------------------------- server

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def _send(self, code, body, content_type):
        payload = body if isinstance(body, bytes) else body.encode("utf-8")
        self.send_response(code)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(payload)))
        self.send_header("cache-control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def _json(self, payload, code=200):
        self._send(code, json.dumps(payload, ensure_ascii=False), "application/json; charset=utf-8")

    def do_GET(self):  # noqa: N802 - the base class names it
        parsed = urlparse(self.path)
        query = {k: v[0] for k, v in parse_qs(parsed.query).items()}

        if parsed.path in ("/", "/index.html"):
            try:
                with open(PAGE, "rb") as handle:
                    self._send(200, handle.read(), "text/html; charset=utf-8")
            except OSError:
                self._send(404, "doh-probe.html not found next to the server", "text/plain")
            return

        if parsed.path == "/api/ping":
            self._json({"ok": True, "backend": True,
                        "resolvers": detect_resolvers()})
            return

        if parsed.path == "/api/probe":
            endpoint = query.get("endpoint", "")
            name = query.get("name", "")
            qtype = QTYPE_AAAA if query.get("type", "A").upper() == "AAAA" else QTYPE_A
            if not endpoint.startswith("https://") or not name:
                self._json({"ok": False, "kind": "bad-request",
                            "error": "endpoint must be https and name is required"}, 400)
                return
            self._json(probe_doh(endpoint, name, qtype))
            return

        if parsed.path == "/api/baseline":
            name = query.get("name", "")
            resolver = query.get("resolver", "").strip()
            if not name:
                self._json({"ok": False, "error": "name is required"}, 400)
                return
            if not resolver:
                candidates = detect_resolvers()
                resolver = candidates[0] if candidates else ""
            if not resolver:
                self._json({"ok": False, "kind": "no-resolver",
                            "error": "no system resolver detected; pass ?resolver="}, 400)
                return
            qtype = QTYPE_AAAA if query.get("type", "A").upper() == "AAAA" else QTYPE_A
            self._json({"ok": True, "resolver": resolver,
                        "udp": probe_udp(resolver, name, qtype),
                        "tcp": probe_tcp(resolver, name, qtype)})
            return

        self._send(404, "not found", "text/plain")


def main():
    parser = argparse.ArgumentParser(description="DoH probe backend")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"doh-probe serving on http://{args.host}:{args.port}")
    print(f"  page:     http://{args.host}:{args.port}/")
    print(f"  resolvers detected: {detect_resolvers() or '(none -- pass ?resolver=)'}")
    print("  for a device:  adb reverse tcp:%d tcp:%d" % (args.port, args.port))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")


if __name__ == "__main__":
    main()
