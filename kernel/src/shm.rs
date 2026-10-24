//! Shared memory objects: page frames that several address spaces map.
//!
//! An object owns its frames (zeroed at creation) and frees them when the
//! last reference goes: handles and mappings both hold one. Mappings are
//! tracked by the process (`process.rs`) and removed before an address
//! space is destroyed, so `destroy_address_space` never frees a shared
//! frame. Device memory (the framebuffer) uses the same mapping machinery
//! but its frames are never freed.

use crate::mem::{alloc_frame, free_frame, PAGE_SIZE};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_abi::Errno;

/// Frames held by all shared memory objects (diagnostics).
static SHARED_FRAMES: AtomicU64 = AtomicU64::new(0);

pub struct SharedMemory {
    frames: Vec<u64>,
    size: u64,
    /// RAM frames from the allocator (freed on drop) or device memory.
    owned: bool,
    /// Device memory is mapped write-combining (framebuffers).
    pub write_combining: bool,
    /// A file-view alias borrows frames from a shared backing object.
    _owner: Option<Arc<dyn core::any::Any + Send + Sync>>,
    identity: Option<(usize, u64)>,
}

impl SharedMemory {
    /// `size` bytes (rounded up to pages) of zeroed memory.
    pub fn new(size: u64) -> Result<Arc<SharedMemory>, Errno> {
        if size == 0 || size > osyx_abi::ipc::MAX_SHM {
            return Err(Errno::InvalidArgument);
        }
        let pages = size.div_ceil(PAGE_SIZE);
        // Keep a reserve so a client cannot starve the kernel.
        let (free, _) = crate::mem::frame_stats();
        if pages + 4096 > free {
            return Err(Errno::NoMemory);
        }
        let mut frames = Vec::with_capacity(pages as usize);
        for _ in 0..pages {
            match alloc_frame() {
                Some(f) => frames.push(f),
                None => {
                    for f in frames {
                        let _ = free_frame(f);
                    }
                    return Err(Errno::NoMemory);
                }
            }
        }
        SHARED_FRAMES.fetch_add(pages, Ordering::Relaxed);
        Ok(Arc::new(SharedMemory { frames, size, owned: true, write_combining: false, _owner: None, identity: None }))
    }

    /// Physical memory of a device (page aligned).
    pub fn device(phys: u64, size: u64) -> Arc<SharedMemory> {
        let start = phys & !(PAGE_SIZE - 1);
        let end = (phys + size).next_multiple_of(PAGE_SIZE);
        let frames = (start..end).step_by(PAGE_SIZE as usize).collect();
        Arc::new(SharedMemory { frames, size: end - start, owned: false, write_combining: true, _owner: None, identity: None })
    }

    pub fn alias(frames: Vec<u64>, owner: Arc<dyn core::any::Any + Send + Sync>, identity: (usize, u64)) -> Arc<SharedMemory> {
        let size = frames.len() as u64 * PAGE_SIZE;
        Arc::new(SharedMemory { frames, size, owned: false, write_combining: false, _owner: Some(owner), identity: Some(identity) })
    }

    pub fn key(&self, offset: u64) -> (usize, u64) {
        let (object, base) = self.identity.unwrap_or((self as *const Self as usize, 0));
        (object, base + offset)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn pages(&self) -> u64 {
        self.frames.len() as u64
    }

    pub fn frames(&self) -> &[u64] {
        &self.frames
    }

}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        if self.owned {
            for &f in &self.frames {
                let _ = free_frame(f);
            }
            SHARED_FRAMES.fetch_sub(self.frames.len() as u64, Ordering::Relaxed);
        }
    }
}

pub fn shared_frames() -> u64 {
    SHARED_FRAMES.load(Ordering::Relaxed)
}
