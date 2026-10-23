//! OSyx back end: system calls, process start-up, heap and panic handling.

use crate::error::{Error, Result};
use crate::io::ChunkWriter;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    let code = __osyx_user_main();
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
