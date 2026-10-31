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

// ---- MIB tables, interface names and routing choice -------------------------------------

const AF_UNSPEC: u16 = 0;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;
const ERROR_NETWORK_UNREACHABLE: u32 = 1231;
const IPINTERFACE_ROW_SIZE: usize = 168;

/// A MIB table block: the size sits in the 16 bytes before the pointer that
/// FreeMibTable receives.
fn mib_alloc(bytes: usize) -> u64 {
    let layout = core::alloc::Layout::from_size_align(bytes + 16, 16).unwrap();
    // SAFETY: the layout has a non-zero size.
    let base = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if base.is_null() {
        return 0;
    }
    // SAFETY: the block holds at least 16 bytes.
    unsafe { *(base as *mut usize) = bytes + 16 };
    base as u64 + 16
}

/// Releases a table of GetIpInterfaceTable. NULL is accepted and ignored.
extern "win64" fn FreeMibTable(table: u64) {
    if table == 0 {
        return;
    }
    let base = (table - 16) as *mut u8;
    // SAFETY: `table` came from mib_alloc, which stored the block size first.
    unsafe {
        let layout = core::alloc::Layout::from_size_align(*(base as *const usize), 16).unwrap();
        alloc::alloc::dealloc(base, layout);
    }
}

fn if_type(i: &net::NetIface) -> u64 {
    if i.flags & net::IFF_LOOPBACK != 0 {
        24
    } else if i.flags & net::IFF_WIRELESS != 0 {
        71
    } else {
        6
    }
}

/// GetIpInterfaceTable(family, &table): one MIB_IPINTERFACE_ROW per IPv4
/// interface. OSyx has no IPv6 stack, so AF_INET6 yields an empty table.
extern "win64" fn GetIpInterfaceTable(family: u16, table: *mut u64) -> u32 {
    if table.is_null() || !matches!(family, AF_UNSPEC | AF_INET | AF_INET6) {
        return ERROR_INVALID_PARAMETER;
    }
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let ifs: Vec<_> = if family == AF_INET6 { Vec::new() } else { ifs };
    let block = mib_alloc(8 + ifs.len() * IPINTERFACE_ROW_SIZE);
    if block == 0 {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    // SAFETY: the block holds the count, 4 bytes of padding and the rows.
    unsafe {
        teb::put(block, 0, ifs.len() as u32);
        for (k, i) in ifs.iter().enumerate() {
            let row = block + 8 + (k * IPINTERFACE_ROW_SIZE) as u64;
            *(row as *mut u16) = AF_INET;
            // NET_LUID: NetLuidIndex in bits 24..48, IfType in bits 48..64.
            *((row + 8) as *mut u64) = (if_type(i) << 48) | ((i.index as u64) << 24);
            teb::put(row, 16, i.index as u32);
            teb::put(row, 152, i.mtu);
            *((row + 156) as *mut u8) = (i.flags & net::IFF_UP != 0 && i.flags & net::IFF_LINK != 0) as u8;
        }
        *table = block;
    }
    0 // NO_ERROR
}

/// GetBestInterfaceEx(&destination, &index): the interface whose subnet holds
/// the destination, else the one with a default gateway.
extern "win64" fn GetBestInterfaceEx(dest: *const u8, index: *mut u32) -> u32 {
    if dest.is_null() || index.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: a SOCKADDR_IN or larger; the family is its first field.
    let family = unsafe { *(dest as *const u16) };
    if family != AF_INET {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: sockaddr_in: port at 2, the address at 4.
    let target = unsafe { [*dest.add(4), *dest.add(5), *dest.add(6), *dest.add(7)] };
    let ifs = match list() {
        Ok(v) => v,
        Err(e) => return e,
    };
    let up = |i: &&net::NetIface| i.flags & net::IFF_UP != 0 && i.addr != [0; 4];
    let masked = |a: [u8; 4], prefix: u8| u32::from_be_bytes(a) & u32::from_be_bytes(mask(prefix));
    let on_link = ifs
        .iter()
        .filter(up)
        .filter(|i| masked(i.addr, i.prefix) == masked(target, i.prefix))
        .max_by_key(|i| i.prefix);
    let best = on_link.or_else(|| ifs.iter().filter(up).find(|i| i.gateway != [0; 4]));
    match best {
        Some(i) => {
            // SAFETY: the caller's out pointer.
            unsafe { *index = i.index as u32 };
            0 // NO_ERROR
        }
        None => ERROR_NETWORK_UNREACHABLE,
    }
}

/// if_indextoname(index, buffer): the interface name, or NULL.
extern "win64" fn if_indextoname(index: u32, name: *mut u8) -> *mut u8 {
    if name.is_null() {
        return core::ptr::null_mut();
    }
    match list().ok().and_then(|v| v.into_iter().find(|i| i.index as u32 == index)) {
        Some(i) => {
            let n = net::field(&i.name);
            // SAFETY: the caller's buffer holds IF_NAMESIZE (256) bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(n.as_ptr(), name, n.len());
                *name.add(n.len()) = 0;
            }
            name
        }
        None => core::ptr::null_mut(),
    }
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

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![GetAdaptersInfo, GetIfTable, GetNetworkParams, IcmpCreateFile, IcmpCloseHandle, IcmpSendEcho, NotifyIpInterfaceChange, NotifyUnicastIpAddressChange, NotifyRouteChange2, CancelMibChangeNotify2, FreeMibTable, GetIpInterfaceTable, GetBestInterfaceEx, if_indextoname, if_nametoindex]
}
