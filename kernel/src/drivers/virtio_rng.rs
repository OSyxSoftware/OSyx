//! virtio-rng / virtio-entropy (virtio specification section 5.4): the
//! device fills driver-supplied buffers with entropy from the host (QEMU
//! reads the host's random source). One request queue, one page at a time.

use super::virtio::{Buffer, Transport, VirtioError, Virtqueue};
use crate::mem::{alloc_frame, phys_to_virt};
use crate::pci;
use crate::sync::KMutex;

/// Transitional and modern virtio-rng PCI device ids.
pub const DEVICE_IDS: [u16; 2] = [0x1005, 0x1044];

pub struct VirtioRng {
    queue: KMutex<(Virtqueue, u64)>,
    pub location: pci::Address,
    _transport: Transport,
}

impl VirtioRng {
    pub fn probe(dev: &pci::Device) -> Result<VirtioRng, VirtioError> {
        let t = Transport::new(dev)?;
        t.negotiate(0)?;
        let queue = t.setup_queue(0)?;
        let page = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        t.driver_ok();
        Ok(VirtioRng { queue: KMutex::new((queue, page)), location: dev.addr, _transport: t })
    }

    /// Fills `out` with device entropy; returns how many bytes the device
    /// actually provided (it may return fewer than asked).
    pub fn read(&self, out: &mut [u8]) -> Result<usize, VirtioError> {
        let mut g = self.queue.lock();
        let page = g.1;
        let mut done = 0;
        while done < out.len() {
            let want = (out.len() - done).min(4096);
            let n = g.0.run(&[Buffer { phys: page, len: want as u32, device_writes: true }])? as usize;
            if n == 0 {
                break;
            }
            let n = n.min(want);
            // SAFETY: the page belongs to this driver and the device has
            // returned it.
            unsafe {
                core::ptr::copy_nonoverlapping(phys_to_virt(page) as *const u8, out[done..].as_mut_ptr(), n);
                core::ptr::write_bytes(phys_to_virt(page) as *mut u8, 0, 4096);
            }
            done += n;
        }
        Ok(done)
    }
}
