//! Ring bookkeeping: producer rings (command and transfer rings) and the
//! consumer side of an event ring (xHCI 4.9).
//!
//! The rings here are single segments: `len` TRB slots, the last of which a
//! producer ring uses for a Link TRB back to the start (with Toggle Cycle).
//! The memory itself is reached through [`TrbMemory`], so the same logic
//! runs on DMA memory in the kernel and on a plain array in host tests.
//!
//! Ownership is decided by the cycle bit: a producer writes TRBs with its
//! current cycle state and flips that state each time it passes the Link
//! TRB; the consumer owns a TRB whose cycle bit equals its own cycle state.

use super::trb::{Trb, CHAIN, CYCLE};

/// Storage of a ring segment.
pub trait TrbMemory {
    /// Number of TRB slots.
    fn slots(&self) -> usize;
    /// Physical (bus) address of slot 0.
    fn phys(&self) -> u64;
    fn read(&self, index: usize) -> Trb;
    /// Stores `trb` at `index`. Implementations for device memory must make
    /// the first three dwords visible before the control dword (which holds
    /// the cycle bit that hands the TRB to the controller).
    fn write(&mut self, index: usize, trb: Trb);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// Not enough free slots for the request.
    Full,
    /// A TD with no TRBs, or more than the ring can ever hold.
    BadRequest,
    /// A completion pointed outside this ring.
    ForeignPointer,
}

/// Producer side of a command or transfer ring.
#[derive(Clone, Debug)]
pub struct ProducerRing {
    base: u64,
    /// Slots including the Link TRB.
    len: usize,
    enqueue: usize,
    /// Oldest slot the controller may still own.
    dequeue: usize,
    cycle: bool,
}

impl ProducerRing {
    /// A ring over `mem` (at least 4 slots). The memory must be zeroed: a
    /// zero cycle bit everywhere means "owned by software" for the initial
    /// producer cycle state 1.
    pub fn new(mem: &impl TrbMemory) -> ProducerRing {
        assert!(mem.slots() >= 4, "ring too small");
        ProducerRing { base: mem.phys(), len: mem.slots(), enqueue: 0, dequeue: 0, cycle: true }
    }

    /// Usable slots (the last one holds the Link TRB).
    pub fn capacity(&self) -> usize {
        self.len - 1
    }

    /// Slots between dequeue and enqueue (in use by the controller).
    pub fn occupied(&self) -> usize {
        (self.enqueue + self.capacity() - self.dequeue) % self.capacity()
    }

    /// Slots that can still be filled. One slot always stays empty so that
    /// "full" and "empty" are distinguishable.
    pub fn free(&self) -> usize {
        self.capacity() - 1 - self.occupied()
    }

    /// Physical address and cycle state for CRCR / an endpoint's TR
    /// Dequeue Pointer.
    pub fn start(&self) -> (u64, bool) {
        (self.slot_phys(self.enqueue), self.cycle)
    }

    pub fn cycle(&self) -> bool {
        self.cycle
    }

    pub fn slot_phys(&self, index: usize) -> u64 {
        self.base + 16 * index as u64
    }

    /// Index of the TRB at physical address `phys`, if it lies in this ring.
    pub fn index_of(&self, phys: u64) -> Option<usize> {
        let off = phys.checked_sub(self.base)?;
        (off % 16 == 0 && off / 16 < self.len as u64).then_some((off / 16) as usize)
    }

    /// Queues one Transfer Descriptor (or one command): the TRBs are chained
    /// (`CHAIN` on all but the last) and handed to the controller. Returns
    /// the physical address of the last TRB, which completion events refer to.
    pub fn push_td(&mut self, mem: &mut impl TrbMemory, trbs: &[Trb]) -> Result<u64, RingError> {
        if trbs.is_empty() || trbs.len() >= self.capacity() {
            return Err(RingError::BadRequest);
        }
        if trbs.len() > self.free() {
            return Err(RingError::Full);
        }
        // The first TRB is written with the wrong cycle bit and only handed
        // over after the rest of the TD, so a controller that is already
        // walking this ring never starts on a half-written TD.
        let (first_index, first_cycle) = (self.enqueue, self.cycle);
        let mut first = Trb::default();
        let mut last = 0;
        for (i, t) in trbs.iter().enumerate() {
            let chained = i + 1 < trbs.len();
            let t = if chained { t.with_flags(CHAIN) } else { t.without_flags(CHAIN) };
            last = self.slot_phys(self.enqueue);
            if i == 0 {
                first = t.with_cycle(self.cycle);
                mem.write(self.enqueue, t.with_cycle(!self.cycle));
            } else {
                mem.write(self.enqueue, t.with_cycle(self.cycle));
            }
            self.enqueue += 1;
            if self.enqueue == self.capacity() {
                // The Link TRB continues the chain if the TD does.
                let mut link = Trb::link(self.base, true).with_cycle(self.cycle);
                if chained {
                    link = link.with_flags(CHAIN);
                }
                mem.write(self.enqueue, link);
                self.enqueue = 0;
                self.cycle = !self.cycle;
            }
        }
        debug_assert_eq!(first.cycle(), first_cycle);
        mem.write(first_index, first);
        Ok(last)
    }

    /// The controller has completed the TRB at `phys` (and everything
    /// before it): those slots are free again.
    pub fn retire(&mut self, phys: u64) -> Result<(), RingError> {
        let i = self.index_of(phys).ok_or(RingError::ForeignPointer)?;
        if i >= self.capacity() {
            return Err(RingError::ForeignPointer);
        }
        self.dequeue = (i + 1) % self.capacity();
        Ok(())
    }

    /// Forgets everything in flight (after the endpoint was reset and its
    /// dequeue pointer moved to [`ProducerRing::start`]).
    pub fn reset_dequeue(&mut self) {
        self.dequeue = self.enqueue;
    }
}

/// Consumer side of a single-segment event ring.
#[derive(Clone, Debug)]
pub struct EventRing {
    base: u64,
    len: usize,
    dequeue: usize,
    cycle: bool,
}

impl EventRing {
    /// The consumer cycle state starts at 1; the controller writes events
    /// with cycle 1 on its first pass over zeroed memory.
    pub fn new(mem: &impl TrbMemory) -> EventRing {
        assert!(mem.slots() >= 16, "event ring segments hold at least 16 TRBs");
        EventRing { base: mem.phys(), len: mem.slots(), dequeue: 0, cycle: true }
    }

    /// The next event, if the controller has written one.
    pub fn pop(&mut self, mem: &impl TrbMemory) -> Option<Trb> {
        let t = mem.read(self.dequeue);
        if t.control() & CYCLE != u32::from(self.cycle) {
            return None;
        }
        self.dequeue += 1;
        if self.dequeue == self.len {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(t)
    }

    /// Value for ERDP: the address of the next slot the driver will read.
    pub fn dequeue_phys(&self) -> u64 {
        self.base + 16 * self.dequeue as u64
    }

    pub fn segment_len(&self) -> usize {
        self.len
    }
}

/// One Event Ring Segment Table entry (xHCI 6.5).
pub fn erst_entry(segment_phys: u64, trbs: u16) -> [u32; 4] {
    [segment_phys as u32, (segment_phys >> 32) as u32, trbs as u32, 0]
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::xhci::trb::TrbType;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Ring memory on the host.
    pub struct VecMem {
        pub trbs: Vec<Trb>,
        pub phys: u64,
    }

    impl VecMem {
        pub fn new(slots: usize, phys: u64) -> VecMem {
            VecMem { trbs: vec![Trb::default(); slots], phys }
        }
    }

    impl TrbMemory for VecMem {
        fn slots(&self) -> usize {
            self.trbs.len()
        }
        fn phys(&self) -> u64 {
            self.phys
        }
        fn read(&self, i: usize) -> Trb {
            self.trbs[i]
        }
        fn write(&mut self, i: usize, t: Trb) {
            self.trbs[i] = t;
        }
    }

    /// A model controller consuming a producer ring: follows the cycle bit
    /// and Link TRBs exactly as xHCI 4.9.2 describes.
    struct Consumer {
        index: usize,
        cycle: bool,
    }

    impl Consumer {
        fn next(&mut self, mem: &VecMem) -> Option<(u64, Trb)> {
            loop {
                let t = mem.trbs[self.index];
                if t.cycle() != self.cycle {
                    return None;
                }
                if t.trb_type() == Some(TrbType::Link) {
                    self.index = ((t.param() - mem.phys) / 16) as usize;
                    if t.control() & super::super::trb::TOGGLE_CYCLE != 0 {
                        self.cycle = !self.cycle;
                    }
                    continue;
                }
                let phys = mem.phys + 16 * self.index as u64;
                self.index += 1;
                return Some((phys, t));
            }
        }
    }

    #[test]
    fn producer_wraps_with_link_and_cycle_toggle() {
        let mut mem = VecMem::new(8, 0x10_0000);
        let mut ring = ProducerRing::new(&mem);
        let mut hc = Consumer { index: 0, cycle: true };
        assert_eq!(ring.capacity(), 7);
        assert_eq!(ring.start(), (0x10_0000, true));
        // Push and consume 40 single-TRB TDs: the ring wraps five times.
        for n in 0..40u64 {
            let at = ring.push_td(&mut mem, &[Trb::normal(n, 1, true)]).unwrap();
            let (phys, t) = hc.next(&mem).expect("controller sees the TRB");
            assert_eq!(phys, at);
            assert_eq!(t.param(), n);
            assert!(hc.next(&mem).is_none(), "nothing beyond the enqueue pointer");
            ring.retire(phys).unwrap();
            assert_eq!(ring.occupied(), 0);
        }
        // The link TRB sits in the last slot and toggles the cycle.
        let link = mem.trbs[7];
        assert_eq!(link.trb_type(), Some(TrbType::Link));
        assert_eq!(link.param(), 0x10_0000);
        assert_ne!(link.control() & super::super::trb::TOGGLE_CYCLE, 0);
    }

    #[test]
    fn producer_refuses_to_overrun_the_consumer() {
        let mut mem = VecMem::new(8, 0x2000);
        let mut ring = ProducerRing::new(&mem);
        for _ in 0..6 {
            ring.push_td(&mut mem, &[Trb::noop_command()]).unwrap();
        }
        assert_eq!(ring.free(), 0);
        assert_eq!(ring.push_td(&mut mem, &[Trb::noop_command()]), Err(RingError::Full));
        // Completing the third TRB frees three slots.
        ring.retire(0x2000 + 2 * 16).unwrap();
        assert_eq!(ring.free(), 3);
        assert_eq!(ring.push_td(&mut mem, &[Trb::noop_command(); 7]), Err(RingError::BadRequest));
        assert_eq!(ring.push_td(&mut mem, &[]), Err(RingError::BadRequest));
        assert_eq!(ring.retire(0x9999), Err(RingError::ForeignPointer));
        assert_eq!(ring.retire(0x2000 + 7 * 16), Err(RingError::ForeignPointer), "the link slot is never completed");
    }

    #[test]
    fn chained_td_crossing_the_link_keeps_the_chain() {
        let mut mem = VecMem::new(6, 0x4000);
        let mut ring = ProducerRing::new(&mem);
        let mut hc = Consumer { index: 0, cycle: true };
        // Fill 3 slots, consume them, then push a 3-TRB TD that spans the link.
        let last = ring.push_td(&mut mem, &[Trb::normal(1, 1, false); 3]).unwrap();
        for _ in 0..3 {
            hc.next(&mem).unwrap();
        }
        ring.retire(last).unwrap();
        let tds = [Trb::normal(10, 1, false), Trb::normal(11, 1, false), Trb::normal(12, 1, true)];
        let end = ring.push_td(&mut mem, &tds).unwrap();
        assert!(mem.trbs[3].has_chain());
        assert!(mem.trbs[4].has_chain());
        assert!(mem.trbs[5].has_chain(), "link TRB inside a TD carries CHAIN");
        assert!(!mem.trbs[0].has_chain(), "last TRB of the TD ends the chain");
        assert_eq!(end, 0x4000);
        let got: Vec<u64> = (0..3).map(|_| hc.next(&mem).unwrap().1.param()).collect();
        assert_eq!(got, [10, 11, 12]);
        assert!(!mem.trbs[0].cycle(), "second pass uses cycle 0");
    }

    #[test]
    fn first_trb_of_a_td_is_handed_over_last() {
        struct Recording {
            inner: VecMem,
            log: Vec<(usize, bool)>,
        }
        impl TrbMemory for Recording {
            fn slots(&self) -> usize {
                self.inner.slots()
            }
            fn phys(&self) -> u64 {
                self.inner.phys
            }
            fn read(&self, i: usize) -> Trb {
                self.inner.read(i)
            }
            fn write(&mut self, i: usize, t: Trb) {
                self.log.push((i, t.cycle()));
                self.inner.write(i, t);
            }
        }
        let mut mem = Recording { inner: VecMem::new(8, 0), log: Vec::new() };
        let mut ring = ProducerRing::new(&mem);
        ring.push_td(&mut mem, &[Trb::normal(0, 1, false), Trb::normal(1, 1, false), Trb::normal(2, 1, true)]).unwrap();
        // Slot 0 is written twice: first not owned by the controller, last owned.
        assert_eq!(mem.log, [(0, false), (1, true), (2, true), (0, true)]);
    }

    #[test]
    fn event_ring_consumes_by_cycle_and_wraps() {
        let mut mem = VecMem::new(16, 0x8000);
        let mut ev = EventRing::new(&mem);
        assert!(ev.pop(&mem).is_none(), "zeroed ring is empty");
        // Controller writes 16 events with cycle 1, then 3 with cycle 0.
        for i in 0..16 {
            mem.trbs[i] = Trb::new(i as u64, 1 << 24, (33 << 10) | 1);
        }
        for i in 0..16u64 {
            assert_eq!(ev.pop(&mem).unwrap().param(), i);
        }
        assert_eq!(ev.dequeue_phys(), 0x8000);
        assert!(ev.pop(&mem).is_none(), "old cycle-1 events are not consumed twice");
        for i in 0..3 {
            mem.trbs[i] = Trb::new(100 + i as u64, 0, 33 << 10);
        }
        assert_eq!(ev.pop(&mem).unwrap().param(), 100);
        assert_eq!(ev.pop(&mem).unwrap().param(), 101);
        assert_eq!(ev.pop(&mem).unwrap().param(), 102);
        assert!(ev.pop(&mem).is_none());
        assert_eq!(ev.dequeue_phys(), 0x8000 + 3 * 16);
    }

    #[test]
    fn erst_entry_layout() {
        assert_eq!(erst_entry(0x1_2345_6000, 256), [0x2345_6000, 1, 256, 0]);
    }
}
