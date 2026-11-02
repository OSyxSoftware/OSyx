//! version.dll: real RT_VERSION data from PE files, not fabricated versions.
//! MUI satellite merging is not implemented. Neutral numeric resource
//! selection and UTF-16 VerQueryValue queries are backed by the PE parser.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, path};
use alloc::{string::String, vec::Vec};
use osyx_pe::{version_value, PeFile};

fn read_version(filename: *const u16) -> Result<Vec<u8>, u32> {
    let name = unsafe { path::wide_to_string(filename) }.ok_or(ERROR_INVALID_PARAMETER)?;
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
