//! Windows process and thread environment blocks.
//!
//! Windows code reaches its thread's TEB through the GS segment
//! (`gs:[0x30]` is the TEB's own address) and the PEB through the TEB.
//! OSyx keeps FS for its own TLS, so GS is free: each Windows thread gets a
//! TEB and its GS base points at it (`set_gs_base`, saved per thread by
//! the kernel). Layouts follow the documented x64 offsets (winternl.h, the
//! public symbols of ntdll); only fields programs and compilers actually
//! read are filled in:
//!
//! ```text
//! TEB  0x000 NT_TIB { ExceptionList, StackBase 0x08, StackLimit 0x10, ..., Self 0x30 }
//!      0x040 ClientId { UniqueProcess, UniqueThread }   0x058 ThreadLocalStoragePointer
//!      0x060 ProcessEnvironmentBlock   0x068 LastErrorValue
//!      0x1480 TlsSlots[64]             0x1780 TlsExpansionSlots
//! PEB  0x002 BeingDebugged  0x010 ImageBaseAddress  0x018 Ldr  0x020 ProcessParameters
//!      0x030 ProcessHeap    0x0B8 NumberOfProcessors 0x118 OSMajorVersion ...
//! RTL_USER_PROCESS_PARAMETERS 0x038 CurrentDirectory 0x060 ImagePathName
//!      0x070 CommandLine 0x080 Environment
//! LDR_DATA_TABLE_ENTRY 0x00/0x10/0x20 links, 0x30 DllBase, 0x38 EntryPoint,
//!      0x40 SizeOfImage, 0x48 FullDllName, 0x58 BaseDllName
//! ```

use crate::mem;
use core::sync::atomic::{AtomicU64, Ordering};

pub const TEB_SIZE: usize = 0x2000;
pub const TLS_MINIMUM_AVAILABLE: usize = 64;
pub const TLS_EXPANSION_SLOTS: usize = 1024;

pub const TEB_STACK_BASE: usize = 0x08;
pub const TEB_STACK_LIMIT: usize = 0x10;
pub const TEB_SELF: usize = 0x30;
pub const TEB_PROCESS_ID: usize = 0x40;
pub const TEB_THREAD_ID: usize = 0x48;
pub const TEB_TLS_POINTER: usize = 0x58;
pub const TEB_PEB: usize = 0x60;
pub const TEB_LAST_ERROR: usize = 0x68;
pub const TEB_TLS_SLOTS: usize = 0x1480;
pub const TEB_TLS_EXPANSION: usize = 0x1780;

pub const PEB_IMAGE_BASE: usize = 0x10;
pub const PEB_LDR: usize = 0x18;
pub const PEB_PARAMETERS: usize = 0x20;
pub const PEB_PROCESS_HEAP: usize = 0x30;
pub const PEB_NUMBER_OF_PROCESSORS: usize = 0xB8;
pub const PEB_OS_MAJOR: usize = 0x118;
pub const PEB_OS_MINOR: usize = 0x11C;
pub const PEB_OS_BUILD: usize = 0x120;
pub const PEB_OS_PLATFORM: usize = 0x124;

pub const PARAMS_CURDIR: usize = 0x38;
pub const PARAMS_IMAGE_PATH: usize = 0x60;
pub const PARAMS_COMMAND_LINE: usize = 0x70;
pub const PARAMS_ENVIRONMENT: usize = 0x80;

/// The PEB of this process (set once at start).
static PEB: AtomicU64 = AtomicU64::new(0);

pub fn peb() -> u64 {
    PEB.load(Ordering::Relaxed)
}

/// Writes a value at `base + off`.
///
/// # Safety
/// The range must be inside memory owned by the runtime.
pub unsafe fn put<T>(base: u64, off: usize, v: T) {
    // SAFETY: the caller guarantees the range.
    unsafe { core::ptr::write_unaligned((base as usize + off) as *mut T, v) }
}

/// # Safety
/// As [`put`].
pub unsafe fn get<T: Copy>(base: u64, off: usize) -> T {
    // SAFETY: the caller guarantees the range.
    unsafe { core::ptr::read_unaligned((base as usize + off) as *const T) }
}

/// The current thread's TEB (`gs:[0x30]`); 0 outside Windows threads.
#[inline]
pub fn current() -> u64 {
    let t: u64;
    // SAFETY: a Windows thread's GS base points at its TEB; for other
    // threads GS base is 0, and reading gs:[0x30] would fault, so this is
    // only called from code reached through Windows imports.
    unsafe { core::arch::asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly, preserves_flags)) };
    t
}

/// The calling thread's Windows thread id (`ClientId.UniqueThread`).
///
/// Reading the TEB costs a couple of cycles where GETTID is a syscall, and
/// Firefox's locks resolve their owner on every operation. Like
/// [`current`], this is only safe on a thread running Windows code.
#[inline]
pub fn current_tid() -> u64 {
    let t: u64;
    // SAFETY: as [`current`]: gs:[0x30] (the TEB itself) is set.
    unsafe {
        core::arch::asm!("mov {}, gs:[{off}]", out(reg) t, off = const TEB_THREAD_ID,
            options(nostack, readonly, preserves_flags))
    };
    t
}

pub fn last_error() -> u32 {
    // SAFETY: the TEB of the calling Windows thread.
    unsafe { get(current(), TEB_LAST_ERROR) }
}

pub fn set_last_error(e: u32) {
    // SAFETY: as above.
    unsafe { put(current(), TEB_LAST_ERROR, e) }
}

/// A UNICODE_STRING (16 bytes) at `at`, its UTF-16 text (NUL-terminated)
/// allocated from the arena.
///
/// # Safety
/// `at` must have 16 writable bytes.
pub unsafe fn unicode_string(at: u64, s: &str) {
    let units: alloc::vec::Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).collect();
    let buf = mem::arena_alloc(units.len() * 2, 8);
    // SAFETY: `buf` holds `units.len()` u16s; `at` is the caller's.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
        put(at, 0, ((units.len() - 1) * 2) as u16);
        put(at, 2, (units.len() * 2) as u16);
        put(at, 8, buf);
    }
}

pub struct ProcessInfo<'a> {
    pub image_base: u64,
    pub image_path: &'a str,
    pub command_line: &'a str,
    pub current_dir: &'a str,
    /// `NAME=value` pairs.
    pub environment: &'a [alloc::string::String],
    pub heap: u64,
}

/// Builds the PEB, the loader data and the process parameters.
pub fn create_peb(p: &ProcessInfo) -> u64 {
    let peb = mem::arena_alloc(0x400, 16);
    let ldr = mem::arena_alloc(0x58, 16);
    let params = mem::arena_alloc(0x440, 16);
    // SAFETY: all three blocks were just allocated with the sizes used.
    unsafe {
        put(peb, PEB_IMAGE_BASE, p.image_base);
        put(peb, PEB_LDR, ldr);
        put(peb, PEB_PARAMETERS, params);
        put(peb, PEB_PROCESS_HEAP, p.heap);
        put(peb, PEB_NUMBER_OF_PROCESSORS, os_sys::cpu_info().map_or(1, |(count,_)|count));
        // The Windows version OSyx's compatibility layer presents (10.0).
        put(peb, PEB_OS_MAJOR, 10u32);
        put(peb, PEB_OS_MINOR, 0u32);
        put(peb, PEB_OS_BUILD, 19041u16);
        put(peb, PEB_OS_PLATFORM, 2u32); // VER_PLATFORM_WIN32_NT
        put(ldr, 0, 0x58u32);
        put(ldr, 4, 1u8);
        for list in [0x10usize, 0x20, 0x30] {
            put(ldr, list, ldr + list as u64);
            put(ldr, list + 8, ldr + list as u64);
        }
        put(params, 0, 0x440u32); // MaximumLength
        put(params, 4, 0x440u32); // Length
        put(params, 8, 1u32); // Flags: RTL_USER_PROC_PARAMS_NORMALIZED
        unicode_string(params + PARAMS_CURDIR as u64, p.current_dir);
        unicode_string(params + PARAMS_IMAGE_PATH as u64, p.image_path);
        unicode_string(params + PARAMS_COMMAND_LINE as u64, p.command_line);
        // Environment block: NAME=value\0 ... \0 in UTF-16.
        let mut env: alloc::vec::Vec<u16> = alloc::vec::Vec::new();
        for kv in p.environment {
            env.extend(kv.encode_utf16());
            env.push(0);
        }
        env.push(0);
        let block = mem::arena_alloc(env.len() * 2, 8);
        core::ptr::copy_nonoverlapping(env.as_ptr(), block as *mut u16, env.len());
        put(params, PARAMS_ENVIRONMENT, block);
    }
    PEB.store(peb, Ordering::Relaxed);
    peb
}

/// Appends a module to the three loader lists (load, memory and
/// initialisation order are the same here). Returns the entry.
pub fn add_ldr_entry(base: u64, entry: u64, size: u32, full_name: &str, base_name: &str) -> u64 {
    let e = mem::arena_alloc(0x100, 16);
    // SAFETY: the PEB and its LDR data exist (create_peb ran first); `e`
    // is a fresh 0x100-byte block.
    unsafe {
        let ldr: u64 = get(peb(), PEB_LDR);
        for (i, list) in [0x10usize, 0x20, 0x30].into_iter().enumerate() {
            let head = ldr + list as u64;
            let link = e + (i * 0x10) as u64;
            let tail: u64 = get(head, 8);
            put(link, 0, head); // Flink
            put(link, 8, tail); // Blink
            put(tail, 0, link);
            put(head, 8, link);
        }
        put(e, 0x30, base);
        put(e, 0x38, entry);
        put(e, 0x40, size);
        unicode_string(e + 0x48, full_name);
        unicode_string(e + 0x58, base_name);
    }
    e
}

/// Allocates and fills a TEB for a thread whose stack is
/// `[stack_limit, stack_base)`.
pub fn create_teb(stack_base: u64, stack_limit: u64, pid: u64, tid: u64) -> u64 {
    let teb = mem::alloc_pages(TEB_SIZE, mem::Prot::ReadWrite).expect("memory for a TEB");
    // SAFETY: `teb` is TEB_SIZE fresh zeroed bytes.
    unsafe {
        put(teb, 0, u64::MAX); // ExceptionList: end of chain
        put(teb, TEB_STACK_BASE, stack_base);
        put(teb, TEB_STACK_LIMIT, stack_limit);
        put(teb, TEB_SELF, teb);
        put(teb, TEB_PROCESS_ID, pid);
        put(teb, TEB_THREAD_ID, tid);
        put(teb, TEB_PEB, peb());
    }
    teb
}

/// Makes `teb` the calling thread's TEB.
pub fn install(teb: u64) {
    os_sys::set_gs_base(teb as usize).expect("set_gs_base");
}
