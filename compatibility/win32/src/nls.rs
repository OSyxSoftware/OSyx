//! Locale identifiers and formatting for OSyx's one locale (en-US, see
//! `locale`): LCID and name conversion, preferred UI languages, the
//! geographic location, and date, time, number and currency formatting
//! with Windows picture strings and NUMBERFMT/CURRENCYFMT overrides.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    locale, path, sysext, teb,
};
use alloc::{string::String, vec::Vec};

const LCID_EN_US: u32 = 0x0409;
const LCID_INVARIANT: u32 = 0x007F;
const ERROR_INVALID_FLAGS: u32 = 1004;

/// The locale an LCID denotes: Some(true) en-US, Some(false) invariant.
fn lcid_locale(lcid: u32) -> Option<bool> {
    match lcid {
        // LOCALE_USER_DEFAULT, LOCALE_SYSTEM_DEFAULT, LOCALE_CUSTOM_DEFAULT,
        // LOCALE_CUSTOM_UI_DEFAULT, 0 (the thread locale) and en-US itself.
        0 | 0x0400 | 0x0800 | 0x0C00 | 0x1400 | LCID_EN_US => Some(true),
        LCID_INVARIANT => Some(false),
        _ => None,
    }
}

fn locale_name(en_us: bool) -> &'static str {
    if en_us { "en-US" } else { "" }
}

/// The name as UTF-16 with its NUL, for calling the -Ex forms.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Copies `s` and a NUL into a buffer of `size` units: the length with the
/// NUL, or with `size` 0 the length needed; ERROR_INSUFFICIENT_BUFFER if
/// it does not fit.
fn out_count(s: &str, buf: *mut u16, size: i32) -> i32 {
    let n = s.encode_utf16().count() as i32 + 1;
    if size == 0 {
        return n;
    }
    if buf.is_null() || size < n {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    kernel32::out_w(s, buf, size as u32);
    n
}

/// As [`out_count`] for an ANSI (UTF-8) buffer.
fn out_count_a(s: &str, buf: *mut u8, size: i32) -> i32 {
    let n = s.len() as i32 + 1;
    if size == 0 {
        return n;
    }
    if buf.is_null() || size < n {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer holds `n` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    n
}

// ---- identifiers ------------------------------------------------------------------

extern "win64" fn GetLocaleInfoW(lcid: u32, lctype: u32, buf: *mut u16, size: i32) -> i32 {
    let Some(en_us) = lcid_locale(lcid) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let name = wide(locale_name(en_us));
    locale::GetLocaleInfoEx(name.as_ptr(), lctype, buf, size)
}

/// GetLocaleInfoA: the Unicode text as ANSI (UTF-8 here); with
/// LOCALE_RETURN_NUMBER the DWORD itself (4 bytes).
extern "win64" fn GetLocaleInfoA(lcid: u32, lctype: u32, buf: *mut u8, size: i32) -> i32 {
    const LOCALE_RETURN_NUMBER: u32 = 0x2000_0000;
    if size < 0 || (size > 0 && buf.is_null()) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if lctype & LOCALE_RETURN_NUMBER != 0 {
        let mut n = 0u32;
        if GetLocaleInfoW(lcid, lctype, &mut n as *mut u32 as *mut u16, 2) == 0 {
            return 0;
        }
        if size == 0 {
            return 4;
        }
        if size < 4 {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        // SAFETY: four bytes of the caller's buffer.
        unsafe { core::ptr::write_unaligned(buf as *mut u32, n) };
        return 4;
    }
    let need = GetLocaleInfoW(lcid, lctype, core::ptr::null_mut(), 0);
    if need == 0 {
        return 0;
    }
    let mut w = alloc::vec![0u16; need as usize];
    if GetLocaleInfoW(lcid, lctype, w.as_mut_ptr(), need) == 0 {
        return 0;
    }
    let text = String::from_utf16_lossy(&w[..need as usize - 1]);
    let n = text.len() as i32 + 1;
    if size == 0 {
        return n;
    }
    if size < n {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer holds `size` >= n bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
        *buf.add(text.len()) = 0;
    }
    n
}

const LCID_INSTALLED: u32 = 1;
const LCID_SUPPORTED: u32 = 2;
const LCID_ALTERNATE_SORTS: u32 = 4;

/// IsValidLocale: the LCIDs this system has data for (en-US, the
/// invariant locale and the default aliases), installed and supported alike.
extern "win64" fn IsValidLocale(lcid: u32, flags: u32) -> i32 {
    if flags != LCID_INSTALLED && flags != LCID_SUPPORTED {
        return 0;
    }
    lcid_locale(lcid).is_some() as i32
}

/// EnumSystemLocalesW: LOCALE_ENUMPROCW gets each LCID as eight hex digits
/// ("00000409"); there are no alternate sorts.
extern "win64" fn EnumSystemLocalesW(proc_: u64, flags: u32) -> i32 {
    if proc_ == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if flags & !(LCID_INSTALLED | LCID_SUPPORTED | LCID_ALTERNATE_SORTS) != 0 {
        return fail(ERROR_INVALID_FLAGS, 0);
    }
    if flags & LCID_ALTERNATE_SORTS != 0 {
        return 1;
    }
    let text: Vec<u16> = alloc::format!("{LCID_EN_US:08X}").encode_utf16().chain(core::iter::once(0)).collect();
    crate::wow64::callback(proc_, &[text.as_ptr() as u64]);
    1
}

/// FoldStringW: MAP_FOLDDIGITS maps every Unicode decimal digit to its
/// ASCII form; MAP_FOLDCZONE maps the compatibility zone (full-width ASCII
/// and the ideographic space) to standard characters; MAP_EXPAND_LIGATURES
/// expands the Latin ligatures. MAP_PRECOMPOSED and MAP_COMPOSITE use the
/// decompositions of the Latin-1 and Latin Extended-A letters with one
/// diacritic ([`COMPOSED`]).
extern "win64" fn FoldStringW(flags: u32, src: *const u16, n: i32, dst: *mut u16, cap: i32) -> i32 {
    const MAP_FOLDCZONE: u32 = 0x10;
    const MAP_PRECOMPOSED: u32 = 0x20;
    const MAP_COMPOSITE: u32 = 0x40;
    const MAP_FOLDDIGITS: u32 = 0x80;
    const MAP_EXPAND_LIGATURES: u32 = 0x2000;
    let known = MAP_FOLDCZONE | MAP_PRECOMPOSED | MAP_COMPOSITE | MAP_FOLDDIGITS | MAP_EXPAND_LIGATURES;
    if flags == 0
        || flags & !known != 0
        || (flags & MAP_PRECOMPOSED != 0 && flags & MAP_COMPOSITE != 0)
        || (flags & MAP_EXPAND_LIGATURES != 0 && flags & (MAP_PRECOMPOSED | MAP_COMPOSITE) != 0)
    {
        return fail(ERROR_INVALID_FLAGS, 0);
    }
    if src.is_null() || n == 0 || cap < 0 || (cap > 0 && dst.is_null()) || src == dst as *const u16 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: `n` units, or a NUL-terminated string (the NUL included).
    let input: Vec<u16> = unsafe {
        let len = if n < 0 { (0..).take_while(|&i| *src.add(i) != 0).count() + 1 } else { n as usize };
        core::slice::from_raw_parts(src, len).to_vec()
    };
    let mut out: Vec<u16> = Vec::with_capacity(input.len());
    for c in char::decode_utf16(input.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')) {
        let mut c = c;
        if flags & MAP_FOLDCZONE != 0 {
            c = match c as u32 {
                0x3000 => ' ',
                v @ 0xFF01..=0xFF5E => char::from_u32(v - 0xFF01 + 0x21).unwrap_or(c),
                _ => c,
            };
        }
        if flags & MAP_FOLDDIGITS != 0 {
            if let Some(d) = unicode_digit(c) {
                c = char::from_digit(d, 10).unwrap_or(c);
            }
        }
        if flags & MAP_EXPAND_LIGATURES != 0 {
            if let Some(e) = ligature(c) {
                out.extend(e.encode_utf16());
                continue;
            }
        }
        if flags & MAP_COMPOSITE != 0 {
            if let Some((base, mark)) = decompose(c) {
                out.push(base as u16);
                out.push(mark);
                continue;
            }
        }
        if flags & MAP_PRECOMPOSED != 0 {
            if let Some(p) = out.last().and_then(|&last| precompose(last, c as u32)) {
                *out.last_mut().unwrap() = p as u16;
                continue;
            }
        }
        let mut buf = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut buf));
    }
    if cap == 0 {
        return out.len() as i32;
    }
    if out.len() > cap as usize {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `cap` units.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), dst, out.len()) };
    out.len() as i32
}

/// The value of a decimal digit in any script (Unicode Nd): the blocks of
/// ten consecutive digits, plus the superscript and subscript digits.
fn unicode_digit(c: char) -> Option<u32> {
    const ZEROS: [u32; 25] = [
        0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
        0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x2080, 0xFF10,
    ];
    let v = c as u32;
    if let Some(z) = ZEROS.iter().find(|&&z| v >= z && v < z + 10) {
        return Some(v - z);
    }
    match v {
        0x2070 => Some(0),
        0x00B9 => Some(1),
        0x00B2 => Some(2),
        0x00B3 => Some(3),
        0x2074..=0x2079 => Some(v - 0x2070),
        _ => None,
    }
}

fn ligature(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{00C6}' => "AE",
        '\u{00E6}' => "ae",
        '\u{0152}' => "OE",
        '\u{0153}' => "oe",
        '\u{00DF}' => "ss",
        '\u{FB00}' => "ff",
        '\u{FB01}' => "fi",
        '\u{FB02}' => "fl",
        '\u{FB03}' => "ffi",
        '\u{FB04}' => "ffl",
        '\u{FB06}' => "st",
        _ => return None,
    })
}

/// Letters with one diacritic: (precomposed, base, combining mark).
const COMPOSED: &[(u32, char, u16)] = &[
    (0xC0, 'A', 0x300), (0xC1, 'A', 0x301), (0xC2, 'A', 0x302), (0xC3, 'A', 0x303), (0xC4, 'A', 0x308), (0xC5, 'A', 0x30A),
    (0xC7, 'C', 0x327), (0xC8, 'E', 0x300), (0xC9, 'E', 0x301), (0xCA, 'E', 0x302), (0xCB, 'E', 0x308), (0xCC, 'I', 0x300),
    (0xCD, 'I', 0x301), (0xCE, 'I', 0x302), (0xCF, 'I', 0x308), (0xD1, 'N', 0x303), (0xD2, 'O', 0x300), (0xD3, 'O', 0x301),
    (0xD4, 'O', 0x302), (0xD5, 'O', 0x303), (0xD6, 'O', 0x308), (0xD9, 'U', 0x300), (0xDA, 'U', 0x301), (0xDB, 'U', 0x302),
    (0xDC, 'U', 0x308), (0xDD, 'Y', 0x301), (0xE0, 'a', 0x300), (0xE1, 'a', 0x301), (0xE2, 'a', 0x302), (0xE3, 'a', 0x303),
    (0xE4, 'a', 0x308), (0xE5, 'a', 0x30A), (0xE7, 'c', 0x327), (0xE8, 'e', 0x300), (0xE9, 'e', 0x301), (0xEA, 'e', 0x302),
    (0xEB, 'e', 0x308), (0xEC, 'i', 0x300), (0xED, 'i', 0x301), (0xEE, 'i', 0x302), (0xEF, 'i', 0x308), (0xF1, 'n', 0x303),
    (0xF2, 'o', 0x300), (0xF3, 'o', 0x301), (0xF4, 'o', 0x302), (0xF5, 'o', 0x303), (0xF6, 'o', 0x308), (0xF9, 'u', 0x300),
    (0xFA, 'u', 0x301), (0xFB, 'u', 0x302), (0xFC, 'u', 0x308), (0xFD, 'y', 0x301), (0xFF, 'y', 0x308), (0x100, 'A', 0x304),
    (0x101, 'a', 0x304), (0x102, 'A', 0x306), (0x103, 'a', 0x306), (0x104, 'A', 0x328), (0x105, 'a', 0x328), (0x106, 'C', 0x301),
    (0x107, 'c', 0x301), (0x10C, 'C', 0x30C), (0x10D, 'c', 0x30C), (0x10E, 'D', 0x30C), (0x10F, 'd', 0x30C), (0x112, 'E', 0x304),
    (0x113, 'e', 0x304), (0x118, 'E', 0x328), (0x119, 'e', 0x328), (0x11A, 'E', 0x30C), (0x11B, 'e', 0x30C), (0x12A, 'I', 0x304),
    (0x12B, 'i', 0x304), (0x143, 'N', 0x301), (0x144, 'n', 0x301), (0x147, 'N', 0x30C), (0x148, 'n', 0x30C), (0x14C, 'O', 0x304),
    (0x14D, 'o', 0x304), (0x150, 'O', 0x30B), (0x151, 'o', 0x30B), (0x158, 'R', 0x30C), (0x159, 'r', 0x30C), (0x15A, 'S', 0x301),
    (0x15B, 's', 0x301), (0x160, 'S', 0x30C), (0x161, 's', 0x30C), (0x164, 'T', 0x30C), (0x165, 't', 0x30C), (0x16A, 'U', 0x304),
    (0x16B, 'u', 0x304), (0x16E, 'U', 0x30A), (0x16F, 'u', 0x30A), (0x170, 'U', 0x30B), (0x171, 'u', 0x30B), (0x178, 'Y', 0x308),
    (0x179, 'Z', 0x301), (0x17A, 'z', 0x301), (0x17B, 'Z', 0x307), (0x17C, 'z', 0x307), (0x17D, 'Z', 0x30C), (0x17E, 'z', 0x30C),
];

/// MAP_COMPOSITE: a precomposed letter as base letter and combining mark.
fn decompose(c: char) -> Option<(char, u16)> {
    COMPOSED.iter().find(|e| e.0 == c as u32).map(|e| (e.1, e.2))
}

/// MAP_PRECOMPOSED: a base letter followed by a combining mark as one letter.
fn precompose(base: u16, combining: u32) -> Option<u32> {
    COMPOSED.iter().find(|e| e.1 as u32 == base as u32 && e.2 as u32 == combining).map(|e| e.0)
}

extern "win64" fn LCIDToLocaleName(lcid: u32, buf: *mut u16, size: i32, _flags: u32) -> i32 {
    let Some(en_us) = lcid_locale(lcid) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    out_count(locale_name(en_us), buf, size)
}

extern "win64" fn LocaleNameToLCID(name: *const u16, _flags: u32) -> u32 {
    match locale::locale(name) {
        Some(true) => LCID_EN_US,
        Some(false) => LCID_INVARIANT,
        None => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

/// The supported locale closest to `name`: English names resolve to
/// en-US; others have no match here and resolve to the empty name.
extern "win64" fn ResolveLocaleName(name: *const u16, buf: *mut u16, size: i32) -> i32 {
    // SAFETY: NULL or a NUL-terminated name.
    let n = unsafe { path::wide_to_string(name) }.unwrap_or_default();
    let lang = n.split(['-', '_']).next().unwrap_or("");
    let best = if name.is_null() || lang.eq_ignore_ascii_case("en") { "en-US" } else { "" };
    out_count(best, buf, size)
}

extern "win64" fn GetSystemDefaultLangID() -> u16 {
    LCID_EN_US as u16
}

extern "win64" fn GetThreadLocale() -> u32 {
    LCID_EN_US
}

const MUI_LANGUAGE_ID: u32 = 0x4;
const MUI_LANGUAGE_NAME: u32 = 0x8;

/// A double-NUL-terminated list with the one UI language.
extern "win64" fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buf: *mut u16, size: *mut u32) -> i32 {
    if count.is_null() || size.is_null() || flags & !(MUI_LANGUAGE_ID | MUI_LANGUAGE_NAME | 0x40) != 0
        || flags & (MUI_LANGUAGE_ID | MUI_LANGUAGE_NAME) == (MUI_LANGUAGE_ID | MUI_LANGUAGE_NAME)
    {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let list: &[u16] = if flags & MUI_LANGUAGE_ID != 0 {
        &[b'0' as u16, b'4' as u16, b'0' as u16, b'9' as u16, 0, 0]
    } else {
        &[b'e' as u16, b'n' as u16, b'-' as u16, b'U' as u16, b'S' as u16, 0, 0]
    };
    // SAFETY: the caller's out pointers and buffer of `*size` units.
    unsafe {
        *count = 1;
        if buf.is_null() || *size == 0 {
            *size = list.len() as u32;
            return 1;
        }
        if (*size as usize) < list.len() {
            *size = list.len() as u32;
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::copy_nonoverlapping(list.as_ptr(), buf, list.len());
        *size = list.len() as u32;
    }
    1
}

const GEOID_US: i32 = 244;

extern "win64" fn GetUserGeoID(class: u32) -> i32 {
    if class == 16 { GEOID_US } else { -1 } // GEOCLASS_NATION; GEOID_NOT_AVAILABLE
}

extern "win64" fn GetGeoInfoW(geo: i32, kind: u32, buf: *mut u16, size: i32, _lang: u16) -> i32 {
    if geo != GEOID_US {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let v = match kind {
        1 => "244",                      // GEO_NATION
        2 => "39.45",                    // GEO_LATITUDE (as Windows reports it)
        3 => "-98.908",                  // GEO_LONGITUDE
        4 => "US",                       // GEO_ISO2
        5 => "USA",                      // GEO_ISO3
        6 => "1",                        // GEO_RFC1766... reported as the LCID nation code
        8 => "United States",            // GEO_FRIENDLYNAME
        9 => "United States of America", // GEO_OFFICIALNAME
        12 => "840",                     // GEO_ISO_UN_NUMBER
        13 => "19",                      // GEO_PARENT (Americas)
        _ => return fail(ERROR_INVALID_FLAGS, 0),
    };
    out_count(v, buf, size)
}

const CSTR_LESS_THAN: i32 = 1;

/// Code-unit order, optionally after simple upper-casing of each unit (as
/// Windows' ordinal comparison does). Lengths of -1 mean NUL-terminated.
extern "win64" fn CompareStringOrdinal(a: *const u16, alen: i32, b: *const u16, blen: i32, ignore_case: i32) -> i32 {
    let units = |p: *const u16, n: i32| -> Option<Vec<u16>> {
        if p.is_null() || n < -1 {
            return None;
        }
        let len = if n == -1 {
            // SAFETY: a NUL-terminated string.
            (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count()
        } else {
            n as usize
        };
        // SAFETY: `len` units of the caller's string.
        Some(unsafe { core::slice::from_raw_parts(p, len) }.to_vec())
    };
    let (Some(x), Some(y)) = (units(a, alen), units(b, blen)) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let upper = |u: u16| -> u16 {
        if ignore_case == 0 {
            return u;
        }
        char::from_u32(u as u32)
            .map(|c| {
                let mut up = c.to_uppercase();
                match (up.next(), up.next()) {
                    (Some(v), None) if (v as u32) < 0x10000 => v as u16,
                    _ => u,
                }
            })
            .unwrap_or(u)
    };
    let ord = x.iter().map(|&u| upper(u)).cmp(y.iter().map(|&u| upper(u)));
    CSTR_LESS_THAN + (ord as i32 + 1)
}

// ---- date and time pictures -------------------------------------------------------

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Expands a picture: runs of one letter are fields, 'text' is literal
/// ('' is a quote), anything else is copied.
fn picture(fmt: &str, mut field: impl FnMut(char, usize) -> Option<String>) -> Option<String> {
    let c: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\'' {
            i += 1;
            while i < c.len() {
                if c[i] == '\'' {
                    if c.get(i + 1) == Some(&'\'') {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                out.push(c[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < c.len() && c[i + n] == c[i] {
            n += 1;
        }
        match field(c[i], n) {
            Some(s) => out.push_str(&s),
            None => (0..n).for_each(|_| out.push(c[i])),
        }
        i += n;
    }
    Some(out)
}

/// The SYSTEMTIME at `st`, or the current local time.
fn date_of(st: *const [u16; 8]) -> Option<[u16; 8]> {
    if st.is_null() {
        let mut now = [0u16; 8];
        crate::zone::local_now(&mut now);
        return Some(now);
    }
    // SAFETY: the caller's SYSTEMTIME.
    let v = unsafe { *st };
    let mut ft = 0u64;
    // Validates the date; the day of week is recomputed from it.
    if sysext::SystemTimeToFileTime(&v, &mut ft) == 0 {
        return None;
    }
    Some(sysext::system_time(ft))
}

const DATE_SHORTDATE: u32 = 0x1;
const DATE_LONGDATE: u32 = 0x2;
const DATE_USE_ALT_CALENDAR: u32 = 0x4;
const DATE_YEARMONTH: u32 = 0x8;
const DATE_MONTHDAY: u32 = 0x80;

fn format_date(en_us: bool, flags: u32, st: *const [u16; 8], fmt: Option<String>) -> Result<String, u32> {
    let kinds = flags & (DATE_SHORTDATE | DATE_LONGDATE | DATE_YEARMONTH | DATE_MONTHDAY);
    if flags & !(kinds | DATE_USE_ALT_CALENDAR | 0x8000_0000 | 0x10 | 0x20) != 0 || kinds.count_ones() > 1 || (fmt.is_some() && kinds != 0) {
        return Err(ERROR_INVALID_FLAGS);
    }
    let d = date_of(st).ok_or(ERROR_INVALID_PARAMETER)?;
    let fmt = fmt.unwrap_or_else(|| {
        String::from(match (kinds, en_us) {
            (DATE_LONGDATE, true) => "dddd, MMMM d, yyyy",
            (DATE_LONGDATE, false) => "dddd, dd MMMM yyyy",
            (DATE_YEARMONTH, true) => "MMMM yyyy",
            (DATE_YEARMONTH, false) => "yyyy MMMM",
            (DATE_MONTHDAY, true) => "MMMM d",
            (DATE_MONTHDAY, false) => "MMMM dd",
            (_, true) => "M/d/yyyy",
            (_, false) => "MM/dd/yyyy",
        })
    });
    let (y, m, wd, day) = (d[0], d[1] as usize, d[2] as usize, d[3]);
    picture(&fmt, |c, n| {
        Some(match c {
            'd' => match n {
                1 => alloc::format!("{day}"),
                2 => alloc::format!("{day:02}"),
                3 => String::from(&DAYS[wd][..3]),
                _ => String::from(DAYS[wd]),
            },
            'M' => match n {
                1 => alloc::format!("{m}"),
                2 => alloc::format!("{m:02}"),
                3 => String::from(&MONTHS[m - 1][..3]),
                _ => String::from(MONTHS[m - 1]),
            },
            'y' => match n {
                1 => alloc::format!("{}", y % 100),
                2 => alloc::format!("{:02}", y % 100),
                _ => alloc::format!("{y}"),
            },
            'g' => String::from("A.D."),
            _ => return None,
        })
    })
    .ok_or(ERROR_INVALID_PARAMETER)
}

const TIME_NOMINUTESORSECONDS: u32 = 0x1;
const TIME_NOSECONDS: u32 = 0x2;
const TIME_NOTIMEMARKER: u32 = 0x4;
const TIME_FORCE24HOURFORMAT: u32 = 0x8;

fn format_time(en_us: bool, flags: u32, st: *const [u16; 8], fmt: Option<String>) -> Result<String, u32> {
    if flags & !(TIME_NOMINUTESORSECONDS | TIME_NOSECONDS | TIME_NOTIMEMARKER | TIME_FORCE24HOURFORMAT | 0x8000_0000) != 0 {
        return Err(ERROR_INVALID_FLAGS);
    }
    let t = if st.is_null() {
        let mut now = [0u16; 8];
        crate::zone::local_now(&mut now);
        now
    } else {
        // SAFETY: the caller's SYSTEMTIME.
        unsafe { *st }
    };
    let (h, mi, s) = (t[4], t[5], t[6]);
    if h > 23 || mi > 59 || s > 59 || t[7] > 999 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut fmt = fmt.unwrap_or_else(|| String::from(if en_us { "h:mm:ss tt" } else { "HH:mm:ss" }));
    if flags & TIME_FORCE24HOURFORMAT != 0 {
        fmt = fmt.replace('h', "H");
    }
    // Dropping a field also drops the separator before it.
    let drop = |fmt: &str, field: char| -> String {
        let mut out = String::new();
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == field {
                while out.ends_with(|c: char| !c.is_alphanumeric() && c != '\'') {
                    out.pop();
                }
                while i < chars.len() && chars[i] == field {
                    i += 1;
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    };
    if flags & (TIME_NOMINUTESORSECONDS | TIME_NOSECONDS) != 0 {
        fmt = drop(&fmt, 's');
    }
    if flags & TIME_NOMINUTESORSECONDS != 0 {
        fmt = drop(&fmt, 'm');
    }
    if flags & TIME_NOTIMEMARKER != 0 {
        fmt = String::from(drop(&fmt, 't').trim_end());
    }
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    picture(&fmt, |c, n| {
        Some(match c {
            'h' => if n == 1 { alloc::format!("{h12}") } else { alloc::format!("{h12:02}") },
            'H' => if n == 1 { alloc::format!("{h}") } else { alloc::format!("{h:02}") },
            'm' => if n == 1 { alloc::format!("{mi}") } else { alloc::format!("{mi:02}") },
            's' => if n == 1 { alloc::format!("{s}") } else { alloc::format!("{s:02}") },
            't' => {
                let marker = if h < 12 { "AM" } else { "PM" };
                String::from(if n == 1 { &marker[..1] } else { marker })
            }
            _ => return None,
        })
    })
    .ok_or(ERROR_INVALID_PARAMETER)
}

fn finish_w(r: Result<String, u32>, buf: *mut u16, size: i32) -> i32 {
    match r {
        Ok(s) => out_count(&s, buf, size),
        Err(e) => fail(e, 0),
    }
}

fn finish_a(r: Result<String, u32>, buf: *mut u8, size: i32) -> i32 {
    match r {
        Ok(s) => out_count_a(&s, buf, size),
        Err(e) => fail(e, 0),
    }
}

pub(crate) extern "win64" fn GetDateFormatEx(
    name: *const u16,
    flags: u32,
    st: *const [u16; 8],
    fmt: *const u16,
    buf: *mut u16,
    size: i32,
    calendar: *const u16,
) -> i32 {
    let Some(en_us) = locale::locale(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if !calendar.is_null() || size < 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: NULL or a NUL-terminated picture.
    finish_w(format_date(en_us, flags, st, unsafe { path::wide_to_string(fmt) }), buf, size)
}

extern "win64" fn GetDateFormatA(lcid: u32, flags: u32, st: *const [u16; 8], fmt: *const u8, buf: *mut u8, size: i32) -> i32 {
    let Some(en_us) = lcid_locale(lcid) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // SAFETY: NULL or a NUL-terminated picture.
    finish_a(format_date(en_us, flags, st, unsafe { path::ansi_to_string(fmt) }), buf, size)
}

pub(crate) extern "win64" fn GetTimeFormatEx(name: *const u16, flags: u32, st: *const [u16; 8], fmt: *const u16, buf: *mut u16, size: i32) -> i32 {
    let Some(en_us) = locale::locale(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // SAFETY: NULL or a NUL-terminated picture.
    finish_w(format_time(en_us, flags, st, unsafe { path::wide_to_string(fmt) }), buf, size)
}

extern "win64" fn GetTimeFormatA(lcid: u32, flags: u32, st: *const [u16; 8], fmt: *const u8, buf: *mut u8, size: i32) -> i32 {
    let Some(en_us) = lcid_locale(lcid) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // SAFETY: NULL or a NUL-terminated picture.
    finish_a(format_time(en_us, flags, st, unsafe { path::ansi_to_string(fmt) }), buf, size)
}

// ---- numbers and currency ------------------------------------------------------------

/// NUMBERFMTW / the number part of CURRENCYFMTW.
struct NumFmt {
    digits: u32,
    leading_zero: bool,
    grouping: u32,
    decimal: String,
    thousand: String,
    negative: u32,
}

/// # Safety
/// `p` must be a readable NUMBERFMTW or CURRENCYFMTW.
unsafe fn read_numfmt(p: u64) -> Option<NumFmt> {
    // SAFETY: the caller's structure.
    unsafe {
        Some(NumFmt {
            digits: teb::get(p, 0),
            leading_zero: teb::get::<u32>(p, 4) != 0,
            grouping: teb::get(p, 8),
            decimal: path::wide_to_string(teb::get::<u64>(p, 16) as *const u16)?,
            thousand: path::wide_to_string(teb::get::<u64>(p, 24) as *const u16)?,
            negative: teb::get(p, 32),
        })
    }
}

/// Validates a number string ("-", digits, ".", digits) and lays it out:
/// rounded half up to `digits` decimals, grouped, with the locale's
/// separators. Returns (negative, text without sign).
fn layout(value: &str, f: &NumFmt) -> Option<(bool, String)> {
    let (neg, v) = match value.strip_prefix('-') {
        Some(v) => (true, v),
        None => (false, value),
    };
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    if (int.is_empty() && frac.is_empty()) || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut digits: Vec<u8> = int.bytes().map(|b| b - b'0').collect();
    let mut fr: Vec<u8> = frac.bytes().map(|b| b - b'0').collect();
    let want = f.digits as usize;
    let round_up = fr.len() > want && fr[want] >= 5;
    fr.resize(want, 0);
    if round_up {
        let mut carry = true;
        for d in fr.iter_mut().rev().chain(digits.iter_mut().rev()) {
            if !carry {
                break;
            }
            *d += 1;
            carry = *d == 10;
            if carry {
                *d = 0;
            }
        }
        if carry {
            digits.insert(0, 1);
        }
    }
    while digits.len() > 1 && digits[0] == 0 {
        digits.remove(0);
    }
    let zero_int = digits.iter().all(|&d| d == 0);
    let neg = neg && !(zero_int && fr.iter().all(|&d| d == 0));
    // Grouping as NUMBERFMT encodes it: one digit per group from the
    // right (3 = "3", 32 = "3;2"); the last group repeats unless the
    // number ends in 0 (320 = "3;2;0": no repeat). 0 = no grouping.
    let mut groups: Vec<usize> = alloc::format!("{}", f.grouping).bytes().map(|b| (b - b'0') as usize).collect();
    let repeat = !(groups.len() > 1 && groups.last() == Some(&0));
    if !repeat {
        groups.pop();
    }
    let mut int_s = String::new();
    if !(zero_int && !f.leading_zero && !fr.is_empty()) {
        let s: Vec<char> = digits.iter().map(|&d| (b'0' + d) as char).collect();
        let mut parts: Vec<String> = Vec::new();
        let mut end = s.len();
        let mut gi = 0;
        while end > 0 {
            let g = match groups.get(gi).or(if repeat { groups.last() } else { None }) {
                Some(&g) if g > 0 => g.min(end),
                _ => end,
            };
            parts.push(s[end - g..end].iter().collect());
            end -= g;
            gi += 1;
        }
        parts.reverse();
        int_s = parts.join(&f.thousand);
    }
    let mut out = int_s;
    if !fr.is_empty() {
        out.push_str(&f.decimal);
        out.extend(fr.iter().map(|&d| (b'0' + d) as char));
    }
    Some((neg, out))
}

fn default_number(en_us: bool) -> NumFmt {
    let _ = en_us;
    NumFmt { digits: 2, leading_zero: true, grouping: 3, decimal: String::from("."), thousand: String::from(","), negative: 1 }
}

extern "win64" fn GetNumberFormatEx(name: *const u16, flags: u32, value: *const u16, fmt: u64, buf: *mut u16, size: i32) -> i32 {
    let Some(en_us) = locale::locale(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if fmt != 0 && flags & !0x8000_0000 != 0 {
        return fail(ERROR_INVALID_FLAGS, 0);
    }
    // SAFETY: NULL or the caller's NUMBERFMTW.
    let f = if fmt == 0 { Some(default_number(en_us)) } else { unsafe { read_numfmt(fmt) } };
    let Some(f) = f.filter(|f| f.digits <= 9 && f.negative <= 4) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // SAFETY: a NUL-terminated value.
    let Some(v) = (unsafe { path::wide_to_string(value) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let Some((neg, n)) = layout(&v, &f) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let s = if !neg {
        n
    } else {
        match f.negative {
            0 => alloc::format!("({n})"),
            1 => alloc::format!("-{n}"),
            2 => alloc::format!("- {n}"),
            3 => alloc::format!("{n}-"),
            _ => alloc::format!("{n} -"),
        }
    };
    out_count(&s, buf, size)
}

extern "win64" fn GetCurrencyFormatEx(name: *const u16, flags: u32, value: *const u16, fmt: u64, buf: *mut u16, size: i32) -> i32 {
    let Some(en_us) = locale::locale(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if fmt != 0 && flags & !0x8000_0000 != 0 {
        return fail(ERROR_INVALID_FLAGS, 0);
    }
    // CURRENCYFMTW adds PositiveOrder (36) and lpCurrencySymbol (40).
    // SAFETY: NULL or the caller's CURRENCYFMTW.
    let parsed = if fmt == 0 {
        let mut f = default_number(en_us);
        f.negative = 0;
        Some((f, 0u32, String::from(if en_us { "$" } else { "\u{a4}" })))
    } else {
        unsafe { read_numfmt(fmt).and_then(|f| Some((f, teb::get::<u32>(fmt, 36), path::wide_to_string(teb::get::<u64>(fmt, 40) as *const u16)?))) }
    };
    let Some((f, pos, sym)) = parsed.filter(|(f, p, _)| f.digits <= 9 && f.negative <= 15 && *p <= 3) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    // SAFETY: a NUL-terminated value.
    let Some(v) = (unsafe { path::wide_to_string(value) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let Some((neg, n)) = layout(&v, &f) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let s = if !neg {
        match pos {
            0 => alloc::format!("{sym}{n}"),
            1 => alloc::format!("{n}{sym}"),
            2 => alloc::format!("{sym} {n}"),
            _ => alloc::format!("{n} {sym}"),
        }
    } else {
        match f.negative {
            0 => alloc::format!("({sym}{n})"),
            1 => alloc::format!("-{sym}{n}"),
            2 => alloc::format!("{sym}-{n}"),
            3 => alloc::format!("{sym}{n}-"),
            4 => alloc::format!("({n}{sym})"),
            5 => alloc::format!("-{n}{sym}"),
            6 => alloc::format!("{n}-{sym}"),
            7 => alloc::format!("{n}{sym}-"),
            8 => alloc::format!("-{n} {sym}"),
            9 => alloc::format!("-{sym} {n}"),
            10 => alloc::format!("{n} {sym}-"),
            11 => alloc::format!("{sym} {n}-"),
            12 => alloc::format!("{sym} -{n}"),
            13 => alloc::format!("{n}- {sym}"),
            14 => alloc::format!("({sym} {n})"),
            _ => alloc::format!("({n} {sym})"),
        }
    };
    out_count(&s, buf, size)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetLocaleInfoW,
        GetLocaleInfoA,
        IsValidLocale,
        EnumSystemLocalesW,
        FoldStringW,
        CompareStringOrdinal,
        LCIDToLocaleName,
        LocaleNameToLCID,
        ResolveLocaleName,
        GetSystemDefaultLangID,
        GetThreadLocale,
        GetUserPreferredUILanguages,
        GetUserGeoID,
        GetGeoInfoW,
        GetDateFormatEx,
        GetDateFormatA,
        GetTimeFormatEx,
        GetTimeFormatA,
        GetNumberFormatEx,
        GetCurrencyFormatEx,
    ]
}
