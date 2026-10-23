//! IPv4 (RFC 791): header validation and generation, fragmentation and
//! bounded reassembly.

use crate::wire::{be16, checksum, ip_at, put16, Ipv4Addr};
use alloc::vec::Vec;

pub const MIN_HEADER: usize = 20;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;
pub const DEFAULT_TTL: u8 = 64;
pub const MAX_DATAGRAM: usize = 65_535;

const FLAG_DF: u16 = 0x4000;
const FLAG_MF: u16 = 0x2000;
const OFFSET_MASK: u16 = 0x1FFF;

/// Datagrams reassembled at once; a new one evicts the oldest.
pub const MAX_REASSEMBLY: usize = 8;
/// Fragments one datagram may consist of.
pub const MAX_FRAGMENTS: usize = 64;
/// Bytes buffered for all incomplete datagrams together.
pub const REASSEMBLY_BYTES: usize = 256 * 1024;
pub const REASSEMBLY_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub header_len: usize,
    pub total_len: usize,
    pub ident: u16,
    pub dont_fragment: bool,
    pub more_fragments: bool,
    /// Offset of this fragment's payload in bytes.
    pub fragment_offset: usize,
    pub ttl: u8,
    pub protocol: u8,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
}

impl Header {
    pub fn is_fragment(&self) -> bool {
        self.more_fragments || self.fragment_offset != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpError {
    Truncated,
    NotVersion4,
    BadHeaderLength,
    BadTotalLength,
    BadChecksum,
    /// Source address that can never send (multicast, broadcast).
    BadSource,
    /// Fragment beyond 64 KiB or misaligned.
    BadFragment,
}

/// Validates the header of `packet`; returns it and the payload (trimmed
/// to the total length, so link-layer padding is removed).
pub fn parse(packet: &[u8]) -> Result<(Header, &[u8]), IpError> {
    if packet.len() < MIN_HEADER {
        return Err(IpError::Truncated);
    }
    if packet[0] >> 4 != 4 {
        return Err(IpError::NotVersion4);
    }
    let header_len = (packet[0] & 0x0F) as usize * 4;
    if header_len < MIN_HEADER || header_len > packet.len() {
        return Err(IpError::BadHeaderLength);
    }
    let total_len = be16(packet, 2) as usize;
    if total_len < header_len || total_len > packet.len() {
        return Err(IpError::BadTotalLength);
    }
    if checksum(&packet[..header_len]) != 0 {
        return Err(IpError::BadChecksum);
    }
    let frag = be16(packet, 6);
    let h = Header {
        header_len,
        total_len,
        ident: be16(packet, 4),
        dont_fragment: frag & FLAG_DF != 0,
        more_fragments: frag & FLAG_MF != 0,
        fragment_offset: (frag & OFFSET_MASK) as usize * 8,
        ttl: packet[8],
        protocol: packet[9],
        src: ip_at(packet, 12),
        dst: ip_at(packet, 16),
    };
    if h.src.is_multicast() || h.src.is_broadcast() {
        return Err(IpError::BadSource);
    }
    let payload_len = total_len - header_len;
    if h.fragment_offset + payload_len > MAX_DATAGRAM || (h.more_fragments && !payload_len.is_multiple_of(8)) {
        return Err(IpError::BadFragment);
    }
    Ok((h, &packet[header_len..total_len]))
}

/// Parses the IP header quoted in an ICMP error: the datagram is cut
/// short, so only the header itself must be complete. Returns the header
/// and the quoted payload bytes.
pub fn parse_quoted(b: &[u8]) -> Result<(Header, &[u8]), IpError> {
    if b.len() < MIN_HEADER {
        return Err(IpError::Truncated);
    }
    if b[0] >> 4 != 4 {
        return Err(IpError::NotVersion4);
    }
    let header_len = (b[0] & 0x0F) as usize * 4;
    if header_len < MIN_HEADER || header_len > b.len() {
        return Err(IpError::BadHeaderLength);
    }
    let frag = be16(b, 6);
    let h = Header {
        header_len,
        total_len: be16(b, 2) as usize,
        ident: be16(b, 4),
        dont_fragment: frag & FLAG_DF != 0,
        more_fragments: frag & FLAG_MF != 0,
        fragment_offset: (frag & OFFSET_MASK) as usize * 8,
        ttl: b[8],
        protocol: b[9],
        src: ip_at(b, 12),
        dst: ip_at(b, 16),
    };
    Ok((h, &b[header_len..]))
}

/// Writes a 20-byte header (no options) for `payload_len` bytes.
#[allow(clippy::too_many_arguments)]
pub fn write_header(
    out: &mut [u8],
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    ttl: u8,
    ident: u16,
    payload_len: usize,
    offset: usize,
    more: bool,
    dont_fragment: bool,
) {
    out[0] = 0x45;
    out[1] = 0;
    put16(out, 2, (MIN_HEADER + payload_len) as u16);
    put16(out, 4, ident);
    let mut frag = (offset / 8) as u16 & OFFSET_MASK;
    if more {
        frag |= FLAG_MF;
    }
    if dont_fragment {
        frag |= FLAG_DF;
    }
    put16(out, 6, frag);
    out[8] = ttl;
    out[9] = protocol;
    put16(out, 10, 0);
    out[12..16].copy_from_slice(&src.0);
    out[16..20].copy_from_slice(&dst.0);
    let c = checksum(&out[..MIN_HEADER]);
    put16(out, 10, c);
}

/// Builds one or more datagrams carrying `payload`, fragmenting to `mtu`.
/// Returns `None` if the payload does not fit a datagram, or needs
/// fragmenting while `dont_fragment` is set.
#[allow(clippy::too_many_arguments)]
pub fn build(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    ttl: u8,
    ident: u16,
    payload: &[u8],
    mtu: usize,
    dont_fragment: bool,
) -> Option<Vec<Vec<u8>>> {
    if payload.len() + MIN_HEADER > MAX_DATAGRAM || mtu < 68 {
        return None;
    }
    if payload.len() + MIN_HEADER <= mtu {
        let mut p = alloc::vec![0u8; MIN_HEADER + payload.len()];
        write_header(&mut p, src, dst, protocol, ttl, ident, payload.len(), 0, false, dont_fragment);
        p[MIN_HEADER..].copy_from_slice(payload);
        return Some(alloc::vec![p]);
    }
    if dont_fragment {
        return None;
    }
    let chunk = (mtu - MIN_HEADER) & !7;
    let mut out = Vec::new();
    let mut off = 0;
    while off < payload.len() {
        let n = chunk.min(payload.len() - off);
        let more = off + n < payload.len();
        let mut p = alloc::vec![0u8; MIN_HEADER + n];
        write_header(&mut p, src, dst, protocol, ttl, ident, n, off, more, false);
        p[MIN_HEADER..].copy_from_slice(&payload[off..off + n]);
        out.push(p);
        off += n;
    }
    Some(out)
}

struct Partial {
    key: (Ipv4Addr, Ipv4Addr, u8, u16),
    /// (offset, bytes) without overlaps, sorted by offset.
    pieces: Vec<(usize, Vec<u8>)>,
    /// Total payload length once the last fragment has arrived.
    total: Option<usize>,
    bytes: usize,
    started: u64,
    header: Header,
}

/// Reassembles fragmented datagrams with hard limits on memory.
#[derive(Default)]
pub struct Reassembler {
    partials: Vec<Partial>,
    pub timeouts: u64,
    pub dropped: u64,
}

impl Reassembler {
    pub fn new() -> Reassembler {
        Reassembler::default()
    }

    fn buffered(&self) -> usize {
        self.partials.iter().map(|p| p.bytes).sum()
    }

    /// Adds a fragment; returns the complete datagram (header of the first
    /// fragment with fragment fields cleared, and the payload) when done.
    pub fn add(&mut self, h: &Header, payload: &[u8], now: u64) -> Option<(Header, Vec<u8>)> {
        let key = (h.src, h.dst, h.protocol, h.ident);
        let idx = match self.partials.iter().position(|p| p.key == key) {
            Some(i) => i,
            None => {
                if self.partials.len() >= MAX_REASSEMBLY {
                    let oldest = self.partials.iter().enumerate().min_by_key(|(_, p)| p.started).map(|(i, _)| i)?;
                    self.partials.remove(oldest);
                    self.dropped += 1;
                }
                self.partials.push(Partial { key, pieces: Vec::new(), total: None, bytes: 0, started: now, header: *h });
                self.partials.len() - 1
            }
        };
        if self.buffered() + payload.len() > REASSEMBLY_BYTES {
            self.partials.remove(idx);
            self.dropped += 1;
            return None;
        }
        let p = &mut self.partials[idx];
        let start = h.fragment_offset;
        let end = start + payload.len();
        if !h.more_fragments {
            if p.total.is_some_and(|t| t != end) || p.pieces.iter().any(|(o, b)| o + b.len() > end) {
                // Conflicting last fragments: discard the datagram.
                self.partials.remove(idx);
                self.dropped += 1;
                return None;
            }
            p.total = Some(end);
        } else if p.total.is_some_and(|t| end > t) {
            self.partials.remove(idx);
            self.dropped += 1;
            return None;
        }
        if start == 0 {
            p.header = *h;
        }
        // Keep only the bytes not covered yet (first copy wins).
        let mut cursor = start;
        let mut new_pieces = Vec::new();
        for &(o, ref b) in p.pieces.iter() {
            let (ps, pe) = (o, o + b.len());
            if pe <= cursor || ps >= end {
                continue;
            }
            if ps > cursor {
                new_pieces.push((cursor, payload[cursor - start..ps - start].to_vec()));
            }
            cursor = cursor.max(pe);
        }
        if cursor < end {
            new_pieces.push((cursor, payload[cursor - start..].to_vec()));
        }
        for np in new_pieces {
            p.bytes += np.1.len();
            p.pieces.push(np);
        }
        if p.pieces.len() > MAX_FRAGMENTS {
            self.partials.remove(idx);
            self.dropped += 1;
            return None;
        }
        p.pieces.sort_by_key(|(o, _)| *o);
        let total = p.total?;
        let mut next = 0;
        for (o, b) in &p.pieces {
            if *o != next {
                return None;
            }
            next += b.len();
        }
        if next != total {
            return None;
        }
        let p = self.partials.remove(idx);
        let mut data = Vec::with_capacity(total);
        for (_, b) in p.pieces {
            data.extend_from_slice(&b);
        }
        let mut header = p.header;
        header.more_fragments = false;
        header.fragment_offset = 0;
        header.total_len = header.header_len + total;
        Some((header, data))
    }

    /// Drops datagrams that did not complete in time; returns the expired
    /// ones' first-fragment headers (for ICMP time exceeded, if wanted).
    pub fn expire(&mut self, now: u64) -> usize {
        let before = self.partials.len();
        self.partials.retain(|p| now.saturating_sub(p.started) < REASSEMBLY_TIMEOUT_MS);
        let n = before - self.partials.len();
        self.timeouts += n as u64;
        n
    }

    pub fn pending(&self) -> usize {
        self.partials.len()
    }
}
