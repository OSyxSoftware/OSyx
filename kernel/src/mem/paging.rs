//! Page table manipulation for the kernel address space.
//!
//! The kernel keeps running on the PML4 built by the boot loader. Page table
//! pages are accessed through the HHDM. All modifications are serialised by
//! `PAGING` (lock order: HEAP -> PAGING -> FRAMES).

use super::{alloc_frame, hhdm_offset, hhdm_size, phys_to_virt, PAGE_SIZE};
use crate::arch::cpu;
use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicBool, Ordering};

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const NO_EXEC: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// The top 2 GiB hold the kernel image; generic mappings may not touch it.
const KERNEL_IMAGE_SPACE: u64 = 0xFFFF_FFFF_8000_0000;

static NX: AtomicBool = AtomicBool::new(false);
static PAGING: IrqSpinLock<()> = IrqSpinLock::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    Unaligned,
    NonCanonical,
    ReservedRange,
    AlreadyMapped,
    NotMapped,
    HugePage,
    OutOfMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub uncached: bool,
}

impl Flags {
    pub const KERNEL_RW: Flags = Flags { writable: true, executable: false, user: false, uncached: false };

    fn bits(self) -> u64 {
        let mut b = PRESENT;
        if self.writable {
            b |= WRITABLE;
        }
        if self.user {
            b |= USER;
        }
        if self.uncached {
            b |= NO_CACHE;
        }
        if !self.executable && NX.load(Ordering::Relaxed) {
            b |= NO_EXEC;
        }
        b
    }
}

/// Effective permissions of a mapped address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translation {
    pub phys: u64,
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub page_size: u64,
}

pub fn is_canonical(va: u64) -> bool {
    let top = va >> 47;
    top == 0 || top == 0x1_FFFF
}

fn table(pa: u64) -> *mut u64 {
    phys_to_virt(pa) as *mut u64
}

fn pml4() -> u64 {
    cpu::read_cr3() & ADDR_MASK
}

/// Records whether EFER.NXE is active (set by the boot loader when supported).
pub fn init() {
    // SAFETY: IA32_EFER exists on every x86-64 CPU.
    let efer = unsafe { cpu::rdmsr(0xC000_0080) };
    NX.store(efer & (1 << 11) != 0, Ordering::Relaxed);
}

pub fn nx_enabled() -> bool {
    NX.load(Ordering::Relaxed)
}

/// Removes the boot loader's identity map of low memory (PML4 slots 0..256).
/// After this, no lower-half address is mapped in the kernel address space.
pub fn remove_identity_map() -> usize {
    let _g = PAGING.lock();
    let root = table(pml4());
    let mut removed = 0;
    for i in 0..256 {
        // SAFETY: entries of the active PML4, accessed via the HHDM.
        unsafe {
            if *root.add(i) & PRESENT != 0 {
                *root.add(i) = 0;
                removed += 1;
            }
        }
    }
    // SAFETY: reloading CR3 with the same root flushes non-global TLB entries.
    unsafe { cpu::write_cr3(cpu::read_cr3()) };
    removed
}

fn check_virt(va: u64) -> Result<(), MapError> {
    if !va.is_multiple_of(PAGE_SIZE) {
        return Err(MapError::Unaligned);
    }
    if !is_canonical(va) {
        return Err(MapError::NonCanonical);
    }
    let hhdm = hhdm_offset();
    if (va >= hhdm && va < hhdm + hhdm_size()) || va >= KERNEL_IMAGE_SPACE {
        return Err(MapError::ReservedRange);
    }
    Ok(())
}

/// Maps the 4 KiB page at `va` to the frame at `pa`.
pub fn map(va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
    check_virt(va)?;
    if !pa.is_multiple_of(PAGE_SIZE) {
        return Err(MapError::Unaligned);
    }
    let _g = PAGING.lock();
    let mut t = pml4();
    for shift in [39u64, 30, 21] {
        let e = unsafe { table(t).add(((va >> shift) & 511) as usize) };
        // SAFETY: `e` points into a page table page reachable through the HHDM.
        let val = unsafe { *e };
        if val & PRESENT == 0 {
            let new = alloc_frame().ok_or(MapError::OutOfMemory)?;
            let mut inter = new | PRESENT | WRITABLE;
            if flags.user {
                inter |= USER;
            }
            // SAFETY: as above.
            unsafe { *e = inter };
            t = new;
        } else if val & HUGE != 0 {
            return Err(MapError::HugePage);
        } else {
            if flags.user && val & USER == 0 {
                // SAFETY: as above; widening an intermediate entry is harmless
                // because leaf entries still control access.
                unsafe { *e = val | USER };
            }
            t = val & ADDR_MASK;
        }
    }
    let e = unsafe { table(t).add(((va >> 12) & 511) as usize) };
    // SAFETY: as above.
    unsafe {
        if *e & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *e = (pa & ADDR_MASK) | flags.bits();
    }
    cpu::invlpg(va);
    Ok(())
}

/// Unmaps the 4 KiB page at `va` and returns the frame it pointed to (the
/// caller owns that frame afterwards).
pub fn unmap(va: u64) -> Result<u64, MapError> {
    check_virt(va)?;
    let _g = PAGING.lock();
    let mut t = pml4();
    for shift in [39u64, 30, 21] {
        // SAFETY: page table pages reachable through the HHDM.
        let val = unsafe { *table(t).add(((va >> shift) & 511) as usize) };
        if val & PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        if val & HUGE != 0 {
            return Err(MapError::HugePage);
        }
        t = val & ADDR_MASK;
    }
    let e = unsafe { table(t).add(((va >> 12) & 511) as usize) };
    // SAFETY: as above.
    let val = unsafe { *e };
    if val & PRESENT == 0 {
        return Err(MapError::NotMapped);
    }
    // SAFETY: as above.
    unsafe { *e = 0 };
    cpu::invlpg(va);
    Ok(val & ADDR_MASK)
}

/// Walks the active page tables (handling 2 MiB / 1 GiB pages).
pub fn translate(va: u64) -> Option<Translation> {
    if !is_canonical(va) {
        return None;
    }
    let _g = PAGING.lock();
    let mut t = pml4();
    let (mut writable, mut user, mut nx) = (true, true, false);
    for (level, shift) in [39u64, 30, 21, 12].into_iter().enumerate() {
        // SAFETY: page table pages reachable through the HHDM.
        let val = unsafe { *table(t).add(((va >> shift) & 511) as usize) };
        if val & PRESENT == 0 {
            return None;
        }
        writable &= val & WRITABLE != 0;
        user &= val & USER != 0;
        nx |= val & NO_EXEC != 0 && nx_enabled();
        let leaf = level == 3 || (level > 0 && val & HUGE != 0);
        if leaf {
            let size = 1u64 << shift;
            let base = val & ADDR_MASK & !(size - 1);
            return Some(Translation {
                phys: base + (va & (size - 1)),
                writable,
                executable: !nx,
                user,
                page_size: size,
            });
        }
        t = val & ADDR_MASK;
    }
    None
}
