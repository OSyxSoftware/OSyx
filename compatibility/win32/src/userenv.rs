//! userenv: AppContainer SIDs. The SID of an AppContainer name is derived
//! exactly as on Windows: S-1-15-2 followed by the first seven DWORDs of
//! the SHA-256 of the lower-cased UTF-16LE name. Creating and running
//! AppContainers is not supported on OSyx (tokens.rs); the SIDs are
//! still the ones such a container would have.
#![allow(non_snake_case)]
use crate::{kernel32, path, security::sid};
use alloc::vec::Vec;

const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
/// AppContainer names are 1 to 64 characters.
const MAX_NAME: usize = 64;

/// The AppContainer SID of `name`.
pub fn appcontainer_sid(name: &str) -> Vec<u8> {
    let lower: Vec<u8> = name.chars().flat_map(|c| c.to_lowercase()).collect::<alloc::string::String>().encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let hash = osyx_crypto::Sha256::digest(&lower);
    let mut sub = alloc::vec![2u32];
    sub.extend((0..7).map(|i| u32::from_le_bytes([hash[i * 4], hash[i * 4 + 1], hash[i * 4 + 2], hash[i * 4 + 3]])));
    sid(15, &sub)
}

extern "win64" fn DeriveAppContainerSidFromAppContainerName(name: *const u16, out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: NULL or a NUL-terminated name.
    let Some(name) = (unsafe { path::wide_to_string(name) }).filter(|n| !n.is_empty() && n.encode_utf16().count() <= MAX_NAME) else {
        return E_INVALIDARG;
    };
    let s = appcontainer_sid(&name);
    let Some(p) = kernel32::heap_alloc(s.len(), false) else { return E_OUTOFMEMORY };
    // SAFETY: a fresh block of s.len() bytes (freed with FreeSid); the caller's out pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p as *mut u8, s.len());
        *out = p;
    }
    0
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![DeriveAppContainerSidFromAppContainerName]
}
