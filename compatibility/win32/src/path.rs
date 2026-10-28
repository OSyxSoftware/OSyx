//! Windows path names on the OSyx VFS.
//!
//! `C:` is the OSyx root (`C:\home\user` is `/home/user`); `\` and `/`
//! both separate components; `\\?\` prefixes are accepted; `NUL` is
//! `/dev/null`. Names are matched ASCII case-insensitively, component by
//! component, keeping each file's canonical spelling (an exact match
//! wins, as on the VFS); new names keep the caller's spelling. Other drive
//! letters and UNC paths do not exist (`ERROR_PATH_NOT_FOUND`).

use crate::error::*;
use alloc::string::String;
use alloc::vec::Vec;
use os_std::fs;

/// TEMP: histogram of file writes, dumped every 1000 calls.
pub fn note_write(path: &str, n: usize) {
    if !crate::EVENTS.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    static N: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    static W: os_std::sync::Mutex<Vec<(String, u64, u64)>> = os_std::sync::Mutex::new(Vec::new());
    let k = N.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    let mut list = W.lock().unwrap();
    match list.iter_mut().find(|(p, _, _)| p == path) {
        Some(e) => {
            e.1 += 1;
            e.2 += n as u64;
        }
        None => list.push((String::from(path), 1, n as u64)),
    }
    if k.is_multiple_of(1000) {
        list.sort_unstable_by_key(|a| core::cmp::Reverse(a.1));
        let mut s = String::new();
        for (p, c, b) in list.iter().take(8) {
            core::fmt::Write::write_fmt(&mut s, format_args!(" {}x{c}/{b}B", p)).ok();
        }
        os_std::eprintln!("winrun: write top n={k} tid={}{s}", os_sys::gettid());
        list.clear();
    }
}

/// TEMP: logs every 1000th stat/lstat the shims perform, with its caller tag.
pub fn note_stat(path: &str, what: &str) {
    if !crate::EVENTS.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    static N: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    static STATS: os_std::sync::Mutex<Vec<(String, u64)>> = os_std::sync::Mutex::new(Vec::new());
    let n = N.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    let mut list = STATS.lock().unwrap();
    match list.iter_mut().find(|(p, _)| p == path) {
        Some(e) => e.1 += 1,
        None => list.push((String::from(path), 1)),
    }
    if n.is_multiple_of(1000) {
        list.sort_unstable_by_key(|a| core::cmp::Reverse(a.1));
        let mut s = String::new();
        for (p, c) in list.iter().take(8) {
            core::fmt::Write::write_fmt(&mut s, format_args!(" {}x{c}", p)).ok();
        }
        os_std::eprintln!("winrun: stat top n={n} what={what} tid={}{s}", os_sys::gettid());
        list.clear();
    }
}

/// Longest path accepted (Windows `MAX_PATH` does not apply with `\\?\`).
pub const MAX_PATH_CHARS: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Special {
    Null,
    ConsoleOut,
    ConsoleIn,
}

/// Translates a Windows path to an absolute OSyx path (not yet matched
/// case-insensitively). Errors are Win32 codes.
pub fn to_osyx(win: &str, cwd: &str) -> Result<String, u32> {
    if win.is_empty() || win.len() > MAX_PATH_CHARS || win.contains('\0') {
        return Err(if win.len() > MAX_PATH_CHARS { ERROR_FILENAME_EXCED_RANGE } else { ERROR_INVALID_NAME });
    }
    let mut p: String = win.replace('/', "\\");
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        p = String::from(rest);
    }
    if p.starts_with("\\\\") {
        return Err(ERROR_PATH_NOT_FOUND); // UNC
    }
    let b = p.as_bytes();
    let (rooted, rest): (bool, &str) = if b.len() >= 2 && b[1] == b':' {
        if !b[0].eq_ignore_ascii_case(&b'c') {
            return Err(ERROR_PATH_NOT_FOUND);
        }
        let r = &p[2..];
        if r.starts_with('\\') {
            (true, r)
        } else {
            // "C:foo" is relative to the current directory of drive C.
            (false, r)
        }
    } else {
        (p.starts_with('\\'), p.as_str())
    };
    let mut parts: Vec<&str> = Vec::new();
    if !rooted {
        let c = cwd.get(2..).unwrap_or("\\");
        parts.extend(c.split('\\').filter(|s| !s.is_empty()));
    }
    for comp in rest.split('\\') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => {
                // Trailing dots and spaces are dropped by Windows.
                let t = c.trim_end_matches(['.', ' ']);
                if t.is_empty() || t.contains(['<', '>', '"', '|', '?', '*', ':']) {
                    return Err(ERROR_INVALID_NAME);
                }
                parts.push(t);
            }
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    Ok(out)
}

/// Device names that are not files.
pub fn special(win: &str) -> Option<Special> {
    let last = win.rsplit(['\\', '/']).next().unwrap_or(win);
    let stem = last.split('.').next().unwrap_or("");
    if stem.eq_ignore_ascii_case("NUL") {
        Some(Special::Null)
    } else if last.eq_ignore_ascii_case("CONOUT$") || stem.eq_ignore_ascii_case("CON") {
        Some(Special::ConsoleOut)
    } else if last.eq_ignore_ascii_case("CONIN$") {
        Some(Special::ConsoleIn)
    } else {
        None
    }
}

/// The OSyx path back as a Windows path.
pub fn to_windows(osyx: &str) -> String {
    let mut s = String::from("C:");
    if osyx == "/" {
        s.push('\\');
    } else {
        s.push_str(&osyx.replace('/', "\\"));
    }
    s
}

/// Matches `path` (absolute, `/`-separated) case-insensitively. Returns
/// the canonical path and whether the last component exists; a missing
/// directory before the last component is `ERROR_PATH_NOT_FOUND`.
pub fn resolve(path: &str) -> Result<(String, bool), u32> {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let mut cur = String::from("/");
    for (i, comp) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        let exact = join(&cur, comp);
        note_stat(&exact, "resolve");
        let found = match fs::symlink_metadata(&exact) {
            Ok(m) => Some((exact, Some(m))),
            Err(_) => find_ci(&cur, comp).map(|n| {
                let p = join(&cur, &n);
                let m = fs::symlink_metadata(&p).ok();
                (p, m)
            }),
        };
        match found {
            Some((p, m)) => {
                if !last {
                    // A link still has to be followed; a plain non-directory
                    // is rejected without a second syscall.
                    let is_dir = match m {
                        Some(m) if m.is_dir() => true,
                        Some(m) if !m.is_symlink() => false,
                        _ => fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false),
                    };
                    if !is_dir {
                        return Err(ERROR_PATH_NOT_FOUND);
                    }
                }
                cur = p;
            }
            None if last => return Ok((join(&cur, comp), false)),
            None => return Err(ERROR_PATH_NOT_FOUND),
        }
    }
    Ok((cur, true))
}

fn join(dir: &str, name: &str) -> String {
    let mut s = String::from(dir);
    if !s.ends_with('/') {
        s.push('/');
    }
    s.push_str(name);
    s
}

fn find_ci(dir: &str, name: &str) -> Option<String> {
    let rd = fs::read_dir(dir).ok()?;
    for e in rd {
        if e.file_name().eq_ignore_ascii_case(name) {
            return Some(String::from(e.file_name()));
        }
    }
    None
}

/// Decodes a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must point to a readable NUL-terminated UTF-16 string.
pub unsafe fn wide_to_string(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: the caller guarantees a terminator within readable memory.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
        if n > MAX_PATH_CHARS * 8 {
            return None;
        }
    }
    // SAFETY: `n` units were just read.
    let units = unsafe { core::slice::from_raw_parts(p, n) };
    Some(char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect())
}

/// A NUL-terminated ANSI string as NUL-terminated UTF-16, for forwarding
/// an -A function to its -W implementation (None for NULL).
///
/// # Safety
/// As [`ansi_to_string`].
pub unsafe fn ansi_to_wide(p: *const u8) -> Option<Vec<u16>> {
    // SAFETY: forwarded.
    unsafe { ansi_to_string(p) }.map(|s| s.encode_utf16().chain(core::iter::once(0)).collect())
}

/// Decodes a NUL-terminated ANSI string (treated as UTF-8, bytes above
/// 0x7F that are not valid UTF-8 as Latin-1).
///
/// # Safety
/// `p` must point to a readable NUL-terminated byte string.
pub unsafe fn ansi_to_string(p: *const u8) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: as above.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
        if n > MAX_PATH_CHARS * 8 {
            return None;
        }
    }
    // SAFETY: `n` bytes were just read.
    let bytes = unsafe { core::slice::from_raw_parts(p, n) };
    Some(match core::str::from_utf8(bytes) {
        Ok(s) => String::from(s),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    })
}
