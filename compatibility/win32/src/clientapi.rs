//! Win32 functions the updated Steam client imports that winrun lacked
//! (steam.exe, SteamUI.dll, steamclient.dll, tier0_s/vstdlib_s, video.dll
//! and the FFmpeg and SDL3 DLLs, all 32-bit): mostly the ANSI forms of
//! existing Unicode functions, which convert their strings (ANSI is UTF-8
//! here, see GetACP) and delegate, plus module, file-type and debugging
//! queries.
#![allow(non_snake_case)]
use crate::error::*;
use crate::kernel32::{self, fail, Bool, FALSE, TRUE};
use crate::{objects, path};
use alloc::{string::String, vec::Vec};

/// An ANSI argument as a NUL-terminated UTF-16 copy (`None` for NULL).
fn wide_arg(p: *const u8) -> Option<Vec<u16>> {
    // SAFETY: NULL or the caller's NUL-terminated string.
    unsafe { path::ansi_to_wide(p) }
}

fn ptr(v: &Option<Vec<u16>>) -> *const u16 {
    v.as_ref().map_or(core::ptr::null(), |w| w.as_ptr())
}

/// The UTF-16 text in `w` up to its NUL.
fn until_nul(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

/// Copies `s` and a NUL into an ANSI buffer of `cap` bytes when it fits.
/// Returns false (nothing written) when it does not.
fn put_ansi(s: &str, buf: *mut u8, cap: usize) -> bool {
    if buf.is_null() || s.len() + 1 > cap {
        return false;
    }
    // SAFETY: the caller's buffer of `cap` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    true
}

/// A length-counted ANSI string (`n` < 0: NUL-terminated) as UTF-16
/// without a terminator.
fn counted_ansi(p: *const u8, n: i32) -> Option<Vec<u16>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller's `n` bytes or NUL-terminated string.
    let bytes = unsafe {
        let len = if n < 0 { (0..).take_while(|&i| *p.add(i) != 0).count() } else { n as usize };
        core::slice::from_raw_parts(p, len)
    };
    let s = match core::str::from_utf8(bytes) {
        Ok(s) => String::from(s),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    };
    Some(s.encode_utf16().collect())
}

// ---- strings ---------------------------------------------------------------------

/// CompareStringA: the strings compared as their Unicode forms.
extern "win64" fn CompareStringA(lcid: u32, flags: u32, s1: *const u8, n1: i32, s2: *const u8, n2: i32) -> i32 {
    let (Some(a), Some(b)) = (counted_ansi(s1, n1), counted_ansi(s2, n2)) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    crate::apiextra::CompareStringW(lcid, flags, a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32)
}

/// lstrcmpA: a case-sensitive comparison by the user locale (-1, 0, 1).
extern "win64" fn lstrcmpA(a: *const u8, b: *const u8) -> i32 {
    let (wa, wb) = (wide_arg(a).unwrap_or_else(|| alloc::vec![0]), wide_arg(b).unwrap_or_else(|| alloc::vec![0]));
    kernel32::lstrcmpW(wa.as_ptr(), wb.as_ptr())
}

// ---- registry --------------------------------------------------------------------

extern "win64" fn RegDeleteValueA(hkey: u64, name: *const u8) -> u32 {
    let n = wide_arg(name);
    crate::registry::RegDeleteValueW(hkey, ptr(&n))
}

extern "win64" fn RegDeleteTreeA(hkey: u64, sub: *const u8) -> u32 {
    let s = wide_arg(sub);
    crate::registry::RegDeleteTreeW(hkey, ptr(&s))
}

/// RegCreateKeyA: RegCreateKeyEx with default options and full access.
extern "win64" fn RegCreateKeyA(hkey: u64, sub: *const u8, out: *mut u64) -> u32 {
    const KEY_ALL_ACCESS: u32 = 0xF003F;
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let s = wide_arg(sub).unwrap_or_else(|| alloc::vec![0]);
    crate::registry::RegCreateKeyExW(hkey, s.as_ptr(), 0, core::ptr::null(), 0, KEY_ALL_ACCESS, 0, out, core::ptr::null_mut())
}

/// RegEnumKeyExA: the name and class as ANSI; the lengths are in bytes
/// without the NUL, as the Unicode function counts characters.
#[allow(clippy::too_many_arguments)]
extern "win64" fn RegEnumKeyExA(
    hkey: u64,
    index: u32,
    name: *mut u8,
    name_len: *mut u32,
    reserved: *const u32,
    class: *mut u8,
    class_len: *mut u32,
    written: *mut u64,
) -> u32 {
    if name.is_null() || name_len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut wn = alloc::vec![0u16; 256];
    let mut wn_len = wn.len() as u32;
    let mut wc = alloc::vec![0u16; 1024];
    let mut wc_len = wc.len() as u32;
    let want_class = !class.is_null() && !class_len.is_null();
    let r = crate::registry::RegEnumKeyExW(
        hkey,
        index,
        wn.as_mut_ptr(),
        &mut wn_len,
        reserved,
        if want_class { wc.as_mut_ptr() } else { core::ptr::null_mut() },
        if want_class { &mut wc_len } else { core::ptr::null_mut() },
        written,
    );
    if r != ERROR_SUCCESS {
        return r;
    }
    let n = String::from_utf16_lossy(&wn[..wn_len as usize]);
    // SAFETY: the caller's in/out lengths.
    unsafe {
        if !put_ansi(&n, name, *name_len as usize) {
            *name_len = n.len() as u32;
            return ERROR_MORE_DATA;
        }
        *name_len = n.len() as u32;
        if want_class {
            let c = String::from_utf16_lossy(&wc[..wc_len as usize]);
            if !put_ansi(&c, class, *class_len as usize) {
                *class_len = c.len() as u32;
                return ERROR_MORE_DATA;
            }
            *class_len = c.len() as u32;
        }
    }
    ERROR_SUCCESS
}

/// RegEnumValueA: the name as ANSI; string data (REG_SZ, REG_EXPAND_SZ,
/// REG_MULTI_SZ) is converted to ANSI as RegQueryValueExA does.
#[allow(clippy::too_many_arguments)]
extern "win64" fn RegEnumValueA(
    hkey: u64,
    index: u32,
    name: *mut u8,
    name_len: *mut u32,
    reserved: *const u32,
    kind: *mut u32,
    data: *mut u8,
    data_len: *mut u32,
) -> u32 {
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_MULTI_SZ: u32 = 7;
    if name.is_null() || name_len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut wn = alloc::vec![0u16; 16384];
    let mut wn_len = wn.len() as u32;
    let mut t = 0u32;
    // First the size, then the data itself.
    let mut size = 0u32;
    let r = crate::registry::RegEnumValueW(hkey, index, wn.as_mut_ptr(), &mut wn_len, reserved, &mut t, core::ptr::null_mut(), &mut size);
    if r != ERROR_SUCCESS {
        return r;
    }
    let mut raw = alloc::vec![0u8; size as usize];
    wn_len = wn.len() as u32;
    let r = crate::registry::RegEnumValueW(hkey, index, wn.as_mut_ptr(), &mut wn_len, reserved, &mut t, raw.as_mut_ptr(), &mut size);
    if r != ERROR_SUCCESS {
        return r;
    }
    raw.truncate(size as usize);
    let bytes: Vec<u8> = if matches!(t, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ) {
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).into_bytes()
    } else {
        raw
    };
    let n = String::from_utf16_lossy(&wn[..wn_len as usize]);
    // SAFETY: the caller's in/out lengths and buffers.
    unsafe {
        if !put_ansi(&n, name, *name_len as usize) {
            *name_len = n.len() as u32;
            return ERROR_MORE_DATA;
        }
        *name_len = n.len() as u32;
        if !kind.is_null() {
            *kind = t;
        }
        if !data_len.is_null() {
            let fits = !data.is_null() && bytes.len() <= *data_len as usize;
            let had_room = !data.is_null();
            *data_len = bytes.len() as u32;
            if fits {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            } else if had_room {
                return ERROR_MORE_DATA;
            }
        }
    }
    ERROR_SUCCESS
}

/// SHDeleteKeyA: the key and everything below it.
extern "win64" fn SHDeleteKeyA(hkey: u64, sub: *const u8) -> u32 {
    let s = wide_arg(sub);
    crate::registry::RegDeleteTreeW(hkey, ptr(&s))
}

// ---- files and volumes ---------------------------------------------------------------

extern "win64" fn CopyFileA(from: *const u8, to: *const u8, fail_if_exists: i32) -> i32 {
    let (f, t) = (wide_arg(from), wide_arg(to));
    crate::files::CopyFileW(ptr(&f), ptr(&t), fail_if_exists)
}

extern "win64" fn SetFileAttributesA(name: *const u8, attrs: u32) -> i32 {
    let n = wide_arg(name);
    crate::files::SetFileAttributesW(ptr(&n), attrs)
}

extern "win64" fn GetDriveTypeA(root: *const u8) -> u32 {
    let r = wide_arg(root);
    crate::volumes::GetDriveTypeW(ptr(&r))
}

extern "win64" fn GetDiskFreeSpaceExA(root: *const u8, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32 {
    let r = wide_arg(root);
    crate::volumes::GetDiskFreeSpaceExW(ptr(&r), avail, total, free)
}

/// GetLogicalDriveStringsA: "C:\\\0...\0\0"; the size without the final NUL.
extern "win64" fn GetLogicalDriveStringsA(size: u32, out: *mut u8) -> u32 {
    let need = crate::volumes::GetLogicalDriveStringsW(0, core::ptr::null_mut());
    if need == 0 {
        return 0;
    }
    let mut w = alloc::vec![0u16; need as usize + 1];
    let n = crate::volumes::GetLogicalDriveStringsW(w.len() as u32, w.as_mut_ptr());
    if n == 0 || n as usize >= w.len() {
        return 0;
    }
    // Drive strings are ASCII: one byte per unit.
    let bytes: Vec<u8> = w[..n as usize].iter().map(|&u| u as u8).collect();
    if out.is_null() || bytes.len() + 1 > size as usize {
        return bytes.len() as u32 + 1;
    }
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    bytes.len() as u32
}

/// GetSystemDirectoryA: as the Unicode form, in bytes.
extern "win64" fn GetSystemDirectoryA(buf: *mut u8, size: u32) -> u32 {
    let mut w = alloc::vec![0u16; 1024];
    let n = crate::sysext::GetSystemDirectoryW(w.as_mut_ptr(), w.len() as u32);
    if n == 0 || n as usize >= w.len() {
        return 0;
    }
    kernel32::out_a(&String::from_utf16_lossy(&w[..n as usize]), buf, size)
}

/// GetTempFileNameA: the name is MAX_PATH bytes at most.
extern "win64" fn GetTempFileNameA(dir: *const u8, prefix: *const u8, unique: u32, out: *mut u8) -> u32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let (d, p) = (wide_arg(dir), wide_arg(prefix));
    let mut w = alloc::vec![0u16; 260];
    let r = kernel32::GetTempFileNameW(ptr(&d), ptr(&p), unique, w.as_mut_ptr());
    if r == 0 {
        return 0;
    }
    let name = until_nul(&w);
    if !put_ansi(&name, out, 260) {
        return fail(111, 0); // ERROR_BUFFER_OVERFLOW
    }
    r
}

/// ExpandEnvironmentStringsA: the size in bytes with the NUL.
extern "win64" fn ExpandEnvironmentStringsA(src: *const u8, dst: *mut u8, size: u32) -> u32 {
    let Some(s) = wide_arg(src) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let need = crate::sysext::ExpandEnvironmentStringsW(s.as_ptr(), core::ptr::null_mut(), 0);
    if need == 0 {
        return 0;
    }
    let mut w = alloc::vec![0u16; need as usize];
    if crate::sysext::ExpandEnvironmentStringsW(s.as_ptr(), w.as_mut_ptr(), need) == 0 {
        return 0;
    }
    let text = until_nul(&w);
    put_ansi(&text, dst, size as usize);
    text.len() as u32 + 1
}

/// GetPrivateProfileStringA: the Unicode result as ANSI; a truncated
/// value ends at the buffer, NUL-terminated, as on Windows.
extern "win64" fn GetPrivateProfileStringA(
    section: *const u8,
    key: *const u8,
    default: *const u8,
    buf: *mut u8,
    size: u32,
    file: *const u8,
) -> u32 {
    if buf.is_null() || size == 0 {
        return 0;
    }
    let (s, k, d, f) = (wide_arg(section), wide_arg(key), wide_arg(default), wide_arg(file));
    let mut w = alloc::vec![0u16; 65536];
    let n = crate::ini::GetPrivateProfileStringW(ptr(&s), ptr(&k), ptr(&d), w.as_mut_ptr(), w.len() as u32, ptr(&f)) as usize;
    // Section and key lists are NUL-separated: convert unit by unit.
    let text: Vec<u8> = String::from_utf16_lossy(&w[..n]).into_bytes();
    let cap = size as usize;
    let list = s.is_none() || k.is_none();
    let take = text.len().min(if list { cap.saturating_sub(2) } else { cap - 1 });
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, take);
        *buf.add(take) = 0;
        if list && cap >= 2 {
            *buf.add(take + 1) = 0;
        }
    }
    take as u32
}

/// GetBinaryTypeW: the kind of executable a file is, from its headers:
/// SCS_32BIT_BINARY (0) and SCS_64BIT_BINARY (6) for PE images by
/// machine, SCS_DOS_BINARY (1) for MZ files without a PE header,
/// SCS_OS216_BINARY (5) for NE and SCS_WOW_BINARY (2) for 16-bit Windows.
/// Other files are ERROR_BAD_EXE_FORMAT.
extern "win64" fn GetBinaryTypeW(name: *const u16, kind: *mut u32) -> i32 {
    const ERROR_BAD_EXE_FORMAT: u32 = 193;
    if kind.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: a NUL-terminated name.
    let Some(win) = (unsafe { path::wide_to_string(name) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let p = match kernel32::locate(&win) {
        Ok((p, true)) => p,
        Ok((_, false)) => return fail(ERROR_FILE_NOT_FOUND, 0),
        Err(e) => return fail(e, 0),
    };
    let Ok(mut f) = os_std::fs::File::open(&p) else { return fail(ERROR_ACCESS_DENIED, 0) };
    let mut head = [0u8; 0x400];
    let n = os_std::io::Read::read(&mut f, &mut head).unwrap_or(0);
    let head = &head[..n];
    if head.len() < 0x40 || &head[..2] != b"MZ" {
        return fail(ERROR_BAD_EXE_FORMAT, 0);
    }
    let lfanew = u32::from_le_bytes([head[0x3C], head[0x3D], head[0x3E], head[0x3F]]) as usize;
    let mut sig = [0u8; 6];
    let got = os_std::io::Seek::seek(&mut f, os_std::io::SeekFrom::Start(lfanew as u64)).is_ok()
        && os_std::io::Read::read(&mut f, &mut sig).is_ok_and(|n| n == sig.len());
    let t = if !got {
        1
    } else if &sig[..4] == b"PE\0\0" {
        match u16::from_le_bytes([sig[4], sig[5]]) {
            0x14C | 0x1C0 | 0x1C4 => 0,
            0x8664 | 0xAA64 => 6,
            _ => return fail(ERROR_BAD_EXE_FORMAT, 0),
        }
    } else if &sig[..2] == b"NE" {
        // NE target OS byte (0x36): 1 OS/2, 2 Windows.
        let mut os = [0u8; 1];
        let ok = os_std::io::Seek::seek(&mut f, os_std::io::SeekFrom::Start(lfanew as u64 + 0x36)).is_ok()
            && os_std::io::Read::read(&mut f, &mut os).is_ok();
        if ok && os[0] == 1 { 5 } else { 2 }
    } else {
        1
    };
    // SAFETY: the caller's out pointer.
    unsafe { *kind = t };
    TRUE
}

// ---- synchronisation and jobs ----------------------------------------------------------

extern "win64" fn OpenMutexA(access: u32, inherit: Bool, name: *const u8) -> u64 {
    let n = wide_arg(name);
    kernel32::OpenMutexW(access, inherit, ptr(&n))
}

extern "win64" fn CreateJobObjectA(sa: u64, name: *const u8) -> u64 {
    let n = wide_arg(name);
    crate::process::CreateJobObjectW(sa, ptr(&n))
}

// ---- debugging -------------------------------------------------------------------------

/// DebugActiveProcess: OSyx has no debugging interface (no debug events,
/// no way to stop another process's threads), so no process can be
/// attached: ERROR_NOT_SUPPORTED. The program's own handle still has to
/// name a process (ERROR_INVALID_PARAMETER otherwise, as on Windows).
extern "win64" fn DebugActiveProcess(pid: u32) -> Bool {
    if pid == 0 || os_sys::proc_info(pid as u64).is_err() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    fail(ERROR_NOT_SUPPORTED, FALSE)
}

/// DebugActiveProcessStop: no process is ever being debugged (see
/// DebugActiveProcess); Windows fails with ERROR_INVALID_PARAMETER for a
/// process the caller does not debug.
extern "win64" fn DebugActiveProcessStop(_pid: u32) -> Bool {
    fail(ERROR_INVALID_PARAMETER, FALSE)
}

// ---- psapi -----------------------------------------------------------------------------

/// The file name of a module of this process (the module handle, or the
/// executable for NULL).
fn module_path(process: u64, module: u64) -> Result<String, u32> {
    if process != objects::CURRENT_PROCESS {
        return Err(ERROR_ACCESS_DENIED);
    }
    let h = if module == 0 { crate::loader::get_module_handle(None).unwrap_or(0) } else { module };
    kernel32::module_name(h).ok_or(ERROR_INVALID_HANDLE)
}

fn base_name(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

/// GetModuleBaseNameW: the file name without its directory, truncated to
/// `size` characters (the result then has no NUL, as on Windows).
pub(crate) extern "win64" fn GetModuleBaseNameW(process: u64, module: u64, buf: *mut u16, size: u32) -> u32 {
    let p = match module_path(process, module) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if buf.is_null() || size == 0 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let w: Vec<u16> = base_name(&p).encode_utf16().collect();
    let n = w.len().min(size as usize);
    // SAFETY: the caller's buffer of `size` units.
    unsafe {
        core::ptr::copy_nonoverlapping(w.as_ptr(), buf, n);
        if n < size as usize {
            *buf.add(n) = 0;
        }
    }
    n as u32
}

extern "win64" fn GetModuleBaseNameA(process: u64, module: u64, buf: *mut u8, size: u32) -> u32 {
    let p = match module_path(process, module) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if buf.is_null() || size == 0 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let b = base_name(&p).as_bytes();
    let n = b.len().min(size as usize);
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(b.as_ptr(), buf, n);
        if n < size as usize {
            *buf.add(n) = 0;
        }
    }
    n as u32
}

/// GetModuleFileNameExA: the full file name, truncated as the base-name
/// form is.
extern "win64" fn GetModuleFileNameExA(process: u64, module: u64, buf: *mut u8, size: u32) -> u32 {
    let p = match module_path(process, module) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if buf.is_null() || size == 0 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let b = p.as_bytes();
    let n = b.len().min(size as usize);
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(b.as_ptr(), buf, n);
        if n < size as usize {
            *buf.add(n) = 0;
        }
    }
    n as u32
}

// ---- version ---------------------------------------------------------------------------

extern "win64" fn GetFileVersionInfoSizeA(name: *const u8, handle: *mut u32) -> u32 {
    let n = wide_arg(name);
    crate::version::GetFileVersionInfoSizeW(ptr(&n), handle)
}

extern "win64" fn GetFileVersionInfoA(name: *const u8, handle: u32, len: u32, out: *mut u8) -> i32 {
    let n = wide_arg(name);
    crate::version::GetFileVersionInfoW(ptr(&n), handle, len, out)
}

/// VerQueryValueA: the block holds the Unicode (VS_VERSIONINFO) form, so
/// string values ("\StringFileInfo\...") are converted to ANSI into a
/// copy that lives as long as the process (Windows returns them inside
/// the block's ANSI area); binary values point into the block.
extern "win64" fn VerQueryValueA(block: *const u8, query: *const u8, out: *mut *const u8, len: *mut u32) -> i32 {
    let Some(q) = wide_arg(query) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let mut value: *const u8 = core::ptr::null();
    let mut n = 0u32;
    if crate::version::VerQueryValueW(block, q.as_ptr(), &mut value, &mut n) == 0 {
        return 0;
    }
    let is_string = until_nul(&q).to_ascii_lowercase().starts_with("\\stringfileinfo\\");
    let (p, l) = if is_string && !value.is_null() {
        // SAFETY: a NUL-terminated UTF-16 value inside the block.
        let s = unsafe { path::wide_to_string(value as *const u16) }.unwrap_or_default();
        let mut bytes = s.into_bytes();
        let l = bytes.len() as u32 + 1;
        bytes.push(0);
        (alloc::boxed::Box::leak(bytes.into_boxed_slice()).as_ptr(), l)
    } else {
        (value, n)
    };
    // SAFETY: the caller's out pointers.
    unsafe {
        if !out.is_null() {
            *out = p;
        }
        if !len.is_null() {
            *len = l;
        }
    }
    1
}

// ---- shell -----------------------------------------------------------------------------

/// PathFileExistsW: whether the file or directory exists.
extern "win64" fn PathFileExistsW(name: *const u16) -> Bool {
    // SAFETY: NULL or a NUL-terminated name.
    let Some(win) = (unsafe { path::wide_to_string(name) }) else { return FALSE };
    matches!(kernel32::locate(&win), Ok((_, true))) as Bool
}

// ---- user32 ----------------------------------------------------------------------------

extern "win64" fn GetClassNameA(wnd: u64, buf: *mut u8, size: i32) -> i32 {
    if buf.is_null() || size <= 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut w = alloc::vec![0u16; 257];
    let n = crate::user32::GetClassNameW(wnd, w.as_mut_ptr(), w.len() as i32);
    if n == 0 {
        return 0;
    }
    let s = String::from_utf16_lossy(&w[..n as usize]);
    let b = s.as_bytes();
    let k = b.len().min(size as usize - 1);
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(b.as_ptr(), buf, k);
        *buf.add(k) = 0;
    }
    k as i32
}

extern "win64" fn RegisterWindowMessageA(name: *const u8) -> u32 {
    let n = wide_arg(name);
    crate::user32::RegisterWindowMessageW(ptr(&n))
}

/// Window messages carry the same values in both character sets here.
extern "win64" fn SendMessageTimeoutA(wnd: u64, msg: u32, wparam: u64, lparam: i64, flags: u32, timeout: u32, out: *mut u64) -> i64 {
    crate::user32::SendMessageTimeoutW(wnd, msg, wparam, lparam, flags, timeout, out)
}

extern "win64" fn SendNotifyMessageA(wnd: u64, msg: u32, wparam: u64, lparam: i64) -> Bool {
    crate::user32::SendNotifyMessageW(wnd, msg, wparam, lparam)
}

extern "win64" fn MapVirtualKeyA(code: u32, kind: u32) -> u32 {
    crate::user32::MapVirtualKeyW(code, kind)
}

extern "win64" fn MapVirtualKeyExA(code: u32, kind: u32, hkl: u64) -> u32 {
    crate::user32::MapVirtualKeyExW(code, kind, hkl)
}

extern "win64" fn GetClipboardFormatNameA(format: u32, buf: *mut u8, n: i32) -> i32 {
    if buf.is_null() || n <= 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut w = alloc::vec![0u16; 256];
    let got = crate::user32::GetClipboardFormatNameW(format, w.as_mut_ptr(), w.len() as i32);
    if got == 0 {
        return 0;
    }
    let s = String::from_utf16_lossy(&w[..got as usize]);
    let b = s.as_bytes();
    let k = b.len().min(n as usize - 1);
    // SAFETY: the caller's buffer of `n` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(b.as_ptr(), buf, k);
        *buf.add(k) = 0;
    }
    k as i32
}

/// CharLowerBuffW: lower-cases `n` characters in place (Unicode simple
/// case mapping); returns how many were processed.
extern "win64" fn CharLowerBuffW(s: *mut u16, n: u32) -> u32 {
    if s.is_null() {
        return 0;
    }
    // SAFETY: the caller's buffer of `n` units.
    let units = unsafe { core::slice::from_raw_parts_mut(s, n as usize) };
    for u in units.iter_mut() {
        if let Some(c) = char::from_u32(*u as u32) {
            let mut lower = c.to_lowercase();
            if let (Some(l), None) = (lower.next(), lower.next()) {
                if (l as u32) < 0x10000 {
                    *u = l as u16;
                }
            }
        }
    }
    n
}

pub fn kernel32_exports() -> Vec<(&'static str, u64)> {
    export_table![
        CompareStringA,
        lstrcmpA,
        CopyFileA,
        SetFileAttributesA,
        GetDriveTypeA,
        GetDiskFreeSpaceExA,
        GetLogicalDriveStringsA,
        GetSystemDirectoryA,
        GetTempFileNameA,
        ExpandEnvironmentStringsA,
        GetPrivateProfileStringA,
        GetBinaryTypeW,
        OpenMutexA,
        CreateJobObjectA,
        DebugActiveProcess,
        DebugActiveProcessStop,
    ]
    .into_iter()
    .chain([
        ("K32GetModuleBaseNameW", GetModuleBaseNameW as *const () as u64),
        ("K32GetModuleBaseNameA", GetModuleBaseNameA as *const () as u64),
        ("K32GetModuleFileNameExA", GetModuleFileNameExA as *const () as u64),
    ])
    .collect()
}

pub fn advapi32_exports() -> Vec<(&'static str, u64)> {
    export_table![RegDeleteValueA, RegDeleteTreeA, RegCreateKeyA, RegEnumKeyExA, RegEnumValueA]
}

pub fn psapi_exports() -> Vec<(&'static str, u64)> {
    export_table![GetModuleBaseNameW, GetModuleBaseNameA, GetModuleFileNameExA]
}

pub fn version_exports() -> Vec<(&'static str, u64)> {
    export_table![GetFileVersionInfoSizeA, GetFileVersionInfoA, VerQueryValueA]
}

pub fn shlwapi_exports() -> Vec<(&'static str, u64)> {
    export_table![PathFileExistsW, SHDeleteKeyA]
}

pub fn user32_exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetClassNameA,
        RegisterWindowMessageA,
        SendMessageTimeoutA,
        SendNotifyMessageA,
        MapVirtualKeyA,
        MapVirtualKeyExA,
        GetClipboardFormatNameA,
        CharLowerBuffW,
    ]
}
