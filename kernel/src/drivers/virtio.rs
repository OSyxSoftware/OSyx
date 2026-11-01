//! Virtio 1.x devices on PCI (the "modern" transport of the virtio
//! specification, section 4.1) and split virtqueues (section 2.7).
//!
//! The driver side is deliberately small: queues are polled (no interrupts,
//! `VIRTQ_AVAIL_F_NO_INTERRUPT` is set) and each queue carries one request
//! at a time, serialised by its owner. Legacy (pre-1.0) devices are not
//! supported; QEMU's transitional devices expose the modern interface too.

use crate::mem::{self, alloc_frame, phys_to_virt};
use crate::pci::{self, Bar};
use crate::arch::timer::Deadline;
use crate::sched;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

pub const VENDOR: u16 = 0x1AF4;

const CAP_VENDOR: u8 = 0x09;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_DEVICE: u8 = 4;

pub const STATUS_ACKNOWLEDGE: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;
pub const STATUS_NEEDS_RESET: u8 = 0x40;
pub const STATUS_FAILED: u8 = 0x80;

pub const F_VERSION_1: u64 = 1 << 32;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const AVAIL_F_NO_INTERRUPT: u16 = 1;
const USED_F_NO_NOTIFY: u16 = 1;

/// Largest queue this driver sets up (all rings fit one 4 KiB frame).
const MAX_QUEUE: u16 = 64;
/// Largest asynchronous queue (each ring in its own frame).
const MAX_ASYNC_QUEUE: u16 = 256;
const AVAIL_OFFSET: u64 = 1024;
const USED_OFFSET: u64 = 2048;

/// A request that takes longer than this is treated as a device failure.
const TIMEOUT_MS: u64 = 5000;
/// Checks of the used ring before a waiting request starts yielding. Every
/// request runs under the global kernel lock, so a long spin here stalls
/// unrelated threads (including the UI): yield early and let them run while
/// the device finishes.
const SPIN_POLLS: u32 = 1_000;

// Common configuration register offsets.
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const MSIX_CONFIG: u64 = 0x10;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    MissingCapability(&'static str),
    BarUnassigned,
    Map,
    NoVersion1,
    FeaturesRejected,
    NoQueue,
    OutOfMemory,
    Timeout,
}

/// Volatile access to a mapped register block.
#[derive(Clone, Copy)]
struct Regs(u64);

impl Regs {
    fn r8(self, off: u64) -> u8 {
        // SAFETY: `self.0` is a live uncached MMIO mapping covering `off`.
        unsafe { read_volatile((self.0 + off) as *const u8) }
    }
    fn r16(self, off: u64) -> u16 {
        // SAFETY: as in `r8`.
        unsafe { read_volatile((self.0 + off) as *const u16) }
    }
    fn r32(self, off: u64) -> u32 {
        // SAFETY: as in `r8`.
        unsafe { read_volatile((self.0 + off) as *const u32) }
    }
    fn w8(self, off: u64, v: u8) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.0 + off) as *mut u8, v) }
    }
    fn w16(self, off: u64, v: u16) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.0 + off) as *mut u16, v) }
    }
    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.0 + off) as *mut u32, v) }
    }
    fn w64(self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

/// The register blocks of one virtio PCI function.
pub struct Transport {
    common: Regs,
    notify: Regs,
    notify_multiplier: u32,
    device: Regs,
}

impl Transport {
    /// Locates and maps the common, notify and device configuration
    /// structures, and enables memory decoding and bus mastering.
    pub fn new(dev: &pci::Device) -> Result<Transport, VirtioError> {
        let mut bars: [Option<Bar>; 6] = [None; 6];
        let (mut common, mut notify, mut device) = (None, None, None);
        let mut multiplier = 0;
        for (off, id) in dev.capabilities() {
            if id != CAP_VENDOR {
                continue;
            }
            let cfg_type = dev.addr.read8(off + 3);
            let bar = dev.addr.read8(off + 4);
            if bar > 5 || !matches!(cfg_type, CFG_COMMON | CFG_NOTIFY | CFG_DEVICE) {
                continue;
            }
            let offset = dev.addr.read32(off + 8) as u64;
            let length = dev.addr.read32(off + 12) as u64;
            if bars[bar as usize].is_none() {
                bars[bar as usize] = dev.bar(bar);
            }
            let Some(Bar::Memory { base, size, .. }) = bars[bar as usize] else { continue };
            if base == 0 {
                return Err(VirtioError::BarUnassigned);
            }
            if length == 0 || offset.checked_add(length).is_none_or(|end| end > size) {
                continue;
            }
            let slot = match cfg_type {
                CFG_COMMON => &mut common,
                CFG_NOTIFY => {
                    multiplier = dev.addr.read32(off + 16);
                    &mut notify
                }
                _ => &mut device,
            };
            if slot.is_none() {
                *slot = Some((base + offset, length));
            }
        }
        let map = |r: Option<(u64, u64)>, what| -> Result<Regs, VirtioError> {
            let (phys, len) = r.ok_or(VirtioError::MissingCapability(what))?;
            mem::map_mmio(phys, len).map(Regs).map_err(|_| VirtioError::Map)
        };
        let t = Transport {
            common: map(common, "common configuration")?,
            notify: map(notify, "notification area")?,
            notify_multiplier: multiplier,
            device: map(device, "device configuration")?,
        };
        let cmd = dev.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER | pci::CMD_INTX_DISABLE;
        dev.addr.write16(0x04, cmd);
        Ok(t)
    }

    pub fn status(&self) -> u8 {
        self.common.r8(DEVICE_STATUS)
    }

    pub fn set_status(&self, s: u8) {
        self.common.w8(DEVICE_STATUS, s);
    }

    /// Resets the device and waits until the reset has completed.
    pub fn reset(&self) {
        self.set_status(0);
        let deadline = Deadline::after_ms(TIMEOUT_MS);
        while self.status() != 0 && !deadline.expired() {
            core::hint::spin_loop();
        }
    }

    /// Reset, acknowledge and negotiate: the device must offer
    /// `F_VERSION_1`; of `wanted` only what it offers is accepted. Returns
    /// the accepted features.
    pub fn negotiate(&self, wanted: u64) -> Result<u64, VirtioError> {
        self.reset();
        self.set_status(STATUS_ACKNOWLEDGE);
        self.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        let mut offered = 0u64;
        for half in 0..2u32 {
            self.common.w32(DEVICE_FEATURE_SELECT, half);
            offered |= (self.common.r32(DEVICE_FEATURE) as u64) << (32 * half);
        }
        if offered & F_VERSION_1 == 0 {
            self.set_status(STATUS_FAILED);
            return Err(VirtioError::NoVersion1);
        }
        let accepted = offered & (wanted | F_VERSION_1);
        for half in 0..2u32 {
            self.common.w32(DRIVER_FEATURE_SELECT, half);
            self.common.w32(DRIVER_FEATURE, (accepted >> (32 * half)) as u32);
        }
        self.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
        if self.status() & STATUS_FEATURES_OK == 0 {
            self.set_status(STATUS_FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        Ok(accepted)
    }

    /// Sets up queue `index` with up to `MAX_QUEUE` entries.
    pub fn setup_queue(&self, index: u16) -> Result<Virtqueue, VirtioError> {
        self.common.w16(QUEUE_SELECT, index);
        let max = self.common.r16(QUEUE_SIZE);
        if max == 0 {
            return Err(VirtioError::NoQueue);
        }
        let size = max.min(MAX_QUEUE);
        let frame = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        self.common.w16(QUEUE_SIZE, size);
        self.common.w64(QUEUE_DESC, frame);
        self.common.w64(QUEUE_DRIVER, frame + AVAIL_OFFSET);
        self.common.w64(QUEUE_DEVICE, frame + USED_OFFSET);
        let notify_off = self.common.r16(QUEUE_NOTIFY_OFF) as u64;
        let q = Virtqueue {
            index,
            size,
            phys: frame,
            virt: phys_to_virt(frame),
            notify: self.notify.0 + notify_off * self.notify_multiplier as u64,
            avail_idx: 0,
            used_idx: 0,
        };
        // SAFETY: the avail ring lies inside the frame just allocated.
        unsafe { write_volatile((q.virt + AVAIL_OFFSET) as *mut u16, AVAIL_F_NO_INTERRUPT) };
        self.common.w16(QUEUE_ENABLE, 1);
        Ok(q)
    }

    /// Sets up queue `index` for asynchronous use: up to `MAX_QUEUE`
    /// descriptors handed out from a free list, completions signalled
    /// through MSI-X table entry `vector` (`None`: no interrupt, the driver
    /// polls).
    pub fn setup_async_queue(&self, index: u16, vector: Option<u16>) -> Result<AsyncQueue, VirtioError> {
        self.common.w16(QUEUE_SELECT, index);
        let max = self.common.r16(QUEUE_SIZE);
        if max == 0 {
            return Err(VirtioError::NoQueue);
        }
        // Up to 256 entries: descriptors (16 bytes each), the available ring
        // and the used ring get one frame each.
        let size = max.min(MAX_ASYNC_QUEUE);
        let desc = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        let avail = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        let used = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        self.common.w16(QUEUE_SIZE, size);
        self.common.w64(QUEUE_DESC, desc);
        self.common.w64(QUEUE_DRIVER, avail);
        self.common.w64(QUEUE_DEVICE, used);
        let mut irq = false;
        if let Some(v) = vector {
            self.common.w16(QUEUE_MSIX_VECTOR, v);
            // The device answers NO_VECTOR if it could not take the entry.
            irq = self.common.r16(QUEUE_MSIX_VECTOR) == v;
        }
        let notify_off = self.common.r16(QUEUE_NOTIFY_OFF) as u64;
        // SAFETY: the avail ring is the frame just allocated.
        unsafe { write_volatile(phys_to_virt(avail) as *mut u16, if irq { 0 } else { AVAIL_F_NO_INTERRUPT }) };
        let next: alloc::vec::Vec<u16> = (1..=size).collect();
        self.common.w16(QUEUE_ENABLE, 1);
        Ok(AsyncQueue {
            index,
            size,
            phys: desc,
            desc: phys_to_virt(desc),
            avail: phys_to_virt(avail),
            used: phys_to_virt(used),
            notify: self.notify.0 + notify_off * self.notify_multiplier as u64,
            avail_idx: 0,
            used_idx: 0,
            free_head: 0,
            free: size,
            next,
            chain_len: alloc::vec![0; size as usize],
        })
    }

    /// Routes configuration-change interrupts to MSI-X entry `vector`.
    pub fn set_config_vector(&self, vector: u16) -> bool {
        self.common.w16(MSIX_CONFIG, vector);
        self.common.r16(MSIX_CONFIG) == vector
    }

    pub fn driver_ok(&self) {
        self.set_status(self.status() | STATUS_DRIVER_OK);
    }

    /// The device asks to be reset after an internal error.
    pub fn needs_reset(&self) -> bool {
        self.status() & (STATUS_NEEDS_RESET | STATUS_FAILED) != 0
    }

    pub fn device_config_u8(&self, off: u64) -> u8 {
        self.device.r8(off)
    }

    pub fn device_config_u16(&self, off: u64) -> u16 {
        self.device.r16(off)
    }

    pub fn device_config_u32(&self, off: u64) -> u32 {
        self.device.r32(off)
    }

    pub fn write_device_config_u32(&self, off: u64, v: u32) {
        self.device.w32(off, v)
    }

    pub fn device_config_u64(&self, off: u64) -> u64 {
        // Re-read until the 64-bit value is consistent (config generation);
        // a device whose generation never settles gets its last reading.
        let mut v = 0;
        for _ in 0..1000 {
            let generation = self.common.r8(0x15);
            v = self.device.r32(off) as u64 | (self.device.r32(off + 4) as u64) << 32;
            if self.common.r8(0x15) == generation {
                break;
            }
        }
        v
    }
}

/// One buffer of a request: (physical address, length, device writes it).
#[derive(Clone, Copy)]
pub struct Buffer {
    pub phys: u64,
    pub len: u32,
    pub device_writes: bool,
}

/// A split virtqueue whose rings live in one frame: descriptors at 0, the
/// available ring at 1024, the used ring at 2048.
pub struct Virtqueue {
    index: u16,
    size: u16,
    phys: u64,
    virt: u64,
    notify: u64,
    avail_idx: u16,
    used_idx: u16,
}

// SAFETY: the queue is owned by one driver instance and only used under its lock.
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Submits one descriptor chain and polls (briefly spinning, then
    /// yielding the CPU) until the device has used it. Returns the number of
    /// bytes the device wrote.
    pub fn run(&mut self, chain: &[Buffer]) -> Result<u32, VirtioError> {
        assert!(!chain.is_empty() && chain.len() <= self.size as usize, "descriptor chain does not fit the queue");
        for (i, b) in chain.iter().enumerate() {
            let mut flags = if b.device_writes { DESC_F_WRITE } else { 0 };
            if i + 1 < chain.len() {
                flags |= DESC_F_NEXT;
            }
            let d = self.virt + i as u64 * 16;
            // SAFETY: descriptor `i` lies in the queue frame (i < size).
            unsafe {
                write_volatile(d as *mut u64, b.phys);
                write_volatile((d + 8) as *mut u32, b.len);
                write_volatile((d + 12) as *mut u16, flags);
                write_volatile((d + 14) as *mut u16, (i + 1) as u16);
            }
        }
        let avail = self.virt + AVAIL_OFFSET;
        let slot = (self.avail_idx % self.size) as u64;
        // SAFETY: the avail ring lies in the queue frame.
        unsafe { write_volatile((avail + 4 + slot * 2) as *mut u16, 0) };
        self.avail_idx = self.avail_idx.wrapping_add(1);
        fence(Ordering::SeqCst);
        // SAFETY: as above.
        unsafe { write_volatile((avail + 2) as *mut u16, self.avail_idx) };
        fence(Ordering::SeqCst);
        // SAFETY: `notify` is this queue's doorbell in the mapped notify area.
        unsafe { write_volatile(self.notify as *mut u16, self.index) };

        let used = self.virt + USED_OFFSET;
        let deadline = Deadline::after_ms(TIMEOUT_MS);
        let mut polls = 0;
        loop {
            fence(Ordering::SeqCst);
            // SAFETY: the used ring lies in the queue frame.
            let idx = unsafe { read_volatile((used + 2) as *const u16) };
            if idx != self.used_idx {
                let slot = (self.used_idx % self.size) as u64;
                // SAFETY: as above.
                let written = unsafe { read_volatile((used + 4 + slot * 8 + 4) as *const u32) };
                self.used_idx = self.used_idx.wrapping_add(1);
                return Ok(written);
            }
            if deadline.expired() {
                return Err(VirtioError::Timeout);
            }
            // Most requests complete within microseconds: poll briefly
            // before yielding, since a yield to a busy thread costs a whole
            // time slice.
            if polls < SPIN_POLLS {
                polls += 1;
                core::hint::spin_loop();
            } else {
                sched::yield_now();
            }
        }
    }

    /// Physical address of the queue frame (for diagnostics).
    pub fn phys(&self) -> u64 {
        self.phys
    }
}

/// A split virtqueue with many requests in flight: descriptors come from a
/// free list, the driver adds chains and later collects completions from
/// the used ring (after an interrupt, or by polling).
///
/// Ownership: a buffer belongs to the device from `add` until its chain is
/// returned by `pop_used`; the driver must neither touch nor free it in
/// between.
pub struct AsyncQueue {
    index: u16,
    size: u16,
    phys: u64,
    desc: u64,
    avail: u64,
    used: u64,
    notify: u64,
    avail_idx: u16,
    used_idx: u16,
    free_head: u16,
    free: u16,
    next: alloc::vec::Vec<u16>,
    chain_len: alloc::vec::Vec<u16>,
}

// SAFETY: owned by one driver instance and only used under its lock.
unsafe impl Send for AsyncQueue {}

impl AsyncQueue {
    pub fn size(&self) -> u16 {
        self.size
    }

    pub fn free(&self) -> u16 {
        self.free
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    /// Makes a descriptor chain available; returns its head (the token
    /// `pop_used` reports), or `None` if not enough descriptors are free.
    /// Call `kick` to tell the device.
    pub fn add(&mut self, chain: &[Buffer]) -> Option<u16> {
        if chain.is_empty() || chain.len() > self.free as usize {
            return None;
        }
        let head = self.free_head;
        let mut d = head;
        for (i, b) in chain.iter().enumerate() {
            let mut flags = if b.device_writes { DESC_F_WRITE } else { 0 };
            let last = i + 1 == chain.len();
            let next = self.next[d as usize];
            if !last {
                flags |= DESC_F_NEXT;
            }
            let addr = self.desc + d as u64 * 16;
            // SAFETY: descriptor `d` (< size) lies in the queue frame and is free.
            unsafe {
                write_volatile(addr as *mut u64, b.phys);
                write_volatile((addr + 8) as *mut u32, b.len);
                write_volatile((addr + 12) as *mut u16, flags);
                write_volatile((addr + 14) as *mut u16, next);
            }
            if !last {
                d = next;
            } else {
                self.free_head = next;
            }
        }
        self.free -= chain.len() as u16;
        self.chain_len[head as usize] = chain.len() as u16;
        let avail = self.avail;
        let slot = (self.avail_idx % self.size) as u64;
        // SAFETY: the avail ring frame holds 4 + 2 * size bytes.
        unsafe { write_volatile((avail + 4 + slot * 2) as *mut u16, head) };
        // The ring entry must be visible before the index that publishes it.
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        // SAFETY: as above.
        unsafe { write_volatile((avail + 2) as *mut u16, self.avail_idx) };
        Some(head)
    }

    /// Notifies the device of new available buffers, unless it asked not to
    /// be (`VIRTQ_USED_F_NO_NOTIFY`, set while it is still working through
    /// the ring; it reads the ring again before clearing it, spec 2.7.7.2).
    /// A doorbell write is an exit to the hypervisor, so this matters at
    /// thousands of packets a second.
    pub fn kick(&self) {
        // The new avail index must be visible before the flag is read.
        fence(Ordering::SeqCst);
        // SAFETY: the used ring (flags at offset 0) lies in the queue frame.
        if unsafe { read_volatile(self.used as *const u16) } & USED_F_NO_NOTIFY != 0 {
            return;
        }
        // SAFETY: `notify` is this queue's doorbell in the mapped notify area.
        unsafe { write_volatile(self.notify as *mut u16, self.index) };
    }

    /// The next completed chain: (head token, bytes the device wrote). Its
    /// descriptors return to the free list.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        let used = self.used;
        fence(Ordering::SeqCst);
        // SAFETY: the used ring lies in the queue frame.
        let idx = unsafe { read_volatile((used + 2) as *const u16) };
        if idx == self.used_idx {
            return None;
        }
        // Read the element only after seeing the index.
        fence(Ordering::SeqCst);
        let slot = (self.used_idx % self.size) as u64;
        // SAFETY: as above.
        let (id, len) = unsafe {
            (read_volatile((used + 4 + slot * 8) as *const u32), read_volatile((used + 4 + slot * 8 + 4) as *const u32))
        };
        self.used_idx = self.used_idx.wrapping_add(1);
        let head = id as u16;
        if head >= self.size || self.chain_len[head as usize] == 0 {
            // A device bug: an id we never handed out. Ignore it rather
            // than corrupt the free list.
            crate::kprintln!("virtio: queue {} used an unknown descriptor {}", self.index, id);
            return self.pop_used();
        }
        // Walk the chain to its tail and splice it onto the free list.
        let n = self.chain_len[head as usize];
        self.chain_len[head as usize] = 0;
        let mut tail = head;
        for _ in 1..n {
            tail = self.next[tail as usize];
        }
        self.next[tail as usize] = self.free_head;
        self.free_head = head;
        self.free += n;
        Some((head, len))
    }
}
