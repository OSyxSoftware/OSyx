//! Benchmarks for the TCP/IP packet paths: checksums, IPv4 and TCP
//! encoding/decoding, fragment reassembly, and whole-stack loopback TCP and
//! UDP (segmenting, copies, checksums and ACKs without a device).

use divan::{black_box, Bencher};
use osyx_net::wire::{checksum, transport_checksum};
use osyx_net::{ipv4, tcp, Endpoint, Ipv4Addr};

fn main() {
    divan::main();
}

const SRC: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const DST: Ipv4Addr = Ipv4Addr([93, 184, 216, 34]);

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(13).wrapping_add(1)).collect()
}

#[divan::bench(args = [20, 1500, 65535])]
fn internet_checksum(bencher: Bencher, len: usize) {
    let data = payload(len);
    bencher.bench_local(|| checksum(black_box(&data)));
}

#[divan::bench]
fn tcp_transport_checksum_1460(bencher: Bencher) {
    let data = payload(1480);
    bencher.bench_local(|| transport_checksum(black_box(SRC), black_box(DST), ipv4::PROTO_TCP, black_box(&data)));
}

#[divan::bench]
fn ipv4_build_1500(bencher: Bencher) {
    let data = payload(1480);
    bencher.bench_local(|| ipv4::build(SRC, DST, ipv4::PROTO_TCP, 64, 1, black_box(&data), 1500, true).unwrap());
}

#[divan::bench]
fn ipv4_parse_1500(bencher: Bencher) {
    let packet = ipv4::build(SRC, DST, ipv4::PROTO_UDP, 64, 1, &payload(1480), 1500, false).unwrap().remove(0);
    bencher.bench_local(|| ipv4::parse(black_box(&packet)).unwrap().0);
}

#[divan::bench]
fn ipv4_fragment_and_reassemble_64k(bencher: Bencher) {
    let fragments = ipv4::build(SRC, DST, ipv4::PROTO_UDP, 64, 7, &payload(65000), 1500, false).unwrap();
    bencher.bench_local(|| {
        let mut r = ipv4::Reassembler::new();
        let mut done = None;
        for f in black_box(&fragments) {
            let (h, body) = ipv4::parse(f).unwrap();
            if let Some(d) = r.add(&h, body, 0) {
                done = Some(d.1.len());
            }
        }
        done.unwrap()
    });
}

#[divan::bench]
fn tcp_build_segment_1460(bencher: Bencher) {
    let local = Endpoint::new(SRC, 49152);
    let remote = Endpoint::new(DST, 443);
    let data = payload(1460);
    bencher.bench_local(|| {
        tcp::build(local, remote, 1000, 2000, tcp::ACK | tcp::PSH, 65535, None, None, black_box(&data))
    });
}

#[divan::bench]
fn tcp_parse_segment_1460(bencher: Bencher) {
    let local = Endpoint::new(SRC, 49152);
    let remote = Endpoint::new(DST, 443);
    let seg = tcp::build(local, remote, 1000, 2000, tcp::ACK | tcp::PSH, 65535, Some(1460), Some(7), &payload(1460));
    bencher.bench_local(|| tcp::parse(SRC, DST, black_box(&seg)).unwrap().seq);
}

#[divan::bench]
fn parse_endpoint() -> Option<Endpoint> {
    Endpoint::parse(black_box("192.168.100.254:65535"))
}

/// A stack with a loopback connection: (stack, client, server).
fn loopback_pair() -> (osyx_net::stack::Stack, u32, u32) {
    let mut s = osyx_net::stack::Stack::new(7);
    let l = s.tcp_listen(Endpoint::new(Ipv4Addr::LOOPBACK, 80), 4).unwrap();
    let c = s.tcp_connect(None, Endpoint::new(Ipv4Addr::LOOPBACK, 80)).unwrap();
    let (a, _) = s.tcp_accept(l).unwrap();
    (s, c, a)
}

/// One mebibyte through a loopback TCP connection: segmenting, checksums,
/// copies and acknowledgements of the whole stack, without a device.
#[divan::bench]
fn stack_loopback_tcp_1mib(bencher: Bencher) {
    let data = payload(1 << 20);
    // One connection for every iteration: steady state, as in a long
    // transfer, not the first growth of its buffers.
    let (mut s, c, a) = loopback_pair();
    let mut buf = vec![0u8; 64 * 1024];
    bencher.bench_local(|| {
        let (mut sent, mut got) = (0, 0);
        while got < data.len() {
            if sent < data.len() {
                sent += s.tcp_send(c, &data[sent..(sent + 64 * 1024).min(data.len())]).unwrap_or(0);
            }
            while let Ok(n) = s.tcp_recv(a, &mut buf) {
                got += n;
            }
        }
        got
    });
}

/// Request and response of 64 bytes over a loopback TCP connection.
#[divan::bench]
fn stack_loopback_tcp_round_trip(bencher: Bencher) {
    let (mut s, c, a) = loopback_pair();
    let msg = [0x42u8; 64];
    let mut buf = [0u8; 64];
    bencher.bench_local(|| {
        s.tcp_send(c, &msg).unwrap();
        let n = s.tcp_recv(a, &mut buf).unwrap();
        s.tcp_send(a, &buf[..n]).unwrap();
        s.tcp_recv(c, &mut buf).unwrap()
    });
}

/// A 1400-byte datagram to ourselves and back out of the socket.
#[divan::bench]
fn stack_loopback_udp_datagram(bencher: Bencher) {
    let mut s = osyx_net::stack::Stack::new(7);
    let rx = s.udp_bind(Endpoint::new(Ipv4Addr::LOOPBACK, 5000)).unwrap();
    let tx = s.udp_bind(Endpoint::new(Ipv4Addr::LOOPBACK, 0)).unwrap();
    let data = payload(1400);
    let mut buf = vec![0u8; 2048];
    bencher.bench_local(|| {
        s.udp_send_to(tx, Some(Endpoint::new(Ipv4Addr::LOOPBACK, 5000)), black_box(&data)).unwrap();
        s.udp_recv_from(rx, &mut buf).unwrap().1
    });
}
