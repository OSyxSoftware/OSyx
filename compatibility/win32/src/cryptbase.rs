//! `cryptbase.dll`: the system random generator.
//!
//! Windows' `advapi32!SystemFunction036` (better known as `RtlGenRandom`)
//! forwards here, and NSS/freebl loads `cryptbase.dll` first when it looks
//! for one (`nss/lib/freebl/win_rand.c`), falling back to `advapi32.dll`.
//! OSyx has a kernel CSPRNG behind `/dev/random`, so both names exist.
#![allow(non_snake_case)]
use alloc::vec::Vec;

/// `BOOLEAN SystemFunction036(PVOID buffer, ULONG length)`: fills `buffer`
/// with `length` random bytes.
///
/// Windows documents this as never failing. OSyx does not invent bytes when
/// the entropy device cannot be read, so it reports failure instead; a
/// caller that needs randomness must be able to see that it got none.
extern "win64" fn SystemFunction036(buffer: *mut u8, length: u32) -> u8 {
    if length == 0 {
        return 1;
    }
    if buffer.is_null() {
        return 0;
    }
    // SAFETY: the caller's buffer of `length` bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buffer, length as usize) };
    crate::ucrt::entropy(out) as u8
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![SystemFunction036]
}
