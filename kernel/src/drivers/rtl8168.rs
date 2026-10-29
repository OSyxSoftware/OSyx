//! Realtek r8169-family Ethernet: RTL8111/8168 (PCIe gigabit, most laptops
//! and desktop boards, e.g. the Lenovo ThinkPad T495's 10ec:8168), the
//! RTL8101/8102/8105 fast Ethernet parts, the PCI RTL8169/8110 and boards
//! built on them under other vendor ids.
//!
//! Generic bring-up through the registers every member shares: soft
//! reset, MAC from IDR0..5, one receive and one transmit ring of 64
//! descriptors (2 KiB buffers, one page each), broadcast + multicast +
//! own address, MSI when the function has it (otherwise the network
//! thread polls). PHY setup, ASPM and the link watchdog are OSyx's own
//! (`rtl8168_phy`); the vendor's per-revision PHY tuning tables and PHY
//! firmware are not used. Link state comes from PHYstatus.

use super::rtl8168_phy::{self as phy, LinkEvent, Mdio, Watchdog};
use super::virtio_net::Counters;
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicU64, Ordering};
use osyx_net::wire::MacAddr;

/// (vendor, device) of the supported functions.
pub const DEVICE_IDS: &[(u16, u16)] = &[
    (0x10ec, 0x8168), // RTL8111/8168 B..H (PCIe gigabit)
    (0x10ec, 0x8161), // RTL8111/8168 (newer PCI id)
    (0x10ec, 0x8136), // RTL8101/8102/8103/8105/8106 (fast Ethernet)
    (0x10ec, 0x8169), // RTL8169/8110 (PCI gigabit)
    (0x10ec, 0x8167), // RTL8169SC/8110SC
    (0x10ec, 0x2502), // RTL8111 (Killer E2500 boards)
    (0x10ec, 0x2600), // RTL8111 (Killer E2600 boards)
    (0x1186, 0x4300), // D-Link DGE-528T (RTL8169)
    (0x1186, 0x4302), // D-Link DGE-530T rev C (RTL8169)
    (0x1259, 0xc107), // Allied Telesyn AT-2450 (RTL8169)
    (0x16ec, 0x0116), // U.S. Robotics USR997902 (RTL8169S)
    (0x1737, 0x1032), // Linksys EG1032 v3 (RTL8169)
];

pub fn supports(d: &pci::Device) -> bool {
    DEVICE_IDS.contains(&(d.vendor, d.device))
}

const IDR0: usize = 0x00;
const MAR0: usize = 0x08;
const TX_DESC_LO: usize = 0x20;
const TX_DESC_HI: usize = 0x24;
const CHIP_CMD: usize = 0x37;
const TX_POLL: usize = 0x38;
const INTR_MASK: usize = 0x3c;
const INTR_STATUS: usize = 0x3e;
const TX_CONFIG: usize = 0x40;
const RX_CONFIG: usize = 0x44;
const CFG9346: usize = 0x50;
const PHYAR: usize = 0x60;
const PHY_STATUS: usize = 0x6c;
const PMCH: usize = 0x6f;
const GPHY_OCP: usize = 0xb8;
const RX_MAX_SIZE: usize = 0xda;
const CPLUS_CMD: usize = 0xe0;
const INTR_MITIGATE: usize = 0xe2;
const RX_DESC_LO: usize = 0xe4;
const RX_DESC_HI: usize = 0xe8;
const MAX_TX_PACKET: usize = 0xec;

const CMD_RESET: u8 = 0x10;
const CMD_RX_ENABLE: u8 = 0x08;
const CMD_TX_ENABLE: u8 = 0x04;
const TX_POLL_NORMAL: u8 = 0x40;
const CFG_UNLOCK: u8 = 0xc0;
const CFG_LOCK: u8 = 0x00;
const PHY_LINK: u8 = 0x02;
/// Use 64-bit descriptor addresses (PCI parts; PCIe parts always do).
const CPLUS_PCI_DAC: u16 = 0x0010;

const ACCEPT_BROADCAST: u32 = 0x08;
const ACCEPT_MULTICAST: u32 = 0x04;
const ACCEPT_MY_PHYS: u32 = 0x02;
const RX_DMA_BURST: u32 = 7 << 8;
const RX_FIFO_NONE: u32 = 7 << 13;
const RX_128_INT: u32 = 1 << 15;
const RX_EARLY_OFF: u32 = 1 << 11;
const TX_DMA_BURST: u32 = 7 << 8;
const TX_IFG: u32 = 3 << 24;
const TX_AUTO_FIFO: u32 = 1 << 7;

/// RxOK, RxErr, TxOK, TxErr, RxOverflow, LinkChg, RxFIFOOver, SysErr.
const INTR_BITS: u16 = 0x0001 | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0020 | 0x0040 | 0x8000;

const RING: usize = 64;
const DESC: usize = 16;
const RX_BUF: usize = 2048;
const MAX_FRAME: usize = 1518;
const MIN_FRAME: usize = 60;

const OWN: u32 = 1 << 31;
const RING_END: u32 = 1 << 30;
const FIRST: u32 = 1 << 29;
const LAST: u32 = 1 << 28;
const RX_ERROR: u32 = 1 << 21;
const RX_LEN: u32 = 0x3fff;

/// Interrupts taken by all devices of this driver (diagnostics).
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
    rx_next: usize,
    tx_next: usize,
    tx_clean: usize,
}

pub struct Rtl8168 {
    pub name: alloc::string::String,
    pub location: pci::Address,
    pub mac: MacAddr,
    regs: u64,
    /// Chip version id from TxConfig (Linux's "XID").
    xid: u32,
    vector: Option<u8>,
    rings: IrqSpinLock<Rings>,
    counters: IrqSpinLock<Counters>,
    watchdog: IrqSpinLock<Watchdog>,
}

impl Mdio for Rtl8168 {
    fn mdio_read(&self, reg: u8) -> Option<u16> {
        self.phy_read(reg)
    }

    fn mdio_write(&self, reg: u8, v: u16) -> bool {
        self.phy_write(reg, v)
    }
}

impl Rtl8168 {
    fn r8(&self, reg: usize) -> u8 {
        // SAFETY: `regs` maps the device's register BAR (at least 256 bytes).
        unsafe { read_volatile((self.regs + reg as u64) as *const u8) }
    }
    fn r16(&self, reg: usize) -> u16 {
        // SAFETY: as in `r8`.
        unsafe { read_volatile((self.regs + reg as u64) as *const u16) }
    }
    fn r32(&self, reg: usize) -> u32 {
        // SAFETY: as in `r8`.
        unsafe { read_volatile((self.regs + reg as u64) as *const u32) }
    }
    fn w8(&self, reg: usize, v: u8) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.regs + reg as u64) as *mut u8, v) }
    }
    fn w16(&self, reg: usize, v: u16) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.regs + reg as u64) as *mut u16, v) }
    }
    fn w32(&self, reg: usize, v: u32) {
        // SAFETY: as in `r8`.
        unsafe { write_volatile((self.regs + reg as u64) as *mut u32, v) }
    }

    /// RTL8168G and later (and the 8168E-VL/F) reach the PHY through the
    /// OCP window and want automatic TX FIFO handling.
    fn modern(&self) -> bool {
        matches!(self.xid, 0x2c8 | 0x480 | 0x488) || self.xid >= 0x4c0
    }

    fn pci_gigabit(&self, dev: u16) -> bool {
        !matches!(dev, 0x8168 | 0x8161 | 0x8136 | 0x2502 | 0x2600)
    }

    fn wait(&self, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = crate::arch::timer::Deadline::after_ms(100);
        while !deadline.expired() {
            if done(self) {
                return true;
            }
            core::hint::spin_loop();
        }
        done(self)
    }

    fn phy_read(&self, reg: u8) -> Option<u16> {
        if self.xid >= 0x4c0 {
            self.w32(GPHY_OCP, (0xa400 + reg as u32 * 2) << 15);
            self.wait(|s| s.r32(GPHY_OCP) & OWN != 0).then(|| self.r32(GPHY_OCP) as u16)
        } else {
            self.w32(PHYAR, (reg as u32 & 0x1f) << 16);
            self.wait(|s| s.r32(PHYAR) & OWN != 0).then(|| self.r32(PHYAR) as u16)
        }
    }

    fn phy_write(&self, reg: u8, v: u16) -> bool {
        if self.xid >= 0x4c0 {
            self.w32(GPHY_OCP, OWN | (0xa400 + reg as u32 * 2) << 15 | v as u32);
            self.wait(|s| s.r32(GPHY_OCP) & OWN == 0)
        } else {
            self.w32(PHYAR, OWN | (reg as u32 & 0x1f) << 16 | v as u32);
            self.wait(|s| s.r32(PHYAR) & OWN == 0)
        }
    }

    pub fn probe(dev: &pci::Device, name: &str) -> Result<Rtl8168, &'static str> {
        // Registers: the first memory BAR (BAR2 on PCIe parts, BAR1 on PCI
        // ones; BAR0 is the same block in I/O space).
        let (base, size) = (0..6)
            .find_map(|i| match dev.bar(i) {
                Some(pci::Bar::Memory { base, size, .. }) if size >= 256 => Some((base, size)),
                _ => None,
            })
            .ok_or("no memory BAR")?;
        let regs = crate::mem::map_mmio(base, size.min(0x1000)).map_err(|_| "cannot map the registers")?;
        dev.addr.write16(0x04, dev.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER);
        let page = || alloc_frame().ok_or("out of DMA memory");
        let (rx_ring, tx_ring) = (page()?, page()?);
        let mut rx_buf = Vec::with_capacity(RING);
        let mut tx_buf = Vec::with_capacity(RING);
        for _ in 0..RING {
            rx_buf.push(page()?);
            tx_buf.push(page()?);
        }
        let mut n = Rtl8168 {
            name: name.into(),
            location: dev.addr,
            mac: MacAddr([0; 6]),
            regs,
            xid: 0,
            vector: None,
            rings: IrqSpinLock::new(Rings { rx_ring, tx_ring, rx_buf, tx_buf, rx_next: 0, tx_next: 0, tx_clean: 0 }),
            counters: IrqSpinLock::new(Counters::default()),
            watchdog: IrqSpinLock::new(Watchdog::new(false, 0)),
        };
        if n.r32(TX_CONFIG) == u32::MAX {
            return Err("registers read all ones");
        }
        n.xid = (n.r32(TX_CONFIG) >> 20) & 0xfcf;
        n.w16(INTR_MASK, 0);
        n.w16(INTR_STATUS, 0xffff);
        n.w8(CHIP_CMD, CMD_RESET);
        if !n.wait(|s| s.r8(CHIP_CMD) & CMD_RESET == 0) {
            return Err("reset did not complete");
        }
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = n.r8(IDR0 + i);
        }
        if mac == [0; 6] || mac == [0xff; 6] {
            let mut r = [0u8; 6];
            let _ = crate::random::fill(&mut r);
            mac = [0x02, r[1], r[2], r[3], r[4], r[5]];
        }
        n.mac = MacAddr(mac);
        if !n.pci_gigabit(dev.device) {
            // Keep the PLL running (Linux rtl_pll_power_up).
            n.w8(PMCH, n.r8(PMCH) | if n.modern() { 0xc0 } else { 0x80 });
        }
        if phy::disable_aspm(dev) {
            crate::kprintln!("rtl8168 {}: PCIe ASPM turned off", name);
        }
        // 0x8136 is the fast Ethernet (10/100) branch of the family.
        if let Err(e) = phy::configure(&n, dev.device != 0x8136) {
            crate::kprintln!("rtl8168 {}: PHY setup skipped: {}", name, e);
        }
        n.setup(dev.device);
        n.vector = crate::irq::allocate(irq_handler, 0).and_then(|v| {
            let (address, data) = crate::arch::apic::msi_message(v);
            match dev.enable_msi(address, data) {
                Ok(()) => Some(v),
                Err(why) => {
                    crate::kprintln!("rtl8168 {}: no MSI ({}), polling", name, why);
                    crate::irq::free(v);
                    None
                }
            }
        });
        if n.vector.is_some() {
            n.w16(INTR_MASK, INTR_BITS);
        }
        crate::kprintln!(
            "rtl8168 {}: {:04x}:{:04x} at {}, {} (chip id {:#05x}), MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}",
            name,
            dev.vendor,
            dev.device,
            dev.addr,
            phy::chip_name(n.xid),
            n.xid,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            if n.link_up() { "up" } else { "down (cable or autonegotiation pending)" }
        );
        *n.watchdog.lock() = Watchdog::new(n.link_up(), crate::interrupts::uptime_ms());
        Ok(n)
    }

    fn setup(&self, device: u16) {
        let r = self.rings.lock();
        // SAFETY: ring pages and buffers are frames owned by this device.
        unsafe {
            core::ptr::write_bytes(phys_to_virt(r.rx_ring) as *mut u8, 0, PAGE_SIZE as usize);
            core::ptr::write_bytes(phys_to_virt(r.tx_ring) as *mut u8, 0, PAGE_SIZE as usize);
        }
        for i in 0..RING {
            Self::give_rx(&r, i);
        }
        let pci = self.pci_gigabit(device);
        self.w8(CFG9346, CFG_UNLOCK);
        let mut cplus = self.r16(CPLUS_CMD);
        if pci {
            cplus |= CPLUS_PCI_DAC;
        }
        self.w16(CPLUS_CMD, cplus);
        self.w16(INTR_MITIGATE, 0);
        if !pci {
            self.w8(MAX_TX_PACKET, 0x3f);
        }
        self.w16(RX_MAX_SIZE, RX_BUF as u16);
        self.w32(TX_DESC_HI, (r.tx_ring >> 32) as u32);
        self.w32(TX_DESC_LO, r.tx_ring as u32);
        self.w32(RX_DESC_HI, (r.rx_ring >> 32) as u32);
        self.w32(RX_DESC_LO, r.rx_ring as u32);
        self.w8(CFG9346, CFG_LOCK);
        let _ = self.r8(CHIP_CMD); // flush posted writes
        self.w8(CHIP_CMD, CMD_TX_ENABLE | CMD_RX_ENABLE);
        let rx_base = if pci {
            RX_FIFO_NONE | RX_DMA_BURST
        } else if self.xid >= 0x4c0 {
            RX_128_INT | RX_DMA_BURST | RX_EARLY_OFF
        } else {
            RX_128_INT | RX_DMA_BURST
        };
        self.w32(RX_CONFIG, rx_base | ACCEPT_BROADCAST | ACCEPT_MULTICAST | ACCEPT_MY_PHYS);
        let mut tx = TX_DMA_BURST | TX_IFG;
        if self.modern() {
            tx |= TX_AUTO_FIFO;
        }
        self.w32(TX_CONFIG, tx);
        // All multicast groups (IPv4 only uses a few; filtering is not worth it).
        self.w32(MAR0, u32::MAX);
        self.w32(MAR0 + 4, u32::MAX);
    }

    fn rx_desc(r: &Rings, i: usize) -> u64 {
        phys_to_virt(r.rx_ring) + (i * DESC) as u64
    }

    fn tx_desc(r: &Rings, i: usize) -> u64 {
        phys_to_virt(r.tx_ring) + (i * DESC) as u64
    }

    /// Hands receive descriptor `i` (with its buffer) to the chip.
    fn give_rx(r: &Rings, i: usize) {
        let d = Self::rx_desc(r, i);
        let end = if i == RING - 1 { RING_END } else { 0 };
        // SAFETY: descriptor memory of this device; the address is written
        // before ownership passes to the chip.
        unsafe {
            write_volatile((d + 8) as *mut u64, r.rx_buf[i]);
            write_volatile((d + 4) as *mut u32, 0);
            fence(Ordering::Release);
            write_volatile(d as *mut u32, OWN | end | RX_BUF as u32);
        }
    }

    pub fn link_up(&self) -> bool {
        self.r8(PHY_STATUS) & PHY_LINK != 0
    }

    pub fn has_interrupts(&self) -> bool {
        self.vector.is_some()
    }

    pub fn counters(&self) -> Counters {
        *self.counters.lock()
    }

    pub fn receive(&self, out: &mut Vec<Vec<u8>>) {
        // Acknowledge whatever was signalled (write 1 to clear).
        let status = self.r16(INTR_STATUS);
        if status != 0 && status != 0xffff {
            self.w16(INTR_STATUS, status);
        }
        let mut r = self.rings.lock();
        let mut c = self.counters.lock();
        for _ in 0..RING {
            let i = r.rx_next;
            let d = Self::rx_desc(&r, i);
            // SAFETY: descriptor memory of this device.
            let opts = unsafe { read_volatile(d as *const u32) };
            if opts & OWN != 0 {
                break;
            }
            fence(Ordering::Acquire);
            let len = (opts & RX_LEN) as usize;
            if opts & FIRST != 0 && opts & LAST != 0 && opts & RX_ERROR == 0 && len > 4 && len <= RX_BUF {
                // The length includes the frame check sequence.
                let len = len - 4;
                let mut f = alloc::vec![0u8; len];
                // SAFETY: the buffer page holds `len` received bytes.
                unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(r.rx_buf[i]) as *const u8, f.as_mut_ptr(), len) };
                c.rx_frames += 1;
                c.rx_bytes += len as u64;
                out.push(f);
            } else {
                c.rx_errors += 1;
            }
            Self::give_rx(&r, i);
            r.rx_next = (i + 1) % RING;
        }
    }

    pub fn transmit(&self, frames: &[Vec<u8>]) -> usize {
        let mut r = self.rings.lock();
        let mut c = self.counters.lock();
        while r.tx_clean != r.tx_next {
            // SAFETY: descriptor memory of this device.
            let opts = unsafe { read_volatile(Self::tx_desc(&r, r.tx_clean) as *const u32) };
            if opts & OWN != 0 {
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
            // Short frames are padded here: not every member pads itself.
            let len = f.len().max(MIN_FRAME);
            let end = if i == RING - 1 { RING_END } else { 0 };
            // SAFETY: the slot is free; its buffer page and descriptor belong to us.
            unsafe {
                let buf = phys_to_virt(r.tx_buf[i]) as *mut u8;
                core::ptr::copy_nonoverlapping(f.as_ptr(), buf, f.len());
                core::ptr::write_bytes(buf.add(f.len()), 0, len - f.len());
                write_volatile((d + 8) as *mut u64, r.tx_buf[i]);
                write_volatile((d + 4) as *mut u32, 0);
                fence(Ordering::Release);
                write_volatile(d as *mut u32, OWN | end | FIRST | LAST | len as u32);
            }
            r.tx_next = (i + 1) % RING;
            c.tx_frames += 1;
            c.tx_bytes += f.len() as u64;
            sent += 1;
        }
        if sent > 0 {
            fence(Ordering::SeqCst);
            self.w8(TX_POLL, TX_POLL_NORMAL);
        }
        sent
    }

    /// Once a second: logs link changes, renegotiates a link that stays
    /// down, and re-enables receive and transmit if an error stopped them.
    pub fn recover(&self) -> bool {
        let up = self.link_up();
        let event = self.watchdog.lock().check(up, crate::interrupts::uptime_ms());
        match event {
            LinkEvent::Up => crate::kprintln!("rtl8168 {}: link up, {}", self.name, phy::describe_status(self.r8(PHY_STATUS))),
            LinkEvent::Down => crate::kprintln!("rtl8168 {}: link down", self.name),
            LinkEvent::Renegotiate => {
                let n = self.watchdog.lock().renegotiations;
                if n <= 3 {
                    crate::kprintln!("rtl8168 {}: no link for {} s; restarting autonegotiation", self.name, phy::RENEGOTIATE_MS / 1000);
                }
                phy::restart_autoneg(self);
            }
            LinkEvent::None => {}
        }
        let cmd = self.r8(CHIP_CMD);
        if cmd != 0xff && cmd & (CMD_RX_ENABLE | CMD_TX_ENABLE) != CMD_RX_ENABLE | CMD_TX_ENABLE {
            self.counters.lock().resets += 1;
            self.w8(CHIP_CMD, CMD_TX_ENABLE | CMD_RX_ENABLE);
            return true;
        }
        false
    }
}
