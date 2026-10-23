//! OSyx back end: system calls, process start-up, heap and panic handling.

use crate::error::{Error, Result};
use crate::io::ChunkWriter;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use osyx_abi::{SystemInfo, PROT_READ, PROT_WRITE};
use osyx_mm::heap::Heap;

pub const NAME: &str = "osyx";

pub fn write(fd: u64, buf: &[u8]) -> Result<usize> {
    os_sys::write(fd, buf).map_err(Error::from)
}

pub fn exit(code: i32) -> ! {
    os_sys::exit(code)
}

pub fn monotonic_ns() -> Result<u64> {
    os_sys::clock(osyx_abi::CLOCK_MONOTONIC).map_err(Error::from)
}

pub fn sysinfo() -> Result<SystemInfo> {
    os_sys::sysinfo().map_err(Error::from)
}

pub fn spawn(name: &str, args: &[u8]) -> Result<u64> {
    os_sys::spawn(name, args).map_err(Error::from)
}

pub fn wait(pid: u64) -> Result<(u64, i32)> {
    os_sys::wait(pid).map_err(Error::from)
}

pub fn sleep_ns(ns: u64) {
    os_sys::sleep(ns)
}

pub fn yield_now() {
    os_sys::yield_now()
}

// ---- threads, futexes, TLS --------------------------------------------------

/// Stack size of threads created with `thread::spawn` (the region `map`
/// returns is followed by a guard page; the previous region's guard page
/// lies below it).
pub const DEFAULT_STACK: usize = 256 * 1024;

/// A started thread whose stack must be released once it has finished.
pub struct RawThread {
    stack: *mut u8,
    stack_len: usize,
}

// SAFETY: the stack is only touched by the thread itself and freed after it
// has provably left it (its done word is 1).
unsafe impl Send for RawThread {}

struct Start {
    entry: fn(usize),
    arg: usize,
    done: *const AtomicU32,
}

global_asm!(
    r#"
    .section .text
    // First instruction of every thread created by thread_spawn: rdi = Start.
    .global osyx_rt_thread_entry
osyx_rt_thread_entry:
    and rsp, -16
    xor ebp, ebp
    call osyx_rt_thread_start
    ud2
    "#
);

extern "C" {
    fn osyx_rt_thread_entry();
}

#[no_mangle]
unsafe extern "C" fn osyx_rt_thread_start(start: *mut Start) -> ! {
    // SAFETY: leaked by `thread_spawn` for exactly this thread.
    let s = alloc::boxed::Box::from_raw(start);
    let (entry, arg, done) = (s.entry, s.arg, s.done);
    drop(s);
    entry(arg);
    // The kernel publishes `done` after we have left this stack.
    os_sys::thread_exit(0, done as *const u32)
}

/// Starts `entry(arg)` on a new thread with thread pointer `tcb`. When it
/// returns the kernel stores 1 to `done` and wakes its futex waiters.
pub fn thread_spawn(entry: fn(usize), arg: usize, tcb: usize, done: &AtomicU32, stack: usize) -> Result<RawThread> {
    let stack_len = stack.max(16 * 1024).next_multiple_of(4096);
    let base = os_sys::map(stack_len, PROT_READ | PROT_WRITE).map_err(Error::from)?;
    let start = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Start { entry, arg, done }));
    let top = base as usize + stack_len;
    match os_sys::thread_spawn(osyx_rt_thread_entry as *const () as usize, top, start as usize, tcb) {
        Ok(_) => Ok(RawThread { stack: base, stack_len }),
        Err(e) => {
            // SAFETY: the thread was not created, so nothing else owns these.
            unsafe {
                drop(alloc::boxed::Box::from_raw(start));
                let _ = os_sys::unmap(base, stack_len);
            }
            Err(Error::from(e))
        }
    }
}

/// Frees the stack of a thread whose done word is 1.
pub fn thread_release(t: RawThread) {
    // SAFETY: the thread has exited, nothing references its stack.
    let _ = unsafe { os_sys::unmap(t.stack, t.stack_len) };
}

/// Blocks while `word == expected`; false if the timeout expired.
pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: Option<core::time::Duration>) -> bool {
    let ns = timeout.map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX).max(1));
    !matches!(os_sys::futex_wait(word.as_ptr(), expected, ns), Err(osyx_abi::Errno::TimedOut))
}

pub fn futex_wake(word: *const AtomicU32, count: u32) {
    os_sys::futex_wake(word as *const u32, count);
}

/// The current thread pointer (`fs:[0]`, which holds the TCB's own address).
pub fn tcb() -> usize {
    let p: usize;
    // SAFETY: the runtime installs a TCB before any code can ask for it
    // (main thread in `osyx_rt_start`, other threads in `thread::spawn`).
    unsafe { core::arch::asm!("mov {}, fs:[0]", out(reg) p, options(nostack, readonly, preserves_flags)) };
    p
}

pub fn set_tcb(p: usize) {
    os_sys::set_fs_base(p).expect("set_fs_base rejected the TCB address");
}

pub fn current_tid() -> u64 {
    os_sys::gettid()
}

// ---- start-up -------------------------------------------------------------

static ARGC: AtomicUsize = AtomicUsize::new(0);
static ARGV: AtomicUsize = AtomicUsize::new(0);

global_asm!(
    r#"
    .section .text._start, "ax"
    .global _start
_start:
    mov rdi, rsp        // -> argc, argv[], NULL, envp[], NULL, auxv
    and rsp, -16
    xor ebp, ebp        // terminate frame-pointer chains
    call osyx_rt_start
    ud2
    "#
);

extern "Rust" {
    /// Defined by `os_std::entry!` in the program.
    fn __osyx_user_main() -> i32;
}

#[no_mangle]
unsafe extern "C" fn osyx_rt_start(sp: *const usize) -> ! {
    // SAFETY: the kernel builds a System V initial stack: argc followed by
    // argc pointers to NUL-terminated strings that live for the whole run.
    let argc = *sp;
    ARGC.store(argc, Ordering::Relaxed);
    ARGV.store(sp.add(1) as usize, Ordering::Relaxed);
    crate::tls::init_main_thread();
    let code = __osyx_user_main();
    // Returning from main ends the whole process (other threads included).
    exit(code)
}

pub fn args() -> Vec<String> {
    let argc = ARGC.load(Ordering::Relaxed);
    let argv = ARGV.load(Ordering::Relaxed) as *const *const u8;
    let mut out = Vec::with_capacity(argc);
    for i in 0..argc {
        // SAFETY: see `osyx_rt_start`; every argv entry is a C string.
        unsafe {
            let p = *argv.add(i);
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            out.push(String::from_utf8_lossy(core::slice::from_raw_parts(p, len)).into_owned());
        }
    }
    out
}

// ---- heap -----------------------------------------------------------------

/// Minimum amount of address space requested from the kernel per growth.
const GROW_MIN: usize = 256 * 1024;

struct UserHeap {
    locked: AtomicBool,
    heap: UnsafeCell<Heap>,
}

// SAFETY: all access to `heap` happens while `locked` is held.
unsafe impl Sync for UserHeap {}

impl UserHeap {
    fn with<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> R {
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        // SAFETY: the lock gives exclusive access.
        let r = f(unsafe { &mut *self.heap.get() });
        self.locked.store(false, Ordering::Release);
        r
    }
}

#[global_allocator]
static HEAP: UserHeap = UserHeap { locked: AtomicBool::new(false), heap: UnsafeCell::new(Heap::new()) };

// SAFETY: blocks come from `Heap`, which hands out disjoint, aligned ranges
// of memory the kernel mapped for this process.
unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with(|h| {
            if let Some(p) = h.alloc(layout) {
                return p.as_ptr();
            }
            let need = Heap::block_size(layout) + layout.align() + 16;
            let size = need.max(GROW_MIN).next_multiple_of(4096);
            match os_sys::map(size, PROT_READ | PROT_WRITE) {
                Ok(base) => {
                    // SAFETY: fresh, zeroed, writable mapping owned by the heap.
                    unsafe { h.add_region(base as usize, size) };
                    h.alloc(layout).map_or(core::ptr::null_mut(), |p| p.as_ptr())
                }
                Err(_) => core::ptr::null_mut(),
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            // SAFETY: forwarded caller contract.
            self.with(|h| unsafe { h.dealloc(p, layout) });
        }
    }
}

/// Heap statistics, for diagnostics.
pub fn heap_stats() -> osyx_mm::heap::HeapStats {
    HEAP.with(|h| h.stats())
}

// ---- panics ---------------------------------------------------------------

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if PANICKING.swap(true, Ordering::Relaxed) {
        exit(101); // panic while panicking: do not try to print again
    }
    let mut w = ChunkWriter::<256, _>::new(|c: &[u8]| {
        let _ = os_sys::write(osyx_abi::STDERR, c);
        Ok(())
    });
    let _ = core::fmt::write(&mut w, format_args!("program panicked: {}\n", info));
    let _ = w.finish();
    exit(101)
}
