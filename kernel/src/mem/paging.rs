//! Page table manipulation.
//!
//! Every function takes the physical address of a PML4 ("root"). The kernel
//! root is the PML4 built by the boot loader; each process gets its own root
//! whose upper half (slots 256..512) is copied from the kernel root. Because
//! [`init`] pre-allocates a PDPT for every upper-half slot, later kernel
//! mappings (heap growth, MMIO) appear in every address space automatically.
//!
//! Page table pages are accessed through the HHDM. All modifications are
//! serialised by `PAGING` (lock order: HEAP -> PAGING -> FRAMES).

use super::{alloc_frame, free_frame, hhdm_offset, hhdm_size, phys_to_virt, PAGE_SIZE};
use crate::arch::cpu;
use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use osyx_abi::USER_END;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
/// PAT bit of a 4 KiB page table entry (same position as HUGE one level up).
const PAT_4K: u64 = 1 << 7;
const IA32_PAT: u32 = 0x277;
/// PAT memory type "write-combining".
const PAT_WC: u64 = 0x01;
pub const NO_EXEC: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// The top 2 GiB hold the kernel image; generic mappings may not touch it.
const KERNEL_IMAGE_SPACE: u64 = 0xFFFF_FFFF_8000_0000;
/// The first 64 KiB of user space stay unmapped to catch null dereferences.
pub const USER_START: u64 = 0x1_0000;

static NX: AtomicBool = AtomicBool::new(false);
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);
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
    /// Write-combining (framebuffers): PAT entry 4, see [`init`].
    pub write_combining: bool,
}

impl Flags {
    pub const KERNEL_RW: Flags = Flags { writable: true, executable: false, user: false, uncached: false, write_combining: false };

    pub const fn user(writable: bool, executable: bool) -> Flags {
        Flags { writable, executable, user: true, uncached: false, write_combining: false }
    }

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
        } else if self.write_combining {
            b |= PAT_4K;
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

/// Root of the kernel address space.
pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(Ordering::Relaxed)
}

/// Root of the currently active address space.
pub fn current_root() -> u64 {
    cpu::read_cr3() & ADDR_MASK
}

/// Records NX state and the kernel root, and pre-allocates all upper-half
/// PDPTs so the kernel half can be shared by reference between roots.
pub fn init() {
    // SAFETY: IA32_EFER exists on every x86-64 CPU.
    let efer = unsafe { cpu::rdmsr(0xC000_0080) };
    NX.store(efer & (1 << 11) != 0, Ordering::Relaxed);
    // PAT entry 4 (selected by the PAT bit alone) becomes write-combining;
    // the default PAT has write-back there, which nothing uses (the kernel
    // never set the PAT bit before). Entries 0-3 keep their defaults.
    // SAFETY: IA32_PAT exists on every x86-64 CPU; no mapping uses entry 4
    // yet, so changing it alters no live memory type.
    unsafe {
        let pat = cpu::rdmsr(IA32_PAT);
        cpu::wrmsr(IA32_PAT, (pat & !(0xFF << 32)) | (PAT_WC << 32));
    }
    let root = current_root();
    KERNEL_ROOT.store(root, Ordering::Relaxed);
    let _g = PAGING.lock();
    for i in 256..512 {
        // SAFETY: entries of the active PML4, accessed via the HHDM.
        unsafe {
            let e = table(root).add(i);
            if *e & PRESENT == 0 {
                let pdpt = alloc_frame().expect("out of memory pre-allocating kernel PDPTs");
                *e = pdpt | PRESENT | WRITABLE;
            }
        }
    }
}

pub fn nx_enabled() -> bool {
    NX.load(Ordering::Relaxed)
}

/// Removes the boot loader's identity map of low memory (PML4 slots 0..256).
/// After this, no lower-half address is mapped in the kernel address space.
pub fn remove_identity_map() -> usize {
    let _g = PAGING.lock();
    let root = table(kernel_root());
    let mut removed = 0;
    for i in 0..256 {
        // SAFETY: entries of the kernel PML4, accessed via the HHDM.
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

fn check_virt(va: u64, user: bool) -> Result<(), MapError> {
    if !va.is_multiple_of(PAGE_SIZE) {
        return Err(MapError::Unaligned);
    }
    if !is_canonical(va) {
        return Err(MapError::NonCanonical);
    }
    if user {
        if !(USER_START..USER_END).contains(&va) {
            return Err(MapError::ReservedRange);
        }
        return Ok(());
    }
    let hhdm = hhdm_offset();
    if va < USER_END || (va >= hhdm && va < hhdm + hhdm_size()) || va >= KERNEL_IMAGE_SPACE {
        return Err(MapError::ReservedRange);
    }
    Ok(())
}

/// Maps the 4 KiB page at `va` to `pa` in the kernel address space.
pub fn map(va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
    if flags.user {
        return Err(MapError::ReservedRange);
    }
    map_in(kernel_root(), va, pa, flags).map(|_| ())
}

/// Unmaps a kernel page and returns its frame.
pub fn unmap(va: u64) -> Result<u64, MapError> {
    unmap_in(kernel_root(), va, false)
}

/// Translates through the currently active address space.
pub fn translate(va: u64) -> Option<Translation> {
    translate_in(current_root(), va)
}

/// Maps the 4 KiB page at `va` to the frame at `pa` in `root`. User mappings
/// must lie in the lower half, kernel mappings in the upper half outside the
/// HHDM and the kernel image. Returns how many page-table frames were
/// allocated (so owners can account for every frame they cause).
pub fn map_in(root: u64, va: u64, pa: u64, flags: Flags) -> Result<u64, MapError> {
    check_virt(va, flags.user)?;
    if !pa.is_multiple_of(PAGE_SIZE) {
        return Err(MapError::Unaligned);
    }
    let _g = PAGING.lock();
    let mut t = root;
    let mut tables = 0;
    for shift in [39u64, 30, 21] {
        // SAFETY: `t` is a page table page reachable through the HHDM.
        let e = unsafe { table(t).add(((va >> shift) & 511) as usize) };
        // SAFETY: as above.
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
            tables += 1;
        } else if val & HUGE != 0 {
            return Err(MapError::HugePage);
        } else {
            t = val & ADDR_MASK;
        }
    }
    // SAFETY: as above.
    let e = unsafe { table(t).add(((va >> 12) & 511) as usize) };
    // SAFETY: as above.
    unsafe {
        if *e & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *e = (pa & ADDR_MASK) | flags.bits();
    }
    if root == current_root() || va >= USER_END {
        cpu::invlpg(va);
    }
    Ok(tables)
}

/// Unmaps the 4 KiB page at `va` in `root` and returns the frame it pointed to
/// (the caller owns that frame afterwards).
pub fn unmap_in(root: u64, va: u64, user: bool) -> Result<u64, MapError> {
    check_virt(va, user)?;
    let _g = PAGING.lock();
    let mut t = root;
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
    // SAFETY: as above.
    let e = unsafe { table(t).add(((va >> 12) & 511) as usize) };
    // SAFETY: as above.
    let val = unsafe { *e };
    if val & PRESENT == 0 {
        return Err(MapError::NotMapped);
    }
    // SAFETY: as above.
    unsafe { *e = 0 };
    if root == current_root() || va >= USER_END {
        cpu::invlpg(va);
    }
    Ok(val & ADDR_MASK)
}

/// Walks the page tables of `root` (handling 2 MiB / 1 GiB pages).
pub fn translate_in(root: u64, va: u64) -> Option<Translation> {
    if !is_canonical(va) {
        return None;
    }
    let _g = PAGING.lock();
    let mut t = root;
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

/// Creates a new address space: empty user half, shared kernel half.
pub fn new_address_space() -> Result<u64, MapError> {
    let root = alloc_frame().ok_or(MapError::OutOfMemory)?;
    let _g = PAGING.lock();
    let kernel = table(kernel_root());
    // SAFETY: both roots are page table pages reachable through the HHDM; the
    // new one was zeroed by alloc_frame.
    unsafe {
        for i in 256..512 {
            *table(root).add(i) = *kernel.add(i);
        }
    }
    Ok(root)
}

/// Frees every frame of the user half of `root` (leaf pages and page tables)
/// and the root itself. Returns the number of frames released.
///
/// # Safety
/// `root` must not be active on any CPU and must not be used afterwards; all
/// user-half leaf frames must be owned exclusively by this address space.
pub unsafe fn destroy_address_space(root: u64) -> u64 {
    assert_ne!(root, kernel_root(), "attempt to destroy the kernel address space");
    assert_ne!(root, current_root(), "attempt to destroy the active address space");
    let _g = PAGING.lock();
    let mut freed = 0;
    unsafe fn release(t: u64, level: u32, freed: &mut u64) {
        for i in 0..512 {
            let val = *table(t).add(i);
            if val & PRESENT == 0 {
                continue;
            }
            let next = val & ADDR_MASK;
            if level > 1 && val & HUGE == 0 {
                release(next, level - 1, freed);
            }
            if free_frame(next).is_ok() {
                *freed += 1;
            }
        }
    }
    for i in 0..256 {
        let val = *table(root).add(i);
        if val & PRESENT != 0 {
            release(val & ADDR_MASK, 3, &mut freed);
            if free_frame(val & ADDR_MASK).is_ok() {
                freed += 1;
            }
        }
    }
    if free_frame(root).is_ok() {
        freed += 1;
    }
    freed
}
