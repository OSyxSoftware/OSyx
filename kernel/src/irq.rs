//! Device interrupt vectors (message-signalled interrupts).
//!
//! Vectors `FIRST..=LAST` are handed out to drivers together with a
//! handler that runs in interrupt context: it must only take spinlocks,
//! never allocate and keep its work short (typically acknowledge the device
//! and wake the driver's thread). The dispatcher sends the local APIC EOI.

use crate::arch::apic;
use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU64, Ordering};

pub const FIRST: u8 = 0x50;
pub const LAST: u8 = 0xEF;
const COUNT: usize = (LAST - FIRST + 1) as usize;

#[derive(Clone, Copy)]
struct Handler {
    f: fn(u64),
    arg: u64,
}

static HANDLERS: IrqSpinLock<[Option<Handler>; COUNT]> = IrqSpinLock::new([None; COUNT]);
static COUNTS: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];

/// Reserves a vector for `f(arg)`; `None` if all are taken or no local APIC
/// can receive MSIs.
pub fn allocate(f: fn(u64), arg: u64) -> Option<u8> {
    if !apic::msi_capable() {
        return None;
    }
    let mut h = HANDLERS.lock();
    let i = h.iter().position(Option::is_none)?;
    h[i] = Some(Handler { f, arg });
    Some(FIRST + i as u8)
}

pub fn free(vector: u8) {
    if (FIRST..=LAST).contains(&vector) {
        HANDLERS.lock()[(vector - FIRST) as usize] = None;
    }
}

/// (vector, interrupts delivered) of every vector that has fired so far.
pub fn counts() -> alloc::vec::Vec<(u8, u64)> {
    COUNTS
        .iter()
        .enumerate()
        .map(|(i, c)| (FIRST + i as u8, c.load(Ordering::Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect()
}

/// Called from the interrupt entry for vectors `FIRST..=LAST`.
pub fn dispatch(vector: u8) {
    let i = (vector - FIRST) as usize;
    COUNTS[i].fetch_add(1, Ordering::Relaxed);
    let h = HANDLERS.lock()[i];
    if let Some(h) = h {
        (h.f)(h.arg);
    }
    apic::eoi();
    // A driver thread woken by the handler should not wait for the next
    // timer tick when the CPU was idle.
    crate::sched::preempt_if_idle();
}
