//! The native registry calls (NtCreateKey, NtOpenKey[Ex], NtDeleteKey,
//! NtQueryValueKey, NtSetValueKey) over the Win32 registry of registry.rs.
//! NT key names are `\Registry\Machine\...` for HKEY_LOCAL_MACHINE and
//! `\Registry\User\<SID>\...` (or `<SID>_Classes`) for the current user's
//! hive; a name relative to an open key is that key's subkey. Key handles
//! are the registry's own, so NtClose closes them through
//! [`registry::close_key`].
#![allow(non_snake_case)]
use crate::error::*;
use crate::{kernel32, objsec, registry, teb, tokens};
use alloc::{string::String, vec::Vec};

const HKEY_LOCAL_MACHINE: u64 = 0xFFFF_FFFF_8000_0002;
const HKEY_CURRENT_USER: u64 = 0xFFFF_FFFF_8000_0001;
const STATUS_BUFFER_OVERFLOW: NtStatus = 0x8000_0005;
const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023;
const STATUS_OBJECT_PATH_SYNTAX_BAD: NtStatus = 0xC000_003B;
const STATUS_KEY_DELETED: NtStatus = 0xC000_017C;
const STATUS_CHILD_MUST_BE_VOLATILE: NtStatus = 0xC000_0181;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_KEY_DELETED: u32 = 1018;
const ERROR_CHILD_MUST_BE_VOLATILE: u32 = 1021;

/// The NTSTATUS of a registry.rs Win32 error.
fn status(e: u32) -> NtStatus {
    match e {
        ERROR_SUCCESS => STATUS_SUCCESS,
        ERROR_FILE_NOT_FOUND => STATUS_OBJECT_NAME_NOT_FOUND,
        ERROR_ACCESS_DENIED => STATUS_ACCESS_DENIED,
        ERROR_INVALID_HANDLE => STATUS_INVALID_HANDLE,
        ERROR_KEY_DELETED => STATUS_KEY_DELETED,
        ERROR_CHILD_MUST_BE_VOLATILE => STATUS_CHILD_MUST_BE_VOLATILE,
        ERROR_MORE_DATA => STATUS_BUFFER_OVERFLOW,
        ERROR_NOT_ENOUGH_MEMORY => STATUS_NO_MEMORY,
        ERROR_DISK_FULL => STATUS_DISK_FULL,
        ERROR_INVALID_PARAMETER => STATUS_INVALID_PARAMETER,
        _ => STATUS_UNSUCCESSFUL,
    }
}

/// The current user's SID in string form ("S-1-5-21-...").
fn user_sid() -> String {
    objsec::sid_string(&tokens::user_sid(os_sys::getuid().0))
}

/// The UTF-16 text of a UNICODE_STRING (Length, MaximumLength, padding,
/// Buffer); None for a NULL or malformed one.
///
/// # Safety
/// `p` is NULL or a readable UNICODE_STRING whose buffer holds `Length`
/// bytes.
unsafe fn unicode(p: u64) -> Option<Vec<u16>> {
    if p == 0 {
        return None;
    }
    // SAFETY: the caller's structure.
    let (len, buf): (u16, u64) = unsafe { (teb::get(p, 0), teb::get(p, 8)) };
    if len % 2 != 0 || (buf == 0 && len != 0) {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    // SAFETY: as above.
    Some(unsafe { core::slice::from_raw_parts(buf as *const u16, len as usize / 2) }.to_vec())
}

fn nul_terminated(mut s: Vec<u16>) -> Vec<u16> {
    s.push(0);
    s
}

/// The key OBJECT_ATTRIBUTES `oa` names, as a key to open from and the
/// NUL-terminated path below it.
///
/// # Safety
/// `oa` is NULL or a readable OBJECT_ATTRIBUTES.
unsafe fn target(oa: u64) -> Result<(u64, Vec<u16>), NtStatus> {
    if oa == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    // SAFETY: the caller's OBJECT_ATTRIBUTES (RootDirectory at 8,
    // ObjectName at 16).
    let (root, name) = unsafe { (teb::get::<u64>(oa, 8), unicode(teb::get(oa, 16))) };
    let name = name.ok_or(STATUS_INVALID_PARAMETER)?;
    if root != 0 {
        if name.first() == Some(&(b'\\' as u16)) {
            return Err(STATUS_OBJECT_PATH_SYNTAX_BAD);
        }
        return Ok((root, nul_terminated(name)));
    }
    let s = String::from_utf16_lossy(&name);
    let parts: Vec<&str> = s.split('\\').collect();
    if parts.len() < 3 || !parts[0].is_empty() || !parts[1].eq_ignore_ascii_case("registry") {
        return Err(STATUS_OBJECT_PATH_SYNTAX_BAD);
    }
    let (hkey, rest): (u64, Vec<&str>) = if parts[2].eq_ignore_ascii_case("machine") {
        (HKEY_LOCAL_MACHINE, parts[3..].to_vec())
    } else if parts[2].eq_ignore_ascii_case("user") && parts.len() > 3 {
        let sid = user_sid();
        if parts[3].eq_ignore_ascii_case(&sid) {
            (HKEY_CURRENT_USER, parts[4..].to_vec())
        } else if parts[3].len() == sid.len() + 8 && parts[3].get(..sid.len()).is_some_and(|p| p.eq_ignore_ascii_case(&sid)) && parts[3][sid.len()..].eq_ignore_ascii_case("_Classes") {
            let mut r = alloc::vec!["Software", "Classes"];
            r.extend_from_slice(&parts[4..]);
            (HKEY_CURRENT_USER, r)
        } else {
            // Other users' hives are not loaded in this process.
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        }
    } else {
        return Err(STATUS_OBJECT_NAME_NOT_FOUND);
    };
    Ok((hkey, nul_terminated(rest.join("\\").encode_utf16().collect())))
}

const REG_OPTION_VOLATILE: u32 = 1;
const REG_OPTION_CREATE_LINK: u32 = 2;
const REG_OPTION_BACKUP_RESTORE: u32 = 4;
const REG_OPTION_OPEN_LINK: u32 = 8;

extern "win64" fn NtCreateKey(out: *mut u64, access: u32, oa: u64, _title: u32, _class: u64, options: u32, disposition: *mut u32) -> NtStatus {
    if out.is_null() || options & !(REG_OPTION_VOLATILE | REG_OPTION_CREATE_LINK | REG_OPTION_BACKUP_RESTORE | REG_OPTION_OPEN_LINK) != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: the caller's OBJECT_ATTRIBUTES.
    let (root, path) = match unsafe { target(oa) } {
        Ok(t) => t,
        Err(s) => return s,
    };
    // Backup-restore semantics and opening a link itself have no meaning
    // without ACL checks and symbolic-link opens: the key is opened.
    let options = options & (REG_OPTION_VOLATILE | REG_OPTION_CREATE_LINK);
    status(registry::RegCreateKeyExW(root, path.as_ptr(), 0, core::ptr::null(), options, access, 0, out, disposition))
}

extern "win64" fn NtOpenKeyEx(out: *mut u64, access: u32, oa: u64, options: u32) -> NtStatus {
    if out.is_null() || options & !(REG_OPTION_BACKUP_RESTORE | REG_OPTION_OPEN_LINK) != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: the caller's OBJECT_ATTRIBUTES.
    let (root, path) = match unsafe { target(oa) } {
        Ok(t) => t,
        Err(s) => return s,
    };
    status(registry::RegOpenKeyExW(root, path.as_ptr(), options & REG_OPTION_OPEN_LINK, access, out))
}

extern "win64" fn NtOpenKey(out: *mut u64, access: u32, oa: u64) -> NtStatus {
    NtOpenKeyEx(out, access, oa, 0)
}

extern "win64" fn NtDeleteKey(h: u64) -> NtStatus {
    status(registry::delete_key(h))
}

extern "win64" fn NtSetValueKey(h: u64, name: u64, _title: u32, kind: u32, data: *const u8, size: u32) -> NtStatus {
    // SAFETY: NULL or the caller's UNICODE_STRING.
    let Some(name) = (unsafe { unicode(name) }) else { return STATUS_INVALID_PARAMETER };
    if size != 0 && data.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    status(registry::RegSetValueExW(h, nul_terminated(name).as_ptr(), 0, kind, data, size))
}

const KEY_VALUE_BASIC: u32 = 0;
const KEY_VALUE_FULL: u32 = 1;
const KEY_VALUE_PARTIAL: u32 = 2;
const KEY_VALUE_PARTIAL_ALIGN64: u32 = 3;

/// NtQueryValueKey: KEY_VALUE_BASIC_INFORMATION, _FULL_ (the data right
/// after the name), _PARTIAL_ and _PARTIAL_INFORMATION_ALIGN64. A buffer
/// smaller than the fixed part gets STATUS_BUFFER_TOO_SMALL; one that
/// holds the fixed part but not all of it gets what fits and
/// STATUS_BUFFER_OVERFLOW. `ret` always says the whole size.
extern "win64" fn NtQueryValueKey(h: u64, name: u64, class: u32, buf: *mut u8, len: u32, ret: *mut u32) -> NtStatus {
    // SAFETY: NULL or the caller's UNICODE_STRING.
    let Some(name) = (unsafe { unicode(name) }) else { return STATUS_INVALID_PARAMETER };
    if ret.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let wide = nul_terminated(name.clone());
    let (mut kind, mut size) = (0u32, 0u32);
    let e = registry::RegQueryValueExW(h, wide.as_ptr(), core::ptr::null(), &mut kind, core::ptr::null_mut(), &mut size);
    if e != ERROR_SUCCESS {
        return status(e);
    }
    let mut data = alloc::vec![0u8; size as usize];
    if class != KEY_VALUE_BASIC && size != 0 {
        let e = registry::RegQueryValueExW(h, wide.as_ptr(), core::ptr::null(), &mut kind, data.as_mut_ptr(), &mut size);
        if e != ERROR_SUCCESS {
            return status(e);
        }
        data.truncate(size as usize);
    }
    let name_bytes: Vec<u8> = name.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut out: Vec<u8> = Vec::new();
    let mut word = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    let fixed = match class {
        KEY_VALUE_BASIC => {
            word(0);
            word(kind);
            word(name_bytes.len() as u32);
            out.extend_from_slice(&name_bytes);
            12
        }
        KEY_VALUE_FULL => {
            let offset = 20 + name_bytes.len() as u32;
            word(0);
            word(kind);
            word(offset);
            word(data.len() as u32);
            word(name_bytes.len() as u32);
            out.extend_from_slice(&name_bytes);
            out.extend_from_slice(&data);
            20
        }
        KEY_VALUE_PARTIAL => {
            word(0);
            word(kind);
            word(data.len() as u32);
            out.extend_from_slice(&data);
            12
        }
        KEY_VALUE_PARTIAL_ALIGN64 => {
            word(kind);
            word(data.len() as u32);
            out.extend_from_slice(&data);
            8
        }
        _ => return STATUS_INVALID_INFO_CLASS,
    };
    // SAFETY: the caller's out pointer.
    unsafe { *ret = out.len() as u32 };
    if (len as usize) < fixed {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if buf.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let n = out.len().min(len as usize);
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), buf, n) };
    if n < out.len() { STATUS_BUFFER_OVERFLOW } else { STATUS_SUCCESS }
}

/// "\REGISTRY\USER\<SID>" in a new UNICODE_STRING, freed with
/// RtlFreeUnicodeString.
extern "win64" fn RtlFormatCurrentUserKeyPath(out: u64) -> NtStatus {
    if out == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let path: Vec<u16> = alloc::format!("\\REGISTRY\\USER\\{}", user_sid()).encode_utf16().chain([0]).collect();
    let bytes = path.len() * 2;
    let Some(p) = kernel32::heap_alloc(bytes, false) else { return STATUS_NO_MEMORY };
    // SAFETY: `p` holds `bytes` bytes; `out` is the caller's UNICODE_STRING.
    unsafe {
        core::ptr::copy_nonoverlapping(path.as_ptr(), p as *mut u16, path.len());
        teb::put(out, 0, (bytes - 2) as u16);
        teb::put(out, 2, bytes as u16);
        teb::put(out, 8, p);
    }
    STATUS_SUCCESS
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![NtCreateKey, NtOpenKeyEx, NtOpenKey, NtDeleteKey, NtSetValueKey, NtQueryValueKey, RtlFormatCurrentUserKeyPath]
}
