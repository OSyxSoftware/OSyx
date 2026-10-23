//! dbghelp: symbol handling for this process from what its modules carry.
//!
//! OSyx has no PDB reader, so symbols are the modules' exports, the way
//! dbghelp reports them for a module without debug information
//! (SymExport): an address resolves to the nearest export at or below it
//! in the module containing it, with the displacement. Built-in modules'
//! exports are winrun functions and resolve only at their exact address.
//! There is no source line information. Only the calling process can be
//! inspected (OSyx gives no access to other processes' memory).
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    loader::{self, ModuleInfo},
    objects::{self, Object},
    path,
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use os_std::sync::Mutex;

const ERROR_INVALID_ADDRESS: u32 = 487;
const SYMOPT_UNDNAME: u32 = 0x2;
const SYMOPT_DEFERRED_LOADS: u32 = 0x4;
const SYM_EXPORT: u32 = 4;
const SYMFLAG_EXPORT: u32 = 0x200;
const SYM_TAG_PUBLIC_SYMBOL: u32 = 10;
const IMAGE_FILE_MACHINE_AMD64: u32 = 0x8664;

static OPTIONS: Mutex<u32> = Mutex::new(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
/// Initialised sessions by process handle: the modules loaded into each
/// (name, base).
static SESSIONS: Mutex<BTreeMap<u64, Vec<(String, u64)>>> = Mutex::new(BTreeMap::new());

/// Whether `h` is the calling process.
fn this_process(h: u64) -> bool {
    h == objects::CURRENT_PROCESS || matches!(objects::get(h), Some(Object::Process(p)) if p.pid == kernel32::pid())
}

extern "win64" fn SymInitializeW(h: u64, _search: *const u16, invade: i32) -> i32 {
    if !this_process(h) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    let mut s = SESSIONS.lock().unwrap();
    if s.contains_key(&h) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let modules = if invade != 0 { loader::modules().into_iter().map(|m| (m.name, m.base)).collect() } else { Vec::new() };
    s.insert(h, modules);
    1
}

extern "win64" fn SymInitialize(h: u64, _search: *const u8, invade: i32) -> i32 {
    SymInitializeW(h, core::ptr::null(), invade)
}

extern "win64" fn SymCleanup(h: u64) -> i32 {
    match SESSIONS.lock().unwrap().remove(&h) {
        Some(_) => 1,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn SymSetOptions(options: u32) -> u32 {
    *OPTIONS.lock().unwrap() = options;
    options
}

extern "win64" fn SymGetOptions() -> u32 {
    *OPTIONS.lock().unwrap()
}

/// The loaded module containing `addr`, among a session's modules.
fn module_at(h: u64, addr: u64) -> Result<ModuleInfo, u32> {
    let s = SESSIONS.lock().unwrap();
    let Some(list) = s.get(&h) else { return Err(ERROR_INVALID_HANDLE) };
    let list = list.clone();
    drop(s);
    loader::modules()
        .into_iter()
        .find(|m| list.iter().any(|(n, b)| *n == m.name && *b == m.base) && (addr >= m.base && addr < m.base + m.size || m.symbols.iter().any(|s| s.0 == addr)))
        .ok_or(ERROR_MOD_NOT_FOUND)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn SymLoadModule64(h: u64, _file: u64, image: *const u8, module: *const u8, base: u64, _size: u32) -> u64 {
    // SAFETY: NULL or NUL-terminated names.
    let (image, module) = unsafe { (path::ansi_to_string(image), path::ansi_to_string(module)) };
    let wanted = module.or(image).map(|n| String::from(n.rsplit(['\\', '/']).next().unwrap_or(&n)).to_ascii_lowercase());
    let found = loader::modules().into_iter().find(|m| {
        (base != 0 && m.base == base) || (base == 0 && wanted.as_deref().is_some_and(|w| m.name == w || m.name.strip_suffix(".dll") == Some(w)))
    });
    let Some(m) = found else { return fail(ERROR_MOD_NOT_FOUND, 0) };
    let mut s = SESSIONS.lock().unwrap();
    let Some(list) = s.get_mut(&h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if list.iter().any(|(n, b)| *n == m.name && *b == m.base) {
        return fail(ERROR_SUCCESS, 0); // already loaded
    }
    list.push((m.name, m.base));
    m.base
}

/// Copies `s` into a CHAR array of `cap` bytes at `p`, NUL-terminated.
unsafe fn put_ansi(p: *mut u8, cap: usize, s: &str) {
    let n = s.len().min(cap.saturating_sub(1));
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p, n);
        *p.add(n) = 0;
    }
}

extern "win64" fn SymGetModuleInfo64(h: u64, addr: u64, info: *mut u8) -> i32 {
    if info.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's IMAGEHLP_MODULE64, SizeOfStruct bytes.
    let size = unsafe { *(info as *const u32) } as usize;
    if size < 584 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let m = match module_at(h, addr) {
        Ok(m) => m,
        Err(e) => return fail(e, 0),
    };
    let module = m.name.strip_suffix(".dll").or(m.name.strip_suffix(".exe")).unwrap_or(&m.name);
    // SAFETY: as above; every field written lies below `size`.
    unsafe {
        core::ptr::write_bytes(info.add(4), 0, size - 4);
        let put32 = |at: usize, v: u32| core::ptr::write_unaligned(info.add(at) as *mut u32, v);
        core::ptr::write_unaligned(info.add(8) as *mut u64, m.base);
        put32(16, m.size as u32);
        put32(20, m.time_date_stamp);
        put32(24, m.checksum);
        put32(28, m.symbols.len() as u32);
        put32(32, SYM_EXPORT);
        put_ansi(info.add(36), 32, module);
        put_ansi(info.add(68), 256, &m.path);
        put_ansi(info.add(324), 256, &m.path);
        if size >= 1676 {
            put32(1672, IMAGE_FILE_MACHINE_AMD64);
        }
    }
    1
}

extern "win64" fn SymFromAddr(h: u64, addr: u64, displacement: *mut u64, info: *mut u8) -> i32 {
    if info.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let m = match module_at(h, addr) {
        Ok(m) => m,
        Err(e) => return fail(e, 0),
    };
    let inside = addr >= m.base && addr < m.base + m.size;
    let found = if inside {
        m.symbols.iter().rev().find(|s| s.0 <= addr && s.0 >= m.base)
    } else {
        m.symbols.iter().find(|s| s.0 == addr)
    };
    let Some((at, name)) = found else { return fail(ERROR_INVALID_ADDRESS, 0) };
    // SAFETY: the caller's SYMBOL_INFO with MaxNameLen bytes of Name.
    unsafe {
        let max = *(info.add(80) as *const u32) as usize;
        let put32 = |off: usize, v: u32| core::ptr::write_unaligned(info.add(off) as *mut u32, v);
        put32(4, 0);
        core::ptr::write_bytes(info.add(8), 0, 16);
        put32(24, 0);
        // Size: up to the next export, if known.
        let next = m.symbols.iter().find(|s| s.0 > *at).map_or(0, |s| (s.0 - at).min(u32::MAX as u64) as u32);
        put32(28, if inside { next } else { 0 });
        core::ptr::write_unaligned(info.add(32) as *mut u64, m.base);
        put32(40, SYMFLAG_EXPORT);
        core::ptr::write_unaligned(info.add(48) as *mut u64, 0);
        core::ptr::write_unaligned(info.add(56) as *mut u64, *at);
        put32(64, 0);
        put32(68, 0);
        put32(72, SYM_TAG_PUBLIC_SYMBOL);
        put32(76, name.len() as u32);
        if max > 0 {
            put_ansi(info.add(84), max, name);
        }
        if !displacement.is_null() {
            *displacement = addr - at;
        }
    }
    1
}

extern "win64" fn SymGetLineFromAddr64(h: u64, addr: u64, _displacement: *mut u32, line: *mut u8) -> i32 {
    if line.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if let Err(e) = module_at(h, addr) {
        return fail(e, 0);
    }
    fail(ERROR_INVALID_ADDRESS, 0) // no line information without PDBs
}

type EnumCallback = extern "win64" fn(*const u8, u64, u32, u64) -> i32;

extern "win64" fn EnumerateLoadedModules64(h: u64, callback: u64, context: u64) -> i32 {
    if !this_process(h) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    if callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's PENUMLOADED_MODULES_CALLBACK64.
    let f: EnumCallback = unsafe { core::mem::transmute(callback as usize) };
    for m in loader::modules() {
        let mut name = m.path.into_bytes();
        name.push(0);
        if f(name.as_ptr(), m.base, m.size as u32, context) == 0 {
            break;
        }
    }
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        SymInitializeW,
        SymInitialize,
        SymCleanup,
        SymSetOptions,
        SymGetOptions,
        SymLoadModule64,
        SymGetModuleInfo64,
        SymFromAddr,
        SymGetLineFromAddr64,
        EnumerateLoadedModules64,
    ]
}
