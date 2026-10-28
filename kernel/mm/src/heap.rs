//! First-fit, address-ordered free-list heap with immediate coalescing.
//!
//! Invariants:
//! * every block (free or allocated) starts on a 16-byte boundary and has a
//!   size that is a multiple of 16 (>= 16), so a free block header always fits;
//! * the free list is sorted by address and no two free blocks are adjacent
//!   (they are merged on insertion).
//!
//! An allocation of `layout` occupies exactly [`Heap::block_size`]`(layout)`
//! bytes, which `dealloc` recomputes from the same layout; no per-allocation
//! header is needed.

use core::alloc::Layout;
use core::ptr::{null_mut, NonNull};

const UNIT: usize = 16;

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    pub capacity: usize,
    pub used: usize,
    pub free_blocks: usize,
}

pub struct Heap {
    head: *mut FreeBlock,
    capacity: usize,
    used: usize,
}

// SAFETY: the heap only manages raw memory handed to it; callers serialise
// access (the kernel wraps it in a lock).
unsafe impl Send for Heap {}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub const fn new() -> Self {
        Heap { head: null_mut(), capacity: 0, used: 0 }
    }

    pub fn block_size(layout: Layout) -> usize {
        layout.size().max(1).next_multiple_of(UNIT)
    }

    /// Adds `[start, start + size)` to the heap. Unaligned edges are trimmed.
    ///
    /// # Safety
    /// The range must be valid, writable, unused memory that stays owned by
    /// this heap from now on.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        let s = start.next_multiple_of(UNIT);
        let end = (start + size) & !(UNIT - 1);
        if end <= s || end - s < UNIT {
            return;
        }
        self.capacity += end - s;
        self.insert(s, end - s);
    }

    /// Inserts a free block in address order, merging with neighbours.
    unsafe fn insert(&mut self, addr: usize, size: usize) {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        while !cur.is_null() && (cur as usize) < addr {
            prev = cur;
            cur = (*cur).next;
        }
        // A double free or an out-of-range dealloc overlaps a free block. It
        // would hand the same bytes to two owners, so stop here instead of
        // corrupting live objects silently.
        let clash = (!cur.is_null() && addr + size > cur as usize)
            || (!prev.is_null() && prev as usize + (*prev).size > addr);
        if clash || size == 0 {
            panic!("heap: overlapping free addr={addr:#x} size={size:#x} prev={prev:p} cur={cur:p}");
        }
        let block = addr as *mut FreeBlock;
        block.write(FreeBlock { size, next: cur });
        if !cur.is_null() && addr + size == cur as usize {
            (*block).size += (*cur).size;
            (*block).next = (*cur).next;
        }
        if prev.is_null() {
            self.head = block;
        } else if prev as usize + (*prev).size == addr {
            (*prev).size += (*block).size;
            (*prev).next = (*block).next;
        } else {
            (*prev).next = block;
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let size = Self::block_size(layout);
        let align = layout.align().max(UNIT);
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        // SAFETY: the list only contains blocks inserted by `add_region` /
        // `dealloc`, all of which point to memory owned by the heap.
        unsafe {
            while !cur.is_null() {
                let start = cur as usize;
                let end = start + (*cur).size;
                let mut a = start.next_multiple_of(align);
                // A front remainder must be able to hold a free block header.
                if a != start && a - start < UNIT {
                    a += align;
                }
                if a + size <= end {
                    let next = (*cur).next;
                    // Unlink `cur`, then return the front/back remainders.
                    if prev.is_null() {
                        self.head = next;
                    } else {
                        (*prev).next = next;
                    }
                    if a > start {
                        self.insert(start, a - start);
                    }
                    if end > a + size {
                        self.insert(a + size, end - (a + size));
                    }
                    self.used += size;
                    return NonNull::new(a as *mut u8);
                }
                prev = cur;
                cur = (*cur).next;
            }
        }
        None
    }

    /// # Safety
    /// `ptr` must come from `alloc` on this heap with the same `layout`, and
    /// must not be used afterwards.
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let size = Self::block_size(layout);
        self.used -= size;
        self.insert(ptr.as_ptr() as usize, size);
    }

    pub fn stats(&self) -> HeapStats {
        let mut n = 0;
        let mut cur = self.head;
        // SAFETY: walking our own list.
        unsafe {
            while !cur.is_null() {
                n += 1;
                cur = (*cur).next;
            }
        }
        HeapStats { capacity: self.capacity, used: self.used, free_blocks: n }
    }

    /// Largest free block, for diagnostics and growth decisions.
    pub fn largest_free(&self) -> usize {
        let mut best = 0;
        let mut cur = self.head;
        // SAFETY: walking our own list.
        unsafe {
            while !cur.is_null() {
                best = best.max((*cur).size);
                cur = (*cur).next;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    struct Arena {
        mem: Vec<u128>,
    }

    impl Arena {
        fn new(bytes: usize) -> Arena {
            Arena { mem: std::vec![0u128; bytes / 16] }
        }
        fn range(&mut self) -> (usize, usize) {
            (self.mem.as_mut_ptr() as usize, self.mem.len() * 16)
        }
    }

    fn heap(arena: &mut Arena) -> Heap {
        let (s, n) = arena.range();
        let mut h = Heap::new();
        unsafe { h.add_region(s, n) };
        h
    }

    #[test]
    fn alloc_respects_alignment_and_bounds() {
        let mut a = Arena::new(64 * 1024);
        let (base, len) = a.range();
        let mut h = heap(&mut a);
        for align in [1usize, 8, 16, 32, 64, 256, 4096] {
            let l = Layout::from_size_align(24, align).unwrap();
            let p = h.alloc(l).unwrap().as_ptr() as usize;
            assert_eq!(p % align, 0);
            assert!(p >= base && p + 24 <= base + len);
        }
    }

    #[test]
    fn freeing_everything_restores_one_block() {
        let mut a = Arena::new(64 * 1024);
        let mut h = heap(&mut a);
        let cap = h.stats().capacity;
        let mut live = Vec::new();
        // Deterministic pseudo-random sizes/alignments.
        let mut seed = 0x2545F4914F6CDD1Du64;
        for _ in 0..400 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let size = (seed % 300) as usize + 1;
            let align = 1usize << (seed >> 60 & 7);
            let l = Layout::from_size_align(size, align).unwrap();
            if let Some(p) = h.alloc(l) {
                unsafe { core::ptr::write_bytes(p.as_ptr(), 0xA5, size) };
                live.push((p, l));
            }
            if seed.is_multiple_of(3) && !live.is_empty() {
                let (p, l) = live.swap_remove((seed as usize / 3) % live.len());
                unsafe { h.dealloc(p, l) };
            }
        }
        for (p, l) in live.drain(..) {
            unsafe { h.dealloc(p, l) };
        }
        let s = h.stats();
        assert_eq!(s.used, 0);
        assert_eq!(s.free_blocks, 1, "free list must coalesce back to one block");
        assert_eq!(h.largest_free(), cap);
    }

    #[test]
    fn allocations_never_overlap() {
        let mut a = Arena::new(32 * 1024);
        let mut h = heap(&mut a);
        let mut spans = Vec::new();
        for i in 0..200 {
            let l = Layout::from_size_align(1 + i % 97, 1 << (i % 6)).unwrap();
            match h.alloc(l) {
                Some(p) => spans.push((p.as_ptr() as usize, Heap::block_size(l))),
                None => break,
            }
        }
        spans.sort();
        for w in spans.windows(2) {
            assert!(w[0].0 + w[0].1 <= w[1].0, "overlap {:x?}", w);
        }
    }

    #[test]
    fn exhaustion_returns_none_and_regions_can_grow() {
        let mut a = Arena::new(4096);
        let mut b = Arena::new(8192);
        let mut h = heap(&mut a);
        let big = Layout::from_size_align(6000, 16).unwrap();
        assert!(h.alloc(big).is_none());
        let (s, n) = b.range();
        unsafe { h.add_region(s, n) };
        assert!(h.alloc(big).is_some());
        assert_eq!(h.stats().capacity, 4096 + 8192);
    }

    #[test]
    fn adjacent_regions_merge() {
        let mut a = Arena::new(8192);
        let (s, _) = a.range();
        let mut h = Heap::new();
        unsafe {
            h.add_region(s + 4096, 4096);
            h.add_region(s, 4096);
        }
        assert_eq!(h.stats().free_blocks, 1);
        assert_eq!(h.largest_free(), 8192);
    }
}
