//! AHCI (SATA) driver: PCI class 0x01, subclass 0x06, interface 0x01.
//!
//! Every implemented port with an ATA disk gets its own command list, FIS
//! area, command table and DMA bounce pages. IDENTIFY DEVICE reads the
//! capacity; READ/WRITE DMA EXT (LBA48) and FLUSH CACHE serve the block
//! layer. Requests are serialised by a sleeping mutex and polled to
//! completion: no interrupts are used.

use crate::arch::timer::{self, Deadline};
use crate::block::{BlockDev, BlockError, SECTOR_SIZE};
use crate::mem::{alloc_frame, free_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::KMutex;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicUsize, Ordering};

const PAGE: usize = PAGE_SIZE as usize;
/// Bounce buffer size: 32 KiB per request.
const MAX_DATA_PAGES: usize = 8;
const TIMEOUT_MS: u64 = 5000;

// Generic host control registers.
const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;
const GHC_AE: u32 = 1 << 31;

const PORT_BASE: u64 = 0x100;
const PORT_STRIDE: u64 = 0x80;

// Port registers.
const PX_CLB: u64 = 0x00;
const PX_CLBU: u64 = 0x04;
const PX_FB: u64 = 0x08;
const PX_FBU: u64 = 0x0C;
const PX_IS: u64 = 0x10;
const PX_CMD: u64 = 0x18;
const PX_TFD: u64 = 0x20;
const PX_SIG: u64 = 0x24;
const PX_SSTS: u64 = 0x28;
const PX_SCTL: u64 = 0x2C;
const PX_SERR: u64 = 0x30;
const PX_CI: u64 = 0x38;

const CMD_ST: u32 = 1;
const CMD_FRE: u32 = 1 << 4;
const CMD_CR: u32 = 1 << 15;

const TFD_ERR: u32 = 1;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BSY: u32 = 1 << 7;

const IS_TFES: u32 = 1 << 30;
const IS_HBDS: u32 = 1 << 29;

const SIG_ATA: u32 = 0x0000_0101;

const CMD_IDENTIFY: u8 = 0xEC;
const CMD_FLUSH_CACHE: u8 = 0xE7;
const CMD_READ_DMA: u8 = 0xC8;
const CMD_WRITE_DMA: u8 = 0xCA;
const CMD_READ_DMA_EXT: u8 = 0x25;
const CMD_WRITE_DMA_EXT: u8 = 0x35;

static DISKS: AtomicUsize = AtomicUsize::new(0);

/// Volatile access to the ABAR.
#[derive(Clone, Copy)]
struct Mmio(u64);

impl Mmio {
    fn r32(self, off: u64) -> u32 {
        // SAFETY: the ABAR is mapped uncached; offsets stay within it.
        unsafe { read_volatile((self.0 + off) as *const u32) }
    }

    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r32`.
        unsafe { write_volatile((self.0 + off) as *mut u32, v) }
    }
}

struct Inner {
    cmd_list: u64,
    _fis: u64,
    table: u64,
    data: [u64; MAX_DATA_PAGES],
    failed: bool,
}

pub struct AhciDisk {
    mmio: Mmio,
    port: u32,
    name: String,
    sectors: u64,
    lba48: bool,
    inner: KMutex<Inner>,
}

/// Probes one AHCI controller and registers every attached ATA disk.
pub fn probe(dev: &pci::Device) -> Vec<Arc<dyn BlockDev>> {
    match start(dev) {
        Ok(disks) => disks,
        Err(e) => {
            crate::kprintln!("ahci: {:04x}:{:04x} at {}: not usable: {}", dev.vendor, dev.device, dev.addr, e);
            Vec::new()
        }
    }
}

/// Allocates a frame the controller can address.
fn dma_frame(wide: bool) -> Result<u64, &'static str> {
    for _ in 0..64 {
        let frame = alloc_frame().ok_or("out of DMA memory")?;
        if wide || frame + PAGE as u64 <= 1 << 32 {
            return Ok(frame);
        }
        let _ = free_frame(frame);
    }
    Err("no frame below 4 GiB for a 32-bit AHCI controller")
}

fn wait_idle(m: Mmio, port: u64, timeout_ms: u64) -> bool {
    let deadline = Deadline::after_ms(timeout_ms);
    loop {
        if m.r32(port + PX_TFD) & (TFD_BSY | TFD_DRQ) == 0 {
            return true;
        }
        if deadline.expired() {
            return false;
        }
        timer::pause_tick();
    }
}

/// Brings the link up if it is not established yet; returns the device
/// signature when the port has a device.
fn link_sig(m: Mmio, port: u64) -> Option<u32> {
    let det = m.r32(port + PX_SSTS) & 0xF;
    if det == 3 {
        return Some(m.r32(port + PX_SIG));
    }
    if det == 0 {
        return None;
    }
    // COMRESET, then wait for the link (SATA spec: at least 1 ms low).
    m.w32(port + PX_SCTL, 1);
    timer::sleep_ms(1);
    m.w32(port + PX_SCTL, 0);
    let deadline = Deadline::after_ms(1000);
    loop {
        if m.r32(port + PX_SSTS) & 0xF == 3 {
            return Some(m.r32(port + PX_SIG));
        }
        if deadline.expired() {
            return None;
        }
        timer::sleep_ms(10);
    }
}

fn start(dev: &pci::Device) -> Result<Vec<Arc<dyn BlockDev>>, &'static str> {
    let Some(pci::Bar::Memory { base, size, .. }) = dev.bar(5) else { return Err("BAR5 is not a memory BAR") };
    if base == 0 || size < 0x1000 {
        return Err("ABAR unassigned or too small");
    }
    let virt = crate::mem::map_mmio(base, size).map_err(|_| "cannot map the ABAR")?;
    dev.addr.write16(0x04, dev.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER | pci::CMD_INTX_DISABLE);
    let m = Mmio(virt);
    let cap = m.r32(HBA_CAP);
    if cap == u32::MAX {
        return Err("registers read all ones (device not present)");
    }
    let wide = cap & (1 << 31) != 0; // 64-bit DMA addresses
    let implemented = m.r32(HBA_PI);
    let max_port = cap & 0x1F;
    m.w32(HBA_GHC, m.r32(HBA_GHC) | GHC_AE);
    crate::kprintln!(
        "ahci: {:04x}:{:04x} at {}, {} ports implemented, {} DMA, BAR5 {:#x} ({} KiB)",
        dev.vendor,
        dev.device,
        dev.addr,
        implemented.count_ones(),
        if wide { "64-bit" } else { "32-bit" },
        base,
        size >> 10
    );
    let mut disks: Vec<Arc<dyn BlockDev>> = Vec::new();
    for port in 0..=max_port.min(31) {
        if implemented & (1 << port) == 0 {
            continue;
        }
        let p = PORT_BASE + port as u64 * PORT_STRIDE;
        let Some(sig) = link_sig(m, p) else { continue };
        if sig != SIG_ATA {
            crate::kprintln!("ahci: port {}: device signature {:#x}, not an ATA disk; skipped", port, sig);
            continue;
        }
        let index = DISKS.fetch_add(1, Ordering::Relaxed);
        let name = if index < 26 { format!("sd{}", (b'a' + index as u8) as char) } else { format!("sd{index}") };
        match start_port(m, port, wide, name) {
            Ok(disk) => {
                crate::kprintln!("ahci: {}: {} MiB on port {} ({})", disk.name, disk.sectors >> 11, port, if disk.lba48 { "LBA48" } else { "LBA28" });
                disks.push(Arc::new(disk) as Arc<dyn BlockDev>);
            }
            Err(e) => crate::kprintln!("ahci: port {}: disk not usable: {}", port, e),
        }
    }
    Ok(disks)
}

fn start_port(m: Mmio, port: u32, wide: bool, name: String) -> Result<AhciDisk, &'static str> {
    let cmd_list = dma_frame(wide)?;
    let fis = dma_frame(wide)?;
    let table = dma_frame(wide)?;
    let mut data = [0u64; MAX_DATA_PAGES];
    for frame in &mut data {
        *frame = dma_frame(wide)?;
    }
    let p = PORT_BASE + port as u64 * PORT_STRIDE;
    // A port that is already running must be stopped before its command
    // list and FIS area can be replaced.
    let cmd = m.r32(p + PX_CMD);
    if cmd & CMD_ST != 0 {
        m.w32(p + PX_CMD, cmd & !(CMD_ST | CMD_FRE));
        let deadline = Deadline::after_ms(500);
        while m.r32(p + PX_CMD) & CMD_CR != 0 && !deadline.expired() {
            timer::pause_tick();
        }
    }
    m.w32(p + PX_CLB, cmd_list as u32);
    m.w32(p + PX_CLBU, (cmd_list >> 32) as u32);
    m.w32(p + PX_FB, fis as u32);
    m.w32(p + PX_FBU, (fis >> 32) as u32);
    m.w32(p + PX_IS, 0xFFFF_FFFF);
    m.w32(p + PX_SERR, 0xFFFF_FFFF);
    m.w32(p + PX_CMD, CMD_ST | CMD_FRE);
    let deadline = Deadline::after_ms(500);
    while m.r32(p + PX_CMD) & CMD_CR == 0 {
        if deadline.expired() {
            crate::kprintln!("ahci: {}: command list did not start; continuing", name);
            break;
        }
        timer::pause_tick();
    }

    let mut disk = AhciDisk {
        mmio: m,
        port,
        name,
        sectors: 0,
        lba48: true,
        inner: KMutex::new(Inner { cmd_list, _fis: fis, table, data, failed: false }),
    };
    {
        let mut g = disk.inner.lock();
        disk.run(&mut g, CMD_IDENTIFY, 0, 0, 512, false).map_err(|_| "IDENTIFY DEVICE failed")?;
    }

    let page = data[0];
    // SAFETY: the IDENTIFY result is 256 words in the first DMA page.
    let word = |i: usize| unsafe { read_volatile((phys_to_virt(page) as *const u16).add(i)) };
    let lba48 = word(83) & (1 << 10) != 0;
    let mut sectors = if lba48 {
        word(100) as u64 | (word(101) as u64) << 16 | (word(102) as u64) << 32 | (word(103) as u64) << 48
    } else {
        0
    };
    if sectors == 0 {
        sectors = word(60) as u64 | (word(61) as u64) << 16;
    }
    let logical = if word(106) & (1 << 12) != 0 { word(117) as u64 | (word(118) as u64) << 16 } else { 512 };
    if logical != 512 {
        return Err("logical sector size is not 512 bytes");
    }
    if sectors == 0 {
        return Err("IDENTIFY reports no sectors");
    }
    disk.sectors = sectors;
    disk.lba48 = lba48;
    Ok(disk)
}

impl AhciDisk {
    fn port_base(&self) -> u64 {
        PORT_BASE + self.port as u64 * PORT_STRIDE
    }

    fn transfer(&self, write: bool, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.is_empty() || buf.len() > MAX_DATA_PAGES * PAGE || !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(BlockError::InvalidRequest);
        }
        let mut g = self.inner.lock();
        if g.failed {
            return Err(BlockError::Io);
        }
        if write {
            for (i, chunk) in buf.chunks(PAGE).enumerate() {
                // SAFETY: each entry is a whole DMA frame owned by this disk.
                unsafe {
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(g.data[i]) as *mut u8, chunk.len());
                }
            }
        }
        let sectors = (buf.len() / SECTOR_SIZE) as u16;
        let command = match (write, self.lba48) {
            (false, true) => CMD_READ_DMA_EXT,
            (true, true) => CMD_WRITE_DMA_EXT,
            (false, false) => CMD_READ_DMA,
            (true, false) => CMD_WRITE_DMA,
        };
        self.run(&mut g, command, lba, sectors, buf.len(), write)?;
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

    fn run(&self, g: &mut Inner, command: u8, lba: u64, sectors: u16, len: usize, write: bool) -> Result<(), BlockError> {
        let m = self.mmio;
        let p = self.port_base();
        if g.failed {
            return Err(BlockError::Io);
        }
        if !wait_idle(m, p, TIMEOUT_MS) {
            g.failed = true;
            crate::kprintln!("ahci {}: port did not become idle; disk disabled", self.name);
            return Err(BlockError::Io);
        }
        m.w32(p + PX_IS, 0xFFFF_FFFF);
        m.w32(p + PX_SERR, 0xFFFF_FFFF);

        // Host-to-device register FIS (20 bytes).
        let mut f = [0u8; 20];
        f[0] = 0x27;
        f[1] = 0x80;
        f[2] = command;
        match command {
            CMD_READ_DMA_EXT | CMD_WRITE_DMA_EXT => {
                f[4] = lba as u8;
                f[5] = (lba >> 8) as u8;
                f[6] = (lba >> 16) as u8;
                f[7] = 0x40;
                f[8] = (lba >> 24) as u8;
                f[9] = (lba >> 32) as u8;
                f[10] = (lba >> 40) as u8;
                f[12] = sectors as u8;
                f[13] = (sectors >> 8) as u8;
            }
            CMD_READ_DMA | CMD_WRITE_DMA => {
                f[4] = lba as u8;
                f[5] = (lba >> 8) as u8;
                f[6] = (lba >> 16) as u8;
                f[7] = 0xE0 | ((lba >> 24) as u8 & 0x0F);
                f[12] = sectors as u8;
            }
            _ => {}
        }
        let table = phys_to_virt(g.table);
        // SAFETY: the command table is a page owned by this disk.
        unsafe {
            core::ptr::write_bytes(table as *mut u8, 0, 0x80);
            core::ptr::copy_nonoverlapping(f.as_ptr(), table as *mut u8, f.len());
        }
        let entries = len.div_ceil(PAGE);
        for i in 0..entries {
            let n = (len - i * PAGE).min(PAGE);
            let prd = table + 0x80 + (i * 16) as u64;
            // SAFETY: PRDT entries live in the same command table page.
            unsafe {
                write_volatile(prd as *mut u64, g.data[i]);
                write_volatile((prd + 8) as *mut u32, 0);
                write_volatile((prd + 12) as *mut u32, (n as u32 - 1) | (1 << 31));
            }
        }
        let header = phys_to_virt(g.cmd_list);
        // SAFETY: command slot 0 of the command list owned by this disk.
        unsafe {
            let mut flags = 5u32 | ((entries as u32) << 16); // 20-byte FIS
            if write {
                flags |= 1 << 6;
            } else if len > 0 {
                flags |= 1 << 7; // prefetch reads
            }
            write_volatile(header as *mut u32, flags);
            write_volatile((header + 4) as *mut u32, 0);
            write_volatile((header + 8) as *mut u64, g.table);
        }
        fence(Ordering::Release);
        m.w32(p + PX_CI, 1);

        let deadline = Deadline::after_ms(TIMEOUT_MS);
        loop {
            let ci = m.r32(p + PX_CI);
            let tfd = m.r32(p + PX_TFD);
            if ci & 1 == 0 && tfd & TFD_BSY == 0 {
                break;
            }
            if deadline.expired() {
                g.failed = true;
                crate::kprintln!("ahci {}: command {:#04x} timed out (CI {:#x}, TFD {:#x}); disk disabled", self.name, command, ci, tfd);
                return Err(BlockError::Io);
            }
            timer::pause_tick();
        }
        let is = m.r32(p + PX_IS);
        let tfd = m.r32(p + PX_TFD);
        m.w32(p + PX_IS, is);
        if tfd & TFD_ERR != 0 || is & (IS_TFES | IS_HBDS) != 0 {
            crate::kprintln!("ahci {}: command {:#04x} failed (TFD {:#x}, IS {:#x})", self.name, command, tfd, is);
            return Err(BlockError::Io);
        }
        Ok(())
    }
}

impl BlockDev for AhciDisk {
    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> String {
        format!("SATA disk on AHCI port {} ({}{} MiB)", self.port, self.sectors >> 11, if self.lba48 { ", LBA48" } else { "" })
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
        self.transfer(false, lba, buf)
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let mut copy = alloc::vec![0u8; buf.len()];
        copy.copy_from_slice(buf);
        self.transfer(true, lba, &mut copy)
    }

    fn flush(&self) -> Result<(), BlockError> {
        let mut g = self.inner.lock();
        if g.failed {
            return Err(BlockError::Io);
        }
        self.run(&mut g, CMD_FLUSH_CACHE, 0, 0, 0, false)
    }
}
