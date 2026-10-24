//! shlwapi path helpers and the WinRT string/activation entry points of
//! combase. HSTRINGs are real counted strings; OSyx registers no WinRT
//! runtime classes, so activation reports REGDB_E_CLASSNOTREG.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::vec::Vec;

const MAX_PATH: usize = 260;

/// # Safety
/// `p` is a NUL-terminated UTF-16 string.
unsafe fn read(p: *const u16) -> Vec<u16> {
    let mut n = 0;
    // SAFETY: the caller's guarantee.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units were just read.
    unsafe { core::slice::from_raw_parts(p, n) }.to_vec()
}

/// # Safety
/// `p` has room for MAX_PATH units.
unsafe fn write(p: *mut u16, s: &[u16]) {
    // SAFETY: the caller's guarantee; s.len() < MAX_PATH is checked by callers.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
        *p.add(s.len()) = 0;
    }
}

const BS: u16 = b'\\' as u16;

/// Length of the root ("C:\", "\", "\\server\share") of a path.
fn root_len(s: &[u16]) -> usize {
    if s.len() >= 2 && s[1] == b':' as u16 {
        return if s.get(2) == Some(&BS) { 3 } else { 2 };
    }
    if s.len() >= 2 && s[0] == BS && s[1] == BS {
        // \\server\share
        let mut parts = 0;
        for (i, &c) in s.iter().enumerate().skip(2) {
            if c == BS {
                parts += 1;
                if parts == 2 {
                    return i;
                }
            }
        }
        return s.len();
    }
    usize::from(s.first() == Some(&BS))
}

/// Removes the last element and its backslash; TRUE if anything changed.
extern "win64" fn PathRemoveFileSpecW(p: *mut u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path buffer.
    let s = unsafe { read(p) };
    let root = root_len(&s);
    let cut = match s.iter().rposition(|&c| c == BS) {
        Some(i) if i >= root => i,
        Some(_) | None if s.len() > root => root,
        _ => return 0,
    };
    let cut = if cut < root { root } else { cut };
    if cut == s.len() {
        return 0;
    }
    // SAFETY: shortening in place.
    unsafe { *p.add(cut) = 0 };
    1
}

/// Resolves "." and ".." elements after the root.
fn canonicalize(s: &[u16]) -> Vec<u16> {
    let root = root_len(s);
    let mut parts: Vec<&[u16]> = Vec::new();
    for part in s[root..].split(|&c| c == BS) {
        match part {
            [] => {}
            [d] if *d == b'.' as u16 => {}
            [a, b] if *a == b'.' as u16 && *b == b'.' as u16 => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = s[..root].to_vec();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 || (!out.is_empty() && out.last() != Some(&BS)) {
            out.push(BS);
        }
        out.extend_from_slice(p);
    }
    if s.last() == Some(&BS) && !parts.is_empty() {
        out.push(BS);
    }
    out
}

extern "win64" fn PathAppendW(p: *mut u16, more: *const u16) -> i32 {
    if p.is_null() || more.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (base, add) = unsafe { (read(p), read(more)) };
    if add.len() >= 2 && add[0] == BS && add[1] == BS {
        return 0; // a UNC path cannot be appended
    }
    let mut joined = base.clone();
    let add_trimmed: &[u16] = {
        let start = add.iter().position(|&c| c != BS).unwrap_or(add.len());
        &add[start..]
    };
    if !add_trimmed.is_empty() {
        if !joined.is_empty() && joined.last() != Some(&BS) {
            joined.push(BS);
        }
        joined.extend_from_slice(add_trimmed);
    }
    let out = canonicalize(&joined);
    if out.len() >= MAX_PATH {
        return 0;
    }
    // SAFETY: the caller's MAX_PATH buffer.
    unsafe { write(p, &out) };
    1
}

fn lower(c: u16) -> u16 {
    char::from_u32(c as u32).map_or(c, |ch| {
        let mut l = ch.to_lowercase();
        match (l.next(), l.next()) {
            (Some(x), None) if (x as u32) < 0x10000 => x as u16,
            _ => c,
        }
    })
}

fn wildcard(name: &[u16], pat: &[u16]) -> bool {
    let (mut n, mut p) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while n < name.len() {
        if p < pat.len() && (pat[p] == b'?' as u16 || lower(pat[p]) == lower(name[n])) {
            n += 1;
            p += 1;
        } else if p < pat.len() && pat[p] == b'*' as u16 {
            star = Some(p);
            mark = n;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' as u16 {
        p += 1;
    }
    p == pat.len()
}

/// Matches the file name against ';'-separated wildcard specs, ignoring
/// case; "*.*" matches every name.
extern "win64" fn PathMatchSpecW(file: *const u16, spec: *const u16) -> i32 {
    if file.is_null() || spec.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (name, specs) = unsafe { (read(file), read(spec)) };
    let leaf_start = name.iter().rposition(|&c| c == BS).map_or(0, |i| i + 1);
    let leaf = &name[leaf_start..];
    for s in specs.split(|&c| c == b';' as u16) {
        let s: Vec<u16> = s.iter().copied().skip_while(|&c| c == b' ' as u16).collect();
        if s.is_empty() {
            continue;
        }
        let star_dot_star = [b'*' as u16, b'.' as u16, b'*' as u16];
        if s == star_dot_star || wildcard(leaf, &s) || wildcard(&name, &s) {
            return 1;
        }
    }
    0
}


/// PathCchCanonicalize: removes "." and ".." components (never above the
/// root) of a path; the result must fit `cch` units with its NUL.
extern "win64" fn PathCchCanonicalize(out: *mut u16, cch: usize, input: *const u16) -> u32 {
    const E_INVALIDARG: u32 = 0x8007_0057;
    const E_INSUFFICIENT_BUFFER: u32 = 0x8007_007A;
    if out.is_null() || cch == 0 || cch > 32768 {
        return E_INVALIDARG;
    }
    // SAFETY: a NUL-terminated path.
    let Some(p) = (unsafe { crate::path::wide_to_string(input) }) else { return E_INVALIDARG };
    let sep = '\\';
    // A root that ".." cannot climb above: "C:\", "\\server\share\" or "\".
    let (root, rest) = if p.len() >= 3 && p.as_bytes()[1] == b':' && p[2..].starts_with(sep) {
        (alloc::string::String::from(&p[..3]), &p[3..])
    } else if let Some(unc) = p.strip_prefix("\\\\") {
        let parts: Vec<&str> = unc.splitn(3, sep).collect();
        let root = alloc::format!("\\\\{}\\{}", parts.first().unwrap_or(&""), parts.get(1).unwrap_or(&""));
        (if parts.len() > 2 { alloc::format!("{root}\\") } else { root }, *parts.get(2).unwrap_or(&""))
    } else if let Some(r) = p.strip_prefix(sep) {
        (alloc::string::String::from("\\"), r)
    } else {
        (alloc::string::String::new(), p.as_str())
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in rest.split(sep) {
        match c {
            "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    // A trailing "." or ".." leaves no empty last component behind.
    if (rest.ends_with("\\.") || rest.ends_with("\\..") || rest == "." || rest == "..") && parts.last() == Some(&"") {
        parts.pop();
    }
    let mut s = root + &parts.join("\\");
    if s.is_empty() {
        s.push('\\');
    }
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.len() + 1 > cch {
        // SAFETY: the caller's buffer of at least one unit.
        unsafe { *out = 0 };
        return E_INSUFFICIENT_BUFFER;
    }
    // SAFETY: the caller's buffer of `cch` units.
    unsafe {
        core::ptr::copy_nonoverlapping(w.as_ptr(), out, w.len());
        *out.add(w.len()) = 0;
    }
    0
}

/// The kernelbase export address of PathCchCanonicalize.
pub fn path_cch_canonicalize() -> u64 {
    PathCchCanonicalize as *const () as u64
}

/// Joins `dir` and `file` the way PathCombineW does: an absolute `file`
/// (drive or UNC) replaces `dir`, a rooted one ("\x") keeps only the root
/// of `dir`; the result is canonicalized. NULL if both are NULL or the
/// result does not fit MAX_PATH (the buffer is then empty).
extern "win64" fn PathCombineW(dest: *mut u16, dir: *const u16, file: *const u16) -> *mut u16 {
    if dest.is_null() {
        return core::ptr::null_mut();
    }
    let fail = || {
        // SAFETY: the caller's MAX_PATH buffer.
        unsafe { *dest = 0 };
        core::ptr::null_mut()
    };
    if dir.is_null() && file.is_null() {
        return fail();
    }
    // SAFETY: NUL-terminated strings (or NULL).
    let d = if dir.is_null() { Vec::new() } else { unsafe { read(dir) } };
    // SAFETY: as above.
    let f = if file.is_null() { Vec::new() } else { unsafe { read(file) } };
    let absolute = (f.len() >= 2 && f[1] == b':' as u16) || (f.len() >= 2 && f[0] == BS && f[1] == BS);
    let joined = if absolute || d.is_empty() {
        f
    } else if f.first() == Some(&BS) {
        let mut j = d[..root_len(&d)].to_vec();
        while j.last() == Some(&BS) {
            j.pop();
        }
        j.extend_from_slice(&f);
        j
    } else {
        let mut j = d;
        if !f.is_empty() {
            if j.last() != Some(&BS) {
                j.push(BS);
            }
            j.extend_from_slice(&f);
        }
        j
    };
    let out = canonicalize(&joined);
    if out.len() >= MAX_PATH {
        return fail();
    }
    // SAFETY: the caller's MAX_PATH buffer.
    unsafe { write(dest, &out) };
    dest
}

/// The last element: what follows the last '\', '/' or ':' that has a
/// non-separator after it (so "C:\a\b\" gives "b\").
extern "win64" fn PathFindFileNameW(p: *const u16) -> *const u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path.
    let s = unsafe { read(p) };
    let sep = |c: u16| c == BS || c == b'/' as u16;
    let mut start = 0;
    for i in 0..s.len() {
        if (sep(s[i]) || s[i] == b':' as u16) && i + 1 < s.len() && !sep(s[i + 1]) {
            start = i + 1;
        }
    }
    // SAFETY: within the string.
    unsafe { p.add(start) }
}

/// OSyx has no network drives: only UNC paths are network paths.
extern "win64" fn PathIsNetworkPathW(p: *const u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    let s = unsafe { read(p) };
    (s.len() >= 2 && s[0] == BS && s[1] == BS) as i32
}

/// Replaces the longest leading folder that one of the documented
/// variables names with "%NAME%"; FALSE if none matches or it does not fit.
extern "win64" fn PathUnExpandEnvStringsW(p: *const u16, out: *mut u16, cch: u32) -> i32 {
    if p.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    let s = unsafe { read(p) };
    let mut best: Option<(&str, usize)> = None;
    for name in ["ALLUSERSPROFILE", "APPDATA", "ProgramFiles", "SystemRoot", "SystemDrive", "USERPROFILE"] {
        let Some(v) = kernel32::env_get(name) else { continue };
        let v: Vec<u16> = v.encode_utf16().collect();
        if v.is_empty() || v.len() > s.len() || best.is_some_and(|(_, n)| n >= v.len()) {
            continue;
        }
        if s[..v.len()].iter().zip(&v).all(|(a, b)| lower(*a) == lower(*b)) {
            best = Some((name, v.len()));
        }
    }
    let Some((name, n)) = best else { return 0 };
    let mut r: Vec<u16> = alloc::format!("%{name}%").encode_utf16().collect();
    r.extend_from_slice(&s[n..]);
    if r.len() + 1 > cch as usize {
        return 0;
    }
    // SAFETY: the caller's buffer of `cch` units.
    unsafe {
        core::ptr::copy_nonoverlapping(r.as_ptr(), out, r.len());
        *out.add(r.len()) = 0;
    }
    1
}

/// Case-insensitive search; an empty needle finds nothing.
extern "win64" fn StrStrIW(hay: *const u16, needle: *const u16) -> *const u16 {
    if hay.is_null() || needle.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (h, n) = unsafe { (read(hay), read(needle)) };
    if n.is_empty() || n.len() > h.len() {
        return core::ptr::null();
    }
    match (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].iter().zip(&n).all(|(a, b)| lower(*a) == lower(*b))) {
        // SAFETY: within the string.
        Some(i) => unsafe { hay.add(i) },
        None => core::ptr::null(),
    }
}

/// OSyx registers no file associations.
extern "win64" fn AssocQueryStringW(_flags: u32, _what: u32, _assoc: *const u16, _extra: *const u16, out: *mut u16, len: *mut u32) -> u32 {
    if !out.is_null() && !len.is_null() {
        // SAFETY: the caller's buffer of at least one unit when *len > 0.
        unsafe {
            if *len > 0 {
                *out = 0;
            }
        }
    }
    0x8007_0483 // HRESULT_FROM_WIN32(ERROR_NO_ASSOCIATION)
}

/// IsOS (ordinal 437): the answers Windows 10/11 Pro gives on a workstation
/// that is not in a domain, as observed on Windows 10.0.26300.
extern "win64" fn IsOS(feature: u32) -> i32 {
    matches!(feature, 1 | 3 | 4 | 7 | 18 | 20 | 25 | 26 | 37) as i32
}

pub fn ordinals() -> Vec<(u16, u64)> {
    alloc::vec![(437, IsOS as *const () as u64)]
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        PathRemoveFileSpecW,
        PathAppendW,
        PathMatchSpecW,
        PathCombineW,
        PathFindFileNameW,
        PathIsNetworkPathW,
        PathUnExpandEnvStringsW,
        StrStrIW,
        AssocQueryStringW,
        IsOS,
    ]
}

// ---- WinRT ---------------------------------------------------------------------

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;

/// HSTRING_HEADER / heap string: flags (1 = reference), length, buffer.
#[repr(C)]
struct HString {
    flags: u32,
    length: u32,
    buffer: *const u16,
    _reserved: u64,
}

extern "win64" fn WindowsCreateStringReference(src: *const u16, len: u32, header: *mut HString, out: *mut u64) -> u32 {
    if out.is_null() || header.is_null() || (src.is_null() && len != 0) {
        return E_INVALIDARG;
    }
    // A reference must point at a NUL-terminated string of `len` units.
    // SAFETY: the caller's string of len + 1 units.
    if len != 0 && unsafe { *src.add(len as usize) } != 0 {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's header and out pointer.
    unsafe {
        if len == 0 {
            *out = 0;
            return S_OK;
        }
        header.write(HString { flags: 1, length: len, buffer: src, _reserved: 0 });
        *out = header as u64;
    }
    S_OK
}

extern "win64" fn WindowsCreateString(src: *const u16, len: u32, out: *mut u64) -> u32 {
    if out.is_null() || (src.is_null() && len != 0) {
        return E_INVALIDARG;
    }
    if len == 0 {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
        return S_OK;
    }
    let Some(p) = kernel32::heap_alloc(core::mem::size_of::<HString>() + (len as usize + 1) * 2, false) else { return E_OUTOFMEMORY };
    // SAFETY: a fresh block holding the header and len + 1 units.
    unsafe {
        let text = (p as usize + core::mem::size_of::<HString>()) as *mut u16;
        core::ptr::copy_nonoverlapping(src, text, len as usize);
        *text.add(len as usize) = 0;
        (p as *mut HString).write(HString { flags: 0, length: len, buffer: text, _reserved: 0 });
        *out = p;
    }
    S_OK
}

extern "win64" fn WindowsDeleteString(s: u64) -> u32 {
    // SAFETY: NULL, a reference (not owned) or a WindowsCreateString block.
    if s != 0 && unsafe { (*(s as *const HString)).flags } == 0 {
        kernel32::heap_free(s);
    }
    S_OK
}

extern "win64" fn WindowsGetStringRawBuffer(s: u64, len: *mut u32) -> *const u16 {
    static EMPTY: u16 = 0;
    // SAFETY: NULL or a valid HSTRING.
    let (buf, n) = if s == 0 { (&EMPTY as *const u16, 0) } else { unsafe { ((*(s as *const HString)).buffer, (*(s as *const HString)).length) } };
    if !len.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *len = n };
    }
    buf
}

extern "win64" fn WindowsGetStringLen(s: u64) -> u32 {
    // SAFETY: as above.
    if s == 0 { 0 } else { unsafe { (*(s as *const HString)).length } }
}

extern "win64" fn RoGetActivationFactory(class: u64, _iid: u64, out: *mut u64) -> u32 {
    if out.is_null() || class == 0 {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = 0 };
    REGDB_E_CLASSNOTREG
}

/// Nothing on OSyx is an activatable WinRT class, so activation reports the
/// class as not registered (as `RoGetActivationFactory` does) and the caller
/// takes its non-WinRT path.
extern "win64" fn RoActivateInstance(class: u64, out: *mut u64) -> u32 {
    if out.is_null() || class == 0 {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = 0 };
    REGDB_E_CLASSNOTREG
}

/// `WindowsCompareStringOrdinal`: the UTF-16 code units compared in order,
/// which is what the WinRT string functions define.
extern "win64" fn WindowsCompareStringOrdinal(a: u64, b: u64, out: *mut i32) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: NULL or a valid HSTRING.
    let (pa, la) = if a == 0 { (core::ptr::null::<u16>(), 0) } else { unsafe { ((*(a as *const HString)).buffer, (*(a as *const HString)).length) } };
    // SAFETY: as above.
    let (pb, lb) = if b == 0 { (core::ptr::null::<u16>(), 0) } else { unsafe { ((*(b as *const HString)).buffer, (*(b as *const HString)).length) } };
    let mut result = 0i32;
    for i in 0..la.min(lb) {
        // SAFETY: both strings have at least that many units.
        let (x, y) = unsafe { (*pa.add(i as usize), *pb.add(i as usize)) };
        if x != y {
            result = if x < y { -1 } else { 1 };
            break;
        }
    }
    if result == 0 && la != lb {
        result = if la < lb { -1 } else { 1 };
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = result };
    S_OK
}

pub fn combase_exports() -> Vec<(&'static str, u64)> {
    export_table![
        WindowsCreateStringReference,
        WindowsCreateString,
        WindowsDeleteString,
        WindowsGetStringRawBuffer,
        WindowsGetStringLen,
        RoGetActivationFactory,
        RoActivateInstance,
        WindowsCompareStringOrdinal,
    ]
}
