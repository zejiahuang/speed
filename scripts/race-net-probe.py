import socket, select, time

def probe(addr, port, label):
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setblocking(False)
    r = s.connect_ex((addr, port))
    # r==0 means connected synchronously; EINPROGRESS(115) means in progress
    t0 = time.time()
    w, _, _ = select.select([], [s], [], 2.0)
    elapsed = time.time() - t0
    err = s.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
    print(f"{label}: connect_ex={r} writable={bool(w)} elapsed={elapsed:.3f}s SO_ERROR={err}")
    s.close()

# A closed loopback port: bind then close.
tmp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
tmp.bind(("127.0.0.1", 0))
closed_port = tmp.getsockname()[1]
tmp.close()

probe("127.0.0.1", closed_port, "closed-loopback")
probe("203.0.113.250", 9, "test-net-3-250")
probe("203.0.113.251", 9, "test-net-3-251")
probe("203.0.113.252", 9, "test-net-3-252")

# A live loopback listener for contrast.
live = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
live.bind(("127.0.0.1", 0))
live.listen(4)
live_port = live.getsockname()[1]
probe("127.0.0.1", live_port, "live-loopback")
live.close()
