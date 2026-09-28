#!/usr/bin/env python3
"""What this machine can reach directly, before any tunnel is involved.

The kernel steers traffic to addresses the rule document supplies. It does not
create connectivity: if a destination is unreachable from this network, pointing
a domain at it changes nothing. So the first question to answer about "can it
reach site X" is whether site X is reachable at all from here, and that is what
this reports.

Each host is tried twice: once by name, and once by the address the name resolves
to. The difference between those two answers is the whole story — a name that
resolves but whose address does not answer is a blocked destination, while a name
that does not resolve is a DNS problem, and the two need different responses.

Run it from a shell, not through `wsl.exe` with a pipe.
"""

import argparse
import socket
import ssl
import sys
import time

DEFAULT_HOSTS = [
    "github.com",
    "raw.githubusercontent.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "google.com",
    "www.google.com",
    "youtube.com",
    "www.youtube.com",
    "facebook.com",
    "www.facebook.com",
    "x.com",
    "twitter.com",
    "www.instagram.com",
    "en.wikipedia.org",
    "zh.wikipedia.org",
    "telegram.org",
    "web.telegram.org",
    "reddit.com",
    "www.reddit.com",
    "discord.com",
    "medium.com",
    "example.com",
]

CONNECT_TIMEOUT = 4.0
TLS_TIMEOUT = 6.0


def resolve(host):
    """Every A record for a host, or an empty list."""
    try:
        infos = socket.getaddrinfo(host, 443, socket.AF_INET, socket.SOCK_STREAM)
    except OSError as err:
        return [], str(err)
    seen = []
    for info in infos:
        address = info[4][0]
        if address not in seen:
            seen.append(address)
    return seen, None


def tcp_reachable(address, port=443):
    """Can a TCP connection to this address be established?"""
    started = time.monotonic()
    try:
        with socket.create_connection((address, port), timeout=CONNECT_TIMEOUT):
            return True, time.monotonic() - started, None
    except OSError as err:
        return False, time.monotonic() - started, str(err)


def tls_ok(address, server_name):
    """Does a real TLS handshake with certificate verification complete?"""
    context = ssl.create_default_context()
    try:
        with socket.create_connection((address, 443), timeout=TLS_TIMEOUT) as raw:
            with context.wrap_socket(raw, server_hostname=server_name) as tls:
                return True, tls.version(), None
    except ssl.SSLCertVerificationError as err:
        return False, None, f"certificate: {err.verify_message}"
    except ssl.SSLError as err:
        return False, None, f"tls: {err}"
    except OSError as err:
        return False, None, str(err)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("hosts", nargs="*", default=None)
    parser.add_argument("--tls", action="store_true",
                        help="also attempt a verified TLS handshake")
    options = parser.parse_args()

    hosts = options.hosts or DEFAULT_HOSTS

    print(f"{'host':<30} {'dns':<18} {'tcp':<7} {'ms':>7}  detail")
    print("-" * 100)

    reachable = []
    blocked = []
    unresolved = []

    for host in hosts:
        addresses, dns_error = resolve(host)
        if not addresses:
            print(f"{host:<30} {'-':<18} {'no':<7} {'-':>7}  {dns_error}")
            unresolved.append(host)
            continue

        first = addresses[0]
        ok, elapsed, error = tcp_reachable(first)
        if not ok:
            print(f"{host:<30} {first:<18} {'no':<7} {elapsed * 1000:>7.0f}  {error}")
            blocked.append((host, first, error))
            continue

        detail = f"{len(addresses)} address(es)"
        if options.tls:
            verified, version, tls_error = tls_ok(first, host)
            if verified:
                detail = f"{version}, certificate verified"
            else:
                detail = f"TCP up but {tls_error}"
                blocked.append((host, first, f"tls: {tls_error}"))
                print(f"{host:<30} {first:<18} {'tls':<7} {elapsed * 1000:>7.0f}  {detail}")
                continue

        print(f"{host:<30} {first:<18} {'yes':<7} {elapsed * 1000:>7.0f}  {detail}")
        reachable.append(host)

    print()
    print(f"reachable : {len(reachable)}")
    for host in reachable:
        print(f"  {host}")
    print(f"blocked   : {len(blocked)}")
    for host, address, error in blocked:
        print(f"  {host} ({address}): {error}")
    print(f"unresolved: {len(unresolved)}")
    for host in unresolved:
        print(f"  {host}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
