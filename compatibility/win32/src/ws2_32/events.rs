//! WSAEventSelect and the WSA event functions.
//!
//! WSA events are kernel32 manual-reset events. A socket selected for
//! network events is made non-blocking and watched by one thread, which
//! waits on the selected sockets with the kernel's handle wait, records
//! each event that happened (FD_READ, FD_WRITE, FD_ACCEPT, FD_CONNECT,
//! FD_CLOSE) and signals the socket's event. As on Windows, a recorded
//! event is not recorded again until the call that consumes it
//! re-enables it: a receive for FD_READ, a send for FD_WRITE, accept for
//! FD_ACCEPT. WSAEnumNetworkEvents reports and clears the record.
use super::*;
use core::sync::atomic::AtomicBool;

const FD_READ: i32 = 0x01;
const FD_WRITE: i32 = 0x02;
const FD_OOB: i32 = 0x04;
const FD_ACCEPT: i32 = 0x08;
const FD_CONNECT: i32 = 0x10;
const FD_CLOSE: i32 = 0x20;
const FD_ALL: i32 = 0x3ff;

const WSA_WAIT_FAILED: u32 = u32::MAX;
const WSA_INVALID_HANDLE: u32 = 6;
const WSA_INVALID_PARAMETER: u32 = 87;
const WSA_MAXIMUM_WAIT_EVENTS: u32 = 64;

#[derive(Clone, Copy)]
struct Selection {
    event: u64,
    mask: i32,
    /// Events that may be recorded (enabled and not yet consumed).
    armed: i32,
    /// Events recorded since the last WSAEnumNetworkEvents.
    pending: i32,
    /// iErrorCode per FD_*_BIT.
    errors: [i32; 10],
    /// FD_CLOSE was recorded: Windows reports it once.
    closed: bool,
}

static SELECTIONS: Mutex<BTreeMap<u64, Selection>> = Mutex::new(BTreeMap::new());
/// Sockets in the listening state: readable means a connection to accept.
static LISTENING: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static WATCHER: AtomicBool = AtomicBool::new(false);

pub(super) fn listening(s: u64) {
    let mut l = LISTENING.lock().unwrap();
    if !l.contains(&s) {
        l.push(s);
    }
}

pub(super) fn forget(s: u64) {
    SELECTIONS.lock().unwrap().remove(&s);
    LISTENING.lock().unwrap().retain(|&x| x != s);
}

/// The re-enabling functions: the consumed event may be recorded again.
/// A receive also re-enables FD_CLOSE until it has been reported, so a
/// close after data is not lost.
pub(super) fn rearm(s: u64, bits: i32) {
    if let Some(sel) = SELECTIONS.lock().unwrap().get_mut(&s) {
        let mut bits = bits;
        if bits & FD_READ != 0 && !sel.closed {
            bits |= FD_CLOSE;
        }
        sel.armed |= bits & sel.mask;
    }
}

fn record(s: u64, bits: i32, error: Option<(usize, i32)>) {
    let event = {
        let mut sels = SELECTIONS.lock().unwrap();
        let Some(sel) = sels.get_mut(&s) else { return };
        let bits = bits & sel.armed;
        if bits == 0 {
            return;
        }
        sel.armed &= !bits;
        sel.pending |= bits;
        sel.closed |= bits & FD_CLOSE != 0;
        if let Some((bit, code)) = error {
            sel.errors[bit] = code;
        }
        sel.event
    };
    crate::kernel32::SetEvent(event);
}

/// What a readable socket means: a connection to accept, data, or the
/// peer's close (a stream that reads end of file or an error). The
/// readable events stay off afterwards until a call re-enables them, so
/// a socket left at end of file does not wake the watcher again and again.
fn classify_readable(s: u64, fd: u64, stream: bool) {
    let readable = FD_READ | FD_OOB | FD_ACCEPT | FD_CLOSE;
    if LISTENING.lock().unwrap().contains(&s) {
        record(s, FD_ACCEPT, None);
    } else {
        let mut b = [0u8; 1];
        match os_sys::sock_recv(fd, &mut b, net::MSG_PEEK) {
            Ok((0, _)) if stream => record(s, FD_CLOSE, None),
            Ok(_) => record(s, FD_READ, None),
            Err(Errno::WouldBlock) => return,
            Err(e) => record(s, FD_CLOSE, Some((5, wsa(e) as i32))),
        }
    }
    disarm(s, readable);
}

fn disarm(s: u64, bits: i32) {
    if let Some(sel) = SELECTIONS.lock().unwrap().get_mut(&s) {
        sel.armed &= !bits;
    }
}

/// The watcher: waits on every selected socket for the events still
/// armed, a short while at a time so new selections join the wait.
extern "win64" fn watch(_param: u64) -> u32 {
    loop {
        let sels: Vec<(u64, i32)> = SELECTIONS.lock().unwrap().iter().map(|(&s, sel)| (s, sel.armed)).collect();
        let mut items = Vec::new();
        for (s, armed) in sels {
            let Ok(st) = socket_state(s) else { continue };
            let mut ev = 0;
            if armed & (FD_READ | FD_OOB | FD_ACCEPT | FD_CLOSE) != 0 {
                ev |= osyx_abi::ipc::WAIT_READABLE;
            }
            if armed & (FD_WRITE | FD_CONNECT) != 0 {
                ev |= osyx_abi::ipc::WAIT_WRITABLE;
            }
            if ev != 0 {
                items.push((s, st.stream, os_sys::ipc::WaitItem { handle: st.fd, events: ev, revents: 0 }));
            }
        }
        if items.is_empty() {
            os_sys::sleep(20_000_000);
            continue;
        }
        let mut raw: Vec<os_sys::ipc::WaitItem> = items.iter().map(|i| i.2).collect();
        if os_sys::wait_handles(&mut raw, 20_000_000).is_err() {
            continue;
        }
        for ((s, stream, item), r) in items.iter().zip(raw.iter()) {
            if r.revents & osyx_abi::ipc::WAIT_READABLE != 0 {
                classify_readable(*s, item.handle, *stream);
            }
            if r.revents & osyx_abi::ipc::WAIT_WRITABLE != 0 {
                record(*s, FD_CONNECT | FD_WRITE, None);
                disarm(*s, FD_CONNECT | FD_WRITE);
            }
        }
    }
}

fn start_watcher() -> Result<(), u32> {
    if WATCHER.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let stack = crate::kernel32::stack_reserve(0, false);
    match crate::kernel32::spawn_thread(watch as *const () as u64, 0, stack, false) {
        Ok(_) => Ok(()),
        Err(_) => {
            WATCHER.store(false, Ordering::SeqCst);
            Err(WSAENOBUFS)
        }
    }
}

extern "win64" fn WSAEventSelect(s: u64, event: u64, mask: i32) -> i32 {
    let r = (|| {
        let st = socket_state(s)?;
        if mask == 0 {
            // Cancels the association; the socket stays non-blocking.
            SELECTIONS.lock().unwrap().remove(&s);
            return Ok(());
        }
        if event == 0 || mask & !FD_ALL != 0 {
            return Err(WSAEINVAL);
        }
        if !st.nonblocking {
            os_sys::sock_ctl(st.fd, net::ctl::NONBLOCK, 1).map_err(wsa)?;
            if let Some(k) = SOCKETS.lock().unwrap().get_mut(&s) {
                k.nonblocking = true;
            }
        }
        start_watcher()?;
        // A new selection keeps what was recorded, as Windows does.
        let mut sels = SELECTIONS.lock().unwrap();
        let old = sels.get(&s).copied();
        sels.insert(s, Selection {
            event,
            mask,
            armed: if old.is_some_and(|o| o.closed) { mask & !FD_CLOSE } else { mask },
            pending: old.map_or(0, |o| o.pending & mask),
            errors: old.map_or([0; 10], |o| o.errors),
            closed: old.is_some_and(|o| o.closed),
        });
        drop(sels);
        if old.is_some_and(|o| o.pending & mask != 0) {
            crate::kernel32::SetEvent(event);
        }
        Ok(())
    })();
    result(r)
}

/// WSANETWORKEVENTS: lNetworkEvents and iErrorCode[10].
extern "win64" fn WSAEnumNetworkEvents(s: u64, event: u64, out: *mut i32) -> i32 {
    let r = (|| {
        socket_state(s)?;
        if out.is_null() {
            return Err(WSAEFAULT);
        }
        let taken = SELECTIONS.lock().unwrap().get_mut(&s).map(|sel| {
            let t = (sel.pending, sel.errors);
            sel.pending = 0;
            sel.errors = [0; 10];
            t
        });
        let (pending, errors) = taken.unwrap_or((0, [0; 10]));
        if event != 0 {
            crate::kernel32::ResetEvent(event);
        }
        // SAFETY: the caller's WSANETWORKEVENTS (44 bytes).
        unsafe {
            *out = pending;
            for (i, e) in errors.iter().enumerate() {
                *out.add(1 + i) = *e;
            }
        }
        Ok(())
    })();
    result(r)
}

extern "win64" fn WSACreateEvent() -> u64 {
    match crate::kernel32::create_event(true, false, None) {
        0 => err(WSA_NOT_ENOUGH_MEMORY, 0),
        h => h,
    }
}

const WSA_NOT_ENOUGH_MEMORY: u32 = 8;

fn bool_result(ok: i32) -> i32 {
    if ok != 0 { 1 } else { err(WSA_INVALID_HANDLE, 0) }
}

extern "win64" fn WSACloseEvent(event: u64) -> i32 {
    bool_result(crate::kernel32::CloseHandle(event))
}

extern "win64" fn WSASetEvent(event: u64) -> i32 {
    bool_result(crate::kernel32::SetEvent(event))
}

extern "win64" fn WSAResetEvent(event: u64) -> i32 {
    bool_result(crate::kernel32::ResetEvent(event))
}

/// WaitForMultipleObjectsEx with Winsock's limits and error codes:
/// WSA_WAIT_EVENT_0 + i, WSA_WAIT_IO_COMPLETION, WSA_WAIT_TIMEOUT.
extern "win64" fn WSAWaitForMultipleEvents(n: u32, events: *const u64, all: i32, ms: u32, alertable: i32) -> u32 {
    if n == 0 || n > WSA_MAXIMUM_WAIT_EVENTS || events.is_null() {
        return err(WSA_INVALID_PARAMETER, WSA_WAIT_FAILED);
    }
    crate::kernel32::WaitForMultipleObjectsEx(n, events, all, ms, alertable)
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![WSAEventSelect, WSAEnumNetworkEvents, WSACreateEvent, WSACloseEvent, WSASetEvent, WSAResetEvent, WSAWaitForMultipleEvents]
}
