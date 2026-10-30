//! Futexes: wait on / wake a 32-bit word in user memory.
//!
//! The key is (process, user virtual address), except for a word in shared
//! memory: then it is (shared memory object, offset), so processes that map
//! the same object wait and wake on the same word. `wait` reads the word,
//! compares and registers the
//! caller, then rechecks the word after registration so a concurrent wake
//! on another CPU cannot be lost. Waiters are woken in FIFO order.
//! A waiter that is still registered when it resumes was woken by its
//! timeout or by a kill request, not by `wake`.
//!
//! Priority inheritance: a waiter may name the thread that holds the lock
//! the word guards ([`wait_owned`]). While it waits, that thread runs at
//! least at the waiter's priority, so a low-priority holder preempted by
//! medium-priority work cannot keep a high-priority waiter blocked.

use crate::arch::cpu;
use crate::fixup::{copy_from_user, user_range_ok};
use crate::sched::{self, Tid};
use crate::sync::IrqSpinLock;
use crate::{interrupts, process::Pid};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
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
    /// Thread holding the lock (0: not named), and the priority this waiter
    /// lends it.
    owner: Tid,
    lends: u8,
}

fn key_of(pid: Pid, addr: u64) -> Key {
    match crate::process::shared_key(addr) {
        Some((obj, off)) => Key::Shared(obj, off),
        None => Key::Private(pid, addr),
    }
}

static WAITERS: IrqSpinLock<Vec<Waiter>> = IrqSpinLock::new(Vec::new());
// The sequence catches a wake between the initial read and registration,
// including when the word did not change. Sharding limits spurious retries
// caused by unrelated futexes; collisions remain safe because the ABI
// explicitly permits spurious returns.
const WAKE_BUCKETS: usize = 1024;
static WAKE_SEQUENCES: [AtomicU64; WAKE_BUCKETS] = [const { AtomicU64::new(0) }; WAKE_BUCKETS];
static SEQUENCE_RETRIES: AtomicU64 = AtomicU64::new(0);

fn wake_sequence(key: Key) -> &'static AtomicU64 {
    let mut x = match key {
        Key::Private(pid, addr) => addr.rotate_left(17) ^ pid,
        Key::Shared(obj, off) => (obj as u64).rotate_left(17) ^ off ^ 0x9e37_79b9_7f4a_7c15,
    };
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    &WAKE_SEQUENCES[x as usize % WAKE_BUCKETS]
}

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
    wait_owned(addr, expected, timeout_ticks, 0)
}

/// Like [`wait`]; `owner` (0: unknown) is the thread of the caller's process
/// that holds the lock, and inherits the caller's priority while it waits.
pub fn wait_owned(addr: u64, expected: u32, timeout_ticks: Option<u64>, owner: Tid) -> Result<(), Errno> {
    check_addr(addr)?;
    let pid = sched::current_pid();
    let tid = sched::current_tid();
    let owner = if owner == tid { 0 } else { owner };
    let lends = if owner != 0 { sched::effective_priority(tid) } else { 0 };
    let result = wait_registered(addr, expected, timeout_ticks, pid, tid, owner, lends);
    if owner != 0 {
        lend_to(owner, pid);
    }
    result
}

/// Recomputes the priority `owner` inherits from the waiters still naming it.
fn lend_to(owner: Tid, pid: Pid) {
    let level = WAITERS.lock().iter().filter(|w| w.owner == owner).map(|w| w.lends).max().unwrap_or(0);
    sched::set_inherited(owner, pid, level);
}

#[allow(clippy::too_many_arguments)]
fn wait_registered(addr: u64, expected: u32, timeout_ticks: Option<u64>, pid: Pid, tid: Tid, owner: Tid, lends: u8) -> Result<(), Errno> {
    // Looked up before interrupts go off: it takes the process table lock.
    let key = key_of(pid, addr);
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    let result = (|| {
        let wake_seq = wake_sequence(key);
        let sequence = wake_seq.load(Ordering::Acquire);
        let mut word = [0u8; 4];
        copy_from_user(&mut word, addr)?;
        if u32::from_le_bytes(word) != expected {
            return Err(Errno::WouldBlock);
        }
        if sched::current_kill_pending() {
            return Err(Errno::Interrupted);
        }
        WAITERS.lock().push(Waiter { pid, key, tid, owner, lends });
        // A different CPU may have changed the word and issued its wake
        // between our first read and registration. Recheck without holding
        // WAITERS: a user-memory fault may need the outer process-table lock.
        let rechecked = copy_from_user(&mut word, addr);
        if rechecked.is_err() || u32::from_le_bytes(word) != expected {
            let mut waiters = WAITERS.lock();
            if let Some(i) = waiters.iter().position(|x| x.tid == tid) {
                waiters.remove(i);
            }
            return rechecked.and(Err(Errno::WouldBlock));
        }
        if wake_seq.load(Ordering::Acquire) != sequence {
            SEQUENCE_RETRIES.fetch_add(1, Ordering::Relaxed);
            let mut waiters = WAITERS.lock();
            if let Some(i) = waiters.iter().position(|x| x.tid == tid) {
                waiters.remove(i);
            }
            return Ok(());
        }
        let deadline = timeout_ticks.map(|t| interrupts::ticks().saturating_add(t.max(1)));
        if owner != 0 {
            // Before blocking: the scheduler lock is innermost, so this is
            // safe with interrupts off.
            lend_to(owner, pid);
        }
        loop {
            sched::mark_current_blocked_until(deadline);
            sched::schedule();
            let mut w = WAITERS.lock();
            let Some(i) = w.iter().position(|x| x.tid == tid) else { return Ok(()); };
            // A stale scheduler wake can leave this waiter registered. It is
            // not a timeout, especially for an infinite wait: block again.
            if sched::current_kill_pending() || deadline.is_some_and(|d| interrupts::ticks() >= d) {
                w.remove(i);
                drop(w);
                return Err(if sched::current_kill_pending() { Errno::Interrupted } else { Errno::TimedOut });
            }
            drop(w);
        }
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
    // Owners the woken waiters lent priority to: the lock was just released,
    // so what they lent ends now, not when each waiter gets to run.
    let mut owners: Vec<(Tid, Pid)> = Vec::new();
    {
        let mut w = WAITERS.lock();
        wake_sequence(key).fetch_add(1, Ordering::Release);
        let mut i = 0;
        while i < w.len() && woken.len() < count as usize {
            if w[i].key == key {
                let x = w.remove(i);
                if x.owner != 0 && !owners.contains(&(x.owner, x.pid)) {
                    owners.push((x.owner, x.pid));
                }
                woken.push(x.tid);
            } else {
                i += 1;
            }
        }
    }
    for &(owner, pid) in &owners {
        lend_to(owner, pid);
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
    crate::kprintln!("futex: wake-sequence retries={}", SEQUENCE_RETRIES.load(Ordering::Relaxed));
}

/// Drops every registration of process `pid` (called when it is torn down).
pub fn forget_process(pid: Pid) {
    WAITERS.lock().retain(|w| w.pid != pid);
}
