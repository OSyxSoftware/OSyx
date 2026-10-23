//! Ethernet II framing.

use crate::wire::{be16, put16, MacAddr};

pub const HEADER_LEN: usize = 14;
/// Payload bytes of a standard frame.
pub const MTU: usize = 1500;
/// Smallest frame without the FCS; shorter frames are padded on transmit.
pub const MIN_FRAME: usize = 60;
pub const MAX_FRAME: usize = HEADER_LEN + MTU;

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_VLAN: u16 = 0x8100;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    /// The 16-bit field is a length (IEEE 802.3), not an EtherType.
    NotEthernetII,
    TooLong,
    /// A source address with the group bit set is never valid.
    BadSource,
}

/// Splits a received frame into header and payload. Trailing padding stays
/// in the payload; the upper layer trims it by its own length field.
pub fn parse(frame: &[u8]) -> Result<(Header, &[u8]), FrameError> {
    if frame.len() < HEADER_LEN {
        return Err(FrameError::Truncated);
    }
    // VLAN tags are allowed through the length check (4 extra bytes).
    if frame.len() > MAX_FRAME + 4 {
        return Err(FrameError::TooLong);
    }
    let mut dst = [0; 6];
    let mut src = [0; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    let ethertype = be16(frame, 12);
    if ethertype < 0x0600 {
        return Err(FrameError::NotEthernetII);
    }
    let h = Header { dst: MacAddr(dst), src: MacAddr(src), ethertype };
    if h.src.is_multicast() {
        return Err(FrameError::BadSource);
    }
    Ok((h, &frame[HEADER_LEN..]))
}

/// Builds a frame: header, payload, zero padding to the minimum size.
pub fn build(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> alloc::vec::Vec<u8> {
    let len = (HEADER_LEN + payload.len()).max(MIN_FRAME);
    let mut f = alloc::vec![0u8; len];
    f[0..6].copy_from_slice(&dst.0);
    f[6..12].copy_from_slice(&src.0);
    put16(&mut f, 12, ethertype);
    f[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    f
}

/// Receive filter: frames for us, broadcast, or (when enabled) multicast.
pub fn accepts(own: MacAddr, dst: MacAddr, multicast: bool) -> bool {
    dst == own || dst.is_broadcast() || (multicast && dst.is_multicast())
}
