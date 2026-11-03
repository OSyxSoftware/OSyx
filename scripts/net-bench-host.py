"""Host side of the OSyx network benchmark (scripts/net-bench.ps1).

Listens on 127.0.0.1; QEMU's user network maps the guest's 10.0.2.2:<port>
to it. The protocol is described in userspace/apps/src/net_bench.rs:

TCP: one command byte and a little-endian u64 length.
  U  read that many bytes, answer with the u64 count received
  D  send that many bytes, then close
  E  echo until the guest closes
UDP: the first byte of each datagram.
  E  echo the datagram
  B  reset the sink counter
  S  count the datagram
  Z  answer R + u64 count since B
  D  u32 count, u16 size: send that many datagrams of that size back

usage: python net-bench-host.py <tcp port> <udp port> <log file>
"""

import socket
import struct
import sys
import threading
import time

tcp_port, udp_port, log_path = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
lock = threading.Lock()


def log(msg):
    with lock, open(log_path, "a", encoding="utf-8") as f:
        f.write(f"{time.strftime('%H:%M:%S')} {msg}\n")


def read_exact(conn, n):
    buf = bytearray()
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return bytes(buf)


def serve_tcp(conn, peer):
    try:
        conn.settimeout(120)
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        head = read_exact(conn, 9)
        if head is None:
            return
        cmd, length = head[0:1], struct.unpack("<Q", head[1:])[0]
        start = time.perf_counter()
        if cmd == b"U":
            got = 0
            while got < length:
                data = conn.recv(min(1 << 20, length - got))
                if not data:
                    break
                got += len(data)
            conn.sendall(struct.pack("<Q", got))
            detail = f"received {got} bytes"
        elif cmd == b"D":
            block = b"\xa5" * (1 << 16)
            left = length
            while left > 0:
                n = min(left, len(block))
                conn.sendall(block[:n])
                left -= n
            detail = f"sent {length} bytes"
        elif cmd == b"E":
            total = 0
            while True:
                data = conn.recv(65536)
                if not data:
                    break
                conn.sendall(data)
                total += len(data)
            detail = f"echoed {total} bytes"
        else:
            detail = f"unknown command {cmd!r}"
        log(f"tcp {peer[0]}:{peer[1]} {detail} in {time.perf_counter() - start:.3f} s")
    except OSError as e:
        log(f"tcp {peer}: error {e}")
    finally:
        conn.close()


def tcp_server():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", tcp_port))
    s.listen(16)
    log(f"tcp bench listening on 127.0.0.1:{tcp_port}")
    while True:
        conn, peer = s.accept()
        threading.Thread(target=serve_tcp, args=(conn, peer), daemon=True).start()


def udp_server():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
    s.bind(("127.0.0.1", udp_port))
    log(f"udp bench listening on 127.0.0.1:{udp_port}")
    sink = 0
    while True:
        data, peer = s.recvfrom(65536)
        cmd = data[0:1]
        if cmd == b"E":
            s.sendto(data, peer)
        elif cmd == b"S":
            sink += 1
        elif cmd == b"B":
            sink = 0
        elif cmd == b"Z":
            s.sendto(b"R" + struct.pack("<Q", sink), peer)
            log(f"udp sink counted {sink} datagrams from {peer[0]}:{peer[1]}")
        elif cmd == b"D" and len(data) >= 7:
            count, size = struct.unpack("<IH", data[1:7])
            payload = b"d" * size
            for _ in range(count):
                s.sendto(payload, peer)
            log(f"udp sent {count} datagrams of {size} bytes to {peer[0]}:{peer[1]}")


threading.Thread(target=tcp_server, daemon=True).start()
threading.Thread(target=udp_server, daemon=True).start()
while True:
    time.sleep(3600)
