//! File system functions (filesystem contract) over the Win32 file API:
//! errors become errno through the UCRT's Win32 error mapping.

use super::*;
use alloc::string::String;

k32fn!(fn set_current_directory = "SetCurrentDirectoryW"(p: *const u16) -> i32);
k32fn!(fn get_full_path_name = "GetFullPathNameW"(p: *const u16, n: u32, buf: *mut u16, part: u64) -> u32);
k32fn!(fn delete_file = "DeleteFileW"(p: *const u16) -> i32);
k32fn!(fn move_file_ex = "MoveFileExW"(a: *const u16, b: *const u16, flags: u32) -> i32);
k32fn!(fn remove_directory = "RemoveDirectoryW"(p: *const u16) -> i32);
k32fn!(fn create_directory = "CreateDirectoryW"(p: *const u16, sa: u64) -> i32);
k32fn!(pub fn get_file_attributes = "GetFileAttributesW"(p: *const u16) -> u32);

/// A narrow path as a NUL-terminated wide string (ANSI = UTF-8 on OSyx).
pub fn widen_path(p: *const u8) -> Vec<u16> {
    // SAFETY: a NUL-terminated path.
    let mut w: Vec<u16> = String::from_utf8_lossy(unsafe { cstr(p) }).encode_utf16().collect();
    w.push(0);
    w
}

/// A wide path with its terminator.
pub fn wide_path(p: *const u16) -> Vec<u16> {
    // SAFETY: a NUL-terminated path.
    let mut w = unsafe { wstr(p) }.to_vec();
    w.push(0);
    w
}

/// Runs a Win32 call that returns nonzero on success: 0, or -1 with errno.
fn win(ok: i32) -> i32 {
    if ok != 0 {
        0
    } else {
        dosmaperr(get_last_error());
        -1
    }
}

extern "win64" fn _wchdir(p: *const u16) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(set_current_directory(wide_path(p).as_ptr()))
}

extern "win64" fn _chdir(p: *const u8) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(set_current_directory(widen_path(p).as_ptr()))
}

extern "win64" fn _wremove(p: *const u16) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(delete_file(wide_path(p).as_ptr()))
}

extern "win64" fn remove(p: *const u8) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(delete_file(widen_path(p).as_ptr()))
}

const MOVEFILE_COPY_ALLOWED: u32 = 2;

extern "win64" fn _wrename(a: *const u16, b: *const u16) -> i32 {
    validate!(!a.is_null() && !b.is_null(), EINVAL, -1);
    win(move_file_ex(wide_path(a).as_ptr(), wide_path(b).as_ptr(), MOVEFILE_COPY_ALLOWED))
}

extern "win64" fn rename(a: *const u8, b: *const u8) -> i32 {
    validate!(!a.is_null() && !b.is_null(), EINVAL, -1);
    win(move_file_ex(widen_path(a).as_ptr(), widen_path(b).as_ptr(), MOVEFILE_COPY_ALLOWED))
}

extern "win64" fn _wrmdir(p: *const u16) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(remove_directory(wide_path(p).as_ptr()))
}

extern "win64" fn _rmdir(p: *const u8) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(remove_directory(widen_path(p).as_ptr()))
}

extern "win64" fn _wmkdir(p: *const u16) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(create_directory(wide_path(p).as_ptr(), 0))
}

extern "win64" fn _mkdir(p: *const u8) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    win(create_directory(widen_path(p).as_ptr(), 0))
}

const FILE_ATTRIBUTE_READONLY: u32 = 1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// `_access_s`: 0 (exists), 2 (write), 4 (read), 6 (read/write).
fn access_s(path: Vec<u16>, mode: i32) -> i32 {
    validate!(mode & !6 == 0, EINVAL, EINVAL);
    let a = get_file_attributes(path.as_ptr());
    if a == u32::MAX {
        dosmaperr(get_last_error());
        return errno();
    }
    if mode & 2 != 0 && a & FILE_ATTRIBUTE_READONLY != 0 && a & FILE_ATTRIBUTE_DIRECTORY == 0 {
        set_errno(EACCES);
        return EACCES;
    }
    0
}

extern "win64" fn _waccess_s(p: *const u16, mode: i32) -> i32 {
    validate!(!p.is_null(), EINVAL, EINVAL);
    access_s(wide_path(p), mode)
}

extern "win64" fn _access_s(p: *const u8, mode: i32) -> i32 {
    validate!(!p.is_null(), EINVAL, EINVAL);
    access_s(widen_path(p), mode)
}

extern "win64" fn _waccess(p: *const u16, mode: i32) -> i32 {
    if _waccess_s(p, mode) == 0 { 0 } else { -1 }
}

extern "win64" fn _access(p: *const u8, mode: i32) -> i32 {
    if _access_s(p, mode) == 0 { 0 } else { -1 }
}

/// `_wfullpath`: NULL or empty path is the current directory; a NULL
/// buffer is allocated (at least `max` units).
fn fullpath(buf: *mut u16, path: &[u16], max: usize) -> Result<Vec<u16>, ()> {
    let mut p = path.to_vec();
    p.push(0);
    let need = get_full_path_name(p.as_ptr(), 0, core::ptr::null_mut(), 0);
    if need == 0 {
        dosmaperr(get_last_error());
        return Err(());
    }
    let mut out = alloc::vec![0u16; need as usize + 1];
    let n = get_full_path_name(p.as_ptr(), out.len() as u32, out.as_mut_ptr(), 0);
    if n == 0 || n as usize >= out.len() {
        dosmaperr(get_last_error());
        return Err(());
    }
    out.truncate(n as usize);
    if !buf.is_null() && out.len() >= max {
        set_errno(ERANGE);
        return Err(());
    }
    Ok(out)
}

const MAX_PATH: usize = 260;

extern "win64" fn _wfullpath(buf: *mut u16, path: *const u16, max: usize) -> *mut u16 {
    // SAFETY: a NUL-terminated path when not NULL.
    let text = if path.is_null() || unsafe { *path } == 0 {
        super::env::current_dir()
    } else {
        // SAFETY: a NUL-terminated path.
        match fullpath(buf, unsafe { wstr(path) }, max) {
            Ok(t) => t,
            Err(()) => return core::ptr::null_mut(),
        }
    };
    if !buf.is_null() && text.len() >= max {
        set_errno(ERANGE);
        return core::ptr::null_mut();
    }
    let dst = if buf.is_null() {
        let p = crt_malloc(text.len().max(MAX_PATH).max(max) * 2 + 2) as *mut u16;
        if p.is_null() {
            set_errno(ENOMEM);
            return p;
        }
        p
    } else {
        buf
    };
    // SAFETY: `dst` holds at least len + 1 units.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), dst, text.len());
        *dst.add(text.len()) = 0;
    }
    dst
}

extern "win64" fn _fullpath(buf: *mut u8, path: *const u8, max: usize) -> *mut u8 {
    let wide = if path.is_null() { None } else { Some(widen_path(path)) };
    let w = _wfullpath(core::ptr::null_mut(), wide.as_ref().map_or(core::ptr::null(), |w| w.as_ptr()), 0);
    if w.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the block just returned.
    let text = String::from_utf16_lossy(unsafe { wstr(w) }).into_bytes();
    crt_free(w as *mut u8);
    if !buf.is_null() && text.len() >= max {
        set_errno(ERANGE);
        return core::ptr::null_mut();
    }
    let dst = if buf.is_null() {
        let p = crt_malloc(text.len().max(MAX_PATH).max(max) + 1);
        if p.is_null() {
            set_errno(ENOMEM);
            return p;
        }
        p
    } else {
        buf
    };
    // SAFETY: `dst` holds at least len + 1 bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), dst, text.len());
        *dst.add(text.len()) = 0;
    }
    dst
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        _wchdir, _chdir, _wremove, remove, _wrename, rename, _wrmdir, _rmdir, _wmkdir, _mkdir, _waccess_s, _access_s,
        _waccess, _access, _wfullpath, _fullpath,
    ]
}
