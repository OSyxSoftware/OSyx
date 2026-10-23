//! File mappings: `CreateFileMapping`/`MapViewOfFile` and the views behind
//! `NtMapViewOfSection`.
//!
//! Page-file sections are OSyx shared memory objects; their views alias
//! the same pages. File-backed sections are read-only or copy-on-write:
//! each view is private memory filled from the file at map time, which is
//! exactly what Windows gives those views until the file itself changes.
//! Writable shared file views (`PAGE_READWRITE` with a file handle) and
//! image sections (`SEC_IMAGE`) need kernel page-cache sharing and fail
//! with `ERROR_NOT_SUPPORTED` rather than silently losing writes.
//! Section names are process-local, like the other named objects here.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    mem::{self, Prot, GRANULARITY, PAGE, PAGE_EXECUTE_READ, PAGE_EXECUTE_WRITECOPY, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY},
    objects::{self, Object, SectionObj},
    path, teb,
};
use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use os_std::sync::Mutex;

pub const FILE_MAP_COPY: u32 = 0x1;
pub const FILE_MAP_WRITE: u32 = 0x2;
pub const FILE_MAP_READ: u32 = 0x4;
pub const FILE_MAP_EXECUTE: u32 = 0x20;
const SEC_COMMIT: u32 = 0x800_0000;
const SEC_RESERVE: u32 = 0x400_0000;
const SEC_IMAGE: u32 = 0x100_0000;
const INVALID_HANDLE_VALUE: u64 = u64::MAX;
pub const MEM_MAPPED: u32 = 0x40000;

const STATUS_NOT_MAPPED_VIEW: NtStatus = 0xC000_0019;
const STATUS_INVALID_VIEW_SIZE: NtStatus = 0xC000_001F;
const STATUS_SECTION_TOO_BIG: NtStatus = 0xC000_0040;
const STATUS_MAPPED_FILE_SIZE_ZERO: NtStatus = 0xC000_011E;
const STATUS_MAPPED_ALIGNMENT: NtStatus = 0xC000_0220;
const ERROR_FILE_INVALID: u32 = 1006;
const ERROR_MAPPED_ALIGNMENT: u32 = 1132;

fn to_win32(s: NtStatus) -> u32 {
    match s {
        STATUS_NOT_MAPPED_VIEW => ERROR_INVALID_ADDRESS,
        STATUS_INVALID_VIEW_SIZE => ERROR_ACCESS_DENIED,
        STATUS_SECTION_TOO_BIG => ERROR_NOT_ENOUGH_MEMORY,
        STATUS_MAPPED_FILE_SIZE_ZERO => ERROR_FILE_INVALID,
        STATUS_MAPPED_ALIGNMENT => ERROR_MAPPED_ALIGNMENT,
        s => status_to_win32(s),
    }
}

struct View {
    size: u64,
    /// Windows protection reported by VirtualQuery.
    protect: u32,
    allocation_protect: u32,
}

static VIEWS: Mutex<BTreeMap<u64, View>> = Mutex::new(BTreeMap::new());

/// A new section. `file` is None for page-file memory of `size` bytes;
/// with a file, `size` 0 means the file's size.
pub fn create(file: Option<u64>, protect: u32, size: u64, name: Option<String>) -> Result<(u64, bool), NtStatus> {
    if protect & SEC_IMAGE != 0 {
        return Err(STATUS_NOT_SUPPORTED);
    }
    if protect & !(0xFF | SEC_COMMIT | SEC_RESERVE) != 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let page = protect & 0xFF;
    let (writable, executable) = match page {
        PAGE_READONLY => (false, false),
        PAGE_READWRITE | PAGE_WRITECOPY => (true, false),
        PAGE_EXECUTE_READ => (false, true),
        // W^X: a mapping that may become writable and executable.
        mem::PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => return Err(STATUS_NOT_SUPPORTED),
        _ => return Err(STATUS_INVALID_PAGE_PROTECTION),
    };
    if let Some(n) = name.as_deref() {
        if let Some(old) = objects::find_named(n) {
            return match old {
                Object::Section(_) => Ok((objects::insert(old), true)),
                _ => Err(STATUS_OBJECT_NAME_COLLISION),
            };
        }
    }
    let section = match file {
        None => {
            if size == 0 {
                return Err(STATUS_INVALID_PARAMETER);
            }
            let shm = os_sys::shm_create(size).map_err(errno_to_status)?;
            SectionObj { shm, size, writable, executable, file: None, name }
        }
        Some(h) => {
            let f = objects::file(h).map_err(|_| STATUS_INVALID_HANDLE)?;
            if !f.access.0 || (page == PAGE_READWRITE && !f.access.1) {
                return Err(STATUS_ACCESS_DENIED);
            }
            if page == PAGE_READWRITE {
                return Err(STATUS_NOT_SUPPORTED);
            }
            let len = os_sys::fstat(f.fd).map_err(errno_to_status)?.size;
            let size = if size == 0 { len } else { size };
            if size == 0 {
                return Err(STATUS_MAPPED_FILE_SIZE_ZERO);
            }
            // Growing the file needs write access, which these sections lack.
            if size > len {
                return Err(STATUS_SECTION_TOO_BIG);
            }
            // PAGE_WRITECOPY grants read (and copy) views only.
            SectionObj { shm: 0, size, writable: false, executable, file: Some(f), name }
        }
    };
    Ok((objects::insert(Object::Section(Arc::new(section))), false))
}

/// Reads `len` bytes at `offset` of `fd` into `dst` without moving the
/// file pointer other handles share. Bytes past the end stay zero.
fn read_at(fd: u64, offset: u64, dst: u64, len: usize) -> Result<(), NtStatus> {
    let saved = os_sys::seek(fd, 0, osyx_abi::SEEK_CUR).map_err(errno_to_status)?;
    let mut result = os_sys::seek(fd, offset as i64, osyx_abi::SEEK_SET).map(|_| ());
    let mut done = 0;
    while result.is_ok() && done < len {
        match os_sys::read_raw(fd, (dst as usize) + done, len - done) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) => result = Err(e),
        }
    }
    let restored = os_sys::seek(fd, saved as i64, osyx_abi::SEEK_SET);
    result.and(restored.map(|_| ())).map_err(errno_to_status)
}

/// Maps a view of `section`: `access` is FILE_MAP_* rights, `len` 0 maps
/// to the end. Returns (base, size).
pub fn map(section: u64, access: u32, offset: u64, len: u64) -> Result<(u64, u64), NtStatus> {
    let Some(Object::Section(s)) = objects::get(section) else { return Err(STATUS_INVALID_HANDLE) };
    if !offset.is_multiple_of(GRANULARITY as u64) {
        return Err(STATUS_MAPPED_ALIGNMENT);
    }
    // FILE_MAP_COPY (1) is also SECTION_QUERY inside FILE_MAP_ALL_ACCESS,
    // so write access takes precedence, as in kernelbase.
    let write = access & FILE_MAP_WRITE != 0;
    let copy = !write && access & FILE_MAP_COPY != 0;
    let exec = access & FILE_MAP_EXECUTE != 0;
    if (write && !s.writable) || (exec && !s.executable) {
        return Err(STATUS_ACCESS_DENIED);
    }
    if exec && (write || copy) {
        return Err(STATUS_NOT_SUPPORTED); // W^X
    }
    if offset >= s.size {
        return Err(STATUS_INVALID_VIEW_SIZE);
    }
    let len = if len == 0 { s.size - offset } else { len };
    if len > s.size - offset {
        return Err(STATUS_INVALID_VIEW_SIZE);
    }
    let (base, protect) = match &s.file {
        None => {
            // Shared pages: views alias the whole object.
            if offset != 0 || copy || exec {
                return Err(STATUS_NOT_SUPPORTED);
            }
            let p = os_sys::shm_map(s.shm, write).map_err(errno_to_status)? as u64;
            (p, if write { PAGE_READWRITE } else { PAGE_READONLY })
        }
        Some(f) => {
            let size = len as usize;
            let p = mem::alloc_pages(size, Prot::ReadWrite).ok_or(STATUS_NO_MEMORY)?;
            if let Err(e) = read_at(f.fd, offset, p, size) {
                mem::free_pages(p, size);
                return Err(e);
            }
            let (prot, protect) = if copy {
                (Prot::ReadWrite, PAGE_WRITECOPY)
            } else if exec {
                (Prot::ExecuteRead, PAGE_EXECUTE_READ)
            } else {
                (Prot::ReadOnly, PAGE_READONLY)
            };
            if prot != Prot::ReadWrite {
                if let Err(e) = mem::protect(p, size, prot) {
                    mem::free_pages(p, size);
                    return Err(errno_to_status(e));
                }
            }
            (p, protect)
        }
    };
    let size = if s.file.is_some() { len } else { s.size };
    let allocation_protect = if s.writable { PAGE_READWRITE } else { PAGE_READONLY };
    VIEWS.lock().unwrap().insert(base, View { size, protect, allocation_protect });
    Ok((base, size))
}

/// Unmaps the view containing `addr`.
pub fn unmap(addr: u64) -> Result<(), NtStatus> {
    let mut views = VIEWS.lock().unwrap();
    let base = match views.range(..=addr).next_back() {
        Some((&b, v)) if addr < b + v.size => b,
        _ => return Err(STATUS_NOT_MAPPED_VIEW),
    };
    let v = views.remove(&base).unwrap();
    // SAFETY: a whole view made by `map`.
    unsafe { os_sys::unmap(base as *mut u8, (v.size as usize).next_multiple_of(PAGE)) }.map_err(errno_to_status)
}

/// What VirtualQuery reports for an address inside a view.
pub fn query(addr: u64) -> Option<mem::Info> {
    let views = VIEWS.lock().unwrap();
    let (&base, v) = views.range(..=addr).next_back()?;
    let size = (v.size as usize).next_multiple_of(PAGE) as u64;
    (addr < base + size).then(|| mem::Info {
        base: addr & !(PAGE as u64 - 1),
        allocation_base: base,
        allocation_protect: v.allocation_protect,
        size: base + size - (addr & !(PAGE as u64 - 1)),
        state: mem::MEM_COMMIT,
        protect: v.protect,
        kind: MEM_MAPPED,
    })
}

fn create_common(file: u64, attributes: u64, protect: u32, high: u32, low: u32, name: Option<String>) -> u64 {
    if let Err(e) = crate::security::validate_attributes(attributes) {
        return fail(e, 0);
    }
    let file = (file != INVALID_HANDLE_VALUE).then_some(file);
    match create(file, protect, (high as u64) << 32 | low as u64, name) {
        Ok((h, existed)) => {
            teb::set_last_error(if existed { ERROR_ALREADY_EXISTS } else { ERROR_SUCCESS });
            h
        }
        Err(s) => fail(to_win32(s), 0),
    }
}

extern "win64" fn CreateFileMappingW(file: u64, attributes: u64, protect: u32, high: u32, low: u32, name: *const u16) -> u64 {
    // SAFETY: the caller passes NULL or a NUL-terminated name.
    create_common(file, attributes, protect, high, low, unsafe { path::wide_to_string(name) })
}

extern "win64" fn CreateFileMappingA(file: u64, attributes: u64, protect: u32, high: u32, low: u32, name: *const u8) -> u64 {
    // SAFETY: as above.
    create_common(file, attributes, protect, high, low, unsafe { path::ansi_to_string(name) })
}

extern "win64" fn MapViewOfFile(section: u64, access: u32, high: u32, low: u32, len: usize) -> u64 {
    match map(section, access, (high as u64) << 32 | low as u64, len as u64) {
        Ok((base, _)) => base,
        Err(s) => fail(to_win32(s), 0),
    }
}

extern "win64" fn UnmapViewOfFile(addr: u64) -> i32 {
    match unmap(addr) {
        Ok(()) => 1,
        Err(s) => fail(to_win32(s), 0),
    }
}

/// Views are private copies or shared memory: nothing is pending for the
/// file, so flushing only validates the address.
extern "win64" fn FlushViewOfFile(addr: u64, _len: usize) -> i32 {
    if query(addr).is_some() { 1 } else { fail(ERROR_INVALID_ADDRESS, 0) }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![CreateFileMappingW, CreateFileMappingA, MapViewOfFile, UnmapViewOfFile, FlushViewOfFile]
}
