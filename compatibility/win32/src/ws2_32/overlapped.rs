//! Overlapped socket I/O and completion ports.
//!
//! A socket associated with a completion port (CreateIoCompletionPort)
//! gets a packet for each overlapped operation that completes, with the
//! key it was associated with. Sends complete in the call; receives wait
//! for data on a helper thread and complete from there, so WSARecv and
//! WSARecvFrom return WSA_IO_PENDING. Each completion fills the
//! OVERLAPPED (Internal: the NTSTATUS, InternalHigh: the bytes), signals
//! its event, and queues a packet unless the event's low bit is set.
use super::*;
use crate::error::{ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER};
use crate::kernel32::overlapped as ov;

/// Sockets associated with a completion port: (port, key).
static PORTS: Mutex<BTreeMap<u64, (u64, u64)>> = Mutex::new(BTreeMap::new());
/// Operations under way, by OVERLAPPED: their socket, the thread that
/// issued them, and whether they were cancelled.
static PENDING: Mutex<BTreeMap<u64, (u64, u64, bool)>> = Mutex::new(BTreeMap::new());

/// Cancels operations under way on a socket so they end with
/// WSA_OPERATION_ABORTED: the one with OVERLAPPED `o`, or all of them
/// (`o` 0), or with `thread` only those that thread issued (CancelIo).
/// False if there was none.
pub(crate) fn cancel(s: u64, o: u64, thread: Option<u64>) -> bool {
    let mut found = false;
    for (k, (sock, issuer, cancelled)) in PENDING.lock().unwrap().iter_mut() {
        if *sock == s && (o == 0 || *k == o) && thread.is_none_or(|t| t == *issuer) {
            *cancelled = true;
            found = true;
        }
    }
    found
}

const STATUS_PENDING: u64 = 0x103;
const WSA_IO_PENDING: u32 = 997;
const WSA_OPERATION_ABORTED: u32 = 995;

/// Winsock errors and the NTSTATUS an OVERLAPPED carries for them.
const STATUSES: [(u32, u64); 8] = [
    (WSA_OPERATION_ABORTED, 0xC000_0120),  // STATUS_CANCELLED
    (10054, 0xC000_020D),                  // WSAECONNRESET: STATUS_CONNECTION_RESET
    (10053, 0xC000_0241),                  // WSAECONNABORTED: STATUS_CONNECTION_ABORTED
    (10060, 0xC000_00B5),                  // WSAETIMEDOUT: STATUS_IO_TIMEOUT
    (10061, 0xC000_0236),                  // WSAECONNREFUSED: STATUS_CONNECTION_REFUSED
    (10057, 0xC000_0184),                  // WSAENOTCONN: STATUS_INVALID_DEVICE_STATE
    (10040, 0x8000_0005),                  // WSAEMSGSIZE: STATUS_BUFFER_OVERFLOW
    (10051, 0xC000_023C),                  // WSAENETUNREACH: STATUS_NETWORK_UNREACHABLE
];

fn status_of(e: u32) -> u64 {
    STATUSES.iter().find(|(w, _)| *w == e).map_or(0xC000_0001, |(_, s)| *s)
}

/// The Winsock error of an OVERLAPPED's status.
pub(super) fn error_of(status: u64) -> u32 {
    STATUSES.iter().find(|(_, s)| *s == status).map_or(WSAEINVAL, |(w, _)| *w)
}

pub(super) fn port_of(s: u64) -> Option<u64> {
    PORTS.lock().unwrap().get(&s).map(|&(port, _)| port)
}

/// CreateIoCompletionPort for a socket. A socket stays with the first port
/// it was associated with until it is closed: another port is refused
/// (ERROR_INVALID_PARAMETER), as on Windows.
pub(crate) fn associate(s: u64, port: u64, key: u64) -> Result<(), u32> {
    if !SOCKETS.lock().unwrap().contains_key(&s) {
        return Err(ERROR_INVALID_HANDLE);
    }
    let mut ports = PORTS.lock().unwrap();
    match ports.get(&s) {
        Some(&(p, _)) if p != port => Err(ERROR_INVALID_PARAMETER),
        _ => {
            ports.insert(s, (port, key));
            Ok(())
        }
    }
}

pub(super) fn forget(s: u64) {
    PORTS.lock().unwrap().remove(&s);
    SKIP.lock().unwrap().remove(&s);
}

/// Sockets with FILE_SKIP_COMPLETION_PORT_ON_SUCCESS (once set, it stays).
static SKIP: Mutex<alloc::collections::BTreeSet<u64>> = Mutex::new(alloc::collections::BTreeSet::new());

pub(super) fn set_skip(s: u64) {
    SKIP.lock().unwrap().insert(s);
}

/// Completes an operation that finished in the call: as `finish`, without
/// a packet when the socket skips them on success.
pub(super) fn finish_now(s: u64, o: u64, bytes: usize) {
    if SKIP.lock().unwrap().contains(&s) {
        // SAFETY: the caller's OVERLAPPED.
        let event = unsafe {
            ov::set_bytes(o, bytes as u64);
            ov::set_status(o, 0);
            ov::event(o)
        };
        if event & !1 != 0 {
            crate::kernel32::SetEvent(event & !1);
        }
        return;
    }
    finish(s, o, bytes, 0);
}

/// Completes overlapped operation `o` on socket `s` with `bytes`
/// transferred and Winsock error `error` (0: success).
pub(super) fn finish(s: u64, o: u64, bytes: usize, error: u32) {
    let port = PORTS.lock().unwrap().get(&s).copied();
    finish_to(port, o, bytes, error);
}

/// `finish` with the (port, key) the operation was issued under: a socket
/// closed meanwhile still gets the packet for its aborted operation.
fn finish_to(port: Option<(u64, u64)>, o: u64, bytes: usize, error: u32) {
    // Under OSYX_WINRUN_FAILTRACE (or -v): the first completions, to follow
    // a program's socket traffic.
    static TRACED: AtomicU32 = AtomicU32::new(0);
    if traced() && TRACED.fetch_add(1, Ordering::Relaxed) < 400 {
        os_std::eprintln!("winrun: overlapped socket I/O {o:#x} done: {bytes} byte(s), error {error}, port {}", port.is_some());
    }
    let status = if error == 0 { 0 } else { status_of(error) };
    // SAFETY: the caller's OVERLAPPED, which stays valid until it completes.
    let event = unsafe {
        ov::set_bytes(o, bytes as u64);
        ov::set_status(o, status);
        ov::event(o)
    };
    if event & !1 != 0 {
        crate::kernel32::SetEvent(event & !1);
    }
    if event & 1 == 0 {
        if let Some((port, key)) = port {
            crate::threadpool::post(port, bytes as u32, key, o, error);
        }
    }
}

/// Runs `op` for OVERLAPPED `o`, issued on socket `s`, on a helper thread
/// that holds kernel socket `hold` meanwhile: the operation is pending
/// and its result completes `o` (and goes to the port `s` is associated
/// with). `op` is told, between its bounded waits, whether the operation
/// was cancelled or `s` closed. False if no helper thread could start;
/// nothing is then pending.
pub(super) fn run<F>(s: u64, hold: u64, o: *mut u8, op: F) -> bool
where
    F: FnOnce(&dyn Fn() -> bool) -> Result<u32, u32> + Send + 'static,
{
    let o = o as u64;
    // SAFETY: the caller's OVERLAPPED.
    unsafe { ov::set_status(o, STATUS_PENDING) };
    PENDING.lock().unwrap().insert(o, (s, os_sys::gettid(), false));
    let port = PORTS.lock().unwrap().get(&s).copied();
    super::service::hold_fd(hold);
    let started = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let stop = move || PENDING.lock().unwrap().get(&o).is_none_or(|p| p.2) || !SOCKETS.lock().unwrap().contains_key(&s);
        let r = op(&stop);
        PENDING.lock().unwrap().remove(&o);
        if super::service::release_fd(hold) {
            let _ = os_sys::close(hold);
        }
        match r {
            Ok(n) => finish_to(port, o, n as usize, 0),
            Err(e) => finish_to(port, o, 0, e),
        }
    });
    if started.is_err() {
        PENDING.lock().unwrap().remove(&o);
        super::service::release_fd(hold);
        return false;
    }
    true
}

/// Waits up to 200 ms for `events` on kernel socket `fd`: a bounded wait,
/// so a helper notices cancellation and closing between them.
pub(super) fn wait_ready(fd: u64, events: u32) {
    let mut item = [os_sys::ipc::WaitItem { handle: fd, events, revents: 0 }];
    let _ = os_sys::wait_handles(&mut item, 200_000_000);
}

/// An overlapped receive: the WSABUFs are copied now (their memory stays
/// the caller's until completion); the data is received on a helper
/// thread, waiting for it on a non-blocking socket.
pub(super) fn receive(s: u64, bufs: *const u8, count: u32, fl: u64, o: *mut u8, from: *mut u8, fromlen: *mut i32) -> i32 {
    let (fd, stream) = match fd(s) {
        Ok(v) => v,
        Err(e) => return err(e, SOCKET_ERROR),
    };
    let parts = match wsa_bufs(bufs, count) {
        Ok(p) => p,
        Err(e) => return err(e, SOCKET_ERROR),
    };
    let (o, from, fromlen) = (o as u64, from as u64, fromlen as u64);
    // SAFETY: the caller's OVERLAPPED.
    unsafe { ov::set_status(o, STATUS_PENDING) };
    PENDING.lock().unwrap().insert(o, (s, os_sys::gettid(), false));
    let port = PORTS.lock().unwrap().get(&s).copied();
    // The helper holds the descriptor: closesocket meanwhile does not close
    // it (or let the number be reused) until the helper is done.
    super::service::hold_fd(fd);
    let started = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let total: usize = parts.iter().map(|p| p.0 as usize).sum();
        let mut data = alloc::vec![0u8; total];
        let (bytes, error) = loop {
            let cancelled = PENDING.lock().unwrap().get(&o).is_none_or(|p| p.2);
            if cancelled || !SOCKETS.lock().unwrap().contains_key(&s) {
                break (0, WSA_OPERATION_ABORTED);
            }
            // Each attempt is non-blocking, so cancellation and closing are
            // seen between the bounded waits below.
            match os_sys::sock_recv(fd, &mut data, fl | net::MSG_DONTWAIT) {
                Err(Errno::WouldBlock) => {
                    let mut item = [os_sys::ipc::WaitItem { handle: fd, events: osyx_abi::ipc::WAIT_READABLE, revents: 0 }];
                    // A bounded wait, so a socket closed meanwhile is noticed.
                    let _ = os_sys::wait_handles(&mut item, 200_000_000);
                }
                Err(e) => break (0, if SOCKETS.lock().unwrap().contains_key(&s) { wsa(e) } else { WSA_OPERATION_ABORTED }),
                Ok((n, peer)) => {
                    trace_data("WSARecv (overlapped)", s, &data[..n]);
                    let truncated = datagram_truncated(fd, stream, n).unwrap_or(false);
                    let mut done = 0;
                    for &(len, p) in &parts {
                        let take = (len as usize).min(n - done);
                        if take > 0 {
                            // SAFETY: the caller's WSABUF of `len` bytes.
                            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), p as *mut u8, take) };
                            done += take;
                        }
                    }
                    if from != 0 {
                        write_addr(&peer, from as *mut u8, fromlen as *mut i32, s);
                    }
                    // A datagram larger than the buffers: what fit, and WSAEMSGSIZE.
                    break (done, if truncated { 10040 } else { 0 });
                }
            }
        };
        PENDING.lock().unwrap().remove(&o);
        if super::service::release_fd(fd) {
            let _ = os_sys::close(fd);
        }
        finish_to(port, o, bytes, error);
    });
    // No thread for the operation: it fails now, as Windows fails it without
    // the resources to queue it.
    if started.is_err() {
        PENDING.lock().unwrap().remove(&o);
        if super::service::release_fd(fd) {
            let _ = os_sys::close(fd);
        }
        return err(WSAENOBUFS, SOCKET_ERROR);
    }
    err(WSA_IO_PENDING, SOCKET_ERROR)
}

/// An overlapped send: what can be sent at once is, and the operation
/// completes in the call (0); the rest is sent from a helper thread as the
/// socket becomes writable, and the call returns WSA_IO_PENDING.
pub(super) fn send(s: u64, fd: u64, data: Vec<u8>, to: Option<SockAddr>, o: *mut u8, sent: *mut u32) -> i32 {
    let o = o as u64;
    let first = match os_sys::sock_send(fd, &data, to.as_ref(), net::MSG_DONTWAIT) {
        Ok(n) => n,
        Err(Errno::WouldBlock) => 0,
        Err(e) => return err(wsa(e), SOCKET_ERROR),
    };
    if first == data.len() {
        if !sent.is_null() {
            // SAFETY: the caller's out count.
            unsafe { *sent = first as u32 };
        }
        finish_now(s, o, first);
        return 0;
    }
    // SAFETY: the caller's OVERLAPPED.
    unsafe { ov::set_status(o, STATUS_PENDING) };
    PENDING.lock().unwrap().insert(o, (s, os_sys::gettid(), false));
    let port = PORTS.lock().unwrap().get(&s).copied();
    super::service::hold_fd(fd);
    let started = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let mut done = first;
        let error = loop {
            if done == data.len() {
                break 0;
            }
            let cancelled = PENDING.lock().unwrap().get(&o).is_none_or(|p| p.2);
            if cancelled || !SOCKETS.lock().unwrap().contains_key(&s) {
                break WSA_OPERATION_ABORTED;
            }
            match os_sys::sock_send(fd, &data[done..], to.as_ref(), net::MSG_DONTWAIT) {
                Ok(n) => done += n,
                Err(Errno::WouldBlock) => {
                    let mut item = [os_sys::ipc::WaitItem { handle: fd, events: osyx_abi::ipc::WAIT_WRITABLE, revents: 0 }];
                    let _ = os_sys::wait_handles(&mut item, 200_000_000);
                }
                Err(e) => break wsa(e),
            }
        };
        PENDING.lock().unwrap().remove(&o);
        if super::service::release_fd(fd) {
            let _ = os_sys::close(fd);
        }
        finish_to(port, o, done, error);
    });
    // No thread for the operation: it fails now, as Windows fails it without
    // the resources to queue it.
    if started.is_err() {
        PENDING.lock().unwrap().remove(&o);
        if super::service::release_fd(fd) {
            let _ = os_sys::close(fd);
        }
        return err(WSAENOBUFS, SOCKET_ERROR);
    }
    err(WSA_IO_PENDING, SOCKET_ERROR)
}
