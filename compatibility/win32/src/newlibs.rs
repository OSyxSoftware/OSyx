//! Small system libraries: `secur32.dll`, `shfolder.dll`, `winspool.drv`,
//! `uiautomationcore.dll` and `oleacc.dll`. Each does what OSyx can really
//! do (user names, folder paths, an empty printer list, "no UI Automation
//! client") and fails with the documented error for the rest.

use crate::error::*;
use crate::kernel32::fail;
use alloc::string::String;
use alloc::vec::Vec;

const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const S_OK: u32 = 0;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_NOT_SUPPORTED: u32 = 0xC000_00BB;

// ---- secur32.dll ------------------------------------------------------------------------

const ERROR_NONE_MAPPED: u32 = 1332;
const ERROR_MORE_DATA: u32 = 234;

/// EXTENDED_NAME_FORMAT: NameSamCompatible (2) is "COMPUTER\user" and
/// NameDisplay (3) the user name; the directory-based formats (distinguished
/// name, canonical, UPN ...) do not exist for a local account
/// (ERROR_NONE_MAPPED). `*size` is the buffer size in characters, then the
/// length without the NUL; too small is ERROR_MORE_DATA with the size needed.
extern "win64" fn GetUserNameExW(format: u32, buf: *mut u16, size: *mut u32) -> u32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let user = crate::accounts::current_user_name();
    let name = match format {
        2 => alloc::format!("{}\\{}", crate::accounts::computer_name(), user),
        3 => user,
        1 | 4..=13 => return fail(ERROR_NONE_MAPPED, 0),
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    let units: Vec<u16> = name.encode_utf16().collect();
    // SAFETY: the caller's in/out size and buffer.
    unsafe {
        if buf.is_null() || (*size as usize) < units.len() + 1 {
            *size = units.len() as u32 + 1;
            return fail(ERROR_MORE_DATA, 0);
        }
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len());
        *buf.add(units.len()) = 0;
        *size = units.len() as u32;
    }
    1
}

/// Every SSPI entry point of this table: there are no security packages, so
/// each call fails with SEC_E_UNSUPPORTED_FUNCTION.
extern "win64" fn sspi_unsupported() -> u32 {
    0x8009_0302
}

/// SecurityFunctionTableW, version 1: dwVersion (padded to 8 bytes) and the
/// 28 entry points that follow.
#[repr(C)]
struct SspiTable {
    version: u64,
    entries: [*const (); 28],
}

// SAFETY: immutable function pointers.
unsafe impl Sync for SspiTable {}

static SSPI_TABLE: SspiTable = SspiTable { version: 1, entries: [sspi_unsupported as *const (); 28] };

/// A real function table whose calls report that no package is available,
/// as Windows does for an unknown package, instead of a NULL table.
extern "win64" fn InitSecurityInterfaceW() -> *const SspiTable {
    &SSPI_TABLE
}

/// The local security authority is not reachable: no logon sessions can be
/// created or queried.
extern "win64" fn LsaConnectUntrusted(handle: *mut u64) -> u32 {
    if handle.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *handle = 0 };
    STATUS_NOT_SUPPORTED
}

/// No LSA handle exists, so every handle is invalid.
extern "win64" fn LsaDeregisterLogonProcess(_handle: u64) -> u32 {
    STATUS_INVALID_HANDLE
}

/// LSA never returns a buffer, so there is none to free.
extern "win64" fn LsaFreeReturnBuffer(_buffer: u64) -> u32 {
    STATUS_INVALID_PARAMETER
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn LsaLogonUser(_handle: u64, _origin: u64, _kind: u32, _package: u32, _info: u64, _len: u32, _groups: u64, _source: u64, _profile: u64, _profile_len: u64, _luid: u64, _token: u64, _quotas: u64, _substatus: u64) -> u32 {
    STATUS_INVALID_HANDLE
}

pub fn secur32_exports() -> Vec<(&'static str, u64)> {
    export_table![GetUserNameExW, InitSecurityInterfaceW, LsaConnectUntrusted, LsaDeregisterLogonProcess, LsaFreeReturnBuffer, LsaLogonUser]
}

// ---- shfolder.dll -----------------------------------------------------------------------

/// SHGetFolderPathA: the wide function, in the ANSI code page (UTF-8).
extern "win64" fn SHGetFolderPathA(hwnd: u64, csidl: u32, token: u64, kind: u32, out: *mut u8) -> u32 {
    if out.is_null() {
        return 0x8007_0057; // E_INVALIDARG
    }
    let mut wide = [0u16; 260];
    let hr = crate::shell32::SHGetFolderPathW(hwnd, csidl, token, kind, wide.as_mut_ptr());
    if hr != S_OK {
        return hr;
    }
    let len = wide.iter().position(|&u| u == 0).unwrap_or(wide.len());
    let text = String::from_utf16_lossy(&wide[..len]);
    // SAFETY: the caller's buffer of MAX_PATH bytes.
    unsafe {
        let n = text.len().min(259);
        core::ptr::copy_nonoverlapping(text.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    S_OK
}

pub fn shfolder_exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("SHGetFolderPathW", crate::shell32::SHGetFolderPathW as *const () as u64),
        ("SHGetFolderPathA", SHGetFolderPathA as *const () as u64),
    ]
}

// ---- winspool.drv -----------------------------------------------------------------------

const ERROR_INVALID_PRINTER_NAME: u32 = 1801;
const ERROR_INVALID_LEVEL: u32 = 124;

/// OSyx has no printers: no name can be opened.
extern "win64" fn OpenPrinterW(_name: *const u16, handle: *mut u64, _defaults: u64) -> i32 {
    if handle.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *handle = 0 };
    fail(ERROR_INVALID_PRINTER_NAME, 0)
}

/// No printer handle exists, so every handle is invalid.
extern "win64" fn ClosePrinter(_handle: u64) -> i32 {
    fail(ERROR_INVALID_HANDLE, 0)
}

/// The list of printers is empty: success with zero bytes needed and zero
/// entries, for the levels Windows defines (1, 2, 4, 5).
extern "win64" fn EnumPrintersW(_flags: u32, _name: *const u16, level: u32, _buf: *mut u8, _size: u32, needed: *mut u32, returned: *mut u32) -> i32 {
    if needed.is_null() || returned.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !matches!(level, 1 | 2 | 4 | 5) {
        return fail(ERROR_INVALID_LEVEL, 0);
    }
    // SAFETY: the caller's out pointers.
    unsafe {
        *needed = 0;
        *returned = 0;
    }
    1
}

/// There is no printer to ask: -1 with ERROR_INVALID_PRINTER_NAME.
extern "win64" fn DeviceCapabilitiesW(_device: *const u16, _port: *const u16, _capability: u16, _out: *mut u16, _mode: u64) -> i32 {
    fail(ERROR_INVALID_PRINTER_NAME, -1)
}

extern "win64" fn DocumentPropertiesW(_hwnd: u64, _printer: u64, _device: *const u16, _out: u64, _input: u64, _mode: u32) -> i32 {
    fail(ERROR_INVALID_PRINTER_NAME, -1)
}

pub fn winspool_exports() -> Vec<(&'static str, u64)> {
    export_table![OpenPrinterW, ClosePrinter, EnumPrintersW, DeviceCapabilitiesW, DocumentPropertiesW]
}

// ---- uiautomationcore.dll and oleacc.dll -------------------------------------------------

/// A COM object with only IUnknown: the singletons UIA hands out for
/// "not supported" and "mixed attribute".
#[repr(C)]
struct Singleton {
    vtable: *const VTable,
}

/// The three IUnknown slots.
#[repr(transparent)]
struct VTable([*const (); 3]);

// SAFETY: immutable function pointers.
unsafe impl Sync for VTable {}

extern "win64" fn singleton_query(this: u64, iid: *const u8, out: *mut u64) -> u32 {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // IID_IUnknown {00000000-0000-0000-C000-000000000046}
    const IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    // SAFETY: the caller's 16-byte IID and out pointer.
    unsafe {
        if core::slice::from_raw_parts(iid, 16) == IUNKNOWN {
            *out = this;
            S_OK
        } else {
            *out = 0;
            E_NOINTERFACE
        }
    }
}

extern "win64" fn singleton_ref(_this: u64) -> u32 {
    1 // the objects are static: their count never changes
}

static SINGLETON_VTABLE: VTable = VTable([singleton_query as *const (), singleton_ref as *const (), singleton_ref as *const ()]);
static NOT_SUPPORTED_VALUE: Singleton = Singleton { vtable: &SINGLETON_VTABLE };
static MIXED_VALUE: Singleton = Singleton { vtable: &SINGLETON_VTABLE };

// SAFETY: the singletons are immutable.
unsafe impl Sync for Singleton {}

extern "win64" fn UiaGetReservedNotSupportedValue(out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG_UIA;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = &NOT_SUPPORTED_VALUE as *const Singleton as u64 };
    S_OK
}

extern "win64" fn UiaGetReservedMixedAttributeValue(out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG_UIA;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = &MIXED_VALUE as *const Singleton as u64 };
    S_OK
}

const E_INVALIDARG_UIA: u32 = 0x8007_0057;

/// No UI Automation client exists in OSyx, so nobody is listening.
extern "win64" fn UiaClientsAreListening() -> i32 {
    0
}

/// With no client asking, a window has nothing to answer WM_GETOBJECT with.
extern "win64" fn UiaReturnRawElementProvider(_hwnd: u64, _wparam: u64, _lparam: i64, _provider: u64) -> i64 {
    0
}

/// Without a listening client an event has nobody to reach (S_OK, as on
/// Windows when no client subscribed).
extern "win64" fn UiaRaiseAutomationEvent(_provider: u64, _event: i32) -> u32 {
    S_OK
}

extern "win64" fn UiaRaiseAutomationPropertyChangedEvent(_provider: u64, _property: i32, _old: u64, _new: u64) -> u32 {
    S_OK
}

/// UI Automation hosting of a window needs the UIA runtime: not available.
extern "win64" fn UiaHostProviderFromHwnd(_hwnd: u64, out: *mut u64) -> u32 {
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

pub fn uiautomationcore_exports() -> Vec<(&'static str, u64)> {
    export_table![
        UiaGetReservedNotSupportedValue,
        UiaGetReservedMixedAttributeValue,
        UiaClientsAreListening,
        UiaReturnRawElementProvider,
        UiaRaiseAutomationEvent,
        UiaRaiseAutomationPropertyChangedEvent,
        UiaHostProviderFromHwnd,
    ]
}

/// Active Accessibility objects are not provided.
extern "win64" fn CreateStdAccessibleObject(_hwnd: u64, _object: i32, _iid: *const u8, out: *mut u64) -> u32 {
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

extern "win64" fn LresultFromObject(_iid: *const u8, _wparam: u64, _object: u64) -> i64 {
    E_NOTIMPL as i32 as i64
}

extern "win64" fn AccessibleObjectFromWindow(_hwnd: u64, _object: u32, _iid: *const u8, out: *mut u64) -> u32 {
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

pub fn oleacc_exports() -> Vec<(&'static str, u64)> {
    export_table![CreateStdAccessibleObject, LresultFromObject, AccessibleObjectFromWindow]
}

// ---- msvcrt.dll -------------------------------------------------------------------------

/// The C runtime of old programs: the same functions as the UCRT, plus the
/// C++ allocation operators under their msvcrt names (operator new / delete,
/// 32-bit and 64-bit decoration). Unlike msvcrt's operator new this one
/// returns NULL when memory runs out instead of throwing std::bad_alloc.
pub fn msvcrt_exports() -> Vec<(&'static str, u64)> {
    let mut e = crate::ucrt::exports();
    let find = |e: &Vec<(&'static str, u64)>, n: &str| e.iter().find(|(k, _)| *k == n).map(|(_, a)| *a);
    if let (Some(malloc), Some(free)) = (find(&e, "malloc"), find(&e, "free")) {
        for n in ["??2@YAPAXI@Z", "??2@YAPEAX_K@Z"] {
            e.push((n, malloc));
        }
        for n in ["??3@YAXPAX@Z", "??3@YAXPEAX@Z"] {
            e.push((n, free));
        }
    }
    e
}
