//! C: maps to the OSyx root; mounted volumes remain accessible below it.
//! Volume information and directory enumeration use the real VFS.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail, out_w},
    path, teb,
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::{fs, sync::Mutex};
use osyx_abi::{FileStat, FsStat};

fn locate(p: *const u16) -> Result<String, u32> {
    let win = unsafe { path::wide_to_string(p) }.unwrap_or_else(|| String::from("C:\\"));
    let cwd = path::to_windows(os_std::env::current_dir().map_err(|_| ERROR_PATH_NOT_FOUND)?.as_str());
    path::resolve(&path::to_osyx(&win, &cwd)?)
        .and_then(|(p, exists)| if exists { Ok(p) } else { Err(ERROR_PATH_NOT_FOUND) })
}
fn usage(p: *const u16) -> Result<FsStat, u32> {
    os_sys::statfs(&locate(p)?).map_err(errno_to_win32)
}

pub(crate) extern "win64" fn GetLogicalDriveStringsW(size: u32, out: *mut u16) -> u32 {
    if out.is_null() || size < 5 {
        return 5;
    }
    unsafe {
        core::ptr::copy_nonoverlapping([67u16, 58, 92, 0, 0].as_ptr(), out, 5);
    }
    4
}
extern "win64" fn GetLogicalDrives() -> u32 {
    1 << 2
}
pub(crate) extern "win64" fn GetDriveTypeW(root: *const u16) -> u32 {
    match usage(root) {
        Ok(s) => {
            if osyx_abi::ipc::field_str(&s.fstype) == "tmpfs" {
                6
            } else {
                3
            }
        }
        Err(_) => 1,
    }
}
pub(crate) extern "win64" fn GetDiskFreeSpaceExW(root: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32 {
    let s = match usage(root) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    unsafe {
        if !avail.is_null() {
            *avail = s.free_blocks.saturating_mul(s.block_size);
        }
        if !total.is_null() {
            *total = s.total_blocks.saturating_mul(s.block_size);
        }
        if !free.is_null() {
            *free = s.free_blocks.saturating_mul(s.block_size);
        }
    }
    1
}
extern "win64" fn GetDiskFreeSpaceA(
    root: *const u8,
    sectors: *mut u32,
    bytes: *mut u32,
    free: *mut u32,
    total: *mut u32,
) -> i32 {
    // SAFETY: NULL (the current drive) or a NUL-terminated name.
    let w = unsafe { path::ansi_to_wide(root) };
    GetDiskFreeSpaceW(w.as_ref().map_or(core::ptr::null(), |w| w.as_ptr()), sectors, bytes, free, total)
}
extern "win64" fn GetDiskFreeSpaceW(
    root: *const u16,
    sectors: *mut u32,
    bytes: *mut u32,
    free: *mut u32,
    total: *mut u32,
) -> i32 {
    let s = match usage(root) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    unsafe {
        if !sectors.is_null() {
            *sectors = (s.block_size / 512).max(1) as u32;
        }
        if !bytes.is_null() {
            *bytes = 512;
        }
        if !free.is_null() {
            *free = s.free_blocks.min(u32::MAX as u64) as u32;
        }
        if !total.is_null() {
            *total = s.total_blocks.min(u32::MAX as u64) as u32;
        }
    }
    1
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn GetVolumeInformationA(
    root: *const u8,
    label: *mut u8,
    label_cap: u32,
    serial: *mut u32,
    max_component: *mut u32,
    flags: *mut u32,
    fsname: *mut u8,
    fs_cap: u32,
) -> i32 {
    // SAFETY: NULL or a NUL-terminated root.
    let wroot: Option<Vec<u16>> = unsafe { crate::path::ansi_to_wide(root) };
    let mut wl = alloc::vec![0u16; label_cap as usize];
    let mut wf = alloc::vec![0u16; fs_cap as usize];
    let ok = GetVolumeInformationW(
        wroot.as_ref().map_or(core::ptr::null(), |w| w.as_ptr()),
        if label.is_null() { core::ptr::null_mut() } else { wl.as_mut_ptr() },
        label_cap,
        serial,
        max_component,
        flags,
        if fsname.is_null() { core::ptr::null_mut() } else { wf.as_mut_ptr() },
        fs_cap,
    );
    if ok != 0 {
        let narrow = |w: &[u16]| String::from_utf16_lossy(&w[..w.iter().position(|&u| u == 0).unwrap_or(w.len())]);
        crate::kernel32::out_a(&narrow(&wl), label, label_cap);
        crate::kernel32::out_a(&narrow(&wf), fsname, fs_cap);
    }
    ok
}

extern "win64" fn GetVolumeInformationW(
    root: *const u16,
    label: *mut u16,
    label_cap: u32,
    serial: *mut u32,
    max_component: *mut u32,
    flags: *mut u32,
    fsname: *mut u16,
    fs_cap: u32,
) -> i32 {
    let s = match usage(root) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    let fstype = osyx_abi::ipc::field_str(&s.fstype);
    // A mount's source is its display label. The serial is OSyx's volume id.
    let mount = os_std::system::mounts().unwrap_or_default().into_iter().find(|m| m.dev == s.dev);
    let name = mount.as_ref().map_or("OSyx", |m| os_std::system::field_str(&m.source));
    if (!label.is_null() && label_cap <= name.encode_utf16().count() as u32)
        || (!fsname.is_null() && fs_cap <= fstype.encode_utf16().count() as u32)
    {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    unsafe {
        if !serial.is_null() {
            *serial = s.dev as u32;
        }
        if !max_component.is_null() {
            *max_component = 255;
        }
        if !flags.is_null() {
            *flags = 2 | 4 | if s.flags & osyx_abi::MOUNT_READ_ONLY != 0 { 0x80000 } else { 0 };
        }
    }
    out_w(name, label, label_cap);
    out_w(fstype, fsname, fs_cap);
    1
}

struct Search {
    rows: Vec<(String, FileStat)>,
    next: usize,
}
static SEARCHES: Mutex<BTreeMap<u64, Search>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x5000_0000);

fn matches(pattern: &str, name: &str) -> bool {
    if pattern == "*.*" {
        return true;
    }
    let p = pattern.as_bytes();
    let n = name.as_bytes();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == b'?' || p[i].eq_ignore_ascii_case(&n[j])) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn write_find(out: u64, name: &str, st: &FileStat) -> i32 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let name: Vec<u16> = name.encode_utf16().collect();
    if name.len() >= 260 {
        return fail(ERROR_FILENAME_EXCED_RANGE, 0);
    }
    // WIN32_FIND_DATAW: DWORDs/FILETIMEs followed by 260 WCHARs + 14 WCHARs.
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, 592);
        teb::put(out, 0, kernel32::attributes(st) | if st.kind == osyx_abi::KIND_SYMLINK { 0x400 } else { 0 });
        for (off, ns) in [(4, st.ctime_ns), (12, st.atime_ns), (20, st.mtime_ns)] {
            teb::put(out, off, kernel32::filetime(ns));
        }
        teb::put(out, 28, (st.size >> 32) as u32);
        teb::put(out, 32, st.size as u32);
        core::ptr::copy_nonoverlapping(name.as_ptr(), (out + 44) as *mut u16, name.len());
    }
    1
}
fn matches_exact(pattern: &str, name: &str) -> bool {
    if pattern == "*.*" {
        return true;
    }
    let p = pattern.as_bytes();
    let n = name.as_bytes();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

pub(crate) extern "win64" fn FindFirstFileW(pattern: *const u16, out: u64) -> u64 {
    find_first(pattern, out, false)
}

/// FIND_FIRST_EX_CASE_SENSITIVE compares the pattern's characters as they
/// are; the default search folds ASCII case like Windows does.
pub(crate) fn find_first(pattern: *const u16, out: u64, case_sensitive: bool) -> u64 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    let Some(win) = (unsafe { path::wide_to_string(pattern) }) else {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    };
    let win = win.replace('/', "\\");
    let (dir, pattern) = win.rsplit_once('\\').unwrap_or((".", &win));
    let rooted_drive;
    let dir = if dir.len() == 2 && dir.ends_with(':') {
        rooted_drive = alloc::format!("{dir}\\");
        rooted_drive.as_str()
    } else { dir };
    let cwd = path::to_windows(
        os_std::env::current_dir().map(|p| String::from(p.as_str())).unwrap_or_else(|_| String::from("/")).as_str(),
    );
    let dir = match path::to_osyx(if dir.is_empty() { "\\" } else { dir }, &cwd)
        .and_then(|p| path::resolve(&p).map(|(p, _)| p))
    {
        Ok(p) => p,
        Err(e) => return fail(e, u64::MAX),
    };
    let rd = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return fail(ERROR_PATH_NOT_FOUND, u64::MAX),
    };
    let mut rows = Vec::new();
    for entry in rd {
        let name = entry.file_name();
        if if case_sensitive { matches_exact(pattern, name) } else { matches(pattern, name) } {
            if let Ok(st) = os_sys::lstat(&alloc::format!("{dir}/{name}")) {
                rows.push((String::from(name), st));
            }
        }
    }
    let Some((name, st)) = rows.first() else {
        return fail(ERROR_FILE_NOT_FOUND, u64::MAX);
    };
    if write_find(out, name, st) == 0 {
        return u64::MAX;
    }
    let mut searches = SEARCHES.lock().unwrap();
    if searches.len() >= 256 {
        return fail(ERROR_TOO_MANY_OPEN_FILES, u64::MAX);
    }
    let h = NEXT.fetch_add(4, Ordering::Relaxed);
    searches.insert(h, Search { rows, next: 1 });
    h
}
extern "win64" fn FindNextFileW(h: u64, out: u64) -> i32 {
    let mut searches = SEARCHES.lock().unwrap();
    let Some(s) = searches.get_mut(&h) else {
        return fail(ERROR_INVALID_HANDLE, 0);
    };
    let Some((name, st)) = s.rows.get(s.next) else {
        return fail(18, 0);
    };
    let ok = write_find(out, name, st);
    if ok != 0 {
        s.next += 1;
    }
    ok
}
/// WIN32_FIND_DATAW (592 bytes) → WIN32_FIND_DATAA (320 bytes): the same
/// 44-byte head, then cFileName[260] and cAlternateFileName[14] in the
/// ANSI code page (UTF-8).
fn find_data_to_ansi(w: &[u8; 592], out: u64) -> i32 {
    // SAFETY: the name at offset 44 is NUL-terminated within 260 units.
    let name = unsafe { path::wide_to_string(w.as_ptr().add(44) as *const u16) }.unwrap_or_default();
    if name.len() >= 260 {
        return fail(ERROR_FILENAME_EXCED_RANGE, 0);
    }
    // SAFETY: the caller's 320-byte WIN32_FIND_DATAA.
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, 320);
        core::ptr::copy_nonoverlapping(w.as_ptr(), out as *mut u8, 44);
        core::ptr::copy_nonoverlapping(name.as_ptr(), (out + 44) as *mut u8, name.len());
    }
    1
}
extern "win64" fn FindFirstFileA(pattern: *const u8, out: u64) -> u64 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    // SAFETY: a NUL-terminated pattern.
    let Some(p) = (unsafe { path::ansi_to_wide(pattern) }) else { return fail(ERROR_INVALID_PARAMETER, u64::MAX) };
    let mut w = [0u8; 592];
    let h = FindFirstFileW(p.as_ptr(), w.as_mut_ptr() as u64);
    if h != u64::MAX && find_data_to_ansi(&w, out) == 0 {
        FindClose(h);
        return u64::MAX;
    }
    h
}
extern "win64" fn FindNextFileA(h: u64, out: u64) -> i32 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut w = [0u8; 592];
    if FindNextFileW(h, w.as_mut_ptr() as u64) == 0 {
        return 0;
    }
    find_data_to_ansi(&w, out)
}
extern "win64" fn FindClose(h: u64) -> i32 {
    if SEARCHES.lock().unwrap().remove(&h).is_some() {
        1
    } else {
        fail(ERROR_INVALID_HANDLE, 0)
    }
}
/// One volume, drive C:, as \Device\HarddiskVolume1. With a NULL name
/// the list of DOS device names (just "C:") is returned.
extern "win64" fn QueryDosDeviceW(name: *const u16, buf: *mut u16, max: u32) -> u32 {
    // SAFETY: NULL or a NUL-terminated name.
    let n = unsafe { path::wide_to_string(name) };
    let target = match n.as_deref() {
        None => "C:",
        Some(d) if d.eq_ignore_ascii_case("C:") => "\\Device\\HarddiskVolume1",
        Some(_) => return fail(ERROR_FILE_NOT_FOUND, 0),
    };
    let w: Vec<u16> = target.encode_utf16().chain([0, 0]).collect();
    if buf.is_null() || (max as usize) < w.len() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `max` units.
    unsafe { core::ptr::copy_nonoverlapping(w.as_ptr(), buf, w.len()) };
    w.len() as u32
}
/// Every path lies on the one volume, mounted at C:.
extern "win64" fn GetVolumePathNameW(name: *const u16, buf: *mut u16, len: u32) -> i32 {
    // SAFETY: a NUL-terminated name.
    if unsafe { path::wide_to_string(name) }.is_none() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let r = kernel32::out_w("C:\\", buf, len);
    if r >= len {
        return fail(ERROR_FILENAME_EXCED_RANGE, 0);
    }
    1
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetLogicalDriveStringsW,
        GetLogicalDrives,
        GetDriveTypeW,
        GetDiskFreeSpaceExW,
        GetDiskFreeSpaceW,
        QueryDosDeviceW,
        GetVolumePathNameW,
        GetDiskFreeSpaceA,
        GetVolumeInformationW,
        GetVolumeInformationA,
        FindFirstFileW,
        FindFirstFileA,
        FindNextFileA,
        FindNextFileW,
        FindClose
    ]
}
