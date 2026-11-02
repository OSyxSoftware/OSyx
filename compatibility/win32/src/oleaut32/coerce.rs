//! VariantChangeType(Ex): coercion of a VARIANT between the scalar types
//! and BSTR, with the en-US formats of OSyx's one locale. The rules follow
//! Windows: floats round to the nearest integer with ties to even, a
//! value outside the target's range is DISP_E_OVERFLOW, text that is no
//! number is DISP_E_TYPEMISMATCH, NULL converts to nothing but NULL,
//! VT_BOOL is -1/0 and its text "-1"/"0" (VARIANT_ALPHABOOL: "True" and
//! "False"), CY is a 64-bit count of 1/10000 and DATE the days since
//! 1899-12-30. Interface pointers have no value to convert here
//! (DISP_E_TYPEMISMATCH, as for a NULL one on Windows).

use super::*;
use alloc::string::String;

const VT_NULL: u16 = 1;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_CY: u16 = 6;
const VT_DATE: u16 = 7;
const VT_ERROR: u16 = 10;
const VT_BOOL: u16 = 11;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;

const DISP_E_TYPEMISMATCH: u32 = 0x8002_0005;
const DISP_E_OVERFLOW: u32 = 0x8002_000A;
const VARIANT_ALPHABOOL: u16 = 0x2;

/// A VARIANT's value, decoded.
enum Value {
    Empty,
    Null,
    Int(i128),
    Float(f64, bool),
    Cy(i64),
    Date(f64),
    Bool(bool),
    Text(String),
    Error(u32),
}

/// The 8-byte value area of a VARIANT (pointers are 4 bytes in a 32-bit one).
fn read(src: *const u8) -> Result<Value, u32> {
    // SAFETY: the caller's VARIANT: vt at 0, the value at 8.
    unsafe {
        let vt = *(src as *const u16);
        let at = src.add(8);
        let i = |n: usize| -> i128 {
            match n {
                1 => *(at as *const i8) as i128,
                2 => core::ptr::read_unaligned(at as *const i16) as i128,
                4 => core::ptr::read_unaligned(at as *const i32) as i128,
                _ => core::ptr::read_unaligned(at as *const i64) as i128,
            }
        };
        let u = |n: usize| -> i128 {
            match n {
                1 => *at as i128,
                2 => core::ptr::read_unaligned(at as *const u16) as i128,
                4 => core::ptr::read_unaligned(at as *const u32) as i128,
                _ => core::ptr::read_unaligned(at as *const u64) as i128,
            }
        };
        Ok(match vt {
            VT_EMPTY => Value::Empty,
            VT_NULL => Value::Null,
            VT_I1 => Value::Int(i(1)),
            VT_I2 => Value::Int(i(2)),
            VT_I4 | VT_INT => Value::Int(i(4)),
            VT_I8 => Value::Int(i(8)),
            VT_UI1 => Value::Int(u(1)),
            VT_UI2 => Value::Int(u(2)),
            VT_UI4 | VT_UINT => Value::Int(u(4)),
            VT_UI8 => Value::Int(u(8)),
            VT_R4 => Value::Float(core::ptr::read_unaligned(at as *const f32) as f64, true),
            VT_R8 => Value::Float(core::ptr::read_unaligned(at as *const f64), false),
            VT_CY => Value::Cy(core::ptr::read_unaligned(at as *const i64)),
            VT_DATE => Value::Date(core::ptr::read_unaligned(at as *const f64)),
            VT_BOOL => Value::Bool(core::ptr::read_unaligned(at as *const i16) != 0),
            VT_ERROR => Value::Error(core::ptr::read_unaligned(at as *const u32)),
            VT_BSTR => {
                let p = if crate::wow64::is_active() { core::ptr::read_unaligned(at as *const u32) as u64 } else { core::ptr::read_unaligned(at as *const u64) };
                let n = if p == 0 { 0 } else { SysStringLen(p) as usize };
                let units = if p == 0 { &[][..] } else { core::slice::from_raw_parts(p as *const u16, n) };
                Value::Text(String::from_utf16_lossy(units))
            }
            VT_DISPATCH | VT_UNKNOWN => return Err(DISP_E_TYPEMISMATCH),
            _ => return Err(DISP_E_BADVARTYPE),
        })
    }
}

/// The largest integer not above `f` (|f| below 2^63; larger values are
/// integers already).
fn floor(f: f64) -> f64 {
    if !f.is_finite() || f.abs() >= 9.2e18 {
        return f;
    }
    let t = f as i64 as f64;
    if t > f { t - 1.0 } else { t }
}

/// Round half to even, as the VarXxxFromR8 functions do.
fn round_even(f: f64) -> f64 {
    let low = floor(f);
    let diff = f - low;
    if diff > 0.5 || (diff == 0.5 && (low as i64) % 2 != 0) { low + 1.0 } else { low }
}

/// Parses number text as VarR8FromStr does for en-US: blanks around it,
/// an optional sign, digits with "," thousands separators and "." decimals,
/// an exponent, or "&H" / "&O" for hexadecimal and octal integers.
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c == ' ' || c == '\t');
    let lower = t.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("&h") {
        return i64::from_str_radix(h, 16).ok().map(|v| v as i32 as f64);
    }
    if let Some(o) = lower.strip_prefix("&o") {
        return i64::from_str_radix(o, 8).ok().map(|v| v as f64);
    }
    let cleaned: String = t.chars().filter(|&c| c != ',').collect();
    if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn to_f64(v: &Value) -> Result<f64, u32> {
    Ok(match v {
        Value::Empty => 0.0,
        Value::Null | Value::Error(_) => return Err(DISP_E_TYPEMISMATCH),
        Value::Int(i) => *i as f64,
        Value::Float(f, _) | Value::Date(f) => *f,
        Value::Cy(c) => *c as f64 / 10000.0,
        Value::Bool(b) => if *b { -1.0 } else { 0.0 },
        Value::Text(s) => {
            if s.eq_ignore_ascii_case("true") {
                -1.0
            } else if s.eq_ignore_ascii_case("false") {
                0.0
            } else {
                parse_number(s).ok_or(DISP_E_TYPEMISMATCH)?
            }
        }
    })
}

fn to_int(v: &Value) -> Result<i128, u32> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Cy(c) => Ok(round_even(*c as f64 / 10000.0) as i128),
        _ => {
            let f = to_f64(v)?;
            if !f.is_finite() || f.abs() > 1.9e19 {
                return Err(DISP_E_OVERFLOW);
            }
            Ok(round_even(f) as i128)
        }
    }
}

/// %g-like text with `digits` significant digits: plain for exponents
/// from -5 to digits-1, "1.5E+20" style otherwise; no trailing zeros.
fn float_text(f: f64, digits: usize) -> String {
    if f == 0.0 {
        return String::from("0");
    }
    if !f.is_finite() {
        return String::from(if f.is_nan() { "NaN" } else if f > 0.0 { "1.#INF" } else { "-1.#INF" });
    }
    // Round to `digits` significant digits first; that may move the exponent.
    let scaled = alloc::format!("{:.*e}", digits - 1, f);
    let (mantissa, e) = scaled.split_once('e').unwrap_or((&scaled, "0"));
    let e: i32 = e.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') { String::from(s.trim_end_matches('0').trim_end_matches('.')) } else { String::from(s) }
    };
    if e < -5 || e >= digits as i32 {
        let sign = if e < 0 { '-' } else { '+' };
        return alloc::format!("{}E{}{:02}", trim(mantissa), sign, e.abs());
    }
    let decimals = (digits as i32 - 1 - e).max(0) as usize;
    trim(&alloc::format!("{:.*}", decimals, f))
}

/// The en-US short date and long time of an OLE date: "1/2/2003 12:00:00 PM";
/// without a time part only the date, before day 1 only the time.
fn date_text(d: f64) -> String {
    // Before 1899-12-30 the fraction still counts forward from midnight.
    let days = if d < 0.0 { -(floor(-d) as i64) } else { floor(d) as i64 };
    let frac = (d - days as f64).abs();
    let secs = round_even(frac * 86400.0) as i64 % 86400;
    // Days since 1899-12-30 to a civil date (Howard Hinnant's algorithm,
    // from 1970-01-01 = day 25569).
    let z = days - 25569 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let time = alloc::format!("{}:{:02}:{:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, m, s, if h < 12 { "AM" } else { "PM" });
    match (days != 0, secs != 0) {
        (true, false) => alloc::format!("{month}/{day}/{year}"),
        (false, true) => time,
        _ if days == 0 => time,
        _ => alloc::format!("{month}/{day}/{year} {time}"),
    }
}

fn to_text(v: &Value, flags: u16) -> Result<String, u32> {
    Ok(match v {
        Value::Empty => String::new(),
        Value::Null | Value::Error(_) => return Err(DISP_E_TYPEMISMATCH),
        Value::Int(i) => alloc::format!("{i}"),
        Value::Float(f, single) => float_text(*f, if *single { 7 } else { 15 }),
        Value::Cy(c) => float_text(*c as f64 / 10000.0, 19),
        Value::Date(d) => date_text(*d),
        Value::Bool(b) => String::from(match (*b, flags & VARIANT_ALPHABOOL != 0) {
            (true, true) => "True",
            (false, true) => "False",
            (true, false) => "-1",
            (false, false) => "0",
        }),
        Value::Text(s) => s.clone(),
    })
}

/// Writes the converted value into `dst` (already cleared): vt, then the
/// value at 8.
fn store(dst: *mut u8, vt: u16, v: &Value, flags: u16) -> Result<(), u32> {
    let range = |i: i128, lo: i128, hi: i128| if i < lo || i > hi { Err(DISP_E_OVERFLOW) } else { Ok(i) };
    let mut bytes = [0u8; 8];
    match vt {
        VT_EMPTY => {}
        VT_NULL => {
            if !matches!(v, Value::Null) {
                return Err(DISP_E_TYPEMISMATCH);
            }
        }
        _ if matches!(v, Value::Null) => return Err(DISP_E_TYPEMISMATCH),
        VT_I1 => bytes[..1].copy_from_slice(&(range(to_int(v)?, -128, 127)? as i8).to_le_bytes()),
        VT_UI1 => bytes[..1].copy_from_slice(&(range(to_int(v)?, 0, 255)? as u8).to_le_bytes()),
        VT_I2 => bytes[..2].copy_from_slice(&(range(to_int(v)?, -32768, 32767)? as i16).to_le_bytes()),
        VT_UI2 => bytes[..2].copy_from_slice(&(range(to_int(v)?, 0, 65535)? as u16).to_le_bytes()),
        VT_I4 | VT_INT => bytes[..4].copy_from_slice(&(range(to_int(v)?, i32::MIN as i128, i32::MAX as i128)? as i32).to_le_bytes()),
        // VarUI4FromI4 keeps the bits of a negative 32-bit value.
        VT_UI4 | VT_UINT => bytes[..4].copy_from_slice(&(range(to_int(v)?, i32::MIN as i128, u32::MAX as i128)? as u32).to_le_bytes()),
        VT_I8 => bytes.copy_from_slice(&(range(to_int(v)?, i64::MIN as i128, i64::MAX as i128)? as i64).to_le_bytes()),
        VT_UI8 => bytes.copy_from_slice(&(range(to_int(v)?, 0, u64::MAX as i128)? as u64).to_le_bytes()),
        VT_R4 => {
            let f = to_f64(v)?;
            if f.is_finite() && f.abs() > f32::MAX as f64 {
                return Err(DISP_E_OVERFLOW);
            }
            bytes[..4].copy_from_slice(&(f as f32).to_le_bytes());
        }
        VT_R8 => bytes.copy_from_slice(&to_f64(v)?.to_le_bytes()),
        VT_DATE => {
            let f = to_f64(v)?;
            if !(-657434.0..=2958465.999_999_999).contains(&f) {
                return Err(DISP_E_OVERFLOW);
            }
            bytes.copy_from_slice(&f.to_le_bytes());
        }
        VT_CY => {
            let c = match v {
                Value::Cy(c) => *c,
                Value::Int(i) => range(*i * 10000, i64::MIN as i128, i64::MAX as i128)? as i64,
                _ => {
                    let f = to_f64(v)? * 10000.0;
                    if !f.is_finite() || f.abs() >= 9.2e18 {
                        return Err(DISP_E_OVERFLOW);
                    }
                    round_even(f) as i64
                }
            };
            bytes.copy_from_slice(&c.to_le_bytes());
        }
        VT_BOOL => {
            let b = match v {
                Value::Bool(b) => *b,
                _ => to_f64(v)? != 0.0,
            };
            bytes[..2].copy_from_slice(&(if b { -1i16 } else { 0 }).to_le_bytes());
        }
        VT_ERROR => match v {
            Value::Error(e) => bytes[..4].copy_from_slice(&e.to_le_bytes()),
            _ => return Err(DISP_E_TYPEMISMATCH),
        },
        VT_BSTR => {
            let units: Vec<u16> = to_text(v, flags)?.encode_utf16().collect();
            let b = SysAllocStringLen(units.as_ptr(), units.len() as u32);
            if b == 0 {
                return Err(E_OUTOFMEMORY);
            }
            if crate::wow64::is_active() {
                bytes[..4].copy_from_slice(&(b as u32).to_le_bytes());
            } else {
                bytes.copy_from_slice(&b.to_le_bytes());
            }
        }
        VT_DISPATCH | VT_UNKNOWN => return Err(DISP_E_TYPEMISMATCH),
        _ => return Err(DISP_E_BADVARTYPE),
    }
    // SAFETY: the caller's VARIANT.
    unsafe {
        *(dst as *mut u16) = vt;
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.add(8), 8);
    }
    Ok(())
}

pub(super) extern "win64" fn VariantChangeTypeEx(dst: *mut u8, src: *const u8, _lcid: u32, flags: u16, vt: u16) -> u32 {
    if dst.is_null() || src.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's VARIANT.
    if vt & (VT_ARRAY | VT_BYREF) != 0 || unsafe { *(src as *const u16) } & VT_BYREF != 0 {
        return DISP_E_BADVARTYPE;
    }
    let value = match read(src) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // The source may be the destination: it is decoded before dst changes.
    if !core::ptr::eq(dst as *const u8, src) {
        let hr = VariantClear(dst);
        if hr != S_OK {
            return hr;
        }
    } else if let Value::Text(_) = value {
        // The source BSTR is freed once the copy is made.
        let mut tmp = [0u8; 24];
        // SAFETY: a 16 or 24-byte VARIANT.
        unsafe { core::ptr::copy_nonoverlapping(src, tmp.as_mut_ptr(), if crate::wow64::is_active() { 16 } else { 24 }) };
        let r = store(dst, vt, &value, flags);
        if r.is_ok() {
            VariantClear(tmp.as_mut_ptr());
        }
        return r.err().unwrap_or(S_OK);
    }
    match store(dst, vt, &value, flags) {
        Ok(()) => S_OK,
        Err(e) => e,
    }
}

pub(super) extern "win64" fn VariantChangeType(dst: *mut u8, src: *const u8, flags: u16, vt: u16) -> u32 {
    VariantChangeTypeEx(dst, src, 0x0409, flags, vt)
}
