//! Bitmap physical frame allocator.
//!
//! One bit per 4 KiB frame from physical address 0 up to the end of the
//! highest usable region; 1 = used/unavailable, 0 = free. Only frames inside
//! `Usable` regions (or regions later released with [`FrameBitmap::release_range`])
//! can ever become free, so reserved, ACPI, MMIO and kernel memory is never
//! handed out. Freeing a frame that is not currently allocated is reported as
//! an error instead of silently corrupting the state.

use crate::{align_down, align_up, PAGE_SIZE};
use osyx_bootinfo::{MemoryDescriptor, MemoryKind};

/// Frames below 1 MiB are never handed out: they hold legacy BIOS/firmware
/// structures and will host SMP trampolines.
pub const LOW_MEMORY_LIMIT: u64 = 0x10_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    OutOfRange(u64),
    NotAllocated(u64),
    Unaligned(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitmapPlan {
    /// Number of frames covered by the bitmap.
    pub frames: u64,
    /// Bytes of bitmap storage needed (multiple of 8).
    pub bytes: u64,
    /// Physical address where the bitmap should be stored.
    pub phys: u64,
}

/// Decides how large the bitmap must be and where to put it: the first usable
/// region above 1 MiB with room for it.
pub fn plan(map: &[MemoryDescriptor]) -> Option<BitmapPlan> {
    let top = map.iter().filter(|d| d.kind == MemoryKind::Usable).map(|d| d.end()).max()?;
    let frames = top / PAGE_SIZE;
    let bytes = align_up(frames.div_ceil(8), 8);
    let phys = map.iter().filter(|d| d.kind == MemoryKind::Usable).find_map(|d| {
        let start = align_up(d.phys_start.max(LOW_MEMORY_LIMIT), PAGE_SIZE);
        (start < d.end() && d.end() - start >= bytes).then_some(start)
    })?;
    Some(BitmapPlan { frames, bytes, phys })
}

pub struct FrameBitmap<'a> {
    bits: &'a mut [u64],
    frames: u64,
    free: u64,
    total_usable: u64,
    hint: usize,
}

impl<'a> FrameBitmap<'a> {
    /// Builds the allocator from the memory map. `storage` must be the memory
    /// described by `plan.phys` (it is marked used so it is never handed out).
    pub fn new(storage: &'a mut [u64], plan: &BitmapPlan, map: &[MemoryDescriptor]) -> Self {
        assert!(storage.len() as u64 * 64 >= plan.frames, "bitmap storage too small");
        storage.fill(u64::MAX);
        let mut fb = FrameBitmap { bits: storage, frames: plan.frames, free: 0, total_usable: 0, hint: 0 };
        for d in map.iter().filter(|d| d.kind == MemoryKind::Usable) {
            fb.release_range(d.phys_start, d.end());
        }
        fb.reserve_range(0, LOW_MEMORY_LIMIT);
        fb.reserve_range(plan.phys, plan.phys + plan.bytes);
        fb
    }

    fn test(&self, f: u64) -> bool {
        self.bits[(f / 64) as usize] & (1 << (f % 64)) != 0
    }

    fn set(&mut self, f: u64) {
        self.bits[(f / 64) as usize] |= 1 << (f % 64);
    }

    fn clear(&mut self, f: u64) {
        self.bits[(f / 64) as usize] &= !(1 << (f % 64));
    }

    /// Marks every whole frame inside `[start, end)` free (clipped to the
    /// bitmap and to memory above [`LOW_MEMORY_LIMIT`]). Used for usable RAM
    /// and for reclaiming bootloader memory.
    pub fn release_range(&mut self, start: u64, end: u64) {
        let first = align_up(start.max(LOW_MEMORY_LIMIT), PAGE_SIZE) / PAGE_SIZE;
        let last = (align_down(end, PAGE_SIZE) / PAGE_SIZE).min(self.frames);
        for f in first..last {
            if self.test(f) {
                self.clear(f);
                self.free += 1;
                self.total_usable += 1;
            }
        }
    }

    /// Permanently withdraws every free frame touching `[start, end)`.
    /// Frames that are currently allocated are left to their owner.
    pub fn reserve_range(&mut self, start: u64, end: u64) {
        let first = align_down(start, PAGE_SIZE) / PAGE_SIZE;
        let last = (align_up(end, PAGE_SIZE) / PAGE_SIZE).min(self.frames);
        for f in first..last {
            if !self.test(f) {
                self.set(f);
                self.free -= 1;
                self.total_usable -= 1;
            }
        }
    }

    /// Allocates one frame and returns its physical address.
    pub fn alloc(&mut self) -> Option<u64> {
        let words = self.bits.len();
        for i in 0..words {
            let w = (self.hint + i) % words;
            let word = self.bits[w];
            if word == u64::MAX {
                continue;
            }
            let bit = (!word).trailing_zeros() as u64;
            let f = w as u64 * 64 + bit;
            if f >= self.frames {
                continue;
            }
            self.set(f);
            self.free -= 1;
            self.hint = w;
            return Some(f * PAGE_SIZE);
        }
        None
    }

    /// Allocates `count` physically contiguous frames whose first frame is
    /// a multiple of `align` bytes and whose last byte lies below `limit`
    /// (devices with 32-bit DMA). Returns the first frame's address.
    pub fn alloc_contiguous(&mut self, count: u64, align: u64, limit: u64) -> Option<u64> {
        if count == 0 {
            return None;
        }
        let step = (align / PAGE_SIZE).max(1);
        let end = self.frames.min(limit / PAGE_SIZE);
        let mut f = align_up(LOW_MEMORY_LIMIT / PAGE_SIZE, step);
        while f + count <= end {
            match (f..f + count).find(|&g| self.test(g)) {
                // Restart after the used frame, at the next aligned slot.
                Some(used) => f = align_up(used + 1, step),
                None => {
                    for g in f..f + count {
                        self.set(g);
                    }
                    self.free -= count;
                    return Some(f * PAGE_SIZE);
                }
            }
        }
        None
    }

    pub fn free(&mut self, phys: u64) -> Result<(), FrameError> {
        if !phys.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Unaligned(phys));
        }
        let f = phys / PAGE_SIZE;
        if f >= self.frames || phys < LOW_MEMORY_LIMIT {
            return Err(FrameError::OutOfRange(phys));
        }
        if !self.test(f) {
            return Err(FrameError::NotAllocated(phys));
        }
        self.clear(f);
        self.free += 1;
        Ok(())
    }

    pub fn is_free(&self, phys: u64) -> bool {
        let f = phys / PAGE_SIZE;
        f < self.frames && !self.test(f)
    }

    pub fn free_frames(&self) -> u64 {
        self.free
    }

    pub fn total_frames(&self) -> u64 {
        self.total_usable
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::collections::BTreeSet;
    use std::vec;

    fn d(start: u64, pages: u64, kind: MemoryKind) -> MemoryDescriptor {
        MemoryDescriptor { phys_start: start, page_count: pages, kind, _reserved: 0 }
    }

    fn sample_map() -> [MemoryDescriptor; 5] {
        [
            d(0, 160, MemoryKind::Usable),             // 0..640K (below 1 MiB)
            d(0x10_0000, 256, MemoryKind::Usable),     // 1 MiB..2 MiB
            d(0x20_0000, 16, MemoryKind::KernelImage), // kernel
            d(0x21_0000, 240, MemoryKind::Usable),     // ..3 MiB
            d(0xFEC0_0000, 1, MemoryKind::Mmio),       // must not extend bitmap
        ]
    }

    #[test]
    fn plan_places_bitmap_above_low_memory() {
        let p = plan(&sample_map()).unwrap();
        assert_eq!(p.frames, 0x30_0000 / PAGE_SIZE);
        assert_eq!(p.bytes, 96);
        assert_eq!(p.phys, 0x10_0000);
        assert!(plan(&[d(0x10_0000, 1, MemoryKind::Reserved)]).is_none());
    }

    #[test]
    fn only_usable_frames_are_handed_out_once() {
        let map = sample_map();
        let p = plan(&map).unwrap();
        let mut storage = vec![0u64; (p.bytes / 8) as usize];
        let mut fb = FrameBitmap::new(&mut storage, &p, &map);
        // 256 + 240 usable frames above 1 MiB minus one bitmap frame.
        assert_eq!(fb.free_frames(), 256 + 240 - 1);
        assert_eq!(fb.total_frames(), fb.free_frames());
        let mut seen = BTreeSet::new();
        while let Some(f) = fb.alloc() {
            assert!(f >= LOW_MEMORY_LIMIT, "low frame {f:#x}");
            assert!(!(0x20_0000..0x21_0000).contains(&f), "kernel frame {f:#x}");
            assert_ne!(f, p.phys, "bitmap frame handed out");
            assert!(seen.insert(f), "frame {f:#x} handed out twice");
        }
        assert_eq!(seen.len() as u64, 256 + 240 - 1);
        assert_eq!(fb.free_frames(), 0);
        for f in &seen {
            fb.free(*f).unwrap();
        }
        assert_eq!(fb.free_frames(), 256 + 240 - 1);
    }

    #[test]
    fn invalid_frees_are_rejected() {
        let map = sample_map();
        let p = plan(&map).unwrap();
        let mut storage = vec![0u64; (p.bytes / 8) as usize];
        let mut fb = FrameBitmap::new(&mut storage, &p, &map);
        let f = fb.alloc().unwrap();
        assert_eq!(fb.free(f + 1), Err(FrameError::Unaligned(f + 1)));
        fb.free(f).unwrap();
        assert_eq!(fb.free(f), Err(FrameError::NotAllocated(f)), "double free");
        assert_eq!(fb.free(0x1000), Err(FrameError::OutOfRange(0x1000)));
        assert_eq!(fb.free(0x4000_0000), Err(FrameError::OutOfRange(0x4000_0000)));
    }

    #[test]
    fn contiguous_runs_respect_holes_alignment_and_limit() {
        let map = sample_map();
        let p = plan(&map).unwrap();
        let mut storage = vec![0u64; (p.bytes / 8) as usize];
        let mut fb = FrameBitmap::new(&mut storage, &p, &map);
        let free = fb.free_frames();
        // 64 KiB aligned to 64 KiB: the first 1 MiB frame holds the bitmap.
        let a = fb.alloc_contiguous(16, 0x1_0000, u64::MAX).unwrap();
        assert_eq!(a % 0x1_0000, 0);
        assert!(a >= 0x11_0000 && !(0x20_0000..0x21_0000).contains(&a));
        for f in 0..16 {
            assert!(!fb.is_free(a + f * PAGE_SIZE));
        }
        assert_eq!(fb.free_frames(), free - 16);
        // A run cannot cross the kernel image hole.
        // 220 frames fit only after the first run, below the kernel image.
        let b = fb.alloc_contiguous(220, PAGE_SIZE, u64::MAX).unwrap();
        assert!(b >= a + 16 * PAGE_SIZE && b + 220 * PAGE_SIZE <= 0x20_0000);
        assert!(fb.alloc_contiguous(300, PAGE_SIZE, u64::MAX).is_none());
        assert!(fb.alloc_contiguous(1, PAGE_SIZE, 0x10_0000).is_none(), "nothing below 1 MiB");
        for f in 0..16 {
            fb.free(a + f * PAGE_SIZE).unwrap();
        }
    }

    #[test]
    fn reclaim_and_reserve_ranges() {
        let map = sample_map();
        let p = plan(&map).unwrap();
        let mut storage = vec![0u64; (p.bytes / 8) as usize];
        let mut fb = FrameBitmap::new(&mut storage, &p, &map);
        let before = fb.free_frames();
        let total = fb.total_frames();
        fb.release_range(0x20_0000, 0x20_8000); // reclaim half of the "kernel"
        fb.release_range(0x20_0000, 0x20_8000); // idempotent
        assert_eq!(fb.free_frames(), before + 8);
        assert_eq!(fb.total_frames(), total + 8);
        assert!(fb.is_free(0x20_0000));
        fb.reserve_range(0x20_0000, 0x20_0001);
        assert!(!fb.is_free(0x20_0000));
        assert_eq!(fb.free_frames(), before + 7);
        assert_eq!(fb.total_frames(), total + 7);
    }
}
