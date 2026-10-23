//! A simulated Ethernet segment between two stacks, with latency and
//! optional loss, duplication and reordering.

#![allow(dead_code)]

use osyx_net::stack::Stack;
use osyx_net::wire::{Ipv4Addr, MacAddr};

pub const A_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
pub const B_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);
pub const A_MAC: MacAddr = MacAddr([0x52, 0x54, 0, 0, 0, 1]);
pub const B_MAC: MacAddr = MacAddr([0x52, 0x54, 0, 0, 0, 2]);

/// Decides per frame (towards B?, bytes) whether it gets through.
pub type Filter = Box<dyn FnMut(bool, &[u8]) -> bool>;

pub struct Net {
    pub a: Stack,
    pub b: Stack,
    pub now: u64,
    /// Per-mille chance a frame is lost / duplicated.
    pub loss: u32,
    pub dup: u32,
    /// Random extra delay up to this many ms (reorders frames).
    pub jitter: u64,
    pub latency: u64,
    rng: u64,
    in_flight: Vec<(u64, bool, Vec<u8>)>,
    pub delivered: u64,
    pub dropped: u64,
    /// Inspect (and possibly drop) frames: (to_b, frame) -> keep.
    pub filter: Option<Filter>,
}

impl Net {
    /// Two stacks on 10.0.0.0/24 (eth0 is interface 1 on both).
    pub fn new() -> Net {
        let mut n = Net::unconfigured();
        n.a.configure_static(1, A_IP, 24, None).unwrap();
        n.b.configure_static(1, B_IP, 24, None).unwrap();
        n
    }

    /// Two stacks with interfaces but no addresses.
    pub fn unconfigured() -> Net {
        let mut a = Stack::new(0x1234_5678);
        let mut b = Stack::new(0x8765_4321);
        a.add_ethernet("eth0", A_MAC, 1500);
        b.add_ethernet("eth0", B_MAC, 1500);
        Net {
            a,
            b,
            now: 1,
            loss: 0,
            dup: 0,
            jitter: 0,
            latency: 1,
            rng: 0xDEAD_BEEF,
            in_flight: Vec::new(),
            delivered: 0,
            dropped: 0,
            filter: None,
        }
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn collect(&mut self) {
        let from_a = self.a.take_tx();
        let from_b = self.b.take_tx();
        for (to_b, frames) in [(true, from_a), (false, from_b)] {
            for (_, f) in frames {
                if let Some(filter) = self.filter.as_mut() {
                    if !filter(to_b, &f) {
                        self.dropped += 1;
                        continue;
                    }
                }
                if self.loss > 0 && self.rand() % 1000 < self.loss as u64 {
                    self.dropped += 1;
                    continue;
                }
                let delay = self.latency + if self.jitter > 0 { self.rand() % (self.jitter + 1) } else { 0 };
                if self.dup > 0 && self.rand() % 1000 < self.dup as u64 {
                    self.in_flight.push((self.now + delay + 1, to_b, f.clone()));
                }
                self.in_flight.push((self.now + delay, to_b, f));
            }
        }
    }

    /// Advances time by 1 ms: delivers due frames and runs both stacks.
    pub fn step(&mut self) {
        self.now += 1;
        self.a.set_now(self.now);
        self.b.set_now(self.now);
        self.collect();
        let now = self.now;
        let mut due = Vec::new();
        self.in_flight.retain(|(t, to_b, f)| {
            if *t <= now {
                due.push((*to_b, f.clone()));
                false
            } else {
                true
            }
        });
        for (to_b, f) in due {
            self.delivered += 1;
            if to_b {
                self.b.receive(1, &f);
            } else {
                self.a.receive(1, &f);
            }
        }
        self.a.poll();
        self.b.poll();
        self.collect();
    }

    /// Steps until `done` or `max_ms` elapsed; returns whether it finished.
    pub fn run_until(&mut self, max_ms: u64, mut done: impl FnMut(&mut Net) -> bool) -> bool {
        let end = self.now + max_ms;
        while self.now < end {
            if done(self) {
                return true;
            }
            self.step();
        }
        done(self)
    }

    pub fn run(&mut self, ms: u64) {
        for _ in 0..ms {
            self.step();
        }
    }
}
