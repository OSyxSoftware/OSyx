//! ARP for IPv4 over Ethernet (RFC 826) and the neighbour cache.
//!
//! The cache keeps resolved entries for [`REACHABLE_MS`]; an unresolved
//! address gets a pending entry that holds at most [`MAX_PENDING`] packets
//! and is retried [`MAX_PROBES`] times, [`RETRY_MS`] apart, before the
//! packets are dropped and the address is reported unreachable. The table
//! holds at most [`MAX_ENTRIES`] addresses; the oldest entry makes room.

use crate::wire::{be16, ip_at, put16, Ipv4Addr, MacAddr};
use alloc::vec::Vec;

pub const PACKET_LEN: usize = 28;
pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;

pub const REACHABLE_MS: u64 = 60_000;
pub const RETRY_MS: u64 = 1_000;
pub const MAX_PROBES: u32 = 3;
pub const MAX_PENDING: usize = 16;
pub const MAX_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub op: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArpError {
    Truncated,
    /// Not Ethernet/IPv4 (hardware type 1, protocol 0x0800, lengths 6/4).
    Unsupported,
    BadOperation,
}

impl Packet {
    pub fn parse(b: &[u8]) -> Result<Packet, ArpError> {
        if b.len() < PACKET_LEN {
            return Err(ArpError::Truncated);
        }
        if be16(b, 0) != 1 || be16(b, 2) != 0x0800 || b[4] != 6 || b[5] != 4 {
            return Err(ArpError::Unsupported);
        }
        let op = be16(b, 6);
        if op != OP_REQUEST && op != OP_REPLY {
            return Err(ArpError::BadOperation);
        }
        let mac = |o: usize| {
            let mut m = [0; 6];
            m.copy_from_slice(&b[o..o + 6]);
            MacAddr(m)
        };
        Ok(Packet { op, sender_mac: mac(8), sender_ip: ip_at(b, 14), target_mac: mac(18), target_ip: ip_at(b, 24) })
    }

    pub fn emit(&self) -> [u8; PACKET_LEN] {
        let mut b = [0u8; PACKET_LEN];
        put16(&mut b, 0, 1);
        put16(&mut b, 2, 0x0800);
        b[4] = 6;
        b[5] = 4;
        put16(&mut b, 6, self.op);
        b[8..14].copy_from_slice(&self.sender_mac.0);
        b[14..18].copy_from_slice(&self.sender_ip.0);
        b[18..24].copy_from_slice(&self.target_mac.0);
        b[24..28].copy_from_slice(&self.target_ip.0);
        b
    }
}

#[derive(Clone, Debug)]
enum State {
    Resolved { mac: MacAddr, expires: u64 },
    /// Waiting for a reply: packets to send once resolved.
    Pending { probes: u32, next_probe: u64, queued: Vec<Vec<u8>> },
}

#[derive(Clone, Debug)]
struct Entry {
    ip: Ipv4Addr,
    state: State,
    created: u64,
}

/// What [`Cache::poll`] asks the interface to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Broadcast a request for this address.
    Probe(Ipv4Addr),
    /// Resolution failed; this many packets were dropped.
    Unreachable(Ipv4Addr, usize),
}

#[derive(Default)]
pub struct Cache {
    entries: Vec<Entry>,
    /// Replies that changed an existing mapping (possible spoofing or a
    /// moved address); kept for diagnostics.
    pub conflicts: u64,
    pub dropped: u64,
}

/// Result of [`Cache::resolve`].
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Resolved: the packet comes back for sending.
    Found(MacAddr, Vec<u8>),
    /// The packet was queued; send a request now if `probe`.
    Queued { probe: bool },
    /// The pending queue is full: the packet was dropped.
    Dropped,
}

impl Cache {
    pub fn new() -> Cache {
        Cache::default()
    }

    pub fn get(&self, ip: Ipv4Addr, now: u64) -> Option<MacAddr> {
        self.entries.iter().find(|e| e.ip == ip).and_then(|e| match e.state {
            State::Resolved { mac, expires } if expires > now => Some(mac),
            _ => None,
        })
    }

    fn make_room(&mut self) {
        if self.entries.len() >= MAX_ENTRIES {
            // Prefer evicting a resolved entry; pending ones carry packets.
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.state, State::Resolved { .. }))
                .min_by_key(|(_, e)| e.created)
                .or_else(|| self.entries.iter().enumerate().min_by_key(|(_, e)| e.created))
                .map(|(i, _)| i);
            if let Some(i) = victim {
                let e = self.entries.remove(i);
                if let State::Pending { queued, .. } = e.state {
                    self.dropped += queued.len() as u64;
                }
            }
        }
    }

    /// Looks `ip` up for sending `packet` (an IPv4 datagram). On a miss the
    /// datagram is queued until the address resolves.
    pub fn resolve(&mut self, ip: Ipv4Addr, packet: Vec<u8>, now: u64) -> Lookup {
        if let Some(mac) = self.get(ip, now) {
            return Lookup::Found(mac, packet);
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.ip == ip) {
            match &mut e.state {
                State::Pending { queued, .. } => {
                    if queued.len() >= MAX_PENDING {
                        self.dropped += 1;
                        return Lookup::Dropped;
                    }
                    queued.push(packet);
                    return Lookup::Queued { probe: false };
                }
                State::Resolved { .. } => {
                    // Expired: probe again.
                    e.state = State::Pending { probes: 1, next_probe: now + RETRY_MS, queued: alloc::vec![packet] };
                    return Lookup::Queued { probe: true };
                }
            }
        }
        self.make_room();
        self.entries.push(Entry {
            ip,
            state: State::Pending { probes: 1, next_probe: now + RETRY_MS, queued: alloc::vec![packet] },
            created: now,
        });
        Lookup::Queued { probe: true }
    }

    /// Records a mapping learnt from an ARP packet. `create` adds a new entry
    /// (only for packets addressed to us, RFC 826); otherwise only existing
    /// entries are refreshed. Returns packets that were waiting for it.
    pub fn learn(&mut self, ip: Ipv4Addr, mac: MacAddr, now: u64, create: bool) -> Vec<Vec<u8>> {
        if ip.is_unspecified() || ip.is_broadcast() || !mac.is_unicast() {
            return Vec::new();
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.ip == ip) {
            let old = core::mem::replace(&mut e.state, State::Resolved { mac, expires: now + REACHABLE_MS });
            return match old {
                State::Pending { queued, .. } => queued,
                State::Resolved { mac: before, .. } => {
                    if before != mac {
                        self.conflicts += 1;
                    }
                    Vec::new()
                }
            };
        }
        if create {
            self.make_room();
            self.entries.push(Entry { ip, state: State::Resolved { mac, expires: now + REACHABLE_MS }, created: now });
        }
        Vec::new()
    }

    /// Retries pending lookups and expires old entries.
    pub fn poll(&mut self, now: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut dropped = 0u64;
        self.entries.retain_mut(|e| match &mut e.state {
            State::Resolved { expires, .. } => *expires > now,
            State::Pending { probes, next_probe, queued } => {
                if now < *next_probe {
                    return true;
                }
                if *probes >= MAX_PROBES {
                    dropped += queued.len() as u64;
                    actions.push(Action::Unreachable(e.ip, queued.len()));
                    return false;
                }
                *probes += 1;
                *next_probe = now + RETRY_MS;
                actions.push(Action::Probe(e.ip));
                true
            }
        });
        self.dropped += dropped;
        actions
    }

    /// Earliest time at which [`Cache::poll`] has work.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries
            .iter()
            .map(|e| match e.state {
                State::Resolved { expires, .. } => expires,
                State::Pending { next_probe, .. } => next_probe,
            })
            .min()
    }

    /// (address, mac or `None` while pending, ms until expiry) for listings.
    pub fn entries(&self, now: u64) -> Vec<(Ipv4Addr, Option<MacAddr>, u64)> {
        self.entries
            .iter()
            .map(|e| match e.state {
                State::Resolved { mac, expires } => (e.ip, Some(mac), expires.saturating_sub(now)),
                State::Pending { next_probe, .. } => (e.ip, None, next_probe.saturating_sub(now)),
            })
            .collect()
    }

    pub fn flush(&mut self) {
        self.entries.clear();
    }
}
