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
        let found = if fs::symlink_metadata(&exact).is_ok() {
            Some(exact)
        } else {
            find_ci(&cur, comp).map(|n| join(&cur, &n))
        };
        match found {
            Some(p) => {
                if !last && !fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) {
                    return Err(ERROR_PATH_NOT_FOUND);
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
