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

pub fn free_frame(pa: u64) -> Result<(), FrameError> {
    FRAMES.lock().as_mut().expect("frame allocator not initialised").free(pa)
}

pub fn frame_stats() -> (u64, u64) {
    let g = FRAMES.lock();
    let fb = g.as_ref().expect("frame allocator not initialised");
    (fb.free_frames(), fb.total_frames())
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
