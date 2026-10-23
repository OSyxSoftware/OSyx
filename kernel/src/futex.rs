//! Futexes: wait on / wake a 32-bit word in user memory.
//!
//! The key is (process, user virtual address); there is no memory shared
//! between processes yet. `wait` reads the word, compares and registers the
//! caller in one interrupts-disabled section (single CPU), so a `wake` that
//! follows the user's store cannot be missed. Waiters are woken in FIFO order.
//! A waiter that is still registered when it resumes was woken by its
//! timeout or by a kill request, not by `wake`.

use crate::arch::cpu;
use crate::fixup::{copy_from_user, user_range_ok};
use crate::sched::{self, Tid};
use crate::sync::IrqSpinLock;
use crate::{interrupts, process::Pid};
use alloc::vec::Vec;
use osyx_abi::Errno;

struct Waiter {
    pid: Pid,
    addr: u64,
    tid: Tid,
}

static WAITERS: IrqSpinLock<Vec<Waiter>> = IrqSpinLock::new(Vec::new());

fn check_addr(addr: u64) -> Result<(), Errno> {
    if !addr.is_multiple_of(4) {
        return Err(Errno::InvalidArgument);
    }
    if !user_range_ok(addr, 4) {
        return Err(Errno::Fault);
    }
    Ok(())
}

/// Blocks while `*addr == expected`, for at most `timeout_ticks` (if given).
pub fn wait(addr: u64, expected: u32, timeout_ticks: Option<u64>) -> Result<(), Errno> {
    check_addr(addr)?;
    let pid = sched::current_pid();
    let tid = sched::current_tid();
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    let result = (|| {
        let mut word = [0u8; 4];
        copy_from_user(&mut word, addr)?;
        if u32::from_le_bytes(word) != expected {
            return Err(Errno::WouldBlock);
        }
        if sched::current_kill_pending() {
            return Err(Errno::Interrupted);
        }
        WAITERS.lock().push(Waiter { pid, addr, tid });
        sched::mark_current_blocked_until(timeout_ticks.map(|t| interrupts::ticks() + t.max(1)));
        sched::schedule();
        // Still registered => not woken by `wake`.
        let mut w = WAITERS.lock();
        if let Some(i) = w.iter().position(|x| x.tid == tid) {
            w.remove(i);
            drop(w);
            if sched::current_kill_pending() {
                return Err(Errno::Interrupted);
            }
            return Err(Errno::TimedOut);
        }
        Ok(())
    })();
    if were_enabled {
        cpu::enable_interrupts();
    }
    result
}

/// Wakes up to `count` threads of the current process waiting on `addr`.
pub fn wake(addr: u64, count: u32) -> Result<u64, Errno> {
    check_addr(addr)?;
    Ok(wake_in(sched::current_pid(), addr, count))
}

/// Wakes up to `count` waiters of process `pid` on `addr` (FIFO).
pub fn wake_in(pid: Pid, addr: u64, count: u32) -> u64 {
    let mut woken: Vec<Tid> = Vec::new();
    {
        let mut w = WAITERS.lock();
        let mut i = 0;
        while i < w.len() && woken.len() < count as usize {
            if w[i].pid == pid && w[i].addr == addr {
                woken.push(w.remove(i).tid);
            } else {
                i += 1;
            }
        }
    }
    for &t in &woken {
        sched::wake(t);
    }
    woken.len() as u64
}

/// Drops every registration of process `pid` (called when it is torn down).
pub fn forget_process(pid: Pid) {
    WAITERS.lock().retain(|w| w.pid != pid);
}
