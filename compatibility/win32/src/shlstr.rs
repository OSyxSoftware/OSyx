//! More of shlwapi: the path and string helpers nearly every Windows
//! program and installer links (`PathIsRelative`, `PathAddBackslash`,
//! `PathCanonicalize`, `StrCmpI`, `StrToIntEx`, `StrFormatByteSize`, ...),
//! in their wide and ANSI forms. `shlwapi.rs` has the first set; both are
//! exported by shlwapi.dll.
//!
//! The rules are shlwapi's own, where they differ from what a file system
//! would say: a path is text, a root is `X:\`, `\` or `\\server\share`, an
//! extension ends at a space, comparisons use the user's (here: the
//! Unicode simple) case folding. ANSI text is Latin-1, as in the rest of
//! this runtime's ANSI entry points.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::vec::Vec;
use core::cmp::Ordering;

const MAX_PATH: usize = 260;
const BS: u16 = b'\\' as u16;
const COLON: u16 = b':' as u16;
const QUOTE: u16 = b'"' as u16;
const SPACE: u16 = b' ' as u16;
const DOT: u16 = b'.' as u16;

/// # Safety
/// `p` is a NUL-terminated UTF-16 string.
unsafe fn len(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: the caller's guarantee.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

/// # Safety
/// `p` is a NUL-terminated UTF-16 string that lives as long as the result
/// is used.
unsafe fn text<'a>(p: *const u16) -> &'a [u16] {
    // SAFETY: the caller's guarantee; `len` units were just read.
    unsafe { core::slice::from_raw_parts(p, len(p)) }
}

/// # Safety
/// `p` has room for `s.len() + 1` units.
unsafe fn put(p: *mut u16, s: &[u16]) {
    // SAFETY: the caller's guarantee.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
        *p.add(s.len()) = 0;
    }
}

/// # Safety
/// `p` is NULL or a NUL-terminated byte string.
unsafe fn ansi(p: *const u8) -> Option<Vec<u16>> {
    if p.is_null() {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0;
    // SAFETY: the caller's guarantee.
    while unsafe { *p.add(i) } != 0 {
        // SAFETY: inside the string.
        out.push(unsafe { *p.add(i) } as u16);
        i += 1;
    }
    Some(out)
}

/// # Safety
/// `p` has room for `s.len() + 1` bytes.
unsafe fn put_ansi(p: *mut u8, s: &[u16]) {
    for (i, &c) in s.iter().enumerate() {
        // SAFETY: the caller's guarantee. Characters outside Latin-1 have
        // no ANSI form here.
        unsafe { *p.add(i) = if c < 0x100 { c as u8 } else { b'?' } };
    }
    // SAFETY: room for the terminator.
    unsafe { *p.add(s.len()) = 0 };
}

/// The case-insensitive form of a UTF-16 unit (simple folding; units of a
/// surrogate pair are left alone).
fn fold(c: u16) -> u16 {
    match char::from_u32(c as u32) {
        Some(ch) => {
            let mut lower = ch.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) if (l as u32) < 0x1_0000 => l as u32 as u16,
                _ => c,
            }
        }
        None => c,
    }
}

fn compare(a: &[u16], b: &[u16], ignore_case: bool) -> i32 {
    let key = |c: &u16| if ignore_case { fold(*c) } else { *c };
    match a.iter().map(key).cmp(b.iter().map(key)) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn find(hay: &[u16], needle: &[u16], ignore_case: bool) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| compare(&hay[i..i + needle.len()], needle, ignore_case) == 0)
}

/// Length of the root of a path: `X:\` (3), `X:` (2), `\\server\share`
/// (up to the end of the share) or `\` (1); 0 for a relative path.
fn root_len(s: &[u16]) -> usize {
    if s.len() >= 2 && s[1] == COLON {
        return if s.get(2) == Some(&BS) { 3 } else { 2 };
    }
    if s.len() >= 2 && s[0] == BS && s[1] == BS {
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

/// shlwapi's "is a root": `X:\`, `\`, `\\server` or `\\server\share`.
fn is_root(s: &[u16]) -> bool {
    match s {
        [BS] => true,
        [_, COLON, BS] => true,
        [BS, BS, rest @ ..] => !rest.is_empty() && rest.iter().filter(|&&c| c == BS).count() <= 1 && rest.last() != Some(&BS),
        _ => false,
    }
}

/// Index of the file name: after the last separator that is followed by
/// more text (`C:\a\b\` names `b\`).
fn file_name(s: &[u16]) -> usize {
    let mut at = 0;
    for i in 0..s.len() {
        if (s[i] == BS || s[i] == b'/' as u16 || s[i] == COLON) && i + 1 < s.len() && s[i + 1] != BS && s[i + 1] != b'/' as u16 {
            at = i + 1;
        }
    }
    at
}

/// Index of the extension's dot, or the length when there is none.
fn extension(s: &[u16]) -> usize {
    let mut dot = None;
    for (i, &c) in s.iter().enumerate() {
        match c {
            DOT => dot = Some(i),
            BS | SPACE => dot = None,
            _ => {}
        }
    }
    dot.unwrap_or(s.len())
}

/// Resolves `.` and `..` elements after the root, as PathCanonicalize does:
/// `..` at the root is dropped, an emptied path is `\`.
fn canonical(s: &[u16]) -> Vec<u16> {
    let root = root_len(s);
    let mut parts: Vec<&[u16]> = Vec::new();
    for part in s[root..].split(|&c| c == BS) {
        match part {
            [] | [DOT] => {}
            [DOT, DOT] => {
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
    if out.is_empty() {
        out.push(BS);
    } else if out.len() == 2 && out[1] == COLON {
        // "C:" alone is not a path: PathCanonicalize makes it the root.
        out.push(BS);
    }
    out
}

// ---- path predicates ---------------------------------------------------------------------------

extern "win64" fn PathIsRelativeW(p: *const u16) -> i32 {
    if p.is_null() {
        return 1;
    }
    // SAFETY: the caller's NUL-terminated path.
    let s = unsafe { text(p) };
    (s.first() != Some(&BS) && s.get(1) != Some(&COLON)) as i32
}

extern "win64" fn PathIsRootW(p: *const u16) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated path.
    !p.is_null() as i32 & is_root(unsafe { if p.is_null() { &[] } else { text(p) } }) as i32
}

extern "win64" fn PathIsUNCW(p: *const u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    let s = unsafe { text(p) };
    (s.len() >= 2 && s[0] == BS && s[1] == BS && s.get(2) != Some(&(b'?' as u16))) as i32
}

/// A path with no `\` and no `:` is a file specification.
extern "win64" fn PathIsFileSpecW(p: *const u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    (!unsafe { text(p) }.iter().any(|&c| c == BS || c == COLON)) as i32
}

/// A URL has a scheme: two or more letters, digits, `+`, `-` or `.`
/// before a colon (one letter would be a drive).
extern "win64" fn PathIsURLW(p: *const u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string.
    let s = unsafe { text(p) };
    let Some(colon) = s.iter().position(|&c| c == COLON) else { return 0 };
    let scheme_char = |c: &u16| *c < 0x80 && ((*c as u8).is_ascii_alphanumeric() || matches!(*c as u8, b'+' | b'-' | b'.'));
    (colon >= 2 && s[..colon].iter().all(scheme_char) && (s[0] as u8).is_ascii_alphabetic() && s[0] < 0x80) as i32
}

/// FILE_ATTRIBUTE_DIRECTORY (nonzero) for an existing directory.
extern "win64" fn PathIsDirectoryW(p: *const u16) -> i32 {
    // SAFETY: NULL or a NUL-terminated name.
    let Some(win) = (unsafe { crate::path::wide_to_string(p) }) else { return 0 };
    match kernel32::locate(&win) {
        Ok((path, true)) if os_std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) => 0x10,
        _ => 0,
    }
}

/// Whether `prefix` is a whole-element prefix of `path` (ignoring case).
extern "win64" fn PathIsPrefixW(prefix: *const u16, path: *const u16) -> i32 {
    if prefix.is_null() || path.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (a, b) = unsafe { (text(prefix), text(path)) };
    let a = if a.len() > root_len(a) && a.last() == Some(&BS) { &a[..a.len() - 1] } else { a };
    if a.is_empty() || a.len() > b.len() || compare(a, &b[..a.len()], true) != 0 {
        return 0;
    }
    (a.len() == b.len() || b[a.len()] == BS || a.last() == Some(&BS)) as i32
}

// ---- path editing ------------------------------------------------------------------------------

/// Adds a trailing backslash; returns the terminator, or NULL when the
/// path would exceed MAX_PATH.
extern "win64" fn PathAddBackslashW(p: *mut u16) -> *mut u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's MAX_PATH buffer holding a NUL-terminated path.
    unsafe {
        let n = len(p);
        if n == 0 || *p.add(n - 1) == BS {
            return p.add(n);
        }
        if n + 1 >= MAX_PATH {
            return core::ptr::null_mut();
        }
        *p.add(n) = BS;
        *p.add(n + 1) = 0;
        p.add(n + 1)
    }
}

/// Removes a trailing backslash, except a root's; returns the terminator
/// when one was removed, the last character otherwise.
extern "win64" fn PathRemoveBackslashW(p: *mut u16) -> *mut u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path buffer.
    unsafe {
        let s = text(p);
        let Some(last) = s.len().checked_sub(1) else { return p };
        if s[last] == BS && !is_root(s) {
            *p.add(last) = 0;
        }
        p.add(last)
    }
}

/// Leaves only the file name.
extern "win64" fn PathStripPathW(p: *mut u16) {
    if p.is_null() {
        return;
    }
    // SAFETY: the caller's NUL-terminated path buffer, shortened in place.
    unsafe {
        let s = text(p).to_vec();
        let at = file_name(&s);
        if at > 0 {
            put(p, &s[at..]);
        }
    }
}

/// Leaves only the root; FALSE for a path without one.
extern "win64" fn PathStripToRootW(p: *mut u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path buffer, shortened in place.
    unsafe {
        let root = root_len(text(p));
        *p.add(root) = 0;
        (root > 0) as i32
    }
}

/// Adds `ext` (".exe" when NULL) to a path that has no extension.
extern "win64" fn PathAddExtensionW(p: *mut u16, ext: *const u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    const EXE: [u16; 4] = [DOT, b'e' as u16, b'x' as u16, b'e' as u16];
    // SAFETY: the caller's MAX_PATH buffer and NUL-terminated extension.
    unsafe {
        let s = text(p);
        let ext = if ext.is_null() { &EXE[..] } else { text(ext) };
        if extension(s) != s.len() || s.len() + ext.len() >= MAX_PATH {
            return 0;
        }
        let at = s.len();
        put(p.add(at), ext);
        1
    }
}

/// Replaces the extension (or adds one).
extern "win64" fn PathRenameExtensionW(p: *mut u16, ext: *const u16) -> i32 {
    if p.is_null() || ext.is_null() {
        return 0;
    }
    // SAFETY: the caller's MAX_PATH buffer and NUL-terminated extension.
    unsafe {
        let s = text(p);
        let ext = text(ext);
        let at = extension(s);
        if at + ext.len() >= MAX_PATH {
            return 0;
        }
        put(p.add(at), ext);
        1
    }
}

/// Puts a path that contains spaces in quotes.
extern "win64" fn PathQuoteSpacesW(p: *mut u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's MAX_PATH buffer holding a NUL-terminated path.
    unsafe {
        let s = text(p).to_vec();
        if !s.contains(&SPACE) || s.len() + 2 >= MAX_PATH {
            return 0;
        }
        let mut quoted = Vec::with_capacity(s.len() + 2);
        quoted.push(QUOTE);
        quoted.extend_from_slice(&s);
        quoted.push(QUOTE);
        put(p, &quoted);
        1
    }
}

/// Removes the quotes around a path.
extern "win64" fn PathUnquoteSpacesW(p: *mut u16) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path buffer, shortened in place.
    unsafe {
        let s = text(p).to_vec();
        if s.first() != Some(&QUOTE) {
            return 0;
        }
        let end = if s.len() >= 2 && s.last() == Some(&QUOTE) { s.len() - 1 } else { s.len() };
        put(p, &s[1..end]);
        1
    }
}

/// Index of the arguments of a command line: after the first space
/// outside quotes; the length when there are none.
fn args_at(s: &[u16]) -> usize {
    let mut quoted = false;
    for (i, &c) in s.iter().enumerate() {
        match c {
            QUOTE => quoted = !quoted,
            SPACE if !quoted => return i + 1,
            _ => {}
        }
    }
    s.len()
}

extern "win64" fn PathGetArgsW(p: *const u16) -> *const u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated command line.
    unsafe { p.add(args_at(text(p))) }
}

extern "win64" fn PathRemoveArgsW(p: *mut u16) {
    if p.is_null() {
        return;
    }
    // SAFETY: the caller's NUL-terminated command line, shortened in place.
    unsafe {
        let s = text(p);
        let at = args_at(s);
        if at < s.len() {
            *p.add(at - 1) = 0;
        } else if s.last() == Some(&SPACE) {
            // A trailing space and nothing after it.
            *p.add(s.len() - 1) = 0;
        }
    }
}

/// The text after the root; NULL for a path without one.
extern "win64" fn PathSkipRootW(p: *const u16) -> *const u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path.
    unsafe {
        let s = text(p);
        let mut root = root_len(s);
        if root == 0 || (root == 2 && s[1] == COLON) {
            return core::ptr::null();
        }
        // After "\\server\share" comes the separator.
        if s.len() >= 2 && s[1] == BS && root < s.len() {
            root += 1;
        }
        p.add(root)
    }
}

/// The element after the next backslash; the terminator when there is no
/// backslash; NULL at the end.
extern "win64" fn PathFindNextComponentW(p: *const u16) -> *const u16 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path.
    unsafe {
        let s = text(p);
        if s.is_empty() {
            return core::ptr::null();
        }
        match s.iter().position(|&c| c == BS) {
            // A UNC prefix is skipped whole.
            Some(0) if s.get(1) == Some(&BS) => p.add(2),
            Some(i) => p.add(i + 1),
            None => p.add(s.len()),
        }
    }
}

/// Resolves `.` and `..`; `dst` has MAX_PATH units.
extern "win64" fn PathCanonicalizeW(dst: *mut u16, src: *const u16) -> i32 {
    if dst.is_null() {
        return crate::kernel32::fail(87, 0);
    }
    // SAFETY: the caller's MAX_PATH buffer.
    unsafe { *dst = 0 };
    if src.is_null() {
        return crate::kernel32::fail(87, 0);
    }
    // SAFETY: the caller's NUL-terminated path.
    let out = canonical(unsafe { text(src) });
    if out.len() >= MAX_PATH {
        return crate::kernel32::fail(206, 0);
    }
    // SAFETY: checked against MAX_PATH.
    unsafe { put(dst, &out) };
    1
}

// ---- strings -----------------------------------------------------------------------------------

/// # Safety
/// NULL or NUL-terminated strings; NULL compares as an empty string.
unsafe fn pair<'a>(a: *const u16, b: *const u16) -> (&'a [u16], &'a [u16]) {
    // SAFETY: the caller's guarantee.
    unsafe { (if a.is_null() { &[] } else { text(a) }, if b.is_null() { &[] } else { text(b) }) }
}

extern "win64" fn StrCmpW(a: *const u16, b: *const u16) -> i32 {
    // SAFETY: the caller's strings.
    let (a, b) = unsafe { pair(a, b) };
    compare(a, b, false)
}

extern "win64" fn StrCmpIW(a: *const u16, b: *const u16) -> i32 {
    // SAFETY: the caller's strings.
    let (a, b) = unsafe { pair(a, b) };
    compare(a, b, true)
}

fn compare_n(a: &[u16], b: &[u16], n: i32, ignore_case: bool) -> i32 {
    let n = n.max(0) as usize;
    compare(&a[..a.len().min(n)], &b[..b.len().min(n)], ignore_case)
}

extern "win64" fn StrCmpNW(a: *const u16, b: *const u16, n: i32) -> i32 {
    // SAFETY: the caller's strings.
    let (a, b) = unsafe { pair(a, b) };
    compare_n(a, b, n, false)
}

extern "win64" fn StrCmpNIW(a: *const u16, b: *const u16, n: i32) -> i32 {
    // SAFETY: the caller's strings.
    let (a, b) = unsafe { pair(a, b) };
    compare_n(a, b, n, true)
}

/// Zero when the two characters are equal ignoring case.
extern "win64" fn ChrCmpIW(a: u16, b: u16) -> i32 {
    (fold(a) != fold(b)) as i32
}

fn chr(s: *const u16, ch: u16, ignore_case: bool, last: bool) -> *const u16 {
    if s.is_null() {
        return s;
    }
    // SAFETY: the caller's NUL-terminated string.
    let t = unsafe { text(s) };
    let is = |c: &u16| if ignore_case { fold(*c) == fold(ch) } else { *c == ch };
    let at = if last { t.iter().rposition(is) } else { t.iter().position(is) };
    // SAFETY: an index inside the string.
    at.map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

extern "win64" fn StrChrW(s: *const u16, ch: u16) -> *const u16 {
    chr(s, ch, false, false)
}

extern "win64" fn StrChrIW(s: *const u16, ch: u16) -> *const u16 {
    chr(s, ch, true, false)
}

/// The last `ch` before `end` (the end of the string when NULL).
fn rchr(s: *const u16, end: *const u16, ch: u16, ignore_case: bool) -> *const u16 {
    if s.is_null() {
        return s;
    }
    // SAFETY: the caller's NUL-terminated string; `end` points into it.
    let t = unsafe { text(s) };
    let limit = if end.is_null() { t.len() } else { ((end as usize).saturating_sub(s as usize) / 2).min(t.len()) };
    let is = |c: &u16| if ignore_case { fold(*c) == fold(ch) } else { *c == ch };
    // SAFETY: an index inside the string.
    t[..limit].iter().rposition(is).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

extern "win64" fn StrRChrW(s: *const u16, end: *const u16, ch: u16) -> *const u16 {
    rchr(s, end, ch, false)
}

extern "win64" fn StrRChrIW(s: *const u16, end: *const u16, ch: u16) -> *const u16 {
    rchr(s, end, ch, true)
}

extern "win64" fn StrStrW(s: *const u16, sub: *const u16) -> *const u16 {
    if s.is_null() || sub.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated strings; an index inside `s`.
    unsafe { find(text(s), text(sub), false).map_or(core::ptr::null(), |i| s.add(i)) }
}

/// The last occurrence of `sub` in `s` that starts before `end`.
extern "win64" fn StrRStrIW(s: *const u16, end: *const u16, sub: *const u16) -> *const u16 {
    if s.is_null() || sub.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated strings; `end` points into `s`.
    unsafe {
        let (t, needle) = (text(s), text(sub));
        if needle.is_empty() || needle.len() > t.len() {
            return core::ptr::null();
        }
        // A match has to lie wholly before `end`.
        let limit = if end.is_null() { t.len() } else { ((end as usize).saturating_sub(s as usize) / 2).min(t.len()) };
        (0..=t.len() - needle.len())
            .rev()
            .filter(|&i| i + needle.len() <= limit)
            .find(|&i| compare(&t[i..i + needle.len()], needle, true) == 0)
            .map_or(core::ptr::null(), |i| s.add(i))
    }
}

/// Number of leading characters of `s` that are (`inside`) or are not in `set`.
fn span(s: *const u16, set: *const u16, inside: bool) -> i32 {
    if s.is_null() || set.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (t, set) = unsafe { (text(s), text(set)) };
    t.iter().position(|c| set.contains(c) != inside).unwrap_or(t.len()) as i32
}

extern "win64" fn StrSpnW(s: *const u16, set: *const u16) -> i32 {
    span(s, set, true)
}

extern "win64" fn StrCSpnW(s: *const u16, set: *const u16) -> i32 {
    span(s, set, false)
}

/// The first character of `s` that is in `set`.
extern "win64" fn StrPBrkW(s: *const u16, set: *const u16) -> *const u16 {
    if s.is_null() || set.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated strings; an index inside `s`.
    unsafe {
        let (t, set) = (text(s), text(set));
        t.iter().position(|c| set.contains(c)).map_or(core::ptr::null(), |i| s.add(i))
    }
}

/// Parses an integer. `skip_space` and `hex` are what StrToIntEx adds to
/// StrToInt: leading white space, a `+` sign, and with STIF_SUPPORT_HEX a
/// `0x` prefix. `None` without a digit.
fn parse_int(s: &[u16], extended: bool, hex: bool) -> Option<i64> {
    let mut i = 0;
    if extended {
        while s.get(i).is_some_and(|&c| c == SPACE || (9..=13).contains(&c)) {
            i += 1;
        }
    }
    let negative = s.get(i) == Some(&(b'-' as u16));
    if negative || (extended && s.get(i) == Some(&(b'+' as u16))) {
        i += 1;
    }
    let digit = |c: u16, radix: u32| char::from_u32(c as u32).and_then(|ch| ch.to_digit(radix));
    let radix = if hex && s.get(i) == Some(&(b'0' as u16)) && s.get(i + 1).is_some_and(|&c| c == b'x' as u16 || c == b'X' as u16) {
        i += 2;
        16
    } else {
        10
    };
    let mut value: i64 = 0;
    let mut digits = 0;
    while let Some(d) = s.get(i).and_then(|&c| digit(c, radix)) {
        value = value.wrapping_mul(radix as i64).wrapping_add(d as i64);
        digits += 1;
        i += 1;
    }
    (digits > 0).then_some(if negative { value.wrapping_neg() } else { value })
}

/// Decimal digits with an optional `-`; 0 when there are none.
extern "win64" fn StrToIntW(s: *const u16) -> i32 {
    if s.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string.
    parse_int(unsafe { text(s) }, false, false).unwrap_or(0) as i32
}

const STIF_SUPPORT_HEX: u32 = 1;

extern "win64" fn StrToIntExW(s: *const u16, flags: u32, out: *mut i32) -> i32 {
    if s.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string and result.
    unsafe {
        match parse_int(text(s), true, flags & STIF_SUPPORT_HEX != 0) {
            Some(v) => {
                *out = v as i32;
                1
            }
            None => 0,
        }
    }
}

extern "win64" fn StrToInt64ExW(s: *const u16, flags: u32, out: *mut i64) -> i32 {
    if s.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string and result.
    unsafe {
        match parse_int(text(s), true, flags & STIF_SUPPORT_HEX != 0) {
            Some(v) => {
                *out = v;
                1
            }
            None => 0,
        }
    }
}

/// A copy in a LocalAlloc block (the caller frees it with LocalFree).
extern "win64" fn StrDupW(s: *const u16) -> *mut u16 {
    // SAFETY: NULL or the caller's NUL-terminated string.
    let t = unsafe { if s.is_null() { &[] } else { text(s) } };
    let Some(block) = kernel32::heap_alloc((t.len() + 1) * 2, false) else { return core::ptr::null_mut() };
    // SAFETY: a fresh block of len + 1 units.
    unsafe { put(block as *mut u16, t) };
    block as *mut u16
}

/// A copy in a CoTaskMemAlloc block.
extern "win64" fn SHStrDupW(s: *const u16, out: *mut *mut u16) -> u32 {
    const E_INVALIDARG: u32 = 0x8007_0057;
    const E_OUTOFMEMORY: u32 = 0x8007_000E;
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's result pointer.
    unsafe { *out = core::ptr::null_mut() };
    if s.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's NUL-terminated string.
    let t = unsafe { text(s) };
    let block = crate::ole32::CoTaskMemAlloc((t.len() + 1) * 2);
    if block == 0 {
        return E_OUTOFMEMORY;
    }
    // SAFETY: a fresh block of len + 1 units; the caller's result pointer.
    unsafe {
        put(block as *mut u16, t);
        *out = block as *mut u16;
    }
    0
}

extern "win64" fn StrCpyW(dst: *mut u16, src: *const u16) -> *mut u16 {
    if !dst.is_null() && !src.is_null() {
        // SAFETY: the caller's string and a buffer that holds it.
        unsafe { put(dst, &text(src).to_vec()) };
    }
    dst
}

extern "win64" fn StrCatW(dst: *mut u16, src: *const u16) -> *mut u16 {
    if !dst.is_null() && !src.is_null() {
        // SAFETY: the caller's strings and a buffer that holds both.
        unsafe { put(dst.add(len(dst)), &text(src).to_vec()) };
    }
    dst
}

/// Copies at most `cap - 1` characters and always terminates.
extern "win64" fn StrCpyNW(dst: *mut u16, src: *const u16, cap: i32) -> *mut u16 {
    if dst.is_null() || cap <= 0 {
        return dst;
    }
    // SAFETY: the caller's buffer of `cap` units and NUL-terminated source.
    unsafe {
        let t = if src.is_null() { Vec::new() } else { text(src).to_vec() };
        put(dst, &t[..t.len().min(cap as usize - 1)]);
    }
    dst
}

/// Appends as much of `src` as a buffer of `cap` units in all holds.
extern "win64" fn StrCatBuffW(dst: *mut u16, src: *const u16, cap: i32) -> *mut u16 {
    if dst.is_null() || src.is_null() || cap <= 0 {
        return dst;
    }
    // SAFETY: the caller's buffer of `cap` units and NUL-terminated strings.
    unsafe {
        let have = len(dst);
        let room = (cap as usize).saturating_sub(have + 1);
        let t = text(src).to_vec();
        if (cap as usize) > have {
            put(dst.add(have), &t[..t.len().min(room)]);
        }
    }
    dst
}

/// Removes the characters of `set` from both ends; TRUE if any was removed.
extern "win64" fn StrTrimW(s: *mut u16, set: *const u16) -> i32 {
    if s.is_null() || set.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings; `s` is shortened in place.
    unsafe {
        let t = text(s).to_vec();
        let set = text(set);
        let start = t.iter().position(|c| !set.contains(c)).unwrap_or(t.len());
        let end = t.iter().rposition(|c| !set.contains(c)).map_or(start, |i| i + 1);
        if start == 0 && end == t.len() {
            return 0;
        }
        put(s, &t[start..end]);
        1
    }
}

/// A size as the shell shows it: `532 bytes`, `1.50 KB`, `23.5 MB`,
/// `100 GB`: three significant digits, cut off, not rounded.
fn byte_size(bytes: u64) -> alloc::string::String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1024 {
        return alloc::format!("{bytes} bytes");
    }
    // The largest unit the value reaches, and the value in it times 1000:
    // exact, so the digits are cut off where the shell cuts them.
    let unit = ((63 - bytes.leading_zeros()) / 10) as usize - 1;
    let thousandths = (bytes as u128 * 1000) >> (10 * (unit + 1));
    let (int, frac) = ((thousandths / 1000) as u64, (thousandths % 1000) as u64);
    let unit = UNITS[unit];
    match int {
        0..=9 => alloc::format!("{int}.{:02} {unit}", frac / 10),
        10..=99 => alloc::format!("{int}.{} {unit}", frac / 100),
        _ => alloc::format!("{int} {unit}"),
    }
}

extern "win64" fn StrFormatByteSizeW(bytes: i64, buf: *mut u16, cap: u32) -> *mut u16 {
    if buf.is_null() || cap == 0 {
        return buf;
    }
    let text: Vec<u16> = byte_size(bytes.max(0) as u64).encode_utf16().collect();
    // SAFETY: the caller's buffer of `cap` units.
    unsafe { put(buf, &text[..text.len().min(cap as usize - 1)]) };
    buf
}

// ---- ANSI forms --------------------------------------------------------------------------------

/// Runs a wide in-place path edit on an ANSI buffer of MAX_PATH bytes.
///
/// # Safety
/// `p` is NULL or such a buffer holding a NUL-terminated path.
unsafe fn edit_ansi<R: Default>(p: *mut u8, edit: impl FnOnce(*mut u16) -> R) -> R {
    // SAFETY: the caller's guarantee.
    let Some(mut wide) = (unsafe { ansi(p) }) else { return R::default() };
    wide.resize(MAX_PATH + 4, 0);
    let result = edit(wide.as_mut_ptr());
    let n = wide.iter().position(|&c| c == 0).unwrap_or(0).min(MAX_PATH - 1);
    // SAFETY: at most MAX_PATH bytes with the terminator.
    unsafe { put_ansi(p, &wide[..n]) };
    result
}

/// The wide form of an ANSI string, NUL-terminated; empty for NULL.
///
/// # Safety
/// `p` is NULL or a NUL-terminated byte string.
unsafe fn widen(p: *const u8) -> Vec<u16> {
    // SAFETY: the caller's guarantee.
    let mut w = unsafe { ansi(p) }.unwrap_or_default();
    w.push(0);
    w
}

/// A pointer into the ANSI string `p` where the wide function, run on its
/// wide form, pointed to (byte and unit offsets are the same in Latin-1).
fn same_offset(p: *const u8, wide: &[u16], at: *const u16) -> *const u8 {
    if at.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `at` points into `wide`, which mirrors `p` unit for byte.
    unsafe { p.add((at as usize - wide.as_ptr() as usize) / 2) }
}

extern "win64" fn PathIsRelativeA(p: *const u8) -> i32 {
    if p.is_null() {
        return 1;
    }
    // SAFETY: the caller's NUL-terminated path.
    PathIsRelativeW(unsafe { widen(p) }.as_ptr())
}

extern "win64" fn PathIsRootA(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    PathIsRootW(unsafe { widen(p) }.as_ptr())
}

extern "win64" fn PathIsUNCA(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    PathIsUNCW(unsafe { widen(p) }.as_ptr())
}

extern "win64" fn PathIsURLA(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string.
    PathIsURLW(unsafe { widen(p) }.as_ptr())
}

extern "win64" fn PathIsDirectoryA(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    PathIsDirectoryW(unsafe { widen(p) }.as_ptr())
}

extern "win64" fn PathFileExistsA(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated path.
    let wide = unsafe { widen(p) };
    // SAFETY: a NUL-terminated name.
    let Some(win) = (unsafe { crate::path::wide_to_string(wide.as_ptr()) }) else { return 0 };
    matches!(kernel32::locate(&win), Ok((_, true))) as i32
}

extern "win64" fn PathFindFileNameA(p: *const u8) -> *const u8 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path; an index inside it.
    unsafe { p.add(file_name(&ansi(p).unwrap_or_default())) }
}

extern "win64" fn PathFindExtensionA(p: *const u8) -> *const u8 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated path; an index inside it.
    unsafe { p.add(extension(&ansi(p).unwrap_or_default())) }
}

extern "win64" fn PathGetArgsA(p: *const u8) -> *const u8 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's NUL-terminated command line; an index inside it.
    unsafe { p.add(args_at(&ansi(p).unwrap_or_default())) }
}

extern "win64" fn PathAddBackslashA(p: *mut u8) -> *mut u8 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's MAX_PATH buffer.
    let added = unsafe { edit_ansi(p, |w| !PathAddBackslashW(w).is_null()) };
    if !added {
        return core::ptr::null_mut();
    }
    // SAFETY: the edited, NUL-terminated buffer.
    unsafe { p.add(ansi(p).map_or(0, |s| s.len())) }
}

extern "win64" fn PathRemoveBackslashA(p: *mut u8) -> *mut u8 {
    if p.is_null() {
        return p;
    }
    // SAFETY: the caller's buffer; the edit only shortens it.
    unsafe {
        edit_ansi(p, |w| {
            PathRemoveBackslashW(w);
        });
        let n = ansi(p).map_or(0, |s| s.len());
        p.add(n.saturating_sub(1))
    }
}

extern "win64" fn PathStripPathA(p: *mut u8) {
    // SAFETY: NULL or the caller's buffer; the edit only shortens it.
    unsafe { edit_ansi(p, |w| PathStripPathW(w)) }
}

extern "win64" fn PathRemoveArgsA(p: *mut u8) {
    // SAFETY: NULL or the caller's buffer; the edit only shortens it.
    unsafe { edit_ansi(p, |w| PathRemoveArgsW(w)) }
}

extern "win64" fn PathStripToRootA(p: *mut u8) -> i32 {
    // SAFETY: NULL or the caller's buffer; the edit only shortens it.
    unsafe { edit_ansi(p, |w| PathStripToRootW(w)) }
}

extern "win64" fn PathQuoteSpacesA(p: *mut u8) -> i32 {
    // SAFETY: NULL or the caller's MAX_PATH buffer.
    unsafe { edit_ansi(p, |w| PathQuoteSpacesW(w)) }
}

extern "win64" fn PathUnquoteSpacesA(p: *mut u8) -> i32 {
    // SAFETY: NULL or the caller's buffer; the edit only shortens it.
    unsafe { edit_ansi(p, |w| PathUnquoteSpacesW(w)) }
}

extern "win64" fn PathAddExtensionA(p: *mut u8, ext: *const u8) -> i32 {
    // SAFETY: the caller's MAX_PATH buffer and NUL-terminated extension.
    unsafe {
        let ext_wide = if ext.is_null() { None } else { Some(widen(ext)) };
        edit_ansi(p, |w| PathAddExtensionW(w, ext_wide.as_ref().map_or(core::ptr::null(), |e| e.as_ptr())))
    }
}

extern "win64" fn PathRenameExtensionA(p: *mut u8, ext: *const u8) -> i32 {
    if ext.is_null() {
        return 0;
    }
    // SAFETY: the caller's MAX_PATH buffer and NUL-terminated extension.
    unsafe {
        let ext_wide = widen(ext);
        edit_ansi(p, |w| PathRenameExtensionW(w, ext_wide.as_ptr()))
    }
}

extern "win64" fn PathCanonicalizeA(dst: *mut u8, src: *const u8) -> i32 {
    if dst.is_null() {
        return crate::kernel32::fail(87, 0);
    }
    // SAFETY: the caller's MAX_PATH buffer.
    unsafe { *dst = 0 };
    if src.is_null() {
        return crate::kernel32::fail(87, 0);
    }
    // SAFETY: the caller's NUL-terminated path.
    let out = canonical(&unsafe { ansi(src) }.unwrap_or_default());
    if out.len() >= MAX_PATH {
        return crate::kernel32::fail(206, 0);
    }
    // SAFETY: checked against MAX_PATH.
    unsafe { put_ansi(dst, &out) };
    1
}

extern "win64" fn StrChrA(s: *const u8, ch: u16) -> *const u8 {
    if s.is_null() {
        return s;
    }
    // SAFETY: the caller's NUL-terminated string.
    let wide = unsafe { widen(s) };
    same_offset(s, &wide, StrChrW(wide.as_ptr(), ch & 0xff))
}

extern "win64" fn StrChrIA(s: *const u8, ch: u16) -> *const u8 {
    if s.is_null() {
        return s;
    }
    // SAFETY: the caller's NUL-terminated string.
    let wide = unsafe { widen(s) };
    same_offset(s, &wide, StrChrIW(wide.as_ptr(), ch & 0xff))
}

extern "win64" fn StrRChrA(s: *const u8, end: *const u8, ch: u16) -> *const u8 {
    if s.is_null() {
        return s;
    }
    // SAFETY: the caller's NUL-terminated string; `end` points into it.
    let wide = unsafe { widen(s) };
    let wide_end = if end.is_null() {
        core::ptr::null()
    } else {
        // SAFETY: the same offset in the wide copy, clamped to its length.
        unsafe { wide.as_ptr().add(((end as usize).saturating_sub(s as usize)).min(wide.len() - 1)) }
    };
    same_offset(s, &wide, StrRChrW(wide.as_ptr(), wide_end, ch & 0xff))
}

fn str_str_a(s: *const u8, sub: *const u8, ignore_case: bool) -> *const u8 {
    if s.is_null() || sub.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated strings.
    let (hay, needle) = unsafe { (ansi(s).unwrap_or_default(), ansi(sub).unwrap_or_default()) };
    // SAFETY: an index inside `s`.
    find(&hay, &needle, ignore_case).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

extern "win64" fn StrStrA(s: *const u8, sub: *const u8) -> *const u8 {
    str_str_a(s, sub, false)
}

extern "win64" fn StrStrIA(s: *const u8, sub: *const u8) -> *const u8 {
    str_str_a(s, sub, true)
}

fn compare_n_a(a: *const u8, b: *const u8, n: i32, ignore_case: bool) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated strings.
    let (a, b) = unsafe { (ansi(a).unwrap_or_default(), ansi(b).unwrap_or_default()) };
    compare_n(&a, &b, n, ignore_case)
}

extern "win64" fn StrCmpNA(a: *const u8, b: *const u8, n: i32) -> i32 {
    compare_n_a(a, b, n, false)
}

extern "win64" fn StrCmpNIA(a: *const u8, b: *const u8, n: i32) -> i32 {
    compare_n_a(a, b, n, true)
}

extern "win64" fn StrToIntA(s: *const u8) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated string.
    parse_int(&unsafe { ansi(s) }.unwrap_or_default(), false, false).unwrap_or(0) as i32
}

extern "win64" fn StrToIntExA(s: *const u8, flags: u32, out: *mut i32) -> i32 {
    if s.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated string.
    let wide = unsafe { widen(s) };
    StrToIntExW(wide.as_ptr(), flags, out)
}

extern "win64" fn StrDupA(s: *const u8) -> *mut u8 {
    // SAFETY: NULL or the caller's NUL-terminated string.
    let t = unsafe { ansi(s) }.unwrap_or_default();
    let Some(block) = kernel32::heap_alloc(t.len() + 1, false) else { return core::ptr::null_mut() };
    // SAFETY: a fresh block of len + 1 bytes.
    unsafe { put_ansi(block as *mut u8, &t) };
    block as *mut u8
}

extern "win64" fn StrTrimA(s: *mut u8, set: *const u8) -> i32 {
    if s.is_null() || set.is_null() {
        return 0;
    }
    // SAFETY: the caller's NUL-terminated strings; `s` is shortened in place.
    unsafe {
        let mut wide = widen(s);
        let set = widen(set);
        let trimmed = StrTrimW(wide.as_mut_ptr(), set.as_ptr());
        let n = wide.iter().position(|&c| c == 0).unwrap_or(0);
        put_ansi(s, &wide[..n]);
        trimmed
    }
}

extern "win64" fn StrFormatByteSizeA(bytes: u32, buf: *mut u8, cap: u32) -> *mut u8 {
    if buf.is_null() || cap == 0 {
        return buf;
    }
    let text: Vec<u16> = byte_size(bytes as u64).encode_utf16().collect();
    // SAFETY: the caller's buffer of `cap` bytes.
    unsafe { put_ansi(buf, &text[..text.len().min(cap as usize - 1)]) };
    buf
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        PathIsRelativeW,
        PathIsRootW,
        PathIsUNCW,
        PathIsFileSpecW,
        PathIsURLW,
        PathIsDirectoryW,
        PathIsPrefixW,
        PathAddBackslashW,
        PathRemoveBackslashW,
        PathStripPathW,
        PathStripToRootW,
        PathAddExtensionW,
        PathRenameExtensionW,
        PathQuoteSpacesW,
        PathUnquoteSpacesW,
        PathGetArgsW,
        PathRemoveArgsW,
        PathSkipRootW,
        PathFindNextComponentW,
        PathCanonicalizeW,
        StrCmpW,
        StrCmpIW,
        StrCmpNW,
        StrCmpNIW,
        ChrCmpIW,
        StrChrW,
        StrChrIW,
        StrRChrW,
        StrRChrIW,
        StrStrW,
        StrRStrIW,
        StrSpnW,
        StrCSpnW,
        StrPBrkW,
        StrToIntW,
        StrToIntExW,
        StrToInt64ExW,
        StrDupW,
        SHStrDupW,
        StrCpyW,
        StrCatW,
        StrCpyNW,
        StrCatBuffW,
        StrTrimW,
        StrFormatByteSizeW,
        PathIsRelativeA,
        PathIsRootA,
        PathIsUNCA,
        PathIsURLA,
        PathIsDirectoryA,
        PathFileExistsA,
        PathFindFileNameA,
        PathFindExtensionA,
        PathGetArgsA,
        PathAddBackslashA,
        PathRemoveBackslashA,
        PathStripPathA,
        PathRemoveArgsA,
        PathStripToRootA,
        PathQuoteSpacesA,
        PathUnquoteSpacesA,
        PathAddExtensionA,
        PathRenameExtensionA,
        PathCanonicalizeA,
        StrChrA,
        StrChrIA,
        StrRChrA,
        StrStrA,
        StrStrIA,
        StrCmpNA,
        StrCmpNIA,
        StrToIntA,
        StrToIntExA,
        StrDupA,
        StrTrimA,
        StrFormatByteSizeA,
    ]
}
