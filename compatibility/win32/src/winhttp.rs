//! winhttp.dll: HTTP/1.1 client sessions over OSyx's TCP sockets, with
//! TLS 1.2/1.3 from rustls (the RustCrypto provider and the Mozilla root
//! store CertGetCertificateChain uses), URL cracking and proxy queries.
//!
//! Each request has its own connection. Redirects are followed for up to
//! ten hops (never from HTTPS to HTTP), as WinHTTP's default policy does.
//! In an asynchronous session (WINHTTP_FLAG_ASYNC) every operation runs on
//! a worker thread and reports through the status callback, as on Windows.
//!
//! Proxy settings come from the registry key Windows keeps them in
//! (Internet Settings of the current user). Proxy auto-configuration
//! needs a PAC script to run, and OSyx has no JavaScript engine for it: an
//! auto-detection with no WPAD host fails as on Windows, and a script that
//! exists cannot be run (ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT).
//!
//! WinINet's HTTP functions (wininet.dll) are the same sessions, connections
//! and requests: their handles are these handles.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    path, teb,
};
use alloc::{boxed::Box, collections::BTreeMap, format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::io::{Read as _, Write as _};
use os_std::net::TcpStream;
use os_std::sync::Mutex;
use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::{ServerName, UnixTime};
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};

const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
const ERROR_WINHTTP_INVALID_URL: u32 = 12005;
const ERROR_WINHTTP_UNRECOGNIZED_SCHEME: u32 = 12006;
const ERROR_WINHTTP_NAME_NOT_RESOLVED: u32 = 12007;
const ERROR_WINHTTP_INCORRECT_HANDLE_TYPE: u32 = 12018;
const ERROR_WINHTTP_INCORRECT_HANDLE_STATE: u32 = 12019;
const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
const ERROR_WINHTTP_CONNECTION_ERROR: u32 = 12030;
const ERROR_WINHTTP_HEADER_NOT_FOUND: u32 = 12150;
const ERROR_WINHTTP_INVALID_SERVER_RESPONSE: u32 = 12152;
const ERROR_WINHTTP_HEADER_ALREADY_EXISTS: u32 = 12155;
const ERROR_WINHTTP_REDIRECT_FAILED: u32 = 12156;
const ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT: u32 = 12166;
const ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT: u32 = 12167;
const ERROR_WINHTTP_SECURE_FAILURE: u32 = 12175;
const ERROR_WINHTTP_AUTODETECTION_FAILED: u32 = 12180;
const ERROR_IO_PENDING: u32 = 997;

const WINHTTP_FLAG_ASYNC: u32 = 0x1000_0000;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const MAX_HEADER: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 10;

// Status callback notifications.
const STATUS_HANDLE_CLOSING: u32 = 0x0000_0800;
const STATUS_HEADERS_AVAILABLE: u32 = 0x0002_0000;
const STATUS_READ_COMPLETE: u32 = 0x0008_0000;
const STATUS_WRITE_COMPLETE: u32 = 0x0010_0000;
const STATUS_REQUEST_ERROR: u32 = 0x0020_0000;
const STATUS_SENDREQUEST_COMPLETE: u32 = 0x0040_0000;
const STATUS_GETPROXYFORURL_COMPLETE: u32 = 0x0100_0000;
// WINHTTP_ASYNC_RESULT.dwResult: the API that failed.
const API_RECEIVE_RESPONSE: u64 = 1;
const API_READ_DATA: u64 = 3;
const API_WRITE_DATA: u64 = 4;
const API_SEND_REQUEST: u64 = 5;
const API_GET_PROXY_FOR_URL: u64 = 6;

// ---- TLS ---------------------------------------------------------------------------------

#[derive(Debug)]
struct Clock;

impl rustls::time_provider::TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        let ns = os_sys::clock(osyx_abi::CLOCK_REALTIME).ok()?;
        Some(UnixTime::since_unix_epoch(core::time::Duration::from_nanos(ns)))
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, u32> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_details(Arc::new(rustls_rustcrypto::provider()), Arc::new(Clock))
        .with_safe_default_protocol_versions()
        .map_err(|_| ERROR_WINHTTP_SECURE_FAILURE)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A connection: plain TCP or TLS over TCP.
trait Stream: Send {
    fn send(&mut self, data: &[u8]) -> Result<(), u32>;
    /// 0 at the end of the stream.
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, u32>;
}

fn io_error(e: os_std::error::Error) -> u32 {
    if e.kind() == os_std::error::ErrorKind::TimedOut { ERROR_WINHTTP_TIMEOUT } else { ERROR_WINHTTP_CONNECTION_ERROR }
}

impl Stream for TcpStream {
    fn send(&mut self, data: &[u8]) -> Result<(), u32> {
        self.write_all(data).map_err(io_error)
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, u32> {
        self.read(buf).map_err(io_error)
    }
}

/// TLS client over a TCP stream, through rustls' unbuffered API.
struct Tls {
    conn: UnbufferedClientConnection,
    sock: TcpStream,
    incoming: Vec<u8>,
    used: usize,
    outgoing: Vec<u8>,
    out_len: usize,
    plain: Vec<u8>,
    plain_at: usize,
    eof: bool,
}

impl Tls {
    fn new(sock: TcpStream, host: &str) -> Result<Tls, u32> {
        let name = ServerName::try_from(String::from(host)).map_err(|_| ERROR_WINHTTP_INVALID_URL)?;
        let conn = UnbufferedClientConnection::new(tls_config()?, name).map_err(|_| ERROR_WINHTTP_SECURE_FAILURE)?;
        let mut t = Tls { conn, sock, incoming: vec![0; 32 * 1024], used: 0, outgoing: vec![0; 32 * 1024], out_len: 0, plain: Vec::new(), plain_at: 0, eof: false };
        t.drive(None, false)?;
        Ok(t)
    }

    /// Runs the state machine until `write` was sent (and, with `read`,
    /// until application data or the end arrived); with neither, until the
    /// handshake is complete.
    fn drive(&mut self, mut write: Option<&[u8]>, read: bool) -> Result<(), u32> {
        const TLS: u32 = ERROR_WINHTTP_SECURE_FAILURE;
        loop {
            if read && (self.plain_at < self.plain.len() || self.eof) && write.is_none() {
                return Ok(());
            }
            let Tls { conn, sock, incoming, used, outgoing, out_len, plain, eof, .. } = self;
            let UnbufferedStatus { mut discard, state } = conn.process_tls_records(&mut incoming[..*used]);
            let (mut need_input, mut done) = (false, false);
            match state.map_err(|_| TLS)? {
                ConnectionState::EncodeTlsData(mut s) => loop {
                    match s.encode(&mut outgoing[*out_len..]) {
                        Ok(n) => {
                            *out_len += n;
                            break;
                        }
                        Err(EncodeError::InsufficientSize(e)) => outgoing.resize(*out_len + e.required_size, 0),
                        Err(_) => return Err(TLS),
                    }
                },
                ConnectionState::TransmitTlsData(t) => {
                    sock.write_all(&outgoing[..*out_len]).map_err(io_error)?;
                    *out_len = 0;
                    t.done();
                }
                ConnectionState::BlockedHandshake => need_input = true,
                ConnectionState::WriteTraffic(mut w) => {
                    if let Some(data) = write.take() {
                        let n = loop {
                            match w.encrypt(data, &mut outgoing[..]) {
                                Ok(n) => break n,
                                Err(EncryptError::InsufficientSize(e)) => outgoing.resize(e.required_size, 0),
                                Err(_) => return Err(TLS),
                            }
                        };
                        sock.write_all(&outgoing[..n]).map_err(io_error)?;
                    } else if read && !*eof {
                        need_input = true;
                    } else {
                        done = true;
                    }
                }
                ConnectionState::ReadTraffic(mut r) => {
                    while let Some(rec) = r.next_record() {
                        let rec = rec.map_err(|_| TLS)?;
                        discard += rec.discard;
                        plain.extend_from_slice(rec.payload);
                    }
                }
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    *eof = true;
                    done = write.is_none();
                }
                _ => return Err(TLS),
            }
            if discard > 0 {
                incoming.copy_within(discard..*used, 0);
                *used -= discard;
            }
            if need_input {
                if *used == incoming.len() {
                    let n = incoming.len() * 2;
                    incoming.resize(n, 0);
                }
                let n = sock.read(&mut incoming[*used..]).map_err(io_error)?;
                if n == 0 {
                    if !read {
                        return Err(TLS);
                    }
                    *eof = true;
                }
                *used += n;
            }
            if done {
                return Ok(());
            }
        }
    }
}

impl Stream for Tls {
    fn send(&mut self, data: &[u8]) -> Result<(), u32> {
        self.drive(Some(data), false)
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, u32> {
        if self.plain_at == self.plain.len() {
            self.plain.clear();
            self.plain_at = 0;
            self.drive(None, true)?;
        }
        let n = buf.len().min(self.plain.len() - self.plain_at);
        buf[..n].copy_from_slice(&self.plain[self.plain_at..self.plain_at + n]);
        self.plain_at += n;
        Ok(n)
    }
}

/// Connects to `host:port`, trying each address in turn.
fn connect_tcp(host: &str, port: u16, timeouts: [u32; 4]) -> Result<TcpStream, u32> {
    let ms = |v: u32| (v != 0 && v != u32::MAX).then(|| core::time::Duration::from_millis(v as u64));
    let addrs = os_std::net::lookup_host(host).map_err(|_| ERROR_WINHTTP_NAME_NOT_RESOLVED)?;
    let mut last = ERROR_WINHTTP_NAME_NOT_RESOLVED;
    for a in addrs {
        match TcpStream::connect_timeout(&os_std::net::SocketAddr::new(a, port), ms(timeouts[1])) {
            Ok(s) => {
                let _ = s.set_read_timeout(ms(timeouts[3]));
                let _ = s.set_write_timeout(ms(timeouts[2]));
                return Ok(s);
            }
            Err(e) => last = if e.kind() == os_std::error::ErrorKind::TimedOut { ERROR_WINHTTP_TIMEOUT } else { ERROR_WINHTTP_CANNOT_CONNECT },
        }
    }
    Err(last)
}

/// A tunnel to `host:port` through an HTTP proxy (CONNECT).
fn tunnel(mut s: TcpStream, host: &str, port: u16) -> Result<TcpStream, u32> {
    s.send(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    // Byte by byte: what follows the reply belongs to TLS.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > MAX_HEADER || s.recv(&mut byte)? == 0 {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    match line.split(' ').nth(1) {
        Some("200") => Ok(s),
        _ => Err(ERROR_WINHTTP_CANNOT_CONNECT),
    }
}

// ---- handles -----------------------------------------------------------------------------

/// How the response body ends.
enum Body {
    None,
    Length(u64),
    /// Chunked: bytes left in the current chunk (None: at a chunk header).
    Chunked(Option<u64>),
    Close,
}

struct Response {
    status: u32,
    reason: String,
    version: String,
    headers: Vec<(String, String)>,
    /// The status line and headers as received, CRLF-separated.
    raw: String,
}

struct Request {
    host: String,
    port: u16,
    secure: bool,
    verb: String,
    path: String,
    headers: Vec<(String, String)>,
    stream: Option<Box<dyn Stream>>,
    /// Request body bytes still to come through WinHttpWriteData.
    to_write: u64,
    response: Option<Response>,
    body: Body,
    /// Received bytes not yet consumed.
    buffered: Vec<u8>,
    /// Body bytes read ahead by InternetQueryDataAvailable.
    peeked: Vec<u8>,
}

enum Kind {
    /// A session: user agent, flags, and the named proxy with its bypass
    /// list (WINHTTP_ACCESS_TYPE_NAMED_PROXY).
    Session { agent: String, flags: u32, proxy: Option<(String, u16)>, bypass: Vec<String> },
    Connect { host: String, port: u16 },
    Request(Mutex<Request>),
    Resolver(Mutex<Option<Vec<ProxyEntry>>>),
}

struct Obj {
    kind: Kind,
    parent: u64,
    /// The status callback and the notifications it wants.
    callback: Mutex<(u64, u32)>,
    timeouts: Mutex<[u32; 4]>,
    /// The context of the asynchronous operation under way.
    context: AtomicU64,
}

static HANDLES: Mutex<BTreeMap<u64, Arc<Obj>>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x4e70_0010);

fn new_handle(kind: Kind, parent: u64) -> u64 {
    let (callback, timeouts) = match get(parent) {
        Some(p) => (*p.callback.lock().unwrap(), *p.timeouts.lock().unwrap()),
        // Resolve: none; connect 60 s, send and receive 30 s (WinHTTP's).
        None => ((0, 0), [0, 60_000, 30_000, 30_000]),
    };
    let h = NEXT.fetch_add(0x10, Ordering::Relaxed);
    let o = Obj { kind, parent, callback: Mutex::new(callback), timeouts: Mutex::new(timeouts), context: AtomicU64::new(0) };
    HANDLES.lock().unwrap().insert(h, Arc::new(o));
    h
}

fn get(h: u64) -> Option<Arc<Obj>> {
    HANDLES.lock().unwrap().get(&h).cloned()
}

/// The user agent, flags and proxy of the session a handle belongs to.
fn session_info(o: &Obj) -> (String, u32, Option<(String, u16)>, Vec<String>) {
    match &o.kind {
        Kind::Session { agent, flags, proxy, bypass } => (agent.clone(), *flags, proxy.clone(), bypass.clone()),
        _ => get(o.parent).map_or((String::new(), 0, None, Vec::new()), |p| session_info(&p)),
    }
}

fn is_async(o: &Obj) -> bool {
    session_info(o).1 & WINHTTP_FLAG_ASYNC != 0
}

/// Whether `host` is on a proxy bypass list ("<local>": names without a
/// dot; "*" wildcards).
fn bypassed(host: &str, list: &[String]) -> bool {
    list.iter().any(|e| {
        let e = e.to_ascii_lowercase();
        if e == "<local>" {
            return !host.contains('.');
        }
        match e.split_once('*') {
            Some((a, b)) => host.len() >= a.len() + b.len() && host.starts_with(a) && host.ends_with(b),
            None => host == e,
        }
    })
}

/// The proxy of a proxy string ("host:port", or "http=host:port;..."
/// whose entry for the scheme, else the first, is used).
fn parse_proxy(s: &str, secure: bool) -> Option<(String, u16)> {
    let entries: Vec<&str> = s.split([';', ' ']).filter(|e| !e.is_empty()).collect();
    let scheme = if secure { "https=" } else { "http=" };
    let e = entries.iter().find(|e| e.starts_with(scheme)).or(entries.first())?;
    let e = e.split_once('=').map_or(*e, |(_, v)| v);
    let e = e.strip_prefix("http://").unwrap_or(e);
    match e.rsplit_once(':') {
        Some((h, p)) => Some((String::from(h), p.trim_end_matches('/').parse().ok()?)),
        None => Some((String::from(e), 80)),
    }
}

type Callback = extern "win64" fn(u64, u64, u32, u64, u32);

/// Calls the handle's status callback for `status`, if it asked for it.
fn notify(h: u64, o: &Obj, status: u32, info: u64, len: u32) {
    let (cb, wanted) = *o.callback.lock().unwrap();
    if cb != 0 && cb != u64::MAX && wanted & status != 0 {
        // SAFETY: a WINHTTP_STATUS_CALLBACK the caller registered.
        let f: Callback = unsafe { core::mem::transmute(cb) };
        f(h, o.context.load(Ordering::SeqCst), status, info, len);
    }
}

/// Reports an asynchronous failure: REQUEST_ERROR with WINHTTP_ASYNC_RESULT.
fn notify_error(h: u64, o: &Obj, api: u64, e: u32) {
    let mut result = [0u8; 16];
    result[..8].copy_from_slice(&api.to_le_bytes());
    result[8..12].copy_from_slice(&e.to_le_bytes());
    notify(h, o, STATUS_REQUEST_ERROR, result.as_ptr() as u64, 16);
}

/// Runs `job` on a new Windows thread (status callbacks run Windows code).
fn spawn(job: Box<dyn FnOnce() + Send>) {
    extern "win64" fn entry(p: u64) -> u32 {
        // SAFETY: the box `spawn` leaked for this thread.
        let job = unsafe { Box::from_raw(p as *mut Box<dyn FnOnce() + Send>) };
        job();
        0
    }
    let p = Box::into_raw(Box::new(job)) as u64;
    let t = kernel32::CreateThread(0, 0, entry as *const () as u64, p, 0, core::ptr::null_mut());
    if t == 0 {
        // SAFETY: the thread was not made: the job is still ours.
        let job = unsafe { Box::from_raw(p as *mut Box<dyn FnOnce() + Send>) };
        job();
    } else {
        kernel32::CloseHandle(t);
    }
}

/// # Safety
/// `p` is NULL or a NUL-terminated UTF-16 string.
unsafe fn ws(p: *const u16) -> Option<String> {
    // SAFETY: the caller's guarantee.
    unsafe { path::wide_to_string(p) }
}

// ---- sessions, connections, requests --------------------------------------------------

extern "win64" fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> u64 {
    if flags & !(WINHTTP_FLAG_ASYNC | 0x2000_0000) != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: NULL or NUL-terminated strings.
    let (agent, proxy, bypass) = unsafe { (ws(agent).unwrap_or_default(), ws(proxy), ws(bypass)) };
    // WINHTTP_ACCESS_TYPE_NAMED_PROXY: the proxy given; otherwise direct.
    let proxy = if access == 3 { proxy.and_then(|p| parse_proxy(&p, false)) } else { None };
    let bypass = bypass.map_or(Vec::new(), |b| b.split([';', ' ']).filter(|e| !e.is_empty()).map(String::from).collect());
    new_handle(Kind::Session { agent, flags, proxy, bypass }, 0)
}

extern "win64" fn WinHttpConnect(session: u64, server: *const u16, port: u16, _reserved: u32) -> u64 {
    let Some(s) = get(session) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if !matches!(s.kind, Kind::Session { .. }) {
        return fail(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE, 0);
    }
    // SAFETY: NULL or the caller's NUL-terminated name.
    let Some(host) = (unsafe { ws(server) }).filter(|h| !h.is_empty()) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    new_handle(Kind::Connect { host: host.to_ascii_lowercase(), port }, session)
}

extern "win64" fn WinHttpOpenRequest(connect: u64, verb: *const u16, object: *const u16, _version: *const u16, referrer: *const u16, accept: *const u64, flags: u32) -> u64 {
    let Some(c) = get(connect) else { return fail(ERROR_INVALID_HANDLE, 0) };
    let Kind::Connect { host, port } = &c.kind else { return fail(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE, 0) };
    let secure = flags & WINHTTP_FLAG_SECURE != 0;
    let port = if *port == 0 { if secure { 443 } else { 80 } } else { *port };
    // SAFETY: NULL or NUL-terminated strings; a NULL-terminated array of them.
    let (verb, path, referrer, accepts) = unsafe {
        let mut a = Vec::new();
        let mut i = 0;
        while !accept.is_null() && *accept.add(i) != 0 {
            a.extend(ws(*accept.add(i) as *const u16));
            i += 1;
        }
        (ws(verb).unwrap_or_else(|| String::from("GET")), ws(object).unwrap_or_else(|| String::from("/")), ws(referrer), a)
    };
    let path = if path.starts_with('/') { path } else { format!("/{path}") };
    let mut headers = Vec::new();
    if let Some(r) = referrer {
        headers.push((String::from("Referer"), r));
    }
    if !accepts.is_empty() {
        headers.push((String::from("Accept"), accepts.join(", ")));
    }
    let r = Request { host: host.clone(), port, secure, verb, path, headers, stream: None, to_write: 0, response: None, body: Body::None, buffered: Vec::new(), peeked: Vec::new() };
    new_handle(Kind::Request(Mutex::new(r)), connect)
}

const WINHTTP_ADDREQ_FLAG_ADD_IF_NEW: u32 = 0x1000_0000;
const WINHTTP_ADDREQ_FLAG_ADD: u32 = 0x2000_0000;
const WINHTTP_ADDREQ_FLAG_COALESCE_WITH_COMMA: u32 = 0x4000_0000;
const WINHTTP_ADDREQ_FLAG_COALESCE_WITH_SEMICOLON: u32 = 0x0100_0000;
const WINHTTP_ADDREQ_FLAG_REPLACE: u32 = 0x8000_0000;

/// Adds "Name: value" lines to a request's headers under the modifiers.
fn add_headers(r: &mut Request, text: &str, modifiers: u32) -> Result<(), u32> {
    for line in text.split(['\r', '\n']).filter(|l| !l.trim().is_empty()) {
        let (name, value) = line.split_once(':').ok_or(ERROR_INVALID_PARAMETER)?;
        let (name, value) = (name.trim(), value.trim());
        let at = r.headers.iter().position(|(n, _)| n.eq_ignore_ascii_case(name));
        if modifiers & WINHTTP_ADDREQ_FLAG_REPLACE != 0 {
            r.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
            if !value.is_empty() {
                r.headers.push((String::from(name), String::from(value)));
            }
        } else if let (Some(i), true) = (at, modifiers & (WINHTTP_ADDREQ_FLAG_COALESCE_WITH_COMMA | WINHTTP_ADDREQ_FLAG_COALESCE_WITH_SEMICOLON) != 0) {
            let sep = if modifiers & WINHTTP_ADDREQ_FLAG_COALESCE_WITH_COMMA != 0 { ", " } else { "; " };
            r.headers[i].1 = format!("{}{sep}{value}", r.headers[i].1);
        } else if at.is_some() && modifiers & WINHTTP_ADDREQ_FLAG_ADD_IF_NEW != 0 {
            return Err(ERROR_WINHTTP_HEADER_ALREADY_EXISTS);
        } else {
            r.headers.push((String::from(name), String::from(value)));
        }
    }
    Ok(())
}

/// # Safety
/// `p` holds `len` UTF-16 units, or is NUL-terminated for `len` u32::MAX.
unsafe fn header_text(p: *const u16, len: u32) -> Option<String> {
    if p.is_null() {
        return None;
    }
    if len == u32::MAX {
        // SAFETY: the caller's guarantee.
        return unsafe { ws(p) };
    }
    // SAFETY: as above.
    Some(String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(p, len as usize) }))
}

fn request(h: u64) -> Result<Arc<Obj>, u32> {
    let o = get(h).ok_or(ERROR_INVALID_HANDLE)?;
    if !matches!(o.kind, Kind::Request(_)) {
        return Err(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE);
    }
    Ok(o)
}

fn with_request<T>(o: &Obj, f: impl FnOnce(&mut Request) -> Result<T, u32>) -> Result<T, u32> {
    let Kind::Request(r) = &o.kind else { return Err(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE) };
    f(&mut r.lock().unwrap())
}

extern "win64" fn WinHttpAddRequestHeaders(h: u64, headers: *const u16, len: u32, modifiers: u32) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    // SAFETY: the caller's header text.
    let Some(text) = (unsafe { header_text(headers, len) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let modifiers = if modifiers & 0xFFFF_0000 == 0 { modifiers | WINHTTP_ADDREQ_FLAG_ADD } else { modifiers };
    match with_request(&o, |r| add_headers(r, &text, modifiers)) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn WinHttpSetTimeouts(h: u64, resolve: i32, connect: i32, send: i32, receive: i32) -> i32 {
    let Some(o) = get(h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if [resolve, connect, send, receive].iter().any(|&v| v < -1) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    *o.timeouts.lock().unwrap() = [resolve as u32, connect as u32, send as u32, receive as u32];
    1
}

extern "win64" fn WinHttpSetStatusCallback(h: u64, callback: u64, flags: u32, _reserved: u64) -> u64 {
    let Some(o) = get(h) else { return fail(ERROR_INVALID_HANDLE, u64::MAX) };
    let mut c = o.callback.lock().unwrap();
    let old = c.0;
    *c = (callback, flags);
    old
}

/// How a request reaches its server: the session's user agent and
/// proxy, and the timeouts.
struct Route {
    agent: String,
    proxy: Option<(String, u16)>,
    bypass: Vec<String>,
    timeouts: [u32; 4],
}

fn route(o: &Obj) -> Route {
    let (agent, _, proxy, bypass) = session_info(o);
    Route { agent, proxy, bypass, timeouts: *o.timeouts.lock().unwrap() }
}

/// Sends the request head and the optional data on a new connection
/// (through the proxy unless the host bypasses it).
fn send_request(r: &mut Request, via: &Route, optional: &[u8], total: u64) -> Result<(), u32> {
    let proxy = via.proxy.as_ref().filter(|_| !bypassed(&r.host, &via.bypass));
    let default_port = if r.secure { 443 } else { 80 };
    let host = if r.port == default_port { r.host.clone() } else { format!("{}:{}", r.host, r.port) };
    let (mut stream, target): (Box<dyn Stream>, String) = match (proxy, r.secure) {
        (None, false) => (Box::new(connect_tcp(&r.host, r.port, via.timeouts)?), r.path.clone()),
        (None, true) => (Box::new(Tls::new(connect_tcp(&r.host, r.port, via.timeouts)?, &r.host)?), r.path.clone()),
        // A plain request goes to the proxy in absolute form.
        (Some((ph, pp)), false) => (Box::new(connect_tcp(ph, *pp, via.timeouts)?), format!("http://{host}{}", r.path)),
        (Some((ph, pp)), true) => (Box::new(Tls::new(tunnel(connect_tcp(ph, *pp, via.timeouts)?, &r.host, r.port)?, &r.host)?), r.path.clone()),
    };
    let agent = &via.agent;
    let mut head = format!("{} {target} HTTP/1.1\r\nHost: {host}\r\n", r.verb);
    if !agent.is_empty() && !r.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("User-Agent")) {
        head.push_str(&format!("User-Agent: {agent}\r\n"));
    }
    for (n, v) in &r.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    let length = total.max(optional.len() as u64);
    if (length > 0 || matches!(r.verb.as_str(), "POST" | "PUT")) && !r.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("Content-Length")) {
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    head.push_str("\r\n");
    stream.send(head.as_bytes())?;
    if !optional.is_empty() {
        stream.send(optional)?;
    }
    r.to_write = length - optional.len() as u64;
    r.stream = Some(stream);
    r.response = None;
    r.buffered.clear();
    r.peeked.clear();
    Ok(())
}

extern "win64" fn WinHttpSendRequest(h: u64, headers: *const u16, hlen: u32, optional: *const u8, olen: u32, total: u32, context: u64) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    // SAFETY: the caller's header text and optional data.
    let (text, data) = unsafe {
        (header_text(headers, hlen), if optional.is_null() { Vec::new() } else { core::slice::from_raw_parts(optional, olen as usize).to_vec() })
    };
    if let Some(t) = text {
        if let Err(e) = with_request(&o, |r| add_headers(r, &t, WINHTTP_ADDREQ_FLAG_ADD)) {
            return fail(e, 0);
        }
    }
    o.context.store(context, Ordering::SeqCst);
    let via = route(&o);
    let run = move |o: &Obj| with_request(o, |r| send_request(r, &via, &data, total as u64));
    if is_async(&o) {
        spawn(Box::new(move || match run(&o) {
            Ok(()) => notify(h, &o, STATUS_SENDREQUEST_COMPLETE, 0, 0),
            Err(e) => notify_error(h, &o, API_SEND_REQUEST, e),
        }));
        return 1;
    }
    match run(&o) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn WinHttpWriteData(h: u64, buf: *const u8, n: u32, written: *mut u32) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    if buf.is_null() && n != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's `n` bytes.
    let data = if n == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(buf, n as usize) }.to_vec() };
    let run = move |o: &Obj| {
        with_request(o, |r| {
            let s = r.stream.as_mut().ok_or(ERROR_WINHTTP_INCORRECT_HANDLE_STATE)?;
            s.send(&data)?;
            r.to_write = r.to_write.saturating_sub(data.len() as u64);
            Ok(data.len() as u32)
        })
    };
    if is_async(&o) {
        spawn(Box::new(move || match run(&o) {
            Ok(n) => notify(h, &o, STATUS_WRITE_COMPLETE, &n as *const u32 as u64, 4),
            Err(e) => notify_error(h, &o, API_WRITE_DATA, e),
        }));
        return 1;
    }
    match run(&o) {
        Ok(k) => {
            if !written.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *written = k };
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

/// Reads the status line and headers.
fn read_head(r: &mut Request) -> Result<Response, u32> {
    let s = r.stream.as_mut().ok_or(ERROR_WINHTTP_INCORRECT_HANDLE_STATE)?;
    let mut buf = core::mem::take(&mut r.buffered);
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        }
        let mut chunk = [0u8; 4096];
        let n = s.recv(&mut chunk)?;
        if n == 0 {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    r.buffered = buf[end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = String::from(parts.next().unwrap_or_default());
    let status: u32 = parts.next().and_then(|c| c.parse().ok()).ok_or(ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
    if !version.starts_with("HTTP/") {
        return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
    }
    let reason = String::from(parts.next().unwrap_or_default());
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (String::from(n.trim()), String::from(v.trim()))).collect();
    Ok(Response { status, reason, version, headers, raw: head })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Reads up to `out.len()` body bytes; 0 at the end of the body.
fn read_body(r: &mut Request, out: &mut [u8]) -> Result<usize, u32> {
    fn fill(r: &mut Request) -> Result<bool, u32> {
        let s = r.stream.as_mut().ok_or(ERROR_WINHTTP_INCORRECT_HANDLE_STATE)?;
        let mut chunk = [0u8; 16 * 1024];
        let n = s.recv(&mut chunk)?;
        r.buffered.extend_from_slice(&chunk[..n]);
        Ok(n > 0)
    }
    fn take(r: &mut Request, out: &mut [u8], max: u64) -> usize {
        let n = out.len().min(r.buffered.len()).min(max as usize);
        out[..n].copy_from_slice(&r.buffered[..n]);
        r.buffered.drain(..n);
        n
    }
    loop {
        match r.body {
            Body::None | Body::Length(0) => return Ok(0),
            Body::Length(left) => {
                if r.buffered.is_empty() && !fill(r)? {
                    return Err(ERROR_WINHTTP_CONNECTION_ERROR);
                }
                let n = take(r, out, left);
                r.body = Body::Length(left - n as u64);
                return Ok(n);
            }
            Body::Close => {
                if r.buffered.is_empty() && !fill(r)? {
                    r.body = Body::None;
                    return Ok(0);
                }
                return Ok(take(r, out, u64::MAX));
            }
            Body::Chunked(None) => {
                let Some(i) = r.buffered.windows(2).position(|w| w == b"\r\n") else {
                    if !fill(r)? {
                        return Err(ERROR_WINHTTP_CONNECTION_ERROR);
                    }
                    continue;
                };
                let line = String::from_utf8_lossy(&r.buffered[..i]).into_owned();
                r.buffered.drain(..i + 2);
                let size = u64::from_str_radix(line.split(';').next().unwrap_or_default().trim(), 16).map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
                if size == 0 {
                    // The trailer is not read: the connection is not reused.
                    r.body = Body::None;
                    return Ok(0);
                }
                r.body = Body::Chunked(Some(size));
            }
            Body::Chunked(Some(left)) => {
                if r.buffered.is_empty() && !fill(r)? {
                    return Err(ERROR_WINHTTP_CONNECTION_ERROR);
                }
                let n = take(r, out, left);
                if n as u64 == left {
                    // The chunk's CRLF.
                    while r.buffered.len() < 2 {
                        if !fill(r)? {
                            return Err(ERROR_WINHTTP_CONNECTION_ERROR);
                        }
                    }
                    r.buffered.drain(..2);
                    r.body = Body::Chunked(None);
                } else {
                    r.body = Body::Chunked(Some(left - n as u64));
                }
                return Ok(n);
            }
        }
    }
}

/// Reads the response, following redirects.
fn receive_response(r: &mut Request, via: &Route) -> Result<(), u32> {
    if r.to_write != 0 {
        return Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE);
    }
    for _ in 0..=MAX_REDIRECTS {
        let resp = read_head(r)?;
        let no_body = r.verb == "HEAD" || resp.status == 204 || resp.status == 304 || (100..200).contains(&resp.status);
        r.body = if no_body {
            Body::None
        } else if header(&resp.headers, "Transfer-Encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
            Body::Chunked(None)
        } else if let Some(n) = header(&resp.headers, "Content-Length").and_then(|v| v.parse().ok()) {
            Body::Length(n)
        } else {
            Body::Close
        };
        let location = header(&resp.headers, "Location").map(String::from);
        let redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308);
        r.response = Some(resp);
        let (true, Some(loc)) = (redirect, location) else { return Ok(()) };
        let target = if loc.starts_with("http://") || loc.starts_with("https://") {
            loc
        } else {
            format!("{}://{}:{}{}", if r.secure { "https" } else { "http" }, r.host, r.port, if loc.starts_with('/') { loc } else { format!("/{loc}") })
        };
        let (secure, host, port, path) = split_url(&target)?;
        if r.secure && !secure {
            return Ok(()); // WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP
        }
        let status = r.response.as_ref().map_or(0, |x| x.status);
        if status == 303 || ((status == 301 || status == 302) && r.verb == "POST") {
            r.verb = String::from("GET");
            r.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("Content-Length") && !n.eq_ignore_ascii_case("Content-Type"));
        } else if r.verb != "GET" && r.verb != "HEAD" {
            return Err(ERROR_WINHTTP_REDIRECT_FAILED); // a body cannot be sent again
        }
        (r.secure, r.host, r.port, r.path) = (secure, host, port, path);
        send_request(r, via, &[], 0)?;
    }
    Err(ERROR_WINHTTP_REDIRECT_FAILED)
}

/// (https, host, port, path) of an http(s) URL.
fn split_url(url: &str) -> Result<(bool, String, u16, String), u32> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(ERROR_WINHTTP_UNRECOGNIZED_SCHEME);
    };
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], String::from(&rest[i..])),
        None => (rest, String::from("/")),
    };
    let path = String::from(path.split('#').next().unwrap_or("/"));
    let path = if path.starts_with('/') { path } else { format!("/{path}") };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| ERROR_WINHTTP_INVALID_URL)?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(ERROR_WINHTTP_INVALID_URL);
    }
    Ok((secure, host.to_ascii_lowercase(), port, path))
}

extern "win64" fn WinHttpReceiveResponse(h: u64, _reserved: u64) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    let via = route(&o);
    let run = move |o: &Obj| with_request(o, |r| receive_response(r, &via));
    if is_async(&o) {
        spawn(Box::new(move || match run(&o) {
            Ok(()) => notify(h, &o, STATUS_HEADERS_AVAILABLE, 0, 0),
            Err(e) => notify_error(h, &o, API_RECEIVE_RESPONSE, e),
        }));
        return 1;
    }
    match run(&o) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn WinHttpReadData(h: u64, buf: *mut u8, n: u32, read: *mut u32) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    if buf.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let addr = buf as u64;
    let run = move |o: &Obj| {
        with_request(o, |r| {
            if r.response.is_none() {
                return Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE);
            }
            // SAFETY: the caller's buffer of `n` bytes, theirs until the read completes.
            let out = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, n as usize) };
            if !r.peeked.is_empty() {
                let k = out.len().min(r.peeked.len());
                out[..k].copy_from_slice(&r.peeked[..k]);
                r.peeked.drain(..k);
                return Ok(k);
            }
            read_body(r, out)
        })
    };
    if is_async(&o) {
        spawn(Box::new(move || match run(&o) {
            Ok(k) => notify(h, &o, STATUS_READ_COMPLETE, addr, k as u32),
            Err(e) => notify_error(h, &o, API_READ_DATA, e),
        }));
        return 1;
    }
    match run(&o) {
        Ok(k) => {
            if !read.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *read = k as u32 };
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

// ---- headers -----------------------------------------------------------------------------

const WINHTTP_QUERY_VERSION: u32 = 18;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_STATUS_TEXT: u32 = 20;
const WINHTTP_QUERY_RAW_HEADERS: u32 = 21;
const WINHTTP_QUERY_RAW_HEADERS_CRLF: u32 = 22;
const WINHTTP_QUERY_REQUEST_METHOD: u32 = 45;
const WINHTTP_QUERY_CUSTOM: u32 = 65535;
const WINHTTP_QUERY_FLAG_REQUEST_HEADERS: u32 = 0x8000_0000;
const WINHTTP_QUERY_FLAG_SYSTEMTIME: u32 = 0x4000_0000;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
const WINHTTP_QUERY_FLAG_NUMBER64: u32 = 0x0800_0000;

/// The header names of the WINHTTP_QUERY_* indexes.
const QUERY_NAMES: [&str; 71] = [
    "MIME-Version", "Content-Type", "Content-Transfer-Encoding", "Content-ID", "Content-Description", "Content-Length",
    "Content-Language", "Allow", "Public", "Date", "Expires", "Last-Modified", "Message-ID", "URI", "Derived-From", "Cost",
    "Link", "Pragma", "", "", "", "", "", "Connection", "Accept", "Accept-Charset", "Accept-Encoding", "Accept-Language",
    "Authorization", "Content-Encoding", "Forwarded", "From", "If-Modified-Since", "Location", "Orig-URI", "Referer",
    "Retry-After", "Server", "Title", "User-Agent", "WWW-Authenticate", "Proxy-Authenticate", "Accept-Ranges", "Set-Cookie",
    "Cookie", "", "Refresh", "Content-Disposition", "Age", "Cache-Control", "Content-Base", "Content-Location",
    "Content-MD5", "Content-Range", "ETag", "Host", "If-Match", "If-None-Match", "If-Range", "If-Unmodified-Since",
    "Max-Forwards", "Proxy-Authorization", "Range", "Transfer-Encoding", "Upgrade", "Vary", "Via", "Warning", "Expect",
    "Proxy-Connection", "Unless-Modified-Since",
];

/// "Sun, 06 Nov 1994 08:49:37 GMT" as a SYSTEMTIME.
fn http_date(s: &str) -> Option<[u16; 8]> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let mut f = s.split([' ', ',', '-']).filter(|p| !p.is_empty());
    let first = f.next()?;
    let day: u16 = match first.parse() {
        Ok(d) => d,
        Err(_) => f.next()?.parse().ok()?,
    };
    let month = MONTHS.iter().position(|m| f.clone().next().is_some_and(|x| x.to_ascii_lowercase().starts_with(m)))? as u16 + 1;
    f.next();
    let mut year: u16 = f.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let mut t = f.next()?.split(':').map(|x| x.parse::<u16>().ok());
    let (h, m, sec) = (t.next()??, t.next()??, t.next()??);
    let days = osyx_ui::time::days_from_civil(year as i64, month as u32, day as u32);
    let weekday = osyx_ui::time::weekday(days) as u16;
    Some([year, month, weekday, day, h, m, sec, 0])
}

extern "win64" fn WinHttpQueryHeaders(h: u64, level: u32, name: *const u16, buf: *mut u8, len: *mut u32, index: *mut u32) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    if len.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let info = level & 0xFFFF;
    let flags = level & 0xFFFF_0000;
    // SAFETY: NULL or the caller's index and custom name.
    let (wanted_index, custom) = unsafe { (if index.is_null() { 0 } else { *index }, ws(name)) };
    let text = with_request(&o, |r| {
        let req_headers = flags & WINHTTP_QUERY_FLAG_REQUEST_HEADERS != 0;
        let resp = r.response.as_ref();
        if !req_headers && resp.is_none() {
            return Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE);
        }
        let headers: &[(String, String)] = if req_headers { &r.headers } else { &resp.unwrap().headers };
        let single = |v: String| if wanted_index == 0 { Ok(v) } else { Err(ERROR_WINHTTP_HEADER_NOT_FOUND) };
        match info {
            WINHTTP_QUERY_STATUS_CODE if !req_headers => single(format!("{}", resp.unwrap().status)),
            WINHTTP_QUERY_STATUS_TEXT if !req_headers => single(resp.unwrap().reason.clone()),
            WINHTTP_QUERY_VERSION if !req_headers => single(resp.unwrap().version.clone()),
            WINHTTP_QUERY_REQUEST_METHOD => single(r.verb.clone()),
            WINHTTP_QUERY_RAW_HEADERS_CRLF | WINHTTP_QUERY_RAW_HEADERS => {
                let raw = if req_headers {
                    let mut s = format!("{} {} HTTP/1.1", r.verb, r.path);
                    for (n, v) in headers {
                        s.push_str(&format!("\r\n{n}: {v}"));
                    }
                    s
                } else {
                    resp.unwrap().raw.clone()
                };
                single(if info == WINHTTP_QUERY_RAW_HEADERS { raw.replace("\r\n", "\0") + "\0" } else { raw + "\r\n\r\n" })
            }
            _ => {
                let n = if info == WINHTTP_QUERY_CUSTOM {
                    custom.clone().ok_or(ERROR_INVALID_PARAMETER)?
                } else {
                    String::from(*QUERY_NAMES.get(info as usize).filter(|n| !n.is_empty()).ok_or(ERROR_WINHTTP_HEADER_NOT_FOUND)?)
                };
                headers
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case(&n))
                    .nth(wanted_index as usize)
                    .map(|(_, v)| v.clone())
                    .ok_or(ERROR_WINHTTP_HEADER_NOT_FOUND)
            }
        }
    });
    let text = match text {
        Ok(t) => t,
        Err(e) => return fail(e, 0),
    };
    let bump = || {
        if !index.is_null() {
            // SAFETY: the caller's index.
            unsafe { *index = wanted_index + 1 };
        }
    };
    // SAFETY (below): the caller's in/out length and buffer of that size.
    if flags & (WINHTTP_QUERY_FLAG_NUMBER | WINHTTP_QUERY_FLAG_NUMBER64 | WINHTTP_QUERY_FLAG_SYSTEMTIME) != 0 {
        let (bytes, size): ([u8; 16], usize) = if flags & WINHTTP_QUERY_FLAG_SYSTEMTIME != 0 {
            let Some(t) = http_date(&text) else { return fail(ERROR_WINHTTP_INVALID_SERVER_RESPONSE, 0) };
            let mut b = [0u8; 16];
            for (i, v) in t.iter().enumerate() {
                b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
            (b, 16)
        } else {
            let Ok(v) = text.trim().parse::<u64>() else { return fail(ERROR_WINHTTP_INVALID_SERVER_RESPONSE, 0) };
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&v.to_le_bytes());
            (b, if flags & WINHTTP_QUERY_FLAG_NUMBER64 != 0 { 8 } else { 4 })
        };
        unsafe {
            if buf.is_null() || (*len as usize) < size {
                *len = size as u32;
                return fail(ERROR_INSUFFICIENT_BUFFER, 0);
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, size);
            *len = size as u32;
        }
        bump();
        return 1;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let bytes = units.len() * 2;
    unsafe {
        if buf.is_null() || (*len as usize) < bytes + 2 {
            *len = (bytes + 2) as u32;
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
        *(buf.add(bytes) as *mut u16) = 0;
        *len = bytes as u32;
    }
    bump();
    1
}

extern "win64" fn WinHttpCloseHandle(h: u64) -> i32 {
    let Some(o) = HANDLES.lock().unwrap().remove(&h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    let mut hv = h;
    notify(h, &o, STATUS_HANDLE_CLOSING, &mut hv as *mut u64 as u64, 8);
    1
}

// ---- URLs --------------------------------------------------------------------------------

const ICU_DECODE: u32 = 0x1000_0000;
const ICU_ESCAPE: u32 = 0x8000_0000;

/// %XX sequences decoded.
fn unescape(s: &str) -> String {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// WinHttpCrackUrl: the components of an http(s) URL into URL_COMPONENTS.
/// A component with a length and no buffer is pointed at in the URL; one
/// with a buffer is copied (decoded with ICU_DECODE).
extern "win64" fn WinHttpCrackUrl(url: *const u16, len: u32, flags: u32, out: u64) -> i32 {
    if url.is_null() || out == 0 || flags & !(ICU_DECODE | ICU_ESCAPE | 0x0400_0000) != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's URL: `len` units, or NUL-terminated for 0.
    let units: Vec<u16> = unsafe {
        if len == 0 {
            let mut n = 0;
            while *url.add(n) != 0 {
                n += 1;
            }
            core::slice::from_raw_parts(url, n).to_vec()
        } else {
            core::slice::from_raw_parts(url, len as usize).to_vec()
        }
    };
    let s = String::from_utf16_lossy(&units);
    let Some(colon) = s.find("://") else { return fail(ERROR_WINHTTP_UNRECOGNIZED_SCHEME, 0) };
    let scheme = &s[..colon];
    let (n_scheme, default_port) = match scheme.to_ascii_lowercase().as_str() {
        "http" => (1u32, 80u16),
        "https" => (2, 443),
        _ => return fail(ERROR_WINHTTP_UNRECOGNIZED_SCHEME, 0),
    };
    let rest_at = colon + 3;
    let rest = &s[rest_at..];
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let (userinfo, hostport, host_at) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..], rest_at + i + 1),
        None => (None, authority, rest_at),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => match p.parse::<u16>() {
            Ok(v) => (h, v),
            Err(_) => return fail(ERROR_WINHTTP_INVALID_URL, 0),
        },
        _ => (hostport.trim_end_matches(':'), default_port),
    };
    if host.is_empty() {
        return fail(ERROR_WINHTTP_INVALID_URL, 0);
    }
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((a, b)) => (Some((a, rest_at)), Some((b, rest_at + a.len() + 1))),
            None => (Some((u, rest_at)), None),
        },
        None => (None, None),
    };
    let tail_at = rest_at + auth_end;
    let tail = &s[tail_at..];
    let extra_at = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, extra) = (&tail[..extra_at], &tail[extra_at..]);
    // Byte offsets in `s` to UTF-16 offsets in the caller's URL.
    let unit_at = |byte: usize| s[..byte].encode_utf16().count();
    let comps: [(usize, usize, Option<(&str, usize)>); 6] = [
        (8, 16, Some((scheme, 0))),
        (24, 32, Some((host, host_at))),
        (40, 48, user),
        (56, 64, password),
        (72, 80, Some((path, tail_at))),
        (88, 96, Some((extra, tail_at + extra_at))),
    ];
    // SAFETY: the caller's URL_COMPONENTS (104 bytes) and its buffers.
    unsafe {
        if teb::get::<u32>(out, 0) < 104 {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        teb::put(out, 20, n_scheme);
        teb::put(out, 36, port);
        for (ptr_at, len_at, value) in comps {
            let want: u32 = teb::get(out, len_at);
            let buf: u64 = teb::get(out, ptr_at);
            if want == 0 && buf == 0 {
                continue;
            }
            let Some((v, at)) = value else {
                teb::put(out, len_at, 0u32);
                if buf != 0 {
                    *(buf as *mut u16) = 0;
                }
                continue;
            };
            if buf == 0 {
                teb::put(out, ptr_at, url as u64 + unit_at(at) as u64 * 2);
                teb::put(out, len_at, v.encode_utf16().count() as u32);
            } else {
                let text = if flags & ICU_DECODE != 0 { unescape(v) } else { String::from(v) };
                let w: Vec<u16> = text.encode_utf16().collect();
                if w.len() >= want as usize {
                    teb::put(out, len_at, w.len() as u32 + 1);
                    return fail(ERROR_INSUFFICIENT_BUFFER, 0);
                }
                core::ptr::copy_nonoverlapping(w.as_ptr(), buf as *mut u16, w.len());
                *((buf as *mut u16).add(w.len())) = 0;
                teb::put(out, len_at, w.len() as u32);
            }
        }
    }
    1
}

// ---- proxies -----------------------------------------------------------------------------

/// A string in GlobalAlloc memory (the caller frees it with GlobalFree).
fn global_string(s: &str) -> u64 {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    kernel32::heap_alloc(w.len() * 2, false).map_or(0, |p| {
        // SAFETY: a fresh block of w.len() units.
        unsafe { core::ptr::copy_nonoverlapping(w.as_ptr(), p as *mut u16, w.len()) };
        p
    })
}

/// The current user's proxy settings: (auto-detect, auto-config URL,
/// proxy, bypass list), from Internet Settings.
fn ie_settings() -> (bool, Option<String>, Option<String>, Option<String>) {
    const HKCU: u64 = 0xFFFF_FFFF_8000_0001;
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings".encode_utf16().chain([0]).collect();
    let mut h = 0u64;
    if crate::registry::RegOpenKeyExW(HKCU, key.as_ptr(), 0, 0x2_0019, &mut h) != 0 {
        return (false, None, None, None);
    }
    let value = |name: &str| -> Option<(u32, Vec<u8>)> {
        let n: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let (mut kind, mut size) = (0u32, 0u32);
        if crate::registry::RegQueryValueExW(h, n.as_ptr(), core::ptr::null(), &mut kind, core::ptr::null_mut(), &mut size) != 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if crate::registry::RegQueryValueExW(h, n.as_ptr(), core::ptr::null(), &mut kind, data.as_mut_ptr(), &mut size) != 0 {
            return None;
        }
        data.truncate(size as usize);
        Some((kind, data))
    };
    let text = |name: &str| {
        value(name).map(|(_, d)| {
            let w: Vec<u16> = d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
            String::from_utf16_lossy(&w)
        }).filter(|s| !s.is_empty())
    };
    let enabled = value("ProxyEnable").is_some_and(|(_, d)| d.first().is_some_and(|&b| b != 0));
    // DefaultConnectionSettings: flags at byte 8, PROXY_TYPE_AUTO_DETECT 8.
    let detect = value("DefaultConnectionSettings").is_some_and(|(_, d)| d.get(8).is_some_and(|f| f & 0x8 != 0));
    let r = (detect, text("AutoConfigURL"), if enabled { text("ProxyServer") } else { None }, if enabled { text("ProxyOverride") } else { None });
    crate::registry::close_key(h);
    r
}

extern "win64" fn WinHttpGetIEProxyConfigForCurrentUser(out: u64) -> i32 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let (detect, script, proxy, bypass) = ie_settings();
    let s = |v: &Option<String>| v.as_deref().map_or(0, global_string);
    // SAFETY: the caller's WINHTTP_CURRENT_USER_IE_PROXY_CONFIG.
    unsafe {
        teb::put(out, 0, detect as i32);
        teb::put(out, 8, s(&script));
        teb::put(out, 16, s(&proxy));
        teb::put(out, 24, s(&bypass));
    }
    1
}

const WINHTTP_AUTOPROXY_AUTO_DETECT: u32 = 0x1;
const WINHTTP_AUTOPROXY_CONFIG_URL: u32 = 0x2;

/// A WINHTTP_PROXY_RESULT_ENTRY.
#[derive(Clone)]
struct ProxyEntry {
    proxy: Option<(u32, String, u16)>,
}

/// What auto-configuration decides for `url` under WINHTTP_AUTOPROXY_OPTIONS
/// (flags at 0, auto-detect flags at 4, config URL at 8). Finding a script
/// is real; running one is not possible without a JavaScript engine.
fn auto_proxy(options: u64) -> Result<Vec<ProxyEntry>, u32> {
    if options == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    // SAFETY: the caller's WINHTTP_AUTOPROXY_OPTIONS.
    let (flags, script) = unsafe { (teb::get::<u32>(options, 0), ws(teb::get::<u64>(options, 8) as *const u16)) };
    if flags & WINHTTP_AUTOPROXY_CONFIG_URL != 0 {
        let script = script.ok_or(ERROR_INVALID_PARAMETER)?;
        let (_, host, _, _) = split_url(&script).map_err(|_| ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT)?;
        if os_std::net::lookup_host(&host).map_or(true, |a| a.is_empty()) {
            return Err(ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT);
        }
        return Err(ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT);
    }
    if flags & WINHTTP_AUTOPROXY_AUTO_DETECT != 0 {
        // WPAD through DNS: the host "wpad".
        if os_std::net::lookup_host("wpad").map_or(true, |a| a.is_empty()) {
            return Err(ERROR_WINHTTP_AUTODETECTION_FAILED);
        }
        return Err(ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT);
    }
    Err(ERROR_INVALID_PARAMETER)
}

/// WinHttpGetProxyForUrl: WINHTTP_PROXY_INFO (access type, proxy, bypass).
extern "win64" fn WinHttpGetProxyForUrl(session: u64, url: *const u16, options: u64, out: u64) -> i32 {
    if get(session).is_none() {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    // SAFETY: NULL or the caller's URL.
    if unsafe { ws(url) }.is_none() || out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    match auto_proxy(options) {
        Ok(entries) => {
            let proxy = entries.iter().find_map(|e| e.proxy.as_ref().map(|(_, h, p)| format!("{h}:{p}")));
            // SAFETY: the caller's WINHTTP_PROXY_INFO.
            unsafe {
                teb::put(out, 0, if proxy.is_some() { 3u32 } else { 1 }); // NAMED_PROXY / NO_PROXY
                teb::put(out, 8, proxy.as_deref().map_or(0, global_string));
                teb::put(out, 16, 0u64);
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn WinHttpCreateProxyResolver(session: u64, out: *mut u64) -> u32 {
    let Some(s) = get(session) else { return ERROR_INVALID_HANDLE };
    if !matches!(s.kind, Kind::Session { flags, .. } if flags & WINHTTP_FLAG_ASYNC != 0) {
        return ERROR_WINHTTP_INCORRECT_HANDLE_TYPE;
    }
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = new_handle(Kind::Resolver(Mutex::new(None)), session) };
    0
}

/// WinHttpGetProxyForUrlEx: the decision is made on a worker thread and
/// reported through the resolver's callback; the result is read with
/// WinHttpGetProxyResult.
extern "win64" fn WinHttpGetProxyForUrlEx(resolver: u64, url: *const u16, options: u64, context: u64) -> u32 {
    let Some(o) = get(resolver) else { return ERROR_INVALID_HANDLE };
    if !matches!(o.kind, Kind::Resolver(_)) {
        return ERROR_WINHTTP_INCORRECT_HANDLE_TYPE;
    }
    // SAFETY: NULL or the caller's URL.
    if unsafe { ws(url) }.is_none() || options == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // The options are read now: the caller's structure need not outlive the call.
    // SAFETY: the caller's WINHTTP_AUTOPROXY_OPTIONS (32 bytes) and script URL.
    let (flags, script) = unsafe { (teb::get::<u32>(options, 0), ws(teb::get::<u64>(options, 8) as *const u16)) };
    o.context.store(context, Ordering::SeqCst);
    spawn(Box::new(move || {
        let script_w: Vec<u16> = script.unwrap_or_default().encode_utf16().chain([0]).collect();
        let mut copy = [0u8; 32];
        copy[..4].copy_from_slice(&flags.to_le_bytes());
        if script_w.len() > 1 {
            copy[8..16].copy_from_slice(&(script_w.as_ptr() as u64).to_le_bytes());
        }
        let result = auto_proxy(copy.as_ptr() as u64);
        if let Kind::Resolver(r) = &o.kind {
            *r.lock().unwrap() = result.as_ref().ok().cloned();
        }
        match result {
            Ok(_) => notify(resolver, &o, STATUS_GETPROXYFORURL_COMPLETE, 0, 0),
            Err(e) => notify_error(resolver, &o, API_GET_PROXY_FOR_URL, e),
        }
    }));
    ERROR_IO_PENDING
}

/// WINHTTP_PROXY_RESULT: the count and an array of 32-byte entries (fProxy,
/// fBypass, scheme, proxy name, port).
extern "win64" fn WinHttpGetProxyResult(resolver: u64, out: u64) -> u32 {
    let Some(o) = get(resolver) else { return ERROR_INVALID_HANDLE };
    let Kind::Resolver(r) = &o.kind else { return ERROR_WINHTTP_INCORRECT_HANDLE_TYPE };
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(entries) = r.lock().unwrap().clone() else { return ERROR_WINHTTP_INCORRECT_HANDLE_STATE };
    let Some(array) = kernel32::heap_alloc(entries.len().max(1) * 32, true) else { return ERROR_NOT_ENOUGH_MEMORY };
    // SAFETY: the fresh array; the caller's WINHTTP_PROXY_RESULT.
    unsafe {
        for (i, e) in entries.iter().enumerate() {
            let p = array + 32 * i as u64;
            if let Some((scheme, host, port)) = &e.proxy {
                teb::put(p, 0, 1i32);
                teb::put(p, 8, *scheme);
                teb::put(p, 16, global_string(host));
                teb::put(p, 24, *port);
            }
        }
        teb::put(out, 0, entries.len() as u32);
        teb::put(out, 8, array);
    }
    0
}

extern "win64" fn WinHttpFreeProxyResult(result: u64) {
    if result == 0 {
        return;
    }
    // SAFETY: a WINHTTP_PROXY_RESULT WinHttpGetProxyResult filled.
    unsafe {
        let (n, array): (u32, u64) = (teb::get(result, 0), teb::get(result, 8));
        if array != 0 {
            for i in 0..n as u64 {
                let name: u64 = teb::get(array + 32 * i, 16);
                if name != 0 {
                    kernel32::heap_free(name);
                }
            }
            kernel32::heap_free(array);
        }
        teb::put(result, 0, 0u32);
        teb::put(result, 8, 0u64);
    }
}

// ---- WinINet -------------------------------------------------------------------------

/// WinINet's last error is the thread's; its codes are WinHTTP's (both
/// number HTTP errors from 12000).
extern "win64" fn InternetOpenW(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> u64 {
    // INTERNET_OPEN_TYPE_PRECONFIG (0) and _DIRECT (1) connect directly;
    // _PROXY (3) uses the proxy given, as WINHTTP_ACCESS_TYPE_NAMED_PROXY.
    // INTERNET_FLAG_ASYNC is WINHTTP_FLAG_ASYNC; there is no WinINet
    // status callback here, so requests run synchronously.
    WinHttpOpen(agent, if access == 3 { 3 } else { 1 }, proxy, bypass, flags & !(WINHTTP_FLAG_ASYNC | 0x0100_0000))
}

extern "win64" fn InternetConnectW(session: u64, server: *const u16, port: u16, _user: *const u16, _password: *const u16, service: u32, _flags: u32, _context: u64) -> u64 {
    if service != 3 {
        return fail(ERROR_NOT_SUPPORTED, 0); // INTERNET_SERVICE_HTTP only (no FTP or Gopher)
    }
    WinHttpConnect(session, server, port, 0)
}

extern "win64" fn HttpOpenRequestW(connect: u64, verb: *const u16, object: *const u16, version: *const u16, referrer: *const u16, accept: *const u64, flags: u32, _context: u64) -> u64 {
    WinHttpOpenRequest(connect, verb, object, version, referrer, accept, flags & WINHTTP_FLAG_SECURE)
}

extern "win64" fn HttpAddRequestHeadersW(h: u64, headers: *const u16, len: u32, modifiers: u32) -> i32 {
    WinHttpAddRequestHeaders(h, headers, len, modifiers)
}

/// HttpSendRequestW sends the request and reads the response headers.
extern "win64" fn HttpSendRequestW(h: u64, headers: *const u16, hlen: u32, optional: *const u8, olen: u32) -> i32 {
    if WinHttpSendRequest(h, headers, hlen, optional, olen, olen, 0) == 0 {
        return 0;
    }
    WinHttpReceiveResponse(h, 0)
}

const HTTP_QUERY_CUSTOM: u32 = 65535;

/// HttpQueryInfoW: WinHTTP's query levels and flags; a custom header's
/// name comes in the buffer.
extern "win64" fn HttpQueryInfoW(h: u64, level: u32, buf: *mut u8, len: *mut u32, index: *mut u32) -> i32 {
    let name: Vec<u16> = if level & 0xFFFF == HTTP_QUERY_CUSTOM && !buf.is_null() {
        // SAFETY: the caller's NUL-terminated header name in the buffer.
        unsafe { ws(buf as *const u16) }.unwrap_or_default().encode_utf16().chain([0]).collect()
    } else {
        Vec::new()
    };
    let name_ptr = if name.is_empty() { core::ptr::null() } else { name.as_ptr() };
    WinHttpQueryHeaders(h, level, name_ptr, buf, len, index)
}

extern "win64" fn InternetReadFile(h: u64, buf: *mut u8, n: u32, read: *mut u32) -> i32 {
    if read.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    WinHttpReadData(h, buf, n, read)
}

/// The body bytes that can be read without waiting: those read ahead, or
/// (waiting for them) the next block; 0 at the end of the body.
extern "win64" fn InternetQueryDataAvailable(h: u64, avail: *mut u32, _flags: u32, _context: u64) -> i32 {
    let o = match request(h) {
        Ok(o) => o,
        Err(e) => return fail(e, 0),
    };
    let r = with_request(&o, |r| {
        if r.response.is_none() {
            return Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE);
        }
        if r.peeked.is_empty() {
            let mut block = [0u8; 16 * 1024];
            let n = read_body(r, &mut block)?;
            r.peeked.extend_from_slice(&block[..n]);
        }
        Ok(r.peeked.len() as u32)
    });
    match r {
        Ok(n) => {
            if !avail.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *avail = n };
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

/// InternetCrackUrlW: WinHttpCrackUrl, with WinINet's scheme numbers
/// (INTERNET_SCHEME_HTTP 3, _HTTPS 4).
extern "win64" fn InternetCrackUrlW(url: *const u16, len: u32, flags: u32, out: u64) -> i32 {
    if WinHttpCrackUrl(url, len, flags, out) == 0 {
        return 0;
    }
    // SAFETY: the caller's URL_COMPONENTSW, filled above.
    unsafe {
        let scheme: u32 = teb::get(out, 20);
        teb::put(out, 20, scheme + 2);
    }
    1
}

/// InternetCloseHandle of a session, connection or request.
pub(crate) fn close(h: u64) -> bool {
    get(h).is_some() && WinHttpCloseHandle(h) != 0
}

/// InternetSetOptionW on a handle: the timeouts (connect 2, send 5 and 7,
/// receive 6 and 8, in milliseconds). False if `h` is not a handle here.
pub(crate) fn set_option(h: u64, option: u32, value: u32) -> Option<bool> {
    let o = get(h)?;
    let slot = match option {
        2 => 1,
        5 | 7 => 2,
        6 | 8 => 3,
        _ => return Some(false),
    };
    o.timeouts.lock().unwrap()[slot] = value;
    Some(true)
}

pub fn wininet_exports() -> Vec<(&'static str, u64)> {
    export_table![
        InternetOpenW,
        InternetConnectW,
        HttpOpenRequestW,
        HttpAddRequestHeadersW,
        HttpSendRequestW,
        HttpQueryInfoW,
        InternetReadFile,
        InternetQueryDataAvailable,
        InternetCrackUrlW,
    ]
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        WinHttpOpen,
        WinHttpConnect,
        WinHttpOpenRequest,
        WinHttpAddRequestHeaders,
        WinHttpSetTimeouts,
        WinHttpSetStatusCallback,
        WinHttpSendRequest,
        WinHttpWriteData,
        WinHttpReceiveResponse,
        WinHttpReadData,
        WinHttpQueryHeaders,
        WinHttpCloseHandle,
        WinHttpCrackUrl,
        WinHttpGetIEProxyConfigForCurrentUser,
        WinHttpGetProxyForUrl,
        WinHttpCreateProxyResolver,
        WinHttpGetProxyForUrlEx,
        WinHttpGetProxyResult,
        WinHttpFreeProxyResult,
    ]
}
