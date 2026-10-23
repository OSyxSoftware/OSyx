//! kernel32.dll / kernelbase.dll: an original implementation of the Win32
//! base API on OSyx. Every function is backed by an OSyx facility (VFS,
//! threads, futexes, memory mappings, the clock); calls that cannot be
//! honoured fail with a Windows error code rather than pretending.

#![allow(non_snake_case)]

use crate::error::*;
use crate::objects::{self, Console, EventObj, FileObj, MutexObj, Object, SemaphoreObj, ThreadObj};
use crate::{loader, mem, path, teb};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use os_std::sync::Mutex;
use osyx_abi::Errno;
use osyx_pe_loader::link::Symbol;

pub type Bool = i32;
pub const TRUE: Bool = 1;
pub const FALSE: Bool = 0;
pub const INVALID_HANDLE_VALUE: u64 = u64::MAX;

/// Sets the thread's last error and returns `v`.
pub fn fail<T>(err: u32, v: T) -> T {
    teb::set_last_error(err);
    v
}

fn b(ok: bool) -> Bool {
    ok as Bool
}

/// Copies `s` as an ANSI string into `buf` (`size` chars). Windows rule:
/// returns the length without NUL if it fits, else the size needed with NUL.
pub fn out_a(s: &str, buf: *mut u8, size: u32) -> u32 {
    let bytes = s.as_bytes();
    if buf.is_null() || bytes.len() + 1 > size as usize {
        return bytes.len() as u32 + 1;
    }
    // SAFETY: the caller's buffer holds `size` bytes (checked above).
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *buf.add(bytes.len()) = 0;
    }
    bytes.len() as u32
}

/// As [`out_a`] for UTF-16.
pub fn out_w(s: &str, buf: *mut u16, size: u32) -> u32 {
    let units: Vec<u16> = s.encode_utf16().collect();
    if buf.is_null() || units.len() + 1 > size as usize {
        return units.len() as u32 + 1;
    }
    // SAFETY: as above.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len());
        *buf.add(units.len()) = 0;
    }
    units.len() as u32
}

/// # Safety
/// `p` must be NULL or a readable NUL-terminated string.
unsafe fn sa(p: *const u8) -> Option<String> {
    // SAFETY: forwarded.
    unsafe { path::ansi_to_string(p) }
}

/// # Safety
/// As [`sa`] for UTF-16.
unsafe fn sw(p: *const u16) -> Option<String> {
    // SAFETY: forwarded.
    unsafe { path::wide_to_string(p) }
}

// ---- process-wide state ------------------------------------------------------

pub struct ProcessState {
    pub exe_path: String,
    pub command_line_a: u64,
    pub command_line_w: u64,
    pub std: [u64; 3],
    pub env: Vec<(String, String)>,
}

static PROCESS: Mutex<Option<ProcessState>> = Mutex::new(None);
static HEAP: AtomicU64 = AtomicU64::new(0);

pub fn with_process<R>(f: impl FnOnce(&mut ProcessState) -> R) -> R {
    f(PROCESS.lock().unwrap().as_mut().expect("process initialised"))
}

/// Sets up the console handles, command line and environment.
pub fn init_process(exe_path: &str, command_line: &str, env: Vec<(String, String)>) {
    let std = [
        objects::insert(Object::Console(Console::In)),
        objects::insert(Object::Console(Console::Out)),
        objects::insert(Object::Console(Console::Err)),
    ];
    let a = mem::arena_alloc(command_line.len() + 1, 8);
    let units: Vec<u16> = command_line.encode_utf16().chain(core::iter::once(0)).collect();
    let w = mem::arena_alloc(units.len() * 2, 8);
    // SAFETY: both buffers were just allocated with these sizes.
    unsafe {
        core::ptr::copy_nonoverlapping(command_line.as_ptr(), a as *mut u8, command_line.len());
        core::ptr::copy_nonoverlapping(units.as_ptr(), w as *mut u16, units.len());
    }
    *PROCESS.lock().unwrap() =
        Some(ProcessState { exe_path: exe_path.to_string(), command_line_a: a, command_line_w: w, std, env });
}

/// The process heap handle (an opaque value; all heaps share one allocator).
pub fn process_heap() -> u64 {
    let h = HEAP.load(Ordering::Relaxed);
    if h != 0 {
        return h;
    }
    let h = mem::arena_alloc(16, 16);
    HEAP.store(h, Ordering::Relaxed);
    h
}

fn cwd_win() -> String {
    os_std::env::current_dir().map(|p| path::to_windows(p.as_str())).unwrap_or_else(|_| String::from("C:\\"))
}

/// Windows path → canonical OSyx path and whether it exists.
fn locate(win: &str) -> Result<(String, bool), u32> {
    let p = path::to_osyx(win, &cwd_win())?;
    path::resolve(&p)
}

// ---- errors ----------------------------------------------------------------------

extern "win64" fn GetLastError() -> u32 {
    teb::last_error()
}

extern "win64" fn SetLastError(e: u32) {
    teb::set_last_error(e)
}

extern "win64" fn IsDebuggerPresent() -> Bool {
    FALSE
}

extern "win64" fn OutputDebugStringA(s: *const u8) {
    // SAFETY: the caller passes a NUL-terminated string.
    if let Some(s) = unsafe { sa(s) } {
        os_std::eprintln!("[debug] {}", s.trim_end());
    }
}

extern "win64" fn OutputDebugStringW(s: *const u16) {
    // SAFETY: as above.
    if let Some(s) = unsafe { sw(s) } {
        os_std::eprintln!("[debug] {}", s.trim_end());
    }
}

extern "win64" fn SetUnhandledExceptionFilter(f: u64) -> u64 {
    crate::exceptions::set_filter(f)
}

use crate::exceptions::RaiseException;

// ---- console and standard handles ----------------------------------------------

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;

extern "win64" fn GetStdHandle(which: u32) -> u64 {
    let i = match which {
        STD_INPUT_HANDLE => 0,
        STD_OUTPUT_HANDLE => 1,
        STD_ERROR_HANDLE => 2,
        _ => return fail(ERROR_INVALID_HANDLE, INVALID_HANDLE_VALUE),
    };
    with_process(|p| p.std[i])
}

extern "win64" fn SetStdHandle(which: u32, h: u64) -> Bool {
    let i = match which {
        STD_INPUT_HANDLE => 0,
        STD_OUTPUT_HANDLE => 1,
        STD_ERROR_HANDLE => 2,
        _ => return fail(ERROR_INVALID_HANDLE, FALSE),
    };
    with_process(|p| p.std[i] = h);
    TRUE
}

extern "win64" fn GetConsoleMode(h: u64, mode: *mut u32) -> Bool {
    match objects::get(h) {
        Some(Object::Console(_)) if !mode.is_null() => {
            // SAFETY: the caller's out pointer.
            unsafe { *mode = 3 }; // ENABLE_PROCESSED_INPUT/OUTPUT
            TRUE
        }
        _ => fail(ERROR_INVALID_HANDLE, FALSE),
    }
}

extern "win64" fn SetConsoleMode(h: u64, _mode: u32) -> Bool {
    b(matches!(objects::get(h), Some(Object::Console(_))))
}

extern "win64" fn GetConsoleCP() -> u32 {
    65001
}

extern "win64" fn GetConsoleOutputCP() -> u32 {
    65001
}

/// The OSyx console is UTF-8 only.
extern "win64" fn SetConsoleOutputCP(cp: u32) -> Bool {
    if cp == 65001 {
        TRUE
    } else {
        fail(ERROR_NOT_SUPPORTED, FALSE)
    }
}

fn console_write(c: Console, bytes: &[u8]) -> Result<usize, u32> {
    let fd = match c {
        Console::Out => 1,
        Console::Err => 2,
        Console::In => return Err(ERROR_ACCESS_DENIED),
    };
    // Windows programs end lines with CRLF; the OSyx console wants LF.
    let text: Vec<u8> = bytes.iter().copied().filter(|&b| b != b'\r').collect();
    os_sys::write(fd, &text).map(|_| bytes.len()).map_err(errno_to_win32)
}

extern "win64" fn WriteConsoleA(h: u64, buf: *const u8, n: u32, written: *mut u32, _r: u64) -> Bool {
    let Some(Object::Console(c)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    // SAFETY: the caller's buffer of `n` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(buf, n as usize) };
    match console_write(c, bytes) {
        Ok(k) => {
            if !written.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *written = k as u32 };
            }
            TRUE
        }
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn WriteConsoleW(h: u64, buf: *const u16, n: u32, written: *mut u32, _r: u64) -> Bool {
    let Some(Object::Console(c)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    // SAFETY: the caller's buffer of `n` UTF-16 units.
    let units = unsafe { core::slice::from_raw_parts(buf, n as usize) };
    let s: String = char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    match console_write(c, s.as_bytes()) {
        Ok(_) => {
            if !written.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *written = n };
            }
            TRUE
        }
        Err(e) => fail(e, FALSE),
    }
}

// ---- files --------------------------------------------------------------------------

const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
pub const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

pub fn attributes(st: &osyx_abi::FileStat) -> u32 {
    let mut a = if st.kind == osyx_abi::KIND_DIR { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_ARCHIVE };
    if st.mode & 0o200 == 0 {
        a |= FILE_ATTRIBUTE_READONLY;
    }
    a
}

/// CreateFile core shared with NtCreateFile. Returns the handle and
/// whether an existing file was opened (for ERROR_ALREADY_EXISTS).
pub fn create_file(name: &str, access: u32, share: u32, disposition: u32, flags: u32) -> Result<(u64, bool), u32> {
    match path::special(name) {
        Some(path::Special::Null) => return Ok((objects::insert(Object::Null), true)),
        Some(path::Special::ConsoleOut) => return Ok((objects::insert(Object::Console(Console::Out)), true)),
        Some(path::Special::ConsoleIn) => return Ok((objects::insert(Object::Console(Console::In)), true)),
        None => {}
    }
    if !(CREATE_NEW..=TRUNCATE_EXISTING).contains(&disposition) {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let (p, exists) = locate(name)?;
    let rights = objects::rights(access);
    if exists {
        if disposition == CREATE_NEW {
            return Err(ERROR_FILE_EXISTS);
        }
    } else if disposition == OPEN_EXISTING || disposition == TRUNCATE_EXISTING {
        return Err(ERROR_FILE_NOT_FOUND);
    }
    let st = if exists { Some(os_sys::stat(&p).map_err(errno_to_win32)?) } else { None };
    let is_dir = st.is_some_and(|s| s.kind == osyx_abi::KIND_DIR);
    if is_dir && flags & FILE_FLAG_BACKUP_SEMANTICS == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    // Sharing is checked before anything changes on disk.
    let pre = match st {
        Some(s) => Some(objects::share_open((s.dev, s.ino), rights, share)?),
        None => None,
    };
    let mut oflags = 0;
    if rights.0 || !rights.1 {
        oflags |= osyx_abi::OPEN_READ;
    }
    if rights.1 && !is_dir {
        oflags |= osyx_abi::OPEN_WRITE;
    }
    if is_dir {
        oflags = osyx_abi::OPEN_READ | osyx_abi::OPEN_DIRECTORY;
    }
    if !exists {
        oflags |= osyx_abi::OPEN_CREATE | osyx_abi::OPEN_EXCLUSIVE | osyx_abi::OPEN_WRITE;
    } else if disposition == CREATE_ALWAYS || disposition == TRUNCATE_EXISTING {
        oflags |= osyx_abi::OPEN_TRUNCATE | osyx_abi::OPEN_WRITE;
    }
    let fd = match os_sys::open(&p, oflags, 0o644) {
        Ok(fd) => fd,
        Err(e) => {
            if let (Some(id), Some(s)) = (pre, st) {
                // Undo the sharing record of the failed open.
                drop(FileObj::new(u64::MAX, String::new(), rights, (s.dev, s.ino), id, false, false, false));
            }
            return Err(errno_to_win32(e));
        }
    };
    let (key, id) = match (pre, st) {
        (Some(id), Some(s)) => ((s.dev, s.ino), id),
        _ => {
            let s = os_sys::fstat(fd).map_err(errno_to_win32)?;
            ((s.dev, s.ino), objects::share_open((s.dev, s.ino), rights, share)?)
        }
    };
    let f = FileObj::new(
        fd,
        p,
        rights,
        key,
        id,
        is_dir,
        flags & FILE_FLAG_DELETE_ON_CLOSE != 0,
        access & objects::FILE_APPEND_DATA != 0 && access & objects::FILE_WRITE_DATA == 0,
    );
    Ok((objects::insert(Object::File(Arc::new(f))), exists))
}

fn create_file_common(name: Option<String>, access: u32, share: u32, disposition: u32, flags: u32) -> u64 {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE) };
    match create_file(&name, access, share, disposition, flags) {
        Ok((h, existed)) => {
            let opening = disposition == CREATE_ALWAYS || disposition == OPEN_ALWAYS;
            teb::set_last_error(if existed && opening { ERROR_ALREADY_EXISTS } else { ERROR_SUCCESS });
            h
        }
        Err(e) => fail(e, INVALID_HANDLE_VALUE),
    }
}

extern "win64" fn CreateFileA(
    name: *const u8,
    access: u32,
    share: u32,
    attributes: u64,
    disposition: u32,
    flags: u32,
    _template: u64,
) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,INVALID_HANDLE_VALUE); }
    // SAFETY: the caller passes a NUL-terminated name.
    create_file_common(unsafe { sa(name) }, access, share, disposition, flags)
}

extern "win64" fn CreateFileW(
    name: *const u16,
    access: u32,
    share: u32,
    attributes: u64,
    disposition: u32,
    flags: u32,
    _template: u64,
) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,INVALID_HANDLE_VALUE); }
    // SAFETY: as above.
    create_file_common(unsafe { sw(name) }, access, share, disposition, flags)
}

/// OVERLAPPED: Internal, InternalHigh, Offset, OffsetHigh, hEvent.
fn overlapped_offset(ov: *const u8) -> Option<u64> {
    if ov.is_null() {
        return None;
    }
    // SAFETY: the caller's OVERLAPPED structure.
    unsafe {
        Some(
            core::ptr::read_unaligned(ov.add(16) as *const u32) as u64
                | (core::ptr::read_unaligned(ov.add(20) as *const u32) as u64) << 32,
        )
    }
}

pub fn read_handle(h: u64, buf: *mut u8, n: u32, offset: Option<u64>) -> Result<u32, u32> {
    match objects::get(h) {
        Some(Object::Console(Console::In)) => {
            os_sys::read_raw(0, buf as usize, n as usize).map(|k| k as u32).map_err(errno_to_win32)
        }
        Some(Object::Null) => Ok(0),
        Some(Object::File(f)) => {
            if !f.access.0 {
                return Err(ERROR_ACCESS_DENIED);
            }
            if let Some(o) = offset {
                os_sys::seek(f.fd, o as i64, osyx_abi::SEEK_SET).map_err(errno_to_win32)?;
            }
            os_sys::read_raw(f.fd, buf as usize, n as usize).map(|k| k as u32).map_err(errno_to_win32)
        }
        _ => Err(ERROR_INVALID_HANDLE),
    }
}

pub fn write_handle(h: u64, buf: *const u8, n: u32, offset: Option<u64>) -> Result<u32, u32> {
    match objects::get(h) {
        Some(Object::Console(c)) => {
            // SAFETY: the caller's buffer of `n` bytes.
            let bytes = unsafe { core::slice::from_raw_parts(buf, n as usize) };
            console_write(c, bytes).map(|k| k as u32)
        }
        Some(Object::Null) => Ok(n),
        Some(Object::File(f)) => {
            if !f.access.1 {
                return Err(ERROR_ACCESS_DENIED);
            }
            if f.append {
                os_sys::seek(f.fd, 0, osyx_abi::SEEK_END).map_err(errno_to_win32)?;
            } else if let Some(o) = offset {
                os_sys::seek(f.fd, o as i64, osyx_abi::SEEK_SET).map_err(errno_to_win32)?;
            }
            os_sys::write_raw(f.fd, buf as usize, n as usize).map(|k| k as u32).map_err(errno_to_win32)
        }
        _ => Err(ERROR_INVALID_HANDLE),
    }
}

extern "win64" fn ReadFile(h: u64, buf: *mut u8, n: u32, read: *mut u32, ov: *const u8) -> Bool {
    match read_handle(h, buf, n, overlapped_offset(ov)) {
        Ok(k) => {
            if !read.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *read = k };
            }
            TRUE
        }
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn WriteFile(h: u64, buf: *const u8, n: u32, written: *mut u32, ov: *const u8) -> Bool {
    match write_handle(h, buf, n, overlapped_offset(ov)) {
        Ok(k) => {
            if !written.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *written = k };
            }
            TRUE
        }
        Err(e) => fail(e, FALSE),
    }
}

fn file_stat(h: u64) -> Result<osyx_abi::FileStat, u32> {
    let f = objects::file(h)?;
    os_sys::fstat(f.fd).map_err(errno_to_win32)
}

extern "win64" fn GetFileSize(h: u64, high: *mut u32) -> u32 {
    match file_stat(h) {
        Ok(s) => {
            if !high.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *high = (s.size >> 32) as u32 };
            }
            teb::set_last_error(ERROR_SUCCESS);
            s.size as u32
        }
        Err(e) => fail(e, u32::MAX),
    }
}

extern "win64" fn GetFileSizeEx(h: u64, size: *mut i64) -> Bool {
    match file_stat(h) {
        Ok(s) if !size.is_null() => {
            // SAFETY: the caller's out pointer.
            unsafe { *size = s.size as i64 };
            TRUE
        }
        Ok(_) => fail(ERROR_INVALID_PARAMETER, FALSE),
        Err(e) => fail(e, FALSE),
    }
}

fn seek_handle(h: u64, offset: i64, method: u32) -> Result<u64, u32> {
    let f = objects::file(h)?;
    let whence = match method {
        0 => osyx_abi::SEEK_SET,
        1 => osyx_abi::SEEK_CUR,
        2 => osyx_abi::SEEK_END,
        _ => return Err(ERROR_INVALID_PARAMETER),
    };
    os_sys::seek(f.fd, offset, whence).map_err(|e| if e == Errno::InvalidArgument { 131 } else { errno_to_win32(e) })
    // ERROR_NEGATIVE_SEEK
}

extern "win64" fn SetFilePointer(h: u64, lo: i32, hi: *mut i32, method: u32) -> u32 {
    let off = if hi.is_null() {
        lo as i64
    } else {
        // SAFETY: the caller's in/out pointer.
        ((unsafe { *hi } as i64) << 32) | (lo as u32 as i64)
    };
    match seek_handle(h, off, method) {
        Ok(p) => {
            if !hi.is_null() {
                // SAFETY: as above.
                unsafe { *hi = (p >> 32) as i32 };
            }
            teb::set_last_error(ERROR_SUCCESS);
            p as u32
        }
        Err(e) => fail(e, u32::MAX),
    }
}

extern "win64" fn SetFilePointerEx(h: u64, off: i64, newpos: *mut i64, method: u32) -> Bool {
    match seek_handle(h, off, method) {
        Ok(p) => {
            if !newpos.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *newpos = p as i64 };
            }
            TRUE
        }
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn SetEndOfFile(h: u64) -> Bool {
    let r = (|| {
        let f = objects::file(h)?;
        let pos = os_sys::seek(f.fd, 0, osyx_abi::SEEK_CUR).map_err(errno_to_win32)?;
        os_sys::ftruncate(f.fd, pos).map_err(errno_to_win32)
    })();
    r.map_or_else(|e| fail(e, FALSE), |_| TRUE)
}

extern "win64" fn FlushFileBuffers(h: u64) -> Bool {
    match objects::get(h) {
        Some(Object::File(f)) => os_sys::fsync(f.fd).map_or_else(|e| fail(errno_to_win32(e), FALSE), |_| TRUE),
        Some(Object::Console(_)) => TRUE,
        _ => fail(ERROR_INVALID_HANDLE, FALSE),
    }
}

const FILE_TYPE_DISK: u32 = 1;
const FILE_TYPE_CHAR: u32 = 2;

extern "win64" fn GetFileType(h: u64) -> u32 {
    match objects::get(h) {
        Some(Object::File(_)) => FILE_TYPE_DISK,
        Some(Object::Console(_)) | Some(Object::Null) => FILE_TYPE_CHAR,
        _ => fail(ERROR_INVALID_HANDLE, 0),
    }
}

/// Nanoseconds since 1970 → FILETIME (100 ns units since 1601).
pub fn filetime(ns: i64) -> u64 {
    (ns / 100 + 116_444_736_000_000_000) as u64
}

extern "win64" fn GetFileInformationByHandle(h: u64, out: *mut u8) -> Bool {
    let s = match file_stat(h) {
        Ok(s) => s,
        Err(e) => return fail(e, FALSE),
    };
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let v: [u32; 13] = [
        attributes(&s),
        filetime(s.ctime_ns) as u32,
        (filetime(s.ctime_ns) >> 32) as u32,
        filetime(s.atime_ns) as u32,
        (filetime(s.atime_ns) >> 32) as u32,
        filetime(s.mtime_ns) as u32,
        (filetime(s.mtime_ns) >> 32) as u32,
        s.dev as u32,
        (s.size >> 32) as u32,
        s.size as u32,
        s.nlink,
        (s.ino >> 32) as u32,
        s.ino as u32,
    ];
    // SAFETY: BY_HANDLE_FILE_INFORMATION is 52 bytes of DWORDs.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr() as *const u8, out, 52) };
    TRUE
}

fn get_attributes(name: Option<String>) -> u32 {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, INVALID_FILE_ATTRIBUTES) };
    match locate(&name) {
        Ok((p, true)) => match os_sys::stat(&p) {
            Ok(s) => attributes(&s),
            Err(e) => fail(errno_to_win32(e), INVALID_FILE_ATTRIBUTES),
        },
        Ok((_, false)) => fail(ERROR_FILE_NOT_FOUND, INVALID_FILE_ATTRIBUTES),
        Err(e) => fail(e, INVALID_FILE_ATTRIBUTES),
    }
}

extern "win64" fn GetFileAttributesA(name: *const u8) -> u32 {
    // SAFETY: NUL-terminated name.
    get_attributes(unsafe { sa(name) })
}

extern "win64" fn GetFileAttributesW(name: *const u16) -> u32 {
    // SAFETY: NUL-terminated name.
    get_attributes(unsafe { sw(name) })
}

fn create_directory(name: Option<String>) -> Bool {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    match locate(&name) {
        Ok((_, true)) => fail(ERROR_ALREADY_EXISTS, FALSE),
        Ok((p, false)) => os_sys::mkdir(&p, 0o755).map_or_else(|e| fail(errno_to_win32(e), FALSE), |_| TRUE),
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn CreateDirectoryA(name: *const u8, attributes: u64) -> Bool {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,FALSE); }
    // SAFETY: NUL-terminated name.
    create_directory(unsafe { sa(name) })
}

extern "win64" fn CreateDirectoryW(name: *const u16, attributes: u64) -> Bool {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,FALSE); }
    // SAFETY: NUL-terminated name.
    create_directory(unsafe { sw(name) })
}

fn remove_directory(name: Option<String>) -> Bool {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    match locate(&name) {
        Ok((p, true)) => os_sys::rmdir(&p).map_or_else(|e| fail(errno_to_win32(e), FALSE), |_| TRUE),
        Ok((_, false)) => fail(ERROR_FILE_NOT_FOUND, FALSE),
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn RemoveDirectoryA(name: *const u8) -> Bool {
    // SAFETY: NUL-terminated name.
    remove_directory(unsafe { sa(name) })
}

extern "win64" fn RemoveDirectoryW(name: *const u16) -> Bool {
    // SAFETY: NUL-terminated name.
    remove_directory(unsafe { sw(name) })
}

fn delete_file(name: Option<String>) -> Bool {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    let p = match locate(&name) {
        Ok((p, true)) => p,
        Ok((_, false)) => return fail(ERROR_FILE_NOT_FOUND, FALSE),
        Err(e) => return fail(e, FALSE),
    };
    let s = match os_sys::lstat(&p) {
        Ok(s) => s,
        Err(e) => return fail(errno_to_win32(e), FALSE),
    };
    if s.kind == osyx_abi::KIND_DIR {
        return fail(ERROR_ACCESS_DENIED, FALSE);
    }
    if objects::delete_blocked((s.dev, s.ino)) {
        return fail(ERROR_SHARING_VIOLATION, FALSE);
    }
    os_sys::unlink(&p).map_or_else(|e| fail(errno_to_win32(e), FALSE), |_| TRUE)
}

extern "win64" fn DeleteFileA(name: *const u8) -> Bool {
    // SAFETY: NUL-terminated name.
    delete_file(unsafe { sa(name) })
}

extern "win64" fn DeleteFileW(name: *const u16) -> Bool {
    // SAFETY: NUL-terminated name.
    delete_file(unsafe { sw(name) })
}

const MOVEFILE_REPLACE_EXISTING: u32 = 1;

fn move_file(from: Option<String>, to: Option<String>, flags: u32) -> Bool {
    let (Some(from), Some(to)) = (from, to) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    let src = match locate(&from) {
        Ok((p, true)) => p,
        Ok((_, false)) => return fail(ERROR_FILE_NOT_FOUND, FALSE),
        Err(e) => return fail(e, FALSE),
    };
    let (dst, exists) = match locate(&to) {
        Ok(x) => x,
        Err(e) => return fail(e, FALSE),
    };
    if exists && flags & MOVEFILE_REPLACE_EXISTING == 0 {
        return fail(ERROR_ALREADY_EXISTS, FALSE);
    }
    if let Ok(s) = os_sys::lstat(&src) {
        if objects::delete_blocked((s.dev, s.ino)) {
            return fail(ERROR_SHARING_VIOLATION, FALSE);
        }
    }
    os_sys::rename(&src, &dst).map_or_else(
        |e| fail(if e == Errno::CrossDevice { ERROR_NOT_SAME_DEVICE } else { errno_to_win32(e) }, FALSE),
        |_| TRUE,
    )
}

extern "win64" fn MoveFileA(from: *const u8, to: *const u8) -> Bool {
    // SAFETY: NUL-terminated names.
    move_file(unsafe { sa(from) }, unsafe { sa(to) }, 0)
}

extern "win64" fn MoveFileW(from: *const u16, to: *const u16) -> Bool {
    // SAFETY: NUL-terminated names.
    move_file(unsafe { sw(from) }, unsafe { sw(to) }, 0)
}

extern "win64" fn MoveFileExA(from: *const u8, to: *const u8, flags: u32) -> Bool {
    // SAFETY: NUL-terminated names.
    move_file(unsafe { sa(from) }, unsafe { sa(to) }, flags)
}

extern "win64" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> Bool {
    // SAFETY: NUL-terminated names.
    move_file(unsafe { sw(from) }, unsafe { sw(to) }, flags)
}

extern "win64" fn GetCurrentDirectoryA(size: u32, buf: *mut u8) -> u32 {
    out_a(&cwd_win(), buf, size)
}

extern "win64" fn GetCurrentDirectoryW(size: u32, buf: *mut u16) -> u32 {
    out_w(&cwd_win(), buf, size)
}

fn set_current_directory(name: Option<String>) -> Bool {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    match locate(&name) {
        Ok((p, true)) => os_std::env::set_current_dir(&p).map_or_else(|_| fail(ERROR_DIRECTORY, FALSE), |_| TRUE),
        Ok((_, false)) => fail(ERROR_FILE_NOT_FOUND, FALSE),
        Err(e) => fail(e, FALSE),
    }
}

extern "win64" fn SetCurrentDirectoryA(name: *const u8) -> Bool {
    // SAFETY: NUL-terminated name.
    set_current_directory(unsafe { sa(name) })
}

extern "win64" fn SetCurrentDirectoryW(name: *const u16) -> Bool {
    // SAFETY: NUL-terminated name.
    set_current_directory(unsafe { sw(name) })
}

fn full_path(name: Option<String>) -> Result<String, u32> {
    let name = name.ok_or(ERROR_INVALID_PARAMETER)?;
    Ok(path::to_windows(&path::to_osyx(&name, &cwd_win())?))
}

extern "win64" fn GetFullPathNameA(name: *const u8, size: u32, buf: *mut u8, file_part: *mut *mut u8) -> u32 {
    // SAFETY: NUL-terminated name.
    match full_path(unsafe { sa(name) }) {
        Ok(p) => {
            let n = out_a(&p, buf, size);
            if !file_part.is_null() && n < size {
                let i = p.rfind('\\').map_or(0, |i| i + 1);
                // SAFETY: `buf` holds `p` (it fit); the caller's out pointer.
                unsafe { *file_part = buf.add(i) };
            }
            n
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn GetFullPathNameW(name: *const u16, size: u32, buf: *mut u16, file_part: *mut *mut u16) -> u32 {
    // SAFETY: NUL-terminated name.
    match full_path(unsafe { sw(name) }) {
        Ok(p) => {
            let n = out_w(&p, buf, size);
            if !file_part.is_null() && n < size {
                let i = p.rfind('\\').map_or(0, |i| i + 1);
                // SAFETY: as above (ASCII-only prefix positions match).
                unsafe { *file_part = buf.add(p[..i].encode_utf16().count()) };
            }
            n
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn GetTempPathA(size: u32, buf: *mut u8) -> u32 {
    out_a("C:\\tmp\\", buf, size)
}

extern "win64" fn GetTempPathW(size: u32, buf: *mut u16) -> u32 {
    out_w("C:\\tmp\\", buf, size)
}

// ---- handles ------------------------------------------------------------------------

extern "win64" fn CloseHandle(h: u64) -> Bool {
    if h == objects::CURRENT_PROCESS || h == objects::CURRENT_THREAD {
        return TRUE;
    }
    if objects::close(h) {
        TRUE
    } else {
        fail(ERROR_INVALID_HANDLE, FALSE)
    }
}

extern "win64" fn DuplicateHandle(
    src_process: u64,
    src: u64,
    dst_process: u64,
    dst: *mut u64,
    access: u32,
    inherit: Bool,
    options: u32,
) -> Bool {
    if src_process != objects::CURRENT_PROCESS || dst_process != objects::CURRENT_PROCESS {
        return fail(ERROR_NOT_SUPPORTED, FALSE); // no other processes' handles
    }
    let Some(mut o) = objects::get(src) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    if let Object::Token(token) = &o {
        if inherit != 0 { return fail(ERROR_NOT_SUPPORTED, FALSE); }
        if options & !3 != 0 || dst.is_null() { return fail(ERROR_INVALID_PARAMETER, FALSE); }
        let rights = if options & 2 != 0 { token.access } else {
            match crate::security::token_access(access) { Ok(a)=>a,Err(e)=>return fail(e,FALSE) }
        };
        if rights & !token.access != 0 { return fail(ERROR_ACCESS_DENIED,FALSE); }
        o=Object::Token(Arc::new(crate::security::Token {access:rights,..(**token).clone()}));
    }
    let h = objects::insert(o);
    if options & 1 != 0 {
        objects::close(src); // DUPLICATE_CLOSE_SOURCE
    }
    if !dst.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *dst = h };
    }
    TRUE
}

// ---- processes and threads -----------------------------------------------------------

extern "win64" fn GetCurrentProcess() -> u64 {
    objects::CURRENT_PROCESS
}

extern "win64" fn GetCurrentThread() -> u64 {
    objects::CURRENT_THREAD
}

/// This process's id.
pub fn pid() -> u64 {
    os_std::process::id().unwrap_or(0)
}

extern "win64" fn GetCurrentProcessId() -> u32 {
    pid() as u32
}

extern "win64" fn GetCurrentThreadId() -> u32 {
    os_sys::gettid() as u32
}

/// Ends the process: DLL_PROCESS_DETACH notifications, then exit.
pub extern "win64" fn ExitProcess(code: u32) -> ! {
    loader::notify_process_detach();
    os_std::process::exit(code as i32)
}

extern "win64" fn TerminateProcess(h: u64, code: u32) -> Bool {
    if h == objects::CURRENT_PROCESS {
        os_std::process::exit(code as i32);
    }
    fail(ERROR_ACCESS_DENIED, FALSE)
}

extern "win64" fn GetExitCodeProcess(h: u64, code: *mut u32) -> Bool {
    if h == objects::CURRENT_PROCESS && !code.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *code = 259 }; // STILL_ACTIVE
        return TRUE;
    }
    fail(ERROR_INVALID_HANDLE, FALSE)
}

const CREATE_SUSPENDED: u32 = 4;
const STACK_SIZE_PARAM_IS_A_RESERVATION: u32 = 0x10000;
pub const DEFAULT_STACK: usize = 1 << 20;

/// Starts a Windows thread running `start(param)`; returns its object.
pub fn spawn_thread(start: u64, param: u64, stack_size: usize, suspended: bool) -> Result<Arc<ThreadObj>, u32> {
    let obj = Arc::new(ThreadObj {
        tid: AtomicU32::new(0),
        done: AtomicBool::new(false),
        exit_code: AtomicU32::new(259),
        suspended: AtomicU32::new(suspended as u32),
        teb: AtomicU64::new(0),
    });
    let size = stack_size.clamp(64 << 10, 64 << 20).next_multiple_of(mem::GRANULARITY);
    let me = obj.clone();
    let pid = pid();
    os_std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let stack = match mem::alloc_pages(size, mem::Prot::ReadWrite) {
                Some(s) => s,
                None => {
                    me.exit_code.store(ERROR_NOT_ENOUGH_MEMORY, Ordering::SeqCst);
                    me.tid.store(u32::MAX, Ordering::SeqCst);
                    me.done.store(true, Ordering::SeqCst);
                    objects::signal_all();
                    return;
                }
            };
            let tid = os_sys::gettid();
            let t = teb::create_teb(stack + size as u64, stack, pid, tid);
            teb::install(t);
            me.teb.store(t, Ordering::SeqCst);
            loader::setup_thread_tls(t);
            me.tid.store(tid as u32, Ordering::SeqCst);
            while me.suspended.load(Ordering::SeqCst) > 0 {
                let _ = os_sys::futex_wait(me.suspended.as_ptr(), me.suspended.load(Ordering::SeqCst), 0);
            }
            loader::notify_threads(loader::DLL_THREAD_ATTACH);
            let code = loader::run_on_stack(start, param, stack + size as u64) as u32;
            loader::notify_threads(loader::DLL_THREAD_DETACH);
            let _ = os_sys::set_gs_base(0);
            mem::free_pages(stack, size);
            mem::free_pages(t, teb::TEB_SIZE);
            me.exit_code.store(code, Ordering::SeqCst);
            me.done.store(true, Ordering::SeqCst);
            objects::signal_all();
        })
        .map_err(|_| ERROR_NOT_ENOUGH_MEMORY)?;
    while obj.tid.load(Ordering::SeqCst) == 0 {
        os_std::thread::yield_now();
    }
    if obj.tid.load(Ordering::SeqCst) == u32::MAX {
        return Err(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(obj)
}

extern "win64" fn CreateThread(attributes: u64, stack: usize, start: u64, param: u64, flags: u32, tid: *mut u32) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    if start == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let _ = flags & STACK_SIZE_PARAM_IS_A_RESERVATION;
    let size = if stack == 0 { DEFAULT_STACK } else { stack };
    match spawn_thread(start, param, size, flags & CREATE_SUSPENDED != 0) {
        Ok(t) => {
            if !tid.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *tid = t.tid.load(Ordering::SeqCst) };
            }
            objects::insert(Object::Thread(t))
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn ResumeThread(h: u64) -> u32 {
    match objects::get(h) {
        Some(Object::Thread(t)) => {
            let prev = t.suspended.load(Ordering::SeqCst);
            if prev > 0 {
                t.suspended.fetch_sub(1, Ordering::SeqCst);
                os_sys::futex_wake(t.suspended.as_ptr(), u32::MAX);
            }
            prev
        }
        _ => fail(ERROR_INVALID_HANDLE, u32::MAX),
    }
}

/// Only threads created suspended can be held; suspending a running
/// thread is not supported by OSyx.
extern "win64" fn SuspendThread(_h: u64) -> u32 {
    fail(ERROR_NOT_SUPPORTED, u32::MAX)
}

extern "win64" fn ExitThread(code: u32) -> ! {
    loader::exit_thread(code)
}

extern "win64" fn GetExitCodeThread(h: u64, code: *mut u32) -> Bool {
    match objects::get(h) {
        Some(Object::Thread(t)) if !code.is_null() => {
            // SAFETY: the caller's out pointer.
            unsafe { *code = if t.done.load(Ordering::SeqCst) { t.exit_code.load(Ordering::SeqCst) } else { 259 } };
            TRUE
        }
        _ => fail(ERROR_INVALID_HANDLE, FALSE),
    }
}

extern "win64" fn GetThreadId(h: u64) -> u32 {
    match objects::get(h) {
        Some(Object::Thread(t)) => t.tid.load(Ordering::SeqCst),
        _ if h == objects::CURRENT_THREAD => os_sys::gettid() as u32,
        _ => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn Sleep(ms: u32) {
    if ms == 0 {
        os_std::thread::yield_now();
    } else if ms == u32::MAX {
        loop {
            os_std::thread::sleep(core::time::Duration::from_secs(3600));
        }
    } else {
        os_std::thread::sleep(core::time::Duration::from_millis(ms as u64));
    }
}

extern "win64" fn SleepEx(ms: u32, _alertable: Bool) -> u32 {
    Sleep(ms);
    0
}

extern "win64" fn SwitchToThread() -> Bool {
    os_std::thread::yield_now();
    TRUE
}

extern "win64" fn GetCommandLineA() -> u64 {
    with_process(|p| p.command_line_a)
}

extern "win64" fn GetCommandLineW() -> u64 {
    with_process(|p| p.command_line_w)
}

/// STARTUPINFO: everything zero except `cb` (the default for a console process).
extern "win64" fn GetStartupInfoA(si: *mut u8) {
    if !si.is_null() {
        // SAFETY: STARTUPINFOA is 104 bytes.
        unsafe {
            core::ptr::write_bytes(si, 0, 104);
            *(si as *mut u32) = 104;
        }
    }
}

extern "win64" fn GetStartupInfoW(si: *mut u8) {
    GetStartupInfoA(si)
}

// ---- environment ---------------------------------------------------------------------

fn env_get(name: &str) -> Option<String> {
    with_process(|p| p.env.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()))
}

fn env_set(name: &str, value: Option<String>) {
    with_process(|p| {
        p.env.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        if let Some(v) = value {
            p.env.push((name.to_string(), v));
        }
    })
}

extern "win64" fn GetEnvironmentVariableA(name: *const u8, buf: *mut u8, size: u32) -> u32 {
    // SAFETY: NUL-terminated name.
    match unsafe { sa(name) }.and_then(|n| env_get(&n)) {
        Some(v) => out_a(&v, buf, size),
        None => fail(ERROR_ENVVAR_NOT_FOUND, 0),
    }
}

extern "win64" fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, size: u32) -> u32 {
    // SAFETY: NUL-terminated name.
    match unsafe { sw(name) }.and_then(|n| env_get(&n)) {
        Some(v) => out_w(&v, buf, size),
        None => fail(ERROR_ENVVAR_NOT_FOUND, 0),
    }
}

extern "win64" fn SetEnvironmentVariableA(name: *const u8, value: *const u8) -> Bool {
    // SAFETY: NUL-terminated strings (value may be NULL: delete).
    let (Some(n), v) = (unsafe { sa(name) }, unsafe { sa(value) }) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    env_set(&n, v);
    TRUE
}

extern "win64" fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> Bool {
    // SAFETY: as above.
    let (Some(n), v) = (unsafe { sw(name) }, unsafe { sw(value) }) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    env_set(&n, v);
    TRUE
}

/// A NAME=value\0...\0\0 block in UTF-16, freed by FreeEnvironmentStringsW.
extern "win64" fn GetEnvironmentStringsW() -> u64 {
    let mut units: Vec<u16> = Vec::new();
    with_process(|p| {
        for (k, v) in &p.env {
            units.extend(k.encode_utf16());
            units.push(b'=' as u16);
            units.extend(v.encode_utf16());
            units.push(0);
        }
    });
    units.push(0);
    heap_alloc(units.len() * 2, false).map_or(0, |p| {
        // SAFETY: `p` holds `units.len()` u16s.
        unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), p as *mut u16, units.len()) };
        p
    })
}

extern "win64" fn FreeEnvironmentStringsW(p: u64) -> Bool {
    b(heap_free(p))
}

// ---- synchronisation ---------------------------------------------------------------

// CRITICAL_SECTION (40 bytes): DebugInfo, LockCount (i32, the futex word
// here: 0 free, 1 held, 2 held with waiters), RecursionCount, OwningThread,
// LockSemaphore, SpinCount.
const CS_LOCK: usize = 8;
const CS_RECURSION: usize = 12;
const CS_OWNER: usize = 16;

fn cs_word(cs: u64) -> &'static AtomicU32 {
    // SAFETY: the caller's CRITICAL_SECTION outlives every use (Windows
    // contract); the field is 4-byte aligned in the structure.
    unsafe { &*((cs as usize + CS_LOCK) as *const AtomicU32) }
}

extern "win64" fn InitializeCriticalSection(cs: u64) {
    // SAFETY: the caller's 40-byte CRITICAL_SECTION.
    unsafe { core::ptr::write_bytes(cs as *mut u8, 0, 40) };
}

extern "win64" fn InitializeCriticalSectionAndSpinCount(cs: u64, _spin: u32) -> Bool {
    InitializeCriticalSection(cs);
    TRUE
}

extern "win64" fn InitializeCriticalSectionEx(cs: u64, _spin: u32, _flags: u32) -> Bool {
    InitializeCriticalSection(cs);
    TRUE
}

extern "win64" fn DeleteCriticalSection(_cs: u64) {}

extern "win64" fn EnterCriticalSection(cs: u64) {
    let me = os_sys::gettid();
    // SAFETY: the caller's CRITICAL_SECTION.
    unsafe {
        if teb::get::<u64>(cs, CS_OWNER) == me {
            teb::put(cs, CS_RECURSION, teb::get::<i32>(cs, CS_RECURSION) + 1);
            return;
        }
    }
    let w = cs_word(cs);
    if w.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        while w.swap(2, Ordering::Acquire) != 0 {
            let _ = os_sys::futex_wait(w.as_ptr(), 2, 0);
        }
    }
    // SAFETY: as above; we own the section now.
    unsafe {
        teb::put(cs, CS_OWNER, me);
        teb::put(cs, CS_RECURSION, 1i32);
    }
}

extern "win64" fn TryEnterCriticalSection(cs: u64) -> Bool {
    let me = os_sys::gettid();
    // SAFETY: the caller's CRITICAL_SECTION.
    unsafe {
        if teb::get::<u64>(cs, CS_OWNER) == me {
            teb::put(cs, CS_RECURSION, teb::get::<i32>(cs, CS_RECURSION) + 1);
            return TRUE;
        }
    }
    if cs_word(cs).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        // SAFETY: as above.
        unsafe {
            teb::put(cs, CS_OWNER, me);
            teb::put(cs, CS_RECURSION, 1i32);
        }
        TRUE
    } else {
        FALSE
    }
}

extern "win64" fn LeaveCriticalSection(cs: u64) {
    // SAFETY: the caller's CRITICAL_SECTION, held by this thread.
    unsafe {
        let r = teb::get::<i32>(cs, CS_RECURSION) - 1;
        teb::put(cs, CS_RECURSION, r);
        if r > 0 {
            return;
        }
        teb::put(cs, CS_OWNER, 0u64);
    }
    let w = cs_word(cs);
    if w.swap(0, Ordering::Release) == 2 {
        os_sys::futex_wake(w.as_ptr(), 1);
    }
}

// SRWLOCK: high bit = exclusive owner, remaining bits = shared readers.
const SRW_WRITER: u32 = 1 << 31;
fn srw_word(l: u64) -> &'static AtomicU32 {
    // SAFETY: the caller's SRWLOCK (8 bytes, aligned).
    unsafe { &*(l as usize as *const AtomicU32) }
}

extern "win64" fn InitializeSRWLock(l: u64) {
    srw_word(l).store(0, Ordering::Relaxed);
}

pub(crate) extern "win64" fn AcquireSRWLockExclusive(l: u64) {
    let w = srw_word(l);
    loop {
        if w.compare_exchange(0, SRW_WRITER, Ordering::Acquire, Ordering::Relaxed).is_ok() { return; }
        let old = w.load(Ordering::Relaxed);
        if old != 0 { let _ = os_sys::futex_wait(w.as_ptr(), old, 0); }
    }
}

pub(crate) extern "win64" fn ReleaseSRWLockExclusive(l: u64) {
    let w = srw_word(l);
    w.store(0, Ordering::Release);
    os_sys::futex_wake(w.as_ptr(), u32::MAX);
}

extern "win64" fn TryAcquireSRWLockExclusive(l: u64) -> u8 {
    srw_word(l).compare_exchange(0, SRW_WRITER, Ordering::Acquire, Ordering::Relaxed).is_ok() as u8
}

extern "win64" fn TryAcquireSRWLockShared(l: u64) -> u8 {
    let w = srw_word(l);
    loop {
        let old = w.load(Ordering::Relaxed);
        if old >= SRW_WRITER - 1 { return 0; }
        if w.compare_exchange_weak(old, old + 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { return 1; }
    }
}
pub(crate) extern "win64" fn AcquireSRWLockShared(l: u64) {
    let w = srw_word(l);
    while TryAcquireSRWLockShared(l) == 0 {
        let old = w.load(Ordering::Relaxed);
        if old & SRW_WRITER != 0 { let _ = os_sys::futex_wait(w.as_ptr(), old, 0); }
    }
}
pub(crate) extern "win64" fn ReleaseSRWLockShared(l: u64) {
    let w = srw_word(l);
    if w.fetch_sub(1, Ordering::Release) == 1 { os_sys::futex_wake(w.as_ptr(), u32::MAX); }
}

fn create_event(manual: bool, initial: bool, name: Option<String>) -> u64 {
    if let Some(o @ Object::Event(_)) = name.as_deref().and_then(objects::find_named) {
        teb::set_last_error(ERROR_ALREADY_EXISTS);
        return objects::insert(o);
    }
    teb::set_last_error(ERROR_SUCCESS);
    objects::insert(Object::Event(Arc::new(EventObj { signaled: AtomicBool::new(initial), manual, name })))
}

extern "win64" fn CreateEventA(attributes: u64, manual: Bool, initial: Bool, name: *const u8) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    // SAFETY: NULL or NUL-terminated name.
    create_event(manual != 0, initial != 0, unsafe { sa(name) })
}

extern "win64" fn CreateEventW(attributes: u64, manual: Bool, initial: Bool, name: *const u16) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    // SAFETY: as above.
    create_event(manual != 0, initial != 0, unsafe { sw(name) })
}

extern "win64" fn CreateEventExW(attributes: u64, name: *const u16, flags: u32, _access: u32) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    // SAFETY: as above.
    create_event(flags & 1 != 0, flags & 2 != 0, unsafe { sw(name) })
}

fn event_op(h: u64, set: bool) -> Bool {
    match objects::get(h) {
        Some(Object::Event(e)) => {
            e.signaled.store(set, Ordering::SeqCst);
            if set {
                objects::signal_all();
            }
            TRUE
        }
        _ => fail(ERROR_INVALID_HANDLE, FALSE),
    }
}

extern "win64" fn SetEvent(h: u64) -> Bool {
    event_op(h, true)
}

extern "win64" fn ResetEvent(h: u64) -> Bool {
    event_op(h, false)
}

fn create_mutex(owned: bool, name: Option<String>) -> u64 {
    if let Some(o @ Object::Mutex(_)) = name.as_deref().and_then(objects::find_named) {
        teb::set_last_error(ERROR_ALREADY_EXISTS);
        return objects::insert(o);
    }
    teb::set_last_error(ERROR_SUCCESS);
    let owner = if owned { (os_sys::gettid() as u32, 1) } else { (0, 0) };
    objects::insert(Object::Mutex(Arc::new(MutexObj { state: Mutex::new(owner), name })))
}

extern "win64" fn CreateMutexA(attributes: u64, owned: Bool, name: *const u8) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    // SAFETY: NULL or NUL-terminated name.
    create_mutex(owned != 0, unsafe { sa(name) })
}

extern "win64" fn CreateMutexW(attributes: u64, owned: Bool, name: *const u16) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    // SAFETY: as above.
    create_mutex(owned != 0, unsafe { sw(name) })
}

extern "win64" fn ReleaseMutex(h: u64) -> Bool {
    let Some(Object::Mutex(m)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    let me = os_sys::gettid() as u32;
    {
        let mut s = m.state.lock().unwrap();
        if s.0 != me {
            return fail(288, FALSE); // ERROR_NOT_OWNER
        }
        s.1 -= 1;
        if s.1 == 0 {
            s.0 = 0;
        }
    }
    objects::signal_all();
    TRUE
}

extern "win64" fn CreateSemaphoreA(attributes: u64, initial: i32, max: i32, _name: *const u8) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) { return fail(e,0); }
    if max <= 0 || initial < 0 || initial > max {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    objects::insert(Object::Semaphore(Arc::new(SemaphoreObj { count: Mutex::new(initial), max })))
}

extern "win64" fn CreateSemaphoreW(sa_: u64, initial: i32, max: i32, _name: *const u16) -> u64 {
    CreateSemaphoreA(sa_, initial, max, core::ptr::null())
}

extern "win64" fn ReleaseSemaphore(h: u64, n: i32, prev: *mut i32) -> Bool {
    let Some(Object::Semaphore(s)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    {
        let mut c = s.count.lock().unwrap();
        if n <= 0 || *c + n > s.max {
            return fail(298, FALSE); // ERROR_TOO_MANY_POSTS
        }
        if !prev.is_null() {
            // SAFETY: the caller's out pointer.
            unsafe { *prev = *c };
        }
        *c += n;
    }
    objects::signal_all();
    TRUE
}

extern "win64" fn WaitForSingleObject(h: u64, ms: u32) -> u32 {
    objects::wait(&[h], false, ms).unwrap_or_else(|e| fail(e, objects::WAIT_FAILED))
}

extern "win64" fn WaitForSingleObjectEx(h: u64, ms: u32, _alertable: Bool) -> u32 {
    WaitForSingleObject(h, ms)
}

extern "win64" fn WaitForMultipleObjects(n: u32, hs: *const u64, all: Bool, ms: u32) -> u32 {
    if hs.is_null() || n == 0 || n > 64 {
        return fail(ERROR_INVALID_PARAMETER, objects::WAIT_FAILED);
    }
    // SAFETY: the caller's array of `n` handles.
    let list = unsafe { core::slice::from_raw_parts(hs, n as usize) };
    objects::wait(list, all != 0, ms).unwrap_or_else(|e| fail(e, objects::WAIT_FAILED))
}

extern "win64" fn WaitForMultipleObjectsEx(n: u32, hs: *const u64, all: Bool, ms: u32, _alertable: Bool) -> u32 {
    WaitForMultipleObjects(n, hs, all, ms)
}

// TLS slots (TlsAlloc): 64 in the TEB, 1024 more in an expansion array.
static TLS_USED: Mutex<[u64; (teb::TLS_MINIMUM_AVAILABLE + teb::TLS_EXPANSION_SLOTS) / 64]> = Mutex::new([0; 17]);

extern "win64" fn TlsAlloc() -> u32 {
    let mut used = TLS_USED.lock().unwrap();
    for (w, word) in used.iter_mut().enumerate() {
        if *word != u64::MAX {
            let bit = (!*word).trailing_zeros();
            *word |= 1 << bit;
            return (w * 64) as u32 + bit;
        }
    }
    fail(ERROR_NO_MORE_ITEMS, u32::MAX)
}

extern "win64" fn TlsFree(i: u32) -> Bool {
    let mut used = TLS_USED.lock().unwrap();
    if i as usize >= teb::TLS_MINIMUM_AVAILABLE + teb::TLS_EXPANSION_SLOTS
        || used[i as usize / 64] & (1 << (i % 64)) == 0
    {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    used[i as usize / 64] &= !(1 << (i % 64));
    TRUE
}

/// Address of TLS slot `i` of the current thread (allocating the
/// expansion array on first use).
fn tls_slot(i: u32) -> Option<*mut u64> {
    let t = teb::current();
    let i = i as usize;
    if i < teb::TLS_MINIMUM_AVAILABLE {
        return Some((t as usize + teb::TEB_TLS_SLOTS + i * 8) as *mut u64);
    }
    let j = i - teb::TLS_MINIMUM_AVAILABLE;
    if j >= teb::TLS_EXPANSION_SLOTS {
        return None;
    }
    // SAFETY: the current thread's TEB.
    let mut exp: u64 = unsafe { teb::get(t, teb::TEB_TLS_EXPANSION) };
    if exp == 0 {
        exp = mem::alloc_pages(teb::TLS_EXPANSION_SLOTS * 8, mem::Prot::ReadWrite)?;
        // SAFETY: as above.
        unsafe { teb::put(t, teb::TEB_TLS_EXPANSION, exp) };
    }
    Some((exp as usize + j * 8) as *mut u64)
}

extern "win64" fn TlsGetValue(i: u32) -> u64 {
    match tls_slot(i) {
        Some(p) => {
            teb::set_last_error(ERROR_SUCCESS);
            // SAFETY: a slot of this thread's TEB or expansion array.
            unsafe { *p }
        }
        None => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

extern "win64" fn TlsSetValue(i: u32, v: u64) -> Bool {
    match tls_slot(i) {
        Some(p) => {
            // SAFETY: as above.
            unsafe { *p = v };
            TRUE
        }
        None => fail(ERROR_INVALID_PARAMETER, FALSE),
    }
}

// ---- memory -----------------------------------------------------------------------

extern "win64" fn VirtualAlloc(addr: u64, size: usize, kind: u32, protect: u32) -> u64 {
    mem::virtual_alloc(addr, size, kind, protect).unwrap_or_else(|s| fail(status_to_win32(s), 0))
}

extern "win64" fn VirtualFree(addr: u64, size: usize, kind: u32) -> Bool {
    mem::virtual_free(addr, size, kind).map_or_else(|s| fail(status_to_win32(s), FALSE), |_| TRUE)
}

extern "win64" fn VirtualProtect(addr: u64, size: usize, protect: u32, old: *mut u32) -> Bool {
    match mem::virtual_protect(addr, size, protect) {
        Ok(o) => {
            if !old.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *old = o };
            }
            TRUE
        }
        Err(s) => fail(status_to_win32(s), FALSE),
    }
}

/// MEMORY_BASIC_INFORMATION (48 bytes).
pub fn write_mbi(out: *mut u8, i: &mem::Info) {
    // SAFETY: the caller checked the 48-byte buffer.
    unsafe {
        teb::put(out as u64, 0, i.base);
        teb::put(out as u64, 8, i.allocation_base);
        teb::put(out as u64, 16, i.allocation_protect);
        teb::put(out as u64, 24, i.size);
        teb::put(out as u64, 32, i.state);
        teb::put(out as u64, 36, i.protect);
        teb::put(out as u64, 40, i.kind);
    }
}

extern "win64" fn VirtualQuery(addr: u64, out: *mut u8, len: usize) -> usize {
    if out.is_null() || len < 48 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    match mem::virtual_query(addr) {
        Some(i) => {
            write_mbi(out, &i);
            48
        }
        None => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

// Heaps: one allocator (the runtime's); a 16-byte header keeps the size.
const HEAP_ZERO_MEMORY: u32 = 0x8;

pub fn heap_alloc(size: usize, zero: bool) -> Option<u64> {
    let layout = core::alloc::Layout::from_size_align(size.checked_add(16)?, 16).ok()?;
    // SAFETY: a non-zero-sized layout.
    let p = unsafe {
        if zero {
            alloc::alloc::alloc_zeroed(layout)
        } else {
            alloc::alloc::alloc(layout)
        }
    };
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` has room for the header.
    unsafe { *(p as *mut usize) = size };
    Some(p as u64 + 16)
}

pub fn heap_size(p: u64) -> usize {
    // SAFETY: `p` came from `heap_alloc` (Windows contract).
    unsafe { *((p - 16) as *const usize) }
}

pub fn heap_free(p: u64) -> bool {
    if p == 0 {
        return true;
    }
    let size = heap_size(p);
    // SAFETY: as allocated in `heap_alloc`.
    unsafe {
        alloc::alloc::dealloc((p - 16) as *mut u8, core::alloc::Layout::from_size_align_unchecked(size + 16, 16))
    };
    true
}

extern "win64" fn GetProcessHeap() -> u64 {
    process_heap()
}

extern "win64" fn HeapCreate(_opts: u32, _initial: usize, _max: usize) -> u64 {
    mem::arena_alloc(16, 16)
}

extern "win64" fn HeapDestroy(_h: u64) -> Bool {
    TRUE
}

extern "win64" fn HeapAlloc(_h: u64, flags: u32, size: usize) -> u64 {
    heap_alloc(size, flags & HEAP_ZERO_MEMORY != 0).unwrap_or_else(|| fail(ERROR_NOT_ENOUGH_MEMORY, 0))
}

extern "win64" fn HeapFree(_h: u64, _flags: u32, p: u64) -> Bool {
    b(heap_free(p))
}

extern "win64" fn HeapSize(_h: u64, _flags: u32, p: u64) -> usize {
    heap_size(p)
}

extern "win64" fn HeapReAlloc(_h: u64, flags: u32, p: u64, size: usize) -> u64 {
    let Some(n) = heap_alloc(size, flags & HEAP_ZERO_MEMORY != 0) else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    if p != 0 {
        let old = heap_size(p);
        // SAFETY: both blocks are valid for min(old, size) bytes.
        unsafe { core::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, old.min(size)) };
        heap_free(p);
    }
    n
}

extern "win64" fn LocalAlloc(flags: u32, size: usize) -> u64 {
    heap_alloc(size, flags & 0x40 != 0).unwrap_or_else(|| fail(ERROR_NOT_ENOUGH_MEMORY, 0))
}

extern "win64" fn LocalFree(p: u64) -> u64 {
    heap_free(p);
    0
}

extern "win64" fn GlobalAlloc(flags: u32, size: usize) -> u64 {
    if flags & 0x2 != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0); // GMEM_MOVEABLE
    }
    LocalAlloc(flags, size)
}

extern "win64" fn GlobalFree(p: u64) -> u64 {
    LocalFree(p)
}

/// SYSTEM_INFO (48 bytes).
extern "win64" fn GetSystemInfo(out: *mut u8) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's 48-byte SYSTEM_INFO.
    unsafe {
        core::ptr::write_bytes(out, 0, 48);
        teb::put(out as u64, 0, 9u16); // PROCESSOR_ARCHITECTURE_AMD64
        teb::put(out as u64, 4, 4096u32);
        teb::put(out as u64, 8, 0x10000u64);
        teb::put(out as u64, 16, 0x7FFF_FFFE_FFFFu64);
        teb::put(out as u64, 24, 1u64);
        teb::put(out as u64, 32, 1u32);
        teb::put(out as u64, 36, 8664u32); // PROCESSOR_AMD_X8664
        teb::put(out as u64, 40, 0x10000u32);
        teb::put(out as u64, 44, 6u16);
    }
}

extern "win64" fn GetNativeSystemInfo(out: *mut u8) {
    GetSystemInfo(out)
}

// ---- time -------------------------------------------------------------------------

fn now_ns() -> u64 {
    os_sys::clock(osyx_abi::CLOCK_MONOTONIC).unwrap_or(0)
}

extern "win64" fn GetTickCount() -> u32 {
    (now_ns() / 1_000_000) as u32
}

extern "win64" fn GetTickCount64() -> u64 {
    now_ns() / 1_000_000
}

extern "win64" fn QueryPerformanceCounter(out: *mut i64) -> Bool {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = now_ns() as i64 };
    TRUE
}

extern "win64" fn QueryPerformanceFrequency(out: *mut i64) -> Bool {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = 1_000_000_000 };
    TRUE
}

extern "win64" fn GetSystemTimeAsFileTime(out: *mut u64) {
    let ns = os_sys::clock(osyx_abi::CLOCK_REALTIME).unwrap_or(0) as i64;
    if !out.is_null() {
        // SAFETY: the caller's FILETIME.
        unsafe { core::ptr::write_unaligned(out, filetime(ns)) };
    }
}

extern "win64" fn GetSystemTimePreciseAsFileTime(out: *mut u64) {
    GetSystemTimeAsFileTime(out)
}

// ---- modules ------------------------------------------------------------------------

extern "win64" fn GetModuleHandleA(name: *const u8) -> u64 {
    // SAFETY: NULL or a NUL-terminated name.
    let n = unsafe { sa(name) };
    loader::get_module_handle(n.as_deref()).unwrap_or_else(|| fail(ERROR_MOD_NOT_FOUND, 0))
}

extern "win64" fn GetModuleHandleW(name: *const u16) -> u64 {
    // SAFETY: as above.
    let n = unsafe { sw(name) };
    loader::get_module_handle(n.as_deref()).unwrap_or_else(|| fail(ERROR_MOD_NOT_FOUND, 0))
}

extern "win64" fn GetModuleHandleExW(flags: u32, name: *const u16, out: *mut u64) -> Bool {
    if flags & 4 != 0 || out.is_null() {
        return fail(ERROR_NOT_SUPPORTED, FALSE); // FROM_ADDRESS
    }
    let h = GetModuleHandleW(name);
    // SAFETY: the caller's out pointer.
    unsafe { *out = h };
    b(h != 0)
}

fn module_name(h: u64) -> Option<String> {
    let exe = with_process(|p| p.exe_path.clone());
    loader::module_file_name(if h == 0 { None } else { Some(h) }, &exe)
}

extern "win64" fn GetModuleFileNameA(h: u64, buf: *mut u8, size: u32) -> u32 {
    match module_name(h) {
        Some(n) => {
            let r = out_a(&n, buf, size);
            if r >= size {
                // Truncated: Windows copies what fits and reports the size.
                if size > 0 {
                    let k = (size - 1) as usize;
                    // SAFETY: the caller's buffer of `size` bytes.
                    unsafe {
                        core::ptr::copy_nonoverlapping(n.as_ptr(), buf, k);
                        *buf.add(k) = 0;
                    }
                }
                return fail(ERROR_INSUFFICIENT_BUFFER, size);
            }
            r
        }
        None => fail(ERROR_MOD_NOT_FOUND, 0),
    }
}

extern "win64" fn GetModuleFileNameW(h: u64, buf: *mut u16, size: u32) -> u32 {
    match module_name(h) {
        Some(n) => {
            let r = out_w(&n, buf, size);
            if r >= size {
                return fail(ERROR_INSUFFICIENT_BUFFER, size);
            }
            r
        }
        None => fail(ERROR_MOD_NOT_FOUND, 0),
    }
}

extern "win64" fn GetProcAddress(h: u64, name: *const u8) -> u64 {
    let sym_name;
    let sym = if (name as u64) < 0x10000 {
        Symbol::Ordinal(name as u64 as u16)
    } else {
        // SAFETY: a NUL-terminated name.
        sym_name = match unsafe { sa(name) } {
            Some(s) => s,
            None => return fail(ERROR_INVALID_PARAMETER, 0),
        };
        Symbol::Name(&sym_name)
    };
    loader::get_proc_address(h, sym).unwrap_or_else(|e| fail(e, 0))
}

fn load_library(name: Option<String>) -> u64 {
    let Some(n) = name else { return fail(ERROR_INVALID_PARAMETER, 0) };
    loader::load_library(&n).unwrap_or_else(|e| fail(e, 0))
}

extern "win64" fn LoadLibraryA(name: *const u8) -> u64 {
    // SAFETY: a NUL-terminated name.
    load_library(unsafe { sa(name) })
}

extern "win64" fn LoadLibraryW(name: *const u16) -> u64 {
    // SAFETY: a NUL-terminated name.
    load_library(unsafe { sw(name) })
}

extern "win64" fn LoadLibraryExA(name: *const u8, _file: u64, _flags: u32) -> u64 {
    LoadLibraryA(name)
}

extern "win64" fn LoadLibraryExW(name: *const u16, _file: u64, _flags: u32) -> u64 {
    LoadLibraryW(name)
}

extern "win64" fn FreeLibrary(h: u64) -> Bool {
    loader::free_library(h).map_or_else(|e| fail(e, FALSE), |_| TRUE)
}

extern "win64" fn DisableThreadLibraryCalls(_h: u64) -> Bool {
    TRUE
}

// ---- strings and code pages ---------------------------------------------------------

extern "win64" fn lstrlenA(s: *const u8) -> i32 {
    // SAFETY: a NUL-terminated string (or NULL).
    unsafe { sa(s) }.map_or(0, |s| s.len() as i32)
}

extern "win64" fn lstrlenW(s: *const u16) -> i32 {
    // SAFETY: as above.
    unsafe { sw(s) }.map_or(0, |s| s.encode_utf16().count() as i32)
}

extern "win64" fn GetACP() -> u32 {
    65001
}

extern "win64" fn GetOEMCP() -> u32 {
    65001
}

/// CP_ACP / CP_OEMCP / CP_UTF8 are all UTF-8 on OSyx.
extern "win64" fn MultiByteToWideChar(cp: u32, _flags: u32, src: *const u8, len: i32, dst: *mut u16, cap: i32) -> i32 {
    if !matches!(cp, 0 | 1 | 65001) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let bytes = if len < 0 {
        // SAFETY: NUL-terminated input; the terminator is converted too.
        let s = match unsafe { sa(src) } {
            Some(s) => s,
            None => return fail(ERROR_INVALID_PARAMETER, 0),
        };
        let mut v = s.into_bytes();
        v.push(0);
        v
    } else {
        // SAFETY: the caller's buffer of `len` bytes.
        unsafe { core::slice::from_raw_parts(src, len as usize) }.to_vec()
    };
    let text = String::from_utf8_lossy(&bytes);
    let units: Vec<u16> = text.encode_utf16().collect();
    if cap == 0 {
        return units.len() as i32;
    }
    if (cap as usize) < units.len() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `cap` units.
    unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len()) };
    units.len() as i32
}

extern "win64" fn WideCharToMultiByte(
    cp: u32,
    _flags: u32,
    src: *const u16,
    len: i32,
    dst: *mut u8,
    cap: i32,
    _def: *const u8,
    used_def: *mut Bool,
) -> i32 {
    if !matches!(cp, 0 | 1 | 65001) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let units: Vec<u16> = if len < 0 {
        // SAFETY: NUL-terminated input; the terminator is converted too.
        match unsafe { sw(src) } {
            Some(s) => s.encode_utf16().chain(core::iter::once(0)).collect(),
            None => return fail(ERROR_INVALID_PARAMETER, 0),
        }
    } else {
        // SAFETY: the caller's buffer of `len` units.
        unsafe { core::slice::from_raw_parts(src, len as usize) }.to_vec()
    };
    let s: String = char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    if !used_def.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *used_def = FALSE };
    }
    if cap == 0 {
        return s.len() as i32;
    }
    if (cap as usize) < s.len() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `cap` bytes.
    unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), dst, s.len()) };
    s.len() as i32
}

// ---- version ------------------------------------------------------------------------

/// The Windows version the compatibility layer presents: 10.0.19041.
extern "win64" fn GetVersion() -> u32 {
    10 | (19041 << 16)
}

extern "win64" fn GetVersionExA(info: *mut u8) -> Bool {
    if info.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: OSVERSIONINFOA starts with its size; the fields written lie
    // within the 148-byte structure.
    unsafe {
        let size: u32 = teb::get(info as u64, 0);
        if size < 148 {
            return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
        }
        teb::put(info as u64, 4, 10u32);
        teb::put(info as u64, 8, 0u32);
        teb::put(info as u64, 12, 19041u32);
        teb::put(info as u64, 16, 2u32);
    }
    TRUE
}

extern "win64" fn GetVersionExW(info: *mut u8) -> Bool {
    if info.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: as above for the 276-byte OSVERSIONINFOW.
    unsafe {
        let size: u32 = teb::get(info as u64, 0);
        if size < 276 {
            return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
        }
        teb::put(info as u64, 4, 10u32);
        teb::put(info as u64, 8, 0u32);
        teb::put(info as u64, 12, 19041u32);
        teb::put(info as u64, 16, 2u32);
    }
    TRUE
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut exports = export_table![
        GetLastError,
        SetLastError,
        IsDebuggerPresent,
        OutputDebugStringA,
        OutputDebugStringW,
        SetUnhandledExceptionFilter,
        RaiseException,
        GetStdHandle,
        SetStdHandle,
        GetConsoleMode,
        SetConsoleMode,
        GetConsoleCP,
        GetConsoleOutputCP,
        SetConsoleOutputCP,
        WriteConsoleA,
        WriteConsoleW,
        CreateFileA,
        CreateFileW,
        ReadFile,
        WriteFile,
        GetFileSize,
        GetFileSizeEx,
        SetFilePointer,
        SetFilePointerEx,
        SetEndOfFile,
        FlushFileBuffers,
        GetFileType,
        GetFileInformationByHandle,
        GetFileAttributesA,
        GetFileAttributesW,
        CreateDirectoryA,
        CreateDirectoryW,
        RemoveDirectoryA,
        RemoveDirectoryW,
        DeleteFileA,
        DeleteFileW,
        MoveFileA,
        MoveFileW,
        MoveFileExA,
        MoveFileExW,
        GetCurrentDirectoryA,
        GetCurrentDirectoryW,
        SetCurrentDirectoryA,
        SetCurrentDirectoryW,
        GetFullPathNameA,
        GetFullPathNameW,
        GetTempPathA,
        GetTempPathW,
        CloseHandle,
        DuplicateHandle,
        GetCurrentProcess,
        GetCurrentThread,
        GetCurrentProcessId,
        GetCurrentThreadId,
        ExitProcess,
        TerminateProcess,
        GetExitCodeProcess,
        CreateThread,
        ResumeThread,
        SuspendThread,
        ExitThread,
        GetExitCodeThread,
        GetThreadId,
        Sleep,
        SleepEx,
        SwitchToThread,
        GetCommandLineA,
        GetCommandLineW,
        GetStartupInfoA,
        GetStartupInfoW,
        GetEnvironmentVariableA,
        GetEnvironmentVariableW,
        SetEnvironmentVariableA,
        SetEnvironmentVariableW,
        GetEnvironmentStringsW,
        FreeEnvironmentStringsW,
        InitializeCriticalSection,
        InitializeCriticalSectionAndSpinCount,
        InitializeCriticalSectionEx,
        DeleteCriticalSection,
        EnterCriticalSection,
        TryEnterCriticalSection,
        LeaveCriticalSection,
        InitializeSRWLock,
        AcquireSRWLockExclusive,
        ReleaseSRWLockExclusive,
        TryAcquireSRWLockExclusive,
        AcquireSRWLockShared, ReleaseSRWLockShared, TryAcquireSRWLockShared,
        CreateEventA,
        CreateEventW,
        CreateEventExW,
        SetEvent,
        ResetEvent,
        CreateMutexA,
        CreateMutexW,
        ReleaseMutex,
        CreateSemaphoreA,
        CreateSemaphoreW,
        ReleaseSemaphore,
        WaitForSingleObject,
        WaitForSingleObjectEx,
        WaitForMultipleObjects,
        WaitForMultipleObjectsEx,
        TlsAlloc,
        TlsFree,
        TlsGetValue,
        TlsSetValue,
        VirtualAlloc,
        VirtualFree,
        VirtualProtect,
        VirtualQuery,
        GetProcessHeap,
        HeapCreate,
        HeapDestroy,
        HeapAlloc,
        HeapFree,
        HeapSize,
        HeapReAlloc,
        LocalAlloc,
        LocalFree,
        GlobalAlloc,
        GlobalFree,
        GetSystemInfo,
        GetNativeSystemInfo,
        GetTickCount,
        GetTickCount64,
        QueryPerformanceCounter,
        QueryPerformanceFrequency,
        GetSystemTimeAsFileTime,
        GetSystemTimePreciseAsFileTime,
        GetModuleHandleA,
        GetModuleHandleW,
        GetModuleHandleExW,
        GetModuleFileNameA,
        GetModuleFileNameW,
        GetProcAddress,
        LoadLibraryA,
        LoadLibraryW,
        LoadLibraryExA,
        LoadLibraryExW,
        FreeLibrary,
        DisableThreadLibraryCalls,
        lstrlenA,
        lstrlenW,
        GetACP,
        GetOEMCP,
        MultiByteToWideChar,
        WideCharToMultiByte,
        GetVersion,
        GetVersionExA,
        GetVersionExW,
    ];
    exports.extend(crate::volumes::exports());
    exports.extend(crate::synch::exports());
    exports.extend(crate::psapi::kernel_exports());
    exports.extend(crate::timers::exports());
    exports.extend(crate::exceptions::exports());
    exports.extend(crate::mapping::exports());
    exports.push(("OpenProcessToken",crate::security::OpenProcessToken as *const () as u64));
    exports
}
