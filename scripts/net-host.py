"""Host-side endpoints for the OSyx network test.

TCP echo server and UDP echo server on 127.0.0.1 (QEMU's user network maps
the guest's 10.0.2.2:<port> to them). Every connection and datagram is
logged with byte counts to the log file, so the test can show that the
traffic really reached an independent program on the host.

usage: python net-host.py <tcp port> <udp port> <log file>
"""

import socket
import sys
import threading
import time

tcp_port, udp_port, log_path = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
lock = threading.Lock()


def log(msg):
    with lock, open(log_path, "a", encoding="utf-8") as f:
        f.write(f"{time.strftime('%H:%M:%S')} {msg}\n")


def serve_tcp(conn, peer):
    total = 0
    try:
        conn.settimeout(60)
        while True:
            data = conn.recv(65536)
            if not data:
                break
            conn.sendall(data)
            total += len(data)
    except OSError as e:
        log(f"tcp {peer}: error {e}")
    finally:
        conn.close()
        log(f"tcp {peer[0]}:{peer[1]} echoed {total} bytes")


def tcp_server():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", tcp_port))
    s.listen(16)
    log(f"tcp echo listening on 127.0.0.1:{tcp_port}")
    while True:
        conn, peer = s.accept()
        log(f"tcp connection from {peer[0]}:{peer[1]}")
        threading.Thread(target=serve_tcp, args=(conn, peer), daemon=True).start()


def udp_server():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", udp_port))
    log(f"udp echo listening on 127.0.0.1:{udp_port}")
    while True:
        data, peer = s.recvfrom(65536)
        s.sendto(data, peer)
        log(f"udp {len(data)} bytes from {peer[0]}:{peer[1]}: {data[:40]!r}")


threading.Thread(target=tcp_server, daemon=True).start()
threading.Thread(target=udp_server, daemon=True).start()
while True:
    time.sleep(3600)
