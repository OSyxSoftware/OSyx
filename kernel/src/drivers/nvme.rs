//! NVMe (NVM Express) driver: PCI class 0x01, subclass 0x08.
//!
//! One controller: an admin queue pair (Identify Controller and
//! Namespace) and one I/O queue pair. Requests are serialised by a sleeping
//! mutex and polled to completion: no interrupts are used. Data goes
//! through driver-owned DMA pages (bounce buffers) announced with PRP1,
//! PRP2 or a PRP list, so callers can pass any kernel buffer.

use crate::arch::timer::{self, Deadline};
use crate::block::{BlockDev, BlockError, SECTOR_SIZE};
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::KMutex;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicUsize, Ordering};

const PAGE: usize = PAGE_SIZE as usize;
/// Bounce buffer size: 64 KiB per request.
const MAX_DATA_PAGES: usize = 16;
const QUEUE_ENTRIES: usize = 32;
const COMMAND_TIMEOUT_MS: u64 = 2000;
const CONTROLLER_TIMEOUT_MS: u64 = 1000;
const MAX_NAMESPACES: u32 = 8;

const R_CAP: u64 = 0x00;
const R_VS: u64 = 0x08;
const R_CC: u64 = 0x14;
const R_CSTS: u64 = 0x1C;
const R_AQA: u64 = 0x24;
const R_ASQ: u64 = 0x28;
const R_ACQ: u64 = 0x30;
const R_DBS: u64 = 0x1000;

const CSTS_RDY: u32 = 1;

const ADMIN_IDENTIFY: u32 = 0x06;
const ADMIN_CREATE_CQ: u32 = 0x05;
const ADMIN_CREATE_SQ: u32 = 0x01;
const IO_WRITE: u32 = 0x01;
const IO_READ: u32 = 0x02;
const IO_FLUSH: u32 = 0x00;

static CONTROLLERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
enum NvmeError {
    Timeout(&'static str),
    Status(u16),
    Device(&'static str),
    NoMemory,
}

impl fmt::Display for NvmeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NvmeError::Timeout(s) => write!(f, "timeout: {s}"),
            NvmeError::Status(sc) => write!(f, "command failed with status {sc:#x}"),
            NvmeError::Device(s) => f.write_str(s),
            NvmeError::NoMemory => f.write_str("out of DMA memory"),
        }
    }
}

/// Volatile access to the controller register BAR.
#[derive(Clone, Copy)]
struct Mmio(u64);

impl Mmio {
    fn r32(self, off: u64) -> u32 {
        // SAFETY: the BAR is mapped uncached by `map_mmio`; offsets are
        // within the BAR as declared by the caller.
        unsafe { read_volatile((self.0 + off) as *const u32) }
    }

    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r32`.
        unsafe { write_volatile((self.0 + off) as *mut u32, v) }
    }

    fn r64(self, off: u64) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }

    fn w64(self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

struct Queue {
    sq: u64,
    cq: u64,
    qsize: usize,
    tail: u32,
    head: u32,
    /// Phase tag expected in the next completion entry.
    phase: bool,
    sq_doorbell: u64,
    cq_doorbell: u64,
}

struct Inner {
    admin: Queue,
    io: Queue,
    data: [u64; MAX_DATA_PAGES],
    /// One page of PRP list entries for transfers beyond two pages.
    prp: u64,
    cid: u16,
    failed: bool,
    can_flush: bool,
    failures: u32,
}

pub struct Nvme {
    mmio: Mmio,
    inner: KMutex<Inner>,
}

/// One namespace, presented as a disk (`nvme0n1`, ...).
pub struct Namespace {
    ctrl: Arc<Nvme>,
    nsid: u32,
    name: String,
    what: String,
    sectors: u64,
}

fn text(frame: u64, off: usize, len: usize) -> String {
    let mut out = String::new();
    for i in 0..len {
        // SAFETY: inside a frame owned by this driver.
        let b = unsafe { read_volatile((phys_to_virt(frame) + (off + i) as u64) as *const u8) };
        if b == 0 {
            break;
        }
        out.push(b as char);
    }
    String::from(out.trim())
}

fn u32_at(frame: u64, off: usize) -> u32 {
    // SAFETY: inside a frame owned by this driver; callers pass aligned offsets.
    unsafe { read_volatile((phys_to_virt(frame) + off as u64) as *const u32) }
}

fn u64_at(frame: u64, off: usize) -> u64 {
    // SAFETY: as in `u32_at`.
    unsafe { read_volatile((phys_to_virt(frame) + off as u64) as *const u64) }
}

fn u8_at(frame: u64, off: usize) -> u8 {
    // SAFETY: as in `u32_at`.
    unsafe { read_volatile((phys_to_virt(frame) + off as u64) as *const u8) }
}

fn wait_csts(m: Mmio, want_ready: bool, timeout_ms: u64) -> bool {
    let deadline = Deadline::after_ms(timeout_ms);
    loop {
        if (m.r32(R_CSTS) & CSTS_RDY != 0) == want_ready {
            return true;
        }
        if deadline.expired() {
            return false;
        }
        timer::pause_tick();
    }
}

/// Probes every NVMe controller and registers its namespaces as disks.
pub fn probe(dev: &pci::Device) -> Vec<Arc<dyn BlockDev>> {
    let index = CONTROLLERS.fetch_add(1, Ordering::Relaxed);
    match start(dev, index) {
        Ok(disks) => disks,
        Err(e) => {
            crate::kprintln!("nvme{}: {:04x}:{:04x} at {}: not usable: {}", index, dev.vendor, dev.device, dev.addr, e);
            Vec::new()
        }
    }
}

fn start(dev: &pci::Device, index: usize) -> Result<Vec<Arc<dyn BlockDev>>, NvmeError> {
    let Some(pci::Bar::Memory { base, size, .. }) = dev.bar(0) else { return Err(NvmeError::Device("BAR0 is not a memory BAR")) };
    if base == 0 || size < 0x1000 {
        return Err(NvmeError::Device("BAR0 unassigned or too small"));
    }
    let virt = crate::mem::map_mmio(base, size.max(0x2000)).map_err(|_| NvmeError::Device("cannot map BAR0"))?;
    dev.addr.write16(0x04, dev.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER | pci::CMD_INTX_DISABLE);
    let m = Mmio(virt);
    if m.r32(R_VS) == u32::MAX {
        return Err(NvmeError::Device("registers read all ones (device not present)"));
    }
    let cap = m.r64(R_CAP);
    let version = m.r32(R_VS);
    let mqes = (cap & 0xFFFF) as usize + 1;
    let qsize = QUEUE_ENTRIES.min(mqes).max(2);
    // Doorbell stride in bytes (CAP.DSTRD, 4.1.5).
    let stride = 4u64 << ((cap >> 32) & 0xF);

    // Reset: disable the controller and wait until it acknowledges.
    m.w32(R_CC, 0);
    if !wait_csts(m, false, CONTROLLER_TIMEOUT_MS) {
        return Err(NvmeError::Timeout("controller did not disable"));
    }

    let Some(admin_sq) = alloc_frame() else { return Err(NvmeError::NoMemory) };
    let Some(admin_cq) = alloc_frame() else { return Err(NvmeError::NoMemory) };
    let Some(io_sq) = alloc_frame() else { return Err(NvmeError::NoMemory) };
    let Some(io_cq) = alloc_frame() else { return Err(NvmeError::NoMemory) };
    let Some(prp) = alloc_frame() else { return Err(NvmeError::NoMemory) };
    let mut data = [0u64; MAX_DATA_PAGES];
    for page in &mut data {
        *page = alloc_frame().ok_or(NvmeError::NoMemory)?;
    }

    m.w32(R_AQA, (qsize as u32 - 1) | ((qsize as u32 - 1) << 16));
    m.w64(R_ASQ, admin_sq);
    m.w64(R_ACQ, admin_cq);
    // NVM command set, 4 KiB pages, 64-byte submission and 16-byte
    // completion entries.
    m.w32(R_CC, 1 | (6 << 16) | (4 << 20));
    if !wait_csts(m, true, CONTROLLER_TIMEOUT_MS) {
        return Err(NvmeError::Timeout("controller did not enable"));
    }

    let ctrl = Arc::new(Nvme {
        mmio: m,
        inner: KMutex::new(Inner {
            admin: Queue { sq: admin_sq, cq: admin_cq, qsize, tail: 0, head: 0, phase: true, sq_doorbell: 0, cq_doorbell: stride },
            io: Queue { sq: io_sq, cq: io_cq, qsize, tail: 0, head: 0, phase: true, sq_doorbell: 2 * stride, cq_doorbell: 3 * stride },
            data,
            prp,
            cid: 1,
            failed: false,
            can_flush: false,
            failures: 0,
        }),
    });

    let ident = alloc_frame().ok_or(NvmeError::NoMemory)?;
    let (model, serial, firmware, nn, vwc) = {
        let mut g = ctrl.inner.lock();
        identify(&ctrl, &mut g, 0, 1, ident)?;
        let model = text(ident, 24, 40);
        let serial = text(ident, 4, 20);
        let firmware = text(ident, 64, 8);
        let nn = u32_at(ident, 316);
        let vwc = u8_at(ident, 325) & 1 != 0;
        g.can_flush = vwc;
        (model, serial, firmware, nn, vwc)
    };
    let what = format!("\"{}\" serial \"{}\"", model, serial);
    crate::kprintln!(
        "nvme{}: {} at {}, firmware {}, NVMe {}.{}.{}, {} namespace(s){}, queue {} entries on BAR0 {:#x} ({} KiB)",
        index,
        what,
        dev.addr,
        firmware,
        version >> 16,
        (version >> 8) & 0xFF,
        version & 0xFF,
        nn,
        if vwc { ", volatile write cache" } else { "" },
        qsize,
        base,
        size >> 10
    );

    // One I/O queue pair, shared by every namespace (serialised by the
    // same mutex).
    {
        let mut g = ctrl.inner.lock();
        let mut cmd = [0u32; 16];
        cmd[0] = ADMIN_CREATE_CQ | ((g.cid as u32) << 16);
        g.cid = g.cid.wrapping_add(1);
        cmd[6] = io_cq as u32;
        cmd[7] = (io_cq >> 32) as u32;
        cmd[10] = 1 | ((qsize as u32 - 1) << 16);
        cmd[11] = 1; // physically contiguous, enabled
        submit(&ctrl, &mut g.admin, &cmd)?;
        let mut cmd = [0u32; 16];
        cmd[0] = ADMIN_CREATE_SQ | ((g.cid as u32) << 16);
        g.cid = g.cid.wrapping_add(1);
        cmd[6] = io_sq as u32;
        cmd[7] = (io_sq >> 32) as u32;
        cmd[10] = 1 | ((qsize as u32 - 1) << 16);
        cmd[11] = 1 | (1 << 16); // enabled, completion queue 1
        submit(&ctrl, &mut g.admin, &cmd)?;
    }

    let mut disks: Vec<Arc<dyn BlockDev>> = Vec::new();
    {
        let mut g = ctrl.inner.lock();
        // Active Namespace ID list (CNS=2): NN alone is not authoritative
        // (some controllers report 0 and expect the list to be used).
        identify(&ctrl, &mut g, 0, 2, ident)?;
        let mut ids = Vec::new();
        for i in 0..1024 {
            let id = u32_at(ident, i * 4);
            if id == 0 {
                break;
            }
            if ids.len() < MAX_NAMESPACES as usize {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            ids.extend(1..=nn.min(MAX_NAMESPACES));
        }
        for &nsid in &ids {
            // Controllers may have sparse namespace ids; a missing one is
            // skipped, not fatal.
            if let Err(e) = identify(&ctrl, &mut g, nsid, 0, ident) {
                crate::kprintln!("nvme{}: namespace {}: identify failed: {}", index, nsid, e);
                continue;
            }
            let nsze = u64_at(ident, 0);
            if nsze == 0 {
                continue;
            }
            let flbas = u8_at(ident, 26) & 0x0F;
            let lbaf = u32_at(ident, 128 + flbas as usize * 4);
            let lbads = (lbaf >> 16) & 0xFF;
            if lbads != 9 {
                crate::kprintln!("nvme{}: namespace {} uses {} byte blocks; only 512 is supported", index, nsid, 1u64 << lbads);
                continue;
            }
            let name = format!("nvme{}n{}", index, nsid);
            crate::kprintln!("nvme{}: {} {} MiB", index, name, nsze >> 11);
            disks.push(Arc::new(Namespace {
                ctrl: ctrl.clone(),
                nsid,
                name: name.clone(),
                what: what.clone(),
                sectors: nsze,
            }) as Arc<dyn BlockDev>);
        }
    }
    Ok(disks)
}

/// Writes one 64-byte command into the queue and rings its doorbell.
fn submit(ctrl: &Nvme, q: &mut Queue, cmd: &[u32; 16]) -> Result<(), NvmeError> {
    let slot = q.tail as usize % q.qsize;
    let base = phys_to_virt(q.sq) + (slot * 64) as u64;
    for (i, &dword) in cmd.iter().enumerate() {
        // SAFETY: inside the queue frame owned by this driver.
        unsafe { write_volatile((base + (i * 4) as u64) as *mut u32, dword) };
    }
    fence(Ordering::Release);
    q.tail = (q.tail + 1) % q.qsize as u32;
    ctrl.mmio.w32(R_DBS + q.sq_doorbell, q.tail);
    wait_completion(ctrl, q, (cmd[0] >> 16) as u16)
}

/// Polls the completion queue until the entry for `cid` arrives.
fn wait_completion(ctrl: &Nvme, q: &mut Queue, cid: u16) -> Result<(), NvmeError> {
    let deadline = Deadline::after_ms(COMMAND_TIMEOUT_MS);
    let base = phys_to_virt(q.cq);
    loop {
        let entry = base + (q.head as u64) * 16;
        // The status dword (with the phase tag) is read last: an entry the
        // controller is still writing is never taken half-new.
        // SAFETY: inside the completion queue frame owned by this driver.
        let dw3 = unsafe { read_volatile((entry + 12) as *const u32) };
        fence(Ordering::Acquire);
        let phase = (dw3 >> 16) & 1 != 0;
        if phase == q.phase {
            let got = dw3 as u16;
            let status = ((dw3 >> 17) & 0x7FFF) as u16;
            q.head = (q.head + 1) % q.qsize as u32;
            if q.head == 0 {
                q.phase = !q.phase;
            }
            ctrl.mmio.w32(R_DBS + q.cq_doorbell, q.head);
            if got != cid {
                return Err(NvmeError::Device("completion for an unexpected command"));
            }
            if status != 0 {
                return Err(NvmeError::Status(status));
            }
            return Ok(());
        }
        if deadline.expired() {
            return Err(NvmeError::Timeout("command completion"));
        }
        timer::pause_tick();
    }
}

fn identify(ctrl: &Nvme, g: &mut Inner, nsid: u32, cns: u32, frame: u64) -> Result<(), NvmeError> {
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_IDENTIFY | ((g.cid as u32) << 16);
    g.cid = g.cid.wrapping_add(1);
    cmd[1] = nsid;
    cmd[6] = frame as u32;
    cmd[7] = (frame >> 32) as u32;
    cmd[10] = cns;
    submit(ctrl, &mut g.admin, &cmd)
}

impl Nvme {
    fn transfer(&self, nsid: u32, write: bool, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.is_empty() || buf.len() > MAX_DATA_PAGES * PAGE || !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(BlockError::InvalidRequest);
        }
        let mut g = self.inner.lock();
        if g.failed {
            return Err(BlockError::Io);
        }
        let pages = buf.len().div_ceil(PAGE);
        if write {
            for (i, chunk) in buf.chunks(PAGE).enumerate() {
                // SAFETY: `g.data[i]` is a whole DMA frame owned by this driver.
                unsafe {
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(g.data[i]) as *mut u8, chunk.len());
                }
            }
        }
        let prp1 = g.data[0];
        let mut prp2 = 0u64;
        if pages == 2 {
            prp2 = g.data[1];
        } else if pages > 2 {
            for i in 1..pages {
                // SAFETY: inside the PRP list frame owned by this driver.
                unsafe { write_volatile((phys_to_virt(g.prp) + ((i - 1) * 8) as u64) as *mut u64, g.data[i]) };
            }
            prp2 = g.prp;
        }
        fence(Ordering::Release);
        let mut cmd = [0u32; 16];
        cmd[0] = (if write { IO_WRITE } else { IO_READ }) | ((g.cid as u32) << 16);
        g.cid = g.cid.wrapping_add(1);
        cmd[1] = nsid;
        cmd[6] = prp1 as u32;
        cmd[7] = (prp1 >> 32) as u32;
        cmd[8] = prp2 as u32;
        cmd[9] = (prp2 >> 32) as u32;
        cmd[10] = lba as u32;
        cmd[11] = (lba >> 32) as u32;
        cmd[12] = (buf.len() / SECTOR_SIZE) as u32 - 1;
        if let Err(e) = submit(self, &mut g.io, &cmd) {
            g.failures += 1;
            if g.failures <= 3 {
                crate::kprintln!(
                    "nvme: {} of {} bytes at LBA {} ({} page(s)) failed: {}",
                    if write { "write" } else { "read" },
                    buf.len(),
                    lba,
                    pages,
                    e
                );
            }
            if matches!(e, NvmeError::Timeout(_) | NvmeError::Device(_)) {
                g.failed = true;
                crate::kprintln!("nvme: I/O queue unusable after {}; disk disabled", e);
            }
            return Err(BlockError::Io);
        }
        if !write {
            for (i, chunk) in buf.chunks_mut(PAGE).enumerate() {
                // SAFETY: as above; the device has finished with the frame.
                unsafe {
                    core::ptr::copy_nonoverlapping(phys_to_virt(g.data[i]) as *const u8, chunk.as_mut_ptr(), chunk.len());
                }
            }
        }
        Ok(())
    }

    fn flush(&self, nsid: u32) -> Result<(), BlockError> {
        let mut g = self.inner.lock();
        if g.failed {
            return Err(BlockError::Io);
        }
        if !g.can_flush {
            return Ok(());
        }
        let mut cmd = [0u32; 16];
        cmd[0] = IO_FLUSH | ((g.cid as u32) << 16);
        g.cid = g.cid.wrapping_add(1);
        cmd[1] = nsid;
        match submit(self, &mut g.io, &cmd) {
            Ok(()) => Ok(()),
            Err(NvmeError::Timeout(_)) => {
                g.failed = true;
                Err(BlockError::Io)
            }
            Err(_) => Err(BlockError::Io),
        }
    }
}

impl BlockDev for Namespace {
    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> String {
        format!("NVMe namespace {} of {} ({} MiB)", self.nsid, self.what, self.sectors >> 11)
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_only(&self) -> bool {
        false
    }

    fn max_sectors(&self) -> usize {
        MAX_DATA_PAGES * PAGE / SECTOR_SIZE
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.ctrl.transfer(self.nsid, false, lba, buf)
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let mut copy = alloc::vec![0u8; buf.len()];
        copy.copy_from_slice(buf);
        self.ctrl.transfer(self.nsid, true, lba, &mut copy)
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.ctrl.flush(self.nsid)
    }
}
