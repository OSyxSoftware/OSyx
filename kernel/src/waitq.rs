//! Wait queues: threads blocked until a kernel object changes state.
//!
//! A waiter registers on one or more queues, marks itself blocked and
//! switches away, all with interrupts disabled, after re-checking the
//! condition; a waker changes the object's state and then wakes every
//! registered thread, which re-checks. On a single CPU with interrupts off
//! nothing can happen between the check and the block, so wake-ups are
//! never lost (SMP will need the check and the registration under the
//! object's lock).

use crate::arch::cpu;
use crate::sched::{self, Tid};
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use osyx_abi::Errno;

pub struct WaitQueue {
    waiters: IrqSpinLock<Vec<Tid>>,
}

impl WaitQueue {
    pub const fn new() -> WaitQueue {
        WaitQueue { waiters: IrqSpinLock::new(Vec::new()) }
    }

    fn register(&self, tid: Tid) {
        let mut w = self.waiters.lock();
        if !w.contains(&tid) {
            w.push(tid);
        }
    }

    fn unregister(&self, tid: Tid) {
        self.waiters.lock().retain(|&t| t != tid);
    }

    /// Wakes every waiter (they re-check their condition). Usable from
    /// interrupt handlers: it only takes spinlocks.
    pub fn wake_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for t in waiters {
            sched::wake(t);
        }
    }

}

impl Default for WaitQueue {
    fn default() -> Self {
        WaitQueue::new()
    }
}

/// Blocks until `ready` returns `Some`, waking whenever one of `queues` is
/// signalled. `deadline` is an absolute timer tick. `ready` runs with
/// interrupts disabled and must only take spinlocks.
pub fn block_on<T>(queues: &[&WaitQueue], deadline: Option<u64>, mut ready: impl FnMut() -> Option<T>) -> Result<T, Errno> {
    let tid = sched::current_tid();
    loop {
        let were_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        let outcome = if let Some(v) = ready() {
            Some(Ok(v))
        } else if sched::current_kill_pending() {
            Some(Err(Errno::Interrupted))
        } else if deadline.is_some_and(|d| crate::interrupts::ticks() >= d) {
            Some(Err(Errno::TimedOut))
        } else {
            for q in queues {
                q.register(tid);
            }
            sched::mark_current_blocked_until(deadline);
            sched::schedule();
            for q in queues {
                q.unregister(tid);
            }
            None
        };
        if were_enabled {
            cpu::enable_interrupts();
        }
        if let Some(r) = outcome {
            return r;
        }
    }
}

/// Timer tick at which a timeout of `ns` nanoseconds from now expires
/// (`None` = no timeout).
pub fn deadline_after_ns(ns: u64) -> Option<u64> {
    if ns == u64::MAX {
        return None;
    }
    let tick_ns = 1_000_000_000 / crate::arch::pic::TIMER_HZ as u64;
    Some(crate::interrupts::ticks().saturating_add(ns.div_ceil(tick_ns)))
}
