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
