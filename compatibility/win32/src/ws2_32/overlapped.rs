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
use crate::kernel32::overlapped as ov;

/// Sockets associated with a completion port: (port, key).
static PORTS: Mutex<BTreeMap<u64, (u64, u64)>> = Mutex::new(BTreeMap::new());
/// Receives under way, by OVERLAPPED: their socket, and whether they were
/// cancelled.
static PENDING: Mutex<BTreeMap<u64, (u64, bool)>> = Mutex::new(BTreeMap::new());

/// CancelIoEx on a socket: the receives under way on it (all, or the one
/// with OVERLAPPED `o`) end with WSA_OPERATION_ABORTED. False if there was
/// none.
pub(crate) fn cancel(s: u64, o: u64) -> bool {
    let mut found = false;
    for (k, (sock, cancelled)) in PENDING.lock().unwrap().iter_mut() {
        if *sock == s && (o == 0 || *k == o) {
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

/// CreateIoCompletionPort for a socket: false if `s` is not one.
pub(crate) fn associate(s: u64, port: u64, key: u64) -> bool {
    if !SOCKETS.lock().unwrap().contains_key(&s) {
        return false;
    }
    PORTS.lock().unwrap().insert(s, (port, key));
    true
}

pub(super) fn forget(s: u64) {
    PORTS.lock().unwrap().remove(&s);
    SKIP.lock().unwrap().remove(&s);
}

/// Sockets with FILE_SKIP_COMPLETION_PORT_ON_SUCCESS.
static SKIP: Mutex<alloc::collections::BTreeSet<u64>> = Mutex::new(alloc::collections::BTreeSet::new());

pub(super) fn set_skip(s: u64, on: bool) {
    let mut skip = SKIP.lock().unwrap();
    if on {
        skip.insert(s);
    } else {
        skip.remove(&s);
    }
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
        let port = PORTS.lock().unwrap().get(&s).copied();
        if let Some((port, key)) = port {
            crate::threadpool::post(port, bytes as u32, key, o, error);
        }
    }
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
    PENDING.lock().unwrap().insert(o, (s, false));
    os_std::thread::spawn(move || {
        let total: usize = parts.iter().map(|p| p.0 as usize).sum();
        let mut data = alloc::vec![0u8; total];
        let (bytes, error) = loop {
            let cancelled = PENDING.lock().unwrap().get(&o).is_none_or(|p| p.1);
            if cancelled || !SOCKETS.lock().unwrap().contains_key(&s) {
                break (0, WSA_OPERATION_ABORTED);
            }
            match os_sys::sock_recv(fd, &mut data, fl) {
                Err(Errno::WouldBlock) => {
                    let mut item = [os_sys::ipc::WaitItem { handle: fd, events: osyx_abi::ipc::WAIT_READABLE, revents: 0 }];
                    // A bounded wait, so a socket closed meanwhile is noticed.
                    let _ = os_sys::wait_handles(&mut item, 200_000_000);
                }
                Err(e) => break (0, if SOCKETS.lock().unwrap().contains_key(&s) { wsa(e) } else { WSA_OPERATION_ABORTED }),
                Ok((n, peer)) => {
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
                        write_addr(&peer, from as *mut u8, fromlen as *mut i32);
                    }
                    // A datagram larger than the buffers: what fit, and WSAEMSGSIZE.
                    break (done, if truncated { 10040 } else { 0 });
                }
            }
        };
        PENDING.lock().unwrap().remove(&o);
        finish(s, o, bytes, error);
    });
    err(WSA_IO_PENDING, SOCKET_ERROR)
}
