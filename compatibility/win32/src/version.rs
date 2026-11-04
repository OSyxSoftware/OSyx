//! version.dll: real RT_VERSION data from PE files, not fabricated versions.
//! MUI satellite merging is not implemented. Neutral numeric resource
//! selection and UTF-16 VerQueryValue queries are backed by the PE parser.
//! winrun's built-in system DLLs have no file: their version is the
//! Windows version winrun reports, as a system DLL's is on Windows.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, path};
use alloc::{string::String, vec::Vec};
use osyx_pe::{version_value, PeFile};

/// Appends a VS_VERSIONINFO-style node: its header, `key`, the value
/// (DWORD-aligned) and the children, fixing up wLength.
fn node(out: &mut Vec<u8>, key: &str, value: &[u8], text: bool, children: impl FnOnce(&mut Vec<u8>)) {
    let pad = |out: &mut Vec<u8>| {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    };
    pad(out);
    let start = out.len();
    out.extend_from_slice(&0u16.to_le_bytes());
    let value_len = if text { value.len() / 2 } else { value.len() };
    out.extend_from_slice(&(value_len as u16).to_le_bytes());
    out.extend_from_slice(&u16::from(text).to_le_bytes());
    for c in key.encode_utf16().chain(core::iter::once(0)) {
        out.extend_from_slice(&c.to_le_bytes());
    }
    pad(out);
    out.extend_from_slice(value);
    children(out);
    let len = (out.len() - start) as u16;
    out[start..start + 2].copy_from_slice(&len.to_le_bytes());
}

/// The version resource of a built-in system DLL: VS_FIXEDFILEINFO with
/// the reported Windows version, and an English/Unicode translation.
fn builtin_version() -> Vec<u8> {
    let (major, minor, build) = crate::sysext::windows_version();
    let mut fixed = Vec::with_capacity(52);
    for v in [
        0xfeef_04bdu32,           // signature
        0x0001_0000,              // structure version
        major << 16 | minor,      // file version, most significant
        build << 16,              // file version, least significant
        major << 16 | minor,      // product version
        build << 16,
        0x3f,                     // flags mask
        0,                        // flags
        0x0004_0004,              // VOS_NT_WINDOWS32
        2,                        // VFT_DLL
        0,                        // subtype
        0,                        // date
        0,
    ] {
        fixed.extend_from_slice(&v.to_le_bytes());
    }
    let mut out = Vec::new();
    node(&mut out, "VS_VERSION_INFO", &fixed, false, |out| {
        node(out, "VarFileInfo", &[], true, |out| {
            node(out, "Translation", &0x04b0_0409u32.to_le_bytes(), false, |_| {});
        });
    });
    out
}

/// A built-in system DLL named by file name alone or in System32.
fn builtin_named(name: &str) -> bool {
    let lower = name.replace('/', "\\").to_ascii_lowercase();
    let (dir, file) = match lower.rfind('\\') {
        Some(i) => (&lower[..i], &lower[i + 1..]),
        None => ("", lower.as_str()),
    };
    let system = dir.is_empty() || dir.ends_with("\\windows\\system32") || dir.ends_with("\\windows\\syswow64");
    system && crate::loader::is_builtin_dll(file)
}

fn read_version(filename: *const u16) -> Result<Vec<u8>, u32> {
    let name = unsafe { path::wide_to_string(filename) }.ok_or(ERROR_INVALID_PARAMETER)?;
    let from_file = read_file_version(&name);
    match from_file {
        Err(ERROR_FILE_NOT_FOUND) | Err(ERROR_PATH_NOT_FOUND) if builtin_named(&name) => Ok(builtin_version()),
        r => r,
    }
}

fn read_file_version(name: &str) -> Result<Vec<u8>, u32> {
    let name = String::from(name);
    let cwd = os_std::env::current_dir().map(|p| path::to_windows(p.as_str())).map_err(|_| ERROR_PATH_NOT_FOUND)?;
    let osyx = path::to_osyx(&name, &cwd)?;
    let (file, exists) = path::resolve(&osyx)?;
    if !exists {
        return Err(ERROR_FILE_NOT_FOUND);
    }
    let bytes = os_std::fs::read(&file).map_err(|e| e.raw_os_error().map_or(ERROR_GEN_FAILURE, errno_to_win32))?;
    let pe = PeFile::parse(&bytes).map_err(|_| 193u32)?; // ERROR_BAD_EXE_FORMAT
    let data = pe.resource(16, None, None).map_err(|_| 1812u32)?.ok_or(1813u32)?;
    let fixed = version_value(data, "\\").map_err(|_| 1812u32)?.ok_or(1812u32)?;
    if fixed.text || fixed.data.len() != 52 || fixed.data.get(..4) != Some(&0xfeef04bdu32.to_le_bytes()) {
        return Err(1812);
    }
    Ok(data.to_vec())
}
pub(crate) extern "win64" fn GetFileVersionInfoSizeW(filename: *const u16, handle: *mut u32) -> u32 {
    if !handle.is_null() {
        unsafe {
            *handle = 0;
        }
    }
    match read_version(filename) {
        Ok(data) => data.len() as u32,
        Err(e) => fail(e, 0),
    }
}
pub(crate) extern "win64" fn GetFileVersionInfoW(filename: *const u16, _handle: u32, len: u32, out: *mut u8) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    match read_version(filename) {
        Ok(data) => {
            if len < data.len() as u32 {
                return fail(ERROR_INSUFFICIENT_BUFFER, 0);
            }
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), out, data.len());
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}
pub(crate) extern "win64" fn VerQueryValueW(block: *const u8, query: *const u16, out: *mut *const u8, len: *mut u32) -> i32 {
    if block.is_null() || out.is_null() || len.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(query): Option<String> = (unsafe { path::wide_to_string(query) }) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    // Windows supplies no size argument: VS_VERSIONINFO's WORD wLength
    // bounds the borrowed block; inaccessible caller pointers fault only
    // this ring-3 process, as for other pointer-taking compatibility APIs.
    let size = unsafe { core::ptr::read_unaligned(block as *const u16) } as usize;
    let data = unsafe { core::slice::from_raw_parts(block, size) };
    unsafe {
        *out = core::ptr::null();
        *len = 0;
    }
    match version_value(data, &query) {
        Ok(Some(value)) => {
            unsafe {
                *out = value.data.as_ptr();
                *len = value.length;
            }
            1
        }
        Ok(None) => fail(1813, 0),
        Err(_) => fail(1812, 0),
    }
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW]
}
