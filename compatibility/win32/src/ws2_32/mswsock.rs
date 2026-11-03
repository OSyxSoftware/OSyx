//! `mswsock.dll` and the Winsock extension functions: AcceptEx,
//! GetAcceptExSockaddrs, ConnectEx and DisconnectEx.
//!
//! AcceptEx accepts a connection on the listening socket into the
//! caller's unconnected socket, optionally receives the first block of
//! data, and stores both addresses after it; ConnectEx connects and sends
//! a first block; DisconnectEx shuts the connection down. They complete
//! in the call, so they return TRUE when done; with an OVERLAPPED they
//! also fill Internal/InternalHigh, signal hEvent and queue a packet to
//! the socket's completion port (overlapped.rs).
use super::*;

/// sizeof(sockaddr_in) + 16: the address room AcceptEx asks for.
const MIN_ADDR_ROOM: u32 = 32;

/// Reports a finished operation: the out count and the OVERLAPPED
/// {Internal, InternalHigh, Offset, OffsetHigh, hEvent}.
/// A completed operation also queues a packet to the completion port
/// socket `s` is associated with.
fn complete(s: u64, r: Result<u32, u32>, done: *mut u32, overlapped: *mut u8) -> i32 {
    // SAFETY: the caller's out count and OVERLAPPED.
    unsafe {
        match r {
            Ok(n) => {
                if !done.is_null() {
                    *done = n;
                }
                if !overlapped.is_null() {
                    super::overlapped::finish(s, overlapped as u64, n as usize, 0);
                }
                1
            }
            Err(e) => {
                if !overlapped.is_null() {
                    crate::kernel32::overlapped::set_status(overlapped as u64, 0xC000_0001); // STATUS_UNSUCCESSFUL
                }
                err(e, 0)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn AcceptEx(
    listen_s: u64,
    accept_s: u64,
    out: *mut u8,
    recv_len: u32,
    local_len: u32,
    remote_len: u32,
    received: *mut u32,
    overlapped: *mut u8,
) -> i32 {
    let r = (|| {
        let listener = socket_state(listen_s)?;
        let target = socket_state(accept_s)?;
        if !listener.stream || !target.stream {
            return Err(WSAEOPNOTSUPP);
        }
        if out.is_null() || local_len < MIN_ADDR_ROOM || remote_len < MIN_ADDR_ROOM {
            return Err(WSAEFAULT);
        }
        events::rearm(listen_s, 0x08);
        // AcceptEx waits for a connection even on a non-blocking listener.
        let (nfd, peer) = blocking(listen_s, |fd| os_sys::sock_accept(fd).map_err(wsa))?;
        // The first block arrives while the new endpoint is still blocking;
        // a failure leaves the caller's socket as it was.
        let mut n = 0;
        if recv_len > 0 {
            // SAFETY: the caller's buffer of recv_len + local_len +
            // remote_len bytes.
            let data = unsafe { core::slice::from_raw_parts_mut(out, recv_len as usize) };
            n = match os_sys::sock_recv(nfd, data, 0) {
                Ok((n, _)) => n,
                Err(e) => {
                    let _ = os_sys::close(nfd);
                    return Err(wsa(e));
                }
            };
        }
        if target.nonblocking {
            if let Err(e) = os_sys::sock_ctl(nfd, net::ctl::NONBLOCK, 1) {
                let _ = os_sys::close(nfd);
                return Err(wsa(e));
            }
        }
        // The accepted connection takes over the caller's socket, which a
        // DisconnectEx may have shut down before its reuse.
        {
            let mut socks = SOCKETS.lock().unwrap();
            let Some(k) = socks.get_mut(&accept_s) else {
                let _ = os_sys::close(nfd);
                return Err(WSAENOTSOCK);
            };
            let _ = os_sys::close(k.fd);
            k.fd = nfd;
            k.read_shutdown = false;
            k.write_shutdown = false;
        }
        let mut local = SockAddr::default();
        let _ = os_sys::sock_ctl(nfd, net::ctl::LOCAL_ADDR, &mut local as *mut SockAddr as u64);
        let mut room = 16i32;
        // SAFETY: as above: the address areas follow the data.
        unsafe {
            write_addr(&local, out.add(recv_len as usize), &mut room);
            room = 16;
            write_addr(&peer, out.add((recv_len + local_len) as usize), &mut room);
        }
        Ok(n as u32)
    })();
    complete(listen_s, r, received, overlapped)
}

/// The addresses AcceptEx stored after the received data.
#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn GetAcceptExSockaddrs(
    out: *mut u8,
    recv_len: u32,
    local_len: u32,
    _remote_len: u32,
    local: *mut *mut u8,
    local_size: *mut i32,
    remote: *mut *mut u8,
    remote_size: *mut i32,
) {
    if out.is_null() {
        return;
    }
    // SAFETY: the buffer AcceptEx filled and the caller's out pointers.
    unsafe {
        if !local.is_null() {
            *local = out.add(recv_len as usize);
        }
        if !local_size.is_null() {
            *local_size = 16;
        }
        if !remote.is_null() {
            *remote = out.add((recv_len + local_len) as usize);
        }
        if !remote_size.is_null() {
            *remote_size = 16;
        }
    }
}

/// Runs `f` on the socket's kernel endpoint in blocking mode, restoring
/// non-blocking mode afterwards.
fn blocking<T>(s: u64, f: impl FnOnce(u64) -> Result<T, u32>) -> Result<T, u32> {
    let st = socket_state(s)?;
    if st.nonblocking {
        os_sys::sock_ctl(st.fd, net::ctl::NONBLOCK, 0).map_err(wsa)?;
    }
    let r = f(st.fd);
    if st.nonblocking {
        let _ = os_sys::sock_ctl(st.fd, net::ctl::NONBLOCK, 1);
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn ConnectEx(
    s: u64,
    name: *const u8,
    name_len: i32,
    buf: *const u8,
    len: u32,
    sent: *mut u32,
    overlapped: *mut u8,
) -> i32 {
    let r = (|| {
        let st = socket_state(s)?;
        if !st.stream {
            return Err(WSAEOPNOTSUPP);
        }
        let a = read_addr(name, name_len)?;
        blocking(s, |fd| {
            os_sys::sock_connect(fd, &a).map_err(wsa)?;
            // A socket reused after DisconnectEx is open again.
            if let Some(k) = SOCKETS.lock().unwrap().get_mut(&s) {
                k.read_shutdown = false;
                k.write_shutdown = false;
            }
            if len == 0 || buf.is_null() {
                return Ok(0);
            }
            // SAFETY: the caller's `len` bytes.
            let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
            os_sys::sock_send(fd, data, None, 0).map(|n| n as u32).map_err(wsa)
        })
    })();
    complete(s, r, sent, overlapped)
}

/// DisconnectEx: ends the connection (both directions); TF_REUSE_SOCKET
/// is accepted, the handle stays valid either way.
pub(crate) extern "win64" fn DisconnectEx(s: u64, overlapped: *mut u8, _flags: u32, _reserved: u32) -> i32 {
    let r = (|| {
        let (fd, _) = fd(s)?;
        os_sys::sock_ctl(fd, net::ctl::SHUTDOWN_WRITE, 0).map_err(wsa)?;
        if let Some(k) = SOCKETS.lock().unwrap().get_mut(&s) {
            k.read_shutdown = true;
            k.write_shutdown = true;
        }
        Ok(0)
    })();
    complete(s, r, core::ptr::null_mut(), overlapped)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![AcceptEx, GetAcceptExSockaddrs]
}
