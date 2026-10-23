//! PCI configuration space access (legacy mechanism #1: ports 0xCF8/0xCFC,
//! the first 256 bytes of each function) and bus enumeration.

use crate::arch::cpu;
use crate::sync::IrqSpinLock;
use crate::kprintln;
use alloc::vec::Vec;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

pub const CMD_IO: u16 = 1 << 0;
pub const CMD_MEMORY: u16 = 1 << 1;
pub const CMD_BUS_MASTER: u16 = 1 << 2;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;

pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;

/// Serialises the address/data port pair.
static CONFIG: IrqSpinLock<()> = IrqSpinLock::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

impl Address {
    fn cf8(&self, offset: u8) -> u32 {
        0x8000_0000
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset as u32 & 0xFC)
    }

    pub fn read32(&self, offset: u8) -> u32 {
        let _g = CONFIG.lock();
        // SAFETY: the configuration ports are owned by this module; the lock
        // keeps the address/data pair atomic.
        unsafe {
            cpu::outl(CONFIG_ADDRESS, self.cf8(offset));
            cpu::inl(CONFIG_DATA)
        }
    }

    pub fn write32(&self, offset: u8, value: u32) {
        let _g = CONFIG.lock();
        // SAFETY: as in `read32`.
        unsafe {
            cpu::outl(CONFIG_ADDRESS, self.cf8(offset));
            cpu::outl(CONFIG_DATA, value);
        }
    }

    pub fn read16(&self, offset: u8) -> u16 {
        (self.read32(offset) >> ((offset & 2) * 8)) as u16
    }

    pub fn read8(&self, offset: u8) -> u8 {
        (self.read32(offset) >> ((offset & 3) * 8)) as u8
    }

    pub fn write16(&self, offset: u8, value: u16) {
        let shift = (offset & 2) * 8;
        let old = self.read32(offset);
        let new = (old & !(0xFFFF << shift)) | (value as u32) << shift;
        self.write32(offset, new);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub addr: Address,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Memory { base: u64, size: u64, prefetchable: bool },
    Io { port: u16, size: u32 },
}

impl Device {
    pub fn command(&self) -> u16 {
        self.addr.read16(0x04)
    }

    /// Decodes BAR `index` (0..6), measuring its size. Memory decoding is
    /// switched off while the BAR is probed.
    pub fn bar(&self, index: u8) -> Option<Bar> {
        if index >= 6 || self.header_type & 0x7F != 0 {
            return None;
        }
        let off = 0x10 + index * 4;
        let low = self.addr.read32(off);
        let cmd = self.command();
        self.addr.write16(0x04, cmd & !(CMD_IO | CMD_MEMORY));
        let bar = if low & 1 == 1 {
            self.addr.write32(off, 0xFFFF_FFFF);
            let mask = self.addr.read32(off) & 0xFFFF_FFFC;
            self.addr.write32(off, low);
            let size = (!mask).wrapping_add(1) & 0xFFFF;
            (mask != 0).then_some(Bar::Io { port: (low & 0xFFFC) as u16, size })
        } else {
            let is64 = (low >> 1) & 3 == 2;
            let high = if is64 && index < 5 { self.addr.read32(off + 4) } else { 0 };
            self.addr.write32(off, 0xFFFF_FFFF);
            let mask_low = self.addr.read32(off) & 0xFFFF_FFF0;
            self.addr.write32(off, low);
            let mut mask = mask_low as u64 | 0xFFFF_FFFF_0000_0000;
            if is64 && index < 5 {
                self.addr.write32(off + 4, 0xFFFF_FFFF);
                mask = (self.addr.read32(off + 4) as u64) << 32 | mask_low as u64;
                self.addr.write32(off + 4, high);
            }
            let base = (high as u64) << 32 | (low & 0xFFFF_FFF0) as u64;
            let size = (!mask).wrapping_add(1);
            (mask_low != 0).then_some(Bar::Memory { base, size, prefetchable: low & 8 != 0 })
        };
        self.addr.write16(0x04, cmd);
        bar
    }

    /// Offset of the first capability with `id`.
    pub fn find_capability(&self, id: u8) -> Option<u8> {
        self.capabilities().into_iter().find(|&(_, c)| c == id).map(|(off, _)| off)
    }

    /// Routes vector-table entry 0 of MSI-X to `address`/`data` and enables
    /// MSI-X (legacy INTx is disabled). The table is located through the
    /// capability's BAR indicator and offset.
    pub fn enable_msix(&self, address: u64, data: u32) -> Result<(), &'static str> {
        let cap = self.find_capability(CAP_MSIX).ok_or("no MSI-X capability")?;
        let control = self.addr.read16(cap + 2);
        let table = self.addr.read32(cap + 4);
        let bir = (table & 7) as u8;
        let offset = (table & !7) as u64;
        let entries = (control & 0x7FF) as u64 + 1;
        let Some(Bar::Memory { base, size, .. }) = self.bar(bir) else { return Err("MSI-X table BAR is not memory") };
        if base == 0 || offset + entries * 16 > size {
            return Err("MSI-X table outside its BAR");
        }
        let virt = crate::mem::map_mmio(base + offset, entries * 16).map_err(|_| "cannot map the MSI-X table")?;
        // Function mask on while the entry is written, then enable.
        self.addr.write16(cap + 2, control | MSIX_FUNCTION_MASK | MSIX_ENABLE);
        // SAFETY: `virt` maps the MSI-X table (entries * 16 bytes, uncached).
        unsafe {
            let e = virt as *mut u32;
            e.add(3).write_volatile(1); // entry masked
            e.write_volatile(address as u32);
            e.add(1).write_volatile((address >> 32) as u32);
            e.add(2).write_volatile(data);
            e.add(3).write_volatile(0); // unmasked
        }
        self.addr.write16(cap + 2, (control | MSIX_ENABLE) & !MSIX_FUNCTION_MASK);
        self.addr.write16(0x04, self.command() | CMD_INTX_DISABLE);
        Ok(())
    }

    /// Enables a single-vector MSI to `address`/`data` (legacy INTx is
    /// disabled).
    pub fn enable_msi(&self, address: u64, data: u32) -> Result<(), &'static str> {
        let cap = self.find_capability(CAP_MSI).ok_or("no MSI capability")?;
        let control = self.addr.read16(cap + 2);
        let is64 = control & (1 << 7) != 0;
        if !is64 && address >> 32 != 0 {
            return Err("MSI address above 4 GiB on a 32-bit MSI function");
        }
        self.addr.write32(cap + 4, address as u32);
        if is64 {
            self.addr.write32(cap + 8, (address >> 32) as u32);
            self.addr.write16(cap + 12, data as u16);
        } else {
            self.addr.write16(cap + 8, data as u16);
        }
        // One message (MME = 0), enabled.
        self.addr.write16(cap + 2, (control & !(7 << 4)) | 1);
        self.addr.write16(0x04, self.command() | CMD_INTX_DISABLE);
        Ok(())
    }

    /// Capability list: (offset, id) pairs, bounded against loops.
    pub fn capabilities(&self) -> Vec<(u8, u8)> {
        let mut caps = Vec::new();
        if self.addr.read16(0x06) & (1 << 4) == 0 {
            return caps;
        }
        let mut ptr = self.addr.read8(0x34) & 0xFC;
        while ptr >= 0x40 && caps.len() < 48 {
            let id = self.addr.read8(ptr);
            caps.push((ptr, id));
            ptr = self.addr.read8(ptr + 1) & 0xFC;
        }
        caps
    }
}

fn probe(addr: Address) -> Option<Device> {
    let id = addr.read32(0x00);
    let vendor = id as u16;
    if vendor == 0xFFFF {
        return None;
    }
    let class = addr.read32(0x08);
    Some(Device {
        addr,
        vendor,
        device: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
        header_type: addr.read8(0x0E),
    })
}

fn scan_bus(bus: u8, out: &mut Vec<Device>, depth: u32) {
    if depth > 8 {
        return;
    }
    for device in 0..32 {
        let Some(f0) = probe(Address { bus, device, function: 0 }) else { continue };
        let functions = if f0.header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 0..functions {
            let Some(d) = (if function == 0 { Some(f0) } else { probe(Address { bus, device, function }) }) else {
                continue;
            };
            out.push(d);
            // PCI-to-PCI bridge: continue behind it.
            if d.class == 0x06 && d.subclass == 0x04 {
                let secondary = d.addr.read8(0x19);
                if secondary > bus {
                    scan_bus(secondary, out, depth + 1);
                }
            }
        }
    }
}

/// Enumerates all functions reachable from bus 0.
pub fn enumerate() -> Vec<Device> {
    let mut out = Vec::new();
    scan_bus(0, &mut out, 0);
    out
}

pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x01, 0x00) => "SCSI storage",
        (0x01, 0x06) => "SATA controller",
        (0x01, _) => "storage controller",
        (0x02, _) => "network controller",
        (0x03, _) => "display controller",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "bridge",
        (0x0C, 0x03) => "USB controller",
        (0x0C, 0x05) => "SMBus controller",
        _ => "device",
    }
}

pub fn log_devices(devs: &[Device]) {
    for d in devs {
        kprintln!(
            "pci {} {:04x}:{:04x} class {:02x}.{:02x}.{:02x} {}",
            d.addr,
            d.vendor,
            d.device,
            d.class,
            d.subclass,
            d.prog_if,
            class_name(d.class, d.subclass)
        );
    }
}
