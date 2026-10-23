//! FILE streams over the C file descriptors (stdio contract). The buffer
//! keeps the classic protocol that msvcp140's basic_filebuf drives
//! through `_get_stream_buffer_pointers`: `_ptr` is the next byte,
//! `_cnt` the bytes left to read (reading) or the room left (writing),
//! `_base` the buffer; text-mode translation happens below, in lowio, and
//! ftell accounts for it exactly. Flag values are the UCRT's.

use super::lowio::{self, O_APPEND, O_BINARY, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TEXT, O_TRUNC, O_WRONLY};
use super::*;
use core::sync::atomic::AtomicI32;

const READ: u32 = 0x1;
const WRITE: u32 = 0x2;
const UPDATE: u32 = 0x4;
const EOF_FLAG: u32 = 0x8;
const ERROR: u32 = 0x10;
const BUF_CRT: u32 = 0x40;
const BUF_USER: u32 = 0x80;
const BUF_NONE: u32 = 0x400;
const COMMIT: u32 = 0x800;
const ALLOCATED: u32 = 0x2000;

pub const EOF: i32 = -1;
const WEOF: u16 = 0xFFFF;
const BUFSIZ: i32 = 4096;

const O_SEQUENTIAL: i32 = 0x20;
const O_RANDOM: i32 = 0x10;
const O_SHORT_LIVED: i32 = 0x1000;

/// The stream. The first fields are the classic `_iobuf` layout.
#[repr(C)]
pub struct File {
    ptr: *mut u8,
    base: *mut u8,
    cnt: i32,
    flags: u32,
    fd: i32,
    charbuf: i32,
    bufsiz: i32,
    tmpfname: *mut u8,
    lock: RLock,
    /// Text mode: raw file offset of each buffered byte (relative to the
    /// fill), then the raw bytes the fill consumed.
    map: Vec<u32>,
}

impl File {
    fn new() -> File {
        File {
            ptr: core::ptr::null_mut(),
            base: core::ptr::null_mut(),
            cnt: 0,
            flags: 0,
            fd: -1,
            charbuf: 0,
            bufsiz: 0,
            tmpfname: core::ptr::null_mut(),
            lock: RLock::new(),
            map: Vec::new(),
        }
    }

    pub fn lock(&self) {
        self.lock.lock()
    }

    pub fn unlock(&self) {
        self.lock.unlock();
    }

    fn pending(&self) -> usize {
        if self.flags & WRITE != 0 && !self.base.is_null() && self.flags & BUF_NONE == 0 {
            self.ptr as usize - self.base as usize
        } else {
            0
        }
    }
}

/// Every stream in use (for fflush(NULL), _flushall and exit).
static STREAMS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static STD: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static STD_INIT: Mutex<bool> = Mutex::new(false);
static COMMODE: AtomicI32 = AtomicI32::new(0);
const PUBLIC_IOCOMMIT: i32 = 0x4000;

fn std_streams() {
    let mut done = STD_INIT.lock().unwrap();
    if *done {
        return;
    }
    for (i, slot) in STD.iter().enumerate() {
        let mut f = Box::new(File::new());
        f.fd = i as i32;
        f.flags = ALLOCATED | if i == 0 { READ } else { WRITE };
        // stderr, and stdout on a character device, write through at
        // once (the UCRT flushes them at the end of every call).
        if i == 2 || (i == 1 && lowio::is_device(1)) {
            f.flags |= BUF_NONE;
        }
        let p = Box::into_raw(f) as usize;
        slot.store(p as u64, Ordering::SeqCst);
        STREAMS.lock().unwrap().push(p);
    }
    *done = true;
}

pub(super) extern "win64" fn __acrt_iob_func(i: u32) -> *mut File {
    std_streams();
    if i < 3 { STD[i as usize].load(Ordering::SeqCst) as *mut File } else { core::ptr::null_mut() }
}

fn is_std(f: *mut File) -> bool {
    STD.iter().any(|s| s.load(Ordering::SeqCst) == f as u64)
}

/// The stream behind a FILE pointer from the program.
fn file<'a>(f: *mut File) -> &'a mut File {
    // SAFETY: a stream of this runtime (validated non-NULL by callers).
    unsafe { &mut *f }
}

// ---- buffering core (callers hold the stream lock) ---------------------------------------

fn ensure_buf(f: &mut File) {
    if !f.base.is_null() {
        return;
    }
    if f.flags & BUF_NONE == 0 {
        let b = crt_malloc(BUFSIZ as usize);
        if !b.is_null() {
            f.base = b;
            f.bufsiz = BUFSIZ;
            f.flags |= BUF_CRT;
        } else {
            f.flags |= BUF_NONE;
        }
    }
    if f.base.is_null() {
        f.base = &mut f.charbuf as *mut i32 as *mut u8;
        f.bufsiz = 1;
    }
    f.ptr = f.base;
    f.cnt = 0;
}

/// Writes out pending output. 0 or EOF.
fn flush_nolock(f: &mut File) -> i32 {
    let mut r = 0;
    let n = f.pending();
    if n > 0 {
        // SAFETY: [base, ptr) are buffered bytes.
        let data = unsafe { core::slice::from_raw_parts(f.base, n) };
        match lowio::write(f.fd, data) {
            Ok(k) if k == n => {}
            Ok(_) | Err(_) => {
                f.flags |= ERROR;
                r = EOF;
            }
        }
    }
    if f.flags & WRITE != 0 {
        f.ptr = f.base;
        f.cnt = 0;
        if f.flags & UPDATE != 0 {
            f.flags &= !WRITE;
        }
    }
    if r == 0 && f.flags & COMMIT != 0 {
        lowio::commit(f.fd);
    }
    r
}

/// Prepares a write; false when the stream cannot be written now.
fn begin_write(f: &mut File) -> bool {
    if f.flags & (WRITE | UPDATE) == 0 {
        set_errno(EBADF);
        f.flags |= ERROR;
        return false;
    }
    if f.flags & READ != 0 {
        // Switching from reading needs end of file (or a seek first).
        f.cnt = 0;
        if f.flags & EOF_FLAG == 0 {
            f.flags |= ERROR;
            return false;
        }
        f.ptr = f.base;
        f.flags &= !READ;
    }
    f.flags |= WRITE;
    f.flags &= !EOF_FLAG;
    ensure_buf(f);
    true
}

/// `_flsbuf`: stores `c` when the buffer is full or absent.
fn flsbuf(c: i32, f: &mut File) -> i32 {
    if !begin_write(f) {
        return EOF;
    }
    let b = c as u8;
    if f.flags & BUF_NONE != 0 {
        return match lowio::write(f.fd, &[b]) {
            Ok(1) => b as i32,
            _ => {
                f.flags |= ERROR;
                EOF
            }
        };
    }
    let n = f.ptr as usize - f.base as usize;
    if n > 0 {
        // SAFETY: [base, ptr) are buffered bytes.
        let data = unsafe { core::slice::from_raw_parts(f.base, n) };
        if lowio::write(f.fd, data) != Ok(n) {
            f.flags |= ERROR;
            f.ptr = f.base;
            f.cnt = 0;
            return EOF;
        }
    }
    // SAFETY: the buffer holds bufsiz >= 1 bytes.
    unsafe { *f.base = b };
    // SAFETY: as above.
    f.ptr = unsafe { f.base.add(1) };
    f.cnt = f.bufsiz - 1;
    b as i32
}

fn putc_nolock(c: i32, f: &mut File) -> i32 {
    if f.flags & WRITE != 0 && f.cnt > 0 {
        // SAFETY: `cnt` bytes of room at `ptr`.
        unsafe {
            *f.ptr = c as u8;
            f.ptr = f.ptr.add(1);
        }
        f.cnt -= 1;
        return c & 0xFF;
    }
    flsbuf(c, f)
}

/// `_filbuf`: refills the buffer and returns its first byte.
fn filbuf(f: &mut File) -> i32 {
    if f.flags & (READ | UPDATE) == 0 || f.flags & WRITE != 0 {
        f.flags |= ERROR;
        return EOF;
    }
    f.flags |= READ;
    ensure_buf(f);
    // SAFETY: the buffer holds `bufsiz` bytes.
    let buf = unsafe { core::slice::from_raw_parts_mut(f.base, f.bufsiz as usize) };
    let text = lowio::is_text(f.fd);
    let r = if text { lowio::read(f.fd, buf, Some(&mut f.map)) } else { lowio::read(f.fd, buf, None) };
    match r {
        Ok(0) => {
            f.flags |= EOF_FLAG;
            f.cnt = 0;
            f.map.clear();
            EOF
        }
        Ok(n) => {
            if !text {
                f.map.clear();
            }
            f.ptr = f.base;
            f.cnt = n as i32 - 1;
            // SAFETY: n >= 1 bytes were read.
            let c = unsafe { *f.ptr };
            // SAFETY: as above.
            f.ptr = unsafe { f.ptr.add(1) };
            c as i32
        }
        Err(e) => {
            set_errno(e);
            f.flags |= ERROR;
            f.cnt = 0;
            EOF
        }
    }
}

fn getc_nolock(f: &mut File) -> i32 {
    if f.flags & READ != 0 && f.cnt > 0 {
        f.cnt -= 1;
        // SAFETY: `cnt` unread bytes at `ptr`.
        let c = unsafe { *f.ptr };
        // SAFETY: as above.
        f.ptr = unsafe { f.ptr.add(1) };
        return c as i32;
    }
    filbuf(f)
}

/// Writes bytes through the buffer; returns how many were written.
pub fn write_nolock(data: &[u8], f: &mut File) -> usize {
    let mut done = 0;
    while done < data.len() {
        if f.flags & WRITE != 0 && f.cnt > 0 {
            let k = (f.cnt as usize).min(data.len() - done);
            // SAFETY: `cnt` bytes of room at `ptr`.
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr().add(done), f.ptr, k);
                f.ptr = f.ptr.add(k);
            }
            f.cnt -= k as i32;
            done += k;
            continue;
        }
        let rest = data.len() - done;
        let big = f.flags & BUF_NONE != 0 || (f.bufsiz > 0 && rest >= f.bufsiz as usize);
        if big && f.flags & WRITE != 0 && !f.base.is_null() {
            // Bypass the buffer: flush, then write directly.
            if flush_nolock(f) != 0 || !begin_write(f) {
                return done;
            }
            let k = if f.flags & BUF_NONE != 0 { rest } else { rest - rest % f.bufsiz as usize };
            match lowio::write(f.fd, &data[done..done + k]) {
                Ok(w) => {
                    done += w;
                    if w < k {
                        f.flags |= ERROR;
                        return done;
                    }
                }
                Err(_) => {
                    f.flags |= ERROR;
                    return done;
                }
            }
            continue;
        }
        if flsbuf(data[done] as i32, f) == EOF {
            return done;
        }
        done += 1;
    }
    done
}

/// Reads bytes through the buffer; returns how many were read.
fn read_nolock(out: &mut [u8], f: &mut File) -> usize {
    let mut done = 0;
    while done < out.len() {
        if f.flags & READ != 0 && f.cnt > 0 {
            let k = (f.cnt as usize).min(out.len() - done);
            // SAFETY: `cnt` unread bytes at `ptr`.
            unsafe {
                core::ptr::copy_nonoverlapping(f.ptr, out.as_mut_ptr().add(done), k);
                f.ptr = f.ptr.add(k);
            }
            f.cnt -= k as i32;
            done += k;
            continue;
        }
        let c = filbuf(f);
        if c == EOF {
            break;
        }
        out[done] = c as u8;
        done += 1;
    }
    done
}

/// The file position of the stream (buffered bytes accounted for).
fn tell_nolock(f: &mut File) -> i64 {
    if lowio::handle_of(f.fd).is_none() {
        set_errno(EBADF);
        return -1;
    }
    if f.cnt < 0 {
        f.cnt = 0;
    }
    let pos = match lowio::seek(f.fd, 0, 1) {
        Ok(p) => p,
        Err(e) => {
            set_errno(e);
            return -1;
        }
    };
    if f.flags & READ != 0 && !f.base.is_null() {
        let at = f.ptr as usize - f.base as usize;
        let unread = if !f.map.is_empty() && at < f.map.len() {
            (f.map[f.map.len() - 1] - f.map[at]) as i64
        } else {
            f.cnt as i64
        };
        return pos - unread;
    }
    let n = f.pending();
    if n > 0 {
        // SAFETY: [base, ptr) are buffered bytes.
        let data = unsafe { core::slice::from_raw_parts(f.base, n) };
        let extra = if lowio::is_text(f.fd) { data.iter().filter(|&&b| b == b'\n').count() } else { 0 };
        let base = if lowio::is_append(f.fd) { lowio::seek(f.fd, 0, 2).unwrap_or(pos) } else { pos };
        return base + (n + extra) as i64;
    }
    pos
}

fn seek_nolock(f: &mut File, off: i64, whence: i32) -> i32 {
    let (mut off, mut whence) = (off, whence);
    if whence == 1 {
        let cur = tell_nolock(f);
        if cur < 0 {
            return -1;
        }
        off += cur;
        whence = 0;
    }
    if flush_nolock(f) != 0 {
        return -1;
    }
    f.flags &= !EOF_FLAG;
    if f.flags & UPDATE != 0 {
        f.flags &= !(READ | WRITE);
    }
    f.ptr = f.base;
    f.cnt = 0;
    f.map.clear();
    match lowio::seek(f.fd, off, whence) {
        Ok(_) => 0,
        Err(e) => {
            set_errno(e);
            -1
        }
    }
}

fn ungetc_nolock(c: i32, f: &mut File) -> i32 {
    if c == EOF {
        return EOF;
    }
    if f.flags & READ == 0 && !(f.flags & UPDATE != 0 && f.flags & WRITE == 0) {
        return EOF;
    }
    ensure_buf(f);
    if f.ptr == f.base {
        if f.cnt != 0 {
            return EOF;
        }
        // SAFETY: the buffer holds at least one byte.
        f.ptr = unsafe { f.ptr.add(1) };
        f.map.clear();
    }
    // SAFETY: ptr > base, inside the buffer.
    unsafe {
        f.ptr = f.ptr.sub(1);
        *f.ptr = c as u8;
    }
    f.cnt += 1;
    f.flags &= !EOF_FLAG;
    f.flags |= READ;
    c & 0xFF
}

/// fputwc: in text mode the character goes out in the locale's multibyte
/// form (EILSEQ if it has none), in binary mode as its two bytes.
pub fn putwc_nolock(wc: u16, f: &mut File) -> u16 {
    if lowio::is_text(f.fd) {
        let mut mb = Vec::new();
        if super::locale::wide_to_mb(&[wc], &mut mb).is_err() {
            set_errno(EILSEQ);
            return WEOF;
        }
        if write_nolock(&mb, f) != mb.len() {
            return WEOF;
        }
    } else if write_nolock(&wc.to_le_bytes(), f) != 2 {
        return WEOF;
    }
    wc
}

fn getwc_nolock(f: &mut File) -> u16 {
    if !lowio::is_text(f.fd) {
        let lo = getc_nolock(f);
        if lo == EOF {
            return WEOF;
        }
        let hi = getc_nolock(f);
        if hi == EOF {
            return WEOF;
        }
        return (lo as u16) | ((hi as u16) << 8);
    }
    let c = getc_nolock(f);
    if c == EOF {
        return WEOF;
    }
    let mut bytes = alloc::vec![c as u8];
    if super::locale::ctype_utf8() && c >= 0x80 {
        let n = match c {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        };
        while bytes.len() < n {
            let d = getc_nolock(f);
            if d == EOF {
                break;
            }
            bytes.push(d as u8);
        }
    }
    let mut w = Vec::new();
    match super::locale::mb_to_wide(&bytes, &mut w) {
        Ok(()) if w.len() == 1 => w[0],
        _ => {
            set_errno(EILSEQ);
            WEOF
        }
    }
}

fn close_nolock(f: &mut File) -> i32 {
    let mut r = 0;
    if f.flags & ALLOCATED == 0 {
        return EOF;
    }
    if flush_nolock(f) != 0 {
        r = EOF;
    }
    if lowio::close(f.fd).is_err() {
        r = EOF;
    }
    if f.flags & BUF_CRT != 0 {
        crt_free(f.base);
    }
    if !f.tmpfname.is_null() {
        crt_free(f.tmpfname);
    }
    f.base = core::ptr::null_mut();
    f.ptr = core::ptr::null_mut();
    f.cnt = 0;
    f.bufsiz = 0;
    f.flags = 0;
    f.fd = -1;
    f.tmpfname = core::ptr::null_mut();
    f.map.clear();
    r
}

/// Flushes every stream (exit, _flushall).
pub fn flush_all() -> i32 {
    let list: Vec<usize> = STREAMS.lock().unwrap().clone();
    let mut n = 0;
    for p in list {
        let f = file(p as *mut File);
        f.lock();
        if f.flags & ALLOCATED != 0 {
            n += 1;
            flush_nolock(f);
        }
        f.unlock();
    }
    n
}

// ---- opening -----------------------------------------------------------------------------

/// Parses an fopen mode. Ok((open flags, stream flags)); Err(true) for an
/// invalid mode, Err(false) for a valid one OSyx does not provide.
fn parse_mode(mode: &[u16]) -> Result<(i32, u32), bool> {
    // `mode` carries its terminator; the mode ends there.
    let mut it = mode.iter().map(|&c| c as u32).take_while(|&c| c != 0).skip_while(|&c| c == b' ' as u32).peekable();
    let (mut oflag, mut flags) = match it.next().and_then(char::from_u32) {
        Some('r') => (O_RDONLY, READ),
        Some('w') => (O_WRONLY | O_CREAT | O_TRUNC, WRITE),
        Some('a') => (O_WRONLY | O_CREAT | O_APPEND, WRITE),
        _ => return Err(true),
    };
    let (mut plus, mut tb, mut commit, mut seqrand, mut temp, mut excl) = (false, false, false, false, false, false);
    if COMMODE.load(Ordering::SeqCst) & PUBLIC_IOCOMMIT != 0 {
        flags |= COMMIT;
    }
    while let Some(c) = it.next().and_then(char::from_u32) {
        match c {
            '+' if !plus => {
                plus = true;
                oflag = (oflag & !3) | O_RDWR;
                flags = UPDATE;
            }
            't' if !tb => {
                tb = true;
                oflag |= O_TEXT;
            }
            'b' if !tb => {
                tb = true;
                oflag |= O_BINARY;
            }
            'c' if !commit => {
                commit = true;
                flags |= COMMIT;
            }
            'n' if !commit => {
                commit = true;
                flags &= !COMMIT;
            }
            'S' if !seqrand => {
                seqrand = true;
                oflag |= O_SEQUENTIAL;
            }
            'R' if !seqrand => {
                seqrand = true;
                oflag |= O_RANDOM;
            }
            'T' => oflag |= O_SHORT_LIVED,
            'D' if !temp => {
                temp = true;
                oflag |= lowio::O_TEMPORARY;
            }
            'N' => oflag |= lowio::O_NOINHERIT,
            'x' if !excl && oflag & O_TRUNC != 0 => {
                excl = true;
                oflag |= O_EXCL;
            }
            ' ' => {}
            // ",ccs=UTF-8" etc.: Unicode stream encodings are not provided.
            ',' => return Err(false),
            _ => return Err(true),
        }
    }
    Ok((oflag, flags))
}

/// Opens into `f` (an unused stream). Err(errno).
fn open_into(f: &mut File, path: &[u16], mode: &[u16], shflag: i32) -> Result<(), i32> {
    let (oflag, flags) = match parse_mode(mode) {
        Ok(m) => m,
        Err(true) => {
            set_errno(EINVAL);
            invalid_parameter();
            return Err(EINVAL);
        }
        Err(false) => return Err(EINVAL),
    };
    let fd = lowio::open(path, oflag, shflag, 0x180)?;
    *f = File::new();
    f.fd = fd;
    f.flags = flags | ALLOCATED;
    Ok(())
}

fn register(f: Box<File>) -> *mut File {
    let p = Box::into_raw(f);
    STREAMS.lock().unwrap().push(p as usize);
    p
}

fn unregister(f: *mut File) {
    STREAMS.lock().unwrap().retain(|&p| p != f as usize);
}

fn fsopen(path: Option<Vec<u16>>, mode: Option<Vec<u16>>, shflag: i32) -> Result<*mut File, i32> {
    let (Some(path), Some(mode)) = (path, mode) else {
        set_errno(EINVAL);
        invalid_parameter();
        return Err(EINVAL);
    };
    if mode.len() <= 1 {
        set_errno(EINVAL);
        invalid_parameter();
        return Err(EINVAL);
    }
    if path.len() <= 1 {
        set_errno(EINVAL);
        return Err(EINVAL);
    }
    let mut f = Box::new(File::new());
    open_into(&mut f, &path, &mode, shflag).inspect_err(|&e| set_errno(e))?;
    Ok(register(f))
}

fn narrow_arg(p: *const u8) -> Option<Vec<u16>> {
    if p.is_null() { None } else { Some(super::fs::widen_path(p)) }
}

fn wide_arg(p: *const u16) -> Option<Vec<u16>> {
    if p.is_null() { None } else { Some(super::fs::wide_path(p)) }
}

extern "win64" fn fopen(path: *const u8, mode: *const u8) -> *mut File {
    fsopen(narrow_arg(path), narrow_arg(mode), lowio::SH_DENYNO).unwrap_or(core::ptr::null_mut())
}

extern "win64" fn _wfopen(path: *const u16, mode: *const u16) -> *mut File {
    fsopen(wide_arg(path), wide_arg(mode), lowio::SH_DENYNO).unwrap_or(core::ptr::null_mut())
}

extern "win64" fn _fsopen(path: *const u8, mode: *const u8, sh: i32) -> *mut File {
    fsopen(narrow_arg(path), narrow_arg(mode), sh).unwrap_or(core::ptr::null_mut())
}

extern "win64" fn _wfsopen(path: *const u16, mode: *const u16, sh: i32) -> *mut File {
    fsopen(wide_arg(path), wide_arg(mode), sh).unwrap_or(core::ptr::null_mut())
}

fn fopen_s(out: *mut *mut File, path: Option<Vec<u16>>, mode: Option<Vec<u16>>) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    match fsopen(path, mode, lowio::SH_SECURE) {
        Ok(f) => {
            // SAFETY: validated.
            unsafe { *out = f };
            0
        }
        Err(e) => {
            // SAFETY: validated.
            unsafe { *out = core::ptr::null_mut() };
            e
        }
    }
}

extern "win64" fn fopen_s_(out: *mut *mut File, path: *const u8, mode: *const u8) -> i32 {
    fopen_s(out, narrow_arg(path), narrow_arg(mode))
}

extern "win64" fn _wfopen_s(out: *mut *mut File, path: *const u16, mode: *const u16) -> i32 {
    fopen_s(out, wide_arg(path), wide_arg(mode))
}

fn reopen(path: Option<Vec<u16>>, mode: Option<Vec<u16>>, f: *mut File) -> *mut File {
    validate!(!f.is_null(), EINVAL, core::ptr::null_mut());
    let (Some(path), Some(mode)) = (path, mode) else { return invalid(EINVAL, core::ptr::null_mut()) };
    let s = file(f);
    s.lock();
    close_nolock(s);
    let r = open_into(s, &path, &mode, lowio::SH_DENYNO);
    s.unlock();
    match r {
        Ok(()) => f,
        Err(e) => {
            set_errno(e);
            core::ptr::null_mut()
        }
    }
}

extern "win64" fn freopen(path: *const u8, mode: *const u8, f: *mut File) -> *mut File {
    reopen(narrow_arg(path), narrow_arg(mode), f)
}

extern "win64" fn _wfreopen(path: *const u16, mode: *const u16, f: *mut File) -> *mut File {
    reopen(wide_arg(path), wide_arg(mode), f)
}

fn fdopen(fd: i32, mode: Option<Vec<u16>>) -> *mut File {
    let Some(mode) = mode else { return invalid(EINVAL, core::ptr::null_mut()) };
    validate!(lowio::handle_of(fd).is_some(), EBADF, core::ptr::null_mut());
    let flags = match parse_mode(&mode) {
        Ok((_, fl)) => fl,
        Err(true) => return invalid(EINVAL, core::ptr::null_mut()),
        Err(false) => {
            set_errno(EINVAL);
            return core::ptr::null_mut();
        }
    };
    let mut f = Box::new(File::new());
    f.fd = fd;
    f.flags = flags | ALLOCATED;
    register(f)
}

extern "win64" fn _fdopen(fd: i32, mode: *const u8) -> *mut File {
    fdopen(fd, narrow_arg(mode))
}

extern "win64" fn _wfdopen(fd: i32, mode: *const u16) -> *mut File {
    fdopen(fd, wide_arg(mode))
}

extern "win64" fn fclose(f: *mut File) -> i32 {
    validate!(!f.is_null(), EINVAL, EOF);
    let s = file(f);
    s.lock();
    let r = close_nolock(s);
    s.unlock();
    if !is_std(f) {
        unregister(f);
        // SAFETY: a stream made by `register`, now closed and unlisted.
        drop(unsafe { Box::from_raw(f) });
    }
    r
}

extern "win64" fn _fcloseall() -> i32 {
    let list: Vec<usize> = STREAMS.lock().unwrap().clone();
    let mut n = 0;
    for p in list {
        let f = p as *mut File;
        if !is_std(f) && file(f).flags & ALLOCATED != 0 {
            fclose(f);
            n += 1;
        }
    }
    n
}

// ---- operations --------------------------------------------------------------------------

/// Runs `op` on a validated stream under its lock.
fn with<R>(f: *mut File, fail: R, op: impl FnOnce(&mut File) -> R) -> R {
    validate!(!f.is_null(), EINVAL, fail);
    let s = file(f);
    s.lock();
    let r = op(s);
    s.unlock();
    r
}

extern "win64" fn fflush(f: *mut File) -> i32 {
    if f.is_null() {
        let list: Vec<usize> = STREAMS.lock().unwrap().clone();
        let mut r = 0;
        for p in list {
            let s = file(p as *mut File);
            s.lock();
            if s.flags & ALLOCATED != 0 && s.flags & WRITE != 0 && flush_nolock(s) != 0 {
                r = EOF;
            }
            s.unlock();
        }
        return r;
    }
    with(f, EOF, flush_nolock)
}

extern "win64" fn _flushall() -> i32 {
    flush_all()
}

extern "win64" fn fputc(c: i32, f: *mut File) -> i32 {
    with(f, EOF, |s| putc_nolock(c, s))
}

pub(super) extern "win64" fn fgetc(f: *mut File) -> i32 {
    with(f, EOF, getc_nolock)
}

extern "win64" fn putchar(c: i32) -> i32 {
    fputc(c, __acrt_iob_func(1))
}

extern "win64" fn getchar() -> i32 {
    fgetc(__acrt_iob_func(0))
}

pub(super) extern "win64" fn ungetc(c: i32, f: *mut File) -> i32 {
    with(f, EOF, |s| ungetc_nolock(c, s))
}

extern "win64" fn fputs(s: *const u8, f: *mut File) -> i32 {
    validate!(!s.is_null(), EINVAL, EOF);
    // SAFETY: a NUL-terminated string.
    let b = unsafe { cstr(s) };
    with(f, EOF, |st| if write_nolock(b, st) == b.len() { 0 } else { EOF })
}

extern "win64" fn puts(s: *const u8) -> i32 {
    validate!(!s.is_null(), EINVAL, EOF);
    // SAFETY: a NUL-terminated string.
    let b = unsafe { cstr(s) };
    with(__acrt_iob_func(1), EOF, |st| {
        if write_nolock(b, st) == b.len() && putc_nolock(b'\n' as i32, st) != EOF { 0 } else { EOF }
    })
}

extern "win64" fn fgets(buf: *mut u8, n: i32, f: *mut File) -> *mut u8 {
    validate!(!buf.is_null() || n == 0, EINVAL, core::ptr::null_mut());
    validate!(n >= 0, EINVAL, core::ptr::null_mut());
    validate!(!f.is_null(), EINVAL, core::ptr::null_mut());
    if n == 0 {
        return core::ptr::null_mut();
    }
    with(f, core::ptr::null_mut(), |s| {
        let mut i = 0usize;
        while i + 1 < n as usize {
            let c = getc_nolock(s);
            if c == EOF {
                if i == 0 {
                    return core::ptr::null_mut();
                }
                break;
            }
            // SAFETY: i < n - 1 within the caller's buffer.
            unsafe { *buf.add(i) = c as u8 };
            i += 1;
            if c == b'\n' as i32 {
                break;
            }
        }
        // SAFETY: i <= n - 1.
        unsafe { *buf.add(i) = 0 };
        buf
    })
}

extern "win64" fn fread(buf: *mut u8, size: usize, count: usize, f: *mut File) -> usize {
    if size == 0 || count == 0 {
        return 0;
    }
    validate!(!buf.is_null(), EINVAL, 0);
    validate!(!f.is_null(), EINVAL, 0);
    validate!(count <= usize::MAX / size, EINVAL, 0);
    // SAFETY: the caller's buffer of size * count bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, size * count) };
    with(f, 0, |s| read_nolock(out, s) / size)
}

extern "win64" fn fwrite(buf: *const u8, size: usize, count: usize, f: *mut File) -> usize {
    if size == 0 || count == 0 {
        return 0;
    }
    validate!(!buf.is_null(), EINVAL, 0);
    validate!(!f.is_null(), EINVAL, 0);
    validate!(count <= usize::MAX / size, EINVAL, 0);
    // SAFETY: the caller's data of size * count bytes.
    let data = unsafe { core::slice::from_raw_parts(buf, size * count) };
    with(f, 0, |s| write_nolock(data, s) / size)
}

extern "win64" fn fputwc(c: u16, f: *mut File) -> u16 {
    with(f, WEOF, |s| putwc_nolock(c, s))
}

pub(super) extern "win64" fn fgetwc(f: *mut File) -> u16 {
    with(f, WEOF, getwc_nolock)
}

pub(super) extern "win64" fn ungetwc(c: u16, f: *mut File) -> u16 {
    if c == WEOF {
        return WEOF;
    }
    with(f, WEOF, |s| {
        let bytes = if lowio::is_text(s.fd) {
            let mut mb = Vec::new();
            if super::locale::wide_to_mb(&[c], &mut mb).is_err() {
                return WEOF;
            }
            mb
        } else {
            c.to_le_bytes().to_vec()
        };
        // All bytes must fit in front of the read position.
        let room = if s.base.is_null() { 1 } else { s.ptr as usize - s.base as usize };
        if bytes.len() > 1 && room < bytes.len() {
            return WEOF;
        }
        for &b in bytes.iter().rev() {
            if ungetc_nolock(b as i32, s) == EOF {
                return WEOF;
            }
        }
        c
    })
}

extern "win64" fn fputws(s: *const u16, f: *mut File) -> i32 {
    validate!(!s.is_null(), EINVAL, EOF);
    // SAFETY: a NUL-terminated string.
    let w = unsafe { wstr(s) };
    with(f, EOF, |st| if w.iter().all(|&u| putwc_nolock(u, st) != WEOF) { 0 } else { EOF })
}

extern "win64" fn fgetws(buf: *mut u16, n: i32, f: *mut File) -> *mut u16 {
    validate!(!buf.is_null() || n == 0, EINVAL, core::ptr::null_mut());
    validate!(n >= 0 && !f.is_null(), EINVAL, core::ptr::null_mut());
    if n == 0 {
        return core::ptr::null_mut();
    }
    with(f, core::ptr::null_mut(), |s| {
        let mut i = 0usize;
        while i + 1 < n as usize {
            let c = getwc_nolock(s);
            if c == WEOF {
                if i == 0 {
                    return core::ptr::null_mut();
                }
                break;
            }
            // SAFETY: within the caller's buffer.
            unsafe { *buf.add(i) = c };
            i += 1;
            if c == b'\n' as u16 {
                break;
            }
        }
        // SAFETY: i <= n - 1.
        unsafe { *buf.add(i) = 0 };
        buf
    })
}

extern "win64" fn _fseeki64(f: *mut File, off: i64, whence: i32) -> i32 {
    validate!(!f.is_null(), EINVAL, -1);
    validate!((0..=2).contains(&whence), EINVAL, -1);
    with(f, -1, |s| seek_nolock(s, off, whence))
}

extern "win64" fn fseek(f: *mut File, off: i32, whence: i32) -> i32 {
    _fseeki64(f, off as i64, whence)
}

extern "win64" fn _ftelli64(f: *mut File) -> i64 {
    with(f, -1, tell_nolock)
}

extern "win64" fn ftell(f: *mut File) -> i32 {
    let p = _ftelli64(f);
    if p > i32::MAX as i64 {
        set_errno(EINVAL);
        return -1;
    }
    p as i32
}

extern "win64" fn fgetpos(f: *mut File, pos: *mut i64) -> i32 {
    validate!(!f.is_null() && !pos.is_null(), EINVAL, -1);
    let p = _ftelli64(f);
    if p < 0 {
        return -1;
    }
    // SAFETY: validated.
    unsafe { *pos = p };
    0
}

extern "win64" fn fsetpos(f: *mut File, pos: *const i64) -> i32 {
    validate!(!f.is_null() && !pos.is_null(), EINVAL, -1);
    // SAFETY: validated.
    _fseeki64(f, unsafe { *pos }, 0)
}

extern "win64" fn rewind(f: *mut File) {
    with(f, (), |s| {
        seek_nolock(s, 0, 0);
        s.flags &= !(EOF_FLAG | ERROR);
    })
}

extern "win64" fn feof(f: *mut File) -> i32 {
    with(f, 0, |s| (s.flags & EOF_FLAG) as i32)
}

extern "win64" fn ferror(f: *mut File) -> i32 {
    with(f, 0, |s| (s.flags & ERROR) as i32)
}

extern "win64" fn clearerr(f: *mut File) {
    with(f, (), |s| s.flags &= !(EOF_FLAG | ERROR))
}

const IOFBF: i32 = 0;
const IOLBF: i32 = 0x40;
const IONBF: i32 = 4;

extern "win64" fn setvbuf(f: *mut File, buf: *mut u8, mode: i32, size: usize) -> i32 {
    validate!(!f.is_null(), EINVAL, -1);
    validate!(mode == IOFBF || mode == IOLBF || mode == IONBF, EINVAL, -1);
    if mode != IONBF {
        validate!((2..=i32::MAX as usize).contains(&size), EINVAL, -1);
    }
    with(f, -1, |s| {
        flush_nolock(s);
        if s.flags & BUF_CRT != 0 {
            crt_free(s.base);
        }
        s.flags &= !(BUF_CRT | BUF_USER | BUF_NONE);
        // READ/WRITE are the access mode of an "r"/"w" stream; only an
        // update stream's current direction is reset.
        if s.flags & UPDATE != 0 {
            s.flags &= !(READ | WRITE);
        }
        s.base = core::ptr::null_mut();
        s.map.clear();
        if mode == IONBF {
            s.flags |= BUF_NONE;
            ensure_buf(s);
            return 0;
        }
        let size = size & !1;
        if buf.is_null() {
            let b = crt_malloc(size);
            if b.is_null() {
                return -1;
            }
            s.base = b;
            s.flags |= BUF_CRT;
        } else {
            s.base = buf;
            s.flags |= BUF_USER;
        }
        s.bufsiz = size as i32;
        s.ptr = s.base;
        s.cnt = 0;
        0
    })
}

extern "win64" fn setbuf(f: *mut File, buf: *mut u8) {
    if buf.is_null() {
        setvbuf(f, buf, IONBF, 0);
    } else {
        setvbuf(f, buf, IOFBF, 512);
    }
}

extern "win64" fn _fileno(f: *mut File) -> i32 {
    validate!(!f.is_null(), EINVAL, -1);
    file(f).fd
}

extern "win64" fn _get_stream_buffer_pointers(f: *mut File, base: *mut u64, ptr: *mut u64, cnt: *mut u64) -> i32 {
    validate!(!f.is_null(), EINVAL, EINVAL);
    let s = file(f);
    // SAFETY: the caller's optional out pointers receive field addresses.
    unsafe {
        if !base.is_null() {
            *base = &mut s.base as *mut *mut u8 as u64;
        }
        if !ptr.is_null() {
            *ptr = &mut s.ptr as *mut *mut u8 as u64;
        }
        if !cnt.is_null() {
            *cnt = &mut s.cnt as *mut i32 as u64;
        }
    }
    0
}

extern "win64" fn _lock_file(f: *mut File) {
    if !f.is_null() {
        file(f).lock();
    }
}

extern "win64" fn _unlock_file(f: *mut File) {
    if !f.is_null() {
        file(f).unlock();
    }
}

extern "win64" fn __p__commode() -> u64 {
    COMMODE.as_ptr() as u64
}

static MAXSTDIO: AtomicI32 = AtomicI32::new(512);

extern "win64" fn _setmaxstdio(n: i32) -> i32 {
    validate!((20..=8192).contains(&n), EINVAL, -1);
    MAXSTDIO.store(n, Ordering::SeqCst);
    n
}

extern "win64" fn _getmaxstdio() -> i32 {
    MAXSTDIO.load(Ordering::SeqCst)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![
        __acrt_iob_func,
        fopen,
        _wfopen,
        _fsopen,
        _wfsopen,
        _wfopen_s,
        freopen,
        _wfreopen,
        _fdopen,
        _wfdopen,
        fclose,
        _fcloseall,
        fflush,
        _flushall,
        fputc,
        fgetc,
        putchar,
        getchar,
        ungetc,
        fputs,
        puts,
        fgets,
        fread,
        fwrite,
        fputwc,
        fgetwc,
        ungetwc,
        fputws,
        fgetws,
        _fseeki64,
        fseek,
        _ftelli64,
        ftell,
        fgetpos,
        fsetpos,
        rewind,
        feof,
        ferror,
        clearerr,
        setvbuf,
        setbuf,
        _fileno,
        _get_stream_buffer_pointers,
        _lock_file,
        _unlock_file,
        __p__commode,
        _setmaxstdio,
        _getmaxstdio,
    ];
    v.extend([
        ("fopen_s", fopen_s_ as *const () as u64),
        ("getc", fgetc as *const () as u64),
        ("putc", fputc as *const () as u64),
        ("getwc", fgetwc as *const () as u64),
        ("putwc", fputwc as *const () as u64),
    ]);
    v
}
