//! Sampling profiler, enabled by the `prof=<seconds>` boot option.
//!
//! Every timer tick on every CPU records where the CPU is: the interrupted
//! user RIP, or the kernel RIP with a frame-pointer walk of the kernel stack
//! (build the kernel with `-C force-frame-pointers=yes`, see `scripts/build.ps1`
//! `OSYX_FRAME_POINTERS`). CPU 0 also visits every other thread each
//! `OFF_CPU_PERIOD` ticks: a blocked thread is recorded with the kernel stack it
//! sleeps on and its last system call, a runnable one as waiting for a CPU. That
//! is the off-CPU half of the picture, without which a flame graph of a mostly
//! waiting program shows nothing.
//!
//! The interrupt handler only counts into fixed per-CPU hash tables allocated
//! at start-up: no locks, no allocation. After `<seconds>` of uptime a kernel
//! thread merges the tables and writes the heaviest stacks to the log as
//! `prof: ...` lines; `scripts/flamegraph.py` turns them into a flame graph.
//! Serial output is slow, so only the log lines of the final dump cost time.
use crate::sched;
use crate::smp::MAX_CPUS;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Frames kept per sample (leaf first).
pub const DEPTH: usize = 14;
/// Entries per CPU table; a power of two.
const SLOTS: usize = 1 << 14;
const PROBE: usize = 8;
/// Ticks between visits of the other threads (also their weight).
const OFF_CPU_PERIOD: u64 = 20;
/// Most distinct stacks written to the log.
const MAX_LINES: usize = 2000;
const SYS_SLOTS: usize = 1 << 13;

pub const USER: u32 = 0;
pub const KERNEL: u32 = 1;
pub const BLOCKED: u32 = 2;
pub const RUNNABLE: u32 = 3;

#[derive(Clone, Copy)]
#[repr(C)]
struct Entry {
    hash: u64,
    count: u64,
    pid: u32,
    tid: u32,
    kind: u32,
    sys: u32,
    depth: u32,
    frames: [u64; DEPTH],
}

impl Entry {
    const ZERO: Entry = Entry { hash: 0, count: 0, pid: 0, tid: 0, kind: 0, sys: 0, depth: 0, frames: [0; DEPTH] };
}

struct Table {
    entries: UnsafeCell<*mut Entry>,
    dropped: AtomicU64,
    samples: AtomicU64,
}

// SAFETY: a table is written only by its own CPU's timer interrupt (CPU 0's
// also takes the off-CPU samples), and read by the dump after sampling stopped.
unsafe impl Sync for Table {}

static TABLES: [Table; MAX_CPUS] =
    [const { Table { entries: UnsafeCell::new(core::ptr::null_mut()), dropped: AtomicU64::new(0), samples: AtomicU64::new(0) } }; MAX_CPUS];
static ENABLED: AtomicBool = AtomicBool::new(false);
static SYSCALL: [AtomicU64; SYS_SLOTS] = [const { AtomicU64::new(0) }; SYS_SLOTS];

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Remembers the last system call of the running thread (for sleeping threads).
#[inline]
pub fn note_syscall(tid: u64, nr: u64) {
    if enabled() {
        SYSCALL[tid as usize & (SYS_SLOTS - 1)].store(nr, Ordering::Relaxed);
    }
}

fn last_syscall(tid: u64) -> u32 {
    SYSCALL[tid as usize & (SYS_SLOTS - 1)].load(Ordering::Relaxed) as u32
}

fn hash_of(pid: u32, tid: u32, kind: u32, sys: u32, frames: &[u64]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ ((pid as u64) << 32 | tid as u64);
    h = (h ^ ((kind as u64) << 32 | sys as u64)).wrapping_mul(0x0100_0000_01b3);
    for &f in frames {
        h = (h ^ f).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 29;
    }
    h | 1
}

fn record(cpu: usize, pid: u32, tid: u32, kind: u32, sys: u32, frames: &[u64], weight: u64) {
    let t = &TABLES[cpu];
    // SAFETY: see `Table`; the pointer was set once by `init`.
    let base = unsafe { *t.entries.get() };
    if base.is_null() {
        return;
    }
    t.samples.fetch_add(weight, Ordering::Relaxed);
    let h = hash_of(pid, tid, kind, sys, frames);
    for p in 0..PROBE {
        let i = (h as usize).wrapping_add(p) & (SLOTS - 1);
        // SAFETY: `i < SLOTS`; only this CPU writes this table.
        let e = unsafe { &mut *base.add(i) };
        if e.count == 0 {
            *e = Entry { hash: h, count: weight, pid, tid, kind, sys, depth: frames.len() as u32, frames: [0; DEPTH] };
            e.frames[..frames.len()].copy_from_slice(frames);
            return;
        }
        if e.hash == h && e.pid == pid && e.tid == tid && e.kind == kind && e.sys == sys && e.depth as usize == frames.len() && e.frames[..frames.len()] == *frames {
            e.count += weight;
            return;
        }
    }
    t.dropped.fetch_add(weight, Ordering::Relaxed);
}

/// Return addresses along the frame-pointer chain starting at `rbp`.
fn walk(mut rbp: u64, lo: u64, hi: u64, out: &mut [u64; DEPTH], mut n: usize) -> usize {
    while n < DEPTH {
        if rbp < lo || rbp.saturating_add(16) > hi || rbp & 7 != 0 {
            break;
        }
        let (Ok(next), Ok(ret)) = (crate::fixup::probe_read_u64(rbp), crate::fixup::probe_read_u64(rbp + 8)) else { break };
        if ret < 0xffff_ffff_8000_0000 {
            break;
        }
        out[n] = ret;
        n += 1;
        if next <= rbp {
            break;
        }
        rbp = next;
    }
    n
}

/// Frames of a thread that is switched out: `rsp` is its saved stack pointer
/// (see `osyx_switch`: flags, r15..rbx, rbp, return address).
pub fn switched_out_frames(rsp: u64, hi: u64, out: &mut [u64; DEPTH]) -> usize {
    let (Ok(rbp), Ok(ret)) = (crate::fixup::probe_read_u64(rsp + 48), crate::fixup::probe_read_u64(rsp + 56)) else { return 0 };
    out[0] = ret;
    walk(rbp, rsp, hi, out, 1)
}

/// Where user programs' own code lives: the static executables at 4 MiB,
/// winrun at 1 GiB (it leaves the low gigabyte to Windows images). Only
/// samples there get caller candidates.
const USER_TEXT: [core::ops::Range<u64>; 2] = [0x40_0000..0x80_0000, 0x4000_0000..0x4100_0000];

fn user_text(a: u64) -> bool {
    USER_TEXT.iter().any(|r| r.contains(&a))
}

/// Up to three words from the top of the user stack that look like return
/// addresses into program code. A sample inside a leaf routine (the soft
/// float builtins, memcpy) has its caller's return address near the stack
/// pointer. Only the page the stack pointer is in is read, and only after a
/// page-table walk shows it is present, so the interrupt handler cannot fault.
fn user_callers(rsp: u64, out: &mut [u64; DEPTH]) -> usize {
    let page = rsp & !0xfff;
    let Some(t) = crate::mem::paging::translate(page) else { return 1 };
    let base = crate::mem::phys_to_virt(t.phys);
    let mut n = 1;
    let mut off = (rsp & 0xfff) & !7;
    while off < 4096 && off < (rsp & 0xfff) + 8 * 24 && n < 4 {
        // SAFETY: inside one present, mapped page of the current address space.
        let w = unsafe { core::ptr::read_volatile((base + off) as *const u64) };
        if user_text(w) && !out[1..n].contains(&w) {
            out[n] = w;
            n += 1;
        }
        off += 8;
    }
    n
}

/// Called from the timer interrupt of every CPU.
pub fn sample(f: &crate::arch::idt::TrapFrame, tick: u64) {
    if !enabled() {
        return;
    }
    let cpu = crate::smp::index();
    let (pid, tid) = (crate::smp::cached_pid() as u32, crate::smp::cached_tid() as u32);
    if f.cs & 3 == 3 {
        let mut frames = [0u64; DEPTH];
        frames[0] = f.rip;
        let n = if user_text(f.rip) { user_callers(f.rsp, &mut frames) } else { 1 };
        record(cpu, pid, tid, USER, 0, &frames[..n], 1);
    } else {
        let mut frames = [0u64; DEPTH];
        frames[0] = f.rip;
        let n = walk(f.rbp, f.rsp, f.rsp.saturating_add(64 * 1024), &mut frames, 1);
        record(cpu, pid, tid, KERNEL, last_syscall(tid as u64), &frames[..n], 1);
    }
    if cpu == 0 && tick % OFF_CPU_PERIOD == 0 {
        sched::prof_threads(|tid, pid, state, rsp, hi| {
            let mut frames = [0u64; DEPTH];
            let n = switched_out_frames(rsp, hi, &mut frames);
            let kind = if state == sched::State::Blocked { BLOCKED } else { RUNNABLE };
            record(0, pid as u32, tid as u32, kind, last_syscall(tid), &frames[..n], OFF_CPU_PERIOD);
        });
    }
}

/// Allocates the tables and starts sampling; the dump thread ends it.
pub fn init() {
    let Some(secs) = crate::boot_option_value("prof").and_then(|v| v.parse::<u64>().ok()) else { return };
    for t in TABLES.iter().take(crate::smp::online().max(1)) {
        let mut v: Vec<Entry> = Vec::with_capacity(SLOTS);
        v.resize(SLOTS, Entry::ZERO);
        // SAFETY: nothing samples before `ENABLED` is set below.
        unsafe { *t.entries.get() = v.leak().as_mut_ptr() };
    }
    ENABLED.store(true, Ordering::SeqCst);
    sched::spawn_kernel("prof-dump", dump_worker, secs, 0);
    crate::kprintln!("prof: sampling at 1000 Hz for {secs} s");
}

fn dump_worker(secs: u64, _: u64) {
    sched::sleep_ticks(crate::arch::timer::ms_to_ticks(secs * 1000));
    ENABLED.store(false, Ordering::SeqCst);
    // Let samples in flight finish.
    sched::sleep_ticks(crate::arch::timer::ms_to_ticks(20));
    let mut all: Vec<Entry> = Vec::new();
    let (mut samples, mut dropped) = (0u64, 0u64);
    for t in TABLES.iter() {
        // SAFETY: sampling stopped.
        let base = unsafe { *t.entries.get() };
        if base.is_null() {
            continue;
        }
        samples += t.samples.load(Ordering::Relaxed);
        dropped += t.dropped.load(Ordering::Relaxed);
        for i in 0..SLOTS {
            // SAFETY: `i < SLOTS`.
            let e = unsafe { *base.add(i) };
            if e.count != 0 {
                all.push(e);
            }
        }
    }
    // The same stack on several CPUs is one line.
    all.sort_unstable_by_key(|e| (e.hash, e.pid, e.tid, e.kind));
    let mut merged: Vec<Entry> = Vec::with_capacity(all.len());
    for e in all {
        match merged.last_mut() {
            Some(l) if l.hash == e.hash && l.pid == e.pid && l.tid == e.tid && l.kind == e.kind && l.sys == e.sys && l.frames[..l.depth as usize] == e.frames[..e.depth as usize] => l.count += e.count,
            _ => merged.push(e),
        }
    }
    merged.sort_unstable_by(|a, b| b.count.cmp(&a.count));
    let (ticks, idle) = sched::cpu_stats();
    crate::kprintln!("prof: begin cpus={} samples={samples} dropped={dropped} stacks={} ticks={ticks} idle_ticks={idle}", crate::smp::online(), merged.len());
    let mut rest = 0u64;
    for (n, e) in merged.iter().enumerate() {
        if n >= MAX_LINES {
            rest += e.count;
            continue;
        }
        let kind = ['U', 'K', 'B', 'R'][e.kind as usize & 3];
        let mut line = alloc::string::String::new();
        use core::fmt::Write;
        let _ = write!(line, "prof: {kind} {} {} {} {}", e.pid, e.tid, e.sys, e.count);
        for f in &e.frames[..e.depth as usize] {
            let _ = write!(line, " {f:x}");
        }
        crate::kprintln!("{line}");
    }
    crate::kprintln!("prof: end other={rest}");
}
