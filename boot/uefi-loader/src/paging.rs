//! Construction of the kernel's initial 4-level page tables.
//!
//! Layout (see `osyx_bootinfo` docs):
//! * `PML4[0..n]`: identity map of `[0, hhdm_size)` with 2 MiB pages,
//!   executable, needed by the loader's CR3-switch trampoline.
//! * `PML4[256..256+n]`: the same PDPTs again, forming the HHDM at
//!   `HHDM_OFFSET`; marked NX at the PML4 level so the shared leaves are not
//!   executable through this alias.
//! * `PML4[511]`: kernel segments in the top 2 GiB with 4 KiB pages and
//!   per-segment R / RW / RX permissions.

use osyx_elf::{KernelImage, PAGE_SIZE};

pub const HHDM_OFFSET: u64 = 0xFFFF_8000_0000_0000;
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const NO_EXEC: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const GIB: u64 = 1 << 30;
const MIB2: u64 = 1 << 21;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    OutOfTables,
    AlreadyMapped(u64),
    HugePageConflict(u64),
}

/// A bump allocator over a pre-allocated, identity-accessible block of pages.
pub struct TableBuilder {
    pool: u64,
    used: u64,
    capacity: u64,
    nx: bool,
    pub pml4: u64,
}

/// Number of table pages [`TableBuilder`] needs for the given configuration.
pub fn tables_needed(hhdm_size: u64, kernel_span: u64) -> u64 {
    let gib = hhdm_size.div_ceil(GIB);
    let pdpts = gib.div_ceil(512);
    // +2: kernel PDPT and PD; +1: a segment straddling a 2 MiB boundary.
    1 + pdpts + gib + 2 + kernel_span.div_ceil(MIB2) + 1
}

impl TableBuilder {
    /// # Safety
    /// `pool` must be the physical (= identity-mapped, pre-ExitBootServices)
    /// address of `capacity` writable pages owned by the caller.
    pub unsafe fn new(pool: u64, capacity: u64, nx: bool) -> Result<Self, PagingError> {
        let mut t = TableBuilder { pool, used: 0, capacity, nx, pml4: 0 };
        t.pml4 = t.alloc()?;
        Ok(t)
    }

    fn alloc(&mut self) -> Result<u64, PagingError> {
        if self.used == self.capacity {
            return Err(PagingError::OutOfTables);
        }
        let page = self.pool + self.used * PAGE_SIZE;
        self.used += 1;
        // SAFETY: inside the caller-provided pool (see `new`).
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE as usize) };
        Ok(page)
    }

    fn entry(table: u64, index: u64) -> *mut u64 {
        (table + index * 8) as *mut u64
    }

    /// Identity map and HHDM of `[0, size)`; `size` must be a multiple of 1 GiB.
    pub fn map_physical(&mut self, size: u64) -> Result<(), PagingError> {
        let mut pdpt = 0;
        for g in 0..size / GIB {
            if g.is_multiple_of(512) {
                pdpt = self.alloc()?;
                let slot = g / 512;
                // SAFETY: entries of tables allocated from our pool.
                unsafe {
                    *Self::entry(self.pml4, slot) = pdpt | PRESENT | WRITABLE;
                    let nx = if self.nx { NO_EXEC } else { 0 };
                    *Self::entry(self.pml4, 256 + slot) = pdpt | PRESENT | WRITABLE | nx;
                }
            }
            let pd = self.alloc()?;
            for i in 0..512 {
                let phys = g * GIB + i * MIB2;
                // SAFETY: as above.
                unsafe { *Self::entry(pd, i) = phys | PRESENT | WRITABLE | HUGE };
            }
            // SAFETY: as above.
            unsafe { *Self::entry(pdpt, g % 512) = pd | PRESENT | WRITABLE };
        }
        Ok(())
    }

    /// Maps one 4 KiB page, creating intermediate tables as needed.
    pub fn map_page(&mut self, virt: u64, phys: u64, writable: bool, exec: bool) -> Result<(), PagingError> {
        let mut table = self.pml4;
        for shift in [39u64, 30, 21] {
            let e = Self::entry(table, (virt >> shift) & 511);
            // SAFETY: `table` is one of our pool pages.
            let val = unsafe { *e };
            if val & PRESENT == 0 {
                let next = self.alloc()?;
                // SAFETY: as above.
                unsafe { *e = next | PRESENT | WRITABLE };
                table = next;
            } else if val & HUGE != 0 {
                return Err(PagingError::HugePageConflict(virt));
            } else {
                table = val & ADDR_MASK;
            }
        }
        let e = Self::entry(table, (virt >> 12) & 511);
        // SAFETY: as above.
        unsafe {
            if *e & PRESENT != 0 {
                return Err(PagingError::AlreadyMapped(virt));
            }
            let mut flags = PRESENT;
            if writable {
                flags |= WRITABLE;
            }
            if !exec && self.nx {
                flags |= NO_EXEC;
            }
            *e = (phys & ADDR_MASK) | flags;
        }
        Ok(())
    }

    /// Maps every kernel segment from the physical block at `phys_base`.
    pub fn map_kernel(&mut self, k: &KernelImage, phys_base: u64) -> Result<(), PagingError> {
        for seg in k.segments() {
            let mut va = seg.vaddr;
            while va < seg.page_end() {
                let pa = phys_base + (va - k.virt_base());
                self.map_page(va, pa, seg.writable(), seg.executable())?;
                va += PAGE_SIZE;
            }
        }
        Ok(())
    }

    pub fn used(&self) -> u64 {
        self.used
    }
}
