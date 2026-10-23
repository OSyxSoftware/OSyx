//! Utilities (utility contract): random numbers (`rand_s` from the OSyx
//! entropy device, `/dev/random`; it fails when the device is absent),
//! integer helpers, sorting and searching.

use super::*;

/// Reads `buf.len()` bytes from the kernel's random device.
fn entropy(buf: &mut [u8]) -> bool {
    let Ok(fd) = os_sys::open("/dev/random", osyx_abi::OPEN_READ, 0) else { return false };
    let mut done = 0;
    while done < buf.len() {
        match os_sys::read_raw(fd, buf[done..].as_mut_ptr() as usize, buf.len() - done) {
            Ok(0) | Err(_) => break,
            Ok(n) => done += n,
        }
    }
    let _ = os_sys::close(fd);
    done == buf.len()
}

extern "win64" fn rand_s(out: *mut u32) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    // SAFETY: validated.
    unsafe { *out = 0 };
    let mut b = [0u8; 4];
    if !entropy(&mut b) {
        set_errno(ENOMEM);
        return ENOMEM;
    }
    // SAFETY: validated.
    unsafe { *out = u32::from_le_bytes(b) };
    0
}

/// The UCRT's per-thread linear congruential generator.
extern "win64" fn rand() -> i32 {
    let p = ptd();
    p.rand_next = p.rand_next.wrapping_mul(214_013).wrapping_add(2_531_011);
    ((p.rand_next >> 16) & 0x7FFF) as i32
}

extern "win64" fn srand(seed: u32) {
    ptd().rand_next = seed;
}

extern "win64" fn abs(x: i32) -> i32 {
    x.wrapping_abs()
}

extern "win64" fn llabs(x: i64) -> i64 {
    x.wrapping_abs()
}

#[repr(C)]
struct DivT {
    quot: i32,
    rem: i32,
}

#[repr(C)]
struct LldivT {
    quot: i64,
    rem: i64,
}

extern "win64" fn div(a: i32, b: i32) -> DivT {
    DivT { quot: a.wrapping_div(b), rem: a.wrapping_rem(b) }
}

extern "win64" fn lldiv(a: i64, b: i64) -> LldivT {
    LldivT { quot: a.wrapping_div(b), rem: a.wrapping_rem(b) }
}

type Compare = extern "win64" fn(*const u8, *const u8) -> i32;

fn swap(base: *mut u8, width: usize, i: usize, j: usize) {
    if i != j {
        // SAFETY: both elements lie in the caller's array.
        unsafe { core::ptr::swap_nonoverlapping(base.add(i * width), base.add(j * width), width) };
    }
}

/// qsort: an in-place heap sort (no allocation, O(n log n)).
extern "win64" fn qsort(base: *mut u8, n: usize, width: usize, cmp: u64) {
    if (base.is_null() && n != 0) || width == 0 || cmp == 0 {
        set_errno(EINVAL);
        invalid_parameter();
        return;
    }
    if n < 2 {
        return;
    }
    // SAFETY: the program's comparison function.
    let cmp: Compare = unsafe { core::mem::transmute(cmp as usize) };
    // SAFETY: element addresses within the array.
    let at = |i: usize| unsafe { base.add(i * width) as *const u8 };
    let sift = |mut root: usize, end: usize| loop {
        let mut child = 2 * root + 1;
        if child >= end {
            break;
        }
        if child + 1 < end && cmp(at(child), at(child + 1)) < 0 {
            child += 1;
        }
        if cmp(at(root), at(child)) >= 0 {
            break;
        }
        swap(base, width, root, child);
        root = child;
    };
    for start in (0..n / 2).rev() {
        sift(start, n);
    }
    for end in (1..n).rev() {
        swap(base, width, 0, end);
        sift(0, end);
    }
}

extern "win64" fn bsearch(key: *const u8, base: *const u8, n: usize, width: usize, cmp: u64) -> *const u8 {
    validate!(!base.is_null() || n == 0, EINVAL, core::ptr::null());
    validate!(width > 0 && cmp != 0, EINVAL, core::ptr::null());
    // SAFETY: the program's comparison function.
    let cmp: Compare = unsafe { core::mem::transmute(cmp as usize) };
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        // SAFETY: mid < n.
        let p = unsafe { base.add(mid * width) };
        match cmp(key, p) {
            0 => return p,
            r if r < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    core::ptr::null()
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![rand_s, rand, srand, abs, llabs, div, lldiv, qsort, bsearch];
    v.extend([
        ("labs", abs as *const () as u64),
        ("_abs64", llabs as *const () as u64),
        ("ldiv", div as *const () as u64),
    ]);
    v
}
