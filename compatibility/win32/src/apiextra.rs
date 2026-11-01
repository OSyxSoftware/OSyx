//! Win32 functions the Steam client (Steam.exe, a 32-bit program) imports
//! that winrun lacked: ANSI and LCID forms of existing functions, resource
//! lookup by FindResource, console reads, PSAPI module queries and a few
//! state queries. Most delegate to the Unicode / Ex implementation; the
//! structures they fill have the caller's layout (32-bit for WoW64).
#![allow(non_snake_case)]
use crate::error::*;
use crate::kernel32::{self, fail, Bool, FALSE, TRUE};
use crate::{mem, objects, path, teb, wow64};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use os_std::sync::Mutex;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// An ANSI argument as a NUL-terminated UTF-16 copy (`None` for NULL).
fn ansi_arg(p: *const u8) -> Option<Vec<u16>> {
    // SAFETY: NULL or the caller's NUL-terminated string.
    unsafe { path::ansi_to_string(p) }.map(|s| wide(&s))
}

fn ptr_or_null(v: &Option<Vec<u16>>) -> *const u16 {
    v.as_ref().map_or(core::ptr::null(), |w| w.as_ptr())
}

// ---- console -------------------------------------------------------------------

/// ReadConsoleA: the console input is a byte stream (UTF-8).
extern "win64" fn ReadConsoleA(h: u64, buf: *mut u8, n: u32, read: *mut u32, _control: u64) -> Bool {
    kernel32::ReadFile(h, buf, n, read, core::ptr::null())
}

/// ReadConsoleW: reads up to `n` bytes and widens them (UTF-8).
extern "win64" fn ReadConsoleW(h: u64, buf: *mut u16, n: u32, read: *mut u32, _control: u64) -> Bool {
    if buf.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let mut bytes = alloc::vec![0u8; n as usize];
    let mut got = 0u32;
    if kernel32::ReadFile(h, bytes.as_mut_ptr(), n, &mut got, core::ptr::null()) == FALSE {
        return FALSE;
    }
    let units: Vec<u16> = String::from_utf8_lossy(&bytes[..got as usize]).encode_utf16().take(n as usize).collect();
    // SAFETY: the caller's buffer of `n` characters.
    unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len()) };
    if !read.is_null() {
        // SAFETY: the caller's count.
        unsafe { *read = units.len() as u32 };
    }
    TRUE
}

/// SetConsoleCtrlHandler: accepted; OSyx consoles deliver no Ctrl+C events
/// to Windows programs.
extern "win64" fn SetConsoleCtrlHandler(_handler: u64, _add: Bool) -> Bool {
    TRUE
}

// ---- locale --------------------------------------------------------------------

/// Code pages winrun converts: UTF-8 (the ANSI and OEM code page), UTF-16,
/// US-ASCII and Latin-1.
extern "win64" fn IsValidCodePage(cp: u32) -> Bool {
    matches!(cp, 65001 | 1200 | 1201 | 20127 | 28591 | 1252 | 437 | 850) as Bool
}

/// LCMapStringW and CompareStringW: the LCID forms. OSyx has one locale
/// (en-US), the user and system default.
extern "win64" fn LCMapStringW(_lcid: u32, flags: u32, src: *const u16, n: i32, dst: *mut u16, cap: i32) -> i32 {
    crate::locale::LCMapStringEx(core::ptr::null(), flags, src, n, dst, cap, 0, 0, 0)
}

extern "win64" fn CompareStringW(_lcid: u32, flags: u32, s1: *const u16, n1: i32, s2: *const u16, n2: i32) -> i32 {
    crate::locale::CompareStringEx(core::ptr::null(), flags, s1, n1, s2, n2, 0, 0, 0)
}

extern "win64" fn GetDateFormatW(_lcid: u32, flags: u32, st: *const [u16; 8], fmt: *const u16, buf: *mut u16, size: i32) -> i32 {
    crate::nls::GetDateFormatEx(core::ptr::null(), flags, st, fmt, buf, size, core::ptr::null())
}

extern "win64" fn GetTimeFormatW(_lcid: u32, flags: u32, st: *const [u16; 8], fmt: *const u16, buf: *mut u16, size: i32) -> i32 {
    crate::nls::GetTimeFormatEx(core::ptr::null(), flags, st, fmt, buf, size)
}

// ---- processor and process -----------------------------------------------------------

/// IsProcessorFeaturePresent for the features of every x86-64 CPU OSyx
/// runs on (SSE, SSE2, SSE3, RDTSC, NX, CMPXCHG8B/16B). __fastfail is
/// reported absent, so runtimes take their documented fallback.
extern "win64" fn IsProcessorFeaturePresent(feature: u32) -> Bool {
    // PF_COMPARE_EXCHANGE_DOUBLE 2, PF_MMX 3, PF_XMMI 6, PF_RDTSC 8,
    // PF_XMMI64 10, PF_NX 12, PF_SSE3 13, PF_COMPARE_EXCHANGE128 14,
    // PF_SECOND_LEVEL_ADDRESS_TRANSLATION 20 is not claimed.
    matches!(feature, 2 | 3 | 6 | 8 | 10 | 12 | 13 | 14) as Bool
}

/// UnhandledExceptionFilter: no debugger and no error reporting; the
/// caller's handler runs and the process ends (EXCEPTION_EXECUTE_HANDLER).
extern "win64" fn UnhandledExceptionFilter(_pointers: u64) -> i32 {
    1
}

extern "win64" fn SetProcessAffinityMask(process: u64, mask: u64) -> Bool {
    if mask == 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    (process == objects::CURRENT_PROCESS || process != 0) as Bool
}

/// HeapQueryInformation: HeapCompatibilityInformation (class 0) reports
/// the low-fragmentation heap (2), as Windows does by default.
extern "win64" fn HeapQueryInformation(_heap: u64, class: u32, info: *mut u8, len: usize, ret: *mut usize) -> Bool {
    if class != 0 {
        return fail(ERROR_NOT_SUPPORTED, FALSE);
    }
    if !ret.is_null() {
        // SAFETY: the caller's SIZE_T (4 bytes for a 32-bit caller).
        unsafe {
            if wow64::is_active() {
                *(ret as *mut u32) = 4;
            } else {
                *ret = 4;
            }
        }
    }
    if info.is_null() || len < 4 {
        return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
    }
    // SAFETY: the caller's ULONG.
    unsafe { *(info as *mut u32) = 2 };
    TRUE
}

/// IsBadWritePtr / IsBadReadPtr: NULL and the first 64 KiB (never mapped
/// on Windows or OSyx) are bad; OSyx cannot probe other addresses without
/// faulting, so they are reported good, as most callers expect.
extern "win64" fn IsBadWritePtr(p: u64, len: usize) -> Bool {
    (len != 0 && p < 0x10000) as Bool
}

extern "win64" fn IsBadReadPtr(p: u64, len: usize) -> Bool {
    (len != 0 && p < 0x10000) as Bool
}

/// GetModuleHandleExA: GetModuleHandleExW with the name widened (with
/// GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS the "name" is an address).
extern "win64" fn GetModuleHandleExA(flags: u32, name: *const u8, out: *mut u64) -> Bool {
    const FROM_ADDRESS: u32 = 4;
    if flags & FROM_ADDRESS != 0 || name.is_null() {
        return kernel32::GetModuleHandleExW(flags, name as *const u16, out);
    }
    let w = ansi_arg(name);
    kernel32::GetModuleHandleExW(flags, ptr_or_null(&w), out)
}

/// GetFileTime from the handle's BY_HANDLE_FILE_INFORMATION (creation at
/// 4, last access at 12, last write at 20).
extern "win64" fn GetFileTime(h: u64, created: *mut u64, accessed: *mut u64, written: *mut u64) -> Bool {
    let mut info = [0u8; 52];
    if kernel32::GetFileInformationByHandle(h, info.as_mut_ptr()) == FALSE {
        return FALSE;
    }
    let at = |o: usize| u64::from_le_bytes(info[o..o + 8].try_into().unwrap());
    for (out, o) in [(created, 4), (accessed, 12), (written, 20)] {
        if !out.is_null() {
            // SAFETY: the caller's FILETIME (4-byte aligned on 32-bit callers).
            unsafe { core::ptr::write_unaligned(out, at(o)) };
        }
    }
    TRUE
}

// ---- resources -------------------------------------------------------------------

/// A found resource (HRSRC): its data, copied once into memory that lives
/// as long as the process, as resources in a mapped image do.
struct Found {
    data: u64,
    size: u32,
}

/// (module, type, name) -> HRSRC. Each HRSRC is a 16-byte block holding
/// the data address and size.
static FOUND: Mutex<BTreeMap<(u64, u64, u64), u64>> = Mutex::new(BTreeMap::new());

/// An integer resource id, or a "#123" string naming one.
fn resource_id(name: u64, wide: bool) -> Option<u64> {
    if name < 0x10000 {
        return (name != 0).then_some(name);
    }
    // SAFETY: the caller's NUL-terminated name.
    let s = unsafe { if wide { path::wide_to_string(name as *const u16) } else { path::ansi_to_string(name as *const u8) } }?;
    s.strip_prefix('#')?.parse().ok()
}

fn find_resource(module: u64, name: u64, kind: u64, wide: bool) -> u64 {
    let (Some(name), Some(kind)) = (resource_id(name, wide), resource_id(kind, wide)) else {
        // Resources named by strings are not looked up.
        return fail(ERROR_RESOURCE_NAME_NOT_FOUND_P, 0);
    };
    let key = (module, kind, name);
    if let Some(&h) = FOUND.lock().unwrap().get(&key) {
        return h;
    }
    let data = match crate::loader::numeric_resource(module, kind as u16, name) {
        Ok(d) => d,
        Err(e) => return fail(e, 0),
    };
    let block = mem::arena_alloc(16 + data.len().max(1), 16);
    // SAFETY: a fresh arena block of 16 + len bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), (block + 16) as *mut u8, data.len());
        let f = Found { data: block + 16, size: data.len() as u32 };
        teb::put(block, 0, f.data);
        teb::put(block, 8, f.size);
    }
    FOUND.lock().unwrap().insert(key, block);
    block
}

const ERROR_RESOURCE_NAME_NOT_FOUND_P: u32 = 1814;

extern "win64" fn FindResourceA(module: u64, name: u64, kind: u64) -> u64 {
    find_resource(module, name, kind, false)
}

extern "win64" fn FindResourceW(module: u64, name: u64, kind: u64) -> u64 {
    find_resource(module, name, kind, true)
}

extern "win64" fn FindResourceExW(module: u64, kind: u64, name: u64, _language: u16) -> u64 {
    find_resource(module, name, kind, true)
}

fn found(h: u64) -> Option<Found> {
    if !FOUND.lock().unwrap().values().any(|&v| v == h) {
        return None;
    }
    // SAFETY: an HRSRC block made by find_resource.
    unsafe { Some(Found { data: teb::get(h, 0), size: teb::get(h, 8) }) }
}

extern "win64" fn LoadResource(_module: u64, h: u64) -> u64 {
    found(h).map_or_else(|| fail(ERROR_INVALID_HANDLE, 0), |f| f.data)
}

extern "win64" fn LockResource(data: u64) -> u64 {
    data
}

extern "win64" fn SizeofResource(_module: u64, h: u64) -> u32 {
    found(h).map_or_else(|| fail(ERROR_INVALID_HANDLE, 0), |f| f.size)
}

// ---- advapi32: ANSI registry functions ------------------------------------------------

const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_MULTI_SZ: u32 = 7;
const KEY_ALL_ACCESS: u32 = 0xF003F;

extern "win64" fn RegOpenKeyExA(hkey: u64, sub: *const u8, options: u32, sam: u32, out: *mut u64) -> u32 {
    let w = ansi_arg(sub);
    crate::registry::RegOpenKeyExW(hkey, ptr_or_null(&w), options, sam, out)
}

extern "win64" fn RegOpenKeyA(hkey: u64, sub: *const u8, out: *mut u64) -> u32 {
    RegOpenKeyExA(hkey, sub, 0, KEY_ALL_ACCESS, out)
}

#[allow(clippy::too_many_arguments)] // the Win32 signature
extern "win64" fn RegCreateKeyExA(
    hkey: u64, sub: *const u8, reserved: u32, class: *const u8, options: u32, sam: u32, attributes: u64, out: *mut u64,
    disposition: *mut u32,
) -> u32 {
    let (w, c) = (ansi_arg(sub), ansi_arg(class));
    crate::registry::RegCreateKeyExW(hkey, ptr_or_null(&w), reserved, ptr_or_null(&c), options, sam, attributes, out, disposition)
}

/// RegSetValueExA: string data (REG_SZ, REG_EXPAND_SZ, REG_MULTI_SZ) is
/// stored as UTF-16, as Windows does.
extern "win64" fn RegSetValueExA(hkey: u64, name: *const u8, reserved: u32, kind: u32, data: *const u8, size: u32) -> u32 {
    let n = ansi_arg(name);
    if matches!(kind, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ) && !data.is_null() {
        // SAFETY: the caller's `size` bytes.
        let bytes = unsafe { core::slice::from_raw_parts(data, size as usize) };
        let units: Vec<u16> = String::from_utf8_lossy(bytes).encode_utf16().collect();
        return crate::registry::RegSetValueExW(hkey, ptr_or_null(&n), reserved, kind, units.as_ptr() as *const u8, units.len() as u32 * 2);
    }
    crate::registry::RegSetValueExW(hkey, ptr_or_null(&n), reserved, kind, data, size)
}

/// RegQueryValueExA: string data comes back as ANSI (UTF-8); sizes are in
/// bytes of the converted data.
extern "win64" fn RegQueryValueExA(hkey: u64, name: *const u8, reserved: *const u32, kind: *mut u32, buf: *mut u8, size: *mut u32) -> u32 {
    let n = ansi_arg(name);
    let mut t = 0u32;
    let mut need = 0u32;
    let r = crate::registry::RegQueryValueExW(hkey, ptr_or_null(&n), reserved, &mut t, core::ptr::null_mut(), &mut need);
    if r != 0 {
        return r;
    }
    if !kind.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *kind = t };
    }
    let mut data = alloc::vec![0u8; need as usize];
    let mut got = need;
    let r = crate::registry::RegQueryValueExW(hkey, ptr_or_null(&n), reserved, core::ptr::null_mut(), data.as_mut_ptr(), &mut got);
    if r != 0 {
        return r;
    }
    data.truncate(got as usize);
    if matches!(t, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ) {
        let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        data = String::from_utf16_lossy(&units).into_bytes();
    }
    if size.is_null() {
        return if buf.is_null() { 0 } else { ERROR_INVALID_PARAMETER };
    }
    // SAFETY: the caller's size and buffer of that many bytes.
    unsafe {
        let cap = *size as usize;
        *size = data.len() as u32;
        if buf.is_null() {
            return 0;
        }
        if cap < data.len() {
            return ERROR_MORE_DATA_P;
        }
        core::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len());
    }
    0
}

const ERROR_MORE_DATA_P: u32 = 234;

// ---- psapi -------------------------------------------------------------------------

extern "win64" fn GetModuleFileNameExW(process: u64, module: u64, buf: *mut u16, size: u32) -> u32 {
    if process != objects::CURRENT_PROCESS {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    kernel32::GetModuleFileNameW(module, buf, size)
}

/// GetModuleInformation: MODULEINFO { base, SizeOfImage, entry point } for
/// a module of this process (pointer-sized fields: 12 bytes for 32-bit
/// callers, 24 for 64-bit ones).
extern "win64" fn GetModuleInformation(process: u64, module: u64, out: *mut u8, cb: u32) -> Bool {
    if process != objects::CURRENT_PROCESS {
        return fail(ERROR_ACCESS_DENIED, FALSE);
    }
    let narrow = wow64::is_active();
    if out.is_null() || cb < if narrow { 12 } else { 24 } {
        return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
    }
    let base = if module == 0 { crate::loader::get_module_handle(None).unwrap_or(0) } else { module };
    let Some((start, size)) = crate::loader::image_containing(base) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    // SAFETY: a mapped PE image: e_lfanew at 0x3C, AddressOfEntryPoint at
    // NT header + 0x28.
    let entry = unsafe {
        let nt = start + teb::get::<u32>(start, 0x3C) as u64;
        match teb::get::<u32>(nt, 0x28) {
            0 => 0,
            rva => start + rva as u64,
        }
    };
    // SAFETY: the caller's MODULEINFO.
    unsafe {
        let p = out as u64;
        if narrow {
            teb::put(p, 0, start as u32);
            teb::put(p, 4, size as u32);
            teb::put(p, 8, entry as u32);
        } else {
            teb::put(p, 0, start);
            teb::put(p, 8, size as u32);
            teb::put(p, 16, entry);
        }
    }
    TRUE
}

/// GetProcessMemoryInfo: PROCESS_MEMORY_COUNTERS(_EX). 32-bit callers get
/// the layout with 4-byte SIZE_T fields (40 or 44 bytes).
extern "win64" fn GetProcessMemoryInfo(process: u64, out: *mut u8, cb: u32) -> Bool {
    if !wow64::is_active() {
        return crate::procinfo::K32GetProcessMemoryInfo(process, out, cb);
    }
    if out.is_null() || cb < 40 {
        return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
    }
    let mut wide = [0u8; 80];
    if crate::procinfo::K32GetProcessMemoryInfo(process, wide.as_mut_ptr(), 80) == FALSE {
        return FALSE;
    }
    let field = |i: usize| u64::from_le_bytes(wide[8 + 8 * i..16 + 8 * i].try_into().unwrap()).min(u32::MAX as u64) as u32;
    let size = if cb >= 44 { 44u32 } else { 40 };
    // SAFETY: the caller's structure of `cb` bytes.
    unsafe {
        let p = out as u64;
        core::ptr::write_bytes(out, 0, size as usize);
        teb::put(p, 0, size);
        teb::put(p, 4, u32::from_le_bytes(wide[4..8].try_into().unwrap())); // PageFaultCount
        for i in 0..(size as usize - 8) / 4 {
            teb::put(p, 8 + 4 * i, field(i));
        }
    }
    TRUE
}

// ---- gdi32 / comctl32 / ws2_32 ---------------------------------------------------------

/// AddFontMemResourceEx: accepted (text is drawn with the system fonts);
/// one font reported installed.
extern "win64" fn AddFontMemResourceEx(data: *const u8, len: u32, _reserved: u64, count: *mut u32) -> u64 {
    if data.is_null() || len == 0 || count.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's DWORD.
    unsafe { *count = 1 };
    static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0x00F0_0010);
    NEXT.fetch_add(0x10, core::sync::atomic::Ordering::Relaxed)
}

extern "win64" fn RemoveFontMemResourceEx(h: u64) -> Bool {
    (h != 0) as Bool
}

/// InitCommonControlsEx: registers the common control classes (all of
/// them, whichever ICC_* flags are asked for), as InitCommonControls does.
extern "win64" fn InitCommonControlsEx(init: *const u32) -> Bool {
    if init.is_null() {
        return FALSE;
    }
    crate::user32::register_common_controls();
    TRUE
}

/// WSASocketA: WSASocketW; a protocol-info structure is not supported.
extern "win64" fn WSASocketA(af: i32, kind: i32, proto: i32, info: *const u8, group: u32, flags: u32) -> u64 {
    if !info.is_null() {
        return fail(10045, u64::MAX); // WSAEOPNOTSUPP, INVALID_SOCKET
    }
    crate::ws2_32::WSASocketW(af, kind, proto, core::ptr::null(), group, flags)
}

/// GetAcceptExSockaddrs (wsock32 #1142): the local and remote addresses
/// AcceptEx stores after the received data, each in a slot of
/// `local_len` / `remote_len` bytes. (AcceptEx itself is not offered,
/// so this only parses what a caller laid out.)
#[allow(clippy::too_many_arguments)] // the Win32 signature
extern "win64" fn GetAcceptExSockaddrs(
    buf: u64, recv_len: u32, local_len: u32, remote_len: u32, local: *mut u64, local_size: *mut i32, remote: *mut u64,
    remote_size: *mut i32,
) {
    let l = buf + recv_len as u64;
    let r = l + local_len as u64;
    // SAFETY: the caller's out pointers (OutWide on 32-bit callers).
    unsafe {
        if !local.is_null() {
            *local = l;
        }
        if !local_size.is_null() {
            *local_size = local_len.saturating_sub(16) as i32;
        }
        if !remote.is_null() {
            *remote = r;
        }
        if !remote_size.is_null() {
            *remote_size = remote_len.saturating_sub(16) as i32;
        }
    }
}

const WSAEOPNOTSUPP_P: u32 = 10045;

/// AcceptEx, ConnectEx and DisconnectEx (the Winsock extension functions
/// SIO_GET_EXTENSION_FUNCTION_POINTER hands out). They are overlapped
/// operations, which winrun's sockets do not offer: they fail with
/// WSAEOPNOTSUPP, so callers fall back to accept/connect.
#[allow(clippy::too_many_arguments)] // the Win32 signature
extern "win64" fn AcceptEx(_l: u64, _a: u64, _buf: u64, _recv: u32, _local: u32, _remote: u32, _got: *mut u32, _ov: u64) -> Bool {
    crate::teb::set_last_error(WSAEOPNOTSUPP_P);
    FALSE
}

extern "win64" fn ConnectEx(_s: u64, _name: u64, _len: i32, _buf: u64, _n: u32, _sent: *mut u32, _ov: u64) -> Bool {
    crate::teb::set_last_error(WSAEOPNOTSUPP_P);
    FALSE
}

extern "win64" fn DisconnectEx(_s: u64, _ov: u64, _flags: u32, _reserved: u32) -> Bool {
    crate::teb::set_last_error(WSAEOPNOTSUPP_P);
    FALSE
}

/// The extension function a SIO_GET_EXTENSION_FUNCTION_POINTER GUID
/// names: (name, Win64 address).
pub(crate) fn extension_function(guid: &[u8; 16]) -> Option<(&'static str, u64)> {
    let d1 = u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]]);
    let f = |p: *const ()| p as u64;
    match d1 {
        0xb536_7df1 => Some(("AcceptEx", f(AcceptEx as *const ()))),
        0xb536_7df2 => Some(("GetAcceptExSockaddrs", f(GetAcceptExSockaddrs as *const ()))),
        0x25a2_07b9 => Some(("ConnectEx", f(ConnectEx as *const ()))),
        0x7fda_2e11 => Some(("DisconnectEx", f(DisconnectEx as *const ()))),
        _ => None,
    }
}

pub fn wsock32_extra() -> (Vec<(&'static str, u64)>, Vec<(u16, u64)>) {
    let a = GetAcceptExSockaddrs as *const () as u64;
    (alloc::vec![("GetAcceptExSockaddrs", a)], alloc::vec![(1142, a)])
}

pub fn kernel32_exports() -> Vec<(&'static str, u64)> {
    export_table![
        ReadConsoleA,
        ReadConsoleW,
        SetConsoleCtrlHandler,
        IsValidCodePage,
        LCMapStringW,
        CompareStringW,
        GetDateFormatW,
        GetTimeFormatW,
        IsProcessorFeaturePresent,
        UnhandledExceptionFilter,
        SetProcessAffinityMask,
        HeapQueryInformation,
        IsBadWritePtr,
        IsBadReadPtr,
        GetModuleHandleExA,
        GetFileTime,
        FindResourceA,
        FindResourceW,
        FindResourceExW,
        LoadResource,
        LockResource,
        SizeofResource,
        GetModuleFileNameExW,
        GetModuleInformation,
        GetProcessMemoryInfo,
    ]
}

pub fn psapi_exports() -> Vec<(&'static str, u64)> {
    export_table![GetModuleFileNameExW, GetModuleInformation, GetProcessMemoryInfo]
}

pub fn advapi32_exports() -> Vec<(&'static str, u64)> {
    export_table![RegOpenKeyA, RegOpenKeyExA, RegCreateKeyExA, RegSetValueExA, RegQueryValueExA]
}

pub fn gdi32_exports() -> Vec<(&'static str, u64)> {
    export_table![AddFontMemResourceEx, RemoveFontMemResourceEx]
}

pub fn comctl32_exports() -> Vec<(&'static str, u64)> {
    export_table![InitCommonControlsEx]
}

pub fn ws2_32_exports() -> Vec<(&'static str, u64)> {
    export_table![WSASocketA]
}
