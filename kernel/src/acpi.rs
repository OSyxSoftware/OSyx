//! Minimal ACPI table lookup: just enough to find the timers the kernel can
//! calibrate against when the PIT does not count (the PM timer from the FADT
//! and the HPET). Tables are read in place through the HHDM; every table is
//! checksummed and length-checked before a field is used.

use crate::mem;
use core::sync::atomic::{AtomicU64, Ordering};

static RSDP: AtomicU64 = AtomicU64::new(0);

/// Remembers the RSDP (physical address from BootInfo, 0 if none).
pub fn init(rsdp_phys: u64) {
    RSDP.store(rsdp_phys, Ordering::Relaxed);
}

/// Reads `N` bytes of physical memory, if they lie inside the HHDM.
fn read<const N: usize>(pa: u64) -> Option<[u8; N]> {
    if pa == 0 || pa.checked_add(N as u64)? > mem::hhdm_size() {
        return None;
    }
    // SAFETY: the range lies inside the HHDM (checked above); ACPI tables
    // live in firmware-reserved memory the kernel never allocates.
    Some(unsafe { core::ptr::read_unaligned(mem::phys_to_virt(pa) as *const [u8; N]) })
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u32_at(b, off) as u64 | (u32_at(b, off + 4) as u64) << 32
}

/// Physical address and length of the table at `pa` if its checksum holds.
fn checked_table(pa: u64) -> Option<(u64, usize)> {
    let header: [u8; 36] = read(pa)?;
    let len = u32_at(&header, 4) as usize;
    if !(36..=1 << 20).contains(&len) || pa + len as u64 > mem::hhdm_size() {
        return None;
    }
    // SAFETY: [pa, pa + len) lies inside the HHDM (checked above).
    let bytes = unsafe { core::slice::from_raw_parts(mem::phys_to_virt(pa) as *const u8, len) };
    (bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0).then_some((pa, len))
}

/// Finds the table with signature `sig` through the XSDT (or the RSDT).
/// Returns its physical address and length.
pub fn find(sig: &[u8; 4]) -> Option<(u64, usize)> {
    let rsdp: [u8; 36] = read(RSDP.load(Ordering::Relaxed))?;
    if &rsdp[..8] != b"RSD PTR " {
        return None;
    }
    let (root, entry_size) = if rsdp[15] >= 2 && u64_at(&rsdp, 24) != 0 { (u64_at(&rsdp, 24), 8) } else { (u32_at(&rsdp, 16) as u64, 4) };
    let (root, len) = checked_table(root)?;
    for i in 0..(len - 36) / entry_size {
        let at = root + 36 + (i * entry_size) as u64;
        let pa = if entry_size == 8 { u64::from_le_bytes(read(at)?) } else { u32::from_le_bytes(read(at)?) as u64 };
        let Some(s) = read::<4>(pa) else { continue };
        if &s == sig {
            if let Some(t) = checked_table(pa) {
                return Some(t);
            }
        }
    }
    None
}

/// The ACPI PM timer: I/O port and whether the counter is 32 bits wide
/// (otherwise 24). It runs at 3.579545 MHz.
pub fn pm_timer() -> Option<(u16, bool)> {
    let (pa, len) = find(b"FACP")?;
    if len < 116 {
        return None;
    }
    let fadt: [u8; 116] = read(pa)?;
    let ext32 = u32_at(&fadt, 112) & (1 << 8) != 0;
    // X_PM_TMR_BLK (ACPI 2.0+), if it is an I/O-space address.
    if len >= 220 {
        let x: [u8; 220] = read(pa)?;
        let addr = u64_at(&x, 208 + 4);
        if x[208] == 1 && addr != 0 && addr <= 0xFFFF {
            return Some((addr as u16, ext32));
        }
    }
    let port = u32_at(&fadt, 76);
    (port != 0 && port <= 0xFFFF).then_some((port as u16, ext32))
}

/// Physical base address of the HPET register block.
pub fn hpet() -> Option<u64> {
    let (pa, len) = find(b"HPET")?;
    if len < 56 {
        return None;
    }
    let t: [u8; 56] = read(pa)?;
    // Base address: a generic address structure that must be in memory space.
    let base = u64_at(&t, 44);
    (t[40] == 0 && base != 0).then_some(base)
}

/// How to reach S5 and reset the machine, from the FADT and the DSDT.
pub struct Power {
    pub pm1a_cnt: u16,
    pub pm1b_cnt: Option<u16>,
    /// SMI command port and the value that hands the chipset to the OS.
    pub smi_cmd: u16,
    pub acpi_enable: u8,
    /// SLP_TYPa and SLP_TYPb of `\_S5`.
    pub s5: Option<(u8, u8)>,
    pub reset: Option<Reset>,
}

/// The FADT reset register.
pub enum Reset {
    Io(u16, u8),
    /// Bus 0 configuration space: device, function, register, value.
    Pci(u8, u8, u8, u8),
}

/// An I/O port from a generic address structure (space id 1) or a 32-bit
/// block address.
fn io_port(space: u8, addr: u64) -> Option<u16> {
    (space == 1 && addr != 0 && addr <= 0xFFFF).then_some(addr as u16)
}

/// One `\_S5` package element: a data object that is a small integer.
fn aml_byte(b: &[u8], i: &mut usize) -> Option<u8> {
    let op = *b.get(*i)?;
    *i += 1;
    match op {
        0x00 => Some(0),
        0x01 => Some(1),
        0xFF => Some(0xFF),
        0x0A => {
            *i += 1;
            b.get(*i - 1).copied()
        }
        0x0B | 0x0C => {
            let v = *b.get(*i)?;
            *i += if op == 0x0B { 2 } else { 4 };
            Some(v)
        }
        _ => None,
    }
}

/// SLP_TYPa/b from the `Name (_S5, Package () {a, b, ...})` object of the
/// DSDT. Only this static form is read (it is how firmware declares S5);
/// no AML is executed.
fn s5_from_dsdt(pa: u64) -> Option<(u8, u8)> {
    let (pa, len) = checked_table(pa)?;
    // SAFETY: checked_table verified the range lies inside the HHDM.
    let t = unsafe { core::slice::from_raw_parts(mem::phys_to_virt(pa) as *const u8, len) };
    let body = &t[36..];
    let mut from = 0;
    while let Some(pos) = body[from..].windows(4).position(|w| w == b"_S5_") {
        let at = from + pos;
        from = at + 4;
        // NameOp, optionally followed by the root prefix.
        let name_op = match at.checked_sub(1).map(|i| body[i]) {
            Some(0x08) => true,
            Some(0x5C) => at >= 2 && body[at - 2] == 0x08,
            _ => false,
        };
        if !name_op || body.get(at + 4) != Some(&0x12) {
            continue;
        }
        let mut i = at + 5;
        let lead = *body.get(i)?;
        i += 1 + (lead >> 6) as usize; // PkgLength
        i += 1; // NumElements
        let a = aml_byte(body, &mut i)?;
        let b = aml_byte(body, &mut i).unwrap_or(a);
        return Some((a & 7, b & 7));
    }
    None
}

pub fn power() -> Option<Power> {
    let (pa, len) = find(b"FACP")?;
    if len < 116 {
        return None;
    }
    let mut f = [0u8; 276];
    // SAFETY: the table lies inside the HHDM (find checked it).
    let src = unsafe { core::slice::from_raw_parts(mem::phys_to_virt(pa) as *const u8, len.min(276)) };
    f[..src.len()].copy_from_slice(src);
    let x = |off: usize| len >= off + 12;
    let pm1a = if x(172) { io_port(f[172], u64_at(&f, 176)) } else { None }
        .or_else(|| io_port(1, u32_at(&f, 64) as u64))?;
    let pm1b = if x(184) { io_port(f[184], u64_at(&f, 188)) } else { None }.or_else(|| io_port(1, u32_at(&f, 68) as u64));
    let dsdt = if len >= 148 && u64_at(&f, 140) != 0 { u64_at(&f, 140) } else { u32_at(&f, 40) as u64 };
    let reset = if len >= 129 && u32_at(&f, 112) & (1 << 10) != 0 {
        let addr = u64_at(&f, 120);
        let value = f[128];
        match f[116] {
            1 if addr <= 0xFFFF && addr != 0 => Some(Reset::Io(addr as u16, value)),
            2 => Some(Reset::Pci((addr >> 32) as u8, (addr >> 16) as u8, addr as u8, value)),
            _ => None,
        }
    } else {
        None
    };
    Some(Power {
        pm1a_cnt: pm1a,
        pm1b_cnt: pm1b,
        smi_cmd: u32_at(&f, 48) as u16,
        acpi_enable: f[52],
        s5: s5_from_dsdt(dsdt),
        reset,
    })
}
