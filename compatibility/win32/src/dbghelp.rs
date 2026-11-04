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
/// The symbol search path of each session. With no PDB reader nothing is
/// searched for; the path is kept so it reads back.
static SEARCH_PATHS: Mutex<BTreeMap<u64, Vec<u16>>> = Mutex::new(BTreeMap::new());

/// Whether `h` is the calling process.
fn this_process(h: u64) -> bool {
    h == objects::CURRENT_PROCESS || matches!(objects::get(h), Some(Object::Process(p)) if p.pid == kernel32::pid())
}

/// A session's search path: the one given, else the current directory
/// and the _NT_SYMBOL_PATH and _NT_ALT_SYMBOL_PATH variables, as dbghelp
/// builds it.
fn initial_search(search: Option<String>) -> Vec<u16> {
    let path = search.unwrap_or_else(|| {
        let mut p = String::from(".");
        for v in ["_NT_SYMBOL_PATH", "_NT_ALT_SYMBOL_PATH"] {
            if let Ok(x) = os_std::env::var(v) {
                p.push(';');
                p.push_str(&x);
            }
        }
        p
    });
    path.encode_utf16().collect()
}

fn initialize(h: u64, search: Option<String>, invade: i32) -> i32 {
    if !this_process(h) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    let mut s = SESSIONS.lock().unwrap();
    if s.contains_key(&h) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let modules = if invade != 0 { loader::modules().into_iter().map(|m| (m.name, m.base)).collect() } else { Vec::new() };
    s.insert(h, modules);
    SEARCH_PATHS.lock().unwrap().insert(h, initial_search(search));
    1
}

extern "win64" fn SymInitializeW(h: u64, search: *const u16, invade: i32) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated path.
    initialize(h, unsafe { path::wide_to_string(search) }, invade)
}

extern "win64" fn SymInitialize(h: u64, search: *const u8, invade: i32) -> i32 {
    // SAFETY: as above.
    initialize(h, unsafe { path::ansi_to_string(search) }, invade)
}

extern "win64" fn SymCleanup(h: u64) -> i32 {
    SEARCH_PATHS.lock().unwrap().remove(&h);
    match SESSIONS.lock().unwrap().remove(&h) {
        Some(_) => 1,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn SymSetSearchPathW(h: u64, search: *const u16) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated path.
    let path = unsafe { path::wide_to_string(search) };
    match SEARCH_PATHS.lock().unwrap().get_mut(&h) {
        Some(p) => {
            *p = initial_search(path);
            1
        }
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn SymGetSearchPathW(h: u64, out: *mut u16, len: u32) -> i32 {
    let paths = SEARCH_PATHS.lock().unwrap();
    let Some(p) = paths.get(&h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if out.is_null() || p.len() >= len as usize {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `len` units.
    unsafe {
        core::ptr::copy_nonoverlapping(p.as_ptr(), out, p.len());
        *out.add(p.len()) = 0;
    }
    1
}

/// The RUNTIME_FUNCTION of the code at `addr` in this process.
extern "win64" fn SymFunctionTableAccess64(h: u64, addr: u64) -> u64 {
    if !this_process(h) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    crate::unwind::lookup(addr).map_or_else(|| fail(ERROR_MOD_NOT_FOUND, 0), |(e, _)| e)
}

/// The base of the module containing `addr`: one of the session's, else
/// any image loaded in this process.
extern "win64" fn SymGetModuleBase64(h: u64, addr: u64) -> u64 {
    if !this_process(h) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    match module_at(h, addr) {
        Ok(m) => m.base,
        Err(_) => loader::image_containing(addr).map_or_else(|| fail(ERROR_MOD_NOT_FOUND, 0), |(b, _)| b),
    }
}

// CONTEXT (x64) fields StackWalk64 reads and moves.
const CTX_RSP: usize = 0x98;
const CTX_RIP: usize = 0xF8;
const CONTEXT_SIZE: usize = 0x4D0;

/// Whether 8 bytes at `addr` can be read.
fn readable(addr: u64) -> bool {
    addr != 0 && crate::mem::virtual_query(addr).is_some_and(|i| i.state == crate::mem::MEM_COMMIT && i.protect & 0x101 == 0 && addr + 8 <= i.base + i.size)
}

/// Unwinds `ctx` one frame: through the function's unwind data, or, for
/// a leaf function without any, by popping the return address.
fn unwind_frame(ctx: u64) -> bool {
    // SAFETY: the caller's CONTEXT.
    let (rip, rsp): (u64, u64) = unsafe { (crate::teb::get(ctx, CTX_RIP), crate::teb::get(ctx, CTX_RSP)) };
    if rip == 0 || !readable(rsp) {
        return false;
    }
    match crate::unwind::lookup(rip) {
        Some((entry, base)) => {
            let (mut data, mut frame) = (0u64, 0u64);
            crate::unwind::virtual_unwind(0, base, rip, entry, ctx, &mut data, &mut frame, 0);
        }
        // SAFETY: a readable stack slot; the caller's CONTEXT.
        None => unsafe {
            crate::teb::put(ctx, CTX_RIP, crate::teb::get::<u64>(rsp, 0));
            crate::teb::put(ctx, CTX_RSP, rsp + 8);
        },
    }
    true
}

/// STACKFRAME64: AddrPC, AddrReturn, AddrFrame, AddrStack (ADDRESS64:
/// offset, segment, mode), AddrBStore, FuncTableEntry, Params[4], Far,
/// Virtual, Reserved[3], KdHelp.
const SF_PC: usize = 0;
const SF_RETURN: usize = 16;
const SF_FRAME: usize = 32;
const SF_STACK: usize = 48;
const SF_FUNCTION: usize = 80;
const SF_PARAMS: usize = 88;
const SF_VIRTUAL: usize = 124;
const SF_RESERVED: usize = 128;
const ADDR_MODE_FLAT: u32 = 3;

/// StackWalk64 for x64 code of this process, from the CONTEXT given (which
/// it moves up a frame per call, as dbghelp does). The first call reports
/// the frame the context is in; each later one the caller's. Memory is
/// read directly (only this process can be inspected), so the read,
/// function-table, module-base and address-translation routines are not
/// needed.
#[allow(clippy::too_many_arguments)]
extern "win64" fn StackWalk64(machine: u32, process: u64, _thread: u64, frame: u64, ctx: u64, _read: u64, _table: u64, _base: u64, _translate: u64) -> i32 {
    if machine != IMAGE_FILE_MACHINE_AMD64 || frame == 0 || ctx == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !this_process(process) {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    use crate::teb::{get, put};
    // SAFETY: the caller's STACKFRAME64 and CONTEXT.
    unsafe {
        // Reserved[0] marks a walk under way.
        let started: u64 = get(frame, SF_RESERVED);
        if started == 0 {
            let pc: u64 = get(frame, SF_PC);
            if pc != 0 {
                put(ctx, CTX_RIP, pc);
            }
        } else if !unwind_frame(ctx) {
            return 0;
        }
        let (rip, rsp): (u64, u64) = (get(ctx, CTX_RIP), get(ctx, CTX_RSP));
        if rip == 0 {
            return 0;
        }
        // The return address: where the frame above resumes.
        // CONTEXT is 16-byte aligned (its XMM registers).
        #[repr(C, align(16))]
        struct Context([u8; CONTEXT_SIZE]);
        let mut above = Context([0u8; CONTEXT_SIZE]);
        core::ptr::copy_nonoverlapping(ctx as *const u8, above.0.as_mut_ptr(), CONTEXT_SIZE);
        let ret = if unwind_frame(above.0.as_mut_ptr() as u64) { get::<u64>(above.0.as_ptr() as u64, CTX_RIP) } else { 0 };
        for (at, v) in [(SF_PC, rip), (SF_RETURN, ret), (SF_FRAME, if started == 0 && get::<u64>(frame, SF_FRAME) != 0 { get(frame, SF_FRAME) } else { rsp }), (SF_STACK, rsp)] {
            put(frame, at, v);
            put(frame, at + 8, 0u16);
            put(frame, at + 12, ADDR_MODE_FLAT);
        }
        put(frame, SF_FUNCTION, crate::unwind::lookup(rip).map_or(0, |(e, _)| e));
        // The four argument home slots of the frame's caller.
        let caller_rsp: u64 = get(above.0.as_ptr() as u64, CTX_RSP);
        for i in 0..4u64 {
            let slot = caller_rsp + 8 * i;
            put(frame, SF_PARAMS + 8 * i as usize, if readable(slot) { get::<u64>(slot, 0) } else { 0 });
        }
        put(frame, SF_VIRTUAL, 1u32);
        put(frame, SF_RESERVED, 1u64);
    }
    1
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
        SymSetSearchPathW,
        SymGetSearchPathW,
        SymFunctionTableAccess64,
        SymGetModuleBase64,
        StackWalk64,
    ]
}
