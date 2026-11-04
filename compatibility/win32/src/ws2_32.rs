//! ws2_32.dll: Winsock 2 over the OSyx network stack. A SOCKET is an entry
//! in this table that owns an OSyx kernel socket (TCP or UDP, IPv4);
//! blocking calls block in the kernel, names resolve through the kernel's
//! DNS resolver, and errors are the documented WSA codes (also returned by
//! GetLastError, as on Windows).

#![allow(non_snake_case)]

use crate::teb;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use os_std::sync::Mutex;
use os_sys::net::{self, SockAddr};
use osyx_abi::Errno;

mod events;
mod overlapped;
mod service;

/// CancelIoEx (every thread's operations) and CancelIo (the calling
/// thread's) on a socket handle (see overlapped.rs).
pub(crate) fn cancel_socket_io(s: u64, ov: u64, own_thread_only: bool) -> bool {
    overlapped::cancel(s, ov, own_thread_only.then(os_sys::gettid))
}

/// SetFileCompletionNotificationModes' FILE_SKIP_COMPLETION_PORT_ON_SUCCESS
/// (it cannot be turned off again).
pub(crate) fn set_skip_on_success(s: u64) {
    overlapped::set_skip(s);
}

/// Whether `s` is an open socket handle.
pub(crate) fn is_socket(s: u64) -> bool {
    SOCKETS.lock().unwrap().contains_key(&s)
}

/// The completion port a socket handle is associated with.
pub(crate) fn socket_port(s: u64) -> Option<u64> {
    overlapped::port_of(s)
}

/// CreateIoCompletionPort for a socket handle.
pub(crate) fn associate_socket(s: u64, port: u64, key: u64) -> Result<(), u32> {
    overlapped::associate(s, port, key)
}
pub(crate) mod mswsock;

const INVALID_SOCKET: u64 = u64::MAX;
const SOCKET_ERROR: i32 = -1;
const AF_INET: u16 = 2;
const AF_INET6: i32 = 23;
const AI_PASSIVE: i32 = 1;
const AI_NUMERICHOST: i32 = 4;
const AI_NUMERICSERV: i32 = 8;
const AI_V4MAPPED: i32 = 0x800;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOL_SOCKET: i32 = 0xFFFF;
const SO_REUSEADDR: i32 = 0x4;
const SO_KEEPALIVE: i32 = 0x8;
const SO_LINGER: i32 = 0x80;
const SO_BROADCAST: i32 = 0x20;
const SO_SNDTIMEO: i32 = 0x1005;
const SO_RCVTIMEO: i32 = 0x1006;
const SO_ERROR: i32 = 0x1007;
const SO_TYPE: i32 = 0x1008;
const SO_SNDBUF: i32 = 0x1001;
const SO_RCVBUF: i32 = 0x1002;
/// mswsock: the socket AcceptEx accepted on takes the listening socket's
/// properties; ConnectEx's socket becomes an ordinary connected one.
const SO_UPDATE_ACCEPT_CONTEXT: i32 = 0x700B;
const SO_UPDATE_CONNECT_CONTEXT: i32 = 0x7010;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_UDP: i32 = 17;
const IPPROTO_IPV6: i32 = 41;
const IPV6_V6ONLY: i32 = 27;
const TCP_NODELAY: i32 = 1;
/// Hands out AcceptEx, ConnectEx and the other extension functions.
const SIO_GET_EXTENSION_FUNCTION_POINTER: u32 = 0xC800_0006;
const FIONBIO: u32 = 0x8004_667E;
const SIO_KEEPALIVE_VALS: u32 = 0x9800_0004;
const SIO_LOOPBACK_FAST_PATH: u32 = 0x9800_0010;
const SIO_IDEAL_SEND_BACKLOG_QUERY: u32 = 0x4004_747B;
/// The stack's per-connection send buffer (network/net/src/tcp.rs).
const SEND_BACKLOG: u32 = 64 * 1024;
/// The stack's per-connection receive buffer (network/net/src/tcp.rs).
const RECV_BUFFER_SIZE: u32 = 256 * 1024;
const MSG_PEEK: i32 = 2;
const MSG_WAITALL: i32 = 8;

const WSAEINTR: u32 = 10004;
const WSAEACCES: u32 = 10013;
const WSAEFAULT: u32 = 10014;
const WSAEINVAL: u32 = 10022;
const WSAEMFILE: u32 = 10024;
const WSAEWOULDBLOCK: u32 = 10035;
const WSAENOTSOCK: u32 = 10038;
const WSAEMSGSIZE: u32 = 10040;
const WSAENOPROTOOPT: u32 = 10042;
const WSAEPROTONOSUPPORT: u32 = 10043;
const WSAEOPNOTSUPP: u32 = 10045;
const WSAEAFNOSUPPORT: u32 = 10047;
const WSAEADDRINUSE: u32 = 10048;
const WSAEADDRNOTAVAIL: u32 = 10049;
const WSAENETUNREACH: u32 = 10051;
const WSAECONNRESET: u32 = 10054;
const WSAENOBUFS: u32 = 10055;
const WSAEISCONN: u32 = 10056;
const WSAENOTCONN: u32 = 10057;
const WSAESHUTDOWN: u32 = 10058;
const WSAETIMEDOUT: u32 = 10060;
const WSAECONNREFUSED: u32 = 10061;
const WSAEHOSTUNREACH: u32 = 10065;
const WSANOTINITIALISED: u32 = 10093;
const WSA_INVALID_HANDLE: u32 = 6;
const WSA_IO_INCOMPLETE: u32 = 996;
const WSAHOST_NOT_FOUND: u32 = 11001;
const WSATRY_AGAIN: u32 = 11002;
const WSAVERNOTSUPPORTED: u32 = 10092;
const WSANO_DATA: u32 = 11004;
const WSAEINVALIDPROVIDER: u32 = 10105;
const WSATYPE_NOT_FOUND: u32 = 10109;
const ERROR_INVALID_PARAMETER: u32 = 87;

fn wsa(e: Errno) -> u32 {
    match e {
        Errno::WouldBlock => WSAEWOULDBLOCK,
        Errno::TimedOut => WSAETIMEDOUT,
        Errno::AddrInUse => WSAEADDRINUSE,
        Errno::AddrNotAvailable => WSAEADDRNOTAVAIL,
        Errno::NetUnreachable => WSAENETUNREACH,
        Errno::HostUnreachable => WSAEHOSTUNREACH,
        Errno::ConnRefused => WSAECONNREFUSED,
        Errno::ConnReset | Errno::BrokenPipe => WSAECONNRESET,
        Errno::NotConnected => WSAENOTCONN,
        Errno::AlreadyConnected => WSAEISCONN,
        Errno::MessageTooLong => WSAEMSGSIZE,
        Errno::NameNotFound => WSAHOST_NOT_FOUND,
        Errno::DnsFailure => WSATRY_AGAIN,
        Errno::Interrupted => WSAEINTR,
        Errno::PermissionDenied => WSAEACCES,
        Errno::Fault => WSAEFAULT,
        Errno::NoMemory => WSAENOBUFS,
        Errno::TooManyHandles => WSAEMFILE,
        Errno::NotSupported | Errno::Unsupported => WSAEOPNOTSUPP,
        _ => WSAEINVAL,
    }
}

#[track_caller]
/// Whether Winsock calls are logged: under -v, or OSYX_WINRUN_FAILTRACE.
fn traced() -> bool {
    crate::VERBOSE.load(Ordering::Relaxed) || crate::kernel32::TRACE.load(Ordering::Relaxed)
}

/// Under OSYX_WINRUN_FAILTRACE (or -v): the first bytes of the first
/// socket transfers, as text with other bytes escaped.
fn trace_data(what: &str, s: u64, data: &[u8]) {
    static LINES: AtomicU32 = AtomicU32::new(0);
    if !traced() || LINES.fetch_add(1, Ordering::Relaxed) >= 300 {
        return;
    }
    let mut text = String::new();
    for &b in data.iter().take(240) {
        match b {
            b'\r' => text.push_str(r"\r"),
            b'\n' => text.push_str(r"\n"),
            0x20..=0x7E => text.push(b as char),
            _ => text.push_str(&alloc::format!(r"\x{b:02x}")),
        }
    }
    os_std::eprintln!("winrun: {what} {s:#x} {} byte(s): \"{text}\"", data.len());
}

fn err<T>(code: u32, v: T) -> T {
    static ERRORS: AtomicU32 = AtomicU32::new(0);
    if code != WSAEWOULDBLOCK && traced() && ERRORS.fetch_add(1, Ordering::Relaxed) < 128 {
        os_std::eprintln!("winrun: Winsock error {code} in thread {} at {}", os_sys::gettid(), core::panic::Location::caller());
    }
    teb::set_last_error(code);
    v
}

#[derive(Clone, Copy)]
struct Sock {
    fd: u64,
    stream: bool,
    nonblocking: bool,
    /// SO_LINGER seconds; zero means off.
    linger: u16,
    /// SO_RCVTIMEO/SO_SNDTIMEO in milliseconds; zero means forever.
    recv_timeout: u32,
    send_timeout: u32,
    /// SO_REUSEADDR/SO_KEEPALIVE, accepted as no-ops but reported back.
    reuseaddr: bool,
    keepalive: bool,
    /// SO_RCVBUF/SO_SNDBUF as set (None: never set; zero is a valid
    /// setting). The stack's buffers have fixed sizes; the values are kept
    /// so getsockopt reports them back.
    rcvbuf: Option<u32>,
    sndbuf: Option<u32>,
    /// `shutdown(SD_RECEIVE)` / `shutdown(SD_SEND)` were called.
    read_shutdown: bool,
    write_shutdown: bool,
    /// An AF_INET6 socket: its addresses are sockaddr_in6, carried by the
    /// IPv4 stack (see `v6_to_v4`).
    v6: bool,
    /// IPV6_V6ONLY, on by default as on Windows: IPv4-mapped addresses are
    /// refused.
    v6only: bool,
}

static SOCKETS: Mutex<BTreeMap<u64, Sock>> = Mutex::new(BTreeMap::new());

/// Sets the blocking mode of kernel socket `fd` in every handle to it:
/// duplicated handles (WSADuplicateSocket) share the socket's state.
fn set_nonblocking(socks: &mut BTreeMap<u64, Sock>, fd: u64, on: bool) {
    for k in socks.values_mut().filter(|k| k.fd == fd) {
        k.nonblocking = on;
    }
}
static NEXT: Mutex<u64> = Mutex::new(0x1000);
static STARTED: AtomicU32 = AtomicU32::new(0);

fn trace_io(args: core::fmt::Arguments<'_>) {
    static LINES: AtomicU32 = AtomicU32::new(0);
    if crate::kernel32::TRACE.load(Ordering::Relaxed) && LINES.fetch_add(1, Ordering::Relaxed) < 96 {
        os_std::eprintln!("winrun: socket I/O thread {}: {args}", os_sys::gettid());
    }
}

fn started() -> bool {
    STARTED.load(Ordering::SeqCst) > 0
}

/// The kernel socket of SOCKET `s`.
fn fd(s: u64) -> Result<(u64, bool), u32> {
    socket_state(s).map(|s| (s.fd, s.stream))
}

/// Reject I/O after the matching half of the socket was shut down.
fn check_shutdown(s: u64, read: bool) -> Result<(), u32> {
    let st = socket_state(s)?;
    // Every receive and send passes here: they re-enable FD_READ and
    // FD_WRITE for WSAEventSelect.
    events::rearm(s, if read { 0x05 } else { 0x02 });
    if (read && st.read_shutdown) || (!read && st.write_shutdown) {
        return Err(WSAESHUTDOWN);
    }
    Ok(())
}

fn socket_state(s: u64) -> Result<Sock, u32> {
    if !started() {
        return Err(WSANOTINITIALISED);
    }
    SOCKETS.lock().unwrap().get(&s).copied().ok_or(WSAENOTSOCK)
}

/// The IPv4 address an IPv6 address stands for. OSyx has only the IPv4
/// stack, so an AF_INET6 socket reaches what IPv4 can: ::1 is 127.0.0.1,
/// :: is 0.0.0.0 and ::ffff:a.b.c.d is a.b.c.d (the bool: this mapped
/// form). Any other address is beyond this host, as on a Windows machine
/// without IPv6 connectivity.
fn v6_to_v4(a: &[u8; 16]) -> Option<([u8; 4], bool)> {
    if a[..15].iter().all(|&b| b == 0) && a[15] <= 1 {
        return Some((if a[15] == 1 { [127, 0, 0, 1] } else { [0; 4] }, false));
    }
    if a[..10].iter().all(|&b| b == 0) && a[10] == 0xFF && a[11] == 0xFF {
        return Some(([a[12], a[13], a[14], a[15]], true));
    }
    None
}

/// The IPv6 form an AF_INET6 socket reports for an IPv4 address of the
/// stack: 127.0.0.1 is ::1, 0.0.0.0 is ::, others are IPv4-mapped.
fn v4_to_v6(a: [u8; 4]) -> [u8; 16] {
    let mut b = [0u8; 16];
    match a {
        [127, 0, 0, 1] => b[15] = 1,
        [0, 0, 0, 0] => {}
        _ => {
            b[10] = 0xFF;
            b[11] = 0xFF;
            b[12..].copy_from_slice(&a);
        }
    }
    b
}

/// The sockaddr size of socket `s`'s family.
fn addr_size(s: u64) -> i32 {
    if socket_state(s).is_ok_and(|k| k.v6) { 28 } else { 16 }
}

/// The address the caller gives socket `s`: a sockaddr_in (family, port
/// big-endian, address, zero[8]), or a sockaddr_in6 (family, port, flow
/// information, address, scope) for an AF_INET6 socket. An IPv6 address
/// IPv4 cannot carry (see `v6_to_v4`) is unreachable.
fn read_addr(p: *const u8, len: i32, s: u64) -> Result<SockAddr, u32> {
    let k = socket_state(s)?;
    let (v6, v6only) = (k.v6, k.v6only);
    if p.is_null() || len < if v6 { 28 } else { 16 } {
        return Err(WSAEFAULT);
    }
    // SAFETY: the caller's sockaddr of at least 16 (28) bytes.
    let b = unsafe { core::slice::from_raw_parts(p, if v6 { 28 } else { 16 }) };
    let port = u16::from_be_bytes([b[2], b[3]]);
    let family = u16::from_le_bytes([b[0], b[1]]);
    if !v6 {
        if family != AF_INET {
            return Err(WSAEAFNOSUPPORT);
        }
        return Ok(SockAddr::new([b[4], b[5], b[6], b[7]], port));
    }
    if family != AF_INET6 as u16 {
        return Err(WSAEAFNOSUPPORT);
    }
    let raw: [u8; 16] = b[8..24].try_into().expect("16 bytes");
    match v6_to_v4(&raw) {
        Some((a, mapped)) if !(mapped && v6only) => Ok(SockAddr::new(a, port)),
        _ => Err(WSAENETUNREACH),
    }
}

/// Writes `a` as socket `s`'s family gives it (see `put_addr`).
fn write_addr(a: &SockAddr, p: *mut u8, len: *mut i32, s: u64) {
    put_addr(a, p, len, socket_state(s).is_ok_and(|k| k.v6));
}

/// Writes `a` as a sockaddr_in, or as a sockaddr_in6 (`v6`). `len` (when
/// given) is the buffer's size, then the address's.
fn put_addr(a: &SockAddr, p: *mut u8, len: *mut i32, v6: bool) {
    if p.is_null() {
        return;
    }
    let size = if v6 { 28 } else { 16 };
    // SAFETY: the caller's buffer; `len` (when given) says it holds `size`
    // bytes.
    unsafe {
        if !len.is_null() {
            if *len < size {
                return;
            }
            *len = size;
        }
        let mut b = [0u8; 28];
        b[2..4].copy_from_slice(&a.port.to_be_bytes());
        if v6 {
            b[..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
            b[8..24].copy_from_slice(&v4_to_v6(a.addr));
        } else {
            b[..2].copy_from_slice(&AF_INET.to_le_bytes());
            b[4..8].copy_from_slice(&a.addr);
        }
        core::ptr::copy_nonoverlapping(b.as_ptr(), p, size as usize);
    }
}

/// Winsock 1.0 to 2.2 are accepted (wsock32 users ask for 1.1); a newer
/// request negotiates 2.2, as on Windows.
pub(crate) extern "win64" fn WSAStartup(version: u16, data: *mut u8) -> i32 {
    let (major, minor) = (version & 0xFF, version >> 8);
    if major == 0 {
        return WSAVERNOTSUPPORTED as i32;
    }
    if !data.is_null() {
        // SAFETY: the caller's WSADATA (408 bytes on x64).
        unsafe {
            core::ptr::write_bytes(data, 0, 408);
            teb::put(data as u64, 0, if major == 1 || (major == 2 && minor <= 2) { version } else { 0x0202u16 });
            teb::put(data as u64, 2, 0x0202u16);
            let desc = b"OSyx Winsock 2.2 (native TCP/IP stack)";
            core::ptr::copy_nonoverlapping(desc.as_ptr(), data.add(16), desc.len());
            let status = b"Running";
            core::ptr::copy_nonoverlapping(status.as_ptr(), data.add(273), status.len());
        }
    }
    STARTED.fetch_add(1, Ordering::SeqCst);
    0
}

extern "win64" fn WSACleanup() -> i32 {
    if STARTED.load(Ordering::SeqCst) == 0 {
        return err(WSANOTINITIALISED, SOCKET_ERROR);
    }
    if STARTED.fetch_sub(1, Ordering::SeqCst) == 1 {
        let all: Vec<Sock> = core::mem::take(&mut *SOCKETS.lock().unwrap()).into_values().collect();
        for s in all {
            let _ = os_sys::close(s.fd);
        }
    }
    0
}

extern "win64" fn WSAGetLastError() -> i32 {
    teb::last_error() as i32
}

extern "win64" fn WSASetLastError(e: i32) {
    teb::set_last_error(e as u32)
}

extern "win64" fn socket(af: i32, kind: i32, proto: i32) -> u64 {
    if traced() {
        os_std::eprintln!("winrun: socket family {af}, kind {kind}, protocol {proto}, started {}", started());
    }
    if !started() {
        return err(WSANOTINITIALISED, INVALID_SOCKET);
    }
    if af != AF_INET as i32 && af != AF_INET6 {
        return err(WSAEAFNOSUPPORT, INVALID_SOCKET);
    }
    let (k, stream) = match (kind, proto) {
        (SOCK_STREAM, 0 | 6) => (net::SOCK_STREAM, true),
        (SOCK_DGRAM, 0 | 17) => (net::SOCK_DGRAM, false),
        _ => return err(WSAEPROTONOSUPPORT, INVALID_SOCKET),
    };
    match os_sys::socket(k) {
        Ok(fd) => {
            let mut n = NEXT.lock().unwrap();
            let s = *n;
            *n += 4;
            SOCKETS.lock().unwrap().insert(s, Sock {
                fd,
                stream,
                nonblocking: false,
                linger: 0,
                recv_timeout: 0,
                send_timeout: 0,
                reuseaddr: false,
                keepalive: false,
                rcvbuf: None,
                sndbuf: None,
                read_shutdown: false,
                write_shutdown: false,
                v6: af == AF_INET6,
                v6only: true,
            });
            s
        }
        Err(e) => err(wsa(e), INVALID_SOCKET),
    }
}

extern "win64" fn closesocket(s: u64) -> i32 {
    if !started() {
        return err(WSANOTINITIALISED, SOCKET_ERROR);
    }
    match SOCKETS.lock().unwrap().remove(&s) {
        Some(k) => {
            events::forget(s);
            overlapped::forget(s);
            // A socket duplicated with WSADuplicateSocket stays open
            // until its last handle is closed.
            if service::release_fd(k.fd) {
                let _ = os_sys::close(k.fd);
            }
            0
        }
        None => err(WSAENOTSOCK, SOCKET_ERROR),
    }
}

#[track_caller]
fn result(r: Result<(), u32>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => err(e, SOCKET_ERROR),
    }
}

extern "win64" fn bind(s: u64, addr: *const u8, len: i32) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        // An address beyond the IPv4 stack is not one of this host's.
        let a = read_addr(addr, len, s).map_err(|e| if e == WSAENETUNREACH { WSAEADDRNOTAVAIL } else { e })?;
        let r = os_sys::sock_bind(fd, &a).map_err(wsa);
        if traced() {
            os_std::eprintln!("winrun: bind {s:#x} to {:?}:{}: {r:?}", a.addr, a.port);
        }
        r
    })())
}

extern "win64" fn connect(s: u64, addr: *const u8, len: i32) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        let a = read_addr(addr, len, s)?;
        let _blocked = crate::stall::waiting("connect", s, 0);
        let r = os_sys::sock_connect(fd, &a).map_err(wsa);
        // A non-blocking connect never completes in the call on Windows: it
        // fails with WSAEWOULDBLOCK and reports the connection through
        // FD_CONNECT and select's writability (Chromium relies on it). The
        // stack connects loopback at once; that is reported the same way.
        let r = match r {
            Ok(()) if socket_state(s).is_ok_and(|k| k.nonblocking) => Err(WSAEWOULDBLOCK),
            r => r,
        };
        if traced() {
            os_std::eprintln!("winrun: connect {s:#x} to {:?}:{}: {r:?}", a.addr, a.port);
        }
        r
    })())
}

extern "win64" fn listen(s: u64, backlog: i32) -> i32 {
    result((|| {
        let (fd, stream) = fd(s)?;
        if !stream {
            return Err(WSAEOPNOTSUPP);
        }
        os_sys::sock_listen(fd, backlog.clamp(1, 128) as u32).map_err(wsa)?;
        if traced() {
            os_std::eprintln!("winrun: listen {s:#x}");
        }
        events::listening(s);
        Ok(())
    })())
}

extern "win64" fn accept(s: u64, addr: *mut u8, len: *mut i32) -> u64 {
    let r = (|| {
        let listener = socket_state(s)?;
        if !listener.stream {
            return Err(WSAEOPNOTSUPP);
        }
        // A caller-supplied address needs room for the family's sockaddr;
        // Windows rejects the call before taking a pending connection.
        if !addr.is_null() && (len.is_null() || unsafe { *len } < addr_size(s)) {
            return Err(WSAEFAULT);
        }
        events::rearm(s, 0x08);
        let blocked = crate::stall::waiting("accept", s, 0);
        let accepted = os_sys::sock_accept(listener.fd);
        drop(blocked);
        let (nfd, peer) = accepted.map_err(wsa)?;
        if traced() {
            os_std::eprintln!("winrun: accept {s:#x}: peer {:?}:{}", peer.addr, peer.port);
        }
        // Winsock accept inherits the listener's blocking mode. OSyx's
        // native accept creates a blocking endpoint, so apply it here.
        if listener.nonblocking {
            if let Err(e) = os_sys::sock_ctl(nfd, net::ctl::NONBLOCK, 1) {
                let _ = os_sys::close(nfd);
                return Err(wsa(e));
            }
        }
        write_addr(&peer, addr, len, s);
        let mut n = NEXT.lock().unwrap();
        let ns = *n;
        *n += 4;
        SOCKETS.lock().unwrap().insert(ns, Sock {
            fd: nfd,
            stream: true,
            nonblocking: listener.nonblocking,
            linger: 0,
            recv_timeout: 0,
            send_timeout: 0,
            reuseaddr: false,
            keepalive: false,
            // An accepted socket inherits the listener's buffer sizes.
            rcvbuf: listener.rcvbuf,
            sndbuf: listener.sndbuf,
            read_shutdown: false,
            write_shutdown: false,
            v6: listener.v6,
            v6only: listener.v6only,
        });
        Ok(ns)
    })();
    r.unwrap_or_else(|e| err(e, INVALID_SOCKET))
}

fn flags(f: i32) -> Result<u64, u32> {
    if f & !(MSG_PEEK | MSG_WAITALL) != 0 {
        return Err(WSAEOPNOTSUPP);
    }
    let mut fl = 0;
    if f & MSG_PEEK != 0 {
        fl |= net::MSG_PEEK;
    }
    if f & MSG_WAITALL != 0 {
        fl |= net::MSG_WAITALL;
    }
    Ok(fl)
}

extern "win64" fn send(s: u64, buf: *const u8, len: i32, f: i32) -> i32 {
    trace_io(format_args!("send({s:#x}, {len}) begin"));
    let r = (|| {
        let (fd, _) = fd(s)?;
        check_shutdown(s, false)?;
        if f != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        // SAFETY: the caller's buffer of `len` bytes; a zero-length send may
        // pass a NULL pointer, which never becomes a real slice.
        let (n, p) = (len.max(0) as usize, if len > 0 { buf } else { core::ptr::NonNull::dangling().as_ptr() });
        let data = unsafe { core::slice::from_raw_parts(p, n) };
        let r = os_sys::sock_send(fd, data, None, 0).map_err(wsa);
        if let Ok(n) = r {
            trace_data("send", s, &data[..n]);
        }
        r
    })();
    trace_io(format_args!("send({s:#x}) -> {r:?}"));
    r.map_or_else(|e| err(e, SOCKET_ERROR), |n| n as i32)
}

extern "win64" fn sendto(s: u64, buf: *const u8, len: i32, f: i32, to: *const u8, tolen: i32) -> i32 {
    let r = (|| {
        let (fd, _) = fd(s)?;
        check_shutdown(s, false)?;
        if f != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        let a = if to.is_null() { None } else { Some(read_addr(to, tolen, s)?) };
        // SAFETY: the caller's buffer of `len` bytes; a zero-length send may
        // pass a NULL pointer, which never becomes a real slice.
        let (n, p) = (len.max(0) as usize, if len > 0 { buf } else { core::ptr::NonNull::dangling().as_ptr() });
        let data = unsafe { core::slice::from_raw_parts(p, n) };
        os_sys::sock_send(fd, data, a.as_ref(), 0).map_err(wsa)
    })();
    r.map_or_else(|e| err(e, SOCKET_ERROR), |n| n as i32)
}

extern "win64" fn recv(s: u64, buf: *mut u8, len: i32, f: i32) -> i32 {
    recvfrom(s, buf, len, f, core::ptr::null_mut(), core::ptr::null_mut())
}

extern "win64" fn recvfrom(s: u64, buf: *mut u8, len: i32, f: i32, from: *mut u8, fromlen: *mut i32) -> i32 {
    trace_io(format_args!("recv({s:#x}, {len}) begin"));
    let r = (|| {
        let (fd, stream) = fd(s)?;
        check_shutdown(s, true)?;
        // Reject a too-small peer address buffer before the datagram is
        // consumed, as Windows does.
        if !from.is_null() && (fromlen.is_null() || unsafe { *fromlen } < addr_size(s)) {
            return Err(WSAEFAULT);
        }
        let fl = flags(f)?;
        // SAFETY: the caller's buffer of `len` bytes; a zero-length receive
        // may pass a NULL pointer, which never becomes a real slice.
        let (n, p) = (len.max(0) as usize, if len > 0 { buf } else { core::ptr::NonNull::dangling().as_ptr() });
        let data = unsafe { core::slice::from_raw_parts_mut(p, n) };
        let _blocked = crate::stall::waiting("recv", s, 0);
        let (n, peer) = os_sys::sock_recv(fd, data, fl).map_err(wsa)?;
        trace_data("recv", s, &data[..n]);
        write_addr(&peer, from, fromlen, s);
        // A datagram longer than the buffer was truncated; the bytes are
        // delivered, but Winsock reports the original size.
        if datagram_truncated(fd, stream, n)? {
            return Err(WSAEMSGSIZE);
        }
        Ok(n)
    })();
    trace_io(format_args!("recv({s:#x}) -> {r:?}"));
    r.map_or_else(|e| err(e, SOCKET_ERROR), |n| n as i32)
}

extern "win64" fn shutdown(s: u64, how: i32) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        let (read, write) = match how {
            0 => (true, false),       // SD_RECEIVE
            1 => (false, true),       // SD_SEND
            2 => (true, true),        // SD_BOTH
            _ => return Err(WSAEINVAL),
        };
        if write {
            os_sys::sock_ctl(fd, net::ctl::SHUTDOWN_WRITE, 0).map_err(wsa)?;
        }
        if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
            sock.read_shutdown |= read;
            sock.write_shutdown |= write;
        }
        Ok(())
    })())
}

extern "win64" fn setsockopt(s: u64, level: i32, name: i32, val: *const u8, len: i32) -> i32 {
    if traced() {
        os_std::eprintln!("winrun: setsockopt {s:#x} level {level:#x}, option {name:#x}, len {len}");
    }
    result((|| {
        let (fd, _) = fd(s)?;
        let int = || -> Result<u32, u32> {
            if val.is_null() || len < 4 {
                return Err(WSAEFAULT);
            }
            // SAFETY: the caller's 4-byte option value.
            Ok(unsafe { core::ptr::read_unaligned(val as *const u32) })
        };
        match (level, name) {
            (SOL_SOCKET, SO_RCVTIMEO) => {
                let ms = int()?;
                os_sys::sock_ctl(fd, net::ctl::RECV_TIMEOUT, ms as u64 * 1_000_000).map_err(wsa)?;
                if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
                    sock.recv_timeout = ms;
                }
                Ok(())
            }
            (SOL_SOCKET, SO_SNDTIMEO) => {
                let ms = int()?;
                os_sys::sock_ctl(fd, net::ctl::SEND_TIMEOUT, ms as u64 * 1_000_000).map_err(wsa)?;
                if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
                    sock.send_timeout = ms;
                }
                Ok(())
            }
            (SOL_SOCKET, SO_BROADCAST) => {
                os_sys::sock_ctl(fd, net::ctl::BROADCAST, (int()? != 0) as u64).map(|_| ()).map_err(wsa)
            }
            (SOL_SOCKET, SO_REUSEADDR) => {
                let on = int()? != 0;
                if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
                    sock.reuseaddr = on;
                }
                Ok(())
            }
            (SOL_SOCKET, SO_KEEPALIVE) => {
                let on = int()? != 0;
                if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
                    sock.keepalive = on;
                }
                Ok(())
            }
            (IPPROTO_TCP, TCP_NODELAY) => int().map(|_| ()),
            // The value is the listening SOCKET. The accepted socket takes
            // its buffer sizes, as accept gives them; the kernel endpoint is
            // complete already, so getsockname and getpeername work on it.
            (SOL_SOCKET, SO_UPDATE_ACCEPT_CONTEXT) => {
                if val.is_null() || len < 8 {
                    return Err(WSAEFAULT);
                }
                // SAFETY: the caller's SOCKET value.
                let l = unsafe { core::ptr::read_unaligned(val as *const u64) };
                let listener = socket_state(l)?;
                if !listener.stream {
                    return Err(WSAEINVAL);
                }
                let mut sockets = SOCKETS.lock().unwrap();
                let sock = sockets.get_mut(&s).ok_or(WSAENOTSOCK)?;
                sock.rcvbuf = listener.rcvbuf;
                sock.sndbuf = listener.sndbuf;
                Ok(())
            }
            // Takes no value: a socket ConnectEx connected already works
            // with every call.
            (SOL_SOCKET, SO_UPDATE_CONNECT_CONTEXT) => {
                let mut peer = SockAddr::default();
                os_sys::sock_ctl(fd, net::ctl::PEER_ADDR, &mut peer as *mut SockAddr as u64).map(|_| ()).map_err(|_| WSAENOTCONN)
            }
            (IPPROTO_IPV6, IPV6_V6ONLY) => {
                let on = int()? != 0;
                let mut sockets = SOCKETS.lock().unwrap();
                let sock = sockets.get_mut(&s).ok_or(WSAENOTSOCK)?;
                if !sock.v6 {
                    return Err(WSAENOPROTOOPT);
                }
                sock.v6only = on;
                Ok(())
            }
            (SOL_SOCKET, SO_RCVBUF) | (SOL_SOCKET, SO_SNDBUF) => {
                let size = int()?;
                let mut sockets = SOCKETS.lock().unwrap();
                let sock = sockets.get_mut(&s).ok_or(WSAENOTSOCK)?;
                if name == SO_RCVBUF { sock.rcvbuf = Some(size) } else { sock.sndbuf = Some(size) }
                Ok(())
            }
            // Linger 0 closes abortively, which the kernel can do; a positive
            // linger only records the value (close never leaves data behind).
            (SOL_SOCKET, SO_LINGER) => {
                if val.is_null() || len < 4 {
                    return Err(WSAEFAULT);
                }
                // SAFETY: the caller's struct linger { u16 l_onoff, l_linger }.
                let (onoff, linger) = unsafe {
                    (core::ptr::read_unaligned(val as *const u16), core::ptr::read_unaligned((val as *const u16).add(1)))
                };
                let abortive = onoff != 0 && linger == 0;
                os_sys::sock_ctl(fd, net::ctl::RESET_ON_CLOSE, abortive as u64).map_err(wsa)?;
                if let Some(sock) = SOCKETS.lock().unwrap().get_mut(&s) {
                    sock.linger = if onoff != 0 { linger } else { 0 };
                }
                Ok(())
            }
            _ => Err(WSAENOPROTOOPT),
        }
    })())
}

/// The options OSyx can report: the socket type, the pending connect error
/// (a failed non-blocking connect is otherwise only visible on the next
/// call) and the fixed behaviours setsockopt accepts.
extern "win64" fn getsockopt(s: u64, level: i32, name: i32, val: *mut u8, len: *mut i32) -> i32 {
    result((|| {
        let st = socket_state(s)?;
        let stream = st.stream;
        if (level, name) == (SOL_SOCKET, SO_LINGER) {
            // SAFETY: the caller's value buffer of `*len` bytes.
            unsafe {
                if val.is_null() || len.is_null() || *len < 4 {
                    return Err(WSAEFAULT);
                }
                core::ptr::write_unaligned(val as *mut u16, (st.linger != 0) as u16);
                core::ptr::write_unaligned((val as *mut u16).add(1), st.linger);
                *len = 4;
            }
            return Ok(());
        }
        let v: u32 = match (level, name) {
            (SOL_SOCKET, SO_TYPE) => {
                if stream {
                    SOCK_STREAM as u32
                } else {
                    SOCK_DGRAM as u32
                }
            }
            // A failed non-blocking connect reports its failure through
            // SO_ERROR, which also clears it.
            (SOL_SOCKET, SO_ERROR) => {
                let code = os_sys::sock_ctl(st.fd, net::ctl::ERROR, 0).unwrap_or(0);
                Errno::from_raw(code).map_or(0, wsa)
            }
            (SOL_SOCKET, SO_REUSEADDR) => st.reuseaddr as u32,
            (SOL_SOCKET, SO_KEEPALIVE) => st.keepalive as u32,
            (SOL_SOCKET, SO_RCVTIMEO) => st.recv_timeout,
            (SOL_SOCKET, SO_SNDTIMEO) => st.send_timeout,
            // The stack's buffer sizes unless the program set its own.
            (SOL_SOCKET, SO_RCVBUF) => st.rcvbuf.unwrap_or(RECV_BUFFER_SIZE),
            (SOL_SOCKET, SO_SNDBUF) => st.sndbuf.unwrap_or(SEND_BACKLOG),
            (IPPROTO_TCP, TCP_NODELAY) if stream => 1,
            (IPPROTO_IPV6, IPV6_V6ONLY) if st.v6 => st.v6only as u32,
            _ => return Err(WSAENOPROTOOPT),
        };
        // SAFETY: the caller's value buffer of `*len` bytes.
        unsafe {
            if val.is_null() || len.is_null() || *len < 4 {
                return Err(WSAEFAULT);
            }
            core::ptr::write_unaligned(val as *mut u32, v);
            *len = 4;
        }
        Ok(())
    })())
}

extern "win64" fn ioctlsocket(s: u64, cmd: u32, arg: *mut u32) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        if arg.is_null() {
            return Err(WSAEFAULT);
        }
        match cmd {
            FIONBIO => {
                // SAFETY: the caller's argument.
                let on = unsafe { *arg } != 0;
                os_sys::sock_ctl(fd, net::ctl::NONBLOCK, on as u64).map_err(wsa)?;
                let mut socks = SOCKETS.lock().unwrap();
                if !socks.contains_key(&s) {
                    return Err(WSAENOTSOCK);
                }
                set_nonblocking(&mut socks, fd, on);
            }
            FIONREAD => {
                let n = os_sys::sock_ctl(fd, net::ctl::AVAILABLE, 0).map_err(wsa)?;
                // SAFETY: the caller's 4-byte out argument.
                unsafe { core::ptr::write_unaligned(arg, n as u32) };
            }
            _ => {
                if traced() {
                    os_std::eprintln!("winrun: ioctlsocket {s:#x} unsupported command {cmd:#x}");
                }
                return Err(WSAEINVAL);
            }
        }
        Ok(())
    })())
}

extern "win64" fn getsockname(s: u64, addr: *mut u8, len: *mut i32) -> i32 {
    sock_name(s, net::ctl::LOCAL_ADDR, addr, len)
}

extern "win64" fn getpeername(s: u64, addr: *mut u8, len: *mut i32) -> i32 {
    sock_name(s, net::ctl::PEER_ADDR, addr, len)
}

fn sock_name(s: u64, op: u64, addr: *mut u8, len: *mut i32) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        if addr.is_null() || len.is_null() || unsafe { *len } < addr_size(s) {
            return Err(WSAEFAULT);
        }
        let mut a = SockAddr::default();
        os_sys::sock_ctl(fd, op, &mut a as *mut SockAddr as u64).map_err(wsa)?;
        write_addr(&a, addr, len, s);
        Ok(())
    })())
}

extern "win64" fn htons(v: u16) -> u16 {
    v.to_be()
}

extern "win64" fn ntohs(v: u16) -> u16 {
    u16::from_be(v)
}

extern "win64" fn htonl(v: u32) -> u32 {
    v.to_be()
}

extern "win64" fn ntohl(v: u32) -> u32 {
    u32::from_be(v)
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for o in out.iter_mut() {
        *o = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// The address in network byte order (as stored in memory), or INADDR_NONE.
extern "win64" fn inet_addr(s: *const u8) -> u32 {
    // SAFETY: a NUL-terminated string.
    match unsafe { crate::path::ansi_to_string(s) }.and_then(|s| parse_ipv4(&s)) {
        Some(a) => u32::from_le_bytes(a),
        None => u32::MAX,
    }
}

/// Per-thread result buffers (hostent, inet_ntoa), as Windows keeps them.
static BUFFERS: Mutex<BTreeMap<(u64, u8), u64>> = Mutex::new(BTreeMap::new());

fn thread_buffer(kind: u8, size: usize) -> u64 {
    let key = (os_sys::gettid(), kind);
    let mut b = BUFFERS.lock().unwrap();
    *b.entry(key).or_insert_with(|| crate::mem::arena_alloc(size, 16))
}

extern "win64" fn inet_ntoa(a: u32) -> u64 {
    let s = alloc::format!("{}\0", core::net::Ipv4Addr::from(a.to_le_bytes()));
    let p = thread_buffer(1, 16);
    // SAFETY: the 16-byte buffer holds "255.255.255.255\0".
    unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), p as *mut u8, s.len()) };
    p
}

fn resolve(name: &str) -> Result<Vec<[u8; 4]>, u32> {
    if let Some(a) = parse_ipv4(name) {
        return Ok(alloc::vec![a]);
    }
    let mut out = [[0u8; 4]; 8];
    let _blocked = crate::stall::waiting("resolve", name.len() as u64, 0);
    let n = os_sys::resolve(name, &mut out).map_err(wsa)?;
    Ok(out[..n].to_vec())
}

/// hostent (x64): h_name, h_aliases, h_addrtype (i16), h_length (i16),
/// h_addr_list — in a per-thread buffer with up to 8 addresses.
extern "win64" fn gethostbyname(name: *const u8) -> u64 {
    if !started() {
        return err(WSANOTINITIALISED, 0);
    }
    // SAFETY: a NUL-terminated name.
    let Some(n) = (unsafe { crate::path::ansi_to_string(name) }) else { return err(WSAEFAULT, 0) };
    let addrs = match resolve(&n) {
        Ok(a) if !a.is_empty() => a,
        Ok(_) => return err(WSAHOST_NOT_FOUND, 0),
        Err(e) => return err(e, 0),
    };
    hostent(&n, &addrs)
}

/// Fills this thread's hostent buffer.
fn hostent(n: &str, addrs: &[[u8; 4]]) -> u64 {
    let buf = thread_buffer(0, 512);
    let name_bytes: Vec<u8> = n.bytes().take(255).chain(core::iter::once(0)).collect();
    // SAFETY: a 512-byte buffer: hostent (32) + 9 pointers (72) + 8
    // addresses (32) + aliases terminator (8) + the name (<= 256).
    unsafe {
        let list = buf + 32;
        let data = list + 72;
        let aliases = data + 32;
        let name_p = aliases + 8;
        for (i, a) in addrs.iter().take(8).enumerate() {
            core::ptr::copy_nonoverlapping(a.as_ptr(), (data + i as u64 * 4) as *mut u8, 4);
            teb::put(list, i * 8, data + i as u64 * 4);
        }
        teb::put(list, addrs.len().min(8) * 8, 0u64);
        teb::put(aliases, 0, 0u64);
        core::ptr::copy_nonoverlapping(name_bytes.as_ptr(), name_p as *mut u8, name_bytes.len());
        teb::put(buf, 0, name_p);
        teb::put(buf, 8, aliases);
        teb::put(buf, 16, AF_INET as i16);
        teb::put(buf, 18, 4i16);
        teb::put(buf, 24, list);
    }
    buf
}

fn host_name() -> String {
    os_std::fs::read_to_string("/etc/hostname").map(|s| String::from(s.trim())).unwrap_or_else(|_| String::from("osyx"))
}

/// Loopback addresses name this machine, as on Windows. OSyx's resolver
/// has no reverse (PTR) lookup, so any other address is not found.
extern "win64" fn gethostbyaddr(addr: *const u8, len: i32, kind: i32) -> u64 {
    if !started() {
        return err(WSANOTINITIALISED, 0);
    }
    if kind != AF_INET as i32 {
        return err(WSAEAFNOSUPPORT, 0);
    }
    if addr.is_null() || len < 4 {
        return err(WSAEFAULT, 0);
    }
    let mut a = [0u8; 4];
    // SAFETY: the caller's 4-byte address.
    unsafe { core::ptr::copy_nonoverlapping(addr, a.as_mut_ptr(), 4) };
    if a[0] != 127 {
        return err(WSAHOST_NOT_FOUND, 0);
    }
    hostent(&host_name(), &[a])
}

/// The protocols of Windows' default etc\protocol that OSyx implements.
const PROTOCOLS: [(&str, i16); 4] = [("ip", 0), ("icmp", 1), ("tcp", 6), ("udp", 17)];

/// Common named services of Windows' default etc\services.
const SERVICES: [(&str, u16); 9] = [
    ("ftp", 21),
    ("ssh", 22),
    ("smtp", 25),
    ("domain", 53),
    ("http", 80),
    ("pop3", 110),
    ("ntp", 123),
    ("imap", 143),
    ("https", 443),
];

/// The port a decimal string or a known service name stands for.
fn service_port(service: &str) -> Option<u16> {
    match service.parse() {
        Ok(p) => Some(p),
        Err(_) => SERVICES.iter().find(|(n, _)| n.eq_ignore_ascii_case(service)).map(|&(_, p)| p),
    }
}

/// protoent (x64): p_name, p_aliases, p_proto (i16), in a per-thread buffer.
fn protoent(name: &str, proto: i16) -> u64 {
    let buf = thread_buffer(2, 64);
    // SAFETY: a 64-byte buffer: protoent (24) + aliases terminator (8) +
    // the NUL-terminated name (<= 32).
    unsafe {
        let aliases = buf + 24;
        let name_p = aliases + 8;
        teb::put(aliases, 0, 0u64);
        core::ptr::write_bytes(name_p as *mut u8, 0, 32);
        core::ptr::copy_nonoverlapping(name.as_ptr(), name_p as *mut u8, name.len());
        teb::put(buf, 0, name_p);
        teb::put(buf, 8, aliases);
        teb::put(buf, 16, proto);
    }
    buf
}

extern "win64" fn getprotobyname(name: *const u8) -> u64 {
    if !started() {
        return err(WSANOTINITIALISED, 0);
    }
    // SAFETY: a NUL-terminated name.
    let Some(n) = (unsafe { crate::path::ansi_to_string(name) }) else { return err(WSAEFAULT, 0) };
    match PROTOCOLS.iter().find(|(p, _)| p.eq_ignore_ascii_case(&n)) {
        Some(&(p, v)) => protoent(p, v),
        None => err(WSANO_DATA, 0),
    }
}

extern "win64" fn getprotobynumber(number: i32) -> u64 {
    if !started() {
        return err(WSANOTINITIALISED, 0);
    }
    match PROTOCOLS.iter().find(|(_, v)| *v as i32 == number) {
        Some(&(p, v)) => protoent(p, v),
        None => err(WSANO_DATA, 0),
    }
}

extern "win64" fn gethostname(buf: *mut u8, len: i32) -> i32 {
    let name = host_name();
    // A signed length: casting a negative one would look large and let the
    // copy run past a tiny caller buffer.
    if buf.is_null() || len <= 0 || (len as usize) < name.len() + 1 {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    // SAFETY: the caller's buffer holds the name and its NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(name.as_ptr(), buf, name.len());
        *buf.add(name.len()) = 0;
    }
    0
}

/// ADDRINFOA (48 bytes) + sockaddr_in (sockaddr_in6) per address, one heap
/// block each; freed by freeaddrinfo. AF_INET6 gives this host's IPv6
/// addresses (loopback and wildcard), numeric IPv6 addresses, and IPv4
/// addresses mapped when AI_V4MAPPED asks; OSyx resolves no IPv6 names.
pub(crate) extern "win64" fn getaddrinfo(node: *const u8, service: *const u8, hints: *const u8, res: *mut u64) -> i32 {
    if !started() {
        return WSANOTINITIALISED as i32;
    }
    if res.is_null() {
        return WSAEFAULT as i32;
    }
    // SAFETY: NULL or NUL-terminated strings.
    let (n, svc) = unsafe { (crate::path::ansi_to_string(node), crate::path::ansi_to_string(service)) };
    let (mut flags, mut family, mut socktype, mut proto) = (0i32, 0i32, 0i32, 0i32);
    if !hints.is_null() {
        // SAFETY: the caller's ADDRINFOA hints.
        unsafe {
            flags = teb::get(hints as u64, 0);
            family = teb::get(hints as u64, 4);
            if family != 0 && family != AF_INET as i32 && family != AF_INET6 {
                return WSAEAFNOSUPPORT as i32;
            }
            socktype = teb::get(hints as u64, 8);
            proto = teb::get(hints as u64, 12);
        }
    }
    if flags & AI_NUMERICSERV != 0 && svc.as_deref().is_some_and(|s| s.parse::<u16>().is_err()) {
        // EAI_NONAME: the service had to be a number.
        return WSAHOST_NOT_FOUND as i32;
    }
    let port: u16 = match svc.as_deref() {
        None => 0,
        Some(s) => match service_port(s) {
            Some(p) => p,
            // EAI_SERVICE: the name is known to be a service, it just is not one.
            None => return WSATYPE_NOT_FOUND as i32,
        },
    };
    // No type means the common TCP stream; the protocol follows from it.
    if socktype == 0 {
        socktype = SOCK_STREAM;
    }
    if proto == 0 {
        proto = if socktype == SOCK_DGRAM { IPPROTO_UDP } else { IPPROTO_TCP };
    }
    // A numeric IPv6 address, for AF_INET6 or no family.
    let numeric6 = n.as_deref().and_then(|s| s.parse::<core::net::Ipv6Addr>().ok()).filter(|_| family != AF_INET as i32);
    // Addresses: IPv4, or IPv6 (16 bytes) for AF_INET6 and numeric IPv6.
    let mut addrs: Vec<([u8; 4], Option<[u8; 16]>)> = Vec::new();
    if let Some(a) = numeric6 {
        addrs.push(([0; 4], Some(a.octets())));
    } else if family == AF_INET6 {
        let local = n.as_deref().is_none_or(|s| s.eq_ignore_ascii_case("localhost") || s.eq_ignore_ascii_case(&host_name()));
        if local && flags & AI_NUMERICHOST == 0 {
            // This host's IPv6 addresses: loopback, or the wildcard a
            // server binds (AI_PASSIVE with no name).
            let a = if n.is_none() && flags & AI_PASSIVE != 0 { [0; 4] } else { [127, 0, 0, 1] };
            addrs.push(([0; 4], Some(v4_to_v6(a))));
        } else if flags & AI_V4MAPPED != 0 {
            let v4 = match n.as_deref() {
                Some(s) if flags & AI_NUMERICHOST != 0 => parse_ipv4(s).map(|a| alloc::vec![a]).ok_or(WSAHOST_NOT_FOUND),
                Some(s) => resolve(s),
                None => Ok(alloc::vec![[127, 0, 0, 1]]),
            };
            match v4 {
                Ok(v) => addrs.extend(v.into_iter().map(|a| ([0; 4], Some(v4_to_v6(a))))),
                Err(e) => return e as i32,
            }
        } else if n.as_deref().is_some_and(|s| parse_ipv4(s).is_some()) || flags & AI_NUMERICHOST != 0 {
            return WSAHOST_NOT_FOUND as i32;
        } else {
            // A name with no IPv6 address OSyx can give.
            return WSANO_DATA as i32;
        }
    } else {
        let v4 = match n {
            // AI_NUMERICHOST: never fall back to a name lookup.
            Some(n) if flags & AI_NUMERICHOST != 0 => match parse_ipv4(&n) {
                Some(a) => alloc::vec![a],
                None => return WSAHOST_NOT_FOUND as i32,
            },
            Some(n) => match resolve(&n) {
                Ok(a) => a,
                Err(e) => return e as i32,
            },
            // AI_PASSIVE asks for the wildcard address a server can bind.
            None if flags & AI_PASSIVE != 0 => alloc::vec![[0, 0, 0, 0]],
            None => alloc::vec![[127, 0, 0, 1]],
        };
        addrs.extend(v4.into_iter().map(|a| (a, None)));
    }
    let mut head = 0u64;
    for (a, six) in addrs.iter().rev() {
        let size: u64 = if six.is_some() { 28 } else { 16 };
        let Some(block) = crate::kernel32::heap_alloc(48 + size as usize, true) else { return WSAENOBUFS as i32 };
        let sa = block + 48;
        match six {
            // SAFETY: the block's 28 zeroed bytes after the ADDRINFOA.
            Some(b) => unsafe {
                teb::put(sa, 0, AF_INET6 as u16);
                teb::put(sa, 2, port.to_be());
                core::ptr::copy_nonoverlapping(b.as_ptr(), (sa + 8) as *mut u8, 16);
            },
            None => put_addr(&SockAddr::new(*a, port), sa as *mut u8, core::ptr::null_mut(), false),
        }
        // SAFETY: a fresh zeroed block of 48 + `size` bytes.
        unsafe {
            teb::put(block, 4, if six.is_some() { AF_INET6 } else { AF_INET as i32 });
            teb::put(block, 8, socktype);
            teb::put(block, 12, proto);
            teb::put(block, 16, size);
            teb::put(block, 32, sa);
            teb::put(block, 40, head);
        }
        head = block;
    }
    if head == 0 {
        return WSAHOST_NOT_FOUND as i32;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *res = head };
    0
}

const NI_NUMERICHOST: i32 = 2;
const NI_NAMEREQD: i32 = 4;
const NI_NUMERICSERV: i32 = 8;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// The host and service names getnameinfo gives for a sockaddr_in or
/// sockaddr_in6. A loopback address names this machine; other addresses
/// have no reverse lookup in OSyx's resolver and stay numeric, unless
/// NI_NAMEREQD asks for a name. The service is a known name or the
/// decimal port.
fn name_info(sa: *const u8, salen: i32, flags: i32) -> Result<(String, String), u32> {
    if !started() {
        return Err(WSANOTINITIALISED);
    }
    if sa.is_null() || salen < 16 {
        return Err(WSAEFAULT);
    }
    // SAFETY: the caller's sockaddr of at least 16 bytes.
    let (family, port) = unsafe { (core::ptr::read_unaligned(sa as *const u16), u16::from_be(core::ptr::read_unaligned(sa.add(2) as *const u16))) };
    let (numeric, loopback) = if family == AF_INET6 as u16 {
        if salen < 28 {
            return Err(WSAEFAULT);
        }
        let mut b = [0u8; 16];
        // SAFETY: as above, 28 bytes.
        unsafe { core::ptr::copy_nonoverlapping(sa.add(8), b.as_mut_ptr(), 16) };
        let ip = core::net::Ipv6Addr::from(b);
        (alloc::format!("{ip}"), ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))
    } else if family == AF_INET {
        // SAFETY: as above.
        let b = unsafe { core::slice::from_raw_parts(sa.add(4), 4) };
        (alloc::format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]), b[0] == 127)
    } else {
        return Err(WSAEAFNOSUPPORT);
    };
    let a = SockAddr::new([0; 4], port);
    let host = if flags & NI_NUMERICHOST != 0 {
        numeric
    } else if loopback {
        host_name()
    } else if flags & NI_NAMEREQD != 0 {
        return Err(WSAHOST_NOT_FOUND);
    } else {
        numeric
    };
    let named = SERVICES.iter().find(|&&(_, p)| p == a.port).filter(|_| flags & NI_NUMERICSERV == 0);
    let serv = match named {
        Some(&(n, _)) => String::from(n),
        None => alloc::format!("{}", a.port),
    };
    Ok((host, serv))
}

/// Copies `s` into a buffer of `cap` units. As on Windows, a name that
/// does not fit is cut to the buffer (without a terminator) and fails
/// with ERROR_INSUFFICIENT_BUFFER.
fn put_name<T: Copy + Default>(units: &[T], dst: *mut T, cap: u32) -> bool {
    if dst.is_null() || cap == 0 {
        return true;
    }
    let n = units.len().min(cap as usize);
    // SAFETY: the caller's buffer of `cap` units.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), dst, n);
        if units.len() < cap as usize {
            *dst.add(units.len()) = T::default();
            return true;
        }
    }
    false
}

/// Early argument errors return the code without setting the last error;
/// name and buffer failures set it too, matching Windows.
fn name_info_result(r: Result<bool, u32>) -> i32 {
    match r {
        Ok(true) => 0,
        Ok(false) => err(ERROR_INSUFFICIENT_BUFFER, ERROR_INSUFFICIENT_BUFFER as i32),
        Err(WSAHOST_NOT_FOUND) => err(WSAHOST_NOT_FOUND, WSAHOST_NOT_FOUND as i32),
        Err(e) => e as i32,
    }
}

pub(crate) extern "win64" fn getnameinfo(
    sa: *const u8,
    salen: i32,
    host: *mut u8,
    hostlen: u32,
    serv: *mut u8,
    servlen: u32,
    flags: i32,
) -> i32 {
    name_info_result(name_info(sa, salen, flags).map(|(h, s)| {
        let h_ok = put_name(h.as_bytes(), host, hostlen);
        put_name(s.as_bytes(), serv, servlen) && h_ok
    }))
}

extern "win64" fn GetNameInfoW(
    sa: *const u8,
    salen: i32,
    host: *mut u16,
    hostlen: u32,
    serv: *mut u16,
    servlen: u32,
    flags: i32,
) -> i32 {
    name_info_result(name_info(sa, salen, flags).map(|(h, s)| {
        let (h, s): (Vec<u16>, Vec<u16>) = (h.encode_utf16().collect(), s.encode_utf16().collect());
        let h_ok = put_name(&h, host, hostlen);
        put_name(&s, serv, servlen) && h_ok
    }))
}

pub(crate) extern "win64" fn freeaddrinfo(mut p: u64) {
    while p != 0 {
        // SAFETY: a block made by getaddrinfo (ai_next at offset 40).
        let next: u64 = unsafe { teb::get(p, 40) };
        crate::kernel32::heap_free(p);
        p = next;
    }
}

/// WSASocketW: a plain socket. Overlapped I/O is not offered (calls block
/// or fail with WSAEWOULDBLOCK), so WSA_FLAG_OVERLAPPED changes nothing;
/// a specific provider (WSAPROTOCOL_INFOW) is not supported.
pub(crate) extern "win64" fn WSASocketW(af: i32, kind: i32, proto: i32, info: *const u8, group: u32, _flags: u32) -> u64 {
    if traced() {
        os_std::eprintln!("winrun: WSASocket family {af}, kind {kind}, protocol {proto}, provider {}, group {group}, flags {_flags:#x}", !info.is_null());
    }
    if group != 0 {
        return err(WSAEINVAL, INVALID_SOCKET);
    }
    if !info.is_null() {
        if !started() {
            return err(WSANOTINITIALISED, INVALID_SOCKET);
        }
        return service::socket_from_info(info);
    }
    socket(af, kind, proto)
}

const FIONREAD: u32 = 0x4004_667F;

/// WSAIoctl: FIONBIO, FIONREAD and the extension functions
/// (SIO_GET_EXTENSION_FUNCTION_POINTER: AcceptEx, ConnectEx, ...), which
/// complete synchronously (mswsock.rs).
#[allow(clippy::too_many_arguments)]
extern "win64" fn WSAIoctl(
    s: u64,
    code: u32,
    input: *const u8,
    in_len: u32,
    output: *mut u8,
    out_len: u32,
    returned: *mut u32,
    overlapped: *mut u8,
    completion: u64,
) -> i32 {
    result((|| {
        let (fd, _) = fd(s)?;
        if !overlapped.is_null() || completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        let written = match code {
            FIONBIO => {
                if input.is_null() || in_len < 4 {
                    return Err(WSAEFAULT);
                }
                // SAFETY: the caller's 4-byte input.
                let mut v = unsafe { core::ptr::read_unaligned(input as *const u32) };
                if ioctlsocket(s, FIONBIO, &mut v) != 0 {
                    return Err(teb::last_error());
                }
                0
            }
            FIONREAD => {
                if output.is_null() || out_len < 4 {
                    return Err(WSAEFAULT);
                }
                let n = os_sys::sock_ctl(fd, net::ctl::AVAILABLE, 0).map_err(wsa)?;
                // SAFETY: the caller's 4-byte output buffer.
                unsafe { core::ptr::write_unaligned(output as *mut u32, n as u32) };
                4
            }
            SIO_GET_EXTENSION_FUNCTION_POINTER => {
                if input.is_null() || in_len < 16 || output.is_null() {
                    return Err(WSAEFAULT);
                }
                // SAFETY: the caller's 16-byte GUID.
                let guid: [u8; 16] = unsafe { core::ptr::read_unaligned(input as *const [u8; 16]) };
                let (name, host) = crate::apiextra::extension_function(&guid).ok_or(WSAEINVAL)?;
                // A 32-bit caller gets a 4-byte pointer to a bridge stub.
                if crate::wow64::is_active() {
                    let stub = crate::wow64::import_active(name, host).ok_or(WSAEINVAL)?;
                    if out_len < 4 {
                        return Err(WSAEFAULT);
                    }
                    // SAFETY: the caller's 4-byte output.
                    unsafe { core::ptr::write_unaligned(output as *mut u32, stub) };
                    4
                } else {
                    if out_len < 8 {
                        return Err(WSAEFAULT);
                    }
                    // SAFETY: the caller's 8-byte output.
                    unsafe { core::ptr::write_unaligned(output as *mut u64, host) };
                    8
                }
            }
            // struct tcp_keepalive { onoff, keepalivetime, keepaliveinterval }:
            // the switch is kept like SO_KEEPALIVE's (the stack sends no
            // keep-alive probes, so the timings have nothing to drive).
            SIO_KEEPALIVE_VALS => {
                if input.is_null() || in_len < 12 {
                    return Err(WSAEFAULT);
                }
                // SAFETY: the caller's 12-byte input.
                let on = unsafe { core::ptr::read_unaligned(input as *const u32) } != 0;
                SOCKETS.lock().unwrap().get_mut(&s).ok_or(WSAENOTSOCK)?.keepalive = on;
                0
            }
            // Loopback connections already stay inside the stack: the fast
            // path a program asks for is the only path there is.
            SIO_LOOPBACK_FAST_PATH => {
                if input.is_null() || in_len < 4 {
                    return Err(WSAEFAULT);
                }
                0
            }
            // How much to keep queued for sending: the stack's send buffer.
            // Only a connected stream socket has a send backlog.
            SIO_IDEAL_SEND_BACKLOG_QUERY => {
                if output.is_null() || out_len < 4 {
                    return Err(WSAEFAULT);
                }
                if !socket_state(s)?.stream {
                    return Err(WSAEOPNOTSUPP);
                }
                let mut peer = SockAddr::default();
                os_sys::sock_ctl(fd, net::ctl::PEER_ADDR, &mut peer as *mut SockAddr as u64).map_err(|_| WSAENOTCONN)?;
                // SAFETY: the caller's 4-byte output buffer.
                unsafe { core::ptr::write_unaligned(output as *mut u32, SEND_BACKLOG) };
                4
            }
            _ => {
                if traced() {
                    os_std::eprintln!("winrun: WSAIoctl {s:#x} unsupported code {code:#x}");
                }
                return Err(WSAEINVAL);
            }
        };
        if !returned.is_null() {
            // SAFETY: the caller's out count.
            unsafe { *returned = written };
        }
        Ok(())
    })())
}

/// x64 WSABUF entries: ULONG len, four bytes of padding, CHAR *buf.
fn wsa_bufs(bufs: *const u8, count: u32) -> Result<Vec<(u32, u64)>, u32> {
    if count > 0 && bufs.is_null() {
        return Err(WSAEFAULT);
    }
    let base = bufs as u64;
    let mut parts = Vec::with_capacity(count as usize);
    for i in 0..count as u64 {
        let off = (i * 16) as usize;
        // SAFETY: the caller's WSABUF array.
        let (len, p) = unsafe { (teb::get::<u32>(base, off), teb::get::<u64>(base, off + 8)) };
        if len > 0 && p == 0 {
            return Err(WSAEFAULT);
        }
        parts.push((len, p));
    }
    Ok(parts)
}

/// The WSABUFs read as one contiguous buffer.
fn wsa_gather(bufs: *const u8, count: u32) -> Result<Vec<u8>, u32> {
    let mut data = Vec::new();
    for (len, p) in wsa_bufs(bufs, count)? {
        // A zero-length entry may carry a NULL pointer.
        if len == 0 {
            continue;
        }
        // SAFETY: the caller's WSABUF of `len` bytes.
        data.extend_from_slice(unsafe { core::slice::from_raw_parts(p as *const u8, len as usize) });
    }
    Ok(data)
}

/// One receive scattered over the WSABUFs; returns the byte count, peer and
/// whether the datagram was longer than the buffers.
fn wsa_receive(fd: u64, bufs: *const u8, count: u32, fl: u64, stream: bool) -> Result<(usize, SockAddr, bool), u32> {
    let parts = wsa_bufs(bufs, count)?;
    let total: usize = parts.iter().map(|p| p.0 as usize).sum();
    let mut data = alloc::vec![0u8; total];
    let (n, from) = os_sys::sock_recv(fd, &mut data, fl).map_err(wsa)?;
    trace_data("WSARecv on kernel socket", fd, &data[..n]);
    let truncated = datagram_truncated(fd, stream, n)?;
    let mut done = 0;
    for (len, p) in parts {
        let take = (len as usize).min(n - done);
        if take == 0 {
            continue;
        }
        // SAFETY: the caller's WSABUF of `len` bytes.
        unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), p as *mut u8, take) };
        done += take;
    }
    Ok((n, from, truncated))
}

/// The result of a WSASend-style call: the count, and for an overlapped
/// call the completion (sends complete in the call; a failure that
/// happens at once queues no packet, as on Windows).
fn send_result(s: u64, r: Result<usize, u32>, sent: *mut u32, ov: *mut u8) -> i32 {
    match r {
        Ok(n) => {
            if !sent.is_null() {
                // SAFETY: the caller's out count.
                unsafe { *sent = n as u32 };
            }
            if !ov.is_null() {
                overlapped::finish_now(s, ov as u64, n);
            }
            0
        }
        Err(e) => err(e, SOCKET_ERROR),
    }
}

/// WSASend: gathers the WSABUFs into one stream write or datagram.
/// Completion routines are not supported; an OVERLAPPED completes in the
/// call (see overlapped.rs).
pub(crate) extern "win64" fn WSASend(s: u64, bufs: *const u8, count: u32, sent: *mut u32, f: u32, ov: *mut u8, completion: u64) -> i32 {
    let r = (|| {
        let (fd, _) = fd(s)?;
        check_shutdown(s, false)?;
        if f != 0 || completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        let data = wsa_gather(bufs, count)?;
        trace_data("WSASend", s, &data);
        if !ov.is_null() {
            return Ok(Err((fd, data)));
        }
        os_sys::sock_send(fd, &data, None, 0).map_err(wsa).map(Ok)
    })();
    match r {
        Ok(Err((fd, data))) => overlapped::send(s, fd, data, None, ov, sent),
        Ok(Ok(n)) => send_result(s, Ok(n), sent, core::ptr::null_mut()),
        Err(e) => send_result(s, Err(e), sent, core::ptr::null_mut()),
    }
}

/// WSASendTo: like WSASend, with the destination of a datagram.
#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn WSASendTo(
    s: u64,
    bufs: *const u8,
    count: u32,
    sent: *mut u32,
    f: u32,
    to: *const u8,
    tolen: i32,
    ov: *mut u8,
    completion: u64,
) -> i32 {
    let r = (|| {
        let (fd, _) = fd(s)?;
        check_shutdown(s, false)?;
        if f != 0 || completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        let to = if to.is_null() { None } else { Some(read_addr(to, tolen, s)?) };
        let data = wsa_gather(bufs, count)?;
        if !ov.is_null() {
            return Ok(Err((fd, data, to)));
        }
        os_sys::sock_send(fd, &data, to.as_ref(), 0).map_err(wsa).map(Ok)
    })();
    match r {
        Ok(Err((fd, data, to))) => overlapped::send(s, fd, data, to, ov, sent),
        Ok(Ok(n)) => send_result(s, Ok(n), sent, core::ptr::null_mut()),
        Err(e) => send_result(s, Err(e), sent, core::ptr::null_mut()),
    }
}

/// The received count and in/out flags of a WSARecv-style call.
fn recv_result(r: Result<(usize, *mut u32, bool), u32>, received: *mut u32) -> i32 {
    match r {
        Ok((n, f, truncated)) => {
            if !received.is_null() {
                // SAFETY: the caller's out count.
                unsafe { *received = n as u32 };
            }
            if !f.is_null() {
                // SAFETY: the caller's in/out flags; nothing partial to report.
                unsafe { *f = 0 };
            }
            if truncated {
                return err(WSAEMSGSIZE, SOCKET_ERROR);
            }
            0
        }
        Err(e) => err(e, SOCKET_ERROR),
    }
}

/// Whether the last receive truncated a datagram (Winsock WSAEMSGSIZE).
fn datagram_truncated(fd: u64, stream: bool, n: usize) -> Result<bool, u32> {
    if stream {
        return Ok(false);
    }
    let total = os_sys::sock_ctl(fd, net::ctl::LAST_MSG_LEN, 0).map_err(wsa)?;
    Ok(total as usize > n)
}

/// WSARecv: one receive scattered over the WSABUFs.
pub(crate) extern "win64" fn WSARecv(
    s: u64,
    bufs: *const u8,
    count: u32,
    received: *mut u32,
    f: *mut u32,
    overlapped: *mut u8,
    completion: u64,
) -> i32 {
    let r = (|| {
        let (fd, stream) = fd(s)?;
        check_shutdown(s, true)?;
        if completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        // SAFETY: the caller's in/out flags.
        let user_flags = if f.is_null() { 0 } else { unsafe { *f as i32 } };
        let fl = flags(user_flags)?;
        if !overlapped.is_null() {
            return Ok(Err(fl));
        }
        let (n, _, truncated) = wsa_receive(fd, bufs, count, fl, stream)?;
        Ok(Ok((n, f, truncated)))
    })();
    match r {
        Ok(Err(fl)) => overlapped::receive(s, bufs, count, fl, overlapped, core::ptr::null_mut(), core::ptr::null_mut()),
        Ok(Ok(v)) => recv_result(Ok(v), received),
        Err(e) => recv_result(Err(e), received),
    }
}

/// WSARecvFrom: like WSARecv, also returning the sender's address.
#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn WSARecvFrom(
    s: u64,
    bufs: *const u8,
    count: u32,
    received: *mut u32,
    f: *mut u32,
    from: *mut u8,
    fromlen: *mut i32,
    overlapped: *mut u8,
    completion: u64,
) -> i32 {
    let r = (|| {
        let (fd, stream) = fd(s)?;
        check_shutdown(s, true)?;
        if completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        if !from.is_null() && (fromlen.is_null() || unsafe { *fromlen } < addr_size(s)) {
            return Err(WSAEFAULT);
        }
        // SAFETY: the caller's in/out flags.
        let user_flags = if f.is_null() { 0 } else { unsafe { *f as i32 } };
        let fl = flags(user_flags)?;
        if !overlapped.is_null() {
            return Ok(Err(fl));
        }
        let (n, peer, truncated) = wsa_receive(fd, bufs, count, fl, stream)?;
        write_addr(&peer, from, fromlen, s);
        Ok(Ok((n, f, truncated)))
    })();
    match r {
        Ok(Err(fl)) => overlapped::receive(s, bufs, count, fl, overlapped, from, fromlen),
        Ok(Ok(v)) => recv_result(Ok(v), received),
        Err(e) => recv_result(Err(e), received),
    }
}

/// WSASendMsg: WSAMSG { name, namelen, lpBuffers, dwBufferCount, Control,
/// dwFlags } with the buffers gathered into one datagram or stream write.
/// Control data (ancillary messages) is not supported.
extern "win64" fn WSASendMsg(s: u64, msg: *const u8, f: u32, sent: *mut u32, overlapped: *mut u8, completion: u64) -> i32 {
    let r = (|| {
        let (fd, _) = fd(s)?;
        check_shutdown(s, false)?;
        if msg.is_null() {
            return Err(WSAEFAULT);
        }
        if f != 0 || !overlapped.is_null() || completion != 0 {
            return Err(WSAEOPNOTSUPP);
        }
        // SAFETY: the caller's WSAMSG (x64 layout: 56 bytes) and WSABUFs.
        let (to, bufs, count) = unsafe {
            let m = msg as u64;
            let name: u64 = teb::get(m, 0);
            let name_len: i32 = teb::get(m, 8);
            let bufs: u64 = teb::get(m, 16);
            let count: u32 = teb::get(m, 24);
            // Control is a WSABUF at 32; ancillary messages are not supported.
            if teb::get::<u32>(m, 32) != 0 {
                return Err(WSAEINVAL);
            }
            let to = if name == 0 { None } else { Some(read_addr(name as *const u8, name_len, s)?) };
            (to, bufs, count)
        };
        let data = wsa_gather(bufs as *const u8, count)?;
        os_sys::sock_send(fd, &data, to.as_ref(), 0).map_err(wsa)
    })();
    match r {
        Ok(n) => {
            if !sent.is_null() {
                // SAFETY: the caller's out count.
                unsafe { *sent = n as u32 };
            }
            0
        }
        Err(e) => err(e, SOCKET_ERROR),
    }
}

/// inet_pton: dotted-decimal IPv4 or textual IPv6 into network order.
extern "win64" fn inet_pton(af: i32, src: *const u8, dst: *mut u8) -> i32 {
    if src.is_null() || dst.is_null() {
        return err(WSAEFAULT, -1);
    }
    // SAFETY: a NUL-terminated string.
    let Some(text) = (unsafe { crate::path::ansi_to_string(src) }) else { return err(WSAEFAULT, -1) };
    let bytes: Vec<u8> = match af {
        a if a == AF_INET as i32 => match text.parse::<core::net::Ipv4Addr>() {
            Ok(a) => a.octets().to_vec(),
            Err(_) => return 0,
        },
        AF_INET6 => match text.parse::<core::net::Ipv6Addr>() {
            Ok(a) => a.octets().to_vec(),
            Err(_) => return 0,
        },
        _ => return err(WSAEAFNOSUPPORT, -1),
    };
    // SAFETY: the caller's in_addr / in6_addr.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len()) };
    1
}

/// inet_ntop: the RFC 5952 text of an IPv4 or IPv6 address.
extern "win64" fn inet_ntop(af: i32, src: *const u8, dst: *mut u8, size: usize) -> u64 {
    if src.is_null() || dst.is_null() {
        return err(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's in_addr (4 bytes) / in6_addr (16 bytes).
    let text = unsafe {
        match af {
            a if a == AF_INET as i32 => {
                let mut b = [0u8; 4];
                core::ptr::copy_nonoverlapping(src, b.as_mut_ptr(), 4);
                alloc::format!("{}", core::net::Ipv4Addr::from(b))
            }
            AF_INET6 => {
                let mut b = [0u8; 16];
                core::ptr::copy_nonoverlapping(src, b.as_mut_ptr(), 16);
                alloc::format!("{}", core::net::Ipv6Addr::from(b))
            }
            _ => return err(WSAEAFNOSUPPORT, 0),
        }
    };
    if size < text.len() + 1 {
        return err(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), dst, text.len());
        *dst.add(text.len()) = 0;
    }
    dst as u64
}

/// The catalog: one base provider each for TCP and UDP over IPv4, both
/// served by OSyx's own stack. No layered (LSP) providers exist.
const PROVIDER_ID: [u8; 16] = *b"OSyx-Winsock-TCP";
const PROTOCOL_INFO_SIZE: usize = 628;
const PROVIDER_PATH: &str = "%SystemRoot%\\system32\\mswsock.dll";

/// WSAPROTOCOL_INFOW of catalog entry `(socket type, protocol)`.
fn protocol_info(p: u64, kind: i32, proto: i32) {
    // XP1_GUARANTEED_DELIVERY | GUARANTEED_ORDER | GRACEFUL_CLOSE for TCP;
    // XP1_CONNECTIONLESS | MESSAGE_ORIENTED | SUPPORT_BROADCAST for UDP.
    let (flags, msg_size, name) = if kind == SOCK_STREAM {
        (0x26u32, 0u32, "OSyx TCP/IP [TCP/IP]")
    } else {
        (0x209, 65507, "OSyx TCP/IP [UDP/IP]")
    };
    // SAFETY: the caller's 628-byte WSAPROTOCOL_INFOW.
    unsafe {
        core::ptr::write_bytes(p as *mut u8, 0, PROTOCOL_INFO_SIZE);
        teb::put(p, 0, flags);
        core::ptr::copy_nonoverlapping(PROVIDER_ID.as_ptr(), (p + 20) as *mut u8, 16);
        teb::put(p, 36, 1000 + proto as u32);
        teb::put(p, 40, 1i32); // ProtocolChain.ChainLen: BASE_PROTOCOL
        teb::put(p, 72, 2i32); // iVersion
        teb::put(p, 76, AF_INET as i32);
        teb::put(p, 80, 16i32);
        teb::put(p, 84, 16i32);
        teb::put(p, 88, kind);
        teb::put(p, 92, proto);
        teb::put(p, 108, msg_size);
        for (i, c) in name.encode_utf16().enumerate() {
            teb::put(p, 116 + i * 2, c);
        }
    }
}

extern "win64" fn WSCEnumProtocols(filter: *const i32, buf: *mut u8, len: *mut u32, errno: *mut i32) -> i32 {
    let set_errno = |e: u32| {
        if !errno.is_null() {
            // SAFETY: the caller's error out-parameter.
            unsafe { *errno = e as i32 };
        }
    };
    let wanted = |proto: i32| -> bool {
        if filter.is_null() {
            return true;
        }
        // SAFETY: the caller's zero-terminated protocol list.
        unsafe { (0..).map(|i| *filter.add(i)).take_while(|&v| v != 0).any(|v| v == proto) }
    };
    let entries: Vec<(i32, i32)> =
        [(SOCK_STREAM, IPPROTO_TCP), (SOCK_DGRAM, 17)].into_iter().filter(|&(_, p)| wanted(p)).collect();
    if len.is_null() {
        set_errno(WSAEFAULT);
        return SOCKET_ERROR;
    }
    let need = (entries.len() * PROTOCOL_INFO_SIZE) as u32;
    // SAFETY: the caller's buffer length.
    unsafe {
        if buf.is_null() || *len < need {
            *len = need;
            set_errno(WSAENOBUFS);
            return SOCKET_ERROR;
        }
    }
    for (i, &(kind, proto)) in entries.iter().enumerate() {
        protocol_info(buf as u64 + (i * PROTOCOL_INFO_SIZE) as u64, kind, proto);
    }
    entries.len() as i32
}

/// Whether `id` points at OSyx's provider GUID.
fn our_provider(id: *const u8) -> bool {
    // SAFETY: the caller's GUID.
    !id.is_null() && unsafe { core::slice::from_raw_parts(id, 16) } == PROVIDER_ID
}

/// WSCGetProviderInfo: LSP categories are recorded only for layered
/// providers (WSCSetProviderInfo); for a base provider Windows reports
/// WSAEINVALIDPROVIDER, and so does OSyx, which has only base providers.
extern "win64" fn WSCGetProviderInfo(id: *const u8, _kind: i32, _info: *mut u8, _size: *mut usize, _flags: u32, errno: *mut i32) -> i32 {
    if !errno.is_null() {
        // SAFETY: the caller's error out-parameter.
        unsafe { *errno = if our_provider(id) { WSAEINVALIDPROVIDER } else { WSAEINVAL } as i32 };
    }
    SOCKET_ERROR
}

extern "win64" fn WSCGetProviderPath(id: *const u8, path: *mut u16, len: *mut i32, errno: *mut i32) -> i32 {
    let fail = |e: u32| {
        if !errno.is_null() {
            // SAFETY: the caller's error out-parameter.
            unsafe { *errno = e as i32 };
        }
        SOCKET_ERROR
    };
    if !our_provider(id) {
        return fail(WSAEINVAL);
    }
    let w: Vec<u16> = PROVIDER_PATH.encode_utf16().chain(core::iter::once(0)).collect();
    if len.is_null() {
        return fail(WSAEFAULT);
    }
    // SAFETY: the caller's length (in characters) and buffer.
    unsafe {
        if path.is_null() || (*len as usize) < w.len() {
            *len = w.len() as i32;
            return fail(WSAEFAULT);
        }
        core::ptr::copy_nonoverlapping(w.as_ptr(), path, w.len());
    }
    0
}

/// FD_ISSET: whether fd_set `set` holds `s`.
pub(crate) extern "win64" fn __WSAFDIsSet(s: u64, set: *const u8) -> i32 {
    if set.is_null() {
        return 0;
    }
    // SAFETY: the caller's fd_set { u32 count; SOCKET array[64] }.
    unsafe {
        let n: u32 = teb::get(set as u64, 0);
        (0..n.min(64) as usize).any(|i| teb::get::<u64>(set as u64, 8 + i * 8) == s) as i32
    }
}

/// select over fd_set { u32 count; SOCKET array[64] } for reading and
/// writing (the exception set is reported empty); timeval {sec, usec}.
pub(crate) extern "win64" fn select(_nfds: i32, rd: *mut u8, wr: *mut u8, ex: *mut u8, timeout: *const i32) -> i32 {
    trace_io(format_args!("select begin"));
    if rd.is_null() && wr.is_null() && ex.is_null() {
        return err(WSAEINVAL, SOCKET_ERROR);
    }
    let read_set = |p: *mut u8| -> Vec<u64> {
        if p.is_null() {
            return Vec::new();
        }
        // SAFETY: the caller's fd_set.
        unsafe {
            let n: u32 = teb::get(p as u64, 0);
            (0..n.min(64) as usize).map(|i| teb::get::<u64>(p as u64, 8 + i * 8)).collect()
        }
    };
    let (r, w) = (read_set(rd), read_set(wr));
    // The exception set is always reported empty (OSyx has no OOB data), but
    // an invalid handle in it is still an error, as on Windows.
    for s in read_set(ex) {
        if let Err(e) = fd(s) {
            return err(e, SOCKET_ERROR);
        }
    }
    let mut items = Vec::new();
    for (&s, ev) in
        r.iter().map(|s| (s, osyx_abi::ipc::WAIT_READABLE)).chain(w.iter().map(|s| (s, osyx_abi::ipc::WAIT_WRITABLE)))
    {
        match fd(s) {
            Ok((f, _)) => items.push((s, os_sys::ipc::WaitItem { handle: f, events: ev, revents: 0 })),
            Err(e) => return err(e, SOCKET_ERROR),
        }
    }
    let ns = if timeout.is_null() {
        u64::MAX
    } else {
        // SAFETY: the caller's timeval {sec, usec}.
        unsafe {
            let (sec, usec) = (*timeout, *timeout.add(1));
            if sec < 0 || usec < 0 || usec >= 1_000_000 {
                return err(WSAEINVAL, SOCKET_ERROR);
            }
            sec as u64 * 1_000_000_000 + usec as u64 * 1000
        }
    };
    let mut raw: Vec<os_sys::ipc::WaitItem> = items.iter().map(|(_, i)| *i).collect();
    let blocked = crate::stall::waiting("select", raw.len() as u64, 0);
    let waited = os_sys::wait_handles(&mut raw, ns);
    drop(blocked);
    if let Err(e) = waited {
        if e != Errno::TimedOut {
            return err(wsa(e), SOCKET_ERROR);
        }
    }
    trace_io(format_args!("select woke: {raw:?}"));
    let mut count = 0;
    let write_set = |p: *mut u8, list: &[u64]| {
        if p.is_null() {
            return;
        }
        // SAFETY: the caller's fd_set.
        unsafe {
            teb::put(p as u64, 0, list.len() as u32);
            for (i, s) in list.iter().enumerate() {
                teb::put(p as u64, 8 + i * 8, *s);
            }
        }
    };
    if r.is_empty() && w.is_empty() {
        // No descriptor can become ready: Windows simply waits out the
        // timeout (forever when there is none). The kernel's wait rejects an
        // empty handle list, so sleep here instead.
        if timeout.is_null() {
            loop {
                os_sys::sleep(1_000_000_000);
            }
        }
        os_sys::sleep(ns);
        write_set(rd, &[]);
        write_set(wr, &[]);
        write_set(ex, &[]);
        return 0;
    }
    let ready = |ev: u32| -> Vec<u64> {
        items
            .iter()
            .zip(&raw)
            .filter(|((_, i), r)| i.events == ev && r.revents & (ev | osyx_abi::ipc::WAIT_HANGUP) != 0)
            .map(|((s, _), _)| *s)
            .collect()
    };
    let (rr, wr2) = (ready(osyx_abi::ipc::WAIT_READABLE), ready(osyx_abi::ipc::WAIT_WRITABLE));
    count += rr.len() + wr2.len();
    write_set(rd, &rr);
    write_set(wr, &wr2);
    write_set(ex, &[]);
    count as i32
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut e = events::exports();
    e.extend(service::exports());
    e.extend(export_table![
        WSAStartup,
        WSACleanup,
        WSAGetLastError,
        WSASetLastError,
        socket,
        closesocket,
        bind,
        connect,
        listen,
        accept,
        send,
        sendto,
        recv,
        recvfrom,
        shutdown,
        setsockopt,
        getsockopt,
        ioctlsocket,
        getsockname,
        getpeername,
        htons,
        ntohs,
        htonl,
        ntohl,
        inet_addr,
        inet_ntoa,
        gethostbyname,
        gethostbyaddr,
        getprotobyname,
        getprotobynumber,
        gethostname,
        getaddrinfo,
        freeaddrinfo,
        getnameinfo,
        GetNameInfoW,
        select,
        __WSAFDIsSet,
        WSASocketW,
        WSAIoctl,
        WSASend,
        WSASendTo,
        WSARecv,
        WSARecvFrom,
        WSASendMsg,
        inet_pton,
        inet_ntop,
        WSCEnumProtocols,
        WSCGetProviderInfo,
        WSCGetProviderPath,
    ]);
    e
}

/// Winsock 2 export ordinals. The legacy DLL differs at ordinals 10-12.
pub fn ordinals() -> Vec<(u16, u64)> {
    let f = |p: usize| p as u64;
    alloc::vec![
        (1, f(accept as *const () as usize)),
        (2, f(bind as *const () as usize)),
        (3, f(closesocket as *const () as usize)),
        (4, f(connect as *const () as usize)),
        (5, f(getpeername as *const () as usize)),
        (6, f(getsockname as *const () as usize)),
        (7, f(getsockopt as *const () as usize)),
        (8, f(htonl as *const () as usize)),
        (9, f(htons as *const () as usize)),
        (10, f(ioctlsocket as *const () as usize)),
        (11, f(inet_addr as *const () as usize)),
        (12, f(inet_ntoa as *const () as usize)),
        (13, f(listen as *const () as usize)),
        (14, f(ntohl as *const () as usize)),
        (15, f(ntohs as *const () as usize)),
        (16, f(recv as *const () as usize)),
        (17, f(recvfrom as *const () as usize)),
        (18, f(select as *const () as usize)),
        (19, f(send as *const () as usize)),
        (20, f(sendto as *const () as usize)),
        (21, f(setsockopt as *const () as usize)),
        (22, f(shutdown as *const () as usize)),
        (23, f(socket as *const () as usize)),
        (51, f(gethostbyaddr as *const () as usize)),
        (52, f(gethostbyname as *const () as usize)),
        (53, f(getprotobyname as *const () as usize)),
        (54, f(getprotobynumber as *const () as usize)),
        (57, f(gethostname as *const () as usize)),
        (111, f(WSAGetLastError as *const () as usize)),
        (112, f(WSASetLastError as *const () as usize)),
        (115, f(WSAStartup as *const () as usize)),
        (116, f(WSACleanup as *const () as usize)),
        (151, f(__WSAFDIsSet as *const () as usize)),
    ]
}

/// WSOCK32 preserves its older order: inet_addr, inet_ntoa, ioctlsocket.
/// NSPR imports ordinal 12 rather than the function name.
pub fn legacy_ordinals() -> Vec<(u16, u64)> {
    let mut entries = ordinals();
    for (ordinal, address) in &mut entries {
        *address = match *ordinal {
            10 => inet_addr as *const () as u64,
            11 => inet_ntoa as *const () as u64,
            12 => ioctlsocket as *const () as u64,
            _ => *address,
        };
    }
    entries
}
