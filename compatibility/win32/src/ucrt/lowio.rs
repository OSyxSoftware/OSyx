//! Low-level I/O: C file descriptors over Win32 file handles, with the
//! UCRT's text mode (CR-LF to LF on read, LF to CR-LF on write, Ctrl+Z as
//! end of file for disk files), append mode and Win32 error mapping. The
//! Unicode translation modes (_O_WTEXT, _O_U8TEXT, _O_U16TEXT) are not
//! provided and are refused with EINVAL.

use super::*;
use core::sync::atomic::AtomicI32;

pub const O_RDONLY: i32 = 0;
pub const O_WRONLY: i32 = 1;
pub const O_RDWR: i32 = 2;
pub const O_APPEND: i32 = 0x8;
pub const O_TEMPORARY: i32 = 0x40;
pub const O_NOINHERIT: i32 = 0x80;
pub const O_CREAT: i32 = 0x100;
pub const O_TRUNC: i32 = 0x200;
pub const O_EXCL: i32 = 0x400;
pub const O_TEXT: i32 = 0x4000;
pub const O_BINARY: i32 = 0x8000;
const O_WTEXT: i32 = 0x10000;
const O_U16TEXT: i32 = 0x20000;
const O_U8TEXT: i32 = 0x40000;
const UNICODE_MODES: i32 = O_WTEXT | O_U16TEXT | O_U8TEXT;
const O_ACCMODE: i32 = 3;

pub const SH_DENYRW: i32 = 0x10;
pub const SH_DENYWR: i32 = 0x20;
pub const SH_DENYRD: i32 = 0x30;
pub const SH_DENYNO: i32 = 0x40;
pub const SH_SECURE: i32 = 0x80;

const S_IWRITE: i32 = 0x80;

/// `_fmode`: the default translation mode.
pub static FMODE: AtomicI32 = AtomicI32::new(O_TEXT);

#[derive(Clone, Copy)]
struct Fd {
    handle: u64,
    text: bool,
    append: bool,
    /// A character device or pipe (no seeking, no Ctrl+Z rule).
    device: bool,
    /// Ctrl+Z was read in text mode: reads return 0 until a seek.
    eof: bool,
    /// A byte read ahead after a CR at the end of a device read.
    peek: Option<u8>,
}

const MAX_FDS: usize = 8192;
static FDS: Mutex<Vec<Option<Fd>>> = Mutex::new(Vec::new());

k32fn!(fn get_std_handle = "GetStdHandle"(which: u32) -> u64);
k32fn!(fn get_file_type = "GetFileType"(h: u64) -> u32);
k32fn!(fn create_file = "CreateFileW"(n: *const u16, a: u32, s: u32, sa: u64, d: u32, f: u32, t: u64) -> u64);
k32fn!(fn close_handle = "CloseHandle"(h: u64) -> i32);
k32fn!(fn read_file = "ReadFile"(h: u64, b: *mut u8, n: u32, r: *mut u32, o: u64) -> i32);
k32fn!(fn write_file = "WriteFile"(h: u64, b: *const u8, n: u32, w: *mut u32, o: u64) -> i32);
k32fn!(fn set_file_pointer_ex = "SetFilePointerEx"(h: u64, d: i64, n: *mut i64, m: u32) -> i32);
k32fn!(fn flush_file_buffers = "FlushFileBuffers"(h: u64) -> i32);
k32fn!(fn duplicate_handle = "DuplicateHandle"(sp: u64, s: u64, dp: u64, d: *mut u64, a: u32, i: i32, o: u32) -> i32);

const FILE_TYPE_DISK: u32 = 1;

fn new_fd(handle: u64, text: bool, append: bool) -> Fd {
    Fd { handle, text, append, device: get_file_type(handle) != FILE_TYPE_DISK, eof: false, peek: None }
}

/// The table, with fds 0-2 bound to the standard handles on first use.
fn table() -> os_std::sync::MutexGuard<'static, Vec<Option<Fd>>> {
    let mut t = FDS.lock().unwrap();
    if t.is_empty() {
        for which in [-10i32, -11, -12] {
            let h = get_std_handle(which as u32);
            t.push(if h == 0 || h == u64::MAX { None } else { Some(new_fd(h, true, false)) });
        }
    }
    t
}

fn get(fd: i32) -> Option<Fd> {
    let t = table();
    usize::try_from(fd).ok().and_then(|i| t.get(i).copied().flatten())
}

fn update(fd: i32, f: impl FnOnce(&mut Fd)) {
    let mut t = table();
    if let Some(Some(e)) = usize::try_from(fd).ok().and_then(|i| t.get_mut(i)) {
        f(e);
    }
}

/// Validates an fd as the UCRT does (EBADF and the invalid-parameter
/// handler for one that is out of range or closed).
fn checked(fd: i32) -> Option<Fd> {
    let e = get(fd);
    if e.is_none() {
        ptd().doserrno = 0;
        set_errno(EBADF);
        invalid_parameter();
    }
    e
}

/// Allocates the lowest free descriptor.
pub fn alloc_fd(handle: u64, text: bool, append: bool) -> Result<i32, i32> {
    let mut t = table();
    let e = Some(new_fd(handle, text, append));
    if let Some(i) = t.iter().position(|e| e.is_none()) {
        t[i] = e;
        return Ok(i as i32);
    }
    if t.len() >= MAX_FDS {
        return Err(EMFILE);
    }
    t.push(e);
    Ok(t.len() as i32 - 1)
}

pub fn handle_of(fd: i32) -> Option<u64> {
    get(fd).map(|e| e.handle)
}

pub fn is_text(fd: i32) -> bool {
    get(fd).is_some_and(|e| e.text)
}

pub fn is_append(fd: i32) -> bool {
    get(fd).is_some_and(|e| e.append)
}

/// FlushFileBuffers for the fd (stream commit mode).
pub fn commit(fd: i32) {
    if let Some(e) = get(fd) {
        flush_file_buffers(e.handle);
    }
}

pub fn is_device(fd: i32) -> bool {
    get(fd).is_some_and(|e| e.device)
}

/// Opens `path` (wide, NUL-terminated) with C flags; returns the fd.
pub fn open(path: &[u16], oflag: i32, shflag: i32, pmode: i32) -> Result<i32, i32> {
    if oflag & UNICODE_MODES != 0 {
        return Err(EINVAL);
    }
    let access = match oflag & O_ACCMODE {
        O_RDONLY => 0x8000_0000u32,
        O_WRONLY => 0x4000_0000,
        O_RDWR => 0xC000_0000,
        _ => return Err(EINVAL),
    };
    let share = match shflag {
        SH_DENYRW => 0,
        SH_DENYWR => 1,
        SH_DENYRD => 2,
        SH_DENYNO => 3,
        SH_SECURE => {
            if oflag & O_ACCMODE == O_RDONLY {
                1
            } else {
                0
            }
        }
        _ => return Err(EINVAL),
    };
    let disposition = match oflag & (O_CREAT | O_EXCL | O_TRUNC) {
        0 | O_EXCL => 3,                              // OPEN_EXISTING
        O_CREAT => 4,                                 // OPEN_ALWAYS
        x if x == O_CREAT | O_EXCL || x == O_CREAT | O_TRUNC | O_EXCL => 1, // CREATE_NEW
        x if x == O_CREAT | O_TRUNC => 2,             // CREATE_ALWAYS
        _ => 5,                                       // TRUNCATE_EXISTING
    };
    let mut flags = 0x80u32; // FILE_ATTRIBUTE_NORMAL
    if oflag & O_CREAT != 0 && pmode & S_IWRITE == 0 {
        flags = 1; // FILE_ATTRIBUTE_READONLY
    }
    let mut access = access;
    if oflag & O_TEMPORARY != 0 {
        flags |= 0x0400_0000; // FILE_FLAG_DELETE_ON_CLOSE
        access |= 0x1_0000; // DELETE
    }
    let h = create_file(path.as_ptr(), access, share, 0, disposition, flags, 0);
    if h == u64::MAX {
        dosmaperr(get_last_error());
        return Err(errno());
    }
    let text = if oflag & O_BINARY != 0 {
        false
    } else if oflag & O_TEXT != 0 {
        true
    } else {
        FMODE.load(Ordering::SeqCst) != O_BINARY
    };
    alloc_fd(h, text, oflag & O_APPEND != 0).inspect_err(|_| {
        close_handle(h);
    })
}

/// Closes an fd; Err(errno).
pub fn close(fd: i32) -> Result<(), i32> {
    let e = {
        let mut t = table();
        match usize::try_from(fd).ok().and_then(|i| t.get_mut(i)) {
            Some(slot @ Some(_)) => slot.take(),
            _ => None,
        }
    };
    let Some(e) = e else { return Err(EBADF) };
    if close_handle(e.handle) == 0 {
        dosmaperr(get_last_error());
        return Err(errno());
    }
    Ok(())
}

fn raw_read(h: u64, buf: &mut [u8]) -> Result<usize, i32> {
    let mut n = 0u32;
    let len = buf.len().min(u32::MAX as usize) as u32;
    if read_file(h, buf.as_mut_ptr(), len, &mut n, 0) == 0 {
        let e = get_last_error();
        if e == 109 {
            return Ok(0); // ERROR_BROKEN_PIPE: end of file
        }
        dosmaperr(e);
        if e == 5 {
            set_errno(EBADF); // a handle not opened for reading
        }
        return Err(errno());
    }
    Ok(n as usize)
}

/// Reads into `buf`; in text mode translates and, when `map` is given,
/// records for each output byte the raw offset it came from (plus the
/// total raw bytes consumed last).
pub fn read(fd: i32, buf: &mut [u8], map: Option<&mut Vec<u32>>) -> Result<usize, i32> {
    let Some(e) = get(fd) else { return Err(EBADF) };
    if buf.is_empty() {
        return Ok(0);
    }
    if !e.text {
        let n = raw_read(e.handle, buf)?;
        if let Some(m) = map {
            m.clear();
            m.extend(0..=n as u32);
        }
        return Ok(n);
    }
    if e.eof {
        return Ok(0);
    }
    let mut raw = alloc::vec![0u8; buf.len()];
    let mut start = 0;
    if let Some(p) = e.peek {
        raw[0] = p;
        start = 1;
        update(fd, |f| f.peek = None);
    }
    let n = start + raw_read(e.handle, &mut raw[start..])?;
    raw.truncate(n);
    let mut offsets: Vec<u32> = Vec::new();
    let mut out = 0;
    let mut i = 0;
    let mut consumed = n;
    while i < n {
        let c = raw[i];
        if c == 0x1A && !e.device {
            // Ctrl+Z ends a text file: leave the file just before it.
            consumed = i;
            update(fd, |f| f.eof = true);
            if i < n {
                let back = (n - i) as i64;
                set_file_pointer_ex(e.handle, -back, core::ptr::null_mut(), 1);
            }
            break;
        }
        if c == b'\r' {
            if i + 1 < n {
                if raw[i + 1] == b'\n' {
                    offsets.push(i as u32);
                    buf[out] = b'\n';
                    out += 1;
                    i += 2;
                    continue;
                }
            } else {
                // A CR at the end: look at the next byte.
                let mut one = [0u8; 1];
                let k = raw_read(e.handle, &mut one).unwrap_or(0);
                if k == 1 && one[0] == b'\n' {
                    offsets.push(i as u32);
                    buf[out] = b'\n';
                    out += 1;
                    consumed = n + 1;
                    break;
                }
                if k == 1 {
                    if e.device {
                        update(fd, |f| f.peek = Some(one[0]));
                    } else {
                        set_file_pointer_ex(e.handle, -1, core::ptr::null_mut(), 1);
                    }
                }
            }
        }
        offsets.push(i as u32);
        buf[out] = c;
        out += 1;
        i += 1;
    }
    if let Some(m) = map {
        offsets.push(consumed as u32);
        *m = offsets;
    }
    Ok(out)
}

fn raw_write(h: u64, data: &[u8]) -> Result<usize, i32> {
    let mut n = 0u32;
    if write_file(h, data.as_ptr(), data.len() as u32, &mut n, 0) == 0 {
        let e = get_last_error();
        dosmaperr(e);
        if e == 5 {
            set_errno(EBADF);
        }
        return Err(errno());
    }
    Ok(n as usize)
}

/// Writes `data`; returns the number of `data` bytes written.
pub fn write(fd: i32, data: &[u8]) -> Result<usize, i32> {
    let Some(e) = get(fd) else { return Err(EBADF) };
    if e.append {
        set_file_pointer_ex(e.handle, 0, core::ptr::null_mut(), 2);
    }
    if !e.text {
        let mut done = 0;
        while done < data.len() {
            let k = raw_write(e.handle, &data[done..])?;
            if k == 0 {
                set_errno(ENOSPC);
                return if done > 0 { Ok(done) } else { Err(ENOSPC) };
            }
            done += k;
        }
        return Ok(done);
    }
    let mut done = 0;
    for line in data.split_inclusive(|&b| b == b'\n') {
        let mut chunk = line.to_vec();
        if chunk.last() == Some(&b'\n') {
            chunk.pop();
            chunk.extend_from_slice(b"\r\n");
        }
        let mut w = 0;
        while w < chunk.len() {
            let k = raw_write(e.handle, &chunk[w..])?;
            if k == 0 {
                set_errno(ENOSPC);
                return if done > 0 { Ok(done) } else { Err(ENOSPC) };
            }
            w += k;
        }
        done += line.len();
    }
    Ok(done)
}

pub fn seek(fd: i32, off: i64, whence: i32) -> Result<i64, i32> {
    let Some(e) = get(fd) else { return Err(EBADF) };
    if !(0..=2).contains(&whence) {
        return Err(EINVAL);
    }
    let mut pos = 0i64;
    if set_file_pointer_ex(e.handle, off, &mut pos, whence as u32) == 0 {
        dosmaperr(get_last_error());
        return Err(errno());
    }
    update(fd, |f| {
        f.eof = false;
        f.peek = None;
    });
    Ok(pos)
}

// ---- exported functions ------------------------------------------------------------------

extern "win64" fn _open(path: *const u8, oflag: i32, pmode: i32) -> i32 {
    validate!(!path.is_null(), EINVAL, -1);
    open(&super::fs::widen_path(path), oflag, SH_DENYNO, pmode).unwrap_or_else(|e| {
        set_errno(e);
        -1
    })
}

extern "win64" fn _wopen(path: *const u16, oflag: i32, pmode: i32) -> i32 {
    validate!(!path.is_null(), EINVAL, -1);
    open(&super::fs::wide_path(path), oflag, SH_DENYNO, pmode).unwrap_or_else(|e| {
        set_errno(e);
        -1
    })
}

fn sopen_s(out: *mut i32, path: Vec<u16>, oflag: i32, shflag: i32, pmode: i32) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    // SAFETY: validated.
    unsafe { *out = -1 };
    match open(&path, oflag, shflag, pmode) {
        Ok(fd) => {
            // SAFETY: validated.
            unsafe { *out = fd };
            0
        }
        Err(e) => {
            set_errno(e);
            e
        }
    }
}

extern "win64" fn _sopen_s(out: *mut i32, path: *const u8, oflag: i32, shflag: i32, pmode: i32) -> i32 {
    validate!(!path.is_null(), EINVAL, EINVAL);
    sopen_s(out, super::fs::widen_path(path), oflag, shflag, pmode)
}

extern "win64" fn _wsopen_s(out: *mut i32, path: *const u16, oflag: i32, shflag: i32, pmode: i32) -> i32 {
    validate!(!path.is_null(), EINVAL, EINVAL);
    sopen_s(out, super::fs::wide_path(path), oflag, shflag, pmode)
}

extern "win64" fn _close(fd: i32) -> i32 {
    if checked(fd).is_none() {
        return -1;
    }
    match close(fd) {
        Ok(()) => 0,
        Err(e) => {
            set_errno(e);
            -1
        }
    }
}

extern "win64" fn _read(fd: i32, buf: *mut u8, n: u32) -> i32 {
    if checked(fd).is_none() {
        return -1;
    }
    validate!(n <= i32::MAX as u32, EINVAL, -1);
    validate!(!buf.is_null() || n == 0, EINVAL, -1);
    if n == 0 {
        return 0;
    }
    // SAFETY: the caller's buffer of `n` bytes.
    let b = unsafe { core::slice::from_raw_parts_mut(buf, n as usize) };
    read(fd, b, None).map_or_else(
        |e| {
            set_errno(e);
            -1
        },
        |k| k as i32,
    )
}

extern "win64" fn _write(fd: i32, buf: *const u8, n: u32) -> i32 {
    if checked(fd).is_none() {
        return -1;
    }
    if n == 0 {
        return 0;
    }
    validate!(!buf.is_null(), EINVAL, -1);
    // SAFETY: the caller's buffer of `n` bytes.
    let b = unsafe { core::slice::from_raw_parts(buf, n as usize) };
    write(fd, b).map_or_else(
        |e| {
            set_errno(e);
            -1
        },
        |k| k as i32,
    )
}

extern "win64" fn _lseeki64(fd: i32, off: i64, whence: i32) -> i64 {
    if checked(fd).is_none() {
        return -1;
    }
    validate!((0..=2).contains(&whence), EINVAL, -1);
    seek(fd, off, whence).unwrap_or_else(|e| {
        set_errno(e);
        -1
    })
}

extern "win64" fn _lseek(fd: i32, off: i32, whence: i32) -> i32 {
    let r = _lseeki64(fd, off as i64, whence);
    if r > i32::MAX as i64 {
        set_errno(EINVAL);
        return -1;
    }
    r as i32
}

extern "win64" fn _telli64(fd: i32) -> i64 {
    _lseeki64(fd, 0, 1)
}

extern "win64" fn _filelengthi64(fd: i32) -> i64 {
    let Some(_) = checked(fd) else { return -1 };
    let Ok(cur) = seek(fd, 0, 1) else { return -1 };
    let end = seek(fd, 0, 2);
    let _ = seek(fd, cur, 0);
    end.unwrap_or_else(|e| {
        set_errno(e);
        -1
    })
}

extern "win64" fn _get_osfhandle(fd: i32) -> u64 {
    match checked(fd) {
        Some(e) => e.handle,
        None => u64::MAX,
    }
}

extern "win64" fn _open_osfhandle(h: u64, flags: i32) -> i32 {
    if h == 0 || h == u64::MAX {
        set_errno(EBADF);
        return -1;
    }
    let text = if flags & O_BINARY != 0 {
        false
    } else if flags & O_TEXT != 0 {
        true
    } else {
        FMODE.load(Ordering::SeqCst) != O_BINARY
    };
    alloc_fd(h, text, flags & O_APPEND != 0).unwrap_or_else(|e| {
        set_errno(e);
        -1
    })
}

extern "win64" fn _isatty(fd: i32) -> i32 {
    match get(fd) {
        Some(e) if e.device && get_file_type(e.handle) == 2 => 0x40, // FDEV
        Some(_) => 0,
        None => {
            set_errno(EBADF);
            0
        }
    }
}

extern "win64" fn _setmode(fd: i32, mode: i32) -> i32 {
    if checked(fd).is_none() {
        return -1;
    }
    validate!(matches!(mode, O_TEXT | O_BINARY | O_WTEXT | O_U8TEXT | O_U16TEXT), EINVAL, -1);
    if mode & UNICODE_MODES != 0 {
        set_errno(EINVAL);
        return -1;
    }
    let mut old = O_TEXT;
    update(fd, |f| {
        if !f.text {
            old = O_BINARY;
        }
        f.text = mode == O_TEXT;
    });
    old
}

extern "win64" fn _commit(fd: i32) -> i32 {
    let Some(e) = checked(fd) else { return -1 };
    if flush_file_buffers(e.handle) == 0 {
        dosmaperr(get_last_error());
        return -1;
    }
    0
}

fn dup_handle(h: u64) -> Option<u64> {
    let me = crate::objects::CURRENT_PROCESS;
    let mut out = 0u64;
    if duplicate_handle(me, h, me, &mut out, 0, 0, 2) == 0 {
        dosmaperr(get_last_error());
        return None;
    }
    Some(out)
}

extern "win64" fn _dup(fd: i32) -> i32 {
    let Some(e) = checked(fd) else { return -1 };
    let Some(h) = dup_handle(e.handle) else { return -1 };
    match alloc_fd(h, e.text, e.append) {
        Ok(n) => n,
        Err(err) => {
            close_handle(h);
            set_errno(err);
            -1
        }
    }
}

extern "win64" fn _dup2(fd: i32, to: i32) -> i32 {
    let Some(e) = checked(fd) else { return -1 };
    validate!((0..MAX_FDS as i32).contains(&to), EBADF, -1);
    if fd == to {
        return 0;
    }
    let Some(h) = dup_handle(e.handle) else { return -1 };
    if get(to).is_some() {
        let _ = close(to);
    }
    let mut t = table();
    while t.len() <= to as usize {
        t.push(None);
    }
    t[to as usize] = Some(Fd { handle: h, ..e });
    0
}

extern "win64" fn _eof(fd: i32) -> i32 {
    if checked(fd).is_none() {
        return -1;
    }
    let (Ok(cur), Ok(end)) = (seek(fd, 0, 1), seek(fd, 0, 2)) else { return -1 };
    let _ = seek(fd, cur, 0);
    (cur >= end) as i32
}

extern "win64" fn _set_fmode(mode: i32) -> i32 {
    validate!(mode == O_TEXT || mode == O_BINARY, EINVAL, EINVAL);
    FMODE.store(mode, Ordering::SeqCst);
    0
}

extern "win64" fn _get_fmode(out: *mut i32) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    // SAFETY: validated.
    unsafe { *out = FMODE.load(Ordering::SeqCst) };
    0
}

extern "win64" fn __p__fmode() -> u64 {
    FMODE.as_ptr() as u64
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        _open,
        _wopen,
        _sopen_s,
        _wsopen_s,
        _close,
        _read,
        _write,
        _lseeki64,
        _lseek,
        _telli64,
        _filelengthi64,
        _get_osfhandle,
        _open_osfhandle,
        _isatty,
        _setmode,
        _commit,
        _dup,
        _dup2,
        _eof,
        _set_fmode,
        _get_fmode,
        __p__fmode,
    ]
}
