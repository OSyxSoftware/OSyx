//! malloc and friends over the process heap (the same allocator as
//! HeapAlloc, so CRT and Win32 heap blocks are interchangeable as on
//! Windows), with the C++ new-handler protocol (`_set_new_mode`,
//! `_callnewh`).

use super::*;
use crate::kernel32::{heap_alloc, heap_free, heap_size};
use core::sync::atomic::AtomicI32;

/// `_HEAP_MAXREQ`.
const HEAP_MAXREQ: usize = 0xFFFF_FFFF_FFFF_FFE0;

static NEW_MODE: AtomicI32 = AtomicI32::new(0);
static NEW_HANDLER: AtomicU64 = AtomicU64::new(0);

/// `_callnewh`: 1 if a new handler exists and reports it freed memory.
fn call_new_handler(size: usize) -> bool {
    let h = NEW_HANDLER.load(Ordering::SeqCst);
    if h == 0 {
        return false;
    }
    // SAFETY: a `_PNH` installed by the program.
    let f: extern "win64" fn(usize) -> i32 = unsafe { core::mem::transmute(h as usize) };
    f(size) != 0
}

fn alloc(size: usize, zero: bool) -> *mut u8 {
    if size > HEAP_MAXREQ {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    }
    loop {
        if let Some(p) = heap_alloc(size, zero) {
            return p as *mut u8;
        }
        if NEW_MODE.load(Ordering::SeqCst) == 0 || !call_new_handler(size) {
            if crate::VERBOSE.load(Ordering::Relaxed) {
                os_std::eprintln!("winrun: the CRT heap has no room for {size:#x} bytes");
            }
            set_errno(ENOMEM);
            return core::ptr::null_mut();
        }
    }
}

/// `malloc` for the rest of the runtime.
pub fn crt_malloc(size: usize) -> *mut u8 {
    alloc(size, false)
}

pub fn crt_free(p: *mut u8) {
    if !p.is_null() {
        heap_free(p as u64);
    }
}

extern "win64" fn malloc(size: usize) -> *mut u8 {
    alloc(size, false)
}

extern "win64" fn _malloc_base(size: usize) -> *mut u8 {
    alloc(size, false)
}

extern "win64" fn calloc(count: usize, size: usize) -> *mut u8 {
    if count != 0 && HEAP_MAXREQ / count < size {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    }
    alloc(count * size, true)
}

extern "win64" fn _calloc_base(count: usize, size: usize) -> *mut u8 {
    calloc(count, size)
}

extern "win64" fn free(p: *mut u8) {
    crt_free(p)
}

extern "win64" fn _free_base(p: *mut u8) {
    crt_free(p)
}

fn resize(p: *mut u8, size: usize, zero_new: bool) -> *mut u8 {
    if p.is_null() {
        return alloc(size, zero_new);
    }
    if size == 0 {
        crt_free(p);
        return core::ptr::null_mut();
    }
    let old = heap_size(p as u64);
    let n = alloc(size, false);
    if n.is_null() {
        return n; // the old block is untouched
    }
    // SAFETY: both blocks hold at least min(old, size) bytes; the tail of
    // a grown block is zeroed when asked.
    unsafe {
        core::ptr::copy_nonoverlapping(p, n, old.min(size));
        if zero_new && size > old {
            core::ptr::write_bytes(n.add(old), 0, size - old);
        }
    }
    crt_free(p);
    n
}

extern "win64" fn realloc(p: *mut u8, size: usize) -> *mut u8 {
    resize(p, size, false)
}

extern "win64" fn _realloc_base(p: *mut u8, size: usize) -> *mut u8 {
    resize(p, size, false)
}

extern "win64" fn _recalloc(p: *mut u8, count: usize, size: usize) -> *mut u8 {
    if count != 0 && HEAP_MAXREQ / count < size {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    }
    resize(p, count * size, true)
}

extern "win64" fn _msize(p: *mut u8) -> usize {
    validate!(!p.is_null(), EINVAL, usize::MAX);
    heap_size(p as u64)
}

// Aligned blocks: [padding][original block pointer][user data], as the
// UCRT lays them out, so `_aligned_free` finds the underlying block.
const PTR: usize = core::mem::size_of::<usize>();

fn aligned_offset_malloc(size: usize, align: usize, offset: usize) -> *mut u8 {
    validate!(align.is_power_of_two(), EINVAL, core::ptr::null_mut());
    validate!(offset == 0 || offset < size, EINVAL, core::ptr::null_mut());
    let align = align.max(PTR);
    let gap = (PTR - offset % PTR) % PTR;
    let Some(total) = size.checked_add(align).and_then(|t| t.checked_add(gap + PTR)) else {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    };
    let raw = alloc(total, false);
    if raw.is_null() {
        return raw;
    }
    let start = raw as usize + gap + PTR + offset;
    let user = ((start + align - 1) & !(align - 1)) - offset;
    // SAFETY: `user - PTR` lies inside the block, after `raw`.
    unsafe { *((user - PTR) as *mut usize) = raw as usize };
    user as *mut u8
}

fn aligned_raw(p: *mut u8) -> *mut u8 {
    // SAFETY: `p` came from `aligned_offset_malloc` (UCRT contract).
    unsafe { *((p as usize - PTR) as *const usize) as *mut u8 }
}

extern "win64" fn _aligned_malloc(size: usize, align: usize) -> *mut u8 {
    aligned_offset_malloc(size, align, 0)
}

extern "win64" fn _aligned_offset_malloc(size: usize, align: usize, offset: usize) -> *mut u8 {
    aligned_offset_malloc(size, align, offset)
}

extern "win64" fn _aligned_free(p: *mut u8) {
    if !p.is_null() {
        crt_free(aligned_raw(p));
    }
}

/// Bytes usable from `p` to the end of its block (`offset` does not move
/// the block's end).
extern "win64" fn _aligned_msize(p: *mut u8, align: usize, _offset: usize) -> usize {
    validate!(!p.is_null() && align.is_power_of_two(), EINVAL, usize::MAX);
    let raw = aligned_raw(p);
    heap_size(raw as u64) - (p as usize - raw as usize)
}

extern "win64" fn _aligned_realloc(p: *mut u8, size: usize, align: usize) -> *mut u8 {
    if p.is_null() {
        return aligned_offset_malloc(size, align, 0);
    }
    if size == 0 {
        _aligned_free(p);
        return core::ptr::null_mut();
    }
    let old = _aligned_msize(p, align, 0);
    let n = aligned_offset_malloc(size, align, 0);
    if !n.is_null() {
        // SAFETY: both blocks are valid for min(old, size) bytes.
        unsafe { core::ptr::copy_nonoverlapping(p, n, old.min(size)) };
        _aligned_free(p);
    }
    n
}

extern "win64" fn _set_new_mode(mode: i32) -> i32 {
    validate!(mode == 0 || mode == 1, EINVAL, -1);
    NEW_MODE.swap(mode, Ordering::SeqCst)
}

extern "win64" fn _query_new_mode() -> i32 {
    NEW_MODE.load(Ordering::SeqCst)
}

extern "win64" fn _callnewh(size: usize) -> i32 {
    call_new_handler(size) as i32
}

/// `_set_new_handler` (runtime contract): returns the previous handler.
pub extern "win64" fn _set_new_handler(h: u64) -> u64 {
    NEW_HANDLER.swap(h, Ordering::SeqCst)
}

extern "win64" fn _query_new_handler() -> u64 {
    NEW_HANDLER.load(Ordering::SeqCst)
}

extern "win64" fn _get_heap_handle() -> u64 {
    crate::kernel32::process_heap()
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        malloc,
        _malloc_base,
        calloc,
        _calloc_base,
        free,
        _free_base,
        realloc,
        _realloc_base,
        _recalloc,
        _msize,
        _aligned_malloc,
        _aligned_offset_malloc,
        _aligned_free,
        _aligned_msize,
        _aligned_realloc,
        _set_new_mode,
        _query_new_mode,
        _callnewh,
        _set_new_handler,
        _query_new_handler,
        _get_heap_handle,
    ]
}
