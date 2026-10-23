//! Multibyte cursor movement and restartable string conversion for the
//! supported C and C.UTF-8 locales. Noninitial opaque mbstate objects are
//! rejected: this runtime does not yet export mbrtowc to construct them.
use super::*;

/// Decode one complete character without reading past its NUL terminator.
fn decode(p: *const u8) -> Option<(u32, usize)> {
    let mut bytes = [0u8; 4];
    // SAFETY: the caller supplies a NUL-terminated multibyte string.
    bytes[0] = unsafe { *p };
    let n = if !locale::ctype_utf8() || bytes[0] < 128 {
        1
    } else {
        match bytes[0] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return None,
        }
    };
    for (i, b) in bytes.iter_mut().enumerate().take(n).skip(1) {
        // SAFETY: stop at NUL before advancing again.
        *b = unsafe { *p.add(i) };
        if *b == 0 {
            return None;
        }
    }
    locale::mb_char(&bytes[..n])
}

extern "win64" fn _mbsinc(p: *const u8) -> *const u8 {
    validate!(!p.is_null(), EINVAL, core::ptr::null());
    let n = decode(p).map_or(1, |(_, n)| n.max(1));
    // SAFETY: decoded one character, or one byte on an invalid byte.
    unsafe { p.add(n) }
}

extern "win64" fn _mbsdec(start: *const u8, current: *const u8) -> *const u8 {
    validate!(!start.is_null() && !current.is_null(), EINVAL, core::ptr::null());
    if start >= current {
        return core::ptr::null();
    }
    let mut p = start;
    while p < current {
        // SAFETY: the caller supplies current within the same string.
        if unsafe { *p } == 0 {
            return core::ptr::null();
        }
        let next = _mbsinc(p);
        if next >= current {
            return p;
        }
        p = next;
    }
    core::ptr::null()
}

extern "win64" fn mbsrtowcs(out: *mut u16, source: *mut *const u8, cap: usize, state: *mut u64) -> usize {
    validate!(!source.is_null(), EINVAL, usize::MAX);
    // SAFETY: source is a validated pointer to the caller's cursor.
    let mut p = unsafe { *source };
    validate!(!p.is_null(), EINVAL, usize::MAX);
    // mbstate_t is eight bytes in the Windows UCRT. Only the initial
    // state can arise within the supported stateless locales here.
    if !state.is_null() && unsafe { state.read_unaligned() } != 0 {
        set_errno(EILSEQ);
        return usize::MAX;
    }
    let mut count = 0usize;
    while out.is_null() || count < cap {
        let Some((c, bytes)) = decode(p) else {
            if !out.is_null() {
                // SAFETY: source is the caller's writable cursor.
                unsafe { *source = p };
            }
            set_errno(EILSEQ);
            return usize::MAX;
        };
        if c == 0 {
            if !out.is_null() {
                // SAFETY: count < cap, and source was validated.
                unsafe {
                    *out.add(count) = 0;
                    *source = core::ptr::null();
                }
            }
            return count;
        }
        let Some(ch) = char::from_u32(c) else {
            set_errno(EILSEQ);
            return usize::MAX;
        };
        let mut units = [0u16; 2];
        let units = ch.encode_utf16(&mut units);
        if !out.is_null() {
            if units.len() > cap - count {
                break;
            }
            // SAFETY: count + units.len() <= cap.
            unsafe {
                core::ptr::copy_nonoverlapping(units.as_ptr(), out.add(count), units.len());
            }
        }
        count += units.len();
        // SAFETY: bytes covers the complete decoded non-NUL character.
        p = unsafe { p.add(bytes) };
    }
    // SAFETY: source is a validated caller cursor; size-query mode has
    // already returned and does not change it.
    unsafe { *source = p };
    count
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![_mbsinc, _mbsdec, mbsrtowcs]
}
