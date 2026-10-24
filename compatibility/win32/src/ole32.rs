//! ole32: the task allocator (CoTaskMemAlloc/Realloc/Free) over the
//! process heap, which blocks shell32 hands out (SHGetKnownFolderPath)
//! come from, and COM apartments.
//!
//! Threads enter and leave apartments as on Windows (a thread-local STA or
//! the process-wide MTA, with the same counting and mode conflicts).
//! OLE initialization owns separate per-thread nesting while acquiring one
//! COM STA reference per successful call. Data-transfer APIs are separate.
//! OSyx has no COM class registrations and no COM servers, in process or
//! out of it, so class activation reports every class as not registered.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use os_std::sync::Mutex;

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_POINTER: u32 = 0x8000_4003;
const E_INVALIDARG: u32 = 0x8007_0057;
const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;
const RPC_E_TOO_LATE: u32 = 0x8001_0119;
const CO_E_NOTINITIALIZED: u32 = 0x8004_01f0;
const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_KNOWN: u32 = 0x2 | 0x4 | 0x8; // apartment, disable OLE1 DDE, speed over memory

// APTTYPE / APTTYPEQUALIFIER, from the COM apartment API.
const APTTYPE_STA: i32 = 0;
const APTTYPE_MTA: i32 = 1;
const APTTYPEQUALIFIER_NONE: i32 = 0;
const APTTYPEQUALIFIER_IMPLICIT_MTA: i32 = 1;

pub extern "win64" fn CoTaskMemAlloc(size: usize) -> u64 {
    kernel32::heap_alloc(size, false).unwrap_or(0)
}

pub extern "win64" fn CoTaskMemFree(p: u64) {
    kernel32::heap_free(p);
}

extern "win64" fn CoTaskMemRealloc(p: u64, size: usize) -> u64 {
    if p == 0 {
        return CoTaskMemAlloc(size);
    }
    if size == 0 {
        kernel32::heap_free(p);
        return 0;
    }
    let Some(n) = kernel32::heap_alloc(size, false) else { return 0 };
    // SAFETY: both blocks are live heap blocks; the copy fits the smaller.
    unsafe { core::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, kernel32::heap_size(p).min(size)) };
    kernel32::heap_free(p);
    n
}

// ---- apartments -----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Apartment {
    Single,
    Multi,
}

struct Com {
    /// Per thread: its apartment and CoInitialize count.
    threads: BTreeMap<u64, (Apartment, u32)>,
    /// OleInitialize has its own nesting count, even in an existing COM STA.
    ole_threads: BTreeMap<u64, u32>,
    security_set: bool,
    /// CoIncrementMTAUsage references keeping the MTA alive.
    mta_usage: u32,
}

static COM: Mutex<Com> = Mutex::new(Com {
    threads: BTreeMap::new(),
    ole_threads: BTreeMap::new(),
    security_set: false,
    mta_usage: 0,
});

impl Com {
    /// Whether the process has an MTA (any thread entered one), which is what
    /// makes every other thread implicitly part of it.
    fn mta_exists(&self) -> bool {
        self.mta_usage > 0 || self.threads.values().any(|(apartment, _)| *apartment == Apartment::Multi)
    }
}

fn initialized() -> bool {
    COM.lock().unwrap().threads.contains_key(&os_sys::gettid())
}

fn initialize_apartment(com: &mut Com, tid: u64, kind: Apartment) -> u32 {
    match com.threads.get_mut(&tid) {
        Some((k, _)) if *k != kind => RPC_E_CHANGED_MODE,
        Some((_, n)) => {
            *n += 1;
            S_FALSE
        }
        None => {
            com.threads.insert(tid, (kind, 1));
            S_OK
        }
    }
}

extern "win64" fn CoInitializeEx(reserved: u64, flags: u32) -> u32 {
    if reserved != 0 || flags & !COINIT_KNOWN != 0 {
        return E_INVALIDARG;
    }
    let kind = if flags & COINIT_APARTMENTTHREADED != 0 { Apartment::Single } else { Apartment::Multi };
    initialize_apartment(&mut COM.lock().unwrap(), os_sys::gettid(), kind)
}

extern "win64" fn CoInitialize(reserved: u64) -> u32 {
    CoInitializeEx(reserved, COINIT_APARTMENTTHREADED)
}

fn uninitialize_apartment(com: &mut Com, tid: u64) {
    if let Some((_, n)) = com.threads.get_mut(&tid) {
        *n -= 1;
        if *n == 0 {
            com.threads.remove(&tid);
        }
    }
}

extern "win64" fn CoUninitialize() {
    uninitialize_apartment(&mut COM.lock().unwrap(), os_sys::gettid());
}

// Windows also exposes these OLE Automation functions through ole32.
use crate::oleaut32::{GetErrorInfo, SetErrorInfo};

extern "win64" fn OleInitialize(reserved: u64) -> u32 {
    if reserved != 0 {
        return E_INVALIDARG;
    }
    let tid = os_sys::gettid();
    let mut com = COM.lock().unwrap();
    let hr = initialize_apartment(&mut com, tid, Apartment::Single);
    if hr != S_OK && hr != S_FALSE {
        return hr;
    }
    let count = com.ole_threads.entry(tid).or_insert(0);
    let hr = if *count == 0 { S_OK } else { S_FALSE };
    *count += 1;
    hr
}

extern "win64" fn OleUninitialize() {
    let tid = os_sys::gettid();
    let mut com = COM.lock().unwrap();
    let Some(count) = com.ole_threads.get_mut(&tid) else { return };
    *count -= 1;
    if *count == 0 {
        com.ole_threads.remove(&tid);
    }
    uninitialize_apartment(&mut com, tid);
}

/// The calling thread's apartment, and whether it is in it explicitly or
/// only because the process has an MTA (Windows puts every thread of such a
/// process into the MTA implicitly).
extern "win64" fn CoGetApartmentType(kind: *mut i32, qualifier: *mut i32) -> u32 {
    if kind.is_null() || qualifier.is_null() {
        return E_POINTER;
    }
    let com = COM.lock().unwrap();
    let (t, q) = match (com.threads.get(&os_sys::gettid()), com.mta_exists()) {
        (Some((Apartment::Single, _)), _) => (APTTYPE_STA, APTTYPEQUALIFIER_NONE),
        (Some((Apartment::Multi, _)), _) => (APTTYPE_MTA, APTTYPEQUALIFIER_NONE),
        (None, true) => (APTTYPE_MTA, APTTYPEQUALIFIER_IMPLICIT_MTA),
        (None, false) => return CO_E_NOTINITIALIZED,
    };
    // SAFETY: both out pointers were checked above.
    unsafe {
        *kind = t;
        *qualifier = q;
    }
    S_OK
}

/// Records the process's COM security. With no COM servers or remote
/// calls on OSyx nothing ever authenticates with it, but it can be set
/// once, after the caller has entered an apartment, as on Windows.
#[allow(clippy::too_many_arguments)]
extern "win64" fn CoInitializeSecurity(_sd: u64, _count: i32, _services: u64, _r1: u64, _authn: u32, _imp: u32, _list: u64, _caps: u32, _r3: u64) -> u32 {
    let tid = os_sys::gettid();
    let mut com = COM.lock().unwrap();
    if !com.threads.contains_key(&tid) {
        return CO_E_NOTINITIALIZED;
    }
    if com.security_set {
        return RPC_E_TOO_LATE;
    }
    com.security_set = true;
    S_OK
}

extern "win64" fn CoCreateInstance(_clsid: *const u8, outer: u64, _context: u32, _iid: *const u8, out: *mut u64) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = 0 };
    if !initialized() {
        return CO_E_NOTINITIALIZED;
    }
    if outer != 0 {
        return CLASS_E_NOAGGREGATION;
    }
    REGDB_E_CLASSNOTREG // no COM classes are registered on OSyx
}

/// IID_IForegroundTransfer {00000145-0000-0000-C000-000000000046}.
const IID_IFOREGROUND_TRANSFER: [u8; 16] = [0x45, 0x01, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

type QueryInterface = extern "win64" fn(u64, *const u8, *mut u64) -> u32;
type Release = extern "win64" fn(u64) -> u32;
type AllowForegroundTransfer = extern "win64" fn(u64, u64) -> u32;

/// Asks the object (through IForegroundTransfer) to let its server take
/// the foreground.
extern "win64" fn CoAllowSetForegroundWindow(unk: u64, reserved: u64) -> u32 {
    if unk == 0 || reserved != 0 {
        return E_INVALIDARG;
    }
    let mut ft = 0u64;
    // SAFETY: `unk` is a COM object: its first word is its vtable.
    unsafe {
        let vtbl = *(unk as *const *const u64);
        let qi: QueryInterface = core::mem::transmute(*vtbl);
        let hr = qi(unk, IID_IFOREGROUND_TRANSFER.as_ptr(), &mut ft);
        if hr != S_OK || ft == 0 {
            return hr;
        }
        let fvtbl = *(ft as *const *const u64);
        let allow: AllowForegroundTransfer = core::mem::transmute(*fvtbl.add(3));
        let release: Release = core::mem::transmute(*fvtbl.add(2));
        let hr = allow(ft, 0);
        release(ft);
        hr
    }
}

/// Keeps the MTA in existence without entering it (the cookie is opaque).
extern "win64" fn CoIncrementMTAUsage(cookie: *mut u64) -> u32 {
    if cookie.is_null() {
        return E_INVALIDARG;
    }
    let mut com = COM.lock().unwrap();
    com.mta_usage += 1;
    // SAFETY: the caller's CO_MTA_USAGE_COOKIE.
    unsafe { *cookie = 0x4d54_4100 + com.mta_usage as u64 };
    S_OK
}

extern "win64" fn CoDecrementMTAUsage(cookie: u64) -> u32 {
    let mut com = COM.lock().unwrap();
    if cookie == 0 || com.mta_usage == 0 {
        return E_INVALIDARG;
    }
    com.mta_usage -= 1;
    S_OK
}

/// Outside an incoming call (there never is one here) the caller's thread
/// is unknown: 0, with S_OK, as Windows reports it.
extern "win64" fn CoGetCallerTID(tid: *mut u32) -> u32 {
    if tid.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's DWORD.
    unsafe { *tid = 0 };
    S_OK
}

// ---- GUID strings ------------------------------------------------------------------------

const CO_E_CLASSSTRING: u32 = 0x8004_01F3;
const E_OUTOFMEMORY: u32 = 0x8007_000E;

/// "{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}", as StringFromGUID2 writes it.
fn guid_string(g: *const u8) -> String {
    // SAFETY: the caller's 16-byte GUID.
    let b = unsafe { core::slice::from_raw_parts(g, 16) };
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    alloc::format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// A braced GUID string (exactly 38 characters), or None.
fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    let parts: Vec<&str> = inner.split('-').collect();
    if parts.len() != 5 || parts.iter().zip([8, 4, 4, 4, 12]).any(|(p, l)| p.len() != l || !p.bytes().all(|c| c.is_ascii_hexdigit())) {
        return None;
    }
    let hex = |s: &str| u64::from_str_radix(s, 16).ok();
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&(hex(parts[0])? as u32).to_le_bytes());
    g[4..6].copy_from_slice(&(hex(parts[1])? as u16).to_le_bytes());
    g[6..8].copy_from_slice(&(hex(parts[2])? as u16).to_le_bytes());
    g[8..10].copy_from_slice(&(hex(parts[3])? as u16).to_be_bytes());
    g[10..16].copy_from_slice(&hex(parts[4])?.to_be_bytes()[2..]);
    Some(g)
}

/// NULL reads as the nil GUID; anything else must be a braced GUID.
fn guid_from(s: *const u16, out: *mut u8, bad: u32) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let g = if s.is_null() {
        [0u8; 16]
    } else {
        // SAFETY: a NUL-terminated string.
        match unsafe { crate::path::wide_to_string(s) }.as_deref().and_then(parse_guid) {
            Some(g) => g,
            None => return bad,
        }
    };
    // SAFETY: the caller's 16-byte GUID.
    unsafe { core::ptr::copy_nonoverlapping(g.as_ptr(), out, 16) };
    S_OK
}

/// OSyx registers no ProgIDs, so only GUID strings name a class.
extern "win64" fn CLSIDFromString(s: *const u16, out: *mut u8) -> u32 {
    guid_from(s, out, CO_E_CLASSSTRING)
}

extern "win64" fn IIDFromString(s: *const u16, out: *mut u8) -> u32 {
    guid_from(s, out, E_INVALIDARG)
}

extern "win64" fn StringFromGUID2(g: *const u8, buf: *mut u16, cch: i32) -> i32 {
    if g.is_null() || buf.is_null() || cch < 39 {
        return 0;
    }
    for (i, c) in guid_string(g).encode_utf16().chain(core::iter::once(0)).enumerate() {
        // SAFETY: the caller's buffer of at least 39 WCHARs.
        unsafe { *buf.add(i) = c };
    }
    39
}

extern "win64" fn StringFromCLSID(g: *const u8, out: *mut u64) -> u32 {
    if g.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let p = CoTaskMemAlloc(39 * 2);
    if p == 0 {
        return E_OUTOFMEMORY;
    }
    StringFromGUID2(g, p as *mut u16, 39);
    // SAFETY: the caller's out pointer.
    unsafe { *out = p };
    S_OK
}

extern "win64" fn StringFromIID(g: *const u8, out: *mut u64) -> u32 {
    StringFromCLSID(g, out)
}

// ---- drag and drop, the OLE clipboard -------------------------------------------------------
//
// Drop targets are registered per window. OSyx has no drag sources between
// windows, so nothing is ever dropped on them, and a drag this process
// starts ends at once as cancelled. The OLE clipboard holds the data object
// last set in this process.

const DRAGDROP_E_NOTREGISTERED: u32 = 0x8004_0100;
const DRAGDROP_E_ALREADYREGISTERED: u32 = 0x8004_0101;
const DRAGDROP_E_INVALIDHWND: u32 = 0x8004_0102;
const DRAGDROP_S_CANCEL: u32 = 0x0004_0101;
const E_FAIL: u32 = 0x8000_4005;

/// HWND to its IDropTarget (one reference held).
static DROP_TARGETS: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());
/// The IDataObject OleSetClipboard put on the clipboard (one reference).
static CLIPBOARD: Mutex<u64> = Mutex::new(0);

/// IUnknown::AddRef (slot 1) or Release (slot 2) of a COM object.
fn com_call(obj: u64, slot: usize) -> u32 {
    // SAFETY: a COM object: its first word is its vtable; slots 1 and 2
    // take only `this`.
    unsafe {
        let vtbl = *(obj as *const *const u64);
        let f: extern "win64" fn(u64) -> u32 = core::mem::transmute(*vtbl.add(slot));
        f(obj)
    }
}

fn ole_initialized() -> bool {
    COM.lock().unwrap().ole_threads.contains_key(&os_sys::gettid())
}

extern "win64" fn RegisterDragDrop(hwnd: u64, target: u64) -> u32 {
    if !ole_initialized() {
        return E_OUTOFMEMORY; // what Windows returns before OleInitialize
    }
    if target == 0 {
        return E_INVALIDARG;
    }
    if !crate::user32::is_window(hwnd) {
        return DRAGDROP_E_INVALIDHWND;
    }
    let mut t = DROP_TARGETS.lock().unwrap();
    if t.contains_key(&hwnd) {
        return DRAGDROP_E_ALREADYREGISTERED;
    }
    com_call(target, 1);
    t.insert(hwnd, target);
    S_OK
}

extern "win64" fn RevokeDragDrop(hwnd: u64) -> u32 {
    if !crate::user32::is_window(hwnd) {
        return DRAGDROP_E_INVALIDHWND;
    }
    let target = DROP_TARGETS.lock().unwrap().remove(&hwnd);
    match target {
        Some(t) => {
            com_call(t, 2);
            S_OK
        }
        None => DRAGDROP_E_NOTREGISTERED,
    }
}

extern "win64" fn DoDragDrop(data: u64, source: u64, _ok: u32, effect: *mut u32) -> u32 {
    if data == 0 || source == 0 || effect.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's DWORD.
    unsafe { *effect = 0 }; // DROPEFFECT_NONE
    DRAGDROP_S_CANCEL
}

extern "win64" fn OleSetClipboard(data: u64) -> u32 {
    if data != 0 {
        com_call(data, 1);
    }
    let old = core::mem::replace(&mut *CLIPBOARD.lock().unwrap(), data);
    if old != 0 {
        com_call(old, 2);
    }
    S_OK
}

/// The object this process put on the clipboard. With none, OSyx has no
/// system data object to hand out.
extern "win64" fn OleGetClipboard(out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let data = *CLIPBOARD.lock().unwrap();
    if data != 0 {
        com_call(data, 1);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = data };
    if data != 0 { S_OK } else { E_FAIL }
}

/// The data stays available after the flush, as rendered data would.
extern "win64" fn OleFlushClipboard() -> u32 {
    S_OK
}

extern "win64" fn OleIsCurrentClipboard(data: u64) -> u32 {
    if data != 0 && data == *CLIPBOARD.lock().unwrap() { S_OK } else { S_FALSE }
}

const TYMED_HGLOBAL: u32 = 1;
const TYMED_ISTREAM: u32 = 4;
const TYMED_ISTORAGE: u32 = 8;

/// STGMEDIUM: tymed, its handle or interface @8, pUnkForRelease @16.
extern "win64" fn ReleaseStgMedium(m: *mut u8) {
    if m.is_null() {
        return;
    }
    // SAFETY: the caller's STGMEDIUM.
    let (tymed, handle, unk) = unsafe { (*(m as *const u32), *(m.add(8) as *const u64), *(m.add(16) as *const u64)) };
    if unk != 0 {
        com_call(unk, 2);
    } else if handle != 0 {
        match tymed {
            TYMED_HGLOBAL => crate::kernel32::global_free(handle),
            TYMED_ISTREAM | TYMED_ISTORAGE => {
                com_call(handle, 2);
            }
            _ => {}
        }
    }
    // SAFETY: as above.
    unsafe { core::ptr::write_bytes(m, 0, 24) };
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut e = export_table![
        CoTaskMemAlloc,
        CoTaskMemFree,
        CoTaskMemRealloc,
        CoInitializeEx,
        CoInitialize,
        CoUninitialize,
        GetErrorInfo,
        SetErrorInfo,
        OleInitialize,
        OleUninitialize,
        CoInitializeSecurity,
        CoGetApartmentType,
        CoCreateInstance,
        CoAllowSetForegroundWindow,
        CoIncrementMTAUsage,
        CoDecrementMTAUsage,
        CoGetCallerTID,
        CLSIDFromString,
        IIDFromString,
        StringFromGUID2,
        StringFromCLSID,
        StringFromIID,
        RegisterDragDrop,
        RevokeDragDrop,
        DoDragDrop,
        OleSetClipboard,
        OleGetClipboard,
        OleFlushClipboard,
        OleIsCurrentClipboard,
        ReleaseStgMedium
    ];
    e.push(("PropVariantClear", crate::propsys::PropVariantClear as *const () as u64));
    e
}
