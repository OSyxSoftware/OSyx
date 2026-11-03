//! The event log. OSyx has no event log service; an event source writes
//! each report as one line to the process's error stream (the system
//! console when nothing else is attached), the way OutputDebugString does:
//! `[event] <source> <type> <id>: <strings>`. The process also keeps its
//! reports as the Application log that OpenEventLog/ReadEventLog read
//! back as EVENTLOGRECORDs; other logs are empty. Remote event logs are
//! not supported.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    path,
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use os_std::sync::Mutex;

/// Open event sources by handle. Event log handles are not kernel
/// handles on Windows either: they are closed with DeregisterEventSource.
static SOURCES: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u64> = Mutex::new(0x7e00_0004);

/// A reported event: source, type, id, category, strings, time (seconds
/// since 1970).
struct Record {
    source: String,
    kind: u16,
    id: u32,
    category: u16,
    strings: Vec<String>,
    time: u32,
}

/// The Application log of this process, oldest first. Like a Windows log
/// that overwrites as needed, it keeps the newest LOG_LIMIT records; record
/// numbers stay those they were given (the oldest kept is `first`).
struct Log {
    records: alloc::collections::VecDeque<Record>,
    first: u32,
}

static LOG: Mutex<Log> = Mutex::new(Log { records: alloc::collections::VecDeque::new(), first: 1 });
const LOG_LIMIT: usize = 4096;

/// Open logs: (is the Application log, next record to read forwards, or
/// backwards when reading back).
static READERS: Mutex<BTreeMap<u64, (bool, Option<u32>)>> = Mutex::new(BTreeMap::new());

extern "win64" fn RegisterEventSourceW(server: *const u16, source: *const u16) -> u64 {
    // SAFETY: NULL or NUL-terminated strings.
    let (server, source) = unsafe { (path::wide_to_string(server), path::wide_to_string(source)) };
    if server.is_some_and(|s| !s.is_empty()) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    let Some(source) = source.filter(|s| !s.is_empty()) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let mut next = NEXT.lock().unwrap();
    let h = *next;
    *next += 4;
    SOURCES.lock().unwrap().insert(h, source);
    h
}

extern "win64" fn DeregisterEventSource(h: u64) -> i32 {
    match SOURCES.lock().unwrap().remove(&h) {
        Some(_) => 1,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) extern "win64" fn ReportEventW(h: u64, kind: u16, category: u16, id: u32, _user: *const u8, count: u16, _size: u32, strings: *const u64, _data: *const u8) -> i32 {
    let Some(source) = SOURCES.lock().unwrap().get(&h).cloned() else { return fail(ERROR_INVALID_HANDLE, 0) };
    let code = kind;
    let kind = match kind {
        0 => "success",
        1 => "error",
        2 => "warning",
        4 => "information",
        8 => "audit-success",
        0x10 => "audit-failure",
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    if count != 0 && strings.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let text: Vec<String> = (0..count as usize)
        // SAFETY: the caller's array of `count` NUL-terminated strings.
        .filter_map(|i| unsafe { path::wide_to_string(*strings.add(i) as *const u16) })
        .collect();
    // The low 16 bits are the event code; the high bits severity and facility.
    os_std::eprintln!("[event] {source} {kind} {}: {}", id & 0xffff, text.join(" | "));
    let time = (os_sys::clock(osyx_abi::CLOCK_REALTIME).unwrap_or(0) / 1_000_000_000) as u32;
    let mut log = LOG.lock().unwrap();
    if log.records.len() == LOG_LIMIT {
        log.records.pop_front();
        log.first += 1;
    }
    log.records.push_back(Record { source, kind: code, id, category, strings: text, time });
    1
}

fn open_log(server: Option<String>, source: Option<String>) -> u64 {
    if server.is_some_and(|s| !s.is_empty()) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    let Some(source) = source.filter(|s| !s.is_empty()) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // Unknown names open the Application log, as on Windows.
    let application = !["system", "security", "setup"].contains(&source.to_ascii_lowercase().as_str());
    let mut next = NEXT.lock().unwrap();
    let h = *next;
    *next += 4;
    READERS.lock().unwrap().insert(h, (application, None));
    h
}

extern "win64" fn OpenEventLogW(server: *const u16, source: *const u16) -> u64 {
    // SAFETY: NULL or NUL-terminated strings.
    unsafe { open_log(path::wide_to_string(server), path::wide_to_string(source)) }
}

extern "win64" fn OpenEventLogA(server: *const u8, source: *const u8) -> u64 {
    // SAFETY: NULL or NUL-terminated strings.
    unsafe { open_log(path::ansi_to_string(server), path::ansi_to_string(source)) }
}

extern "win64" fn CloseEventLog(h: u64) -> i32 {
    match READERS.lock().unwrap().remove(&h) {
        Some(_) => 1,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

const EVENTLOG_SEQUENTIAL_READ: u32 = 1;
const EVENTLOG_SEEK_READ: u32 = 2;
const EVENTLOG_FORWARDS_READ: u32 = 4;
const EVENTLOG_BACKWARDS_READ: u32 = 8;
const ERROR_HANDLE_EOF: u32 = 38;

/// EVENTLOGRECORD: the 56-byte header, source and computer names, the
/// strings, no SID or data, and the length again at the end.
fn record_bytes(number: u32, r: &Record) -> Vec<u8> {
    let wide = |s: &str| -> Vec<u8> { s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect() };
    let mut names = wide(&r.source);
    names.extend(wide(&crate::accounts::computer_name()));
    while names.len() % 4 != 0 {
        names.push(0);
    }
    let strings: Vec<u8> = r.strings.iter().flat_map(|s| wide(s)).collect();
    let string_offset = 56 + names.len();
    let mut total = string_offset + strings.len() + 4;
    total = total.next_multiple_of(4);
    let mut b = Vec::with_capacity(total);
    for v in [total as u32, 0x654c_664c, number, r.time, r.time, r.id] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [r.kind, r.strings.len() as u16, r.category, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // ClosingRecordNumber, StringOffset, UserSidLength, UserSidOffset,
    // DataLength, DataOffset.
    let data_offset = (string_offset + strings.len()) as u32;
    for v in [0, string_offset as u32, 0, 0, 0, data_offset] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&names);
    b.extend_from_slice(&strings);
    b.resize(total - 4, 0);
    b.extend_from_slice(&(total as u32).to_le_bytes());
    b
}

/// ReadEventLogW: whole records into the buffer, sequentially or from a
/// record number, forwards or backwards. ERROR_HANDLE_EOF past the last;
/// a buffer too small for the next record reports the size it needs.
#[allow(clippy::too_many_arguments)]
extern "win64" fn ReadEventLogW(h: u64, flags: u32, offset: u32, buf: *mut u8, size: u32, read: *mut u32, needed: *mut u32) -> i32 {
    // The handle's position is read and advanced under one lock, so two
    // threads reading through it never return the same records.
    let mut readers = READERS.lock().unwrap();
    let Some((application, pos)) = readers.get(&h).copied() else { return fail(ERROR_INVALID_HANDLE, 0) };
    let forwards = flags & EVENTLOG_FORWARDS_READ != 0;
    let backwards = flags & EVENTLOG_BACKWARDS_READ != 0;
    let seek = flags & EVENTLOG_SEEK_READ != 0;
    if forwards == backwards || seek == (flags & EVENTLOG_SEQUENTIAL_READ != 0) || buf.is_null() || read.is_null() || needed.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let log = LOG.lock().unwrap();
    // Records first..=last exist; ones overwritten since cannot be read.
    let (first, count) = (log.first, if application { log.records.len() as u32 } else { 0 });
    let last = first + count - 1;
    if seek && count > 0 && !(first..=last).contains(&offset) {
        drop(log);
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut next = if seek { offset } else { pos.unwrap_or(if forwards { first } else { last }) };
    // A sequential reader overtaken by overwriting resumes at the oldest.
    if forwards && next < first {
        next = first;
    }
    let mut out = Vec::new();
    while count > 0 && (first..=last).contains(&next) {
        let r = record_bytes(next, &log.records[(next - first) as usize]);
        if out.len() + r.len() > size as usize {
            if out.is_empty() {
                // SAFETY: the caller's out values.
                unsafe {
                    *read = 0;
                    *needed = r.len() as u32;
                }
                return fail(ERROR_INSUFFICIENT_BUFFER, 0);
            }
            break;
        }
        out.extend_from_slice(&r);
        next = if forwards { next + 1 } else { next - 1 };
    }
    drop(log);
    readers.insert(h, (application, Some(next)));
    drop(readers);
    if out.is_empty() {
        return fail(ERROR_HANDLE_EOF, 0);
    }
    // SAFETY: the caller's buffer of `size` bytes and out values.
    unsafe {
        core::ptr::copy_nonoverlapping(out.as_ptr(), buf, out.len());
        *read = out.len() as u32;
        *needed = 0;
    }
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![RegisterEventSourceW, DeregisterEventSource, ReportEventW, OpenEventLogW, OpenEventLogA, CloseEventLog, ReadEventLogW]
}
