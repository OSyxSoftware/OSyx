//! Winsock name-space services and socket duplication: the DNS name-space
//! provider (WSAEnumNameSpaceProviders, WSALookupService*), whose lookups
//! go to OSyx's resolver; WSADuplicateSocket within the process; and
//! WSAGetOverlappedResult.
use super::*;

const NS_DNS: u32 = 12;
const NS_ALL: u32 = 0;
/// The Windows TCP/IP DNS provider's id, {22059D40-7E9E-11CF-AE5A-00AA00A7112B}.
const DNS_PROVIDER_ID: [u8; 16] = [0x40, 0x9d, 0x05, 0x22, 0x9e, 0x7e, 0xcf, 0x11, 0xae, 0x5a, 0x00, 0xaa, 0x00, 0xa7, 0x11, 0x2b];
const WSA_E_NO_MORE: u32 = 10110;
const WSASERVICE_NOT_FOUND: u32 = 10108;

/// WSANAMESPACE_INFOW (40 bytes): provider id, name space, active,
/// version, identifier.
extern "win64" fn WSAEnumNameSpaceProvidersW(len: *mut u32, buf: *mut u8) -> i32 {
    if len.is_null() {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    let name: Vec<u16> = "Tcpip".encode_utf16().chain([0]).collect();
    let need = 40 + name.len() * 2;
    // SAFETY: the caller's in/out length and buffer of that size.
    unsafe {
        if buf.is_null() || (*len as usize) < need {
            *len = need as u32;
            return err(WSAEFAULT, SOCKET_ERROR);
        }
        let p = buf as u64;
        core::ptr::copy_nonoverlapping(DNS_PROVIDER_ID.as_ptr(), buf, 16);
        teb::put(p, 16, NS_DNS);
        teb::put(p, 20, 1i32);
        teb::put(p, 24, 1u32);
        teb::put(p, 32, p + 40);
        core::ptr::copy_nonoverlapping(name.as_ptr(), buf.add(40) as *mut u16, name.len());
    }
    1
}

/// SVCID_HOSTNAME {0002A800-0000-0000-C000-000000000046}: the local host
/// name; SVCID_INET_HOSTADDRBYNAME (0002A803-...): a name's addresses.
const SVCID_HOSTNAME: [u8; 16] = [0x00, 0xa8, 0x02, 0x00, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const SVCID_INET_HOSTADDRBYNAME: [u8; 16] = [0x03, 0xa8, 0x02, 0x00, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

const LUP_RETURN_NAME: u32 = 0x10;
const LUP_RETURN_ADDR: u32 = 0x100;
const LUP_FLUSHPREVIOUS: u32 = 0x1000;

/// A lookup under way: the name found, its addresses, the flags, and
/// whether its one result was returned.
struct Lookup {
    name: String,
    addrs: Vec<[u8; 4]>,
    flags: u32,
    done: bool,
}

static LOOKUPS: Mutex<BTreeMap<u64, Lookup>> = Mutex::new(BTreeMap::new());
static NEXT_LOOKUP: AtomicU32 = AtomicU32::new(0x5100);

/// WSALookupServiceBeginW with a WSAQUERYSETW: lpszServiceInstanceName at
/// 8, lpServiceClassId at 16, dwNameSpace at 40.
extern "win64" fn WSALookupServiceBeginW(query: u64, flags: u32, out: *mut u64) -> i32 {
    if !started() {
        return err(WSANOTINITIALISED, SOCKET_ERROR);
    }
    if query == 0 || out.is_null() {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    // SAFETY: the caller's WSAQUERYSETW, its name and class id.
    let (name, class, ns) = unsafe {
        let name = crate::path::wide_to_string(teb::get::<u64>(query, 8) as *const u16);
        let class_p: u64 = teb::get(query, 16);
        let class = if class_p == 0 { None } else { Some(teb::get::<[u8; 16]>(class_p, 0)) };
        (name, class, teb::get::<u32>(query, 40))
    };
    if ns != NS_DNS && ns != NS_ALL {
        return err(WSASERVICE_NOT_FOUND, SOCKET_ERROR);
    }
    let (name, addrs) = match (class, name) {
        (Some(SVCID_HOSTNAME), None) => (host_name(), Vec::new()),
        (Some(SVCID_HOSTNAME | SVCID_INET_HOSTADDRBYNAME), Some(n)) | (None, Some(n)) => match resolve(&n) {
            Ok(a) => (n, a),
            Err(e) => return err(e, SOCKET_ERROR),
        },
        _ => return err(WSAEINVAL, SOCKET_ERROR),
    };
    let h = NEXT_LOOKUP.fetch_add(4, Ordering::Relaxed) as u64;
    LOOKUPS.lock().unwrap().insert(h, Lookup { name, addrs, flags, done: false });
    // SAFETY: the caller's out handle.
    unsafe { *out = h };
    0
}

/// The one result of a lookup: a WSAQUERYSETW (120 bytes) with the name
/// (LUP_RETURN_NAME) and a CSADDR_INFO (48 bytes) and SOCKADDR_IN pair per
/// address (LUP_RETURN_ADDR), all in the caller's buffer.
extern "win64" fn WSALookupServiceNextW(h: u64, flags: u32, len: *mut u32, out: *mut u8) -> i32 {
    if len.is_null() {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    let mut lookups = LOOKUPS.lock().unwrap();
    let Some(l) = lookups.get_mut(&h) else { return err(WSA_INVALID_HANDLE, SOCKET_ERROR) };
    if l.done {
        return err(WSA_E_NO_MORE, SOCKET_ERROR);
    }
    let want = if flags == 0 { l.flags } else { flags };
    let name: Vec<u16> = l.name.encode_utf16().chain([0]).collect();
    let addrs: &[[u8; 4]] = if want & LUP_RETURN_ADDR != 0 { &l.addrs } else { &[] };
    let name_bytes = if want & LUP_RETURN_NAME != 0 { name.len() * 2 } else { 0 };
    let csaddr_at = 120usize;
    let sockaddr_at = csaddr_at + 48 * addrs.len();
    let name_at = sockaddr_at + 16 * addrs.len();
    let need = name_at + name_bytes;
    // SAFETY: the caller's in/out length and buffer of that size.
    unsafe {
        if out.is_null() || (*len as usize) < need {
            *len = need as u32;
            if flags & LUP_FLUSHPREVIOUS != 0 {
                l.done = true;
            }
            return err(WSAEFAULT, SOCKET_ERROR);
        }
        core::ptr::write_bytes(out, 0, need);
        let p = out as u64;
        teb::put(p, 0, 120u32);
        teb::put(p, 40, NS_DNS);
        if name_bytes != 0 {
            core::ptr::copy_nonoverlapping(name.as_ptr(), out.add(name_at) as *mut u16, name.len());
            teb::put(p, 8, p + name_at as u64);
        }
        teb::put(p, 88, addrs.len() as u32);
        if !addrs.is_empty() {
            teb::put(p, 96, p + csaddr_at as u64);
        }
        for (i, a) in addrs.iter().enumerate() {
            let sa = p + (sockaddr_at + 16 * i) as u64;
            teb::put(sa, 0, AF_INET);
            teb::put(sa, 4, *a);
            // CSADDR_INFO: LocalAddr {lpSockaddr, iSockaddrLength},
            // RemoteAddr, iSocketType, iProtocol.
            let cs = p + (csaddr_at + 48 * i) as u64;
            teb::put(cs, 16, sa);
            teb::put(cs, 24, 16i32);
            teb::put(cs, 32, SOCK_STREAM);
            teb::put(cs, 36, 6i32);
        }
    }
    l.done = true;
    0
}

extern "win64" fn WSALookupServiceEnd(h: u64) -> i32 {
    match LOOKUPS.lock().unwrap().remove(&h) {
        Some(_) => 0,
        None => err(WSA_INVALID_HANDLE, SOCKET_ERROR),
    }
}

/// Registering a service: the DNS name space is read-only without a
/// dynamic-update server, as for Windows' own DNS provider.
extern "win64" fn WSASetServiceW(info: u64, _op: u32, _flags: u32) -> i32 {
    if info == 0 {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    err(WSAEINVAL, SOCKET_ERROR)
}

/// Sockets duplicated by WSADuplicateSocketW and not yet opened, by the
/// token in WSAPROTOCOL_INFOW.dwProviderReserved.
static DUPLICATES: Mutex<BTreeMap<u32, Sock>> = Mutex::new(BTreeMap::new());
static NEXT_TOKEN: AtomicU32 = AtomicU32::new(1);
/// How many socket handles share each kernel socket.
static FD_REFS: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());
const PROVIDER_RESERVED: u64 = 112;

/// Releases one handle's share of kernel socket `fd`: true when it was
/// the last and the socket is to be closed.
pub(super) fn release_fd(fd: u64) -> bool {
    let mut refs = FD_REFS.lock().unwrap();
    match refs.get_mut(&fd) {
        Some(n) if *n > 1 => {
            *n -= 1;
            false
        }
        _ => {
            refs.remove(&fd);
            true
        }
    }
}

/// WSADuplicateSocketW: a WSAPROTOCOL_INFOW from which WSASocketW opens
/// another handle on the same socket. OSyx sockets are handles of the
/// process that made them, so the duplicate is for this process.
extern "win64" fn WSADuplicateSocketW(s: u64, pid: u32, info: u64) -> i32 {
    let k = match socket_state(s) {
        Ok(k) => k,
        Err(e) => return err(e, SOCKET_ERROR),
    };
    if info == 0 {
        return err(WSAEFAULT, SOCKET_ERROR);
    }
    if pid as u64 != crate::kernel32::pid() {
        return err(WSAEINVAL, SOCKET_ERROR);
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let (kind, proto) = if k.stream { (SOCK_STREAM, 6) } else { (SOCK_DGRAM, 17) };
    protocol_info(info, kind, proto);
    // SAFETY: the caller's 628-byte WSAPROTOCOL_INFOW.
    unsafe { teb::put(info, PROVIDER_RESERVED as usize, token) };
    // The duplicate holds the kernel socket from now on: closing the
    // source before WSASocketW opens it must not close the descriptor.
    *FD_REFS.lock().unwrap().entry(k.fd).or_insert(1) += 1;
    DUPLICATES.lock().unwrap().insert(token, k);
    0
}

/// WSASocketW with a WSAPROTOCOL_INFOW: a duplicate's new handle, or a
/// new socket of the catalog entry the structure describes.
pub(super) fn socket_from_info(info: *const u8) -> u64 {
    let p = info as u64;
    // SAFETY: the caller's WSAPROTOCOL_INFOW.
    let (id, token, af, kind, proto): ([u8; 16], u32, i32, i32, i32) =
        unsafe { (teb::get(p, 20), teb::get(p, PROVIDER_RESERVED as usize), teb::get(p, 76), teb::get(p, 88), teb::get(p, 92)) };
    if id != PROVIDER_ID {
        return err(WSAEINVAL, INVALID_SOCKET);
    }
    if token == 0 {
        return socket(af, kind, proto);
    }
    // The reference WSADuplicateSocketW took becomes this handle's.
    let Some(k) = DUPLICATES.lock().unwrap().remove(&token) else { return err(WSAEINVAL, INVALID_SOCKET) };
    let mut n = NEXT.lock().unwrap();
    let s = *n;
    *n += 4;
    SOCKETS.lock().unwrap().insert(s, k);
    s
}

/// Overlapped socket operations complete in the call, so their OVERLAPPED
/// already holds the result; a pending one (from a helper thread) is
/// waited for through its event.
extern "win64" fn WSAGetOverlappedResult(s: u64, ov: *const u8, transfer: *mut u32, wait: i32, flags: *mut u32) -> i32 {
    if let Err(e) = socket_state(s) {
        return err(e, 0);
    }
    if ov.is_null() || transfer.is_null() || flags.is_null() {
        return err(WSAEFAULT, 0);
    }
    use crate::kernel32::overlapped;
    const STATUS_PENDING: u64 = 0x103;
    // SAFETY (all): the caller's OVERLAPPED.
    let status = || unsafe { overlapped::status(ov as u64) };
    while status() == STATUS_PENDING {
        if wait == 0 {
            return err(WSA_IO_INCOMPLETE, 0);
        }
        match unsafe { overlapped::event(ov as u64) } & !1 {
            0 => os_std::thread::sleep(core::time::Duration::from_millis(1)),
            event => {
                let _ = crate::objects::wait(&[event], false, 10);
            }
        }
    }
    // SAFETY: as above; the caller's out pointers.
    unsafe {
        *transfer = overlapped::bytes(ov as u64) as u32;
        *flags = 0;
    }
    match status() {
        0 => 1,
        s => err(super::overlapped::error_of(s), 0),
    }
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        WSAEnumNameSpaceProvidersW,
        WSALookupServiceBeginW,
        WSALookupServiceNextW,
        WSALookupServiceEnd,
        WSASetServiceW,
        WSADuplicateSocketW,
        WSAGetOverlappedResult,
    ]
}
