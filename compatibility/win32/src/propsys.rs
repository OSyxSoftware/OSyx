//! propsys: an in-memory IPropertyStore (PSCreateMemoryPropertyStore),
//! PropVariantToString and VariantCompare. PROPVARIANTs are copied the way
//! PropVariantCopy does for the scalar types, FILETIME, strings
//! (VT_LPWSTR, VT_BSTR), VT_CLSID and VT_UNKNOWN; vectors, arrays, blobs
//! and streams are not supported and are refused with E_INVALIDARG.
//! Window property stores (SHGetPropertyStoreForWindow) keep each window's
//! values as text, the way Windows hands them back (VT_LPWSTR), shared by
//! every store of that window and dropped with the window.
#![allow(non_snake_case)]
use crate::path;
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};
use os_std::sync::Mutex;

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_FAIL: u32 = 0x8000_4005;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000E;
const STRSAFE_E_INSUFFICIENT_BUFFER: u32 = 0x8007_007A;
const TYPE_E_TYPEMISMATCH: u32 = 0x8002_8CA0;

const VT_EMPTY: u16 = 0;
const VT_NULL: u16 = 1;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;
const VT_UNKNOWN: u16 = 13;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;
const VT_LPWSTR: u16 = 31;
const VT_FILETIME: u16 = 64;
const VT_CLSID: u16 = 72;

const IID_IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// {886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}
const IID_IPROPERTYSTORE: [u8; 16] = [0xEB, 0x8E, 0x6D, 0x88, 0xF2, 0x8C, 0x46, 0x44, 0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99];

type PropVariant = [u8; 24];

fn vt(v: &PropVariant) -> u16 {
    u16::from_le_bytes([v[0], v[1]])
}

fn word(v: &PropVariant) -> u64 {
    u64::from_le_bytes(v[8..16].try_into().unwrap())
}

fn wide_len(p: *const u16) -> usize {
    // SAFETY: a NUL-terminated UTF-16 string.
    (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count()
}

/// PropVariantCopy for the supported types; Err(E_INVALIDARG) otherwise.
fn copy(src: &PropVariant) -> Result<PropVariant, u32> {
    let mut out = *src;
    let p = word(src);
    let dup = |bytes: usize| -> Result<u64, u32> {
        let d = crate::ole32::CoTaskMemAlloc(bytes);
        if d == 0 {
            return Err(E_OUTOFMEMORY);
        }
        // SAFETY: `p` holds `bytes` bytes; `d` is a fresh block of that size.
        unsafe { core::ptr::copy_nonoverlapping(p as *const u8, d as *mut u8, bytes) };
        Ok(d)
    };
    let new = match vt(src) {
        VT_EMPTY | VT_NULL | VT_I2 | VT_I4 | VT_R4 | VT_R8 | VT_BOOL | VT_I1 | VT_UI1 | VT_UI2 | VT_UI4 | VT_I8 | VT_UI8 | VT_INT
        | VT_UINT | VT_FILETIME => return Ok(out),
        VT_LPWSTR if p != 0 => dup((wide_len(p as *const u16) + 1) * 2)?,
        VT_CLSID if p != 0 => dup(16)?,
        VT_BSTR if p != 0 => {
            let b = crate::oleaut32::copy_bstr(p);
            if b == 0 {
                return Err(E_OUTOFMEMORY);
            }
            b
        }
        VT_UNKNOWN if p != 0 => {
            // SAFETY: a COM interface: IUnknown::AddRef is vtable slot 1.
            unsafe {
                let vtbl = *(p as *const *const u64);
                let add_ref: extern "win64" fn(u64) -> u32 = core::mem::transmute(*vtbl.add(1));
                add_ref(p);
            }
            p
        }
        VT_LPWSTR | VT_CLSID | VT_BSTR | VT_UNKNOWN => 0,
        _ => return Err(E_INVALIDARG),
    };
    out[8..16].copy_from_slice(&new.to_le_bytes());
    Ok(out)
}

/// PropVariantClear for the supported types.
fn clear(v: &PropVariant) {
    let p = word(v);
    if p == 0 {
        return;
    }
    match vt(v) {
        VT_LPWSTR | VT_CLSID => crate::ole32::CoTaskMemFree(p),
        VT_BSTR => crate::oleaut32::free_bstr(p),
        VT_UNKNOWN => {
            // SAFETY: IUnknown::Release is vtable slot 2.
            unsafe {
                let vtbl = *(p as *const *const u64);
                let release: extern "win64" fn(u64) -> u32 = core::mem::transmute(*vtbl.add(2));
                release(p);
            }
        }
        _ => {}
    }
}

const DISP_E_BADVARTYPE: u32 = 0x8002_0008;

/// ole32's PropVariantClear for the types this module copies; others
/// fail with DISP_E_BADVARTYPE and are left as they are.
pub(crate) extern "win64" fn PropVariantClear(v: *mut PropVariant) -> u32 {
    if v.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's PROPVARIANT.
    let pv = unsafe { *v };
    if copy_supported(vt(&pv)) {
        clear(&pv);
        // SAFETY: as above.
        unsafe { *v = [0; 24] };
        S_OK
    } else {
        DISP_E_BADVARTYPE
    }
}

fn copy_supported(t: u16) -> bool {
    matches!(
        t,
        VT_EMPTY | VT_NULL | VT_I2 | VT_I4 | VT_R4 | VT_R8 | VT_BOOL | VT_I1 | VT_UI1 | VT_UI2 | VT_UI4 | VT_I8 | VT_UI8 | VT_INT | VT_UINT
            | VT_FILETIME | VT_LPWSTR | VT_CLSID | VT_BSTR | VT_UNKNOWN
    )
}

// ---- the memory property store ----------------------------------------------------------

/// IPropertyStore's vtable: IUnknown, then GetCount, GetAt, GetValue,
/// SetValue and Commit.
#[repr(C)]
struct Vtbl {
    query_interface: extern "win64" fn(u64, *const [u8; 16], *mut u64) -> u32,
    add_ref: extern "win64" fn(u64) -> u32,
    release: extern "win64" fn(u64) -> u32,
    get_count: extern "win64" fn(u64, *mut u32) -> u32,
    get_at: extern "win64" fn(u64, u32, *mut [u8; 20]) -> u32,
    get_value: extern "win64" fn(u64, *const [u8; 20], *mut PropVariant) -> u32,
    set_value: extern "win64" fn(u64, *const [u8; 20], *const PropVariant) -> u32,
    commit: extern "win64" fn(u64) -> u32,
}

#[repr(C)]
struct Store {
    vtbl: &'static Vtbl,
    refs: AtomicU32,
    /// (PROPERTYKEY: fmtid + pid, value) in insertion order.
    props: Mutex<Vec<([u8; 20], PropVariant)>>,
}

static VTBL: Vtbl = Vtbl {
    query_interface: QueryInterface,
    add_ref: AddRef,
    release: Release,
    get_count: GetCount,
    get_at: GetAt,
    get_value: GetValue,
    set_value: SetValue,
    commit: Commit,
};

fn store<'a>(this: u64) -> &'a Store {
    // SAFETY: `this` is a Store made by PSCreateMemoryPropertyStore.
    unsafe { &*(this as *const Store) }
}

extern "win64" fn QueryInterface(this: u64, riid: *const [u8; 16], out: *mut u64) -> u32 {
    if riid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's IID and out pointer.
    unsafe {
        if *riid == IID_IUNKNOWN || *riid == IID_IPROPERTYSTORE {
            AddRef(this);
            *out = this;
            return S_OK;
        }
        *out = 0;
    }
    E_NOINTERFACE
}

extern "win64" fn AddRef(this: u64) -> u32 {
    store(this).refs.fetch_add(1, Ordering::AcqRel) + 1
}

extern "win64" fn Release(this: u64) -> u32 {
    let left = store(this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if left == 0 {
        // SAFETY: the last reference: the Box made in PSCreateMemoryPropertyStore.
        let s = unsafe { Box::from_raw(this as *mut Store) };
        for (_, v) in s.props.lock().unwrap().iter() {
            clear(v);
        }
    }
    left
}

extern "win64" fn GetCount(this: u64, count: *mut u32) -> u32 {
    if count.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *count = store(this).props.lock().unwrap().len() as u32 };
    S_OK
}

extern "win64" fn GetAt(this: u64, i: u32, key: *mut [u8; 20]) -> u32 {
    if key.is_null() {
        return E_POINTER;
    }
    match store(this).props.lock().unwrap().get(i as usize) {
        Some((k, _)) => {
            // SAFETY: the caller's PROPERTYKEY.
            unsafe { *key = *k };
            S_OK
        }
        None => E_INVALIDARG,
    }
}

/// An absent property reads as VT_EMPTY.
extern "win64" fn GetValue(this: u64, key: *const [u8; 20], out: *mut PropVariant) -> u32 {
    if key.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's PROPERTYKEY.
    let k = unsafe { *key };
    let props = store(this).props.lock().unwrap();
    let v = match props.iter().find(|(x, _)| *x == k) {
        Some((_, v)) => match copy(v) {
            Ok(c) => c,
            Err(e) => return e,
        },
        None => [0; 24],
    };
    // SAFETY: the caller's PROPVARIANT.
    unsafe { *out = v };
    S_OK
}

extern "win64" fn SetValue(this: u64, key: *const [u8; 20], value: *const PropVariant) -> u32 {
    if key.is_null() || value.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's PROPERTYKEY and PROPVARIANT.
    let (k, v) = unsafe { (*key, *value) };
    let c = match copy(&v) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut props = store(this).props.lock().unwrap();
    match props.iter_mut().find(|(x, _)| *x == k) {
        Some(slot) => {
            clear(&slot.1);
            slot.1 = c;
        }
        None => props.push((k, c)),
    }
    S_OK
}

/// Memory is the store: there is nothing to write back.
extern "win64" fn Commit(_this: u64) -> u32 {
    S_OK
}

extern "win64" fn PSCreateMemoryPropertyStore(riid: *const [u8; 16], out: *mut u64) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    let s = Box::new(Store { vtbl: &VTBL, refs: AtomicU32::new(1), props: Mutex::new(Vec::new()) });
    let this = Box::into_raw(s) as u64;
    let hr = QueryInterface(this, riid, out);
    Release(this);
    hr
}

// ---- window property stores ----------------------------------------------------------------

/// Per window (or per detached store, keyed by its address): PROPERTYKEY and
/// the value's text with its NUL, in insertion order.
type WindowProps = BTreeMap<u64, Vec<([u8; 20], Vec<u16>)>>;
static WINDOW_PROPS: Mutex<WindowProps> = Mutex::new(BTreeMap::new());

#[repr(C)]
struct WindowStore {
    vtbl: &'static Vtbl,
    refs: AtomicU32,
    /// WINDOW_PROPS key.
    key: u64,
    detached: bool,
}

static WINDOW_VTBL: Vtbl = Vtbl {
    query_interface: WsQueryInterface,
    add_ref: WsAddRef,
    release: WsRelease,
    get_count: WsGetCount,
    get_at: WsGetAt,
    get_value: WsGetValue,
    set_value: WsSetValue,
    commit: Commit,
};

fn window_store<'a>(this: u64) -> &'a WindowStore {
    // SAFETY: `this` is a WindowStore made by window_property_store.
    unsafe { &*(this as *const WindowStore) }
}

extern "win64" fn WsQueryInterface(this: u64, riid: *const [u8; 16], out: *mut u64) -> u32 {
    if riid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's IID and out pointer.
    unsafe {
        if *riid == IID_IUNKNOWN || *riid == IID_IPROPERTYSTORE {
            WsAddRef(this);
            *out = this;
            return S_OK;
        }
        *out = 0;
    }
    E_NOINTERFACE
}

extern "win64" fn WsAddRef(this: u64) -> u32 {
    window_store(this).refs.fetch_add(1, Ordering::AcqRel) + 1
}

extern "win64" fn WsRelease(this: u64) -> u32 {
    let left = window_store(this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if left == 0 {
        // SAFETY: the last reference: the Box made in window_property_store.
        let s = unsafe { Box::from_raw(this as *mut WindowStore) };
        if s.detached {
            WINDOW_PROPS.lock().unwrap().remove(&s.key);
        }
    }
    left
}

/// Windows reports an empty window store with E_FAIL and a count of 0.
extern "win64" fn WsGetCount(this: u64, count: *mut u32) -> u32 {
    if count.is_null() {
        return E_POINTER;
    }
    let n = WINDOW_PROPS.lock().unwrap().get(&window_store(this).key).map_or(0, |p| p.len());
    // SAFETY: the caller's out pointer.
    unsafe { *count = n as u32 };
    if n == 0 { E_FAIL } else { S_OK }
}

extern "win64" fn WsGetAt(this: u64, i: u32, key: *mut [u8; 20]) -> u32 {
    if key.is_null() {
        return E_POINTER;
    }
    let props = WINDOW_PROPS.lock().unwrap();
    match props.get(&window_store(this).key).and_then(|p| p.get(i as usize)) {
        Some((k, _)) => {
            // SAFETY: the caller's PROPERTYKEY.
            unsafe { *key = *k };
            S_OK
        }
        None => E_INVALIDARG,
    }
}

/// A stored value comes back as a fresh VT_LPWSTR with S_FALSE, as on
/// Windows; an absent one as VT_EMPTY with S_OK.
extern "win64" fn WsGetValue(this: u64, key: *const [u8; 20], out: *mut PropVariant) -> u32 {
    if key.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's PROPERTYKEY.
    let k = unsafe { *key };
    let props = WINDOW_PROPS.lock().unwrap();
    let mut v = [0u8; 24];
    let found = props.get(&window_store(this).key).and_then(|p| p.iter().find(|(x, _)| *x == k));
    let hr = match found {
        Some((_, text)) => {
            let d = crate::ole32::CoTaskMemAlloc(text.len() * 2);
            if d == 0 {
                return E_OUTOFMEMORY;
            }
            // SAFETY: a fresh block of text.len() UTF-16 units.
            unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), d as *mut u16, text.len()) };
            v[0..2].copy_from_slice(&VT_LPWSTR.to_le_bytes());
            v[8..16].copy_from_slice(&d.to_le_bytes());
            S_FALSE
        }
        None => S_OK,
    };
    // SAFETY: the caller's PROPVARIANT.
    unsafe { *out = v };
    hr
}

/// VT_EMPTY removes the property; any other value is kept as its text.
extern "win64" fn WsSetValue(this: u64, key: *const [u8; 20], value: *const PropVariant) -> u32 {
    if key.is_null() || value.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's PROPERTYKEY and PROPVARIANT.
    let (k, v) = unsafe { (*key, *value) };
    let mut props = WINDOW_PROPS.lock().unwrap();
    let list = props.entry(window_store(this).key).or_default();
    if vt(&v) == VT_EMPTY {
        list.retain(|(x, _)| *x != k);
        return S_OK;
    }
    let mut units: Vec<u16> = match text(&v) {
        Ok(t) => t.encode_utf16().collect(),
        Err(e) => return e,
    };
    units.push(0);
    match list.iter_mut().find(|(x, _)| *x == k) {
        Some(slot) => slot.1 = units,
        None => list.push((k, units)),
    }
    S_OK
}

/// SHGetPropertyStoreForWindow. A handle that is not a window gets an
/// empty store of its own (Windows does not reject it), and a destroyed
/// window's values are gone.
pub(crate) fn window_property_store(hwnd: u64, riid: *const [u8; 16], out: *mut u64) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    if hwnd == 0 {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
        return E_INVALIDARG;
    }
    let live = crate::user32::is_window(hwnd);
    let mut props = WINDOW_PROPS.lock().unwrap();
    props.retain(|&k, _| k & DETACHED != 0 || crate::user32::is_window(k));
    drop(props);
    let s = Box::new(WindowStore { vtbl: &WINDOW_VTBL, refs: AtomicU32::new(1), key: hwnd, detached: !live });
    let this = Box::into_raw(s) as u64;
    if !live {
        // SAFETY: just made; nothing else refers to it yet.
        unsafe { (*(this as *mut WindowStore)).key = this | DETACHED };
    }
    let hr = WsQueryInterface(this, riid, out);
    WsRelease(this);
    hr
}

/// Marks WINDOW_PROPS keys of detached stores (HWNDs never have this bit).
const DETACHED: u64 = 1 << 63;

// ---- conversions ----------------------------------------------------------------------------

/// The text of a PROPVARIANT, as PropVariantChangeType to VT_LPWSTR gives it.
fn text(v: &PropVariant) -> Result<String, u32> {
    let p = word(v);
    let int = |n: usize| -> i64 {
        let b = &v[8..8 + n];
        match n {
            1 => b[0] as i8 as i64,
            2 => i16::from_le_bytes([b[0], b[1]]) as i64,
            4 => i32::from_le_bytes(b.try_into().unwrap()) as i64,
            _ => i64::from_le_bytes(b.try_into().unwrap()),
        }
    };
    Ok(match vt(v) {
        VT_EMPTY => String::new(),
        VT_LPWSTR | VT_BSTR if p == 0 => String::new(),
        // SAFETY: a NUL-terminated string.
        VT_LPWSTR | VT_BSTR => unsafe { path::wide_to_string(p as *const u16) }.unwrap_or_default(),
        VT_I1 => alloc::format!("{}", int(1)),
        VT_I2 => alloc::format!("{}", int(2)),
        VT_I4 | VT_INT => alloc::format!("{}", int(4)),
        VT_I8 => alloc::format!("{}", int(8)),
        VT_UI1 => alloc::format!("{}", v[8]),
        VT_UI2 => alloc::format!("{}", u16::from_le_bytes([v[8], v[9]])),
        VT_UI4 | VT_UINT => alloc::format!("{}", u32::from_le_bytes(v[8..12].try_into().unwrap())),
        VT_UI8 => alloc::format!("{}", p),
        VT_BOOL => String::from(if int(2) != 0 { "1" } else { "0" }),
        _ => return Err(TYPE_E_TYPEMISMATCH),
    })
}

extern "win64" fn PropVariantToString(v: *const PropVariant, buf: *mut u16, cch: u32) -> u32 {
    if v.is_null() || buf.is_null() || cch == 0 {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's PROPVARIANT.
    let s = match text(unsafe { &*v }) {
        Ok(s) => s,
        Err(e) => {
            // SAFETY: the caller's buffer of `cch` units.
            unsafe { *buf = 0 };
            return e;
        }
    };
    let w: Vec<u16> = s.encode_utf16().collect();
    let n = w.len().min(cch as usize - 1);
    // SAFETY: as above; truncated with its NUL when too long, as StringCchCopy does.
    unsafe {
        core::ptr::copy_nonoverlapping(w.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    if n < w.len() { STRSAFE_E_INSUFFICIENT_BUFFER } else { S_OK }
}

/// VARIANTs: empty sorts first; numbers compare by value and strings
/// case-insensitively; a number and a string compare as text.
extern "win64" fn VariantCompare(a: *const PropVariant, b: *const PropVariant) -> i32 {
    if a.is_null() || b.is_null() {
        return 0;
    }
    // SAFETY: the caller's VARIANTs.
    let (x, y) = unsafe { (*a, *b) };
    let num = |v: &PropVariant| -> Option<f64> {
        let w = word(v);
        Some(match vt(v) {
            VT_I1 => v[8] as i8 as f64,
            VT_UI1 => v[8] as f64,
            VT_I2 | VT_BOOL => i16::from_le_bytes([v[8], v[9]]) as f64,
            VT_UI2 => u16::from_le_bytes([v[8], v[9]]) as f64,
            VT_I4 | VT_INT => i32::from_le_bytes(v[8..12].try_into().unwrap()) as f64,
            VT_UI4 | VT_UINT => u32::from_le_bytes(v[8..12].try_into().unwrap()) as f64,
            VT_I8 => w as i64 as f64,
            VT_UI8 => w as f64,
            VT_R4 => f32::from_le_bytes(v[8..12].try_into().unwrap()) as f64,
            VT_R8 => f64::from_bits(w),
            _ => return None,
        })
    };
    let order = match (vt(&x) == VT_EMPTY, vt(&y) == VT_EMPTY) {
        (true, true) => core::cmp::Ordering::Equal,
        (true, false) => core::cmp::Ordering::Less,
        (false, true) => core::cmp::Ordering::Greater,
        _ => match (num(&x), num(&y)) {
            (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(core::cmp::Ordering::Equal),
            _ => {
                let (s, t) = (text(&x).unwrap_or_default().to_lowercase(), text(&y).unwrap_or_default().to_lowercase());
                s.cmp(&t)
            }
        },
    };
    order as i32
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![PSCreateMemoryPropertyStore, PropVariantToString, VariantCompare]
}
