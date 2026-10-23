//! Host back end, used only to unit-test OSyx Std on the development machine.
//! It is implemented with the host's Rust std; nothing here runs on OSyx.

use crate::error::{Error, ErrorKind, Result};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use osyx_abi::SystemInfo;
use std::io::Write as _;

pub const NAME: &str = "host";

pub fn write(fd: u64, buf: &[u8]) -> Result<usize> {
    let r = match fd {
        osyx_abi::STDOUT => std::io::stdout().write(buf),
        osyx_abi::STDERR => std::io::stderr().write(buf),
        _ => return Err(Error::from(osyx_abi::Errno::BadHandle)),
    };
    r.map_err(|_| Error::new(ErrorKind::Other, "host write failed"))
}

pub fn exit(code: i32) -> ! {
    std::process::exit(code)
}

pub fn monotonic_ns() -> Result<u64> {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    Ok(start.elapsed().as_nanos() as u64)
}

pub fn sysinfo() -> Result<SystemInfo> {
    Err(Error::unsupported("sysinfo is only available on OSyx"))
}

pub fn args() -> Vec<String> {
    std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect()
}

pub fn spawn(_name: &str, _args: &[u8]) -> Result<u64> {
    Err(Error::unsupported("spawning OSyx boot modules is only possible on OSyx"))
}

pub fn wait(_pid: u64) -> Result<(u64, i32)> {
    Err(Error::unsupported("waiting for OSyx processes is only possible on OSyx"))
}

pub fn sleep_ns(ns: u64) {
    std::thread::sleep(std::time::Duration::from_nanos(ns))
}

pub fn yield_now() {
    std::thread::yield_now()
}

// Threads, futexes and TLS are emulated with the host's std so the OSyx Std
// synchronisation code can be tested with real, parallel host threads.

pub const DEFAULT_STACK: usize = 256 * 1024;

pub struct RawThread;

/// Wake generation per futex address. A waiter returns when the generation
/// of *its* address changes (a `futex_wake` on it) or its timeout expires;
/// the shared condition variable only serves to sleep.
static FUTEX: std::sync::Mutex<alloc::collections::BTreeMap<usize, u64>> =
    std::sync::Mutex::new(alloc::collections::BTreeMap::new());
static FUTEX_CV: std::sync::Condvar = std::sync::Condvar::new();

pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: Option<core::time::Duration>) -> bool {
    let addr = word as *const AtomicU32 as usize;
    let mut map = FUTEX.lock().unwrap_or_else(|e| e.into_inner());
    if word.load(Ordering::SeqCst) != expected {
        return true;
    }
    let gen = *map.get(&addr).unwrap_or(&0);
    let deadline = timeout.map(|d| std::time::Instant::now() + d);
    loop {
        map = match deadline {
            None => FUTEX_CV.wait(map).unwrap_or_else(|e| e.into_inner()),
            Some(dl) => {
                let left = dl.saturating_duration_since(std::time::Instant::now());
                FUTEX_CV.wait_timeout(map, left).unwrap_or_else(|e| e.into_inner()).0
            }
        };
        if *map.get(&addr).unwrap_or(&0) != gen {
            return true;
        }
        if deadline.is_some_and(|dl| std::time::Instant::now() >= dl) {
            return false;
        }
    }
}

/// Only uses `word` as a key (never dereferences it), like the kernel.
pub fn futex_wake(word: *const AtomicU32, _count: u32) {
    let mut map = FUTEX.lock().unwrap_or_else(|e| e.into_inner());
    *map.entry(word as usize).or_insert(0) += 1;
    FUTEX_CV.notify_all();
}

pub fn thread_spawn(entry: fn(usize), arg: usize, tcb: usize, done: &AtomicU32, stack: usize) -> Result<RawThread> {
    let done_addr = done as *const AtomicU32 as usize;
    std::thread::Builder::new()
        .stack_size(stack.max(64 * 1024))
        .spawn(move || {
            set_tcb(tcb);
            entry(arg);
            // Like the kernel: publish completion, then use the address only
            // as a wake-up key (the joiner may free it right after the store).
            // SAFETY: the joiner keeps the word alive until it reads 1.
            unsafe { (*(done_addr as *const AtomicU32)).store(1, Ordering::Release) };
            futex_wake(done_addr as *const AtomicU32, u32::MAX);
        })
        .map(|_| RawThread)
        .map_err(|_| Error::new(ErrorKind::OutOfMemory, "host thread creation failed"))
}

pub fn thread_release(_t: RawThread) {}

std::thread_local! {
    static TCB: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
    static TID: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

pub fn tcb() -> usize {
    TCB.with(|t| t.get())
}

pub fn set_tcb(p: usize) {
    TCB.with(|t| t.set(p));
}

pub fn current_tid() -> u64 {
    static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
    TID.with(|t| {
        if t.get() == 0 {
            t.set(NEXT.fetch_add(1, Ordering::Relaxed));
        }
        t.get()
    })
}
