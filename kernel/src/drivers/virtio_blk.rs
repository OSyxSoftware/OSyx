//! virtio-blk (virtio specification section 5.2): a disk of 512-byte
//! sectors behind one request queue.
//!
//! Every request is a descriptor chain: a 16-byte header (type, sector), the
//! data pages and a one-byte status the device writes last. Data goes
//! through DMA pages owned by the driver (bounce buffers), so callers can
//! pass any kernel buffer. One request is in flight per disk at a time.

use super::virtio::{Buffer, Transport, VirtioError, Virtqueue};
use crate::block::{BlockDev, BlockError, SECTOR_SIZE};
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::KMutex;
use alloc::format;
use alloc::string::String;

/// Transitional and modern virtio-blk PCI device ids.
pub const DEVICE_IDS: [u16; 2] = [0x1001, 0x1042];

const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const S_OK: u8 = 0;

/// DMA pages per request: 64 KiB, 128 sectors.
const DATA_PAGES: usize = 16;
const PAGE: usize = PAGE_SIZE as usize;

struct Inner {
    queue: Virtqueue,
    /// Request header at offset 0, status byte at offset 16.
    header: u64,
    data: [u64; DATA_PAGES],
    /// Set after a timeout: the device may still own the buffers.
    failed: bool,
}

pub struct VirtioBlk {
    name: String,
    location: pci::Address,
    capacity: u64,
    read_only: bool,
    can_flush: bool,
    inner: KMutex<Inner>,
    _transport: Transport,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Read,
    Write,
    Flush,
}

impl VirtioBlk {
    pub fn probe(dev: &pci::Device, name: String) -> Result<VirtioBlk, VirtioError> {
        let t = Transport::new(dev)?;
        let features = t.negotiate(F_RO | F_FLUSH)?;
        let queue = t.setup_queue(0)?;
        if (queue.size() as usize) < DATA_PAGES + 2 {
            return Err(VirtioError::NoQueue);
        }
        let header = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        let mut data = [0u64; DATA_PAGES];
        for d in data.iter_mut() {
            *d = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        }
        t.driver_ok();
        let capacity = t.device_config_u64(0);
        crate::kprintln!(
            "virtio-blk {}: {} at {}, {} sectors ({} MiB){}{}, queue {} entries at {:#x}",
            name,
            format!("{:04x}:{:04x}", dev.vendor, dev.device),
            dev.addr,
            capacity,
            capacity >> 11,
            if features & F_RO != 0 { ", read-only" } else { "" },
            if features & F_FLUSH != 0 { ", flush" } else { "" },
            queue.size(),
            queue.phys()
        );
        Ok(VirtioBlk {
            name,
            location: dev.addr,
            capacity,
            read_only: features & F_RO != 0,
            can_flush: features & F_FLUSH != 0,
            inner: KMutex::new(Inner { queue, header, data, failed: false }),
            _transport: t,
        })
    }

    /// Runs one request. `len` bytes of data are staged in the DMA pages.
    fn request(&self, dir: Dir, lba: u64, len: usize, stage: impl FnOnce(&[u64]), fetch: impl FnOnce(&[u64])) -> Result<(), BlockError> {
        let mut g = self.inner.lock();
        if g.failed {
            return Err(BlockError::Io);
        }
        let kind = match dir {
            Dir::Read => T_IN,
            Dir::Write => T_OUT,
            Dir::Flush => T_FLUSH,
        };
        let hv = phys_to_virt(g.header);
        // SAFETY: the header frame belongs to this driver; the device does not
        // own it between requests.
        unsafe {
            core::ptr::write_volatile(hv as *mut u32, kind);
            core::ptr::write_volatile((hv + 4) as *mut u32, 0);
            core::ptr::write_volatile((hv + 8) as *mut u64, lba);
            core::ptr::write_volatile((hv + 16) as *mut u8, 0xFF);
        }
        let pages = len.div_ceil(PAGE);
        stage(&g.data[..pages]);
        let mut chain = [Buffer { phys: 0, len: 0, device_writes: false }; DATA_PAGES + 2];
        chain[0] = Buffer { phys: g.header, len: 16, device_writes: false };
        for i in 0..pages {
            let n = (len - i * PAGE).min(PAGE) as u32;
            chain[1 + i] = Buffer { phys: g.data[i], len: n, device_writes: dir == Dir::Read };
        }
        chain[1 + pages] = Buffer { phys: g.header + 16, len: 1, device_writes: true };
        if let Err(e) = g.queue.run(&chain[..pages + 2]) {
            crate::kprintln!("virtio-blk {}: request failed: {:?}; disk disabled", self.name, e);
            g.failed = true;
            return Err(BlockError::Io);
        }
        // SAFETY: the device has returned the chain.
        let status = unsafe { core::ptr::read_volatile((hv + 16) as *const u8) };
        if status != S_OK {
            return Err(BlockError::Io);
        }
        fetch(&g.data[..pages]);
        Ok(())
    }
}

fn stage_pages(pages: &[u64], buf: &[u8]) {
    for (chunk, &page) in buf.chunks(PAGE).zip(pages) {
        // SAFETY: each DMA page is a whole frame owned by the driver.
        unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(page) as *mut u8, chunk.len()) };
    }
}

fn fetch_pages(pages: &[u64], buf: &mut [u8]) {
    for (chunk, &page) in buf.chunks_mut(PAGE).zip(pages) {
        // SAFETY: as in `stage_pages`; the device has finished with the page.
        unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(page) as *const u8, chunk.as_mut_ptr(), chunk.len()) };
    }
}

impl BlockDev for VirtioBlk {
    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> String {
        format!("virtio-blk at PCI {}", self.location)
    }

    fn sector_count(&self) -> u64 {
        self.capacity
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    fn max_sectors(&self) -> usize {
        DATA_PAGES * PAGE / SECTOR_SIZE
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let len = buf.len();
        self.request(Dir::Read, lba, len, |_| {}, |pages| fetch_pages(pages, buf))
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.request(Dir::Write, lba, buf.len(), |pages| stage_pages(pages, buf), |_| {})
    }

    fn flush(&self) -> Result<(), BlockError> {
        if !self.can_flush {
            return Ok(());
        }
        self.request(Dir::Flush, 0, 0, |_| {}, |_| {})
    }
}
