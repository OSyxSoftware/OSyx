//! `imagehlp.dll`: the attribute certificate table of a PE file (the
//! Authenticode signatures, data directory 4), read and edited through a
//! file handle, plus dbghelp's image functions that imagehlp also exports.
//!
//! Certificates are WIN_CERTIFICATE entries (dwLength, wRevision,
//! wCertificateType, data), each starting on an 8-byte boundary. Adding or
//! removing one rewrites the table, updates the directory entry and the
//! optional header's checksum, as Windows does.
#![allow(non_snake_case)]

use crate::{error::*, kernel32::fail};
use alloc::vec::Vec;

const CERT_SECTION_TYPE_ANY: u16 = 0xFF;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_BAD_EXE_FORMAT: u32 = 193;

fn read_all(h: u64) -> Result<Vec<u8>, u32> {
    let mut data = Vec::new();
    let mut buf = alloc::vec![0u8; 1 << 16];
    loop {
        let n = crate::kernel32::read_handle(h, buf.as_mut_ptr(), buf.len() as u32, Some(data.len() as u64))?;
        if n == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
}

fn write_all(h: u64, data: &[u8]) -> Result<(), u32> {
    let mut done = 0;
    while done < data.len() {
        let n = crate::kernel32::write_handle(h, data[done..].as_ptr(), (data.len() - done) as u32, Some(done as u64))?;
        if n == 0 {
            return Err(29); // ERROR_WRITE_FAULT
        }
        done += n as usize;
    }
    let f = crate::objects::file(h)?;
    os_sys::ftruncate(f.fd, data.len() as u64).map_err(crate::error::errno_to_win32)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// File offsets of the checksum and of data directory 4.
struct Layout {
    checksum: usize,
    security: usize,
}

fn layout(d: &[u8]) -> Result<Layout, u32> {
    let pe = u32_at(d, 0x3c).ok_or(ERROR_BAD_EXE_FORMAT)? as usize;
    if d.get(..2) != Some(b"MZ") || d.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(ERROR_BAD_EXE_FORMAT);
    }
    let opt = pe + 24;
    let dirs = match d.get(opt..opt + 2) {
        Some([0x0b, 0x01]) => opt + 96,
        Some([0x0b, 0x02]) => opt + 112,
        _ => return Err(ERROR_BAD_EXE_FORMAT),
    };
    let count = u32_at(d, dirs - 4).ok_or(ERROR_BAD_EXE_FORMAT)?;
    if count < 5 || d.len() < dirs + 5 * 8 {
        return Err(ERROR_BAD_EXE_FORMAT);
    }
    Ok(Layout { checksum: opt + 64, security: dirs + 4 * 8 })
}

/// The certificates: (offset, length) of each WIN_CERTIFICATE.
fn certificates(d: &[u8], l: &Layout) -> Result<Vec<(usize, usize)>, u32> {
    let (start, size) = (u32_at(d, l.security).unwrap_or(0) as usize, u32_at(d, l.security + 4).unwrap_or(0) as usize);
    let mut out = Vec::new();
    if start == 0 || size == 0 {
        return Ok(out);
    }
    let end = start.checked_add(size).filter(|&e| e <= d.len()).ok_or(ERROR_BAD_EXE_FORMAT)?;
    // The table follows the headers; one that overlaps them is damage.
    if start < l.security + 8 {
        return Err(ERROR_BAD_EXE_FORMAT);
    }
    let mut at = start;
    while at + 8 <= end {
        let n = u32_at(d, at).ok_or(ERROR_BAD_EXE_FORMAT)? as usize;
        if n < 8 || at + n > end {
            return Err(ERROR_BAD_EXE_FORMAT);
        }
        out.push((at, n));
        at += n.next_multiple_of(8);
    }
    Ok(out)
}

/// The PE checksum: 16-bit one's-complement sum of the file without the
/// checksum field, plus the file length.
fn checksum(d: &[u8], field: usize) -> u32 {
    let mut sum: u64 = 0;
    for (i, c) in d.chunks(2).enumerate() {
        let at = i * 2;
        if at == field || at == field + 2 {
            continue;
        }
        sum += u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]) as u64;
        sum = (sum & 0xffff) + (sum >> 16);
    }
    sum = (sum & 0xffff) + (sum >> 16);
    (sum as u32).wrapping_add(d.len() as u32)
}

/// Rewrites the table from `certs` (each a whole WIN_CERTIFICATE) at the
/// end of the file, where the table lives.
fn rewrite(h: u64, mut d: Vec<u8>, l: &Layout, existing: &[(usize, usize)], certs: &[Vec<u8>]) -> Result<(), u32> {
    // Rewriting replaces everything from the table on: refuse a file with
    // data after its table (beyond the 8-byte alignment padding).
    if let Some(&(at, n)) = existing.last() {
        let end = at.checked_add(n).and_then(|e| e.checked_next_multiple_of(8)).ok_or(ERROR_BAD_EXE_FORMAT)?;
        if end < d.len() {
            return Err(ERROR_BAD_EXE_FORMAT);
        }
    }
    let start = match existing.first() {
        Some(&(at, _)) => at,
        None => d.len().next_multiple_of(8),
    };
    d.truncate(start.min(d.len()));
    d.resize(start, 0);
    let mut table = Vec::new();
    for c in certs {
        table.extend_from_slice(c);
        table.resize(table.len().next_multiple_of(8), 0);
    }
    d.extend_from_slice(&table);
    let (off, size) = if table.is_empty() { (0, 0) } else { (start as u32, table.len() as u32) };
    d[l.security..l.security + 4].copy_from_slice(&off.to_le_bytes());
    d[l.security + 4..l.security + 8].copy_from_slice(&size.to_le_bytes());
    let sum = checksum(&d, l.checksum);
    d[l.checksum..l.checksum + 4].copy_from_slice(&sum.to_le_bytes());
    write_all(h, &d)
}

fn cert_bytes(d: &[u8], c: (usize, usize)) -> Vec<u8> {
    d[c.0..c.0 + c.1].to_vec()
}

extern "win64" fn ImageEnumerateCertificates(h: u64, filter: u16, count: *mut u32, indices: *mut u32, index_count: u32) -> i32 {
    let r = (|| {
        let d = read_all(h)?;
        let l = layout(&d)?;
        let certs = certificates(&d, &l)?;
        let mut found = Vec::new();
        for (i, &(at, _)) in certs.iter().enumerate() {
            let kind = u16::from_le_bytes([d[at + 6], d[at + 7]]);
            if filter == CERT_SECTION_TYPE_ANY || kind == filter {
                found.push(i as u32);
            }
        }
        Ok(found)
    })();
    match r {
        Ok(found) => {
            if count.is_null() {
                return fail(ERROR_INVALID_PARAMETER, 0);
            }
            // SAFETY: the caller's count and array of index_count entries.
            unsafe {
                *count = found.len() as u32;
                if !indices.is_null() {
                    for (k, i) in found.iter().take(index_count as usize).enumerate() {
                        *indices.add(k) = *i;
                    }
                }
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

fn nth(h: u64, index: u32) -> Result<Vec<u8>, u32> {
    let d = read_all(h)?;
    let l = layout(&d)?;
    let certs = certificates(&d, &l)?;
    let c = *certs.get(index as usize).ok_or(ERROR_INVALID_PARAMETER)?;
    Ok(cert_bytes(&d, c))
}

extern "win64" fn ImageGetCertificateHeader(h: u64, index: u32, header: *mut u8) -> i32 {
    match nth(h, index) {
        Ok(c) if !header.is_null() => {
            // SAFETY: the caller's WIN_CERTIFICATE header (8 bytes).
            unsafe { core::ptr::copy_nonoverlapping(c.as_ptr(), header, 8) };
            1
        }
        Ok(_) => fail(ERROR_INVALID_PARAMETER, 0),
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn ImageGetCertificateData(h: u64, index: u32, out: *mut u8, required: *mut u32) -> i32 {
    if required.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    match nth(h, index) {
        Ok(c) => {
            // SAFETY: the caller's size and buffer of *required bytes.
            unsafe {
                let cap = *required as usize;
                *required = c.len() as u32;
                if out.is_null() || cap < c.len() {
                    return fail(ERROR_INSUFFICIENT_BUFFER, 0);
                }
                core::ptr::copy_nonoverlapping(c.as_ptr(), out, c.len());
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn ImageAddCertificate(h: u64, cert: *const u8, index: *mut u32) -> i32 {
    if cert.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's WIN_CERTIFICATE of dwLength bytes.
    let new = unsafe {
        let n = *(cert as *const u32) as usize;
        if n < 8 {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        core::slice::from_raw_parts(cert, n).to_vec()
    };
    let r = (|| {
        let d = read_all(h)?;
        let l = layout(&d)?;
        let existing = certificates(&d, &l)?;
        let mut all: Vec<Vec<u8>> = existing.iter().map(|&c| cert_bytes(&d, c)).collect();
        all.push(new);
        let i = all.len() as u32 - 1;
        rewrite(h, d, &l, &existing, &all)?;
        Ok(i)
    })();
    match r {
        Ok(i) => {
            if !index.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *index = i };
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn ImageRemoveCertificate(h: u64, index: u32) -> i32 {
    let r = (|| {
        let d = read_all(h)?;
        let l = layout(&d)?;
        let existing = certificates(&d, &l)?;
        if index as usize >= existing.len() {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let all: Vec<Vec<u8>> = existing.iter().enumerate().filter(|(i, _)| *i != index as usize).map(|(_, &c)| cert_bytes(&d, c)).collect();
        rewrite(h, d, &l, &existing, &all)
    })();
    match r {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut e = crate::dbghelp::exports();
    e.extend(export_table![
        ImageEnumerateCertificates,
        ImageGetCertificateHeader,
        ImageGetCertificateData,
        ImageAddCertificate,
        ImageRemoveCertificate,
    ]);
    e
}
