//! Networking: TCP streams and listeners, UDP sockets, ICMP echo
//! ("ping") sockets, host name resolution and interface information, on
//! the kernel's native TCP/IP stack.
//!
//! ```ignore
//! use os_std::net::{TcpStream, UdpSocket};
//! let mut stream = TcpStream::connect("10.0.2.2:8080")?;
//! stream.write_all(b"Hello from OSyx!")?;
//! let socket = UdpSocket::bind("0.0.0.0:9000")?;
//! ```
//!
//! Calls block (sleeping in the kernel) unless the socket was made
//! non-blocking; timeouts are per socket.

use crate::error::{Error, ErrorKind, Result};
use crate::io::{Read, Write};
use crate::ipc::OwnedHandle;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;
pub use os_sys::net::{
    ctl, query, preferred_ethernet, NetArp, NetConfig, NetDnsEntry, NetIface, NetRoute, NetSocket, NetStats, SockAddr, CONFIG_DHCP, CONFIG_DOWN,
    CONFIG_NONE, CONFIG_STATIC, DHCP_PHASE_NAMES, IFF_INTERRUPTS, IFF_LINK, IFF_LOOPBACK, IFF_UP, IFF_WIRELESS, NET_STAT_NAMES, TCP_STATE_NAMES,
};
use os_sys::net::{MAX_RESOLVE, MSG_DONTWAIT, SOCK_DGRAM, SOCK_PING, SOCK_STREAM};


/// An IPv4 address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0; 4]);
    pub const LOCALHOST: Ipv4Addr = Ipv4Addr([127, 0, 0, 1]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255; 4]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr([a, b, c, d])
    }

    pub fn octets(&self) -> [u8; 4] {
        self.0
    }

    pub fn is_unspecified(&self) -> bool {
        self.0 == [0; 4]
    }

    /// Dotted-quad notation only (no DNS).
    pub fn parse(s: &str) -> Option<Ipv4Addr> {
        let mut out = [0u8; 4];
        let mut parts = s.split('.');
        for b in out.iter_mut() {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            *b = p.parse().ok()?;
        }
        parts.next().is_none().then_some(Ipv4Addr(out))
    }
}

impl fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl fmt::Debug for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// An IPv4 address and port.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct SocketAddr {
    pub ip: Ipv4Addr,
    pub port: u16,
}

impl SocketAddr {
    pub const fn new(ip: Ipv4Addr, port: u16) -> SocketAddr {
        SocketAddr { ip, port }
    }

    fn raw(&self) -> SockAddr {
        SockAddr::new(self.ip.0, self.port)
    }

    fn from_raw(a: SockAddr) -> SocketAddr {
        SocketAddr { ip: Ipv4Addr(a.addr), port: a.port }
    }
}

impl fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

/// Things that name one or more socket addresses (resolving host names).
pub trait ToSocketAddrs {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>>;
}

impl ToSocketAddrs for SocketAddr {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        Ok(alloc::vec![*self])
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        Ok(alloc::vec![SocketAddr::new(self.0, self.1)])
    }
}

impl ToSocketAddrs for (&str, u16) {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        Ok(lookup_host(self.0)?.into_iter().map(|ip| SocketAddr::new(ip, self.1)).collect())
    }
}

impl ToSocketAddrs for str {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        let (host, port) = self.rsplit_once(':').ok_or(Error::new(ErrorKind::InvalidInput, "expected host:port"))?;
        let port: u16 = port.parse().map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid port"))?;
        (host, port).to_socket_addrs()
    }
}

impl ToSocketAddrs for String {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        self.as_str().to_socket_addrs()
    }
}

impl<T: ToSocketAddrs + ?Sized> ToSocketAddrs for &T {
    fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>> {
        (**self).to_socket_addrs()
    }
}

fn first_addr<A: ToSocketAddrs + ?Sized>(a: &A) -> Result<SocketAddr> {
    a.to_socket_addrs()?.into_iter().next().ok_or(Error::new(ErrorKind::InvalidInput, "no address"))
}

/// IPv4 addresses of a host name (or a dotted quad), via the kernel's
/// DNS resolver and cache.
pub fn lookup_host(name: &str) -> Result<Vec<Ipv4Addr>> {
    if let Some(a) = Ipv4Addr::parse(name) {
        return Ok(alloc::vec![a]);
    }
    let mut out = [[0u8; 4]; MAX_RESOLVE];
    let n = os_sys::resolve(name, &mut out)?;
    Ok(out[..n].iter().map(|a| Ipv4Addr(*a)).collect())
}

fn ns(d: Option<Duration>) -> u64 {
    d.map_or(0, |d| (d.as_nanos() as u64).max(1))
}

#[derive(Debug)]
struct Socket(OwnedHandle);

impl Socket {
    fn new(kind: u64) -> Result<Socket> {
        let h = os_sys::socket(kind)?;
        // SAFETY: a fresh handle owned by nobody else.
        Ok(Socket(unsafe { OwnedHandle::from_raw(h) }))
    }

    fn h(&self) -> u64 {
        self.0.raw()
    }

    fn ctl(&self, op: u64, arg: u64) -> Result<u64> {
        Ok(os_sys::sock_ctl(self.h(), op, arg)?)
    }

    fn addr(&self, op: u64) -> Result<SocketAddr> {
        let mut a = SockAddr::default();
        self.ctl(op, &mut a as *mut SockAddr as u64)?;
        Ok(SocketAddr::from_raw(a))
    }
}

/// A TCP connection.
#[derive(Debug)]
pub struct TcpStream(Socket);

impl TcpStream {
    /// Connects to the first address `addr` resolves to.
    pub fn connect<A: ToSocketAddrs + ?Sized>(addr: &A) -> Result<TcpStream> {
        let mut last = Error::new(ErrorKind::InvalidInput, "no address");
        for a in addr.to_socket_addrs()? {
            match Self::connect_timeout(&a, None) {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Connects, giving up after `timeout` (`None`: the TCP default of
    /// about a minute of SYN retransmissions).
    pub fn connect_timeout(addr: &SocketAddr, timeout: Option<Duration>) -> Result<TcpStream> {
        let s = Socket::new(SOCK_STREAM)?;
        if timeout.is_some() {
            s.ctl(ctl::CONNECT_TIMEOUT, ns(timeout))?;
        }
        os_sys::sock_connect(s.h(), &addr.raw())?;
        Ok(TcpStream(s))
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.0.addr(ctl::PEER_ADDR)
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.0.addr(ctl::LOCAL_ADDR)
    }

    /// Ends our direction: the peer reads end-of-stream after the data.
    pub fn shutdown_write(&self) -> Result<()> {
        self.0.ctl(ctl::SHUTDOWN_WRITE, 0).map(|_| ())
    }

    pub fn set_nonblocking(&self, on: bool) -> Result<()> {
        self.0.ctl(ctl::NONBLOCK, on as u64).map(|_| ())
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.0.ctl(ctl::RECV_TIMEOUT, ns(d)).map(|_| ())
    }

    pub fn set_write_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.0.ctl(ctl::SEND_TIMEOUT, ns(d)).map(|_| ())
    }

    /// Closing sends a reset instead of an orderly FIN.
    pub fn set_reset_on_close(&self, on: bool) -> Result<()> {
        self.0.ctl(ctl::RESET_ON_CLOSE, on as u64).map(|_| ())
    }

    /// The TCP state name (`ESTABLISHED`, `CLOSE-WAIT`, ...).
    pub fn state(&self) -> Result<&'static str> {
        Ok(TCP_STATE_NAMES.get(self.0.ctl(ctl::STATE, 0)? as usize).copied().unwrap_or("?"))
    }

    pub fn handle(&self) -> u64 {
        self.0.h()
    }

    /// Reads without blocking (`WouldBlock` if nothing is there).
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(os_sys::sock_recv(self.0.h(), buf, MSG_DONTWAIT)?.0)
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(os_sys::sock_recv(self.0.h(), buf, 0)?.0)
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        Ok(os_sys::sock_send(self.0.h(), buf, None, 0)?)
    }
}

/// A TCP server socket.
#[derive(Debug)]
pub struct TcpListener(Socket);

impl TcpListener {
    pub fn bind<A: ToSocketAddrs + ?Sized>(addr: &A) -> Result<TcpListener> {
        let a = first_addr(addr)?;
        let s = Socket::new(SOCK_STREAM)?;
        os_sys::sock_bind(s.h(), &a.raw())?;
        os_sys::sock_listen(s.h(), 16)?;
        Ok(TcpListener(s))
    }

    pub fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        let (h, peer) = os_sys::sock_accept(self.0.h())?;
        // SAFETY: a fresh handle owned by nobody else.
        let s = Socket(unsafe { OwnedHandle::from_raw(h) });
        Ok((TcpStream(s), SocketAddr::from_raw(peer)))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.0.addr(ctl::LOCAL_ADDR)
    }

    pub fn set_nonblocking(&self, on: bool) -> Result<()> {
        self.0.ctl(ctl::NONBLOCK, on as u64).map(|_| ())
    }

    /// Bounds how long `accept` waits.
    pub fn set_accept_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.0.ctl(ctl::RECV_TIMEOUT, ns(d)).map(|_| ())
    }

    pub fn handle(&self) -> u64 {
        self.0.h()
    }
}

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(Socket);

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs + ?Sized>(addr: &A) -> Result<UdpSocket> {
        let a = first_addr(addr)?;
        let s = Socket::new(SOCK_DGRAM)?;
        os_sys::sock_bind(s.h(), &a.raw())?;
        Ok(UdpSocket(s))
    }

    pub fn send_to<A: ToSocketAddrs + ?Sized>(&self, buf: &[u8], addr: &A) -> Result<usize> {
        let a = first_addr(addr)?;
        Ok(os_sys::sock_send(self.0.h(), buf, Some(&a.raw()), 0)?)
    }

    /// The next datagram (truncated to `buf`) and its sender.
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let (n, a) = os_sys::sock_recv(self.0.h(), buf, 0)?;
        Ok((n, SocketAddr::from_raw(a)))
    }

    /// Fixes the peer for `send` / `recv`.
    pub fn connect<A: ToSocketAddrs + ?Sized>(&self, addr: &A) -> Result<()> {
        let a = first_addr(addr)?;
        Ok(os_sys::sock_connect(self.0.h(), &a.raw())?)
    }

    pub fn send(&self, buf: &[u8]) -> Result<usize> {
        Ok(os_sys::sock_send(self.0.h(), buf, None, 0)?)
    }

    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        Ok(os_sys::sock_recv(self.0.h(), buf, 0)?.0)
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.0.addr(ctl::LOCAL_ADDR)
    }

    pub fn set_broadcast(&self, on: bool) -> Result<()> {
        self.0.ctl(ctl::BROADCAST, on as u64).map(|_| ())
    }

    pub fn set_nonblocking(&self, on: bool) -> Result<()> {
        self.0.ctl(ctl::NONBLOCK, on as u64).map(|_| ())
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.0.ctl(ctl::RECV_TIMEOUT, ns(d)).map(|_| ())
    }

    pub fn handle(&self) -> u64 {
        self.0.h()
    }
}

/// An echo reply received by a [`PingSocket`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoReply {
    pub from: Ipv4Addr,
    pub seq: u16,
    pub ttl: u8,
    pub len: usize,
}

/// ICMP echo requests and their replies (no special privileges needed).
#[derive(Debug)]
pub struct PingSocket(Socket);

impl PingSocket {
    pub fn new() -> Result<PingSocket> {
        Ok(PingSocket(Socket::new(SOCK_PING)?))
    }

    /// Sends an echo request with sequence number `seq`.
    pub fn send(&self, to: Ipv4Addr, seq: u16, payload: &[u8]) -> Result<()> {
        os_sys::sock_send(self.0.h(), payload, Some(&SockAddr::new(to.0, seq)), 0)?;
        Ok(())
    }

    /// The next reply (payload into `buf`), or the error an ICMP message
    /// (or a failed address resolution) reported for one of our requests.
    pub fn recv(&self, buf: &mut [u8]) -> Result<EchoReply> {
        let (n, a) = os_sys::sock_recv(self.0.h(), buf, 0)?;
        Ok(EchoReply { from: Ipv4Addr(a.addr), seq: a.port, ttl: a.ttl, len: n })
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> Result<()> {
        self.0.ctl(ctl::RECV_TIMEOUT, ns(d)).map(|_| ())
    }

    pub fn handle(&self) -> u64 {
        self.0.h()
    }
}

fn query_all<T: Copy + Default>(kind: u64) -> Result<Vec<T>> {
    let mut cap = 16;
    loop {
        let mut v = alloc::vec![T::default(); cap];
        let n = os_sys::net_query(kind, &mut v)?;
        if n < cap || cap >= 4096 {
            v.truncate(n);
            return Ok(v);
        }
        cap *= 4;
    }
}

/// Interfaces with their configuration and counters.
pub fn interfaces() -> Result<Vec<NetIface>> {
    query_all(query::IFACES)
}

pub fn routes() -> Result<Vec<NetRoute>> {
    query_all(query::ROUTES)
}

pub fn arp_cache() -> Result<Vec<NetArp>> {
    query_all(query::ARP)
}

/// Every socket of the system (for `netstat`).
pub fn sockets() -> Result<Vec<NetSocket>> {
    query_all(query::SOCKETS)
}

pub fn stats() -> Result<NetStats> {
    let mut s = [NetStats::default()];
    os_sys::net_query(query::STATS, &mut s)?;
    Ok(s[0])
}

pub fn dns_cache() -> Result<Vec<NetDnsEntry>> {
    query_all(query::DNS_CACHE)
}

pub fn dns_servers() -> Result<Vec<Ipv4Addr>> {
    Ok(query_all::<[u8; 4]>(query::DNS_SERVERS)?.into_iter().map(Ipv4Addr).collect())
}

/// Reconfigures an interface (root only).
pub fn configure(c: &NetConfig) -> Result<()> {
    Ok(os_sys::net_config(c)?)
}

/// The text of a NUL-padded name field.
pub fn field(b: &[u8]) -> &str {
    os_sys::ipc::field_str(b)
}
