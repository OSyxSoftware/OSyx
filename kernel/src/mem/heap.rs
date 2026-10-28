//! Kernel heap: `osyx_mm::heap::Heap` on demand-mapped virtual memory.
//!
//! The heap occupies `[HEAP_START, HEAP_START + HEAP_MAX)` (PML4 slot 384,
//! well away from the HHDM in slot 256). It starts with `HEAP_INITIAL` bytes
//! and grows by mapping fresh frames when an allocation does not fit.

use super::paging::{self, Flags};
use super::{alloc_frame, free_frame, PAGE_SIZE};
use crate::sync::IrqSpinLock;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use osyx_mm::heap::{Heap, HeapStats};

pub const HEAP_START: u64 = 0xFFFF_C000_0000_0000;
pub const HEAP_INITIAL: u64 = 4 << 20;
pub const HEAP_MAX: u64 = 1 << 30;
const GROW_MIN: u64 = 1 << 20;

// TEMP heap guard: one bit per 16 bytes of heap (the heap's allocation unit)
// marks a live allocation. Catches overlapping allocations and double/unknown
// frees in release builds; the free-list check alone only compares a freed
// range with free neighbours.
const GUARD_BYTES: u64 = 16;
const GUARD_BITS: usize = (HEAP_MAX / GUARD_BYTES) as usize;
const GUARD_WORDS: usize = GUARD_BITS / 64;
static LIVE: [core::sync::atomic::AtomicU64; GUARD_WORDS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; GUARD_WORDS];

// Ring of recent alloc/free events, so an overlap can name the owners.
const LOG_SLOTS: usize = 8192;
#[derive(Clone, Copy)]
struct Ev {
    addr: u64,
    size: u64,
    seq: u64,
    op: u64,
}
struct EventLog(core::cell::UnsafeCell<[Ev; LOG_SLOTS]>);
// SAFETY: only touched under the heap state lock (or in the panic it causes).
unsafe impl Sync for EventLog {}
static EVENTS: EventLog = EventLog(core::cell::UnsafeCell::new(
    [Ev { addr: 0, size: 0, seq: 0, op: 0 }; LOG_SLOTS],
));
static EVENT_SEQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn note_event(addr: u64, size: u64, op: u64) {
    use core::sync::atomic::Ordering;
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let slot = seq as usize & (LOG_SLOTS - 1);
    // SAFETY: the heap lock serialises all writers.
    unsafe { (&mut *EVENTS.0.get())[slot] = Ev { addr, size, seq, op } };
}

fn dump_stack() {
    let rsp: u64;
    // SAFETY: reading registers and this thread's own kernel stack.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    let mut shown = 0;
    for i in 0..96u64 {
        let w = unsafe { core::ptr::read_volatile((rsp + i * 8) as *const u64) };
        if (0xffff_ffff_8000_0000..0xffff_ffff_c000_0000).contains(&w) {
            crate::kprintln!("heap guard: kstack ra[{}]={w:#x}", i);
            shown += 1;
            if shown >= 12 {
                break;
            }
        }
    }
}

fn guard(addr: u64, size: u64, set: bool) {
    use core::sync::atomic::Ordering;
    let base = addr.wrapping_sub(HEAP_START);
    if base >= HEAP_MAX || size == 0 || size > HEAP_MAX - base {
        return;
    }
    note_event(addr, size, set as u64);
    let first = (base / GUARD_BYTES) as usize;
    let last = ((base + size - 1) / GUARD_BYTES) as usize;
    let (w0, w1) = (first / 64, last / 64);
    for (w, live) in LIVE.iter().enumerate().take(w1 + 1).skip(w0) {
        let lo = if w == w0 { first % 64 } else { 0 };
        let hi = if w == w1 { last % 64 } else { 63 };
        let mask = (!0u64 << lo) & if hi == 63 { !0 } else { (1u64 << (hi + 1)) - 1 };
        let cur = live.load(Ordering::Relaxed);
        if set {
            if cur & mask != 0 {
                let bit = (w * 64 + (cur & mask).trailing_zeros() as usize) as u64;
                let at = HEAP_START + bit * GUARD_BYTES;
                crate::kprintln!(
                    "heap guard: alloc overlaps live block addr={:#x} size={:#x} live_at={:#x}",
                    addr,
                    size,
                    at
                );
                let now = EVENT_SEQ.load(Ordering::Relaxed);
                let mut shown = 0;
                for back in 1..=LOG_SLOTS as u64 {
                    let seq = now.saturating_sub(back);
                    if seq == 0 || shown >= 8 {
                        break;
                    }
                    let ev = unsafe { (&*EVENTS.0.get())[seq as usize & (LOG_SLOTS - 1)] };
                    if ev.seq != seq {
                        continue;
                    }
                    if ev.addr <= at && at < ev.addr.saturating_add(ev.size) {
                        crate::kprintln!(
                            "heap guard: {} addr={:#x} size={:#x} seq={}",
                            if ev.op == 1 { "alloc" } else { "free" },
                            ev.addr,
                            ev.size,
                            ev.seq
                        );
                        shown += 1;
                    }
                }
                dump_stack();
                panic!("heap guard: overlapping allocation");
            }
            LIVE[w].store(cur | mask, Ordering::Relaxed);
        } else {
            if cur & mask != mask {
                crate::kprintln!(
                    "heap guard: free of unknown block addr={:#x} size={:#x} live={:#x}",
                    addr,
                    size,
                    cur & mask
                );
                dump_stack();
                panic!("heap guard: free of unknown block");
            }
            LIVE[w].store(cur & !mask, Ordering::Relaxed);
        }
    }
}

struct State {
    heap: Heap,
    mapped_end: u64,
}

pub struct KernelHeap {
    state: IrqSpinLock<State>,
}

#[global_allocator]
pub static HEAP: KernelHeap =
    KernelHeap { state: IrqSpinLock::new(State { heap: Heap::new(), mapped_end: HEAP_START }) };

impl State {
    /// Maps at least `bytes` more heap memory. Returns the bytes added.
    fn grow(&mut self, bytes: u64) -> u64 {
        let want = bytes.max(GROW_MIN).next_multiple_of(PAGE_SIZE);
        let limit = HEAP_START + HEAP_MAX;
        let start = self.mapped_end;
        let mut end = start;
        while end < start + want && end < limit {
            let Some(frame) = alloc_frame() else { break };
            if paging::map(end, frame, Flags::KERNEL_RW).is_err() {
                let _ = free_frame(frame);
                break;
            }
            end += PAGE_SIZE;
        }
        if end > start {
            // SAFETY: [start, end) was just mapped, is writable and unused.
            unsafe { self.heap.add_region(start as usize, (end - start) as usize) };
            self.mapped_end = end;
        }
        end - start
    }
}

/// Maps the initial heap.
pub fn init() -> u64 {
    HEAP.state.lock().grow(HEAP_INITIAL)
}

pub fn stats() -> HeapStats {
    HEAP.state.lock().heap.stats()
}

// SAFETY: all heap state is behind an IRQ-safe lock; returned blocks are
// disjoint, correctly aligned and inside mapped memory.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut s = self.state.lock();
        if let Some(p) = s.heap.alloc(layout) {
            guard(p.as_ptr() as u64, Heap::block_size(layout) as u64, true);
            return p.as_ptr();
        }
        // Grow enough to fit the block even with worst-case alignment padding.
        let need = (Heap::block_size(layout) + layout.align() + 16) as u64;
        if s.grow(need) == 0 {
            return null_mut();
        }
        match s.heap.alloc(layout) {
            Some(p) => {
                guard(p.as_ptr() as u64, Heap::block_size(layout) as u64, true);
                p.as_ptr()
            }
            None => null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            let mut s = self.state.lock();
            guard(p.as_ptr() as u64, Heap::block_size(layout) as u64, false);
            s.heap.dealloc(p, layout);
        }
    }
}
