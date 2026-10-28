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

/// Largest DMA run per request: 1 MiB. The queue size caps it; probes with a
/// smaller queue use fewer pages.
const MAX_DATA_PAGES: usize = 256;
const PAGE: usize = PAGE_SIZE as usize;

// TEMP per-request timing.
static OPS: [core::sync::atomic::AtomicU64; 3] = [const { core::sync::atomic::AtomicU64::new(0) }; 3];
static OP_CYCLES: [core::sync::atomic::AtomicU64; 3] = [const { core::sync::atomic::AtomicU64::new(0) }; 3];
static OP_MAX: [core::sync::atomic::AtomicU64; 3] = [const { core::sync::atomic::AtomicU64::new(0) }; 3];
static OP_BYTES: [core::sync::atomic::AtomicU64; 3] = [const { core::sync::atomic::AtomicU64::new(0) }; 3];
static OP_WAIT: [core::sync::atomic::AtomicU64; 3] = [const { core::sync::atomic::AtomicU64::new(0) }; 3];

fn slot(dir: Dir) -> usize {
    match dir {
        Dir::Read => 0,
        Dir::Write => 1,
        Dir::Flush => 2,
    }
}

/// (calls, total TSC cycles, max cycles, bytes, queue-wait cycles) per
/// direction: read, write, flush.
pub fn request_stats() -> [(u64, u64, u64, u64, u64); 3] {
    core::array::from_fn(|i| {
        let l = |a: &core::sync::atomic::AtomicU64| a.load(core::sync::atomic::Ordering::Relaxed);
        (l(&OPS[i]), l(&OP_CYCLES[i]), l(&OP_MAX[i]), l(&OP_BYTES[i]), l(&OP_WAIT[i]))
    })
}

struct Inner {
    queue: Virtqueue,
    /// Request header at offset 0, status byte at offset 16.
    header: u64,
    data: [u64; MAX_DATA_PAGES],
    /// Descriptor chain template: header, data pages, status. Kept off the
    /// stack because the request path runs on deep write-back stacks.
    chain: alloc::boxed::Box<[Buffer]>,
    /// DMA pages actually used by one request.
    pages: usize,
    /// Set after a timeout: the device may still own the buffers.
    failed: bool,
}

pub struct VirtioBlk {
    name: String,
    location: pci::Address,
    capacity: u64,
    read_only: bool,
    can_flush: bool,
    data_pages: usize,
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
        let data_pages = (queue.size() as usize).saturating_sub(2).min(MAX_DATA_PAGES);
        if data_pages < 2 {
            return Err(VirtioError::NoQueue);
        }
        let header = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        let mut data = [0u64; MAX_DATA_PAGES];
        for d in data.iter_mut().take(data_pages) {
            *d = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        }
        let mut chain = alloc::vec::Vec::with_capacity(data_pages + 2);
        chain.push(Buffer { phys: header, len: 16, device_writes: false });
        for &page in &data[..data_pages] {
            chain.push(Buffer { phys: page, len: PAGE as u32, device_writes: false });
        }
        chain.push(Buffer { phys: header + 16, len: 1, device_writes: true });
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
            data_pages,
            inner: KMutex::new(Inner {
                queue,
                header,
                data,
                chain: chain.into_boxed_slice(),
                pages: data_pages,
                failed: false,
            }),
            _transport: t,
        })
    }

    /// Runs one request. `len` bytes of data are staged in the DMA pages.
    fn request(&self, dir: Dir, lba: u64, len: usize, stage: impl FnOnce(&[u64]), fetch: impl FnOnce(&[u64])) -> Result<(), BlockError> {
        let t0 = crate::arch::timer::tsc();
        let mut g = self.inner.lock();
        let t1 = crate::arch::timer::tsc();
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
        if pages > g.pages {
            return Err(BlockError::InvalidRequest);
        }
        let inner = &mut *g;
        stage(&inner.data[..pages]);
        let header = inner.header;
        let chain = &mut inner.chain[..pages + 2];
        chain[0] = Buffer { phys: header, len: 16, device_writes: false };
        for i in 0..pages {
            let n = (len - i * PAGE).min(PAGE) as u32;
            chain[1 + i] = Buffer { phys: inner.data[i], len: n, device_writes: dir == Dir::Read };
        }
        chain[1 + pages] = Buffer { phys: header + 16, len: 1, device_writes: true };
        if let Err(e) = inner.queue.run(chain) {
            crate::kprintln!("virtio-blk {}: request failed: {:?}; disk disabled", self.name, e);
            inner.failed = true;
            return Err(BlockError::Io);
        }
        // SAFETY: the device has returned the chain.
        let status = unsafe { core::ptr::read_volatile((hv + 16) as *const u8) };
        if status != S_OK {
            return Err(BlockError::Io);
        }
        fetch(&inner.data[..pages]);
        let dt = crate::arch::timer::tsc().wrapping_sub(t0);
        let s = slot(dir);
        OPS[s].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        OP_CYCLES[s].fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
        OP_MAX[s].fetch_max(dt, core::sync::atomic::Ordering::Relaxed);
        OP_BYTES[s].fetch_add(len as u64, core::sync::atomic::Ordering::Relaxed);
        OP_WAIT[s].fetch_add(t1.wrapping_sub(t0), core::sync::atomic::Ordering::Relaxed);
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
        self.data_pages * PAGE / SECTOR_SIZE
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
