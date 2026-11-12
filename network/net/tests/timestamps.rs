//! The TCP timestamps option (RFC 7323): negotiation, the echo, round-trip
//! time from the echo, the smaller segments and PAWS.

use osyx_net::tcp::{self, OutSegment, SackBlocks, State, Tcb, ACK, PSH, SYN, TS_OPTION_LEN};
use osyx_net::wire::{Endpoint, Ipv4Addr};

const A: Endpoint = Endpoint { addr: Ipv4Addr([10, 0, 0, 1]), port: 40000 };
const B: Endpoint = Endpoint { addr: Ipv4Addr([10, 0, 0, 2]), port: 80 };
const MSS: usize = 1000;

fn deliver(seg: &OutSegment, from: Endpoint, to_addr: Endpoint, to: &mut Tcb, now: u64) {
    let bytes = seg.encode(from, to_addr);
    let parsed = tcp::parse(from.addr, to_addr.addr, &bytes).expect("own segment parses");
    to.input(&parsed, now);
}

/// A client and a server after the handshake at time `now`; `server_ts`:
/// whether the server's side takes part (a peer without the option never
/// sees ours, which is modelled by stripping it from the SYN).
fn pair(server_ts: bool, now: u64) -> (Tcb, Tcb) {
    let mut a = Tcb::connect(A, B, 1000, MSS as u16);
    let mut syn = a.output(now).remove(0);
    let (value, echo) = syn.ts.expect("our SYN offers timestamps");
    assert_eq!(echo, 0, "nothing to echo on the first SYN");
    if !server_ts {
        syn.ts = None;
    }
    let bytes = syn.encode(A, B);
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    let mut b = Tcb::accept(B, A, &parsed, 5000, MSS as u16, now);
    let synack = b.output(now).remove(0);
    assert_eq!(synack.ts.map(|ts| ts.1), server_ts.then_some(value), "the SYN-ACK echoes the SYN's timestamp, or has none");
    deliver(&synack, B, A, &mut a, now);
    for seg in a.output(now) {
        assert_eq!(seg.ts.is_some(), server_ts);
        deliver(&seg, A, B, &mut b, now);
    }
    assert_eq!((a.state(), b.state()), (State::Established, State::Established));
    (a, b)
}

#[test]
fn the_option_survives_the_codec_with_every_other_option() {
    let mut sack = SackBlocks::NONE;
    assert!(sack.push(10, 20) && sack.push(30, 40) && sack.push(50, 60));
    let seg = OutSegment { seq: 1, ack: 2, flags: ACK, window: 9, mss: None, ws: None, sack_permitted: false, sack, ts: Some((0xDEAD_BEEF, 77)), payload: vec![] };
    let bytes = seg.encode(A, B);
    assert_eq!(bytes.len(), 20 + 40, "timestamps and three SACK blocks fill the option space exactly");
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    assert_eq!(parsed.ts, Some((0xDEAD_BEEF, 77)));
    assert_eq!(parsed.sack.blocks().as_slice().len(), 3);

    let syn = OutSegment { seq: 7, ack: 0, flags: SYN, window: 65535, mss: Some(1460), ws: Some(8), sack_permitted: true, sack: SackBlocks::NONE, ts: Some((5, 0)), payload: vec![] };
    let bytes = syn.encode(A, B);
    assert_eq!(bytes.len(), 20 + 24);
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    assert_eq!((parsed.mss, parsed.ws, parsed.sack_permitted, parsed.ts), (Some(1460), Some(8), true, Some((5, 0))));

    // A timestamps option of the wrong length is malformed.
    let mut bad = tcp::build(A, B, 1, 2, ACK, 100, None, None, &[]);
    bad.extend_from_slice(&[8, 6, 0, 0, 0, 1, 1, 1]);
    bad[12] = 7 << 4;
    bad[16] = 0;
    bad[17] = 0;
    let c = osyx_net::wire::transport_checksum(A.addr, B.addr, 6, &bad);
    bad[16..18].copy_from_slice(&c.to_be_bytes());
    assert_eq!(tcp::parse(A.addr, B.addr, &bad).err(), Some(tcp::SegmentError::BadOption));
}

#[test]
fn every_segment_carries_the_option_and_data_segments_shrink_by_it() {
    let (mut a, mut b) = pair(true, 100);
    assert_eq!(a.mss() as usize, MSS - TS_OPTION_LEN);
    let data = vec![7u8; 3 * MSS];
    assert_eq!(a.send(&data), Ok(data.len()));
    let segs = a.output(150);
    assert!(segs.len() >= 3);
    for seg in &segs {
        assert!(seg.ts.is_some());
        assert!(seg.encode(A, B).len() - 20 <= MSS, "options and payload fit the MSS");
    }
    assert_eq!(segs[0].payload.len(), MSS - TS_OPTION_LEN);
    // The receiver echoes the sender's clock; the sender echoes the
    // receiver's SYN-ACK.
    for seg in &segs {
        deliver(seg, A, B, &mut b, 160);
    }
    let acks = b.output(160);
    let sent = segs[0].ts.unwrap().0;
    assert_eq!(acks.last().and_then(|s| s.ts).map(|ts| ts.1), Some(sent), "the ACK echoes the first unacknowledged segment's timestamp");
}

#[test]
fn a_peer_without_the_option_gets_plain_segments() {
    let (mut a, _b) = pair(false, 100);
    assert_eq!(a.mss() as usize, MSS);
    assert_eq!(a.send(&[1u8; 10]), Ok(10));
    let seg = a.output(120).remove(0);
    assert_eq!((seg.ts, seg.payload.len(), seg.flags & PSH), (None, 10, PSH));
}

#[test]
fn round_trip_time_comes_from_the_echo_even_for_a_retransmission() {
    let (mut a, mut b) = pair(true, 1_000);
    // The handshake took no time: the first data round trip of 300 ms is
    // lost once and measured on the retransmission (Karn's rule would have
    // discarded it).
    assert_eq!(a.send(&[9u8; 100]), Ok(100));
    let first = a.output(2_000);
    assert_eq!(first.len(), 1);
    let timeout = 2_000 + a.rto_ms();
    assert!(a.poll(timeout));
    let again = a.output(timeout);
    assert_eq!((again.len(), again[0].seq), (1, first[0].seq));
    deliver(&again[0], A, B, &mut b, timeout + 150);
    for ack in b.output(timeout + 150) {
        deliver(&ack, B, A, &mut a, timeout + 300);
    }
    assert_eq!(a.send_queue(), 0);
    let srtt = a.srtt_ms().expect("a sample was taken");
    assert!((30..=300).contains(&srtt), "smoothed round trip {srtt} ms moved toward the 300 ms sample");
}

#[test]
fn an_old_timestamp_is_dropped_whatever_its_sequence_number() {
    let (mut a, mut b) = pair(true, 1_000);
    assert_eq!(a.send(&[1u8; 50]), Ok(50));
    let early = a.output(2_000).remove(0);
    deliver(&early, A, B, &mut b, 2_010);
    for ack in b.output(2_010) {
        deliver(&ack, B, A, &mut a, 2_020);
    }
    let mut got = [0u8; 64];
    assert_eq!(b.recv(&mut got), Ok(50));
    // Later data, then a segment in the window that claims an older time
    // than the one already accepted: an old duplicate from a wrapped
    // sequence space. It must not reach the application.
    assert_eq!(a.send(&[2u8; 50]), Ok(50));
    let mut next = a.output(9_000).remove(0);
    let good = next.clone();
    next.ts = next.ts.map(|(value, echo)| (value.wrapping_sub(5_000_000), echo));
    next.payload = vec![0xEE; 50];
    let duplicates = b.stats.duplicates;
    deliver(&next, A, B, &mut b, 9_010);
    assert_eq!(b.stats.duplicates, duplicates + 1);
    assert_eq!(b.recv_queue(), 0, "the stale segment was not delivered");
    assert!(b.output(9_010).iter().any(|s| s.flags & ACK != 0), "it is acknowledged");
    deliver(&good, A, B, &mut b, 9_020);
    assert_eq!(b.recv(&mut got), Ok(50));
    assert_eq!(&got[..50], &[2u8; 50]);
}
