//! DMA memory for the xHCI driver.
//!
//! Every structure the controller reads or writes lives in its own 4 KiB
//! frame from the frame allocator: frames are physically contiguous,
//! page aligned (which satisfies every xHCI alignment rule: 64 bytes for
//! contexts and the DCBAA, 16 for rings, page size for scratchpads) and
//! never cross a 64 KiB boundary. x86 DMA is cache coherent, so the
//! kernel reaches the frames through the (write-back) direct map; ordering
//! towards the device is enforced with fences before doorbell writes and
//! when a TRB is handed over. There is no IOMMU: physical addresses are
//! bus addresses. A frame is returned to the allocator only when the
//! controller can no longer use it (after the slot or controller that
//! referenced it was disabled).

use crate::mem::{alloc_frame, free_frame, phys_to_virt, PAGE_SIZE};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
use osyx_usb::xhci::ring::TrbMemory;
use osyx_usb::xhci::trb::Trb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    OutOfMemory,
    /// The controller cannot address the frame (no 64-bit addressing).
    Unreachable,
}

/// One zeroed, page-aligned frame shared with the controller.
pub struct DmaPage {
    phys: u64,
    virt: u64,
}

impl DmaPage {
    /// `ac64`: the controller can address 64-bit memory.
    pub fn new(ac64: bool) -> Result<DmaPage, DmaError> {
        let phys = alloc_frame().ok_or(DmaError::OutOfMemory)?;
        if !ac64 && phys + PAGE_SIZE > 1 << 32 {
            let _ = free_frame(phys);
            return Err(DmaError::Unreachable);
        }
        Ok(DmaPage { phys, virt: phys_to_virt(phys) })
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn read32(&self, off: usize) -> u32 {
        assert!(off + 4 <= PAGE_SIZE as usize && off.is_multiple_of(4));
        // SAFETY: inside this page, aligned; the page is ours.
        unsafe { read_volatile((self.virt + off as u64) as *const u32) }
    }

    pub fn write32(&self, off: usize, v: u32) {
        assert!(off + 4 <= PAGE_SIZE as usize && off.is_multiple_of(4));
        // SAFETY: as in `read32`.
        unsafe { write_volatile((self.virt + off as u64) as *mut u32, v) }
    }

    pub fn write64(&self, off: usize, v: u64) {
        self.write32(off, v as u32);
        self.write32(off + 4, (v >> 32) as u32);
    }

    /// Writes context dwords at a byte offset.
    pub fn write_dwords(&self, off: usize, d: &[u32]) {
        for (i, &v) in d.iter().enumerate() {
            self.write32(off + 4 * i, v);
        }
    }

    pub fn read_dwords(&self, off: usize, out: &mut [u32]) {
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.read32(off + 4 * i);
        }
    }

    pub fn zero(&self) {
        // SAFETY: the whole page is ours.
        unsafe { core::ptr::write_bytes(self.virt as *mut u8, 0, PAGE_SIZE as usize) };
    }

    /// Copies `out.len()` bytes from the start of the page.
    pub fn copy_out(&self, out: &mut [u8]) {
        let n = out.len().min(PAGE_SIZE as usize);
        fence(Ordering::Acquire);
        // SAFETY: `n` bytes inside the page.
        unsafe { core::ptr::copy_nonoverlapping(self.virt as *const u8, out.as_mut_ptr(), n) };
    }

    pub fn copy_in(&self, data: &[u8]) {
        let n = data.len().min(PAGE_SIZE as usize);
        // SAFETY: `n` bytes inside the page.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.virt as *mut u8, n) };
        fence(Ordering::Release);
    }
}

impl Drop for DmaPage {
    fn drop(&mut self) {
        let _ = free_frame(self.phys);
    }
}

/// A one-page ring segment (256 TRBs).
pub struct DmaRing {
    page: DmaPage,
}

impl DmaRing {
    pub fn new(ac64: bool) -> Result<DmaRing, DmaError> {
        Ok(DmaRing { page: DmaPage::new(ac64)? })
    }
}

impl TrbMemory for DmaRing {
    fn slots(&self) -> usize {
        (PAGE_SIZE / 16) as usize
    }

    fn phys(&self) -> u64 {
        self.page.phys
    }

    fn read(&self, i: usize) -> Trb {
        let base = i * 16;
        // The control dword (with the cycle bit) first: the rest of the TRB
        // is only read after it, so a TRB the controller is still writing is
        // never seen half-new.
        let control = self.page.read32(base + 12);
        fence(Ordering::Acquire);
        Trb([self.page.read32(base), self.page.read32(base + 4), self.page.read32(base + 8), control])
    }

    fn write(&mut self, i: usize, t: Trb) {
        let base = i * 16;
        self.page.write32(base, t.0[0]);
        self.page.write32(base + 4, t.0[1]);
        self.page.write32(base + 8, t.0[2]);
        // The control dword (cycle bit) hands the TRB over: everything else
        // must be visible first.
        fence(Ordering::Release);
        self.page.write32(base + 12, t.0[3]);
    }
}
