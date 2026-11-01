//! virtio-net (virtio specification 1.x, section 5.1) over the modern PCI
//! transport.
//!
//! Feature set: `VIRTIO_NET_F_MAC`, `VIRTIO_NET_F_STATUS` and
//! `VIRTIO_NET_F_MTU` when offered; no offloads (checksums are computed by
//! the stack), no mergeable receive buffers, one queue pair. With
//! `VIRTIO_F_VERSION_1` every frame is preceded by a 12-byte
//! `virtio_net_hdr` (flags, gso_type, hdr_len, gso_size, csum_start,
//! csum_offset, num_buffers), all zero on transmit.
//!
//! Buffers: each receive and transmit slot owns one 4 KiB frame (DMA
//! memory never handed to anyone else). A receive buffer belongs to the
//! device from `add` until the used ring returns it; its frame is copied
//! out and the slot is posted again. Transmit frames are copied into a free
//! slot, which comes back when the device has sent it.
//!
//! Interrupts: one MSI-X vector for both queues and configuration changes;
//! the handler only wakes the network thread. Without MSI-X the thread
//! polls every timer tick.

use super::virtio::{AsyncQueue, Buffer, Transport, VirtioError};
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_net::wire::MacAddr;

/// Transitional and modern virtio-net PCI device ids.
pub const DEVICE_IDS: [u16; 2] = [0x1000, 0x1041];

const F_MTU: u64 = 1 << 3;
const F_MAC: u64 = 1 << 5;
const F_STATUS: u64 = 1 << 16;

const STATUS_LINK_UP: u16 = 1;
const HDR_LEN: usize = 12;
const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
/// Largest frame accepted from the stack (header + 1500 + VLAN tag).
const MAX_FRAME: usize = 1522;

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_full: u64,
    pub resets: u64,
}

struct Queues {
    rx: AsyncQueue,
    tx: AsyncQueue,
    /// Frame of each receive descriptor head (indexed by token).
    rx_frames: Vec<u64>,
    /// Frame per transmit descriptor; `tx_busy` marks those in flight.
    tx_frames: Vec<u64>,
    tx_busy: Vec<bool>,
}

pub struct VirtioNet {
    pub name: alloc::string::String,
    pub location: pci::Address,
    pub mac: MacAddr,
    pub mtu: usize,
    features: u64,
    transport: Transport,
    vector: Option<u8>,
    queues: IrqSpinLock<Queues>,
    counters: IrqSpinLock<Counters>,
}

/// Interrupts taken by all virtio-net devices (diagnostics).
pub static INTERRUPTS: AtomicU64 = AtomicU64::new(0);

fn irq_handler(_: u64) {
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    crate::net::wake();
}

impl VirtioNet {
    pub fn probe(dev: &pci::Device, name: &str) -> Result<VirtioNet, VirtioError> {
        let t = Transport::new(dev)?;
        let features = t.negotiate(F_MAC | F_STATUS | F_MTU)?;
        // One vector for everything; queues fall back to polling without it.
        let vector = crate::irq::allocate(irq_handler, 0).and_then(|v| {
            let (address, data) = crate::arch::apic::msi_message(v);
            match dev.enable_msix(address, data) {
                Ok(()) => Some(v),
                Err(why) => {
                    crate::kprintln!("virtio-net {}: no MSI-X ({}), polling", name, why);
                    crate::irq::free(v);
                    None
                }
            }
        });
        let entry = vector.map(|_| 0u16);
        if entry.is_some() && features & F_STATUS != 0 {
            t.set_config_vector(0);
        }
        let queues = Self::build_queues(&t, entry)?;
        t.driver_ok();
        let mut mac = [0u8; 6];
        if features & F_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = t.device_config_u8(i as u64);
            }
        } else {
            // No MAC from the device: a locally administered random one.
            let mut r = [0u8; 6];
            let _ = crate::random::fill(&mut r);
            mac = [0x02, r[1], r[2], r[3], r[4], r[5]];
        }
        let mtu = if features & F_MTU != 0 { (t.device_config_u16(10) as usize).clamp(576, 1500) } else { 1500 };
        let n = VirtioNet {
            name: name.into(),
            location: dev.addr,
            mac: MacAddr(mac),
            mtu,
            features,
            transport: t,
            vector,
            queues: IrqSpinLock::new(queues),
            counters: IrqSpinLock::new(Counters::default()),
        };
        // Read first: a guard taken inside the format arguments would live
        // until the end of the statement.
        let (rx_size, tx_size, rx_phys, tx_phys) = {
            let q = n.queues.lock();
            (q.rx.size(), q.tx.size(), q.rx.phys(), q.tx.phys())
        };
        crate::kprintln!(
            "virtio-net {}: {:04x}:{:04x} at {}, MAC {}, MTU {}, link {}, features {:#x}, rx/tx queues {}/{} entries at {:#x}/{:#x}, {}",
            name,
            dev.vendor,
            dev.device,
            dev.addr,
            n.mac,
            mtu,
            if n.link_up() { "up" } else { "down" },
            features,
            rx_size,
            tx_size,
            rx_phys,
            tx_phys,
            match vector {
                Some(v) => alloc::format!("MSI-X vector {:#x}", v),
                None => alloc::string::String::from("polled"),
            }
        );
        Ok(n)
    }

    fn build_queues(t: &Transport, entry: Option<u16>) -> Result<Queues, VirtioError> {
        let mut rx = t.setup_async_queue(RX_QUEUE, entry)?;
        let tx = t.setup_async_queue(TX_QUEUE, entry)?;
        let mut rx_frames = alloc::vec![0u64; rx.size() as usize];
        // Post every receive slot.
        for _ in 0..rx.size() {
            let f = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
            let head = rx.add(&[Buffer { phys: f, len: PAGE_SIZE as u32, device_writes: true }]).ok_or(VirtioError::NoQueue)?;
            rx_frames[head as usize] = f;
        }
        rx.kick();
        let mut tx_frames = Vec::with_capacity(tx.size() as usize);
        for _ in 0..tx.size() {
            tx_frames.push(alloc_frame().ok_or(VirtioError::OutOfMemory)?);
        }
        let busy = alloc::vec![false; tx.size() as usize];
        Ok(Queues { rx, tx, rx_frames, tx_frames, tx_busy: busy })
    }

    pub fn link_up(&self) -> bool {
        self.features & F_STATUS == 0 || self.transport.device_config_u16(6) & STATUS_LINK_UP != 0
    }

    pub fn has_interrupts(&self) -> bool {
        self.vector.is_some()
    }

    pub fn counters(&self) -> Counters {
        *self.counters.lock()
    }

    /// Received frames since the last call (the virtio header removed);
    /// the buffers are posted again.
    pub fn receive(&self, out: &mut Vec<Vec<u8>>) {
        let mut q = self.queues.lock();
        let mut c = self.counters.lock();
        let mut reposted = false;
        while let Some((head, len)) = q.rx.pop_used() {
            let frame = q.rx_frames[head as usize];
            let len = len as usize;
            if (HDR_LEN..=PAGE_SIZE as usize).contains(&len) {
                let n = len - HDR_LEN;
                // Filled by the copy below, so not zeroed first.
                let mut v = Vec::with_capacity(n);
                // SAFETY: the device returned the buffer; the frame is ours
                // and `HDR_LEN + n <= PAGE_SIZE`; the copy initialises all
                // `n` bytes of the vector's capacity.
                unsafe {
                    core::ptr::copy_nonoverlapping((phys_to_virt(frame) + HDR_LEN as u64) as *const u8, v.as_mut_ptr(), n);
                    v.set_len(n);
                }
                c.rx_frames += 1;
                c.rx_bytes += n as u64;
                out.push(v);
            } else {
                c.rx_errors += 1;
            }
            match q.rx.add(&[Buffer { phys: frame, len: PAGE_SIZE as u32, device_writes: true }]) {
                Some(h) => q.rx_frames[h as usize] = frame,
                None => c.rx_errors += 1,
            }
            reposted = true;
        }
        if reposted {
            q.rx.kick();
        }
    }

    /// Queues frames for transmission; returns how many were accepted
    /// (the rest are dropped when the ring is full).
    pub fn transmit(&self, frames: &[Vec<u8>]) -> usize {
        let mut q = self.queues.lock();
        let mut c = self.counters.lock();
        // Reclaim sent buffers first.
        while let Some((head, _)) = q.tx.pop_used() {
            if let Some(b) = q.tx_busy.get_mut(head as usize) {
                *b = false;
            }
        }
        let mut sent = 0;
        for f in frames {
            if f.len() > MAX_FRAME || f.is_empty() {
                continue;
            }
            if q.tx.free() == 0 {
                // The device sends quickly: wait briefly for buffers to come back
                // rather than drop the frame.
                for _ in 0..200_000 {
                    if let Some((head, _)) = q.tx.pop_used() {
                        if let Some(b) = q.tx_busy.get_mut(head as usize) {
                            *b = false;
                        }
                        break;
                    }
                    core::hint::spin_loop();
                }
                if q.tx.free() == 0 {
                    c.tx_full += 1;
                    break;
                }
            }
            // The head descriptor id the queue will use next is not known
            // in advance, so pick any idle frame and record it by token.
            let Some(slot) = q.tx_busy.iter().position(|b| !b) else {
                c.tx_full += 1;
                break;
            };
            let frame = q.tx_frames[slot];
            // SAFETY: the frame is idle (not owned by the device) and large
            // enough for the header and the frame.
            unsafe {
                let p = phys_to_virt(frame) as *mut u8;
                core::ptr::write_bytes(p, 0, HDR_LEN);
                core::ptr::copy_nonoverlapping(f.as_ptr(), p.add(HDR_LEN), f.len());
            }
            let Some(head) = q.tx.add(&[Buffer { phys: frame, len: (HDR_LEN + f.len()) as u32, device_writes: false }]) else {
                break;
            };
            // Keep frame <-> token association: swap so tx_frames[head] is the frame in flight.
            q.tx_frames.swap(slot, head as usize);
            q.tx_busy.swap(slot, head as usize);
            q.tx_busy[head as usize] = true;
            c.tx_frames += 1;
            c.tx_bytes += f.len() as u64;
            sent += 1;
        }
        if sent > 0 {
            q.tx.kick();
        }
        sent
    }

    /// Checks for a device that asks to be reset and rebuilds its queues.
    /// Returns true if a reset happened.
    pub fn recover(&self) -> bool {
        if !self.transport.needs_reset() {
            return false;
        }
        crate::kprintln!("virtio-net {}: device needs reset (status {:#x}), reinitialising", self.name, self.transport.status());
        let entry = self.vector.map(|_| 0u16);
        let r = self.transport.negotiate(self.features).and_then(|_| {
            if entry.is_some() && self.features & F_STATUS != 0 {
                self.transport.set_config_vector(0);
            }
            Self::build_queues(&self.transport, entry)
        });
        match r {
            Ok(q) => {
                // The old buffers' frames are abandoned (the device was reset,
                // so it no longer owns them); leaking them is safer than
                // reusing memory the device might still have touched.
                *self.queues.lock() = q;
                self.transport.driver_ok();
                self.counters.lock().resets += 1;
                true
            }
            Err(e) => {
                crate::kprintln!("virtio-net {}: reinitialisation failed: {:?}", self.name, e);
                false
            }
        }
    }
}
