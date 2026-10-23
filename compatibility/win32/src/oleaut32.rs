//! oleaut32: BSTRs and VARIANT initialisation and clearing.
//!
//! A BSTR is a UTF-16 string preceded by its byte length (a DWORD) and
//! followed by a NUL; the pointer handed out is the string itself. Blocks
//! come from the process heap and are never cached, so SetOaNoCache has
//! nothing to turn off. VARIANTs holding SAFEARRAYs or records are not
//! supported (no SAFEARRAY or IRecordInfo implementation): clearing one
//! fails with DISP_E_BADVARTYPE and leaves it untouched.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::vec::Vec;

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const DISP_E_BADVARTYPE: u32 = 0x8002_0008;

/// The heap block of a BSTR: the 4-byte length sits right before the
/// string, at the end of a 16-byte header.
const HEADER: u64 = 16;

/// A BSTR of `bytes` bytes, copied from `src` (or zeroed when NULL).
fn alloc_bstr(src: *const u8, bytes: u32) -> u64 {
    let Some(block) = kernel32::heap_alloc(HEADER as usize + bytes as usize + 2, true) else { return 0 };
    let s = block + HEADER;
    // SAFETY: a fresh block of HEADER + bytes + 2 bytes; `src` has `bytes`.
    unsafe {
        *((s - 4) as *mut u32) = bytes;
        if !src.is_null() {
            core::ptr::copy_nonoverlapping(src, s as *mut u8, bytes as usize);
        }
    }
    s
}

fn wide_len(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: a NUL-terminated UTF-16 string.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

extern "win64" fn SysAllocString(s: *const u16) -> u64 {
    if s.is_null() {
        return 0;
    }
    alloc_bstr(s as *const u8, (wide_len(s) * 2) as u32)
}

extern "win64" fn SysAllocStringLen(s: *const u16, len: u32) -> u64 {
    if len > u32::MAX / 2 - 16 {
        return 0;
    }
    alloc_bstr(s as *const u8, len * 2)
}

extern "win64" fn SysAllocStringByteLen(s: *const u8, len: u32) -> u64 {
    if len > u32::MAX - 32 {
        return 0;
    }
    alloc_bstr(s, len)
}

extern "win64" fn SysFreeString(s: u64) {
    if s != 0 {
        kernel32::heap_free(s - HEADER);
    }
}

extern "win64" fn SysStringByteLen(s: u64) -> u32 {
    if s == 0 {
        return 0;
    }
    // SAFETY: a BSTR: its length precedes it.
    unsafe { *((s - 4) as *const u32) }
}

extern "win64" fn SysStringLen(s: u64) -> u32 {
    SysStringByteLen(s) / 2
}

/// Replaces *old with a new BSTR (the source may overlap the old one).
fn realloc(old: *mut u64, src: *const u8, bytes: u32) -> i32 {
    if old.is_null() {
        return 0;
    }
    let new = alloc_bstr(src, bytes);
    if new == 0 {
        return 0;
    }
    // SAFETY: the caller's BSTR pointer.
    unsafe {
        SysFreeString(*old);
        *old = new;
    }
    1
}

extern "win64" fn SysReAllocString(old: *mut u64, s: *const u16) -> i32 {
    if s.is_null() {
        return realloc(old, core::ptr::null(), 0);
    }
    realloc(old, s as *const u8, (wide_len(s) * 2) as u32)
}

extern "win64" fn SysReAllocStringLen(old: *mut u64, s: *const u16, len: u32) -> i32 {
    if len > u32::MAX / 2 - 16 {
        return 0;
    }
    realloc(old, s as *const u8, len * 2)
}

extern "win64" fn SetOaNoCache() {}

// ---- VARIANT ----------------------------------------------------------------------------

const VT_EMPTY: u16 = 0;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_UNKNOWN: u16 = 13;
const VT_RECORD: u16 = 36;
const VT_ARRAY: u16 = 0x2000;
const VT_BYREF: u16 = 0x4000;

/// Types a VARIANT holds by value without owning anything.
fn plain(vt: u16) -> bool {
    matches!(vt, 0..=7 | 10 | 11 | 14 | 16..=23 | 25 | 26)
}

/// A copy of a BSTR (PropVariantCopy uses it).
pub(crate) fn copy_bstr(b: u64) -> u64 {
    SysAllocStringByteLen(b as *const u8, SysStringByteLen(b))
}

pub(crate) fn free_bstr(b: u64) {
    SysFreeString(b);
}

extern "win64" fn VariantInit(v: *mut u8) {
    if !v.is_null() {
        // SAFETY: the caller's 24-byte VARIANT; only vt and the reserved words are set.
        unsafe { core::ptr::write_bytes(v, 0, 8) };
    }
}

extern "win64" fn VariantClear(v: *mut u8) -> u32 {
    if v.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's VARIANT.
    let vt = unsafe { *(v as *const u16) };
    let value = unsafe { *(v.add(8) as *const u64) };
    if vt & VT_BYREF != 0 {
        // References are not owned; only the type must be valid.
        if !plain(vt & !VT_BYREF) && ![VT_BSTR, VT_DISPATCH, VT_UNKNOWN, 12].contains(&(vt & !VT_BYREF)) {
            return DISP_E_BADVARTYPE;
        }
    } else if vt & VT_ARRAY != 0 || vt == VT_RECORD {
        return if value == 0 { S_OK } else { DISP_E_BADVARTYPE };
    } else if vt == VT_BSTR {
        SysFreeString(value);
    } else if vt == VT_DISPATCH || vt == VT_UNKNOWN {
        if value != 0 {
            // SAFETY: a COM interface pointer: IUnknown::Release is vtable slot 2.
            unsafe {
                let vtbl = *(value as *const *const u64);
                let release: extern "win64" fn(u64) -> u32 = core::mem::transmute(*vtbl.add(2));
                release(value);
            }
        }
    } else if !plain(vt) {
        return DISP_E_BADVARTYPE;
    }
    // SAFETY: as above.
    unsafe { *(v as *mut u16) = VT_EMPTY };
    S_OK
}

extern "win64" fn VariantCopy(dst: *mut u8, src: *const u8) -> u32 {
    if dst.is_null() || src.is_null() {
        return E_INVALIDARG;
    }
    if core::ptr::eq(dst, src) {
        return S_OK;
    }
    // SAFETY: the caller's VARIANTs (24 bytes each).
    let (vt, value) = unsafe { (*(src as *const u16), *(src.add(8) as *const u64)) };
    if vt & VT_ARRAY != 0 && vt & VT_BYREF == 0 || vt == VT_RECORD {
        return DISP_E_BADVARTYPE; // no SAFEARRAY or record copies
    }
    let hr = VariantClear(dst);
    if hr != S_OK {
        return hr;
    }
    // SAFETY: as above.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, 24) };
    if vt == VT_BSTR && value != 0 {
        let copy = SysAllocStringByteLen(value as *const u8, SysStringByteLen(value));
        if copy == 0 {
            // SAFETY: as above.
            unsafe { *(dst as *mut u16) = VT_EMPTY };
            return E_OUTOFMEMORY;
        }
        // SAFETY: as above.
        unsafe { *(dst.add(8) as *mut u64) = copy };
    } else if (vt == VT_DISPATCH || vt == VT_UNKNOWN) && value != 0 {
        // SAFETY: IUnknown::AddRef is vtable slot 1.
        unsafe {
            let vtbl = *(value as *const *const u64);
            let add_ref: extern "win64" fn(u64) -> u32 = core::mem::transmute(*vtbl.add(1));
            add_ref(value);
        }
    }
    S_OK
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        SysAllocString,
        SysReAllocString,
        SysAllocStringLen,
        SysReAllocStringLen,
        SysAllocStringByteLen,
        SysFreeString,
        SysStringLen,
        SysStringByteLen,
        SetOaNoCache,
        VariantInit,
        VariantClear,
        VariantCopy,
    ]
}

/// The ordinals programs import these by.
pub fn ordinals() -> Vec<(u16, u64)> {
    let f = |p: *const ()| p as usize as u64;
    alloc::vec![
        (2, f(SysAllocString as *const ())),
        (3, f(SysReAllocString as *const ())),
        (4, f(SysAllocStringLen as *const ())),
        (5, f(SysReAllocStringLen as *const ())),
        (6, f(SysFreeString as *const ())),
        (7, f(SysStringLen as *const ())),
        (8, f(VariantInit as *const ())),
        (9, f(VariantClear as *const ())),
        (10, f(VariantCopy as *const ())),
        (149, f(SysStringByteLen as *const ())),
        (150, f(SysAllocStringByteLen as *const ())),
        (327, f(SetOaNoCache as *const ())),
    ]
}
