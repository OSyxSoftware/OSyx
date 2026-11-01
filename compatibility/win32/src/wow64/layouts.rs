//! Host functions for 32-bit callers whose arguments or results have a
//! different layout than the Win64 implementation expects: pointer-sized
//! fields, arrays of pointers, and structures that hold them. Bound
//! through `host_override`.
//!
//! It also holds the x86 structured exception dispatch: RaiseException and
//! RtlUnwind for 32-bit code walk the frame-based handler chain at
//! `fs:[0]` and call each handler as 32-bit code, as Windows does on x86.
#![allow(non_snake_case)]
use super::{call32, current_teb32};
use crate::error::*;
use crate::kernel32::fail;
use crate::teb;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ---- pointer encoding ---------------------------------------------------------------

/// The per-process secret EncodePointer mixes in, kept to 32 bits so an
/// encoded 32-bit pointer still fits a 32-bit variable.
fn cookie32() -> u32 {
    static COOKIE: AtomicU32 = AtomicU32::new(0);
    let c = COOKIE.load(Ordering::Relaxed);
    if c != 0 {
        return c;
    }
    let fresh = (crate::rtl::EncodePointer(0) as u32) | 1;
    match COOKIE.compare_exchange(0, fresh, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => fresh,
        Err(other) => other,
    }
}

pub(super) extern "win64" fn EncodePointer32(p: u64) -> u64 {
    let c = cookie32();
    ((p as u32 ^ c).rotate_right(c & 31)) as u64
}

pub(super) extern "win64" fn DecodePointer32(p: u64) -> u64 {
    let c = cookie32();
    ((p as u32).rotate_left(c & 31) ^ c) as u64
}

// ---- interlocked singly linked lists -----------------------------------------------------
//
// SLIST_HEADER (x86, 8 bytes): Next (4) | Depth (2) | Sequence (2). Updates
// are one 8-byte compare-exchange.

fn slist(h: u64) -> Option<&'static AtomicU64> {
    // SAFETY: the caller's 8-byte aligned SLIST_HEADER.
    (h != 0 && h.is_multiple_of(8)).then(|| unsafe { &*(h as *const AtomicU64) })
}

pub(super) extern "win64" fn InitializeSListHead32(h: u64) {
    if let Some(a) = slist(h) {
        a.store(0, Ordering::Relaxed);
    }
}

pub(super) extern "win64" fn InterlockedPushEntrySList32(h: u64, entry: u64) -> u64 {
    let Some(a) = slist(h) else { return 0 };
    loop {
        let old = a.load(Ordering::Acquire);
        let first = old as u32;
        // SAFETY: the caller's SLIST_ENTRY (its Next field).
        unsafe { core::ptr::write_volatile(entry as *mut u32, first) };
        let depth = ((old >> 32) as u16).wrapping_add(1);
        let seq = ((old >> 48) as u16).wrapping_add(1);
        let new = entry as u32 as u64 | (depth as u64) << 32 | (seq as u64) << 48;
        if a.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return first as u64;
        }
    }
}

pub(super) extern "win64" fn InterlockedPopEntrySList32(h: u64) -> u64 {
    let Some(a) = slist(h) else { return 0 };
    loop {
        let old = a.load(Ordering::Acquire);
        let first = old as u32;
        if first == 0 {
            return 0;
        }
        // SAFETY: the first entry of the list.
        let next = unsafe { core::ptr::read_volatile(first as u64 as *const u32) };
        let depth = ((old >> 32) as u16).wrapping_sub(1);
        let seq = ((old >> 48) as u16).wrapping_add(1);
        let new = next as u64 | (depth as u64) << 32 | (seq as u64) << 48;
        if a.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return first as u64;
        }
    }
}

pub(super) extern "win64" fn InterlockedFlushSList32(h: u64) -> u64 {
    let Some(a) = slist(h) else { return 0 };
    loop {
        let old = a.load(Ordering::Acquire);
        let seq = ((old >> 48) as u16).wrapping_add(1);
        if a.compare_exchange(old, (seq as u64) << 48, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return old as u32 as u64;
        }
    }
}

pub(super) extern "win64" fn QueryDepthSList32(h: u64) -> u32 {
    slist(h).map_or(0, |a| (a.load(Ordering::Acquire) >> 32) as u16 as u32)
}

// ---- memory and system ------------------------------------------------------------------

/// VirtualQuery: MEMORY_BASIC_INFORMATION of 28 bytes (4-byte pointers
/// and SIZE_T).
pub(super) extern "win64" fn VirtualQuery32(addr: u64, out: *mut u8, len: usize) -> usize {
    if out.is_null() || len < 28 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let mut wide = [0u8; 48];
    if crate::kernel32::VirtualQuery(addr, wide.as_mut_ptr(), 48) == 0 {
        return 0;
    }
    let q = |o: usize| u64::from_le_bytes(wide[o..o + 8].try_into().unwrap()) as u32;
    let d = |o: usize| u32::from_le_bytes(wide[o..o + 4].try_into().unwrap());
    // SAFETY: the caller's 28-byte structure.
    unsafe {
        let p = out as u64;
        teb::put(p, 0, q(0));
        teb::put(p, 4, q(8));
        teb::put(p, 8, d(16));
        teb::put(p, 12, q(24));
        teb::put(p, 16, d(32));
        teb::put(p, 20, d(36));
        teb::put(p, 24, d(40));
    }
    28
}

/// GetSystemInfo: the 36-byte x86 SYSTEM_INFO of a 32-bit process on a
/// 64-bit system (as GetSystemInfo reports under WoW64): x86 architecture,
/// the 2 GiB user range.
pub(super) extern "win64" fn GetSystemInfo32(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let mut wide = [0u8; 48];
    crate::kernel32::GetSystemInfo(wide.as_mut_ptr());
    let count = u32::from_le_bytes(wide[32..36].try_into().unwrap());
    // SAFETY: the caller's 36-byte SYSTEM_INFO.
    unsafe {
        let p = out as u64;
        core::ptr::write_bytes(out, 0, 36);
        teb::put(p, 0, 0u16); // PROCESSOR_ARCHITECTURE_INTEL
        teb::put(p, 4, 4096u32);
        teb::put(p, 8, 0x1_0000u32);
        teb::put(p, 12, 0x7FFE_FFFFu32);
        teb::put(p, 16, if count >= 32 { u32::MAX } else { (1u32 << count) - 1 });
        teb::put(p, 20, count);
        teb::put(p, 24, 586u32); // PROCESSOR_INTEL_PENTIUM
        teb::put(p, 28, 0x1_0000u32);
        teb::put(p, 32, 6u16); // wProcessorLevel
    }
}

// ---- strings ------------------------------------------------------------------------------

/// CommandLineToArgvW: one LocalAlloc block of 4-byte pointers followed by
/// the strings, freed by the caller with LocalFree.
pub(super) extern "win64" fn CommandLineToArgvW32(cmdline: *const u16, count: *mut i32) -> u64 {
    let wide = crate::shell32::CommandLineToArgvW(cmdline, count);
    if wide == 0 {
        return 0;
    }
    // SAFETY: the count CommandLineToArgvW stored.
    let n = unsafe { *count }.max(0) as usize;
    let mut args: Vec<Vec<u16>> = Vec::with_capacity(n);
    for i in 0..n {
        // SAFETY: the array of `n` string pointers just returned.
        let s = unsafe { teb::get::<u64>(wide, 8 * i) } as *const u16;
        let mut v = Vec::new();
        let mut k = 0;
        // SAFETY: NUL-terminated strings of that array.
        while unsafe { *s.add(k) } != 0 {
            v.push(unsafe { *s.add(k) });
            k += 1;
        }
        v.push(0);
        args.push(v);
    }
    crate::kernel32::heap_free(wide);
    let table = 4 * n;
    let total = table + args.iter().map(|a| a.len() * 2).sum::<usize>();
    let Some(block) = crate::kernel32::heap_alloc(total, true) else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    let mut at = block + table as u64;
    for (i, a) in args.iter().enumerate() {
        // SAFETY: the fresh block of `total` bytes.
        unsafe {
            teb::put(block, 4 * i, at as u32);
            core::ptr::copy_nonoverlapping(a.as_ptr(), at as *mut u16, a.len());
        }
        at += a.len() as u64 * 2;
    }
    block
}

/// ReportEventW: the string array holds 4-byte pointers.
#[allow(clippy::too_many_arguments)] // the Win32 signature
pub(super) extern "win64" fn ReportEventW32(
    h: u64, kind: u16, category: u16, id: u32, user: *const u8, count: u16, size: u32, strings: *const u32, data: *const u8,
) -> i32 {
    let wide: Vec<u64> = if strings.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller's array of `count` pointers.
        (0..count as usize).map(|i| unsafe { *strings.add(i) } as u64).collect()
    };
    let p = if strings.is_null() { core::ptr::null() } else { wide.as_ptr() };
    crate::eventlog::ReportEventW(h, kind, category, id, user, count, size, p, data)
}

// ---- Winsock ------------------------------------------------------------------------------

/// getaddrinfo: the result list is rebuilt with the 32-byte x86 ADDRINFOA
/// (flags, family, socktype, protocol, addrlen, canonname, addr, next),
/// each node followed by its address. freeaddrinfo32 frees it.
pub(super) extern "win64" fn getaddrinfo32(node: *const u8, service: *const u8, hints: *const u8, res: *mut u32) -> i32 {
    if res.is_null() {
        return 10014; // WSAEFAULT
    }
    let mut list = 0u64;
    let r = crate::ws2_32::getaddrinfo(node, service, hints, &mut list);
    if r != 0 {
        return r;
    }
    let (mut head, mut tail) = (0u32, 0u64);
    let mut p = list;
    while p != 0 {
        // SAFETY: a node of the list getaddrinfo built (64-bit layout).
        let (flags, family, socktype, protocol, addrlen, addr, next) = unsafe {
            (
                teb::get::<i32>(p, 0),
                teb::get::<i32>(p, 4),
                teb::get::<i32>(p, 8),
                teb::get::<i32>(p, 12),
                teb::get::<u64>(p, 16) as usize,
                teb::get::<u64>(p, 32),
                teb::get::<u64>(p, 40),
            )
        };
        let Some(block) = crate::kernel32::heap_alloc(32 + addrlen, true) else { break };
        // SAFETY: the fresh node and the source address of `addrlen` bytes.
        unsafe {
            teb::put(block, 0, flags);
            teb::put(block, 4, family);
            teb::put(block, 8, socktype);
            teb::put(block, 12, protocol);
            teb::put(block, 16, addrlen as u32);
            if addr != 0 {
                core::ptr::copy_nonoverlapping(addr as *const u8, (block + 32) as *mut u8, addrlen);
                teb::put(block, 24, (block + 32) as u32);
            }
            if tail != 0 {
                teb::put(tail, 28, block as u32);
            } else {
                head = block as u32;
            }
        }
        tail = block;
        p = next;
    }
    crate::ws2_32::freeaddrinfo(list);
    // SAFETY: the caller's out pointer.
    unsafe { *res = head };
    0
}

pub(super) extern "win64" fn freeaddrinfo32(p: u64) {
    let mut p = p as u32 as u64;
    while p != 0 {
        // SAFETY: a node made by getaddrinfo32 (ai_next at 28).
        let next = unsafe { teb::get::<u32>(p, 28) } as u64;
        crate::kernel32::heap_free(p);
        p = next;
    }
}

/// x86 WSABUF arrays (length, 4-byte pointer) as the 16-byte x64 ones.
fn wsabufs(bufs: *const u8, count: u32) -> Vec<u64> {
    let mut out = Vec::with_capacity(2 * count as usize);
    for i in 0..count as usize {
        // SAFETY: the caller's array of `count` 8-byte WSABUFs.
        let (len, p) = unsafe { (teb::get::<u32>(bufs as u64, 8 * i), teb::get::<u32>(bufs as u64, 8 * i + 4)) };
        out.push(len as u64);
        out.push(p as u64);
    }
    out
}

pub(super) extern "win64" fn WSARecv32(s: u64, bufs: *const u8, count: u32, got: *mut u32, f: *mut u32, ov: *mut u8, cr: u64) -> i32 {
    let b = wsabufs(bufs, count);
    crate::ws2_32::WSARecv(s, b.as_ptr() as *const u8, count, got, f, ov, cr)
}

pub(super) extern "win64" fn WSASend32(s: u64, bufs: *const u8, count: u32, sent: *mut u32, f: u32, ov: *mut u8, cr: u64) -> i32 {
    let b = wsabufs(bufs, count);
    crate::ws2_32::WSASend(s, b.as_ptr() as *const u8, count, sent, f, ov, cr)
}

#[allow(clippy::too_many_arguments)] // the Win32 signature
pub(super) extern "win64" fn WSARecvFrom32(
    s: u64, bufs: *const u8, count: u32, got: *mut u32, f: *mut u32, from: *mut u8, fromlen: *mut i32, ov: *mut u8, cr: u64,
) -> i32 {
    let b = wsabufs(bufs, count);
    crate::ws2_32::WSARecvFrom(s, b.as_ptr() as *const u8, count, got, f, from, fromlen, ov, cr)
}

#[allow(clippy::too_many_arguments)] // the Win32 signature
pub(super) extern "win64" fn WSASendTo32(
    s: u64, bufs: *const u8, count: u32, sent: *mut u32, f: u32, to: *const u8, tolen: i32, ov: *mut u8, cr: u64,
) -> i32 {
    let b = wsabufs(bufs, count);
    crate::ws2_32::WSASendTo(s, b.as_ptr() as *const u8, count, sent, f, to, tolen, ov, cr)
}

/// x86 fd_set { u32 count; u32 sockets[64] } as the x64 one (8-byte
/// SOCKETs), and back.
fn fd_set64(p: *mut u8) -> Option<[u64; 65]> {
    if p.is_null() {
        return None;
    }
    let mut out = [0u64; 65];
    // SAFETY: the caller's fd_set.
    unsafe {
        let n: u32 = teb::get(p as u64, 0);
        out[0] = n.min(64) as u64;
        for i in 0..n.min(64) as usize {
            out[1 + i] = teb::get::<u32>(p as u64, 4 + 4 * i) as i32 as i64 as u64;
        }
    }
    Some(out)
}

fn fd_set32(p: *mut u8, wide: &[u64; 65]) {
    // SAFETY: the caller's fd_set; the x64 copy's count (low word).
    unsafe {
        let n = (wide[0] as u32).min(64);
        teb::put(p as u64, 0, n);
        for i in 0..n as usize {
            teb::put(p as u64, 4 + 4 * i, wide[1 + i] as u32);
        }
    }
}

pub(super) extern "win64" fn select32(nfds: i32, rd: *mut u8, wr: *mut u8, ex: *mut u8, timeout: *const i32) -> i32 {
    let (mut r, mut w, mut e) = (fd_set64(rd), fd_set64(wr), fd_set64(ex));
    let p = |s: &mut Option<[u64; 65]>| s.as_mut().map_or(core::ptr::null_mut(), |a| a.as_mut_ptr() as *mut u8);
    let n = crate::ws2_32::select(nfds, p(&mut r), p(&mut w), p(&mut e), timeout);
    for (user, wide) in [(rd, &r), (wr, &w), (ex, &e)] {
        if let Some(a) = wide {
            fd_set32(user, a);
        }
    }
    n
}

pub(super) extern "win64" fn WSAFDIsSet32(s: u64, set: *const u8) -> i32 {
    match fd_set64(set as *mut u8) {
        Some(a) => crate::ws2_32::__WSAFDIsSet(s, a.as_ptr() as *const u8),
        None => 0,
    }
}

/// WSAStartup: the x86 WSADATA (400 bytes) keeps the strings before the
/// counts: wVersion, wHighVersion, szDescription[257], szSystemStatus[129],
/// iMaxSockets, iMaxUdpDg, lpVendorInfo.
pub(super) extern "win64" fn WSAStartup32(version: u16, data: *mut u8) -> i32 {
    let mut wide = [0u8; 408];
    let r = crate::ws2_32::WSAStartup(version, if data.is_null() { core::ptr::null_mut() } else { wide.as_mut_ptr() });
    if r == 0 && !data.is_null() {
        // SAFETY: the caller's 400-byte WSADATA.
        unsafe {
            core::ptr::write_bytes(data, 0, 400);
            core::ptr::copy_nonoverlapping(wide.as_ptr(), data, 4);
            core::ptr::copy_nonoverlapping(wide.as_ptr().add(16), data.add(4), 257 + 129);
            core::ptr::copy_nonoverlapping(wide.as_ptr().add(4), data.add(390), 4);
        }
    }
    r
}

// ---- x86 structured exception handling ---------------------------------------------------------

/// The 32-bit top-level filter (SetUnhandledExceptionFilter of a 32-bit
/// program); called as 32-bit code when no frame handles an exception.
static FILTER32: AtomicU32 = AtomicU32::new(0);

pub(super) extern "win64" fn SetUnhandledExceptionFilter32(f: u64) -> u64 {
    FILTER32.swap(f as u32, Ordering::SeqCst) as u64
}

const EXCEPTION_NONCONTINUABLE: u32 = 1;
const EXCEPTION_UNWINDING: u32 = 2;
const EXCEPTION_EXIT_UNWIND: u32 = 4;
const END_OF_CHAIN: u32 = u32::MAX;
/// EXCEPTION_RECORD (x86): code, flags, record, address, count, 15 args.
const RECORD32: usize = 80;
/// CONTEXT (x86).
const CONTEXT32: usize = 716;
const CONTEXT_I386_CONTROL: u32 = 0x0001_0001;
/// The MSVC thread-naming exception, which only a debugger handles.
const MS_VC_THREAD_NAME: u32 = 0x406D_1388;

/// Memory for an exception record and context that 32-bit code can address.
fn low_block(size: usize) -> Option<u64> {
    crate::kernel32::heap_alloc(size, true).filter(|&p| p + (size as u64) < 1 << 32)
}

fn exception_list(teb32: u32) -> u32 {
    // SAFETY: NT_TIB.ExceptionList of the calling thread's TEB32.
    unsafe { core::ptr::read_volatile(teb32 as u64 as *const u32) }
}

fn set_exception_list(teb32: u32, frame: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(teb32 as u64 as *mut u32, frame) };
}

/// Calls the handler of every frame on the chain, innermost first, until
/// one continues execution (true) or the chain ends (false). A handler
/// that executes an __except block or a catch does not return here: it
/// unwinds (RtlUnwind) and resumes the program in that frame.
fn dispatch32(record: u64, context: u64) -> bool {
    let Some(teb32) = current_teb32() else { return false };
    let mut frame = exception_list(teb32);
    while frame != END_OF_CHAIN && frame != 0 {
        // SAFETY: an EXCEPTION_REGISTRATION_RECORD on the 32-bit stack.
        let (next, handler) = unsafe { (teb::get::<u32>(frame as u64, 0), teb::get::<u32>(frame as u64, 4)) };
        // ExceptionContinueExecution 0, ExceptionContinueSearch 1.
        match call32(handler, &[record as u32, frame, context as u32, 0]) {
            0 => return true,
            _ => frame = next,
        }
    }
    false
}

/// RaiseException for 32-bit code: builds the x86 EXCEPTION_RECORD and
/// CONTEXT and dispatches to the frame-based handlers. Unhandled, the
/// 32-bit top-level filter runs and the process ends with the code.
pub(super) extern "win64" fn RaiseException32(code: u32, flags: u32, count: u32, args: *const u32) {
    if code == MS_VC_THREAD_NAME {
        return; // no debugger: the program's own __except ignores it
    }
    let (Some(record), Some(context)) = (low_block(RECORD32), low_block(CONTEXT32)) else {
        crate::kernel32::ExitProcess(code);
    };
    let n = count.min(15);
    let (esp, eip) = super::caller_frame32().unwrap_or((0, 0));
    // SAFETY: the fresh blocks; the caller's `count` arguments.
    unsafe {
        teb::put(record, 0, code);
        teb::put(record, 4, flags & EXCEPTION_NONCONTINUABLE);
        teb::put(record, 12, eip);
        teb::put(record, 16, n);
        for i in 0..n as usize {
            teb::put(record, 20 + 4 * i, *args.add(i));
        }
        teb::put(context, 0, CONTEXT_I386_CONTROL);
        teb::put(context, 0xB8, eip); // Eip
        teb::put(context, 0xC4, esp + 4); // Esp after the return
    }
    if dispatch32(record, context) && flags & EXCEPTION_NONCONTINUABLE == 0 {
        crate::kernel32::heap_free(record);
        crate::kernel32::heap_free(context);
        return;
    }
    let filter = FILTER32.load(Ordering::SeqCst);
    if filter != 0 {
        if let Some(pointers) = low_block(8) {
            // SAFETY: the fresh EXCEPTION_POINTERS.
            unsafe {
                teb::put(pointers, 0, record as u32);
                teb::put(pointers, 4, context as u32);
            }
            // EXCEPTION_CONTINUE_EXECUTION (-1) resumes the caller.
            if call32(filter, &[pointers as u32]) as i32 == -1 {
                return;
            }
        }
    }
    os_std::eprintln!("winrun: unhandled 32-bit exception {code:#x} raised at {eip:#x}");
    crate::kernel32::ExitProcess(code);
}

/// RtlUnwind for 32-bit code: calls the handlers of the frames above
/// `target` with EXCEPTION_UNWINDING, removes them from the chain and
/// returns to the caller (which continues at its `TargetIp` label). A null
/// target unwinds the whole chain (exit unwind).
pub(super) extern "win64" fn RtlUnwind32(target: u64, _ip: u64, record: u64, _value: u64) {
    let Some(teb32) = current_teb32() else { return };
    let target = target as u32;
    let own = if record as u32 == 0 { low_block(RECORD32) } else { None };
    let record = own.unwrap_or(record as u32 as u64);
    let flags = EXCEPTION_UNWINDING | if target == 0 { EXCEPTION_EXIT_UNWIND } else { 0 };
    // SAFETY: the caller's (or our fresh) EXCEPTION_RECORD.
    unsafe {
        if own.is_some() {
            teb::put(record, 0, 0xC000_0027u32); // STATUS_UNWIND
        }
        let old: u32 = teb::get(record, 4);
        teb::put(record, 4, old | flags);
    }
    let context = low_block(CONTEXT32).unwrap_or(0);
    let mut frame = exception_list(teb32);
    while frame != END_OF_CHAIN && frame != 0 && frame != target {
        // SAFETY: an EXCEPTION_REGISTRATION_RECORD on the 32-bit stack.
        let (next, handler) = unsafe { (teb::get::<u32>(frame as u64, 0), teb::get::<u32>(frame as u64, 4)) };
        call32(handler, &[record as u32, frame, context as u32, 0]);
        set_exception_list(teb32, next);
        frame = next;
    }
    if target == 0 {
        set_exception_list(teb32, END_OF_CHAIN);
    }
    if context != 0 {
        crate::kernel32::heap_free(context);
    }
    if let Some(p) = own {
        crate::kernel32::heap_free(p);
    }
}
