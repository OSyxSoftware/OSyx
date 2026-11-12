//! Selective acknowledgements (RFC 2018) between two connections joined by
//! a wire that can lose segments: the option on the wire, the negotiation,
//! the blocks a receiver reports and what a sender retransmits.

use osyx_net::tcp::{self, OutSegment, SackBlocks, State, Tcb, ACK, SYN};
use osyx_net::wire::{Endpoint, Ipv4Addr};

const A: Endpoint = Endpoint { addr: Ipv4Addr([10, 0, 0, 1]), port: 40000 };
const B: Endpoint = Endpoint { addr: Ipv4Addr([10, 0, 0, 2]), port: 80 };
const MSS: usize = 1000;

/// Encodes `seg` as `from` sends it and hands it to `to`, as the stack
/// would (so every option goes through the codec).
fn deliver(seg: &OutSegment, from: Endpoint, to_addr: Endpoint, to: &mut Tcb, now: u64) {
    let bytes = seg.encode(from, to_addr);
    let parsed = tcp::parse(from.addr, to_addr.addr, &bytes).expect("own segment parses");
    to.input(&parsed, now);
}

/// A client (`a`) and a server (`b`) after the handshake. `server_sack`:
/// whether the server's side takes part in SACK (a peer without it never
/// sends the option, which is modelled by stripping it from the SYN).
fn pair(server_sack: bool) -> (Tcb, Tcb) {
    let mut a = Tcb::connect(A, B, 1000, MSS as u16);
    let syn = a.output(0).remove(0);
    assert!(syn.sack_permitted, "our SYN offers SACK");
    let mut wire_syn = syn.clone();
    wire_syn.sack_permitted = server_sack;
    let bytes = wire_syn.encode(A, B);
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    assert_eq!(parsed.sack_permitted, server_sack);
    let mut b = Tcb::accept(B, A, &parsed, 5000, MSS as u16, 0);
    let synack = b.output(0).remove(0);
    assert_eq!(synack.flags, SYN | ACK);
    assert_eq!(synack.sack_permitted, server_sack, "a SYN-ACK permits SACK only in answer to an offer");
    deliver(&synack, B, A, &mut a, 1);
    for seg in a.output(1) {
        deliver(&seg, A, B, &mut b, 1);
    }
    assert_eq!((a.state(), b.state()), (State::Established, State::Established));
    (a, b)
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

#[test]
fn sack_options_survive_the_codec() {
    let mut sack = SackBlocks::NONE;
    assert!(sack.push(3000, 4000) && sack.push(6000, 7000) && sack.push(9000, 9500));
    let seg = OutSegment { seq: 1, ack: 2000, flags: ACK, window: 512, mss: None, ws: None, sack_permitted: false, sack, payload: vec![] };
    let bytes = seg.encode(A, B);
    assert_eq!(bytes.len(), 20 + 4 + 3 * 8, "two NOPs, kind, length and three blocks");
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    assert_eq!(parsed.sack.as_slice(), &[(3000, 4000), (6000, 7000), (9000, 9500)]);
    assert!(!parsed.sack_permitted);

    // A SYN with every option: MSS, window scale, SACK-permitted.
    let syn = OutSegment { seq: 7, ack: 0, flags: SYN, window: 65535, mss: Some(1460), ws: Some(8), sack_permitted: true, sack: SackBlocks::NONE, payload: vec![] };
    let bytes = syn.encode(A, B);
    assert_eq!(bytes.len(), 20 + 12);
    let parsed = tcp::parse(A.addr, B.addr, &bytes).unwrap();
    assert_eq!((parsed.mss, parsed.ws, parsed.sack_permitted), (Some(1460), Some(8), true));
    assert!(parsed.sack.is_empty());

    // Segments without the new options are encoded as before.
    assert_eq!(tcp::build(A, B, 1, 2, ACK, 100, None, None, b"data").len(), 24);
    assert_eq!(tcp::build(A, B, 1, 0, SYN, 100, Some(1460), Some(7), &[]).len(), 28);
}

#[test]
fn malformed_sack_options_are_rejected() {
    // A valid header with room for 12 option bytes, filled per case.
    let with_options = |opts: [u8; 12]| {
        let mut b = tcp::build(A, B, 1, 2, ACK, 100, None, None, &[]);
        b.extend_from_slice(&opts);
        b[12] = 8 << 4;
        b[16] = 0;
        b[17] = 0;
        let c = osyx_net::wire::transport_checksum(A.addr, B.addr, 6, &b);
        b[16..18].copy_from_slice(&c.to_be_bytes());
        tcp::parse(A.addr, B.addr, &b).map(|s| (s.sack_permitted, s.sack.as_slice().to_vec()))
    };
    assert_eq!(with_options([1, 1, 5, 10, 0, 0, 0, 1, 0, 0, 0, 2]), Ok((false, vec![(1, 2)])));
    assert_eq!(with_options([4, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]), Ok((true, vec![])));
    // SACK-permitted with a length other than 2.
    assert_eq!(with_options([4, 3, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1]), Err(tcp::SegmentError::BadOption));
    // A SACK option without a whole block, and one with half a block more.
    assert_eq!(with_options([5, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]), Err(tcp::SegmentError::BadOption));
    assert_eq!(with_options([5, 6, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1]), Err(tcp::SegmentError::BadOption));
}

#[test]
fn a_receiver_reports_what_it_holds_most_recent_first() {
    let (mut a, mut b) = pair(true);
    let data = pattern(6 * MSS);
    assert_eq!(a.send(&data), Ok(data.len()));
    let segs = a.output(2);
    assert_eq!(segs.len(), 6);
    let first = segs[0].seq;
    let block = |i: u32| (first + i * MSS as u32, first + (i + 1) * MSS as u32);

    // Segments 2 and 3 arrive, then 5: two ranges, the newest first.
    deliver(&segs[2], A, B, &mut b, 3);
    let ack = b.output(3).remove(0);
    assert_eq!((ack.ack, ack.sack.as_slice()), (first, &[block(2)][..]));
    deliver(&segs[3], A, B, &mut b, 3);
    let ack = b.output(3).remove(0);
    assert_eq!(ack.sack.as_slice(), &[(block(2).0, block(3).1)], "adjacent segments are one block");
    deliver(&segs[5], A, B, &mut b, 3);
    let ack = b.output(3).remove(0);
    assert_eq!(ack.sack.as_slice(), &[block(5), (block(2).0, block(3).1)]);

    // The first hole closes: the cumulative ACK moves, both blocks stay.
    deliver(&segs[0], A, B, &mut b, 4);
    let ack = b.output(4).remove(0);
    assert_eq!(ack.ack, block(1).0);
    assert_eq!(ack.sack.as_slice().len(), 2);
    // Everything arrives: nothing is left to report.
    deliver(&segs[1], A, B, &mut b, 5);
    deliver(&segs[4], A, B, &mut b, 5);
    let ack = b.output(5).remove(0);
    assert_eq!(ack.ack, block(5).1);
    assert!(ack.sack.is_empty());
    let mut got = vec![0u8; data.len()];
    assert_eq!(b.recv(&mut got), Ok(data.len()));
    assert_eq!(got, data);
}

/// Sends `data` from `a` to `b`, losing the first transmission of every
/// segment whose index (in MSS units from the first byte) is in `lose`.
/// Returns the bytes retransmitted and whether a retransmission timeout
/// was needed.
fn transfer(a: &mut Tcb, b: &mut Tcb, data: &[u8], lose: &[u32]) -> (usize, bool) {
    assert_eq!(a.send(data), Ok(data.len()));
    let mut received = Vec::new();
    let mut lost: Vec<u32> = Vec::new();
    let (mut first, mut high, mut resent) = (None, 0u32, 0usize);
    let mut now = 10;
    while received.len() < data.len() {
        now += 1;
        assert!(now < 20_000, "transfer stalled with {} of {} bytes", received.len(), data.len());
        a.poll(now);
        for seg in a.output(now) {
            if seg.payload.is_empty() {
                continue;
            }
            let base = *first.get_or_insert(seg.seq);
            let end = seg.seq.wrapping_sub(base) + seg.payload.len() as u32;
            if end <= high {
                resent += seg.payload.len();
            }
            high = high.max(end);
            let index = seg.seq.wrapping_sub(base) / MSS as u32;
            if lose.contains(&index) && !lost.contains(&index) {
                lost.push(index);
                continue;
            }
            deliver(&seg, A, B, b, now);
            for ack in b.output(now) {
                deliver(&ack, B, A, a, now);
            }
        }
        let mut buf = [0u8; 4096];
        while let Ok(n) = b.recv(&mut buf) {
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buf[..n]);
        }
        for ack in b.output(now) {
            deliver(&ack, B, A, a, now);
        }
    }
    assert_eq!(received, data);
    (resent, a.stats.retransmits > 0)
}

#[test]
fn fast_recovery_retransmits_only_the_holes() {
    let (mut a, mut b) = pair(true);
    let data = pattern(40 * MSS);
    // Two losses in one window.
    let (resent, timed_out) = transfer(&mut a, &mut b, &data, &[1, 4]);
    assert!(!timed_out, "recovered without a retransmission timeout");
    assert_eq!(a.stats.fast_retransmits, 1);
    assert_eq!(resent, 2 * MSS, "exactly the two lost segments were sent again");
}

#[test]
fn without_sack_the_transfer_still_completes_and_no_option_is_sent() {
    let (mut a, mut b) = pair(false);
    let data = pattern(40 * MSS);
    assert_eq!(a.send(&data[..6 * MSS]), Ok(6 * MSS));
    let segs = a.output(2);
    deliver(&segs[2], A, B, &mut b, 3);
    let ack = b.output(3).remove(0);
    assert!(ack.sack.is_empty(), "no SACK option for a peer that did not permit it");
    for i in [0, 1, 3, 4, 5] {
        deliver(&segs[i], A, B, &mut b, 3);
    }
    for ack in b.output(3) {
        deliver(&ack, B, A, &mut a, 3);
    }
    let mut got = vec![0u8; 6 * MSS];
    assert_eq!(b.recv(&mut got), Ok(6 * MSS));
    let (resent_plain, _) = transfer(&mut a, &mut b, &data[6 * MSS..], &[1, 4]);

    let (mut c, mut d) = pair(true);
    let (resent_sack, _) = transfer(&mut c, &mut d, &data[6 * MSS..], &[1, 4]);
    assert!(resent_sack <= resent_plain, "SACK never sends more again: {resent_sack} against {resent_plain}");
}

#[test]
fn blocks_outside_the_sent_data_are_ignored() {
    let (mut a, mut b) = pair(true);
    let data = pattern(4 * MSS);
    assert_eq!(a.send(&data), Ok(data.len()));
    let segs = a.output(2);
    let first = segs[0].seq;
    // An ACK that reports data never sent, a reversed block and one below
    // the cumulative ACK: none may make the sender skip real data.
    let mut sack = SackBlocks::NONE;
    sack.push(first + 100 * MSS as u32, first + 101 * MSS as u32);
    sack.push(first + 2 * MSS as u32, first + MSS as u32);
    sack.push(first.wrapping_sub(5000), first.wrapping_sub(4000));
    // A real duplicate ACK from the receiver, with its block replaced.
    deliver(&segs[1], A, B, &mut b, 3);
    let mut bogus = b.output(3).remove(0);
    assert_eq!((bogus.ack, bogus.sack.as_slice().len()), (first, 1));
    bogus.sack = sack;
    // The first copy also carries the receiver's first scaled window (a
    // window update, not a duplicate); the next three are duplicates.
    for _ in 0..4 {
        deliver(&bogus, B, A, &mut a, 3);
    }
    // Fast retransmit starts at the first byte.
    let again = a.output(4);
    assert_eq!(again.first().map(|s| s.seq), Some(first));
    for seg in segs.iter().chain(&again) {
        deliver(seg, A, B, &mut b, 5);
    }
    let mut got = vec![0u8; data.len()];
    assert_eq!(b.recv(&mut got), Ok(data.len()));
    assert_eq!(got, data);
}

#[test]
fn a_data_segment_with_sack_blocks_still_fits_the_mss() {
    let (mut a, mut b) = pair(true);
    let data = pattern(4 * MSS);
    assert_eq!(a.send(&data), Ok(data.len()));
    let segs = a.output(2);
    // The receiver holds one out-of-order segment and has data of its own.
    deliver(&segs[2], A, B, &mut b, 3);
    assert_eq!(b.send(&pattern(3 * MSS)), Ok(3 * MSS));
    let out = b.output(3);
    assert!(out.iter().any(|s| !s.payload.is_empty()));
    for seg in &out {
        assert_eq!(seg.sack.as_slice().len(), 1, "every segment reports the held data");
        let on_wire = seg.encode(B, A).len() - 20;
        assert!(on_wire <= MSS, "options and payload take {on_wire} bytes of a {MSS}-byte MSS");
    }
}

#[test]
fn a_loss_in_a_short_flight_is_repaired_without_a_timeout() {
    let (mut a, mut b) = pair(true);
    // Three segments, the first lost: only two duplicate ACKs can come.
    let data = pattern(3 * MSS);
    let (resent, timed_out) = transfer(&mut a, &mut b, &data, &[0]);
    assert!(!timed_out, "early retransmit, not the retransmission timer");
    assert_eq!(resent, MSS);
}
