//! `mswsock.dll` and the Winsock extension functions: AcceptEx,
//! GetAcceptExSockaddrs, ConnectEx and DisconnectEx.
//!
//! AcceptEx accepts a connection on the listening socket into the
//! caller's unconnected socket, optionally receives the first block of
//! data, and stores both addresses after it; ConnectEx connects and sends
//! a first block; DisconnectEx shuts the connection down. What can be
//! done at once completes in the call (TRUE); AcceptEx waiting for a
//! connection and ConnectEx's handshake run on a helper thread and the
//! call returns ERROR_IO_PENDING. Completion fills Internal/InternalHigh,
//! signals hEvent and queues a packet to the socket's completion port
//! (overlapped.rs).
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
                    super::overlapped::finish_now(s, overlapped as u64, n as usize);
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

/// What AcceptEx does with a connection once the listener has one.
#[derive(Clone, Copy)]
struct Accept {
    accept_s: u64,
    /// The caller's buffer: `recv_len` bytes of data, then the local and
    /// the remote address areas.
    out: u64,
    recv_len: u32,
    local_len: u32,
}

impl Accept {
    /// Receives the first block (if asked for) and hands connection `nfd`
    /// to the caller's socket, storing both addresses. `stop` ends a wait
    /// for the first block (WSA_OPERATION_ABORTED). Returns the bytes
    /// received; on failure `nfd` is closed and the socket left as it was.
    fn finish(self, nfd: u64, peer: SockAddr, stop: &dyn Fn() -> bool) -> Result<u32, u32> {
        let fail = |e: u32| {
            let _ = os_sys::close(nfd);
            Err(e)
        };
        let mut n = 0;
        if self.recv_len > 0 {
            // SAFETY: the caller's buffer of recv_len + local_len +
            // remote_len bytes, which stays valid until completion.
            let data = unsafe { core::slice::from_raw_parts_mut(self.out as *mut u8, self.recv_len as usize) };
            n = loop {
                if stop() {
                    return fail(WSA_OPERATION_ABORTED);
                }
                match os_sys::sock_recv(nfd, data, net::MSG_DONTWAIT) {
                    Ok((n, _)) => break n,
                    Err(osyx_abi::Errno::WouldBlock) => super::overlapped::wait_ready(nfd, osyx_abi::ipc::WAIT_READABLE),
                    Err(e) => return fail(wsa(e)),
                }
            };
        }
        let target = match socket_state(self.accept_s) {
            Ok(t) => t,
            Err(e) => return fail(e),
        };
        if target.nonblocking {
            if let Err(e) = os_sys::sock_ctl(nfd, net::ctl::NONBLOCK, 1) {
                return fail(wsa(e));
            }
        }
        // The accepted connection takes over the caller's socket, which a
        // DisconnectEx may have shut down before its reuse.
        {
            let mut socks = SOCKETS.lock().unwrap();
            let Some(k) = socks.get_mut(&self.accept_s) else {
                drop(socks);
                return fail(WSAENOTSOCK);
            };
            let _ = os_sys::close(k.fd);
            k.fd = nfd;
            k.read_shutdown = false;
            k.write_shutdown = false;
        }
        let mut local = SockAddr::default();
        let _ = os_sys::sock_ctl(nfd, net::ctl::LOCAL_ADDR, &mut local as *mut SockAddr as u64);
        let out = self.out as *mut u8;
        let size = addr_size(self.accept_s);
        let mut room = size;
        // SAFETY: as above: the address areas follow the data.
        unsafe {
            write_addr(&local, out.add(self.recv_len as usize), &mut room, self.accept_s);
            room = size;
            write_addr(&peer, out.add((self.recv_len + self.local_len) as usize), &mut room, self.accept_s);
        }
        Ok(n as u32)
    }
}

const WSA_IO_PENDING: u32 = 997;
const WSA_OPERATION_ABORTED: u32 = 995;

/// AcceptEx: a connection already pending is taken in the call (TRUE);
/// otherwise the call returns ERROR_IO_PENDING and a helper thread waits
/// for one (and for the first block of data, when asked for), completing
/// the OVERLAPPED. Without an OVERLAPPED (which Windows does not allow)
/// the call waits.
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
    let checked = (|| {
        let listener = socket_state(listen_s)?;
        let target = socket_state(accept_s)?;
        if !listener.stream || !target.stream {
            return Err(WSAEOPNOTSUPP);
        }
        // A sockaddr_in6 is 12 bytes longer.
        let min = MIN_ADDR_ROOM + if target.v6 { 12 } else { 0 };
        if out.is_null() || local_len < min || remote_len < min {
            return Err(WSAEFAULT);
        }
        events::rearm(listen_s, 0x08);
        Ok(listener.fd)
    })();
    let lfd = match checked {
        Ok(fd) => fd,
        Err(e) => return complete(listen_s, Err(e), received, overlapped),
    };
    let job = Accept { accept_s, out: out as u64, recv_len, local_len };
    let never = || false;
    if overlapped.is_null() {
        let r = blocking(listen_s, |fd| os_sys::sock_accept(fd).map_err(wsa)).and_then(|(nfd, peer)| job.finish(nfd, peer, &never));
        return complete(listen_s, r, received, overlapped);
    }
    // A connection already waiting, with no data to wait for, completes now.
    if recv_len == 0 {
        match os_sys::sock_accept_flags(lfd, net::MSG_DONTWAIT) {
            Ok((nfd, peer)) => return complete(listen_s, job.finish(nfd, peer, &never), received, overlapped),
            Err(osyx_abi::Errno::WouldBlock) => {}
            Err(e) => return complete(listen_s, Err(wsa(e)), received, overlapped),
        }
    }
    let started = super::overlapped::run(listen_s, lfd, overlapped, move |stop| {
        let (nfd, peer) = loop {
            if stop() {
                return Err(WSA_OPERATION_ABORTED);
            }
            match os_sys::sock_accept_flags(lfd, net::MSG_DONTWAIT) {
                Ok(c) => break c,
                Err(osyx_abi::Errno::WouldBlock) => super::overlapped::wait_ready(lfd, osyx_abi::ipc::WAIT_READABLE),
                Err(e) => return Err(wsa(e)),
            }
        };
        job.finish(nfd, peer, stop)
    });
    if !started {
        return complete(listen_s, Err(WSAENOBUFS), received, overlapped);
    }
    err(WSA_IO_PENDING, 0)
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
        // The family AcceptEx wrote says the size.
        let size = |p: *const u8| if core::ptr::read_unaligned(p as *const u16) == AF_INET6 as u16 { 28 } else { 16 };
        if !local_size.is_null() {
            *local_size = size(out.add(recv_len as usize));
        }
        if !remote.is_null() {
            *remote = out.add((recv_len + local_len) as usize);
        }
        if !remote_size.is_null() {
            *remote_size = size(out.add((recv_len + local_len) as usize));
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
    let checked = (|| {
        let st = socket_state(s)?;
        if !st.stream {
            return Err(WSAEOPNOTSUPP);
        }
        let a = read_addr(name, name_len, s)?;
        // The first block is copied now: the caller's buffer is theirs
        // again once the call returns.
        let data = if len == 0 || buf.is_null() {
            Vec::new()
        } else {
            // SAFETY: the caller's `len` bytes.
            unsafe { core::slice::from_raw_parts(buf, len as usize) }.to_vec()
        };
        Ok((st.fd, a, data))
    })();
    let (fd, a, data) = match checked {
        Ok(v) => v,
        Err(e) => return complete(s, Err(e), sent, overlapped),
    };
    let connect = move || {
        blocking(s, |fd| {
            os_sys::sock_connect(fd, &a).map_err(wsa)?;
            // A socket reused after DisconnectEx is open again.
            if let Some(k) = SOCKETS.lock().unwrap().get_mut(&s) {
                k.read_shutdown = false;
                k.write_shutdown = false;
            }
            if data.is_empty() {
                return Ok(0);
            }
            os_sys::sock_send(fd, &data, None, 0).map(|n| n as u32).map_err(wsa)
        })
    };
    if overlapped.is_null() {
        return complete(s, connect(), sent, overlapped);
    }
    // The handshake runs on a helper thread: the call returns
    // ERROR_IO_PENDING and the OVERLAPPED completes with the result.
    if !super::overlapped::run(s, fd, overlapped, move |_| connect()) {
        return complete(s, Err(WSAENOBUFS), sent, overlapped);
    }
    err(WSA_IO_PENDING, 0)
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
