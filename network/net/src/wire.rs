//! Addresses, byte-order helpers and the Internet checksum (RFC 1071).

use core::fmt;

/// A 48-bit Ethernet address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xFF; 6]);
    pub const ZERO: MacAddr = MacAddr([0; 6]);

    pub fn is_broadcast(&self) -> bool {
        *self == Self::BROADCAST
    }

    /// Group bit: broadcast and multicast addresses.
    pub fn is_multicast(&self) -> bool {
        self.0[0] & 1 != 0
    }

    pub fn is_unicast(&self) -> bool {
        !self.is_multicast() && *self != Self::ZERO
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

impl fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// An IPv4 address in network byte order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0; 4]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255; 4]);
    pub const LOOPBACK: Ipv4Addr = Ipv4Addr([127, 0, 0, 1]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr([a, b, c, d])
    }

    pub const fn from_u32(v: u32) -> Ipv4Addr {
        Ipv4Addr(v.to_be_bytes())
    }

    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    pub fn is_unspecified(&self) -> bool {
        self.0 == [0; 4]
    }

    pub fn is_broadcast(&self) -> bool {
        self.0 == [255; 4]
    }

    pub fn is_loopback(&self) -> bool {
        self.0[0] == 127
    }

    pub fn is_multicast(&self) -> bool {
        self.0[0] & 0xF0 == 0xE0
    }

    /// Whether `self` and `other` share the first `prefix` bits.
    pub fn in_subnet(&self, other: Ipv4Addr, prefix: u8) -> bool {
        let mask = prefix_mask(prefix);
        self.to_u32() & mask == other.to_u32() & mask
    }

    /// Parses dotted-quad notation (exactly four decimal parts 0..=255).
    pub fn parse(s: &str) -> Option<Ipv4Addr> {
        let mut out = [0u8; 4];
        let mut parts = s.split('.');
        for b in out.iter_mut() {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let v: u16 = p.parse().ok()?;
            *b = u8::try_from(v).ok()?;
        }
        parts.next().is_none().then_some(Ipv4Addr(out))
    }
}

/// Netmask of a prefix length (`0..=32`).
pub const fn prefix_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    }
}

/// Prefix length of a contiguous netmask; `None` for a non-contiguous one.
pub fn mask_prefix(mask: Ipv4Addr) -> Option<u8> {
    let m = mask.to_u32();
    let ones = m.leading_ones() as u8;
    (prefix_mask(ones) == m).then_some(ones)
}

impl fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl fmt::Debug for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// An IPv4 address and port.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Endpoint {
    pub addr: Ipv4Addr,
    pub port: u16,
}

impl Endpoint {
    pub const fn new(addr: Ipv4Addr, port: u16) -> Endpoint {
        Endpoint { addr, port }
    }

    /// Parses `a.b.c.d:port`.
    pub fn parse(s: &str) -> Option<Endpoint> {
        let (a, p) = s.rsplit_once(':')?;
        Some(Endpoint { addr: Ipv4Addr::parse(a)?, port: p.parse().ok()? })
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.addr, self.port)
    }
}

pub fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

pub fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

pub fn put16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

pub fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

pub fn ip_at(b: &[u8], off: usize) -> Ipv4Addr {
    Ipv4Addr([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// One's-complement sum of `data` (16-bit big-endian words) added to `acc`.
pub fn sum(mut acc: u32, data: &[u8]) -> u32 {
    let (words, rest) = data.as_chunks::<2>();
    for c in words {
        acc += u16::from_be_bytes([c[0], c[1]]) as u32;
        // Fold early so long inputs cannot overflow.
        if acc > 0xFFFF_0000 {
            acc = (acc & 0xFFFF) + (acc >> 16);
        }
    }
    if let [last] = rest {
        acc += (*last as u32) << 8;
    }
    acc
}

/// Folds a running sum into the final 16-bit checksum.
pub fn fold(mut acc: u32) -> u16 {
    while acc >> 16 != 0 {
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    !(acc as u16)
}

/// Internet checksum of `data`.
pub fn checksum(data: &[u8]) -> u16 {
    fold(sum(0, data))
}

/// Running sum of the TCP/UDP pseudo header.
pub fn pseudo_header(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, len: usize) -> u32 {
    let mut acc = sum(0, &src.0);
    acc = sum(acc, &dst.0);
    acc + protocol as u32 + len as u32
}

/// Checksum of a transport segment including its pseudo header.
pub fn transport_checksum(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, segment: &[u8]) -> u16 {
    fold(sum(pseudo_header(src, dst, protocol, segment.len()), segment))
}
