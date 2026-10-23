//! Memory for Windows programs: image and stack mappings, the Win32
//! virtual memory API (`VirtualAlloc` and friends) and a bump arena for
//! loader structures (PEB, loader entries, strings) that live as long as
//! the process.
//!
//! OSyx maps anonymous memory eagerly and enforces W^X, so:
//! * `MEM_RESERVE` without `MEM_COMMIT` still allocates (zeroed) pages;
//!   they are tracked as reserved and become usable on commit;
//! * `PAGE_NOACCESS` pages are mapped read-only (OSyx has no "no access"
//!   user mapping); `VirtualQuery` still reports `PAGE_NOACCESS`;
//! * writable + executable protections (`PAGE_EXECUTE_READWRITE`,
//!   `PAGE_EXECUTE_WRITECOPY`) are refused.

use crate::error::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use os_std::sync::Mutex;
use osyx_abi::{PROT_EXEC, PROT_READ, PROT_WRITE};

pub const PAGE: usize = 4096;
pub const GRANULARITY: usize = 0x10000;

pub const PAGE_NOACCESS: u32 = 0x01;
pub const PAGE_READONLY: u32 = 0x02;
pub const PAGE_READWRITE: u32 = 0x04;
pub const PAGE_WRITECOPY: u32 = 0x08;
pub const PAGE_EXECUTE: u32 = 0x10;
pub const PAGE_EXECUTE_READ: u32 = 0x20;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
pub const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
pub const PAGE_GUARD: u32 = 0x100;

pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
pub const MEM_DECOMMIT: u32 = 0x4000;
pub const MEM_RELEASE: u32 = 0x8000;
pub const MEM_PRIVATE: u32 = 0x20000;
pub const MEM_IMAGE: u32 = 0x100_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prot {
    ReadOnly,
    ReadWrite,
    ExecuteRead,
}

fn prot_bits(p: Prot) -> u64 {
    match p {
        Prot::ReadOnly => PROT_READ,
        Prot::ReadWrite => PROT_READ | PROT_WRITE,
        Prot::ExecuteRead => PROT_READ | PROT_EXEC,
    }
}

/// OSyx protection for a Windows `PAGE_*` value; Err is an NTSTATUS.
pub fn from_win(protect: u32) -> Result<Prot, NtStatus> {
    match protect & !PAGE_GUARD {
        PAGE_NOACCESS | PAGE_READONLY => Ok(Prot::ReadOnly),
        PAGE_READWRITE | PAGE_WRITECOPY => Ok(Prot::ReadWrite),
        PAGE_EXECUTE | PAGE_EXECUTE_READ => Ok(Prot::ExecuteRead),
        PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => Err(STATUS_INVALID_PAGE_PROTECTION),
        _ => Err(STATUS_INVALID_PAGE_PROTECTION),
    }
}

/// Fresh zeroed pages (whole pages, 64 KiB aligned).
pub fn alloc_pages(size: usize, prot: Prot) -> Option<u64> {
    os_sys::map_ex(0, size.next_multiple_of(PAGE), prot_bits(prot), osyx_abi::MAP_ALIGN_64K).ok().map(|p| p as u64)
}

/// Pages at exactly `addr` (for images that cannot be relocated).
pub fn alloc_fixed(addr: u64, size: usize, prot: Prot) -> Option<u64> {
    os_sys::map_ex(addr as usize, size.next_multiple_of(PAGE), prot_bits(prot), osyx_abi::MAP_FIXED)
        .ok()
        .map(|p| p as u64)
}

/// Changes the protection of whole pages of a mapping made here.
pub fn protect(addr: u64, size: usize, prot: Prot) -> Result<(), osyx_abi::Errno> {
    // SAFETY: callers only change pages of mappings they own and no
    // longer write through when making them read-only or executable.
    unsafe { os_sys::protect(addr as *mut u8, size.next_multiple_of(PAGE), prot_bits(prot)) }
}

pub fn free_pages(addr: u64, size: usize) {
    // SAFETY: `addr`/`size` describe one whole mapping made by this module.
    let _ = unsafe { os_sys::unmap(addr as *mut u8, size.next_multiple_of(PAGE)) };
}

// ---- arena -------------------------------------------------------------------

struct Arena {
    cur: u64,
    end: u64,
}

static ARENA: Mutex<Arena> = Mutex::new(Arena { cur: 0, end: 0 });

/// Zeroed memory that lives until the process ends (loader structures that
/// Windows code holds pointers to).
pub fn arena_alloc(size: usize, align: usize) -> u64 {
    let mut a = ARENA.lock().unwrap();
    let mut p = a.cur.next_multiple_of(align as u64);
    if a.cur == 0 || p + size as u64 > a.end {
        let chunk = size.next_multiple_of(GRANULARITY).max(GRANULARITY);
        let base = alloc_pages(chunk, Prot::ReadWrite).expect("memory for loader structures");
        a.cur = base;
        a.end = base + chunk as u64;
        p = base;
    }
    a.cur = p + size as u64;
    p
}

// ---- VirtualAlloc ------------------------------------------------------------

struct Region {
    size: usize,
    /// Per page: Windows protection (0 = reserved, not committed).
    pages: Vec<u32>,
}

static REGIONS: Mutex<BTreeMap<u64, Region>> = Mutex::new(BTreeMap::new());

/// `NtAllocateVirtualMemory` / `VirtualAlloc`. Returns the base.
pub fn virtual_alloc(addr: u64, size: usize, kind: u32, protect: u32) -> Result<u64, NtStatus> {
    if size == 0
        || kind & (MEM_COMMIT | MEM_RESERVE) == 0
        || kind & !(MEM_COMMIT | MEM_RESERVE | 0x20_0000 | 0x10_0000) != 0
    {
        return Err(STATUS_INVALID_PARAMETER); // (TOP_DOWN 0x100000 and RESET 0x80000 aside)
    }
    let prot = from_win(protect)?;
    let mut regions = REGIONS.lock().unwrap();
    if kind & MEM_RESERVE == 0 {
        // Commit inside an existing reservation.
        let (&base, r) = regions.range_mut(..=addr).next_back().ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
        let first = (addr - base) as usize / PAGE;
        let last = ((addr - base) as usize + size).div_ceil(PAGE);
        if last > r.pages.len() {
            return Err(STATUS_CONFLICTING_ADDRESSES);
        }
        let start = base + (first * PAGE) as u64;
        protect_range(start, (last - first) * PAGE, prot).map_err(errno_to_status)?;
        for p in &mut r.pages[first..last] {
            *p = protect;
        }
        return Ok(start);
    }
    let size = size.next_multiple_of(PAGE);
    let base = if addr != 0 {
        let a = addr & !(GRANULARITY as u64 - 1);
        alloc_fixed(a, size, prot).ok_or(STATUS_CONFLICTING_ADDRESSES)?
    } else {
        alloc_pages(size, prot).ok_or(STATUS_NO_MEMORY)?
    };
    let state = if kind & MEM_COMMIT != 0 { protect } else { 0 };
    regions.insert(base, Region { size, pages: alloc::vec![state; size / PAGE] });
    Ok(base)
}

fn protect_range(addr: u64, size: usize, prot: Prot) -> Result<(), osyx_abi::Errno> {
    protect(addr, size, prot)
}

/// `NtFreeVirtualMemory` / `VirtualFree`.
pub fn virtual_free(addr: u64, size: usize, kind: u32) -> Result<(), NtStatus> {
    let mut regions = REGIONS.lock().unwrap();
    match kind {
        MEM_RELEASE => {
            if size != 0 {
                return Err(STATUS_INVALID_PARAMETER);
            }
            let r = regions.remove(&addr).ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
            free_pages(addr, r.size);
            Ok(())
        }
        MEM_DECOMMIT => {
            let (&base, r) = regions.range_mut(..=addr).next_back().ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
            let first = (addr - base) as usize / PAGE;
            let last = if size == 0 { r.pages.len() } else { ((addr - base) as usize + size).div_ceil(PAGE) };
            if last > r.pages.len() {
                return Err(STATUS_MEMORY_NOT_ALLOCATED);
            }
            // The pages stay mapped (OSyx has no partial unmap); their
            // contents are cleared, as a later commit must see zeros.
            let start = base + (first * PAGE) as u64;
            protect_range(start, (last - first) * PAGE, Prot::ReadWrite).map_err(errno_to_status)?;
            // SAFETY: the range lies in this region and is now writable.
            unsafe { core::ptr::write_bytes(start as *mut u8, 0, (last - first) * PAGE) };
            for p in &mut r.pages[first..last] {
                *p = 0;
            }
            Ok(())
        }
        _ => Err(STATUS_INVALID_PARAMETER),
    }
}

/// `NtProtectVirtualMemory` / `VirtualProtect`: returns the old protection
/// of the first page.
pub fn virtual_protect(addr: u64, size: usize, protect: u32) -> Result<u32, NtStatus> {
    let prot = from_win(protect)?;
    let mut regions = REGIONS.lock().unwrap();
    let (&base, r) = regions.range_mut(..=addr).next_back().ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
    let first = (addr - base) as usize / PAGE;
    let last = ((addr - base) as usize + size.max(1)).div_ceil(PAGE);
    if last > r.pages.len() || r.pages[first..last].contains(&0) {
        return Err(STATUS_MEMORY_NOT_ALLOCATED);
    }
    let old = r.pages[first];
    let start = base + (first * PAGE) as u64;
    protect_range(start, (last - first) * PAGE, prot).map_err(errno_to_status)?;
    for p in &mut r.pages[first..last] {
        *p = protect;
    }
    Ok(old)
}

/// What `VirtualQuery` reports for `addr`.
pub struct Info {
    pub base: u64,
    pub allocation_base: u64,
    pub allocation_protect: u32,
    pub size: u64,
    pub state: u32,
    pub protect: u32,
    pub kind: u32,
}

/// `NtQueryVirtualMemory` / `VirtualQuery` for regions made by
/// `virtual_alloc` and for loaded images.
pub fn virtual_query(addr: u64) -> Option<Info> {
    let regions = REGIONS.lock().unwrap();
    if let Some((&base, r)) = regions.range(..=addr).next_back() {
        if addr < base + r.size as u64 {
            let i = (addr - base) as usize / PAGE;
            let state = r.pages[i];
            let run = r.pages[i..].iter().take_while(|&&p| p == state).count();
            return Some(Info {
                base: base + (i * PAGE) as u64,
                allocation_base: base,
                allocation_protect: r.pages[0].max(PAGE_READWRITE),
                size: (run * PAGE) as u64,
                state: if state == 0 { MEM_RESERVE } else { MEM_COMMIT },
                protect: state,
                kind: MEM_PRIVATE,
            });
        }
    }
    drop(regions);
    crate::mapping::query(addr).or_else(|| crate::loader::query_image(addr))
}
