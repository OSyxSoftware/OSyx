//! Handle inheritance across CreateProcess.
//!
//! A child inherits the parent's inheritable handles at the same handle
//! values (programs pass those values on the command line). The parent
//! offers them on a one-shot kernel channel whose name it puts in the
//! child's environment (`OSYX_WINRUN_INHERIT`); the child's winrun takes
//! them before it opens any handle of its own.
//!
//! DuplicateHandle between processes uses each winrun's handle service
//! (below), with the same rules.
//!
//! What a handle can carry to another process: sections backed by
//! page-file memory (shared memory objects, which the kernel passes on a
//! channel), events (their state moves to shared memory, see objects.rs)
//! and connected pipes (relayed, see pipes.rs). Other objects live inside
//! one winrun (mutexes, semaphores, files opened through the VFS...), so
//! CreateProcess refuses to inherit them with ERROR_NOT_SUPPORTED rather
//! than start a child that would find nothing behind a handle it was
//! promised.
use crate::error::*;
use crate::objects::{self, Object, SectionObj};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use osyx_abi::ipc::MAX_MESSAGE_HANDLES;

pub const ENV: &str = "OSYX_WINRUN_INHERIT";

const KIND_SECTION: u32 = 1;
/// A connected pipe: `size` is the relaying process's pid and relay id.
const KIND_PIPE: u32 = 2;
/// An event: its shared state page; flags say whether it resets manually.
const KIND_EVENT: u32 = 3;
const EVENT_MANUAL: u32 = 1;
const SECTION_WRITABLE: u32 = 1;
const SECTION_EXECUTABLE: u32 = 2;
const SECTION_FILE: u32 = 4;
/// One record: handle value, kind, flags, size.
const RECORD: usize = 24;
/// How long the parent waits for the child to collect its handles.
const OFFER_NS: u64 = 30_000_000_000;

static OFFERS: AtomicU32 = AtomicU32::new(0);

/// A handle to pass: its value, the kernel object (if the kind has one),
/// and its record.
struct Item {
    handle: u64,
    kernel: Option<u64>,
    kind: u32,
    flags: u32,
    size: u64,
}

fn item(h: u64) -> Result<Item, u32> {
    match objects::get(h) {
        Some(Object::Section(s)) if s.shm != 0 && (s.file.is_none() || s.file_mapping) => Ok(Item {
            handle: h,
            kernel: Some(s.shm),
            kind: KIND_SECTION,
            flags: if s.writable { SECTION_WRITABLE } else { 0 } | if s.executable { SECTION_EXECUTABLE } else { 0 }
                | if s.file_mapping { SECTION_FILE } else { 0 },
            size: s.size,
        }),
        Some(Object::Event(e)) => Ok(Item {
            handle: h,
            kernel: Some(e.share()?),
            kind: KIND_EVENT,
            flags: if e.manual { EVENT_MANUAL } else { 0 },
            size: 0,
        }),
        Some(Object::Pipe(_)) => {
            let (id, flags) = crate::pipes::relay_out(h)?;
            Ok(Item { handle: h, kernel: None, kind: KIND_PIPE, flags, size: (crate::kernel32::pid() << 32) | id as u64 })
        }
        Some(o) => {
            if crate::VERBOSE.load(Ordering::Relaxed) {
                os_std::eprintln!("winrun: cannot inherit handle {h:#x} ({}) into another process", objects::kind_name(&o));
            }
            Err(ERROR_NOT_SUPPORTED)
        }
        None => Err(ERROR_INVALID_HANDLE),
    }
}

/// Offers `handles` to the next child. Returns the environment entry the
/// child needs (None when there is nothing to inherit); a background
/// thread hands them over once and then closes the offer.
pub fn offer(handles: &[u64]) -> Result<Option<String>, u32> {
    let items = handles.iter().map(|&h| item(h)).collect::<Result<Vec<_>, _>>()?;
    if items.is_empty() {
        return Ok(None);
    }
    let name = alloc::format!("user.{}.winrun.inherit.{}.{}", os_std::os::getuid().0, crate::kernel32::pid(), OFFERS.fetch_add(1, Ordering::Relaxed));
    let listener = os_sys::channel_listen(&name).map_err(errno_to_win32)?;
    // The records go first, then the kernel objects four per message.
    let mut records = Vec::with_capacity(items.len() * RECORD);
    for i in &items {
        records.extend_from_slice(&i.handle.to_le_bytes());
        records.extend_from_slice(&i.kind.to_le_bytes());
        records.extend_from_slice(&i.flags.to_le_bytes());
        records.extend_from_slice(&i.size.to_le_bytes());
    }
    let kernel: Vec<u64> = items.iter().filter_map(|i| i.kernel).collect();
    // The sections stay alive (the parent's handles) while this thread
    // serves; cloning the objects keeps them alive even if those close.
    let keep: Vec<Object> = items.iter().filter_map(|i| objects::get(i.handle)).collect();
    let spawned = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let _keep = keep;
        let mut items = [os_sys::ipc::WaitItem { handle: listener, events: osyx_abi::ipc::WAIT_READABLE, revents: 0 }];
        if matches!(os_sys::wait_handles(&mut items, OFFER_NS), Ok(n) if n > 0) {
            if let Ok(c) = os_sys::channel_accept(listener) {
                let _ = os_sys::channel_send(c, &records, &[]);
                for chunk in kernel.chunks(MAX_MESSAGE_HANDLES) {
                    let _ = os_sys::channel_send(c, &[], chunk);
                }
                // Wait for the child to hang up: it has taken everything.
                let mut done = [os_sys::ipc::WaitItem { handle: c, events: osyx_abi::ipc::WAIT_HANGUP, revents: 0 }];
                let _ = os_sys::wait_handles(&mut done, OFFER_NS);
                let _ = os_sys::close(c);
            }
        }
        let _ = os_sys::close(listener);
    });
    if spawned.is_err() {
        return Err(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(Some(alloc::format!("{ENV}={name}")))
}

/// Takes the handles the parent offered (named by `name`), at their values.
/// Runs before winrun opens any handle of its own.
pub fn receive(name: &str) -> Result<usize, String> {
    let c = os_sys::channel_connect(name).map_err(|e| alloc::format!("connect {name}: {e:?}"))?;
    let recv = |buf: &mut [u8], handles: &mut [u64]| -> Result<(usize, usize), String> {
        loop {
            match os_sys::channel_recv(c, buf, handles) {
                Err(osyx_abi::Errno::WouldBlock) => {
                    let mut w = [os_sys::ipc::WaitItem { handle: c, events: osyx_abi::ipc::WAIT_READABLE | osyx_abi::ipc::WAIT_HANGUP, revents: 0 }];
                    os_sys::wait_handles(&mut w, OFFER_NS).map_err(|e| alloc::format!("wait: {e:?}"))?;
                }
                r => return r.map_err(|e| alloc::format!("receive: {e:?}")),
            }
        }
    };
    let mut buf = alloc::vec![0u8; osyx_abi::ipc::MAX_MESSAGE];
    let (n, _) = recv(&mut buf, &mut [])?;
    let records: Vec<&[u8]> = buf[..n].chunks(RECORD).collect();
    let wanted = records.iter().filter(|r| has_kernel(r)).count();
    let mut kernel = Vec::with_capacity(wanted);
    while kernel.len() < wanted {
        let mut hs = [0u64; MAX_MESSAGE_HANDLES];
        let (_, k) = recv(&mut [], &mut hs)?;
        if k == 0 {
            return Err(String::from("the parent sent fewer objects than records"));
        }
        kernel.extend_from_slice(&hs[..k]);
    }
    let _ = os_sys::close(c);
    let mut kernel = kernel.into_iter();
    for r in &records {
        let k = if has_kernel(r) { kernel.next() } else { None };
        let handle = u64::from_le_bytes(r[0..8].try_into().unwrap());
        let Some(o) = object(r, k) else { return Err(alloc::format!("unknown inherited object for {handle:#x}")) };
        if !objects::insert_at(handle, o) {
            return Err(alloc::format!("handle {handle:#x} is already taken"));
        }
        // Still inheritable in the child, as on Windows.
        objects::set_inheritable(handle, true);
    }
    Ok(records.len())
}

// ---- DuplicateHandle between processes ------------------------------------------------------
//
// Every winrun serves its handle table on `winrun.handles.<pid>`: PUT
// inserts an object another process sends (DuplicateHandle into this
// process) and answers with its new handle; TAKE hands out the object
// behind one of this process's handles (DuplicateHandle out of it), closing
// it with DUPLICATE_CLOSE_SOURCE. The same objects can travel as for
// inheritance. Any process that holds a handle to this process may ask:
// OSyx processes of one user can already open each other.

const PUT: u32 = 1;
const TAKE: u32 = 2;

fn service_name(pid: u64) -> String {
    alloc::format!("user.{}.winrun.handles.{pid}", os_std::os::getuid().0)
}

fn record(i: &Item) -> [u8; RECORD] {
    let mut r = [0u8; RECORD];
    r[0..8].copy_from_slice(&i.handle.to_le_bytes());
    r[8..12].copy_from_slice(&i.kind.to_le_bytes());
    r[12..16].copy_from_slice(&i.flags.to_le_bytes());
    r[16..24].copy_from_slice(&i.size.to_le_bytes());
    r
}

/// Whether a record's object comes with a kernel object.
fn has_kernel(r: &[u8]) -> bool {
    matches!(u32::from_le_bytes(r[8..12].try_into().unwrap()), KIND_SECTION | KIND_EVENT)
}

/// The object a record and its kernel object describe.
fn object(r: &[u8], kernel: Option<u64>) -> Option<Object> {
    let half = |at: usize| u32::from_le_bytes(r[at..at + 4].try_into().unwrap());
    let size = u64::from_le_bytes(r[16..24].try_into().unwrap());
    match half(8) {
        KIND_PIPE => crate::pipes::relay_in(size >> 32, size as u32, half(12)).ok(),
        KIND_EVENT => crate::objects::EventObj::from_shared(kernel?, half(12) & EVENT_MANUAL != 0).map(Object::Event),
        KIND_SECTION => Some(Object::Section(Arc::new(SectionObj {
            shm: kernel?,
            file_mapping: half(12) & SECTION_FILE != 0,
            size,
            writable: half(12) & SECTION_WRITABLE != 0,
            executable: half(12) & SECTION_EXECUTABLE != 0,
            file: None,
            name: None,
        }))),
        _ => None,
    }
}

fn recv_wait(c: u64, buf: &mut [u8], handles: &mut [u64]) -> Result<(usize, usize), u32> {
    loop {
        match os_sys::channel_recv(c, buf, handles) {
            Err(osyx_abi::Errno::WouldBlock) => {
                let mut w = [os_sys::ipc::WaitItem { handle: c, events: osyx_abi::ipc::WAIT_READABLE | osyx_abi::ipc::WAIT_HANGUP, revents: 0 }];
                os_sys::wait_handles(&mut w, OFFER_NS).map_err(errno_to_win32)?;
            }
            r => return r.map_err(errno_to_win32),
        }
    }
}

/// Answers one request on connection `c`.
fn serve(c: u64) {
    let mut buf = [0u8; 4 + RECORD + 4];
    let mut hs = [0u64; 1];
    let Ok((n, k)) = recv_wait(c, &mut buf, &mut hs) else { return };
    let op = if n >= 4 { u32::from_le_bytes(buf[0..4].try_into().unwrap()) } else { 0 };
    match op {
        PUT if n >= 4 + RECORD && k == has_kernel(&buf[4..4 + RECORD]) as usize => {
            let h = match object(&buf[4..4 + RECORD], (k == 1).then_some(hs[0])) {
                Some(o) => objects::insert(o),
                None => {
                    if k == 1 {
                        let _ = os_sys::close(hs[0]);
                    }
                    0
                }
            };
            let _ = os_sys::channel_send(c, &h.to_le_bytes(), &[]);
        }
        TAKE if n >= 4 + 8 + 4 => {
            let h = u64::from_le_bytes(buf[4..12].try_into().unwrap());
            let close = u32::from_le_bytes(buf[12..16].try_into().unwrap()) != 0;
            match item(h) {
                Ok(i) => {
                    let _ = os_sys::channel_send(c, &record(&i), i.kernel.as_slice());
                    if close {
                        objects::close(h);
                    }
                }
                Err(e) => {
                    let _ = os_sys::channel_send(c, &e.to_le_bytes(), &[]);
                }
            }
        }
        _ => {}
    }
}

/// Starts this process's handle service (once, at startup).
pub fn start_service() {
    let Ok(listener) = os_sys::channel_listen(&service_name(crate::kernel32::pid())) else { return };
    let _ = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        if crate::EVENTS.load(core::sync::atomic::Ordering::Relaxed) {
            os_std::eprintln!("winrun: handle service tid={}", os_sys::gettid());
        }
        loop {
        let mut w = [os_sys::ipc::WaitItem { handle: listener, events: osyx_abi::ipc::WAIT_READABLE, revents: 0 }];
        if os_sys::wait_handles(&mut w, u64::MAX).is_err() {
            return;
        }
        if let Ok(c) = os_sys::channel_accept(listener) {
            serve(c);
            let _ = os_sys::close(c);
        }
        }
    });
}

fn connect(pid: u64) -> Result<u64, u32> {
    // A process without a handle service is not a winrun process: its
    // handle table is not Windows handles.
    os_sys::channel_connect(&service_name(pid)).map_err(|_| ERROR_NOT_SUPPORTED)
}

/// DuplicateHandle of `h` from this process into process `pid`.
pub fn put(pid: u64, h: u64) -> Result<u64, u32> {
    let i = item(h)?;
    let c = connect(pid)?;
    let mut msg = Vec::with_capacity(4 + RECORD);
    msg.extend_from_slice(&PUT.to_le_bytes());
    msg.extend_from_slice(&record(&i));
    let r = os_sys::channel_send(c, &msg, i.kernel.as_slice()).map_err(errno_to_win32).and_then(|()| {
        let mut out = [0u8; 8];
        let (n, _) = recv_wait(c, &mut out, &mut [])?;
        let v = u64::from_le_bytes(out);
        if n == 8 && v != 0 { Ok(v) } else { Err(ERROR_NOT_SUPPORTED) }
    });
    let _ = os_sys::close(c);
    r
}

/// DuplicateHandle of handle `h` of process `pid` into this process.
pub fn take(pid: u64, h: u64, close_source: bool) -> Result<u64, u32> {
    let c = connect(pid)?;
    let mut msg = Vec::with_capacity(16);
    msg.extend_from_slice(&TAKE.to_le_bytes());
    msg.extend_from_slice(&h.to_le_bytes());
    msg.extend_from_slice(&(close_source as u32).to_le_bytes());
    let r = os_sys::channel_send(c, &msg, &[]).map_err(errno_to_win32).and_then(|()| {
        let mut buf = [0u8; RECORD];
        let mut hs = [0u64; 1];
        let (n, k) = recv_wait(c, &mut buf, &mut hs)?;
        if n == 4 {
            return Err(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
        }
        match (n == RECORD && k == has_kernel(&buf) as usize).then(|| object(&buf, (k == 1).then_some(hs[0]))).flatten() {
            Some(o) => Ok(objects::insert(o)),
            None => Err(ERROR_NOT_SUPPORTED),
        }
    });
    let _ = os_sys::close(c);
    r
}
