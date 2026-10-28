//! Futexes: wait on / wake a 32-bit word in user memory.
//!
//! The key is (process, user virtual address), except for a word in shared
//! memory: then it is (shared memory object, offset), so processes that map
//! the same object wait and wake on the same word. `wait` reads the word,
//! compares and registers the
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

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Private(Pid, u64),
    /// A shared memory object (its address) and the offset in it.
    Shared(usize, u64),
}

struct Waiter {
    pid: Pid,
    key: Key,
    tid: Tid,
}

fn key_of(pid: Pid, addr: u64) -> Key {
    match crate::process::shared_key(addr) {
        Some((obj, off)) => Key::Shared(obj, off),
        None => Key::Private(pid, addr),
    }
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
    // Looked up before interrupts go off: it takes the process table lock.
    let key = key_of(pid, addr);
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
        WAITERS.lock().push(Waiter { pid, key, tid });
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

/// Wakes up to `count` threads waiting on `addr` of the current process
/// (in any process, for a word in shared memory).
pub fn wake(addr: u64, count: u32) -> Result<u64, Errno> {
    check_addr(addr)?;
    Ok(wake_key(key_of(sched::current_pid(), addr), count))
}

/// Wakes up to `count` waiters on `key` (FIFO).
fn wake_key(key: Key, count: u32) -> u64 {
    let mut woken: Vec<Tid> = Vec::new();
    {
        let mut w = WAITERS.lock();
        let mut i = 0;
        while i < w.len() && woken.len() < count as usize {
            if w[i].key == key {
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

pub fn dump_waiters() {
    use core::fmt::Write;
    let waiters = WAITERS.lock();
    let mut line = alloc::string::String::new();
    for waiter in waiters.iter() {
        match waiter.key {
            Key::Private(_, addr) => write!(line, " {}/{}@{addr:#x}", waiter.pid, waiter.tid).ok(),
            Key::Shared(obj, off) => write!(line, " {}/{}@s{obj:#x}+{off:#x}", waiter.pid, waiter.tid).ok(),
        };
    }
    drop(waiters);
    if !line.is_empty() {
        crate::kprintln!("futexwait:{line}");
    }
}

/// Drops every registration of process `pid` (called when it is torn down).
pub fn forget_process(pid: Pid) {
    WAITERS.lock().retain(|w| w.pid != pid);
}
