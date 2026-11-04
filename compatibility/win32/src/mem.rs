//! Memory for Windows programs: image and stack mappings, the Win32
//! virtual memory API (`VirtualAlloc` and friends) and a bump arena for
//! loader structures (PEB, loader entries, strings) that live as long as
//! the process.
//!
//! OSyx gives anonymous memory a frame on first access (demand-zero) and
//! enforces W^X, so:
//! * `MEM_RESERVE` costs address space only; the pages are tracked as
//!   reserved and become usable on commit;
//! * `MEM_DECOMMIT` drops the pages' frames (`discard`), so a later commit
//!   reads zeros again;
//! * `PAGE_NOACCESS` pages are mapped read-only (OSyx has no "no access"
//!   user mapping); `VirtualQuery` still reports `PAGE_NOACCESS`;
//! * private `PAGE_EXECUTE_READWRITE` memory alternates between RW/NX and
//!   RX on write/execute faults; the kernel never maps a page W+X. A write
//!   instruction executing on the same page it modifies is unsupported.
//!   Executable writable sections (`PAGE_EXECUTE_WRITECOPY`) are refused.

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
/// "The contents are no longer needed": an allocator's purge hint.
pub const MEM_RESET: u32 = 0x8_0000;
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

/// Private JIT memory starts writable, with execution enabled lazily by
/// the fault dispatcher. File/section mappings still use `from_win`.
fn private_prot(protect: u32) -> Result<Prot, NtStatus> {
    if protect == PAGE_EXECUTE_READWRITE {
        Ok(Prot::ReadWrite)
    } else {
        from_win(protect)
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
    crate::mapping::note_freed(addr, size.next_multiple_of(PAGE) as u64, "pages winrun freed");
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
    pages: Pages,
}

/// The per-page states of a region as runs (page index where a run starts
/// -> its state, to the next key): a reservation of a terabyte costs a few
/// entries, not a word per page.
struct Pages {
    len: usize,
    runs: BTreeMap<usize, u32>,
}

impl Pages {
    fn new(len: usize, state: u32) -> Pages {
        let mut runs = BTreeMap::new();
        runs.insert(0, state);
        Pages { len, runs }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, i: usize) -> Option<u32> {
        (i < self.len).then(|| self.runs.range(..=i).next_back().map_or(0, |(_, &s)| s))
    }

    /// The state of page `i` (below `len`) and how many pages from `i` have it.
    fn run(&self, i: usize) -> (u32, usize) {
        let state = self.get(i).unwrap_or(0);
        let end = self.runs.range(i + 1..).find(|(_, &s)| s != state).map_or(self.len, |(&k, _)| k);
        (state, end - i)
    }

    /// Whether a page of `[first, last)` has `state`.
    fn contains(&self, first: usize, last: usize, state: u32) -> bool {
        first < last && (self.get(first) == Some(state) || self.runs.range(first + 1..last).any(|(_, &s)| s == state))
    }

    /// Gives pages `[first, last)` `state`, merging equal neighbours.
    fn set(&mut self, first: usize, last: usize, state: u32) {
        if first >= last {
            return;
        }
        let after = if last < self.len { self.get(last) } else { None };
        let inside: Vec<usize> = self.runs.range(first..last).map(|(&k, _)| k).collect();
        for k in inside {
            self.runs.remove(&k);
        }
        self.runs.insert(first, state);
        match after {
            Some(a) if a == state => {
                self.runs.remove(&last);
            }
            Some(a) => {
                self.runs.insert(last, a);
            }
            None => {}
        }
        if first > 0 && self.get(first - 1) == Some(state) {
            self.runs.remove(&first);
        }
    }
}

static REGIONS: Mutex<BTreeMap<u64, Region>> = Mutex::new(BTreeMap::new());

/// A thread stack: committed read/write private memory, as VirtualQuery
/// reports a Windows stack (its pages get frames when first touched),
/// above a guard page of its own. The guard is read-only (OSyx has no
/// no-access pages), which is enough: an overflowing stack writes, and
/// the write faults instead of landing in whatever is mapped below. The
/// guard belongs to the stack's mapping, so nothing else can take it.
/// Returns the base of the usable part (the stack limit).
pub fn alloc_stack(size: usize) -> Option<u64> {
    let size = size.next_multiple_of(PAGE);
    let reserve = alloc_pages(size + PAGE, Prot::ReadWrite)?;
    if protect(reserve, PAGE, Prot::ReadOnly).is_err() {
        free_pages(reserve, size + PAGE);
        return None;
    }
    let mut pages = Pages::new(size / PAGE + 1, PAGE_READWRITE);
    pages.set(0, 1, PAGE_READONLY);
    REGIONS.lock().unwrap().insert(reserve, Region { size: size + PAGE, pages });
    Some(reserve + PAGE as u64)
}

/// Frees a stack made by [`alloc_stack`] (its base and size as given).
pub fn free_stack(base: u64, size: usize) {
    let reserve = base - PAGE as u64;
    crate::mapping::note_freed(reserve, (size.next_multiple_of(PAGE) + PAGE) as u64, "a freed thread stack");
    REGIONS.lock().unwrap().remove(&reserve);
    free_pages(reserve, size.next_multiple_of(PAGE) + PAGE);
}

/// `NtAllocateVirtualMemory` / `VirtualAlloc`. Returns the base.
pub fn virtual_alloc(addr: u64, size: usize, kind: u32, protect: u32) -> Result<u64, NtStatus> {
    // MEM_RESET says the pages' contents are no longer needed; it is what an
    // allocator calls when it returns memory to the system (jemalloc's
    // purge). OSyx hands the frames back and the region stays reserved, so a
    // later read sees zeros, exactly as Windows promises.
    if kind == MEM_RESET {
        if addr == 0 || size == 0 {
            return Err(STATUS_INVALID_PARAMETER);
        }
        let mut regions = REGIONS.lock().unwrap();
        let (&base, r) = regions.range_mut(..=addr).next_back().ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
        let first = (addr - base) as usize / PAGE;
        let last = ((addr - base) as usize + size).div_ceil(PAGE);
        if last > r.pages.len() {
            return Err(STATUS_MEMORY_NOT_ALLOCATED);
        }
        let start = base + (first * PAGE) as u64;
        // SAFETY: the caller declared the contents no longer needed.
        unsafe { os_sys::discard(start as *mut u8, (last - first) * PAGE) }.map_err(errno_to_status)?;
        r.pages.set(first, last, 0);
        return Ok(start);
    }
    if size == 0
        || kind & (MEM_COMMIT | MEM_RESERVE) == 0
        || kind & !(MEM_COMMIT | MEM_RESERVE | 0x20_0000 | 0x10_0000) != 0
    {
        return Err(STATUS_INVALID_PARAMETER); // (TOP_DOWN 0x100000 aside)
    }
    let prot = private_prot(protect)?;
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
        r.pages.set(first, last, protect);
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
    regions.insert(base, Region { size, pages: Pages::new(size / PAGE, state) });
    Ok(base)
}

fn protect_range(addr: u64, size: usize, prot: Prot) -> Result<(), osyx_abi::Errno> {
    protect(addr, size, prot)
}

/// Emulates private RWX pages without granting simultaneous write and
/// execute permission. Other faults retain normal Windows SEH handling.
pub fn jit_fault(n: &osyx_abi::fault::Context) -> bool {
    // Only a resident page's permission fault is a JIT transition.
    // A non-present fault may be real memory exhaustion and must retain
    // the kernel's normal failure/Windows exception path.
    if n.vector != 14 || n.error & 1 == 0 {
        return false;
    }
    let execute = n.error & 16 != 0;
    let write = n.error & 2 != 0;
    if !execute && !write {
        return false;
    }
    let page = n.address & !(PAGE as u64 - 1);
    // Retrying a same-page self-modifying instruction would alternate
    // forever between a write fault and an instruction-fetch fault.
    if write && n.rip & !(PAGE as u64 - 1) == page {
        return false;
    }
    let regions = REGIONS.lock().unwrap();
    let private = regions.range(..=n.address).next_back().is_some_and(|(&base, r)| {
        let index = ((n.address - base) / PAGE as u64) as usize;
        r.pages.get(index) == Some(PAGE_EXECUTE_READWRITE)
    });
    drop(regions);
    // Image pages VirtualProtect made writable and executable switch the
    // same way.
    let image = !private && matches!(IMAGE_PAGES.lock().unwrap().get(&page), Some(&(PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)));
    if !private && !image {
        return false;
    }
    protect(page, PAGE, if execute { Prot::ExecuteRead } else { Prot::ReadWrite }).is_ok()
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
            // The range stays reserved; its frames go back to the kernel
            // and a later commit reads zeros.
            let start = base + (first * PAGE) as u64;
            // SAFETY: decommitted pages may no longer be used by the program.
            unsafe { os_sys::discard(start as *mut u8, (last - first) * PAGE) }.map_err(errno_to_status)?;
            r.pages.set(first, last, 0);
            Ok(())
        }
        _ => Err(STATUS_INVALID_PARAMETER),
    }
}

/// Protections VirtualProtect gave pages of loaded PE images (page ->
/// PAGE_*); the other image pages have their section's protection.
static IMAGE_PAGES: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

/// The protection of a page of a loaded image: what VirtualProtect last
/// set, or what its section's characteristics give.
pub fn image_page_protect(page: u64) -> Option<u32> {
    if let Some(&p) = IMAGE_PAGES.lock().unwrap().get(&page) {
        return Some(p);
    }
    crate::loader::image_section_protect(page)
}

/// Gives pages of a loaded image the protection `prot` and forgets what
/// VirtualProtect set on them, so they read as their section's again: an
/// image loaded again after FreeLibrary starts as a fresh mapping would.
/// Takes no loader lock (the loader calls it).
pub fn reset_image_pages(start: u64, size: usize, prot: Prot) -> Result<(), osyx_abi::Errno> {
    let start = start & !(PAGE as u64 - 1);
    let end = (start + size as u64).next_multiple_of(PAGE as u64);
    protect_range(start, (end - start) as usize, prot)?;
    IMAGE_PAGES.lock().unwrap().retain(|&p, _| p < start || p >= end);
    Ok(())
}

/// VirtualProtect on pages of a loaded image. Writable protections map
/// the pages writable; the kernel copies a shared image page on its first
/// write, so other processes keep the original. PAGE_EXECUTE_READWRITE
/// and PAGE_EXECUTE_WRITECOPY switch between writing and executing on
/// demand, as for private memory (`jit_fault`).
fn protect_image(addr: u64, size: usize, protect: u32) -> Result<u32, NtStatus> {
    let (base, len) = crate::loader::image_containing(addr).ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
    let start = addr & !(PAGE as u64 - 1);
    // A range that wraps the address space is outside every image.
    let end = addr
        .checked_add(size.max(1) as u64)
        .and_then(|e| e.checked_next_multiple_of(PAGE as u64))
        .ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
    if end > base + len {
        return Err(STATUS_MEMORY_NOT_ALLOCATED);
    }
    let prot = match protect & !PAGE_GUARD {
        PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => Prot::ReadWrite,
        p => from_win(p)?,
    };
    let old = image_page_protect(start).unwrap_or(PAGE_READONLY);
    protect_range(start, (end - start) as usize, prot).map_err(errno_to_status)?;
    let mut pages = IMAGE_PAGES.lock().unwrap();
    let mut p = start;
    while p < end {
        pages.insert(p, protect & !PAGE_GUARD);
        p += PAGE as u64;
    }
    Ok(old)
}

/// `NtProtectVirtualMemory` / `VirtualProtect`: returns the old protection
/// of the first page. Pages of VirtualAlloc regions and of loaded images
/// can be changed.
pub fn virtual_protect(addr: u64, size: usize, protect: u32) -> Result<u32, NtStatus> {
    let mut regions = REGIONS.lock().unwrap();
    let inside = regions.range(..=addr).next_back().is_some_and(|(&base, r)| addr < base + r.size as u64);
    if !inside {
        drop(regions);
        // Image pages validate their own protections (write-copy and
        // guard combinations included).
        return protect_image(addr, size, protect);
    }
    let prot = private_prot(protect)?;
    let (&base, r) = regions.range_mut(..=addr).next_back().ok_or(STATUS_MEMORY_NOT_ALLOCATED)?;
    let first = (addr - base) as usize / PAGE;
    let last = ((addr - base) as usize).checked_add(size.max(1)).ok_or(STATUS_MEMORY_NOT_ALLOCATED)?.div_ceil(PAGE);
    if last > r.pages.len() || r.pages.contains(first, last, 0) {
        return Err(STATUS_MEMORY_NOT_ALLOCATED);
    }
    let old = r.pages.get(first).unwrap_or(0);
    let start = base + (first * PAGE) as u64;
    protect_range(start, (last - first) * PAGE, prot).map_err(errno_to_status)?;
    r.pages.set(first, last, protect);
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

/// `virtual_query` for a fault report: None (no wait) when the calling
/// thread is inside the region table already.
pub fn virtual_query_for_report(addr: u64) -> Option<Info> {
    if REGIONS.is_held_by_current_thread() {
        return None;
    }
    virtual_query(addr)
}

/// `NtQueryVirtualMemory` / `VirtualQuery` for regions made by
/// `virtual_alloc` and for loaded images.
pub fn virtual_query(addr: u64) -> Option<Info> {
    let regions = REGIONS.lock().unwrap();
    if let Some((&base, r)) = regions.range(..=addr).next_back() {
        if addr < base + r.size as u64 {
            let i = (addr - base) as usize / PAGE;
            let (state, run) = r.pages.run(i);
            return Some(Info {
                base: base + (i * PAGE) as u64,
                allocation_base: base,
                allocation_protect: r.pages.get(0).unwrap_or(0).max(PAGE_READWRITE),
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
