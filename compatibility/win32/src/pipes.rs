//! Named pipes (\\.\pipe\NAME) over OSyx IPC channels, so the two ends
//! can live in different processes. CreateNamedPipe listens on the kernel
//! service "winpipe:<name>" (one listener per name, shared by this
//! process's instances); a client's CreateFile connects to it, and each
//! accepted connection serves one instance. Channels carry messages of at
//! most 64 KiB: byte-mode writes are split into such messages and reads
//! join them, message-mode writes must fit one.
//!
//! I/O on a handle opened with FILE_FLAG_OVERLAPPED and given an
//! OVERLAPPED runs on a helper thread: the call returns ERROR_IO_PENDING,
//! and completion is recorded in the OVERLAPPED, signals its event and is
//! queued to the completion port the handle is associated with (unless the
//! event handle's low bit is set). CancelIo/CancelIoEx abort the pending
//! operations of a pipe within one wait slice (50 ms).
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail, INVALID_HANDLE_VALUE},
    objects::{self, Object},
    teb,
};
use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use os_std::sync::Mutex;
use osyx_abi::Errno;

const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_BAD_PIPE: u32 = 230;
const ERROR_PIPE_BUSY: u32 = 231;
const ERROR_NO_DATA: u32 = 232;
const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_PIPE_LISTENING: u32 = 536;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_OPERATION_ABORTED: u32 = 995;

const STATUS_PENDING: u64 = 0x103;
const STATUS_BUFFER_OVERFLOW: u64 = 0x8000_0005;
const STATUS_PIPE_BROKEN: u64 = 0xC000_014B;
const STATUS_CANCELLED: u64 = 0xC000_0120;
const STATUS_PIPE_DISCONNECTED: u64 = 0xC000_00B0;
const STATUS_PIPE_LISTENING: u64 = 0xC000_00B3;
const STATUS_PIPE_CLOSING: u64 = 0xC000_00B1;

const PIPE_ACCESS_INBOUND: u32 = 0x1;
const PIPE_ACCESS_OUTBOUND: u32 = 0x2;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const PIPE_TYPE_MESSAGE: u32 = 0x4;
const PIPE_READMODE_MESSAGE: u32 = 0x2;
const PIPE_NOWAIT: u32 = 0x1;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x8;

const CHUNK: usize = osyx_abi::ipc::MAX_MESSAGE;
const SLICE_NS: u64 = 50_000_000;

/// A kernel handle closed with its last user.
struct Fd(u64);

impl Drop for Fd {
    fn drop(&mut self) {
        let _ = os_sys::close(self.0);
    }
}

/// Listeners of this process by service name, kept while an instance uses
/// them (each server instance holds one reference), with the instance
/// limit the first instance set.
static LISTENERS: Mutex<BTreeMap<String, (alloc::sync::Weak<Fd>, u32)>> = Mutex::new(BTreeMap::new());

pub struct PipeObj {
    server: bool,
    /// Writes are messages (PIPE_TYPE_MESSAGE).
    message_type: bool,
    read_message: AtomicBool,
    nowait: AtomicBool,
    overlapped: bool,
    listener: Option<Arc<Fd>>,
    /// The connection; None while a server instance waits for a client.
    chan: Mutex<Option<Arc<Fd>>>,
    /// The unread rest of the message last received.
    rest: Mutex<Vec<u8>>,
    /// The completion port and key this handle is associated with.
    port: Mutex<Option<(u64, u64)>>,
    /// Bumped by CancelIo; pending operations started earlier abort.
    cancel: AtomicU64,
    pending: AtomicU32,
    /// A disconnected server instance must be connected again.
    disconnected: AtomicBool,
}

/// The kernel service name of a pipe. Names are case-insensitive; the
/// kernel accepts letters, digits, '-', '_' and '.', so a name with other
/// characters, or too long a one, keeps its allowed characters and gains
/// a hash of the whole name.
fn service(name: &str) -> String {
    const PREFIX: &str = "winpipe.";
    let lower = name.to_lowercase();
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
    let full = format!("{PREFIX}{lower}");
    if lower.chars().all(allowed) && full.len() <= osyx_abi::ipc::MAX_SERVICE_NAME {
        return full;
    }
    let h = osyx_crypto::Sha256::digest(lower.as_bytes());
    let tail: String = h[..8].iter().map(|b| format!("{b:02x}")).collect();
    let keep = osyx_abi::ipc::MAX_SERVICE_NAME - PREFIX.len() - tail.len() - 1;
    let kept: String = lower.chars().map(|c| if allowed(c) { c } else { '_' }).take(keep).collect();
    format!("{PREFIX}{kept}-{tail}")
}

/// The pipe name in `\\.\pipe\NAME` (either slash), if `win` is one.
pub fn pipe_name(win: &str) -> Option<&str> {
    let n = win.replace('/', "\\");
    let prefix = "\\\\.\\pipe\\";
    if n.len() > prefix.len() && n[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&win[prefix.len()..])
    } else {
        None
    }
}

fn wait(fd: u64, events: u32, timeout: u64) -> Result<bool, u32> {
    let mut items = [os_sys::ipc::WaitItem { handle: fd, events: events | osyx_abi::ipc::WAIT_HANGUP, revents: 0 }];
    match os_sys::wait_handles(&mut items, timeout) {
        Ok(_) => Ok(items[0].revents != 0),
        Err(Errno::TimedOut) => Ok(false),
        Err(e) => Err(errno_to_win32(e)),
    }
}

impl PipeObj {
    fn chan(&self) -> Result<Arc<Fd>, u32> {
        self.chan.lock().unwrap().clone().ok_or(if self.server && !self.disconnected.load(Ordering::SeqCst) {
            ERROR_PIPE_LISTENING
        } else {
            ERROR_PIPE_NOT_CONNECTED
        })
    }

    /// Waits for `events` on `fd`, giving up when cancelled after `epoch`.
    fn block(&self, fd: u64, events: u32, epoch: u64) -> Result<(), u32> {
        loop {
            if self.cancel.load(Ordering::SeqCst) != epoch {
                return Err(ERROR_OPERATION_ABORTED);
            }
            if wait(fd, events, SLICE_NS)? {
                return Ok(());
            }
        }
    }

    /// Accepts a client on a server instance.
    fn connect(&self, epoch: u64, wait_for_client: bool) -> Result<(), u32> {
        let Some(l) = &self.listener else { return Err(ERROR_INVALID_FUNCTION) };
        if self.chan.lock().unwrap().is_some() {
            return Err(ERROR_PIPE_CONNECTED);
        }
        loop {
            match os_sys::channel_accept(l.0) {
                Ok(c) => {
                    *self.chan.lock().unwrap() = Some(Arc::new(Fd(c)));
                    self.disconnected.store(false, Ordering::SeqCst);
                    return Ok(());
                }
                Err(Errno::WouldBlock) if wait_for_client => self.block(l.0, osyx_abi::ipc::WAIT_READABLE, epoch)?,
                Err(Errno::WouldBlock) => return Err(ERROR_PIPE_LISTENING),
                Err(e) => return Err(errno_to_win32(e)),
            }
        }
    }

    /// Reads up to `out.len()` bytes: (count, whether the message has more).
    fn read(&self, out: &mut [u8], epoch: u64) -> Result<(usize, bool), u32> {
        let chan = self.chan()?;
        loop {
            {
                let mut rest = self.rest.lock().unwrap();
                if !rest.is_empty() {
                    let n = rest.len().min(out.len());
                    out[..n].copy_from_slice(&rest[..n]);
                    rest.drain(..n);
                    return Ok((n, self.read_message.load(Ordering::SeqCst) && !rest.is_empty()));
                }
            }
            let mut buf = alloc::vec![0u8; CHUNK];
            match os_sys::channel_recv(chan.0, &mut buf, &mut []) {
                Ok((n, _)) => {
                    if n == 0 && self.message_type {
                        return Ok((0, false)); // an empty message
                    }
                    buf.truncate(n);
                    *self.rest.lock().unwrap() = buf;
                }
                Err(Errno::WouldBlock) if self.nowait.load(Ordering::SeqCst) => return Err(ERROR_NO_DATA),
                Err(Errno::WouldBlock) => self.block(chan.0, osyx_abi::ipc::WAIT_READABLE, epoch)?,
                Err(Errno::BrokenPipe) => return Err(ERROR_BROKEN_PIPE),
                Err(e) => return Err(errno_to_win32(e)),
            }
        }
    }

    fn write(&self, data: &[u8], epoch: u64) -> Result<usize, u32> {
        let chan = self.chan()?;
        if self.message_type && data.len() > CHUNK {
            return Err(ERROR_NOT_SUPPORTED); // a message must fit one channel message
        }
        let chunks: Vec<&[u8]> = if data.is_empty() { alloc::vec![data] } else { data.chunks(CHUNK).collect() };
        if data.is_empty() && !self.message_type {
            return Ok(0);
        }
        for c in chunks {
            loop {
                match os_sys::channel_send(chan.0, c, &[]) {
                    Ok(()) => break,
                    Err(Errno::WouldBlock) => self.block(chan.0, osyx_abi::ipc::WAIT_WRITABLE, epoch)?,
                    Err(Errno::BrokenPipe) => return Err(ERROR_NO_DATA),
                    Err(e) => return Err(errno_to_win32(e)),
                }
            }
        }
        Ok(data.len())
    }
}

/// The NTSTATUS recorded in an OVERLAPPED for a Win32 result.
fn status_of(e: u32) -> u64 {
    match e {
        ERROR_MORE_DATA => STATUS_BUFFER_OVERFLOW,
        ERROR_BROKEN_PIPE => STATUS_PIPE_BROKEN,
        ERROR_OPERATION_ABORTED => STATUS_CANCELLED,
        ERROR_PIPE_NOT_CONNECTED => STATUS_PIPE_DISCONNECTED,
        ERROR_PIPE_LISTENING => STATUS_PIPE_LISTENING,
        ERROR_NO_DATA => STATUS_PIPE_CLOSING,
        _ => 0xC000_0001, // STATUS_UNSUCCESSFUL
    }
}

/// The Win32 error of an NTSTATUS recorded by `status_of`.
pub fn error_of(status: u64) -> u32 {
    match status {
        STATUS_BUFFER_OVERFLOW => ERROR_MORE_DATA,
        STATUS_PIPE_BROKEN => ERROR_BROKEN_PIPE,
        STATUS_CANCELLED => ERROR_OPERATION_ABORTED,
        STATUS_PIPE_DISCONNECTED => ERROR_PIPE_NOT_CONNECTED,
        STATUS_PIPE_LISTENING => ERROR_PIPE_LISTENING,
        STATUS_PIPE_CLOSING => ERROR_NO_DATA,
        _ => ERROR_GEN_FAILURE,
    }
}

/// Runs `op` for an overlapped request: pending now, completed from a
/// helper thread. `op` returns (status, bytes).
fn start(p: Arc<PipeObj>, ov: u64, op: impl FnOnce(&PipeObj, u64) -> (u64, u32) + Send + 'static) -> i32 {
    let epoch = p.cancel.load(Ordering::SeqCst);
    // SAFETY: the caller's OVERLAPPED, which must stay valid until the
    // operation completes (the Windows contract).
    let event: u64 = unsafe {
        teb::put(ov, 0, STATUS_PENDING);
        teb::put(ov, 8, 0u64);
        teb::get(ov, 24)
    };
    if event & !1 != 0 {
        kernel32::reset_event(event & !1);
    }
    p.pending.fetch_add(1, Ordering::SeqCst);
    let worker = p.clone();
    let spawned = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let (status, bytes) = op(&worker, epoch);
        // SAFETY: as above.
        unsafe {
            teb::put(ov, 8, bytes as u64);
            teb::put(ov, 0, status);
        }
        worker.pending.fetch_sub(1, Ordering::SeqCst);
        if event & !1 != 0 {
            kernel32::set_event(event & !1);
        }
        if event & 1 == 0 {
            if let Some((port, key)) = *worker.port.lock().unwrap() {
                crate::threadpool::post(port, bytes, key, ov, if status == 0 { 0 } else { error_of(status) });
            }
        }
    });
    if spawned.is_err() {
        p.pending.fetch_sub(1, Ordering::SeqCst);
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    fail(ERROR_IO_PENDING, 0)
}

/// Synchronous results: TRUE, or FALSE with the error; ERROR_MORE_DATA
/// still reports the bytes read.
fn finish(r: Result<(usize, bool), u32>, done: *mut u32) -> i32 {
    let (n, err) = match r {
        Ok((n, false)) => (n, None),
        Ok((n, true)) => (n, Some(ERROR_MORE_DATA)),
        Err(e) => (0, Some(e)),
    };
    if !done.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *done = n as u32 };
    }
    match err {
        None => 1,
        Some(e) => fail(e, 0),
    }
}

pub fn read_file(p: Arc<PipeObj>, buf: *mut u8, n: u32, done: *mut u32, ov: u64) -> i32 {
    if p.overlapped && ov != 0 {
        let (b, len) = (buf as u64, n as usize);
        return start(p, ov, move |p, epoch| {
            // SAFETY: the caller's buffer, valid until completion.
            let out = unsafe { core::slice::from_raw_parts_mut(b as *mut u8, len) };
            match p.read(out, epoch) {
                Ok((k, false)) => (0, k as u32),
                Ok((k, true)) => (STATUS_BUFFER_OVERFLOW, k as u32),
                Err(e) => (status_of(e), 0),
            }
        });
    }
    // SAFETY: the caller's buffer of `n` bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, n as usize) };
    let r = p.read(out, p.cancel.load(Ordering::SeqCst));
    record_sync(ov, &r.as_ref().map(|(k, more)| (*k, *more)).map_err(|e| *e));
    finish(r, done)
}

pub fn write_file(p: Arc<PipeObj>, buf: *const u8, n: u32, done: *mut u32, ov: u64) -> i32 {
    if p.overlapped && ov != 0 {
        let (b, len) = (buf as u64, n as usize);
        return start(p, ov, move |p, epoch| {
            // SAFETY: the caller's buffer, valid until completion.
            let data = unsafe { core::slice::from_raw_parts(b as *const u8, len) };
            match p.write(data, epoch) {
                Ok(k) => (0, k as u32),
                Err(e) => (status_of(e), 0),
            }
        });
    }
    // SAFETY: the caller's buffer of `n` bytes.
    let data = unsafe { core::slice::from_raw_parts(buf, n as usize) };
    let r = p.write(data, p.cancel.load(Ordering::SeqCst)).map(|k| (k, false));
    record_sync(ov, &r.clone());
    finish(r, done)
}

/// A synchronous operation given an OVERLAPPED records its result there.
fn record_sync(ov: u64, r: &Result<(usize, bool), u32>) {
    if ov == 0 {
        return;
    }
    let (status, n) = match r {
        Ok((n, false)) => (0, *n),
        Ok((n, true)) => (STATUS_BUFFER_OVERFLOW, *n),
        Err(e) => (status_of(*e), 0),
    };
    // SAFETY: the caller's OVERLAPPED.
    unsafe {
        teb::put(ov, 0, status);
        teb::put(ov, 8, n as u64);
    }
}

/// CancelIo / CancelIoEx: whether anything was pending.
pub fn cancel(p: &PipeObj) -> bool {
    p.cancel.fetch_add(1, Ordering::SeqCst);
    p.pending.load(Ordering::SeqCst) > 0
}

pub fn associate(p: &PipeObj, port: u64, key: u64) {
    *p.port.lock().unwrap() = Some((port, key));
}

/// The client end of `\\.\pipe\name` (CreateFile).
pub fn open_client(name: &str, flags: u32) -> Result<u64, u32> {
    let c = match os_sys::channel_connect(&service(name)) {
        Ok(c) => c,
        Err(Errno::NotFound) | Err(Errno::NameNotFound) => return Err(ERROR_FILE_NOT_FOUND),
        Err(Errno::Busy) | Err(Errno::WouldBlock) => return Err(ERROR_PIPE_BUSY),
        Err(e) => return Err(errno_to_win32(e)),
    };
    Ok(objects::insert(Object::Pipe(Arc::new(PipeObj {
        server: false,
        message_type: false,
        read_message: AtomicBool::new(false),
        nowait: AtomicBool::new(false),
        overlapped: flags & FILE_FLAG_OVERLAPPED != 0,
        listener: None,
        chan: Mutex::new(Some(Arc::new(Fd(c)))),
        rest: Mutex::new(Vec::new()),
        port: Mutex::new(None),
        cancel: AtomicU64::new(0),
        pending: AtomicU32::new(0),
        disconnected: AtomicBool::new(false),
    }))))
}

fn pipe(h: u64) -> Result<Arc<PipeObj>, u32> {
    match objects::get(h) {
        Some(Object::Pipe(p)) => Ok(p),
        Some(_) => Err(ERROR_INVALID_FUNCTION),
        None => Err(ERROR_INVALID_HANDLE),
    }
}

/// The client end's pipe type is fixed by the server here; messages and
/// bytes differ only in how writes split, which the client cannot see.
#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateNamedPipeW(
    name: *const u16,
    open_mode: u32,
    pipe_mode: u32,
    max_instances: u32,
    _out_size: u32,
    _in_size: u32,
    _timeout: u32,
    attributes: u64,
) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) {
        return fail(e, INVALID_HANDLE_VALUE);
    }
    // SAFETY: a NUL-terminated name.
    let Some(win) = (unsafe { crate::path::wide_to_string(name) }) else { return fail(ERROR_INVALID_NAME, INVALID_HANDLE_VALUE) };
    let Some(n) = pipe_name(&win).filter(|n| !n.is_empty() && !n.contains('\\')) else { return fail(ERROR_INVALID_NAME, INVALID_HANDLE_VALUE) };
    if open_mode & (PIPE_ACCESS_INBOUND | PIPE_ACCESS_OUTBOUND) == 0 || max_instances == 0 || max_instances > 255
        || pipe_mode & !(PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS) != 0
        || (pipe_mode & PIPE_READMODE_MESSAGE != 0 && pipe_mode & PIPE_TYPE_MESSAGE == 0)
    {
        return fail(ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE);
    }
    let svc = service(n);
    let listener = {
        let mut all = LISTENERS.lock().unwrap();
        match all.get(&svc).and_then(|(w, max)| Some((w.upgrade()?, *max))) {
            // The instance limit is checked first, as on Windows.
            Some((l, max)) if Arc::strong_count(&l) >= max as usize => return fail(ERROR_PIPE_BUSY, INVALID_HANDLE_VALUE),
            Some(_) if open_mode & FILE_FLAG_FIRST_PIPE_INSTANCE != 0 => return fail(ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE),
            Some((l, _)) => l,
            None => match os_sys::channel_listen(&svc) {
                Ok(fd) => {
                    let l = Arc::new(Fd(fd));
                    all.insert(svc, (Arc::downgrade(&l), max_instances));
                    l
                }
                // Another process serves this name.
                Err(Errno::AlreadyExists) => return fail(ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE),
                Err(e) => return fail(errno_to_win32(e), INVALID_HANDLE_VALUE),
            },
        }
    };
    objects::insert(Object::Pipe(Arc::new(PipeObj {
        server: true,
        message_type: pipe_mode & PIPE_TYPE_MESSAGE != 0,
        read_message: AtomicBool::new(pipe_mode & PIPE_READMODE_MESSAGE != 0),
        nowait: AtomicBool::new(pipe_mode & PIPE_NOWAIT != 0),
        overlapped: open_mode & FILE_FLAG_OVERLAPPED != 0,
        listener: Some(listener),
        chan: Mutex::new(None),
        rest: Mutex::new(Vec::new()),
        port: Mutex::new(None),
        cancel: AtomicU64::new(0),
        pending: AtomicU32::new(0),
        disconnected: AtomicBool::new(false),
    })))
}

/// A client that connected before this call makes it fail with
/// ERROR_PIPE_CONNECTED, as on Windows (the pipe is then connected).
extern "win64" fn ConnectNamedPipe(h: u64, ov: u64) -> i32 {
    let p = match pipe(h) {
        Ok(p) if p.server => p,
        Ok(_) => return fail(ERROR_INVALID_FUNCTION, 0),
        Err(e) => return fail(e, 0),
    };
    let epoch = p.cancel.load(Ordering::SeqCst);
    match p.connect(epoch, false) {
        Ok(()) => return fail(ERROR_PIPE_CONNECTED, 0),
        Err(ERROR_PIPE_LISTENING) => {}
        Err(e) => return fail(e, 0),
    }
    if p.nowait.load(Ordering::SeqCst) {
        return fail(ERROR_PIPE_LISTENING, 0);
    }
    if p.overlapped && ov != 0 {
        return start(p, ov, |p, epoch| match p.connect(epoch, true) {
            Ok(()) | Err(ERROR_PIPE_CONNECTED) => (0, 0),
            Err(e) => (status_of(e), 0),
        });
    }
    match p.connect(epoch, true) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn DisconnectNamedPipe(h: u64) -> i32 {
    let p = match pipe(h) {
        Ok(p) if p.server => p,
        Ok(_) => return fail(ERROR_INVALID_FUNCTION, 0),
        Err(e) => return fail(e, 0),
    };
    p.cancel.fetch_add(1, Ordering::SeqCst);
    *p.chan.lock().unwrap() = None;
    p.rest.lock().unwrap().clear();
    p.disconnected.store(true, Ordering::SeqCst);
    1
}

/// PIPE_READMODE_MESSAGE needs a message-type pipe; PIPE_NOWAIT makes
/// reads and connects return at once. Collection settings are for remote
/// pipes and are refused.
extern "win64" fn SetNamedPipeHandleState(h: u64, mode: *const u32, max_collection: *const u32, timeout: *const u32) -> i32 {
    let p = match pipe(h) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if !max_collection.is_null() || !timeout.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !mode.is_null() {
        // SAFETY: the caller's mode.
        let m = unsafe { *mode };
        if m & !(PIPE_READMODE_MESSAGE | PIPE_NOWAIT) != 0 || (m & PIPE_READMODE_MESSAGE != 0 && !p.message_type && p.server) {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        p.read_message.store(m & PIPE_READMODE_MESSAGE != 0, Ordering::SeqCst);
        p.nowait.store(m & PIPE_NOWAIT != 0, Ordering::SeqCst);
    }
    1
}

/// The server's process: this process for a server instance, the peer of
/// the connection for a client.
extern "win64" fn GetNamedPipeServerProcessId(h: u64, pid: *mut u32) -> i32 {
    let p = match pipe(h) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if pid.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let id = if p.server {
        kernel32::pid()
    } else {
        match p.chan().and_then(|c| os_sys::channel_peer(c.0).map_err(errno_to_win32)) {
            Ok((id, _)) => id,
            Err(e) => return fail(e, 0),
        }
    };
    // SAFETY: the caller's out pointer.
    unsafe { *pid = id as u32 };
    1
}

/// Writes one message and reads the reply, in message read mode.
#[allow(clippy::too_many_arguments)]
extern "win64" fn TransactNamedPipe(h: u64, input: *const u8, in_len: u32, output: *mut u8, out_len: u32, read: *mut u32, ov: u64) -> i32 {
    let p = match pipe(h) {
        Ok(p) => p,
        Err(e) => return fail(e, 0),
    };
    if !p.read_message.load(Ordering::SeqCst) {
        return fail(ERROR_BAD_PIPE, 0);
    }
    let (i, il, o, ol) = (input as u64, in_len as usize, output as u64, out_len as usize);
    let op = move |p: &PipeObj, epoch: u64| -> Result<(usize, bool), u32> {
        // SAFETY: the caller's buffers.
        let (data, out) = unsafe { (core::slice::from_raw_parts(i as *const u8, il), core::slice::from_raw_parts_mut(o as *mut u8, ol)) };
        p.write(data, epoch)?;
        p.read(out, epoch)
    };
    if p.overlapped && ov != 0 {
        return start(p, ov, move |p, epoch| match op(p, epoch) {
            Ok((k, false)) => (0, k as u32),
            Ok((k, true)) => (STATUS_BUFFER_OVERFLOW, k as u32),
            Err(e) => (status_of(e), 0),
        });
    }
    let r = op(&p, p.cancel.load(Ordering::SeqCst));
    finish(r, read)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CreateNamedPipeW,
        ConnectNamedPipe,
        DisconnectNamedPipe,
        SetNamedPipeHandleState,
        GetNamedPipeServerProcessId,
        TransactNamedPipe,
    ]
}
