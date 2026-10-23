//! UDP (RFC 768) datagram format.

use crate::ipv4::PROTO_UDP;
use crate::wire::{be16, put16, transport_checksum, Ipv4Addr};
use alloc::vec::Vec;

pub const HEADER_LEN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub src_port: u16,
    pub dst_port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpError {
    Truncated,
    BadLength,
    BadChecksum,
    ZeroPort,
}

/// Validates a datagram (checksum 0 means "not computed" and is accepted,
/// as RFC 768 allows for IPv4).
pub fn parse(src: Ipv4Addr, dst: Ipv4Addr, b: &[u8]) -> Result<(Header, &[u8]), UdpError> {
    if b.len() < HEADER_LEN {
        return Err(UdpError::Truncated);
    }
    let len = be16(b, 4) as usize;
    if len < HEADER_LEN || len > b.len() {
        return Err(UdpError::BadLength);
    }
    let b = &b[..len];
    if be16(b, 6) != 0 && transport_checksum(src, dst, PROTO_UDP, b) != 0 {
        return Err(UdpError::BadChecksum);
    }
    let h = Header { src_port: be16(b, 0), dst_port: be16(b, 2) };
    if h.dst_port == 0 {
        return Err(UdpError::ZeroPort);
    }
    Ok((h, &b[HEADER_LEN..]))
}

pub fn build(src: Ipv4Addr, dst: Ipv4Addr, src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let mut d = alloc::vec![0u8; HEADER_LEN + data.len()];
    put16(&mut d, 0, src_port);
    put16(&mut d, 2, dst_port);
    let len = d.len() as u16;
    put16(&mut d, 4, len);
    d[HEADER_LEN..].copy_from_slice(data);
    let mut c = transport_checksum(src, dst, PROTO_UDP, &d);
    if c == 0 {
        c = 0xFFFF; // 0 would mean "no checksum"
    }
    put16(&mut d, 6, c);
    d
}
