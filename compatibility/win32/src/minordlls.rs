//! Small system DLLs xul.dll links against: rpcrt4 (UUID strings),
//! cfgmgr32 (device interfaces and notifications), ktmw32 (transactions),
//! windowscodecs (format conversion), bcryptprimitives (the process
//! random generator) and shcore (monitor scaling).
//!
//! OSyx has one display at 96 DPI and 100% scale, exposes no Windows
//! device interfaces (lists are empty and no device arrival is ever
//! reported to a registered notification), has no Kernel Transaction
//! Manager, and no WIC codecs.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, path, teb};
use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

// ---- rpcrt4 ------------------------------------------------------------------------

const RPC_S_OK: u32 = 0;
const RPC_S_OUT_OF_MEMORY: u32 = 14;
const RPC_S_INVALID_STRING_UUID: u32 = 1705;

/// GUID { Data1 u32, Data2 u16, Data3 u16, Data4 [u8; 8] } as text.
fn uuid_text(u: *const u8) -> String {
    // SAFETY: the caller's 16-byte UUID.
    let (d1, d2, d3, d4) = unsafe { (teb::get::<u32>(u as u64, 0), teb::get::<u16>(u as u64, 4), teb::get::<u16>(u as u64, 6), teb::get::<[u8; 8]>(u as u64, 8)) };
    alloc::format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    )
}

extern "win64" fn UuidToStringA(u: *const u8, out: *mut u64) -> u32 {
    if u.is_null() || out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let t = uuid_text(u);
    let Some(p) = crate::kernel32::heap_alloc(t.len() + 1, true) else { return RPC_S_OUT_OF_MEMORY };
    // SAFETY: a fresh zeroed block with room for the NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(t.as_ptr(), p as *mut u8, t.len());
        *out = p;
    }
    RPC_S_OK
}

extern "win64" fn UuidToStringW(u: *const u8, out: *mut u64) -> u32 {
    if u.is_null() || out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let w: Vec<u16> = uuid_text(u).encode_utf16().chain(core::iter::once(0)).collect();
    let Some(p) = crate::kernel32::heap_alloc(w.len() * 2, false) else { return RPC_S_OUT_OF_MEMORY };
    // SAFETY: a fresh block of w.len() units.
    unsafe {
        core::ptr::copy_nonoverlapping(w.as_ptr(), p as *mut u16, w.len());
        *out = p;
    }
    RPC_S_OK
}

/// NULL gives the nil UUID, as on Windows.
extern "win64" fn UuidFromStringW(s: *const u16, out: *mut u8) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: NULL or a NUL-terminated string.
    let text = unsafe { path::wide_to_string(s) };
    let mut bytes = [0u8; 16];
    if let Some(t) = text {
        let parts: Vec<&str> = t.split('-').collect();
        let lens = [8, 4, 4, 4, 12];
        if parts.len() != 5 || parts.iter().zip(lens).any(|(p, l)| p.len() != l || !p.bytes().all(|b| b.is_ascii_hexdigit())) {
            return RPC_S_INVALID_STRING_UUID;
        }
        let hex = |s: &str| u64::from_str_radix(s, 16).unwrap_or(0);
        bytes[0..4].copy_from_slice(&(hex(parts[0]) as u32).to_le_bytes());
        bytes[4..6].copy_from_slice(&(hex(parts[1]) as u16).to_le_bytes());
        bytes[6..8].copy_from_slice(&(hex(parts[2]) as u16).to_le_bytes());
        bytes[8..10].copy_from_slice(&(hex(parts[3]) as u16).to_be_bytes());
        bytes[10..16].copy_from_slice(&hex(parts[4]).to_be_bytes()[2..]);
    }
    // SAFETY: the caller's 16-byte UUID.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 16) };
    RPC_S_OK
}

fn string_free(p: *mut u64) -> u32 {
    if p.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's string pointer.
    unsafe {
        if *p != 0 {
            crate::kernel32::heap_free(*p);
        }
        *p = 0;
    }
    RPC_S_OK
}

extern "win64" fn RpcStringFreeA(p: *mut u64) -> u32 {
    string_free(p)
}

extern "win64" fn RpcStringFreeW(p: *mut u64) -> u32 {
    string_free(p)
}

pub fn rpcrt4_exports() -> Vec<(&'static str, u64)> {
    export_table![UuidToStringA, UuidToStringW, UuidFromStringW, RpcStringFreeA, RpcStringFreeW]
}

// ---- cfgmgr32 --------------------------------------------------------------------------

const CR_SUCCESS: u32 = 0;
const CR_INVALID_POINTER: u32 = 3;
const CR_INVALID_FLAG: u32 = 4;
const CR_INVALID_DEVNODE: u32 = 5;
const CR_INVALID_DATA: u32 = 31;

/// No Windows device instances exist, so no DEVINST is valid.
extern "win64" fn CM_Get_Device_IDW(_devinst: u32, buf: *mut u16, _len: u32, flags: u32) -> u32 {
    if buf.is_null() {
        return CR_INVALID_POINTER;
    }
    if flags != 0 {
        return CR_INVALID_FLAG;
    }
    CR_INVALID_DEVNODE
}

/// Interface lists are empty: one NUL closes the empty multi-string.
extern "win64" fn CM_Get_Device_Interface_List_SizeW(len: *mut u32, class: *const u8, _device: *const u16, flags: u32) -> u32 {
    if len.is_null() || class.is_null() {
        return CR_INVALID_POINTER;
    }
    if flags & !1 != 0 {
        return CR_INVALID_FLAG;
    }
    // SAFETY: the caller's out length.
    unsafe { *len = 1 };
    CR_SUCCESS
}

static NOTIFICATIONS: AtomicU64 = AtomicU64::new(0x0c3f_0000);
static OPEN_NOTIFICATIONS: os_std::sync::Mutex<Vec<u64>> = os_std::sync::Mutex::new(Vec::new());

/// CM_NOTIFY_FILTER: registration succeeds; no device event is delivered.
extern "win64" fn CM_Register_Notification(filter: *const u8, _context: u64, callback: u64, out: *mut u64) -> u32 {
    if filter.is_null() || callback == 0 || out.is_null() {
        return CR_INVALID_POINTER;
    }
    // SAFETY: the filter's cbSize.
    if unsafe { teb::get::<u32>(filter as u64, 0) } < 8 {
        return CR_INVALID_DATA;
    }
    let h = NOTIFICATIONS.fetch_add(4, Ordering::Relaxed);
    OPEN_NOTIFICATIONS.lock().unwrap().push(h);
    // SAFETY: the caller's out pointer.
    unsafe { *out = h };
    CR_SUCCESS
}

extern "win64" fn CM_Unregister_Notification(h: u64) -> u32 {
    let mut open = OPEN_NOTIFICATIONS.lock().unwrap();
    match open.iter().position(|&x| x == h) {
        Some(i) => {
            open.remove(i);
            CR_SUCCESS
        }
        None => CR_INVALID_DATA,
    }
}

pub fn cfgmgr32_exports() -> Vec<(&'static str, u64)> {
    export_table![CM_Get_Device_IDW, CM_Get_Device_Interface_List_SizeW, CM_Register_Notification, CM_Unregister_Notification]
}

// ---- ktmw32 ------------------------------------------------------------------------------

extern "win64" fn CreateTransaction(_sa: u64, _uow: u64, _options: u32, _iso: u32, _flags: u32, _timeout: u32, _desc: *const u16) -> u64 {
    fail(ERROR_NOT_SUPPORTED, u64::MAX)
}

extern "win64" fn CommitTransaction(_h: u64) -> i32 {
    fail(ERROR_INVALID_HANDLE, 0)
}

pub fn ktmw32_exports() -> Vec<(&'static str, u64)> {
    export_table![CreateTransaction, CommitTransaction]
}

// ---- windowscodecs -------------------------------------------------------------------------

/// No WIC components are installed to convert with.
extern "win64" fn WICConvertBitmapSource(_format: *const u8, source: u64, out: *mut u64) -> u32 {
    if source == 0 || out.is_null() {
        return 0x8007_0057; // E_INVALIDARG
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = 0 };
    0x8898_2F50 // WINCODEC_ERR_COMPONENTNOTFOUND
}

pub fn windowscodecs_exports() -> Vec<(&'static str, u64)> {
    export_table![WICConvertBitmapSource]
}

// ---- uxtheme ---------------------------------------------------------------------------------

/// Visual styles are off: the desktop draws its own widgets, so there is no
/// theme to hand out and every query says so. Programs then take the classic
/// drawing path, which is the one OSyx implements (user32/gdi32), instead of
/// asking for theme parts OSyx cannot draw.
extern "win64" fn IsThemeActive() -> u8 {
    0
}

extern "win64" fn IsAppThemed() -> u8 {
    0
}

extern "win64" fn OpenThemeData(_wnd: u64, _classes: *const u16) -> u64 {
    0 // no theme: callers fall back to classic drawing
}

extern "win64" fn CloseThemeData(theme: u64) -> u32 {
    if theme == 0 {
        E_INVALIDARG
    } else {
        0 // S_OK
    }
}

extern "win64" fn GetThemeColor(theme: u64, _part: i32, _state: i32, _property: i32, out: *mut u32) -> u32 {
    if theme == 0 || out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's COLORREF.
    unsafe { *out = 0 };
    E_INVALIDARG // no theme colours exist without a theme
}

/// Touch panning feedback needs a themed window manager.
extern "win64" fn BeginPanningFeedback(_wnd: u64) -> u8 {
    0
}

extern "win64" fn UpdatePanningFeedback(_wnd: u64, _x: i32, _y: i32, _overshoot: u8) -> u8 {
    0
}

extern "win64" fn EndPanningFeedback(_wnd: u64, _animate: u8) -> u8 {
    0
}

pub fn uxtheme_exports() -> Vec<(&'static str, u64)> {
    export_table![
        IsThemeActive,
        IsAppThemed,
        OpenThemeData,
        CloseThemeData,
        GetThemeColor,
        BeginPanningFeedback,
        UpdatePanningFeedback,
        EndPanningFeedback
    ]
}

// ---- bcryptprimitives ------------------------------------------------------------------------

/// Fills `buf` from the kernel's CSPRNG (/dev/random). Windows documents
/// ProcessPrng as never failing; if the device is unreadable OSyx has no
/// safe answer, so the process ends rather than use predictable bytes.
extern "win64" fn ProcessPrng(buf: *mut u8, len: usize) -> i32 {
    if len == 0 {
        return 1;
    }
    // SAFETY: the caller's buffer of `len` bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    if !crate::ucrt::entropy(out) {
        os_std::process::exit(0x4000_0000 | 0xC000_0001u32 as i32 & 0x0FFF_FFFF);
    }
    1
}

pub fn bcryptprimitives_exports() -> Vec<(&'static str, u64)> {
    export_table![ProcessPrng]
}

// ---- shcore ------------------------------------------------------------------------------------

/// The one monitor's handle (MonitorFromWindow and friends return it).
pub const PRIMARY_MONITOR: u64 = 0x0001_0001;
const E_INVALIDARG: u32 = 0x8007_0057;
/// HRESULT_FROM_WIN32(ERROR_INVALID_HANDLE): not a monitor handle.
const E_HANDLE: u32 = 0x8007_0006;

extern "win64" fn GetDpiForMonitor(monitor: u64, kind: u32, x: *mut u32, y: *mut u32) -> u32 {
    if monitor != PRIMARY_MONITOR {
        return E_HANDLE;
    }
    if kind > 2 || x.is_null() || y.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's out pointers.
    unsafe {
        *x = 96;
        *y = 96;
    }
    0
}

extern "win64" fn GetScaleFactorForMonitor(monitor: u64, scale: *mut u32) -> u32 {
    if monitor != PRIMARY_MONITOR {
        return E_HANDLE;
    }
    if scale.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *scale = 100 }; // SCALE_100_PERCENT
    0
}

pub fn shcore_exports() -> Vec<(&'static str, u64)> {
    export_table![GetDpiForMonitor, GetScaleFactorForMonitor]
}
