//! IPv4 adapter data and synchronous ICMP echo over OSyx's native stack.
//! Layouts follow the Windows x64 public IP Helper structures.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{fail, out_a, out_w},
    teb,
};
use alloc::{collections::BTreeMap, format, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use os_std::{
    net::{self, Ipv4Addr, PingSocket},
    sync::Mutex,
    time::{Duration, Instant},
};

fn ip(a: [u8; 4]) -> alloc::string::String {
    format!("{}", Ipv4Addr(a))
}
fn mask(prefix: u8) -> [u8; 4] {
    (u32::MAX.checked_shl(32 - prefix.min(32) as u32).unwrap_or(0)).to_be_bytes()
}
fn list() -> Result<Vec<net::NetIface>, u32> {
    net::interfaces().map_err(|_| ERROR_GEN_FAILURE)
}
// IP_ADDR_STRING (48 bytes): pointer, address[16], mask[16], context.
fn addr_string(p: u64, addr: [u8; 4], prefix: u8) {
    out_a(&ip(addr), (p + 8) as *mut u8, 16);
    out_a(&ip(mask(prefix)), (p + 24) as *mut u8, 16);
}
/// Whether a non-loopback adapter has an address (WinINet's notion of a
/// connection).
pub(crate) fn has_connectivity() -> bool {
    list().is_ok_and(|v| v.iter().any(|i| i.flags & net::IFF_LOOPBACK == 0 && i.addr != [0; 4]))
}

extern "win64" fn GetAdaptersInfo(out: u64, len: *mut u32) -> u32 {
    if len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let ifs: Vec<_> = ifs.into_iter().filter(|i| i.flags & net::IFF_LOOPBACK == 0).collect();
    if ifs.is_empty() {
        return 232;
    } // ERROR_NO_DATA
    let need = ifs.len() as u32 * 704;
    let cap = unsafe {
        let n = *len;
        *len = need;
        n
    };
    if out == 0 || cap < need {
        return 111;
    } // ERROR_BUFFER_OVERFLOW
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, need as usize);
    }
    for (k, i) in ifs.iter().enumerate() {
        let p = out + k as u64 * 704;
        unsafe {
            if k + 1 < ifs.len() {
                teb::put(p, 0, p + 704);
            }
            teb::put(p, 8, i.index as u32);
            teb::put(p, 404, 6u32);
            core::ptr::copy_nonoverlapping(i.mac.as_ptr(), (p + 408) as *mut u8, 6);
            teb::put(p, 416, i.index as u32);
            teb::put(p, 420, if i.flags & net::IFF_WIRELESS != 0 { 71u32 } else { 6u32 });
            teb::put(p, 424, u32::from(i.method == net::CONFIG_DHCP));
        }
        out_a(net::field(&i.name), (p + 12) as *mut u8, 260);
        out_a(&format!("OSyx {}", net::field(&i.name)), (p + 272) as *mut u8, 132);
        addr_string(p + 440, i.addr, i.prefix);
        addr_string(p + 488, i.gateway, 0);
        // OSyx does not expose DHCP server identity or lease start time.
    }
    0
}
extern "win64" fn GetIfTable(out: u64, len: *mut u32, ordered: i32) -> u32 {
    if len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    if ordered != 0 {
        ifs.sort_by_key(|i| i.index);
    }
    let need = 4 + ifs.len() as u32 * 860;
    let cap = unsafe {
        let n = *len;
        *len = need;
        n
    };
    if out == 0 || cap < need {
        return ERROR_INSUFFICIENT_BUFFER;
    }
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, need as usize);
        teb::put(out, 0, ifs.len() as u32);
    }
    for (k, i) in ifs.iter().enumerate() {
        let p = out + 4 + k as u64 * 860;
        let loopback = i.flags & net::IFF_LOOPBACK != 0;
        let name = net::field(&i.name);
        out_w(name, p as *mut u16, 256);
        unsafe {
            teb::put(p, 512, i.index as u32);
            teb::put(p, 516, if loopback { 24u32 } else if i.flags & net::IFF_WIRELESS != 0 { 71u32 } else { 6u32 });
            teb::put(p, 520, i.mtu);
            // Link speed is not provided by OSyx: leave it unknown (zero).
            if !loopback {
                teb::put(p, 528, 6u32);
                core::ptr::copy_nonoverlapping(i.mac.as_ptr(), (p + 532) as *mut u8, 6);
            }
            teb::put(p, 540, if i.flags & net::IFF_UP != 0 { 1u32 } else { 2u32 });
            teb::put(p, 544, if i.flags & net::IFF_LINK != 0 { 5u32 } else { 0u32 });
            for (off, n) in [
                (552, i.rx_bytes),
                (556, i.rx_packets),
                (564, i.rx_dropped),
                (568, i.rx_errors),
                (576, i.tx_bytes),
                (580, i.tx_packets),
                (588, i.tx_dropped),
            ] {
                teb::put(p, off, n as u32);
            }
            teb::put(p, 600, name.len() as u32);
            core::ptr::copy_nonoverlapping(name.as_ptr(), (p + 604) as *mut u8, name.len());
        }
    }
    0
}
extern "win64" fn GetNetworkParams(out: u64, len: *mut u32) -> u32 {
    if len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let servers = match net::dns_servers() {
        Ok(v) => v,
        Err(_) => return ERROR_GEN_FAILURE,
    };
    let need = 600 + servers.len().saturating_sub(1) as u32 * 48;
    let cap = unsafe {
        let n = *len;
        *len = need;
        n
    };
    if out == 0 || cap < need {
        return 111;
    }
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, need as usize);
    }
    let host = os_std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    out_a(host.trim(), out as *mut u8, 132);
    for (k, server) in servers.iter().enumerate() {
        let p = if k == 0 { out + 272 } else { out + 600 + (k - 1) as u64 * 48 };
        addr_string(p, server.0, 0);
        if k + 1 < servers.len() {
            unsafe {
                teb::put(p, 0, out + 600 + k as u64 * 48);
            }
        }
    }
    unsafe {
        teb::put(out, 320, 8u32);
        teb::put(out, 592, 1u32);
    }
    0
}

static PINGS: Mutex<BTreeMap<u64, Arc<Mutex<PingSocket>>>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x6000_0000);
static SEQ: AtomicU32 = AtomicU32::new(0);
extern "win64" fn IcmpCreateFile() -> u64 {
    match PingSocket::new() {
        Ok(s) => {
            let h = NEXT.fetch_add(4, Ordering::Relaxed);
            PINGS.lock().unwrap().insert(h, Arc::new(Mutex::new(s)));
            h
        }
        Err(_) => fail(ERROR_GEN_FAILURE, u64::MAX),
    }
}
extern "win64" fn IcmpCloseHandle(h: u64) -> i32 {
    if PINGS.lock().unwrap().remove(&h).is_some() {
        1
    } else {
        fail(ERROR_INVALID_HANDLE, 0)
    }
}
extern "win64" fn IcmpSendEcho(
    h: u64,
    dest: u32,
    request: *const u8,
    size: u16,
    options: u64,
    reply: u64,
    reply_size: u32,
    timeout: u32,
) -> u32 {
    if options != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    if reply == 0 || reply_size < 40 + size as u32 || (size != 0 && request.is_null()) || size > 65000 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(sock) = PINGS.lock().unwrap().get(&h).cloned() else {
        return fail(ERROR_INVALID_HANDLE, 0);
    };
    let sock = sock.lock().unwrap();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) as u16;
    let data = if size == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(request, size as usize) } };
    let start = Instant::now();
    if sock.send(Ipv4Addr(dest.to_ne_bytes()), seq, data).is_err() {
        return fail(ERROR_GEN_FAILURE, 0);
    }
    let mut buf = vec![0u8; 65536];
    loop {
        let left = Duration::from_millis(timeout as u64).saturating_sub(start.elapsed());
        if left.is_zero() {
            return fail(11010, 0);
        } // IP_REQ_TIMED_OUT
        if sock.set_read_timeout(Some(left)).is_err() {
            return fail(ERROR_GEN_FAILURE, 0);
        }
        let received = match sock.recv(&mut buf) {
            Ok(r) => r,
            Err(_) => return fail(11010, 0),
        };
        if received.seq != seq || received.from.0 != dest.to_ne_bytes() {
            continue;
        }
        if reply_size < 40 + received.len as u32 {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        unsafe {
            core::ptr::write_bytes(reply as *mut u8, 0, 40);
            teb::put(reply, 0, dest);
            teb::put(reply, 8, start.elapsed().as_millis().min(u32::MAX as u128) as u32);
            teb::put(reply, 12, received.len as u16);
            teb::put(reply, 16, reply + 40);
            teb::put(reply, 24, received.ttl);
            core::ptr::copy_nonoverlapping(buf.as_ptr(), (reply + 40) as *mut u8, received.len);
        }
        return 1;
    }
}
// ---- interface change notifications -----------------------------------------

/// IP interface, address and route registrations: (handle, callback,
/// context). OSyx's interfaces are fixed for the life of the boot (DHCP
/// configures the one adapter once), so a registered callback is never
/// called. Windows sends one initial notification when the caller asks for
/// it; OSyx does not, so a program that needs the current state has to query
/// it (GetAdaptersInfo/GetIfTable), not wait for the callback.
static CHANGES: Mutex<Vec<(u64, u64, u64)>> = Mutex::new(Vec::new());
static NEXT_CHANGE: AtomicU64 = AtomicU64::new(1);

extern "win64" fn NotifyIpInterfaceChange(family: u16, callback: u64, context: u64, initial: u8, out: *mut u64) -> u32 {
    register_change(family, callback, context, initial, out)
}

extern "win64" fn NotifyUnicastIpAddressChange(family: u16, callback: u64, context: u64, initial: u8, out: *mut u64) -> u32 {
    register_change(family, callback, context, initial, out)
}

extern "win64" fn NotifyRouteChange2(_family: u16, callback: u64, context: u64, initial: u8, out: *mut u64) -> u32 {
    register_change(0, callback, context, initial, out)
}

fn register_change(_family: u16, callback: u64, context: u64, _initial: u8, out: *mut u64) -> u32 {
    if callback == 0 || out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let h = NEXT_CHANGE.fetch_add(1, Ordering::SeqCst);
    CHANGES.lock().unwrap().push((h, callback, context));
    // SAFETY: the caller's out pointer.
    unsafe { *out = h };
    0 // NO_ERROR
}

extern "win64" fn CancelMibChangeNotify2(h: u64) -> u32 {
    let removed = {
        let mut c = CHANGES.lock().unwrap();
        c.iter().position(|(id, _, _)| *id == h).map(|i| c.remove(i))
    };
    if removed.is_some() {
        0 // NO_ERROR
    } else {
        ERROR_INVALID_PARAMETER
    }
}

struct Layout {
    bytes: Vec<u8>,
    fixups: Vec<(usize, usize)>,
}

impl Layout {
    fn new() -> Self {
        Self { bytes: Vec::new(), fixups: Vec::new() }
    }
    fn reserve(&mut self, n: usize) -> usize {
        while !self.bytes.len().is_multiple_of(8) {
            self.bytes.push(0);
        }
        let at = self.bytes.len();
        self.bytes.resize(at + n, 0);
        at
    }
    fn put16(&mut self, at: usize, v: u16) {
        self.bytes[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(&mut self, at: usize, v: u32) {
        self.bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(&mut self, at: usize, v: u64) {
        self.bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn link(&mut self, at: usize, target: usize) {
        self.fixups.push((at, target));
    }
    fn cstr(&mut self, at: usize, s: &str) {
        let dst = self.reserve(s.len() + 1);
        self.bytes[dst..dst + s.len()].copy_from_slice(s.as_bytes());
        self.link(at, dst);
    }
    fn wstr(&mut self, at: usize, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).collect();
        let dst = self.reserve(units.len() * 2);
        for (k, u) in units.into_iter().enumerate() {
            self.put16(dst + k * 2, u);
        }
        self.link(at, dst);
    }
    fn sockaddr_in(&mut self, addr_field: usize, ip: [u8; 4]) {
        let sa = self.reserve(16);
        self.put16(sa, 2); // AF_INET
        self.bytes[sa + 4..sa + 8].copy_from_slice(&ip);
        self.link(addr_field, sa);
        self.put32(addr_field + 8, 16);
    }
    fn finish(mut self, base: u64) -> Vec<u8> {
        for (at, target) in self.fixups {
            self.bytes[at..at + 8].copy_from_slice(&(base + target as u64).to_le_bytes());
        }
        self.bytes
    }
}

fn iface_type(flags: u16) -> u32 {
    if flags & net::IFF_LOOPBACK != 0 {
        24 // IF_TYPE_SOFTWARE_LOOPBACK
    } else if flags & net::IFF_WIRELESS != 0 {
        71 // IF_TYPE_IEEE80211
    } else {
        6 // IF_TYPE_ETHERNET_CSMACD
    }
}

fn iface_luid(flags: u16, index: u8) -> u64 {
    ((iface_type(flags) as u64) << 48) | ((index as u64 & 0xff_ffff) << 24)
}

/// IP_ADAPTER_ADDRESSES_LH (448 bytes) and its linked address lists.
extern "win64" fn GetAdaptersAddresses(family: u32, flags: u32, reserved: u64, out: u64, size: *mut u32) -> u32 {
    if size.is_null() || reserved != 0 || !matches!(family, 0 | 2 | 23) {
        return ERROR_INVALID_PARAMETER;
    }
    if family == 23 {
        unsafe { *size = 0 };
        return 232; // ERROR_NO_DATA: OSyx is IPv4-only
    }
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    if ifs.is_empty() {
        unsafe { *size = 0 };
        return 232;
    }
    let dns = net::dns_servers().unwrap_or_default();
    let mut b = Layout::new();
    let mut heads = Vec::with_capacity(ifs.len());
    for _ in &ifs {
        heads.push(b.reserve(448));
    }
    for (k, i) in ifs.iter().enumerate() {
        let a = heads[k];
        if k + 1 < heads.len() {
            b.link(a + 8, heads[k + 1]);
        }
        let loopback = i.flags & net::IFF_LOOPBACK != 0;
        let name = net::field(&i.name);
        let desc = format!("OSyx {name}");
        let if_type = iface_type(i.flags);
        let up = i.flags & net::IFF_UP != 0 && (loopback || i.flags & net::IFF_LINK != 0);
        b.put32(a, 448);
        b.put32(a + 4, i.index as u32);
        b.cstr(a + 16, name);
        b.wstr(a + 56, "");
        b.wstr(a + 64, &desc);
        b.wstr(a + 72, name);
        if !loopback {
            b.bytes[a + 80..a + 86].copy_from_slice(&i.mac);
            b.put32(a + 88, 6);
        }
        let mut aflags = 0x80u32; // IP_ADAPTER_IPV4_ENABLED
        if i.method == net::CONFIG_DHCP {
            aflags |= 0x04; // IP_ADAPTER_DHCP_ENABLED
        }
        b.put32(a + 92, aflags);
        b.put32(a + 96, i.mtu);
        b.put32(a + 100, if_type);
        b.put32(a + 104, if up { 1 } else { 2 }); // IfOperStatusUp / IfOperStatusDown
        b.put32(a + 216, 1); // Ipv4Metric
        b.put64(a + 224, iface_luid(i.flags, i.index));
        b.put32(a + 240, 1); // CompartmentId
        b.put32(a + 260, 1); // NET_IF_CONNECTION_DEDICATED

        // GAA_FLAG_SKIP_UNICAST = 0x0001
        if flags & 0x0001 == 0 && i.addr != [0, 0, 0, 0] {
            let u = b.reserve(64);
            b.link(a + 24, u);
            b.put32(u, 64);
            b.sockaddr_in(u + 16, i.addr);
            let origin = if i.method == net::CONFIG_DHCP { 3 } else { 1 };
            b.put32(u + 32, origin);
            b.put32(u + 36, origin);
            b.put32(u + 40, 4); // IpDadStatePreferred
            b.put32(u + 44, u32::MAX);
            b.put32(u + 48, u32::MAX);
            b.put32(u + 52, u32::MAX);
            b.bytes[u + 56] = i.prefix;
        }
        // GAA_FLAG_SKIP_DNS_SERVER = 0x0008
        if flags & 0x0008 == 0 && !loopback && !dns.is_empty() {
            let mut prev = None;
            for srv in &dns {
                let d = b.reserve(32);
                match prev {
                    None => b.link(a + 48, d),
                    Some(p) => b.link(p + 8, d),
                }
                b.put32(d, 32);
                b.sockaddr_in(d + 16, srv.0);
                prev = Some(d);
            }
        }
        // GAA_FLAG_INCLUDE_PREFIX = 0x0010
        if flags & 0x0010 != 0 && i.addr != [0, 0, 0, 0] {
            let p = b.reserve(40);
            b.link(a + 176, p);
            b.put32(p, 40);
            let m = mask(i.prefix);
            let net_ip = [i.addr[0] & m[0], i.addr[1] & m[1], i.addr[2] & m[2], i.addr[3] & m[3]];
            b.sockaddr_in(p + 16, net_ip);
            b.put32(p + 32, i.prefix as u32);
        }
        // GAA_FLAG_INCLUDE_GATEWAYS = 0x0080
        if flags & 0x0080 != 0 && !loopback && i.gateway != [0, 0, 0, 0] {
            let g = b.reserve(32);
            b.link(a + 208, g);
            b.put32(g, 32);
            b.sockaddr_in(g + 16, i.gateway);
        }
    }
    let need = b.bytes.len() as u32;
    let cap = unsafe {
        let n = *size;
        *size = need;
        n
    };
    if out == 0 || cap < need {
        return 111; // ERROR_BUFFER_OVERFLOW
    }
    let bytes = b.finish(out);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
    }
    0
}

/// MIB_IPINTERFACE_TABLE: NumEntries (4) + 4 pad + N * MIB_IPINTERFACE_ROW (168).
extern "win64" fn GetIpInterfaceTable(family: u16, table: *mut u64) -> u32 {
    if table.is_null() || !matches!(family, 0 | 2 | 23) {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs = if family == 23 {
        Vec::new()
    } else {
        match list() {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
    let total = 8 + ifs.len() * 168;
    let Some(base) = crate::kernel32::heap_alloc(total, true) else {
        return ERROR_NOT_ENOUGH_MEMORY;
    };
    unsafe {
        teb::put(base, 0, ifs.len() as u32);
        for (k, i) in ifs.iter().enumerate() {
            let row = base + 8 + k as u64 * 168;
            teb::put(row, 0, 2u16); // AF_INET
            teb::put(row, 8, iface_luid(i.flags, i.index));
            teb::put(row, 16, i.index as u32);
            teb::put(row, 20, 65535u32); // MaxReassemblySize
            teb::put(row, 53, u8::from(i.flags & net::IFF_UP != 0)); // Connected
            teb::put(row, 108, i.mtu);
            teb::put(row, 112, 1u32); // Metric
            teb::put(row, 136, i.prefix); // SitePrefixLength
        }
        *table = base;
    }
    0
}

extern "win64" fn FreeMibTable(memory: u64) {
    if memory != 0 {
        crate::kernel32::heap_free(memory);
    }
}

extern "win64" fn GetBestInterfaceEx(dest: *const u8, index: *mut u32) -> u32 {
    if dest.is_null() || index.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let family = unsafe { *(dest as *const u16) };
    if family == 23 {
        return 1231; // ERROR_NETWORK_UNREACHABLE: no IPv6 routes
    }
    if family != 2 {
        return ERROR_INVALID_PARAMETER;
    }
    let target = unsafe { [*(dest.add(4)), *(dest.add(5)), *(dest.add(6)), *(dest.add(7))] };
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let chosen = ifs
        .iter()
        .find(|i| i.addr == target || (target[0] == 127 && i.flags & net::IFF_LOOPBACK != 0))
        .or_else(|| {
            ifs.iter().find(|i| {
                let m = mask(i.prefix);
                i.addr != [0, 0, 0, 0]
                    && (0..4).all(|k| (i.addr[k] & m[k]) == (target[k] & m[k]))
            })
        })
        .or_else(|| ifs.iter().find(|i| i.flags & net::IFF_LOOPBACK == 0 && i.gateway != [0, 0, 0, 0]))
        .or_else(|| ifs.first());
    match chosen {
        Some(i) => {
            unsafe { *index = i.index as u32 };
            0
        }
        None => 1231, // ERROR_NETWORK_UNREACHABLE
    }
}

extern "win64" fn if_indextoname(index: u32, name: *mut u8) -> *mut u8 {
    if name.is_null() {
        return core::ptr::null_mut();
    }
    let Ok(ifs) = list() else {
        return core::ptr::null_mut();
    };
    let Some(i) = ifs.iter().find(|i| i.index as u32 == index) else {
        return core::ptr::null_mut();
    };
    out_a(net::field(&i.name), name, 256);
    name
}

/// if_nametoindex(name): the interface index, or 0.
extern "win64" fn if_nametoindex(name: *const u8) -> u32 {
    if name.is_null() {
        return 0;
    }
    // SAFETY: a NUL-terminated name.
    let wanted = unsafe { core::ffi::CStr::from_ptr(name as *const core::ffi::c_char) }.to_bytes();
    list()
        .ok()
        .and_then(|v| v.into_iter().find(|i| net::field(&i.name).as_bytes() == wanted))
        .map_or(0, |i| i.index as u32)
}

/// GetBestInterface: GetBestInterfaceEx for an IPv4 address (network
/// byte order).
extern "win64" fn GetBestInterface(dest: u32, index: *mut u32) -> u32 {
    let mut sa = [0u8; 16];
    sa[0] = 2; // AF_INET
    sa[4..8].copy_from_slice(&dest.to_ne_bytes());
    GetBestInterfaceEx(sa.as_ptr(), index)
}

/// An interface's GUID. OSyx keeps no adapter registry, so the GUID is
/// made from the index: {6f737978-0000-4000-8000-0000000000ii}.
fn iface_guid(index: u8) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[..4].copy_from_slice(&0x6f73_7978u32.to_le_bytes());
    g[6..8].copy_from_slice(&0x4000u16.to_le_bytes());
    g[8] = 0x80;
    g[15] = index;
    g
}

extern "win64" fn ConvertInterfaceGuidToLuid(guid: *const [u8; 16], luid: *mut u64) -> u32 {
    if guid.is_null() || luid.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's GUID.
    let want = unsafe { *guid };
    match list().ok().and_then(|v| v.into_iter().find(|i| iface_guid(i.index) == want)) {
        Some(i) => {
            // SAFETY: the caller's out pointer.
            unsafe { *luid = iface_luid(i.flags, i.index) };
            0
        }
        None => ERROR_INVALID_PARAMETER,
    }
}

extern "win64" fn ConvertInterfaceLuidToGuid(luid: *const u64, guid: *mut [u8; 16]) -> u32 {
    if luid.is_null() || guid.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's LUID.
    let want = unsafe { *luid };
    match list().ok().and_then(|v| v.into_iter().find(|i| iface_luid(i.flags, i.index) == want)) {
        Some(i) => {
            // SAFETY: the caller's out pointer.
            unsafe { *guid = iface_guid(i.index) };
            0
        }
        None => ERROR_INVALID_PARAMETER,
    }
}

/// GetIfEntry2: MIB_IF_ROW2 (1352 bytes) of the interface named by its
/// LUID, or by its index when the LUID is zero.
extern "win64" fn GetIfEntry2(row: *mut u8) -> u32 {
    if row.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let p = row as u64;
    // SAFETY: the caller's MIB_IF_ROW2.
    let (luid, index): (u64, u32) = unsafe { (teb::get(p, 0), teb::get(p, 8)) };
    if luid == 0 && index == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(i) = ifs.iter().find(|i| if luid != 0 { iface_luid(i.flags, i.index) == luid } else { i.index as u32 == index }) else {
        return ERROR_FILE_NOT_FOUND;
    };
    let loopback = i.flags & net::IFF_LOOPBACK != 0;
    let wireless = i.flags & net::IFF_WIRELESS != 0;
    let up = i.flags & net::IFF_UP != 0 && (loopback || i.flags & net::IFF_LINK != 0);
    let name = net::field(&i.name);
    // SAFETY: the caller's MIB_IF_ROW2 (1352 bytes).
    unsafe {
        core::ptr::write_bytes(row, 0, 1352);
        teb::put(p, 0, iface_luid(i.flags, i.index));
        teb::put(p, 8, i.index as u32);
        core::ptr::copy_nonoverlapping(iface_guid(i.index).as_ptr(), row.add(12), 16);
        if !loopback {
            teb::put(p, 1056, 6u32);
            core::ptr::copy_nonoverlapping(i.mac.as_ptr(), row.add(1060), 6);
            core::ptr::copy_nonoverlapping(i.mac.as_ptr(), row.add(1092), 6);
        }
        teb::put(p, 1124, i.mtu);
        teb::put(p, 1128, iface_type(i.flags));
        teb::put(p, 1136, if loopback { 13u32 } else { 0 }); // NdisMediumLoopback / NdisMedium802_3
        teb::put(p, 1140, if wireless { 9u32 } else { 0 }); // NdisPhysicalMediumNative802_11
        teb::put(p, 1144, if loopback { 1u32 } else { 2 }); // NET_IF_ACCESS_LOOPBACK / _BROADCAST
        // HardwareInterface and ConnectorPresent for a real adapter.
        *row.add(1152) = if loopback { 0 } else { 0x1 | 0x4 };
        teb::put(p, 1156, if up { 1u32 } else { 2 }); // IfOperStatusUp / Down
        teb::put(p, 1160, 1u32); // NET_IF_ADMIN_STATUS_UP
        teb::put(p, 1164, if up { 1u32 } else { 2 }); // MediaConnectStateConnected / Disconnected
        teb::put(p, 1184, 1u32); // NET_IF_CONNECTION_DEDICATED
        for (off, n) in [
            (1208, i.rx_bytes),
            (1216, i.rx_packets),
            (1232, i.rx_dropped),
            (1240, i.rx_errors),
            (1256, i.rx_bytes),
            (1280, i.tx_bytes),
            (1288, i.tx_packets),
            (1304, i.tx_dropped),
            (1320, i.tx_bytes),
        ] {
            teb::put(p, off, n as u64);
        }
    }
    out_w(name, (p + 28) as *mut u16, 257);
    out_w(&format!("OSyx {name}"), (p + 542) as *mut u16, 257);
    0
}

/// GetIfTable2: a MIB_IF_TABLE2 (count, then MIB_IF_ROW2 rows of 1352
/// bytes from offset 8) of every interface, freed with FreeMibTable.
extern "win64" fn GetIfTable2(table: *mut u64) -> u32 {
    if table.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(base) = crate::kernel32::heap_alloc(8 + ifs.len() * 1352, true) else {
        return ERROR_NOT_ENOUGH_MEMORY;
    };
    let mut n = 0u32;
    for i in &ifs {
        let row = base + 8 + n as u64 * 1352;
        // SAFETY: a row of the fresh table; GetIfEntry2 fills it by LUID.
        unsafe { teb::put(row, 0, iface_luid(i.flags, i.index)) };
        if GetIfEntry2(row as *mut u8) == 0 {
            n += 1;
        }
    }
    // SAFETY: the fresh table; the caller's out pointer.
    unsafe {
        teb::put(base, 0, n);
        *table = base;
    }
    0
}

/// GetIfTable2Ex: MibIfTableNormal (0) and MibIfTableRaw (1) are the same
/// rows here; MibIfTableNormalWithoutStatistics (2) leaves the counters
/// (from InOctets on, offset 1208 of each row) zero.
extern "win64" fn GetIfTable2Ex(level: u32, table: *mut u64) -> u32 {
    if level > 2 {
        return ERROR_INVALID_PARAMETER;
    }
    let r = GetIfTable2(table);
    if r == 0 && level == 2 {
        // SAFETY: the table GetIfTable2 just made: a count and its rows.
        unsafe {
            let base = *table;
            let n: u32 = teb::get(base, 0);
            for k in 0..n as u64 {
                core::ptr::write_bytes((base + 8 + k * 1352 + 1208) as *mut u8, 0, 1352 - 1208);
            }
        }
    }
    r
}

const ERROR_INSUFFICIENT_BUFFER_IP: u32 = 122;
const ERROR_NO_DATA: u32 = 232;

/// The adapter name IP_ADAPTER_INDEX_MAP and the DHCP calls use.
fn adapter_name(index: u8) -> alloc::string::String {
    let g = iface_guid(index);
    format!(
        "\\DEVICE\\TCPIP_{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    )
}

/// GetInterfaceInfo: IP_INTERFACE_INFO, the count and an
/// IP_ADAPTER_INDEX_MAP (index, WCHAR[128] name) per adapter, loopback
/// excepted, as on Windows.
extern "win64" fn GetInterfaceInfo(out: *mut u8, len: *mut u32) -> u32 {
    if len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs: Vec<_> = match list() {
        Ok(v) => v.into_iter().filter(|i| i.flags & net::IFF_LOOPBACK == 0).collect(),
        Err(e) => return e,
    };
    let need = 4 + ifs.len() * 260;
    // SAFETY: the caller's in/out size and buffer of that size.
    unsafe {
        if out.is_null() || (*len as usize) < need {
            *len = need as u32;
            return ERROR_INSUFFICIENT_BUFFER_IP;
        }
        core::ptr::write_bytes(out, 0, need);
        teb::put(out as u64, 0, ifs.len() as i32);
        for (k, i) in ifs.iter().enumerate() {
            let m = out as u64 + 4 + k as u64 * 260;
            teb::put(m, 0, i.index as u32);
            out_w(&adapter_name(i.index), (m + 4) as *mut u16, 128);
        }
    }
    if ifs.is_empty() { ERROR_NO_DATA } else { 0 }
}

/// Interfaces whose lease IpReleaseAddress gave up: still DHCP adapters,
/// so IpRenewAddress can start DHCP on them again.
static RELEASED: Mutex<alloc::collections::BTreeSet<u8>> = Mutex::new(alloc::collections::BTreeSet::new());

/// The interface of an IP_ADAPTER_INDEX_MAP, configured by DHCP.
fn dhcp_adapter(map: *const u8) -> Result<net::NetIface, u32> {
    if map.is_null() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    // SAFETY: the caller's IP_ADAPTER_INDEX_MAP.
    let index: u32 = unsafe { teb::get(map as u64, 0) };
    let i = list()?.into_iter().find(|i| i.index as u32 == index).ok_or(ERROR_INVALID_PARAMETER)?;
    if i.method != net::CONFIG_DHCP && !RELEASED.lock().unwrap().contains(&i.index) {
        return Err(ERROR_NOT_SUPPORTED);
    }
    Ok(i)
}

/// IpReleaseAddress: gives the DHCP lease up; the adapter has no address
/// until IpRenewAddress.
extern "win64" fn IpReleaseAddress(map: *const u8) -> u32 {
    let i = match dhcp_adapter(map) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let c = net::NetConfig { iface: i.index as u32, method: net::CONFIG_DOWN as u32, ..Default::default() };
    match net::configure(&c) {
        Ok(()) => {
            RELEASED.lock().unwrap().insert(i.index);
            0
        }
        Err(_) => ERROR_ACCESS_DENIED,
    }
}

/// IpRenewAddress: restarts DHCP on the adapter.
extern "win64" fn IpRenewAddress(map: *const u8) -> u32 {
    let i = match dhcp_adapter(map) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let c = net::NetConfig { iface: i.index as u32, method: net::CONFIG_DHCP as u32, ..Default::default() };
    match net::configure(&c) {
        Ok(()) => {
            RELEASED.lock().unwrap().remove(&i.index);
            0
        }
        Err(_) => ERROR_ACCESS_DENIED,
    }
}

/// NotifyAddrChange/NotifyRouteChange: OSyx's addresses and routes are
/// fixed for the boot (see CHANGES), so a change never comes. With an
/// OVERLAPPED the request stays pending (ERROR_IO_PENDING) until
/// CancelIPChangeNotify; without one the call waits, as on Windows.
static ADDR_WAITS: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

fn notify_change(handle: *mut u64, ov: *mut u8) -> u32 {
    if ov.is_null() {
        loop {
            os_sys::sleep(3_600_000_000_000);
        }
    }
    let event = crate::kernel32::create_event(true, false, None);
    // SAFETY: the caller's OVERLAPPED and out handle.
    unsafe {
        crate::kernel32::overlapped::set_status(ov as u64, 0x103); // STATUS_PENDING
        if !handle.is_null() {
            *handle = event;
        }
    }
    ADDR_WAITS.lock().unwrap().push((ov as u64, event));
    997 // ERROR_IO_PENDING
}

extern "win64" fn NotifyAddrChange(handle: *mut u64, ov: *mut u8) -> u32 {
    notify_change(handle, ov)
}

extern "win64" fn NotifyRouteChange(handle: *mut u64, ov: *mut u8) -> u32 {
    notify_change(handle, ov)
}

/// Ends a pending notification with ERROR_OPERATION_ABORTED.
extern "win64" fn CancelIPChangeNotify(ov: *mut u8) -> i32 {
    let found = {
        let mut w = ADDR_WAITS.lock().unwrap();
        w.iter().position(|(o, _)| *o == ov as u64).map(|i| w.remove(i))
    };
    let Some((_, event)) = found else { return 0 };
    // SAFETY: the OVERLAPPED of the pending request.
    let user_event = unsafe {
        crate::kernel32::overlapped::set_status(ov as u64, 0xC000_0120); // STATUS_CANCELLED
        crate::kernel32::overlapped::event(ov as u64) & !1
    };
    if user_event != 0 {
        crate::kernel32::SetEvent(user_event);
    }
    crate::kernel32::CloseHandle(event);
    1
}

// ---- dhcpcsvc ------------------------------------------------------------------------------

/// DhcpCApiInitialize: version 2 of the DHCP client API.
extern "win64" fn DhcpCApiInitialize(version: *mut u32) -> u32 {
    if !version.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *version = 2 };
    }
    0
}

extern "win64" fn DhcpCApiCleanup() {}

/// The interface an adapter name names: its interface name, its GUID in
/// braces, or the \DEVICE\TCPIP_ form.
fn named_adapter(name: &str) -> Option<net::NetIface> {
    let bare = name.rsplit('_').next().unwrap_or(name);
    list().ok()?.into_iter().find(|i| {
        net::field(&i.name).eq_ignore_ascii_case(name) || adapter_name(i.index).rsplit('_').next().is_some_and(|g| g.eq_ignore_ascii_case(bare))
    })
}

/// DhcpRequestParams: the options the stack's DHCP client got for the
/// adapter: subnet mask (1), routers (3), DNS servers (6) and the lease
/// time (51). Others (such as the WPAD URL, 252) were not given: they come
/// back with no data, as on Windows. DHCPCAPI_PARAMS (32 bytes): flags,
/// option id, vendor flag, data pointer, data length.
#[allow(clippy::too_many_arguments)]
extern "win64" fn DhcpRequestParams(
    _flags: u32, _reserved: u64, adapter: *const u16, _class: u64, _send: u64, recd: u64, buf: *mut u8, size: *mut u32, _request_id: *const u16,
) -> u32 {
    if adapter.is_null() || size.is_null() || recd == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's NUL-terminated adapter name.
    let Some(name) = (unsafe { crate::path::wide_to_string(adapter) }) else { return ERROR_INVALID_PARAMETER };
    let Some(i) = named_adapter(&name) else { return ERROR_FILE_NOT_FOUND };
    // SAFETY: the caller's DHCPCAPI_PARAMS_ARRAY.
    let (n, params): (u32, u64) = unsafe { (teb::get(recd, 0), teb::get(recd, 8)) };
    let dhcp = i.method == net::CONFIG_DHCP;
    let option = |id: u32| -> Vec<u8> {
        match id {
            1 if dhcp => mask(i.prefix).to_vec(),
            3 if dhcp && i.gateway != [0; 4] => i.gateway.to_vec(),
            6 if dhcp && i.dns != [0; 4] => i.dns.to_vec(),
            51 if dhcp && i.lease_left != 0 => (i.lease_left.min(u32::MAX as u64) as u32).to_be_bytes().to_vec(),
            _ => Vec::new(),
        }
    };
    let values: Vec<Vec<u8>> = (0..n as u64).map(|k| option(unsafe { teb::get::<u32>(params + 32 * k, 4) })).collect();
    let need: usize = values.iter().map(Vec::len).sum();
    // SAFETY: the caller's in/out size, buffer and parameter array.
    unsafe {
        if buf.is_null() || (*size as usize) < need {
            *size = need as u32;
            return 234; // ERROR_MORE_DATA
        }
        let mut at = 0usize;
        for (k, v) in values.iter().enumerate() {
            let p = params + 32 * k as u64;
            if v.is_empty() {
                teb::put(p, 16, 0u64);
            } else {
                core::ptr::copy_nonoverlapping(v.as_ptr(), buf.add(at), v.len());
                teb::put(p, 16, buf as u64 + at as u64);
            }
            teb::put(p, 24, v.len() as u32);
            at += v.len();
        }
        *size = need as u32;
    }
    0
}

pub fn dhcp_exports() -> Vec<(&'static str, u64)> {
    export_table![DhcpCApiInitialize, DhcpCApiCleanup, DhcpRequestParams]
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetBestInterface,
        ConvertInterfaceGuidToLuid,
        ConvertInterfaceLuidToGuid,
        GetIfEntry2,
        GetIfTable2,
        GetIfTable2Ex,
        GetInterfaceInfo,
        IpReleaseAddress,
        IpRenewAddress,
        NotifyAddrChange,
        NotifyRouteChange,
        CancelIPChangeNotify,
        GetAdaptersInfo,
        GetIfTable,
        GetNetworkParams,
        IcmpCreateFile,
        IcmpCloseHandle,
        IcmpSendEcho,
        NotifyIpInterfaceChange,
        NotifyUnicastIpAddressChange,
        NotifyRouteChange2,
        CancelMibChangeNotify2,
        FreeMibTable,
        GetIpInterfaceTable,
        GetBestInterfaceEx,
        if_indextoname,
        if_nametoindex,
        GetAdaptersAddresses,
    ]
}
