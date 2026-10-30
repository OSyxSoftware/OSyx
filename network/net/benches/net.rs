//! Benchmarks for the TCP/IP packet paths: checksums, IPv4 and TCP
//! encoding/decoding and fragment reassembly.

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
