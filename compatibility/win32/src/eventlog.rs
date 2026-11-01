//! The event log. OSyx has no event log service; an event source writes
//! each report as one line to the process's error stream (the system
//! console when nothing else is attached), the way OutputDebugString does:
//! `[event] <source> <type> <id>: <strings>`. Remote event logs are not
//! supported.
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
pub(crate) extern "win64" fn ReportEventW(h: u64, kind: u16, _category: u16, id: u32, _user: *const u8, count: u16, _size: u32, strings: *const u64, _data: *const u8) -> i32 {
    let Some(source) = SOURCES.lock().unwrap().get(&h).cloned() else { return fail(ERROR_INVALID_HANDLE, 0) };
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
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![RegisterEventSourceW, DeregisterEventSource, ReportEventW]
}
