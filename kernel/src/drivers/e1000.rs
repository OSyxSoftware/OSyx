//! Intel 8254x/8257x gigabit Ethernet (e1000 family): 82540EM (QEMU
//! `e1000`), 82545EM, 82574L (QEMU `e1000e`) and the common desktop
//! 82571/82572 parts, all through the legacy descriptor format that every
//! member supports.
//!
//! One receive ring of 64 descriptors with a 2 KiB buffer each (one page
//! per buffer) and one transmit ring of 64 descriptors with a page per
//! frame. Interrupts come through MSI when the function has it (82574L)
//! and only wake the network thread; without MSI the thread polls. Link
//! state is read from STATUS.LU.

use super::virtio_net::Counters;
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_net::wire::MacAddr;

/// Device ids handled (vendor 0x8086).
pub const DEVICE_IDS: &[u16] = &[
    0x100E, // 82540EM (QEMU e1000)
    0x100F, // 82545EM copper
    0x1004, // 82543GC
    0x10D3, // 82574L (QEMU e1000e)
    0x105E, 0x105F, 0x107D, 0x107E, // 82571EB / 82572EI
    0x10B9, // 82572EI
];

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const ICR: usize = 0x00C0;
const IMS: usize = 0x00D0;
const IMC: usize = 0x00D8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL0: usize = 0x5400;
const RAH0: usize = 0x5404;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_RST: u32 = 1 << 26;
const STATUS_LU: u32 = 1 << 1;
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
/// Receive timer, link status change, receive overrun, descriptor minimum.
const IMS_BITS: u32 = (1 << 7) | (1 << 2) | (1 << 6) | (1 << 4);

const RING: usize = 64;
const DESC: usize = 16;
const RX_BUF: usize = 2048;
const MAX_FRAME: usize = 1522;

const RX_DD: u8 = 1;
const RX_EOP: u8 = 2;
const TX_CMD_EOP: u8 = 1;
const TX_CMD_IFCS: u8 = 2;
const TX_CMD_RS: u8 = 8;
const TX_DD: u8 = 1;

/// Interrupts taken by all e1000 devices (diagnostics).
pub static INTERRUPTS: AtomicU64 = AtomicU64::new(0);

fn irq_handler(_: u64) {
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    crate::net::wake();
}

struct Rings {
    rx_ring: u64,
    tx_ring: u64,
    rx_buf: Vec<u64>,
    tx_buf: Vec<u64>,
    /// Next receive descriptor to look at.
    rx_next: usize,
    /// Next free transmit slot and the oldest one still in flight.
    tx_next: usize,
    tx_clean: usize,
}

pub struct E1000 {
    pub name: alloc::string::String,
    pub location: pci::Address,
    pub mac: MacAddr,
    regs: u64,
    vector: Option<u8>,
    rings: IrqSpinLock<Rings>,
    counters: IrqSpinLock<Counters>,
}

impl E1000 {
    fn read(&self, reg: usize) -> u32 {
        // SAFETY: `regs` maps the device's register BAR.
        unsafe { read_volatile((self.regs + reg as u64) as *const u32) }
    }

    fn write(&self, reg: usize, v: u32) {
        // SAFETY: as in `read`.
        unsafe { write_volatile((self.regs + reg as u64) as *mut u32, v) }
    }

    pub fn probe(dev: &pci::Device, name: &str) -> Result<E1000, &'static str> {
        let Some(pci::Bar::Memory { base, size, .. }) = dev.bar(0) else { return Err("BAR0 is not a memory BAR") };
        let regs = crate::mem::map_mmio(base, size.max(0x20000)).map_err(|_| "cannot map the registers")?;
        dev.addr.write16(0x04, dev.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER);
        let page = || alloc_frame().ok_or("out of DMA memory");
        let rx_ring = page()?;
        let tx_ring = page()?;
        let mut rx_buf = Vec::with_capacity(RING);
        let mut tx_buf = Vec::with_capacity(RING);
        for _ in 0..RING {
            rx_buf.push(page()?);
            tx_buf.push(page()?);
        }
        let mut n = E1000 {
            name: name.into(),
            location: dev.addr,
            mac: MacAddr([0; 6]),
            regs,
            vector: None,
            rings: IrqSpinLock::new(Rings { rx_ring, tx_ring, rx_buf, tx_buf, rx_next: 0, tx_next: 0, tx_clean: 0 }),
            counters: IrqSpinLock::new(Counters::default()),
        };
        // The MAC address survives the reset in the receive address
        // registers, which the hardware loads from its EEPROM.
        let (lo, hi) = (n.read(RAL0), n.read(RAH0));
        n.write(IMC, !0);
        n.write(CTRL, n.read(CTRL) | CTRL_RST);
        for _ in 0..1_000_000 {
            if n.read(CTRL) & CTRL_RST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        n.write(IMC, !0);
        let _ = n.read(ICR);
        let (lo2, hi2) = (n.read(RAL0), n.read(RAH0));
        let (lo, hi) = if hi2 & (1 << 31) != 0 { (lo2, hi2) } else { (lo, hi) };
        let mut mac = [lo as u8, (lo >> 8) as u8, (lo >> 16) as u8, (lo >> 24) as u8, hi as u8, (hi >> 8) as u8];
        if mac == [0; 6] {
            let mut r = [0u8; 6];
            let _ = crate::random::fill(&mut r);
            mac = [0x02, r[1], r[2], r[3], r[4], r[5]];
        }
        n.mac = MacAddr(mac);
        n.write(RAL0, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        n.write(RAH0, u32::from_le_bytes([mac[4], mac[5], 0, 0]) | (1 << 31));
        for i in 0..128 {
            n.write(MTA + i * 4, 0);
        }
        n.write(CTRL, (n.read(CTRL) | CTRL_SLU | CTRL_ASDE) & !(1 << 3) & !(1 << 31));
        n.setup_rings();
        n.vector = crate::irq::allocate(irq_handler, 0).and_then(|v| {
            let (address, data) = crate::arch::apic::msi_message(v);
            match dev.enable_msi(address, data) {
                Ok(()) => Some(v),
                Err(why) => {
                    crate::kprintln!("e1000 {}: no MSI ({}), polling", name, why);
                    crate::irq::free(v);
                    None
                }
            }
        });
        if n.vector.is_some() {
            n.write(IMS, IMS_BITS);
        }
        crate::kprintln!(
            "e1000 {}: {:04x} at {}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}",
            name,
            dev.device,
            dev.addr,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            if n.link_up() { "up" } else { "down" }
        );
        Ok(n)
    }

    fn setup_rings(&self) {
        let r = self.rings.lock();
        // SAFETY: both ring pages and all buffers are frames owned by this
        // device, reachable through the HHDM.
        unsafe {
            core::ptr::write_bytes(phys_to_virt(r.rx_ring) as *mut u8, 0, PAGE_SIZE as usize);
            core::ptr::write_bytes(phys_to_virt(r.tx_ring) as *mut u8, 0, PAGE_SIZE as usize);
            for (i, &b) in r.rx_buf.iter().enumerate() {
                write_volatile((phys_to_virt(r.rx_ring) + (i * DESC) as u64) as *mut u64, b);
            }
        }
        self.write(RDBAL, r.rx_ring as u32);
        self.write(RDBAH, (r.rx_ring >> 32) as u32);
        self.write(RDLEN, (RING * DESC) as u32);
        self.write(RDH, 0);
        self.write(RDT, (RING - 1) as u32);
        self.write(TDBAL, r.tx_ring as u32);
        self.write(TDBAH, (r.tx_ring >> 32) as u32);
        self.write(TDLEN, (RING * DESC) as u32);
        self.write(TDH, 0);
        self.write(TDT, 0);
        // 2 KiB buffers (BSIZE 0), broadcasts, CRC stripped.
        self.write(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
        self.write(TIPG, 10 | (8 << 10) | (6 << 20));
        self.write(TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12));
    }

    fn rx_desc(r: &Rings, i: usize) -> u64 {
        phys_to_virt(r.rx_ring) + (i * DESC) as u64
    }

    fn tx_desc(r: &Rings, i: usize) -> u64 {
        phys_to_virt(r.tx_ring) + (i * DESC) as u64
    }

    pub fn link_up(&self) -> bool {
        self.read(STATUS) & STATUS_LU != 0
    }

    pub fn has_interrupts(&self) -> bool {
        self.vector.is_some()
    }

    pub fn counters(&self) -> Counters {
        *self.counters.lock()
    }

    pub fn receive(&self, out: &mut Vec<Vec<u8>>) {
        if self.vector.is_some() {
            // Reading ICR acknowledges (and re-arms) the interrupt.
            let _ = self.read(ICR);
        }
        let mut r = self.rings.lock();
        let mut c = self.counters.lock();
        loop {
            let d = Self::rx_desc(&r, r.rx_next);
            // SAFETY: descriptor memory of this device.
            let status = unsafe { read_volatile((d + 12) as *const u8) };
            if status & RX_DD == 0 {
                break;
            }
            // SAFETY: as above.
            let (len, errors) = unsafe { (read_volatile((d + 8) as *const u16) as usize, read_volatile((d + 13) as *const u8)) };
            if status & RX_EOP != 0 && errors == 0 && len > 0 && len <= RX_BUF {
                let mut f = alloc::vec![0u8; len];
                // SAFETY: the buffer page holds `len` received bytes.
                unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(r.rx_buf[r.rx_next]) as *const u8, f.as_mut_ptr(), len) };
                c.rx_frames += 1;
                c.rx_bytes += len as u64;
                out.push(f);
            } else {
                c.rx_errors += 1;
            }
            // Give the descriptor back.
            // SAFETY: as above.
            unsafe {
                write_volatile((d + 8) as *mut u64, 0);
                write_volatile(d as *mut u64, r.rx_buf[r.rx_next]);
            }
            let done = r.rx_next;
            r.rx_next = (r.rx_next + 1) % RING;
            self.write(RDT, done as u32);
        }
    }

    pub fn transmit(&self, frames: &[Vec<u8>]) -> usize {
        let mut r = self.rings.lock();
        let mut c = self.counters.lock();
        // Reclaim finished slots.
        while r.tx_clean != r.tx_next {
            // SAFETY: descriptor memory of this device.
            let status = unsafe { read_volatile((Self::tx_desc(&r, r.tx_clean) + 12) as *const u8) };
            if status & TX_DD == 0 {
                break;
            }
            r.tx_clean = (r.tx_clean + 1) % RING;
        }
        let mut sent = 0;
        for f in frames {
            if f.is_empty() || f.len() > MAX_FRAME {
                continue;
            }
            if (r.tx_next + 1) % RING == r.tx_clean {
                c.tx_full += 1;
                break;
            }
            let i = r.tx_next;
            let d = Self::tx_desc(&r, i);
            // SAFETY: the slot is free; its buffer page and descriptor belong to us.
            unsafe {
                core::ptr::copy_nonoverlapping(f.as_ptr(), phys_to_virt(r.tx_buf[i]) as *mut u8, f.len());
                write_volatile(d as *mut u64, r.tx_buf[i]);
                write_volatile((d + 8) as *mut u16, f.len() as u16);
                write_volatile((d + 10) as *mut u8, 0);
                write_volatile((d + 11) as *mut u8, TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS);
                write_volatile((d + 12) as *mut u32, 0);
            }
            r.tx_next = (i + 1) % RING;
            c.tx_frames += 1;
            c.tx_bytes += f.len() as u64;
            sent += 1;
        }
        if sent > 0 {
            self.write(TDT, r.tx_next as u32);
        }
        sent
    }

    /// Re-enables the receiver if an overrun stopped it.
    pub fn recover(&self) -> bool {
        if self.read(RCTL) & RCTL_EN == 0 {
            self.counters.lock().resets += 1;
            self.write(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
            return true;
        }
        false
    }
}
