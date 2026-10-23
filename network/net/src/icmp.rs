//! ICMPv4 (RFC 792): echo, destination unreachable and time exceeded.

use crate::wire::{be16, checksum, put16};
use alloc::vec::Vec;

pub const ECHO_REPLY: u8 = 0;
pub const DEST_UNREACHABLE: u8 = 3;
pub const ECHO_REQUEST: u8 = 8;
pub const TIME_EXCEEDED: u8 = 11;

/// Destination unreachable codes.
pub const UNREACH_NET: u8 = 0;
pub const UNREACH_HOST: u8 = 1;
pub const UNREACH_PROTOCOL: u8 = 2;
pub const UNREACH_PORT: u8 = 3;
pub const UNREACH_FRAG_NEEDED: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    EchoRequest { ident: u16, seq: u16, data: &'a [u8] },
    EchoReply { ident: u16, seq: u16, data: &'a [u8] },
    /// `original` is the IP header + first bytes of the offending datagram.
    Unreachable { code: u8, original: &'a [u8] },
    TimeExceeded { code: u8, original: &'a [u8] },
    Other { kind: u8, code: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcmpError {
    Truncated,
    BadChecksum,
}

pub fn parse(b: &[u8]) -> Result<Message<'_>, IcmpError> {
    if b.len() < 8 {
        return Err(IcmpError::Truncated);
    }
    if checksum(b) != 0 {
        return Err(IcmpError::BadChecksum);
    }
    let (kind, code) = (b[0], b[1]);
    Ok(match kind {
        ECHO_REQUEST => Message::EchoRequest { ident: be16(b, 4), seq: be16(b, 6), data: &b[8..] },
        ECHO_REPLY => Message::EchoReply { ident: be16(b, 4), seq: be16(b, 6), data: &b[8..] },
        DEST_UNREACHABLE => Message::Unreachable { code, original: &b[8..] },
        TIME_EXCEEDED => Message::TimeExceeded { code, original: &b[8..] },
        _ => Message::Other { kind, code },
    })
}

fn finish(mut m: Vec<u8>) -> Vec<u8> {
    let c = checksum(&m);
    put16(&mut m, 2, c);
    m
}

pub fn echo(kind: u8, ident: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let mut m = alloc::vec![0u8; 8 + data.len()];
    m[0] = kind;
    put16(&mut m, 4, ident);
    put16(&mut m, 6, seq);
    m[8..].copy_from_slice(data);
    finish(m)
}

/// An error message quoting `original` (IP header and at least 8 payload
/// bytes, RFC 792; we quote up to 64 bytes).
pub fn error(kind: u8, code: u8, original: &[u8]) -> Vec<u8> {
    let quote = original.len().min(64);
    let mut m = alloc::vec![0u8; 8 + quote];
    m[0] = kind;
    m[1] = code;
    m[8..].copy_from_slice(&original[..quote]);
    finish(m)
}

pub fn unreachable_reason(code: u8) -> &'static str {
    match code {
        UNREACH_NET => "network unreachable",
        UNREACH_HOST => "host unreachable",
        UNREACH_PROTOCOL => "protocol unreachable",
        UNREACH_PORT => "port unreachable",
        UNREACH_FRAG_NEEDED => "fragmentation needed",
        _ => "destination unreachable",
    }
}
