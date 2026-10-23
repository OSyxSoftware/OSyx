//! Event Tracing for Windows, provider side. OSyx has no trace sessions:
//! a provider registers and its events are accepted and dropped, which is
//! what Windows does when no session has enabled the provider (EventWrite
//! succeeds). The enable callback is therefore never called, and classic
//! TraceEvent has no session handle to write to.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail};
use alloc::{collections::BTreeMap, vec::Vec};
use os_std::sync::Mutex;

/// Registered providers by handle: their GUIDs.
static PROVIDERS: Mutex<BTreeMap<u64, [u8; 16]>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u64> = Mutex::new(0x0e70_0001);

fn register(guid: *const u8) -> Result<u64, u32> {
    if guid.is_null() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut g = [0u8; 16];
    // SAFETY: the caller's GUID.
    unsafe { core::ptr::copy_nonoverlapping(guid, g.as_mut_ptr(), 16) };
    let mut n = NEXT.lock().unwrap();
    let h = *n;
    *n += 2;
    PROVIDERS.lock().unwrap().insert(h, g);
    Ok(h)
}

extern "win64" fn EventRegister(guid: *const u8, _callback: u64, _context: u64, out: *mut u64) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    match register(guid) {
        Ok(h) => {
            // SAFETY: the caller's REGHANDLE.
            unsafe { *out = h };
            ERROR_SUCCESS
        }
        Err(e) => e,
    }
}

extern "win64" fn EventUnregister(h: u64) -> u32 {
    if PROVIDERS.lock().unwrap().remove(&h).is_some() { ERROR_SUCCESS } else { ERROR_INVALID_HANDLE }
}

fn known(h: u64) -> u32 {
    if PROVIDERS.lock().unwrap().contains_key(&h) { ERROR_SUCCESS } else { ERROR_INVALID_HANDLE }
}

/// EventProviderSetTraits (2) and EventProviderUseDescriptorType (3) are
/// accepted; binary tracking (1) and the rest are not offered.
extern "win64" fn EventSetInformation(h: u64, class: u32, info: *const u8, len: u32) -> u32 {
    let e = known(h);
    if e != ERROR_SUCCESS {
        return e;
    }
    match class {
        2 | 3 if !info.is_null() && len > 0 => ERROR_SUCCESS,
        2 | 3 => ERROR_INVALID_PARAMETER,
        _ => ERROR_NOT_SUPPORTED,
    }
}

extern "win64" fn EventWrite(h: u64, desc: *const u8, _count: u32, _data: *const u8) -> u32 {
    if desc.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    known(h)
}

extern "win64" fn EventWriteTransfer(h: u64, desc: *const u8, _activity: *const u8, _related: *const u8, _count: u32, _data: *const u8) -> u32 {
    if desc.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    known(h)
}

/// Classic (MOF) providers register the same way.
#[allow(clippy::too_many_arguments)]
extern "win64" fn RegisterTraceGuidsW(
    _callback: u64,
    _context: u64,
    guid: *const u8,
    _count: u32,
    _guids: u64,
    _mof_path: *const u16,
    _mof_resource: *const u16,
    out: *mut u64,
) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    match register(guid) {
        Ok(h) => {
            // SAFETY: the caller's TRACEHANDLE.
            unsafe { *out = h };
            ERROR_SUCCESS
        }
        Err(e) => e,
    }
}

extern "win64" fn UnregisterTraceGuids(h: u64) -> u32 {
    EventUnregister(h)
}

/// Session handles come from the enable callback, which no session calls.
extern "win64" fn TraceEvent(_session: u64, header: *const u8) -> u32 {
    if header.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    ERROR_INVALID_HANDLE
}

extern "win64" fn GetTraceLoggerHandle(_buffer: *const u8) -> u64 {
    fail(ERROR_INVALID_HANDLE, u64::MAX)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        EventRegister,
        EventUnregister,
        EventSetInformation,
        EventWrite,
        EventWriteTransfer,
        RegisterTraceGuidsW,
        UnregisterTraceGuids,
        TraceEvent,
        GetTraceLoggerHandle,
    ]
}
