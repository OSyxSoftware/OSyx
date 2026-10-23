//! CRT diagnostics: errno messages, stderr output and failed assertions.
use super::*;

fn message(e: i32) -> &'static [u8] {
    match e {
        0 => b"No error",
        1 => b"Operation not permitted",
        ENOENT => b"No such file or directory",
        3 => b"No such process",
        4 => b"Interrupted function call",
        5 => b"Input/output error",
        6 => b"No such device or address",
        E2BIG => b"Arg list too long",
        ENOEXEC => b"Exec format error",
        EBADF => b"Bad file descriptor",
        ECHILD => b"No child processes",
        EAGAIN => b"Resource temporarily unavailable",
        ENOMEM => b"Not enough space",
        EACCES => b"Permission denied",
        14 => b"Bad address",
        16 => b"Resource device",
        EEXIST => b"File exists",
        EXDEV => b"Improper link",
        19 => b"No such device",
        20 => b"Not a directory",
        21 => b"Is a directory",
        EINVAL => b"Invalid argument",
        23 => b"Too many open files in system",
        EMFILE => b"Too many open files",
        25 => b"Inappropriate I/O control operation",
        27 => b"File too large",
        ENOSPC => b"No space left on device",
        29 => b"Invalid seek",
        30 => b"Read-only file system",
        31 => b"Too many links",
        EPIPE => b"Broken pipe",
        EDOM => b"Domain error",
        ERANGE => b"Result too large",
        36 => b"Resource deadlock avoided",
        38 => b"Filename too long",
        39 => b"No locks available",
        40 => b"Function not implemented",
        ENOTEMPTY => b"Directory not empty",
        EILSEQ => b"Illegal byte sequence",
        _ => b"Unknown error",
    }
}

extern "win64" fn strerror(e: i32) -> *mut u8 {
    let buf = &mut ptd().error_text;
    buf.clear();
    buf.extend_from_slice(message(e));
    buf.push(0);
    buf.as_mut_ptr()
}

extern "win64" fn strerror_s(buf: *mut u8, n: usize, e: i32) -> i32 {
    validate!(!buf.is_null() && n > 0, EINVAL, EINVAL);
    let s = message(e);
    let count = s.len().min(n - 1);
    // SAFETY: count + 1 <= n, and s is immutable storage.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, count);
        *buf.add(count) = 0;
    }
    // Like the UCRT, truncate the message without treating it as a failure.
    0
}

extern "win64" fn _wcserror(e: i32) -> *mut u16 {
    let s = message(e);
    let buf = &mut ptd().wide_error_text;
    buf.clear();
    buf.extend(s.iter().map(|&b| b as u16));
    buf.push(0);
    buf.as_mut_ptr()
}

extern "win64" fn _wcserror_s(buf: *mut u16, n: usize, e: i32) -> i32 {
    validate!(!buf.is_null() && n > 0, EINVAL, EINVAL);
    let s = message(e);
    // SAFETY: validated writable buffer.
    unsafe { *buf = 0 };
    validate!(s.len() < n, ERANGE, ERANGE);
    // SAFETY: s.len() + 1 <= n.
    unsafe {
        for (i, &b) in s.iter().enumerate() {
            *buf.add(i) = b as u16;
        }
        *buf.add(s.len()) = 0;
    }
    0
}

fn stderr(bytes: &[u8]) {
    let f = stdio::__acrt_iob_func(2);
    // SAFETY: the runtime's standard error stream.
    let file = unsafe { &mut *f };
    file.lock();
    stdio::write_nolock(bytes, file);
    file.unlock();
}

extern "win64" fn perror(prefix: *const u8) {
    let saved = errno();
    let mut s = Vec::new();
    if !prefix.is_null() {
        // SAFETY: optional NUL-terminated caller string.
        s.extend_from_slice(unsafe { cstr(prefix) });
    }
    if !s.is_empty() {
        s.extend_from_slice(b": ");
    }
    s.extend_from_slice(message(saved));
    s.push(b'\n');
    stderr(&s);
    set_errno(saved);
}

extern "win64" fn _wperror(prefix: *const u16) {
    let saved = errno();
    let mut s = Vec::new();
    if !prefix.is_null() {
        // SAFETY: optional NUL-terminated caller string.
        if locale::wide_to_mb(unsafe { wstr(prefix) }, &mut s).is_err() {
            set_errno(EILSEQ);
            return;
        }
    }
    s.push(0);
    perror(s.as_ptr());
    set_errno(saved);
}

extern "win64" fn _wassert(expr: *const u16, file: *const u16, line: u32) -> ! {
    let text = |p: *const u16| {
        if p.is_null() {
            alloc::string::String::from("(null)")
        } else {
            // SAFETY: assertion arguments are NUL-terminated strings.
            alloc::string::String::from_utf16_lossy(unsafe { wstr(p) })
        }
    };
    stderr(alloc::format!("Assertion failed: {}, file {}, line {}\n", text(expr), text(file), line).as_bytes());
    runtime::abort()
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![strerror, strerror_s, _wcserror, _wcserror_s, perror, _wperror, _wassert]
}
