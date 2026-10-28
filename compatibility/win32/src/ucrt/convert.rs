//! Number conversion (convert contract): strtol/strtod and their wide and
//! 64-bit forms with the C grammar, overflow (ERANGE) and base rules;
//! decimal floating point is rounded correctly (core's decimal parser),
//! hexadecimal floating point exactly here; and the `_ltoa_s` family.

use super::*;
use alloc::string::String;

/// A code unit of a narrow or wide string, read as a code point.
trait Unit: Copy {
    fn cp(self) -> u32;
}
impl Unit for u8 {
    fn cp(self) -> u32 {
        self as u32
    }
}
impl Unit for u16 {
    fn cp(self) -> u32 {
        self as u32
    }
}

fn at<T: Unit>(s: *const T, i: usize) -> u32 {
    // SAFETY: callers only read up to and including the terminator.
    unsafe { (*s.add(i)).cp() }
}

fn is_space(c: u32) -> bool {
    c == b' ' as u32 || (9..=13).contains(&c)
}

fn digit(c: u32) -> Option<u32> {
    match c {
        0x30..=0x39 => Some(c - 0x30),
        0x41..=0x5A => Some(c - 0x41 + 10),
        0x61..=0x7A => Some(c - 0x61 + 10),
        _ => None,
    }
}

fn set_end<T>(end: *mut *const T, p: *const T) {
    if !end.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *end = p };
    }
}

/// The strtol family. `max` is the type's largest magnitude for positive
/// results; signed types allow max + 1 when negative.
fn parse_int<T: Unit>(s: *const T, end: *mut *const T, base: i32, signed: bool, max: u64) -> u64 {
    set_end(end, s);
    validate!(!s.is_null(), EINVAL, 0);
    validate!(base == 0 || (2..=36).contains(&base), EINVAL, 0);
    let mut i = 0;
    while is_space(at(s, i)) {
        i += 1;
    }
    let neg = at(s, i) == b'-' as u32;
    if neg || at(s, i) == b'+' as u32 {
        i += 1;
    }
    let mut base = base as u32;
    let hex_prefix = at(s, i) == b'0' as u32 && (at(s, i + 1) | 0x20) == b'x' as u32;
    if (base == 0 || base == 16) && hex_prefix {
        // The UCRT converts nothing from a prefix without hex digits ("0x").
        if !digit(at(s, i + 2)).is_some_and(|d| d < 16) {
            return 0;
        }
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if at(s, i) == b'0' as u32 { 8 } else { 10 };
    }
    let limit = if signed && neg { max + 1 } else { max };
    let (mut v, mut any, mut over) = (0u64, false, false);
    while let Some(d) = digit(at(s, i)).filter(|&d| d < base) {
        any = true;
        match v.checked_mul(base as u64).and_then(|x| x.checked_add(d as u64)) {
            Some(x) if x <= limit => v = x,
            _ => over = true,
        }
        i += 1;
    }
    if !any {
        return 0;
    }
    // SAFETY: `i` is within the string.
    set_end(end, unsafe { s.add(i) });
    if over {
        set_errno(ERANGE);
        return if !signed {
            max
        } else if neg {
            (max + 1).wrapping_neg()
        } else {
            max
        };
    }
    if neg { v.wrapping_neg() } else { v }
}

extern "win64" fn strtol(s: *const u8, e: *mut *const u8, b: i32) -> i32 {
    parse_int(s, e, b, true, i32::MAX as u64) as i32
}
extern "win64" fn strtoul(s: *const u8, e: *mut *const u8, b: i32) -> u32 {
    parse_int(s, e, b, false, u32::MAX as u64) as u32
}
extern "win64" fn strtoll(s: *const u8, e: *mut *const u8, b: i32) -> i64 {
    parse_int(s, e, b, true, i64::MAX as u64) as i64
}
extern "win64" fn strtoull(s: *const u8, e: *mut *const u8, b: i32) -> u64 {
    parse_int(s, e, b, false, u64::MAX)
}
extern "win64" fn wcstol(s: *const u16, e: *mut *const u16, b: i32) -> i32 {
    parse_int(s, e, b, true, i32::MAX as u64) as i32
}
extern "win64" fn wcstoul(s: *const u16, e: *mut *const u16, b: i32) -> u32 {
    parse_int(s, e, b, false, u32::MAX as u64) as u32
}
extern "win64" fn wcstoll(s: *const u16, e: *mut *const u16, b: i32) -> i64 {
    parse_int(s, e, b, true, i64::MAX as u64) as i64
}
extern "win64" fn wcstoull(s: *const u16, e: *mut *const u16, b: i32) -> u64 {
    parse_int(s, e, b, false, u64::MAX)
}
extern "win64" fn atoi(s: *const u8) -> i32 {
    strtol(s, core::ptr::null_mut(), 10)
}
extern "win64" fn atol(s: *const u8) -> i32 {
    strtol(s, core::ptr::null_mut(), 10)
}
extern "win64" fn atoll(s: *const u8) -> i64 {
    strtoll(s, core::ptr::null_mut(), 10)
}
extern "win64" fn _wtoi(s: *const u16) -> i32 {
    wcstol(s, core::ptr::null_mut(), 10)
}
extern "win64" fn _wtol(s: *const u16) -> i32 {
    wcstol(s, core::ptr::null_mut(), 10)
}
extern "win64" fn _wtoll(s: *const u16) -> i64 {
    wcstoll(s, core::ptr::null_mut(), 10)
}

// ---- floating point -------------------------------------------------------------------

/// Builds the nearest float of `m * 2^e2` (`sticky`: nonzero bits below
/// m), rounding to nearest even into the precision and exponent range of
/// f64 or f32 (subnormals included). Returns the value as f64 (exact for
/// f32 results too).
fn compose(m: u64, sticky: bool, e2: i64, f32: bool) -> f64 {
    let (mant, emin, emax) = if f32 { (24i64, -126i64, 127i64) } else { (53, -1022, 1023) };
    if m == 0 {
        return 0.0;
    }
    let bits = 64 - m.leading_zeros() as i64;
    let top = e2 + bits - 1; // exponent of the leading bit
    if top > emax + 1 {
        return f64::INFINITY;
    }
    // Significand bits available at this magnitude (fewer when subnormal).
    let keep = if top < emin { mant - (emin - top) } else { mant };
    let shift = bits - keep;
    let (q, e) = if shift <= 0 {
        (m << -shift, e2 + shift)
    } else if shift > 65 {
        return 0.0; // below half the smallest subnormal
    } else {
        let mm = m as u128;
        let q = mm >> shift;
        let rem = mm & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        let up = rem > half || (rem == half && (sticky || q & 1 == 1));
        ((q + up as u128) as u64, e2 + shift)
    };
    if q == 0 {
        return 0.0;
    }
    if e + (64 - q.leading_zeros() as i64) - 1 > emax {
        return f64::INFINITY;
    }
    // q * 2^e is exact in f64 now; scale in steps clear of the limits.
    let mut v = q as f64;
    let mut e = e;
    while e != 0 {
        let s = e.clamp(-1000, 1000);
        v *= pow2(s as i32);
        e -= s;
    }
    v
}
/// 2^k for -1022 <= k <= 1023 (and k in [-1074, -1023] as subnormals).
fn pow2(k: i32) -> f64 {
    if k >= -1022 {
        f64::from_bits(((k + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (k + 1074))
    }
}

struct Parsed {
    value: f64,
    /// Units consumed (0: no conversion).
    len: usize,
    range: bool,
}

/// Convert the complete, NUL-terminated token collected by scanf. The
/// scanner handles the input-item grammar and never reads past its width.
pub(super) fn scan_float(token: &[u8], single: bool) -> Option<f64> {
    let p = parse_float(token.as_ptr(), single);
    if p.len == 0 || p.len + 1 != token.len() {
        return None;
    }
    if p.range {
        set_errno(ERANGE);
    }
    Some(p.value)
}

/// Parses the C floating-point grammar at `s` (after white space).
fn parse_float<T: Unit>(s: *const T, f32: bool) -> Parsed {
    let none = Parsed { value: 0.0, len: 0, range: false };
    let mut i = 0;
    while is_space(at(s, i)) {
        i += 1;
    }
    let neg = at(s, i) == b'-' as u32;
    if neg || at(s, i) == b'+' as u32 {
        i += 1;
    }
    let lc = |k: usize| at(s, k) | 0x20;
    let word = |k: usize, w: &str| w.bytes().enumerate().all(|(j, b)| lc(k + j) == b as u32);
    let signed = |v: f64| if neg { -v } else { v };
    // inf / infinity / nan / nan(chars)
    if word(i, "inf") {
        let n = if word(i, "infinity") { 8 } else { 3 };
        return Parsed { value: signed(f64::INFINITY), len: i + n, range: false };
    }
    if word(i, "nan") {
        let mut n = i + 3;
        if at(s, n) == b'(' as u32 {
            let mut k = n + 1;
            while digit(at(s, k)).is_some() || at(s, k) == b'_' as u32 {
                k += 1;
            }
            if at(s, k) == b')' as u32 {
                n = k + 1;
            }
        }
        return Parsed { value: signed(f64::NAN), len: n, range: false };
    }
    // Hexadecimal.
    if at(s, i) == b'0' as u32 && lc(i + 1) == b'x' as u32 {
        let mut k = i + 2;
        let (mut m, mut sticky, mut e2, mut any) = (0u64, false, 0i64, false);
        let mut frac = false;
        loop {
            let c = at(s, k);
            if c == b'.' as u32 && !frac {
                frac = true;
            } else if let Some(d) = digit(c).filter(|&d| d < 16) {
                any = true;
                if m >> 60 == 0 {
                    m = m << 4 | d as u64;
                    if frac {
                        e2 -= 4;
                    }
                } else {
                    sticky |= d != 0;
                    if !frac {
                        e2 += 4;
                    }
                }
            } else {
                break;
            }
            k += 1;
        }
        if !any {
            // Just the "0".
            return Parsed { value: signed(0.0), len: i + 1, range: false };
        }
        if lc(k) == b'p' as u32 {
            let mut j = k + 1;
            let eneg = at(s, j) == b'-' as u32;
            if eneg || at(s, j) == b'+' as u32 {
                j += 1;
            }
            if digit(at(s, j)).is_some_and(|d| d < 10) {
                let mut x: i64 = 0;
                while let Some(d) = digit(at(s, j)).filter(|&d| d < 10) {
                    x = (x * 10 + d as i64).min(1 << 40);
                    j += 1;
                }
                e2 += if eneg { -x } else { x };
                k = j;
            }
        }
        let v = compose(m, sticky, e2, f32);
        let range = m != 0 && (v == 0.0 || v.is_infinite());
        return Parsed { value: signed(v), len: k, range };
    }
    // Decimal.
    let mut digits = String::new();
    let mut k = i;
    let (mut any, mut frac, mut scale) = (false, false, 0i64);
    loop {
        let c = at(s, k);
        if c == b'.' as u32 && !frac {
            frac = true;
        } else if (0x30..=0x39).contains(&c) {
            any = true;
            if !(digits.is_empty() && c == 0x30) {
                digits.push(c as u8 as char);
            }
            if frac {
                scale -= 1;
            }
        } else {
            break;
        }
        k += 1;
    }
    if !any {
        return none;
    }
    if lc(k) == b'e' as u32 {
        let mut j = k + 1;
        let eneg = at(s, j) == b'-' as u32;
        if eneg || at(s, j) == b'+' as u32 {
            j += 1;
        }
        if (0x30..=0x39).contains(&at(s, j)) {
            let mut x: i64 = 0;
            while (0x30..=0x39).contains(&at(s, j)) {
                x = (x * 10 + (at(s, j) - 0x30) as i64).min(1 << 40);
                j += 1;
            }
            scale += if eneg { -x } else { x };
            k = j;
        }
    }
    while digits.ends_with('0') {
        digits.pop();
        scale += 1;
    }
    if digits.is_empty() {
        return Parsed { value: signed(0.0), len: k, range: false };
    }
    // Magnitude is about 10^(len + scale): settle far-out values directly.
    let mag = digits.len() as i64 + scale;
    let (hi, lo) = if f32 { (40, -48) } else { (310, -326) };
    let v = if mag > hi {
        f64::INFINITY
    } else if mag < lo {
        0.0
    } else {
        let text = alloc::format!("{digits}e{scale}");
        if f32 {
            text.parse::<f32>().map(|x| x as f64).unwrap_or(f64::NAN)
        } else {
            text.parse::<f64>().unwrap_or(f64::NAN)
        }
    };
    Parsed { value: signed(v), len: k, range: v == 0.0 || v.is_infinite() }
}

// TEMP: how often libxul/ucrt clients parse floats.
static STRTO_CALLS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STRTO_WIDE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn strto_float<T: Unit>(s: *const T, end: *mut *const T, f32: bool) -> f64 {
    let c = STRTO_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if c.is_multiple_of(20_000) {
        os_std::eprintln!(
            "winrun: stcount n={c} f32={f32} wide={} tid={}",
            STRTO_WIDE.load(core::sync::atomic::Ordering::Relaxed),
            os_sys::gettid()
        );
    }
    set_end(end, s);
    validate!(!s.is_null(), EINVAL, 0.0);
    let p = parse_float(s, f32);
    if p.len == 0 {
        return 0.0;
    }
    // SAFETY: within the string.
    set_end(end, unsafe { s.add(p.len) });
    if p.range {
        set_errno(ERANGE);
    }
    p.value
}

extern "win64" fn strtod_soft(s: *const u8, e: *mut *const u8) -> f64 {
    strto_float(s, e, false)
}
extern "win64" fn strtof_soft(s: *const u8, e: *mut *const u8) -> f32 {
    strto_float(s, e, true) as f32
}
extern "win64" fn strtold_soft(s: *const u8, e: *mut *const u8) -> f64 {
    strto_float(s, e, false)
}
extern "win64" fn wcstod_soft(s: *const u16, e: *mut *const u16) -> f64 {
    STRTO_WIDE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    strto_float(s, e, false)
}
extern "win64" fn wcstof_soft(s: *const u16, e: *mut *const u16) -> f32 {
    STRTO_WIDE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    strto_float(s, e, true) as f32
}
extern "win64" fn wcstold_soft(s: *const u16, e: *mut *const u16) -> f64 {
    STRTO_WIDE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    strto_float(s, e, false)
}
extern "win64" fn atof_soft(s: *const u8) -> f64 {
    strto_float(s, core::ptr::null_mut(), false)
}
extern "win64" fn _wtof_soft(s: *const u16) -> f64 {
    strto_float(s, core::ptr::null_mut(), false)
}

// ---- integer to string ------------------------------------------------------------------

/// Digits of `v` (negative only in radix 10, as the UCRT does; other
/// radixes see the two's complement value of the `bits`-wide type).
fn int_text(v: i64, bits: u32, radix: u32, signed: bool) -> Vec<u8> {
    let neg = signed && radix == 10 && v < 0;
    let mut u = if neg { (v as i128).unsigned_abs() as u64 } else { v as u64 };
    if bits < 64 {
        u &= (1u64 << bits) - 1;
    }
    let mut out = Vec::new();
    loop {
        let d = (u % radix as u64) as u8;
        out.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
        u /= radix as u64;
        if u == 0 {
            break;
        }
    }
    if neg {
        out.push(b'-');
    }
    out.reverse();
    out
}

/// The `_xtoa_s` contract.
fn to_text_s<T: From<u8> + Copy>(v: i64, bits: u32, signed: bool, buf: *mut T, size: usize, radix: i32) -> i32 {
    validate!(!buf.is_null() && size > 0, EINVAL, EINVAL);
    // SAFETY: validated non-empty buffer.
    unsafe { *buf = T::from(0) };
    validate!((2..=36).contains(&radix), EINVAL, EINVAL);
    let t = int_text(v, bits, radix as u32, signed);
    validate!(t.len() < size, ERANGE, ERANGE);
    for (i, &c) in t.iter().enumerate() {
        // SAFETY: t.len() + 1 <= size.
        unsafe { *buf.add(i) = T::from(c) };
    }
    // SAFETY: as above.
    unsafe { *buf.add(t.len()) = T::from(0) };
    0
}

/// The unchecked `_xtoa` forms (the caller guarantees room: 33/65 units).
fn to_text<T: From<u8> + Copy>(v: i64, bits: u32, signed: bool, buf: *mut T, radix: i32) -> *mut T {
    to_text_s(v, bits, signed, buf, 66, radix);
    buf
}

extern "win64" fn _itoa_s(v: i32, b: *mut u8, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, true, b, n, r)
}
extern "win64" fn _ltoa_s(v: i32, b: *mut u8, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, true, b, n, r)
}
extern "win64" fn _ultoa_s(v: u32, b: *mut u8, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, false, b, n, r)
}
extern "win64" fn _i64toa_s(v: i64, b: *mut u8, n: usize, r: i32) -> i32 {
    to_text_s(v, 64, true, b, n, r)
}
extern "win64" fn _ui64toa_s(v: u64, b: *mut u8, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 64, false, b, n, r)
}
extern "win64" fn _itow_s(v: i32, b: *mut u16, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, true, b, n, r)
}
extern "win64" fn _ltow_s(v: i32, b: *mut u16, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, true, b, n, r)
}
extern "win64" fn _ultow_s(v: u32, b: *mut u16, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 32, false, b, n, r)
}
extern "win64" fn _i64tow_s(v: i64, b: *mut u16, n: usize, r: i32) -> i32 {
    to_text_s(v, 64, true, b, n, r)
}
extern "win64" fn _ui64tow_s(v: u64, b: *mut u16, n: usize, r: i32) -> i32 {
    to_text_s(v as i64, 64, false, b, n, r)
}
extern "win64" fn _itoa(v: i32, b: *mut u8, r: i32) -> *mut u8 {
    to_text(v as i64, 32, true, b, r)
}
extern "win64" fn _ltoa(v: i32, b: *mut u8, r: i32) -> *mut u8 {
    to_text(v as i64, 32, true, b, r)
}
extern "win64" fn _ultoa(v: u32, b: *mut u8, r: i32) -> *mut u8 {
    to_text(v as i64, 32, false, b, r)
}
extern "win64" fn _i64toa(v: i64, b: *mut u8, r: i32) -> *mut u8 {
    to_text(v, 64, true, b, r)
}
extern "win64" fn _ui64toa(v: u64, b: *mut u8, r: i32) -> *mut u8 {
    to_text(v as i64, 64, false, b, r)
}
extern "win64" fn _itow(v: i32, b: *mut u16, r: i32) -> *mut u16 {
    to_text(v as i64, 32, true, b, r)
}
extern "win64" fn _ltow(v: i32, b: *mut u16, r: i32) -> *mut u16 {
    to_text(v as i64, 32, true, b, r)
}
extern "win64" fn _i64tow(v: i64, b: *mut u16, r: i32) -> *mut u16 {
    to_text(v, 64, true, b, r)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![
        strtol, strtoul, strtoll, strtoull, wcstol, wcstoul, wcstoll, wcstoull, atoi, atol, atoll, _wtoi, _wtol,
        _wtoll, strtod, strtof, strtold, wcstod, wcstof, wcstold, atof, _wtof, _itoa_s, _ltoa_s, _ultoa_s, _i64toa_s,
        _ui64toa_s, _itow_s, _ltow_s, _ultow_s, _i64tow_s, _ui64tow_s, _itoa, _ltoa, _ultoa, _i64toa, _ui64toa,
        _itow, _ltow, _i64tow,
    ];
    // The 64-bit names of the same functions.
    v.extend([
        ("_strtoi64", strtoll as *const () as u64),
        ("_strtoui64", strtoull as *const () as u64),
        ("_wcstoi64", wcstoll as *const () as u64),
        ("_wcstoui64", wcstoull as *const () as u64),
        ("_atoi64", atoll as *const () as u64),
        ("_wtoi64", _wtoll as *const () as u64),
    ]);
    v
}

xmm_abi! {
    strtod => strtod_soft [] "movq xmm0, rax";
    strtof => strtof_soft [] "movd xmm0, eax";
    strtold => strtold_soft [] "movq xmm0, rax";
    wcstod => wcstod_soft [] "movq xmm0, rax";
    wcstof => wcstof_soft [] "movd xmm0, eax";
    wcstold => wcstold_soft [] "movq xmm0, rax";
    atof => atof_soft [] "movq xmm0, rax";
    _wtof => _wtof_soft [] "movq xmm0, rax";
}
