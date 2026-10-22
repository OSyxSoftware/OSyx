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
            return p.as_ptr();
        }
        // Grow enough to fit the block even with worst-case alignment padding.
        let need = (Heap::block_size(layout) + layout.align() + 16) as u64;
        if s.grow(need) == 0 {
            return null_mut();
        }
        s.heap.alloc(layout).map_or(null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            self.state.lock().heap.dealloc(p, layout);
        }
    }
}
