//! Physical memory: HHDM translation and the global frame allocator.

pub mod heap;
pub mod paging;

use crate::kprintln;
use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_bootinfo::{MemoryDescriptor, MemoryKind};
use osyx_mm::frames::{self, FrameBitmap, FrameError};

pub use osyx_mm::PAGE_SIZE;

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);
static HHDM_SIZE: AtomicU64 = AtomicU64::new(0);
static FRAMES: IrqSpinLock<Option<FrameBitmap<'static>>> = IrqSpinLock::new(None);

pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

pub fn hhdm_size() -> u64 {
    HHDM_SIZE.load(Ordering::Relaxed)
}

/// Virtual address of physical address `pa` in the higher-half direct map.
pub fn phys_to_virt(pa: u64) -> u64 {
    debug_assert!(pa < hhdm_size(), "physical address {pa:#x} outside HHDM");
    pa + hhdm_offset()
}

/// Builds the frame allocator from the boot memory map.
///
/// # Safety
/// Must run once, after the kernel's own GDT/IDT are loaded (firmware tables
/// live in memory that becomes allocatable here).
pub unsafe fn init(map: &[MemoryDescriptor], hhdm_offset: u64, hhdm_size: u64) {
    HHDM_OFFSET.store(hhdm_offset, Ordering::Relaxed);
    HHDM_SIZE.store(hhdm_size, Ordering::Relaxed);
    let plan = frames::plan(map).expect("no usable memory for the frame bitmap");
    assert!(plan.phys + plan.bytes <= hhdm_size, "frame bitmap outside the HHDM");
    let storage = core::slice::from_raw_parts_mut(phys_to_virt(plan.phys) as *mut u64, (plan.bytes / 8) as usize);
    let fb = FrameBitmap::new(storage, &plan, map);
    kprintln!(
        "frames: {} usable ({} MiB), bitmap {} bytes at {:#x}",
        fb.free_frames(),
        (fb.free_frames() * PAGE_SIZE) >> 20,
        plan.bytes,
        plan.phys
    );
    *FRAMES.lock() = Some(fb);
}

/// Allocates a zeroed physical frame.
pub fn alloc_frame() -> Option<u64> {
    let pa = FRAMES.lock().as_mut()?.alloc()?;
    // SAFETY: the frame was just allocated and is reachable via the HHDM.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, PAGE_SIZE as usize) };
    Some(pa)
}

/// Allocates a physical frame without clearing it. The caller must write
/// every byte before anything else can read the frame (file data copied in
/// full: clearing a page just to overwrite it doubled the cost of reading a
/// 180 MiB browser archive).
pub fn alloc_frame_dirty() -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc()
}

/// Allocates `count` zeroed, physically contiguous frames aligned to
/// `align` bytes, all below `limit` (DMA buffers of devices).
pub fn alloc_frames_contiguous(count: u64, align: u64, limit: u64) -> Option<u64> {
    let pa = FRAMES.lock().as_mut()?.alloc_contiguous(count, align, limit)?;
    // SAFETY: the frames were just allocated and are reachable via the HHDM.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, (count * PAGE_SIZE) as usize) };
    Some(pa)
}

pub fn free_frame(pa: u64) -> Result<(), FrameError> {
    FRAMES.lock().as_mut().expect("frame allocator not initialised").free(pa)
}

pub fn frame_stats() -> (u64, u64) {
    let g = FRAMES.lock();
    let fb = g.as_ref().expect("frame allocator not initialised");
    (fb.free_frames(), fb.total_frames())
}

pub(crate) fn with_frame_allocator<R>(f: impl FnOnce(&mut FrameBitmap<'static>) -> R) -> R {
    let mut g = FRAMES.lock();
    f(g.as_mut().expect("frame allocator not initialised"))
}

/// Virtual window for device memory (PML4 slot 448), mapped uncached.
const MMIO_BASE: u64 = 0xFFFF_E000_0000_0000;
const MMIO_LIMIT: u64 = MMIO_BASE + (1 << 39);
static MMIO_NEXT: IrqSpinLock<u64> = IrqSpinLock::new(MMIO_BASE);

/// Maps `len` bytes of device memory at physical `phys` (uncached,
/// non-executable) and returns the virtual address of `phys`. Mappings are
/// permanent (devices stay attached).
pub fn map_mmio(phys: u64, len: u64) -> Result<u64, paging::MapError> {
    let start = phys & !(PAGE_SIZE - 1);
    let end = phys.checked_add(len).ok_or(paging::MapError::NonCanonical)?.next_multiple_of(PAGE_SIZE);
    let pages = (end - start) / PAGE_SIZE;
    let base = {
        let mut next = MMIO_NEXT.lock();
        let base = *next;
        if base + pages * PAGE_SIZE > MMIO_LIMIT {
            return Err(paging::MapError::OutOfMemory);
        }
        *next = base + pages * PAGE_SIZE;
        base
    };
    let flags = paging::Flags { writable: true, executable: false, user: false, uncached: true, write_combining: false };
    for i in 0..pages {
        paging::map(base + i * PAGE_SIZE, start + i * PAGE_SIZE, flags)?;
    }
    Ok(base + (phys - start))
}

/// Maps RAM frames (in order, write-back cached) at consecutive kernel
/// virtual addresses in the device window, so physically scattered memory
/// (a display backing store) can be addressed linearly. Permanent like
/// `map_mmio`; the frames stay owned by the caller.
pub fn map_frames(frames: &[u64]) -> Result<u64, paging::MapError> {
    let pages = frames.len() as u64;
    let base = {
        let mut next = MMIO_NEXT.lock();
        let base = *next;
        if base + pages * PAGE_SIZE > MMIO_LIMIT {
            return Err(paging::MapError::OutOfMemory);
        }
        *next = base + pages * PAGE_SIZE;
        base
    };
    for (i, &f) in frames.iter().enumerate() {
        paging::map(base + i as u64 * PAGE_SIZE, f, paging::Flags::KERNEL_RW)?;
    }
    Ok(base)
}

/// Returns bootloader-owned memory to the allocator. The caller must have
/// copied everything it needs out of BootInfo first.
pub fn reclaim_bootloader_memory(map: &[MemoryDescriptor]) -> u64 {
    let mut g = FRAMES.lock();
    let fb = g.as_mut().expect("frame allocator not initialised");
    let before = fb.free_frames();
    for d in map.iter().filter(|d| d.kind == MemoryKind::BootloaderReclaimable) {
        fb.release_range(d.phys_start, d.end());
    }
    fb.free_frames() - before
}
