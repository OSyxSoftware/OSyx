//! ole32: the task allocator (CoTaskMemAlloc/Realloc/Free) over the
//! process heap, which blocks shell32 hands out (SHGetKnownFolderPath)
//! come from, and COM apartments.
//!
//! Threads enter and leave apartments as on Windows (a thread-local STA or
//! the process-wide MTA, with the same counting and mode conflicts).
//! OSyx has no COM class registrations and no COM servers, in process or
//! out of it, so class activation reports every class as not registered.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::{collections::BTreeMap, vec::Vec};
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
    security_set: bool,
}

static COM: Mutex<Com> = Mutex::new(Com { threads: BTreeMap::new(), security_set: false });

impl Com {
    /// Whether the process has an MTA (any thread entered one), which is what
    /// makes every other thread implicitly part of it.
    fn mta_exists(&self) -> bool {
        self.threads.values().any(|(apartment, _)| *apartment == Apartment::Multi)
    }
}

fn initialized() -> bool {
    COM.lock().unwrap().threads.contains_key(&os_sys::gettid())
}

extern "win64" fn CoInitializeEx(reserved: u64, flags: u32) -> u32 {
    if reserved != 0 || flags & !COINIT_KNOWN != 0 {
        return E_INVALIDARG;
    }
    let kind = if flags & COINIT_APARTMENTTHREADED != 0 { Apartment::Single } else { Apartment::Multi };
    let mut com = COM.lock().unwrap();
    match com.threads.get_mut(&os_sys::gettid()) {
        Some((k, _)) if *k != kind => RPC_E_CHANGED_MODE,
        Some((_, n)) => {
            *n += 1;
            S_FALSE
        }
        None => {
            com.threads.insert(os_sys::gettid(), (kind, 1));
            S_OK
        }
    }
}

extern "win64" fn CoInitialize(reserved: u64) -> u32 {
    CoInitializeEx(reserved, COINIT_APARTMENTTHREADED)
}

extern "win64" fn CoUninitialize() {
    let mut com = COM.lock().unwrap();
    let tid = os_sys::gettid();
    if let Some((_, n)) = com.threads.get_mut(&tid) {
        *n -= 1;
        if *n == 0 {
            com.threads.remove(&tid);
        }
    }
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

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut e = export_table![
        CoTaskMemAlloc,
        CoTaskMemFree,
        CoTaskMemRealloc,
        CoInitializeEx,
        CoInitialize,
        CoUninitialize,
        CoInitializeSecurity,
        CoGetApartmentType,
        CoCreateInstance,
        CoAllowSetForegroundWindow
    ];
    e.push(("PropVariantClear", crate::propsys::PropVariantClear as *const () as u64));
    e
}
