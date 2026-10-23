//! ole32's task allocator (CoTaskMemAlloc/Realloc/Free), over the process
//! heap. Blocks shell32 hands out (SHGetKnownFolderPath) come from here.
//! COM itself (apartments, class activation) is not provided.
#![allow(non_snake_case)]
use crate::kernel32;
use alloc::vec::Vec;

pub extern "win64" fn CoTaskMemAlloc(size: usize) -> u64 {
    kernel32::heap_alloc(size, false).unwrap_or(0)
}

extern "win64" fn CoTaskMemFree(p: u64) {
    kernel32::heap_free(p);
}

extern "win64" fn CoTaskMemRealloc(p: u64, size: usize) -> u64 {
    if p == 0 {
        return CoTaskMemAlloc(size);
    }
    if size == 0 {
        kernel32::heap_free(p);
        return 0;
    }
    let Some(n) = kernel32::heap_alloc(size, false) else { return 0 };
    // SAFETY: both blocks are live heap blocks; the copy fits the smaller.
    unsafe { core::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, kernel32::heap_size(p).min(size)) };
    kernel32::heap_free(p);
    n
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![CoTaskMemAlloc, CoTaskMemFree, CoTaskMemRealloc]
}
