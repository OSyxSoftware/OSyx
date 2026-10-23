//! Locales: "C", and the C locale with the UTF-8 code page ("C.UTF-8"),
//! per category, globally or per thread (`_configthreadlocale`), with the
//! multibyte conversions they define.

use super::*;
use alloc::string::String;
use core::sync::atomic::{AtomicI32, AtomicU8};

pub const LC_ALL: i32 = 0;
pub const LC_CTYPE: i32 = 2;
const LC_MAX: i32 = 5;
const NAMES: [&str; 6] = ["LC_ALL", "LC_COLLATE", "LC_CTYPE", "LC_MONETARY", "LC_NUMERIC", "LC_TIME"];
const UTF8_NAME: &str = "C.UTF-8";

/// Bit `c` set: category `c` (1..=5) uses the UTF-8 code page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Locale(u8);

impl Locale {
    pub const C: Locale = Locale(0);

    fn utf8(self, cat: i32) -> bool {
        self.0 & (1 << cat) != 0
    }

    fn with(self, cat: i32, utf8: bool) -> Locale {
        let bits = if cat == LC_ALL { 0b11_1110 } else { 1 << cat };
        Locale(if utf8 { self.0 | bits } else { self.0 & !bits })
    }

    /// The name setlocale reports for `cat` (LC_ALL: one name, or the
    /// composite "LC_COLLATE=..;..." form when categories differ).
    fn name(self, cat: i32) -> String {
        let one = |c: i32| if self.utf8(c) { UTF8_NAME } else { "C" };
        if cat != LC_ALL {
            return String::from(one(cat));
        }
        if self.0 == 0 || self.0 == 0b11_1110 {
            return String::from(one(1));
        }
        let mut s = String::new();
        for c in 1..=LC_MAX {
            if c > 1 {
                s.push(';');
            }
            s.push_str(NAMES[c as usize]);
            s.push('=');
            s.push_str(one(c));
        }
        s
    }
}

static GLOBAL: AtomicU8 = AtomicU8::new(0);

/// The locale in effect for the calling thread.
pub fn current() -> Locale {
    let p = ptd();
    if p.own_locale {
        p.locale
    } else {
        Locale(GLOBAL.load(Ordering::SeqCst))
    }
}

/// Whether LC_CTYPE uses UTF-8 (else the C locale's byte = code unit rule).
pub fn ctype_utf8() -> bool {
    current().utf8(LC_CTYPE)
}

/// One locale name (not composite): Some(utf8?) when OSyx can provide it.
fn parse_one(name: &str) -> Option<bool> {
    let n = name.trim();
    if n == "C" {
        return Some(false);
    }
    // "" is the user default: OSyx's ANSI code page is UTF-8 and it has
    // no national conventions beyond C.
    if n.is_empty() {
        return Some(true);
    }
    let cp = n.strip_prefix("C").unwrap_or(n);
    let cp = cp.strip_prefix('.')?;
    if cp.eq_ignore_ascii_case("utf-8") || cp.eq_ignore_ascii_case("utf8") || cp == "65001" {
        Some(true)
    } else {
        None
    }
}

fn apply(loc: Locale, cat: i32, name: &str) -> Option<Locale> {
    if cat == LC_ALL && name.contains('=') {
        let mut l = loc;
        for part in name.split(';') {
            let (k, v) = part.split_once('=')?;
            let c = NAMES.iter().position(|n| *n == k.trim()).filter(|&c| c != 0)? as i32;
            l = l.with(c, parse_one(v)?);
        }
        return Some(l);
    }
    Some(loc.with(cat, parse_one(name)?))
}

/// Storage for the strings setlocale returns: one per locale and
/// category, never freed (there are 32 locales x 6 categories at most).
static NAME_CACHE: Mutex<BTreeMap<(u8, i32, bool), usize>> = Mutex::new(BTreeMap::new());

fn name_ptr(loc: Locale, cat: i32, wide: bool) -> u64 {
    let mut m = NAME_CACHE.lock().unwrap();
    *m.entry((loc.0, cat, wide)).or_insert_with(|| {
        let s = loc.name(cat);
        if wide {
            let mut u: Vec<u16> = s.encode_utf16().collect();
            u.push(0);
            Vec::leak(u).as_ptr() as usize
        } else {
            let mut b = s.into_bytes();
            b.push(0);
            Vec::leak(b).as_ptr() as usize
        }
    }) as u64
}

fn set(cat: i32, name: Option<String>, wide: bool) -> u64 {
    validate!((0..=LC_MAX).contains(&cat), EINVAL, 0);
    let p = ptd();
    let own = p.own_locale;
    let loc = current();
    let Some(name) = name else { return name_ptr(loc, cat, wide) };
    let Some(new) = apply(loc, cat, &name) else { return 0 };
    if own {
        p.locale = new;
    } else {
        GLOBAL.store(new.0, Ordering::SeqCst);
    }
    name_ptr(new, cat, wide)
}

extern "win64" fn setlocale(cat: i32, name: *const u8) -> u64 {
    let n = if name.is_null() {
        None
    } else {
        // SAFETY: a NUL-terminated name.
        Some(String::from_utf8_lossy(unsafe { cstr(name) }).into_owned())
    };
    set(cat, n, false)
}

extern "win64" fn _wsetlocale(cat: i32, name: *const u16) -> u64 {
    let n = if name.is_null() {
        None
    } else {
        // SAFETY: a NUL-terminated name.
        Some(String::from_utf16_lossy(unsafe { wstr(name) }))
    };
    set(cat, n, true)
}

const ENABLE_PER_THREAD: i32 = 1;
const DISABLE_PER_THREAD: i32 = 2;
static GLOBAL_STATUS: AtomicI32 = AtomicI32::new(0);

extern "win64" fn _configthreadlocale(how: i32) -> i32 {
    let p = ptd();
    let prev = if p.own_locale { ENABLE_PER_THREAD } else { DISABLE_PER_THREAD };
    match how {
        0 => {}
        ENABLE_PER_THREAD => {
            if !p.own_locale {
                p.locale = Locale(GLOBAL.load(Ordering::SeqCst));
                p.own_locale = true;
            }
        }
        DISABLE_PER_THREAD => p.own_locale = false,
        // The legacy "global" setting of __globallocalestatus.
        -1 => GLOBAL_STATUS.store(-1, Ordering::SeqCst),
        _ => {
            set_errno(EINVAL);
            invalid_parameter();
            return -1;
        }
    }
    prev
}

extern "win64" fn ___mb_cur_max_func() -> i32 {
    if ctype_utf8() { 4 } else { 1 }
}

extern "win64" fn ___mb_cur_max_l_func(_l: u64) -> i32 {
    ___mb_cur_max_func()
}

extern "win64" fn ___lc_codepage_func() -> u32 {
    if ctype_utf8() { 65001 } else { 0 }
}

extern "win64" fn ___lc_collate_cp_func() -> u32 {
    if current().utf8(1) { 65001 } else { 0 }
}

/// Locale names per category: none (C conventions in every category).
static LOCALE_NAMES: [u64; 6] = [0; 6];

extern "win64" fn ___lc_locale_name_func() -> u64 {
    LOCALE_NAMES.as_ptr() as u64
}

// ctype classes (_UPPER, _LOWER, _DIGIT, _SPACE, _PUNCT, _CONTROL, _BLANK,
// _HEX, _ALPHA).
pub const UPPER: u16 = 0x1;
pub const LOWER: u16 = 0x2;
pub const DIGIT: u16 = 0x4;
pub const SPACE: u16 = 0x8;
pub const PUNCT: u16 = 0x10;
pub const CONTROL: u16 = 0x20;
pub const BLANK: u16 = 0x40;
pub const HEX: u16 = 0x80;
/// _ALPHA: the table has no bit of its own for letters (as in the
/// UCRT's C table); a letter is upper or lower case.
pub const ALPHA: u16 = 0x100 | UPPER | LOWER;

const fn class(c: u8) -> u16 {
    match c {
        b'A'..=b'F' => UPPER | HEX,
        b'G'..=b'Z' => UPPER,
        b'a'..=b'f' => LOWER | HEX,
        b'g'..=b'z' => LOWER,
        b'0'..=b'9' => DIGIT | HEX,
        b' ' => SPACE | BLANK,
        // '\t' is blank only through isblank, as in the UCRT table.
        b'\t' | b'\n' | 0x0b | 0x0c | b'\r' => SPACE | CONTROL,
        0..=0x1f | 0x7f => CONTROL,
        0x21..=0x7e => PUNCT,
        _ => 0,
    }
}

/// The C locale table, index 0 for EOF (-1); bytes above 0x7F have no
/// class in both locales (they are not characters on their own in UTF-8).
static CTYPE: [u16; 257] = {
    let mut t = [0u16; 257];
    let mut i = 0;
    while i < 256 {
        t[i + 1] = class(i as u8);
        i += 1;
    }
    t
};

/// Class bits of `c` (-1..=255; anything else has none).
pub fn ctype(c: i32) -> u16 {
    if (-1..=255).contains(&c) { CTYPE[(c + 1) as usize] } else { 0 }
}

extern "win64" fn __pctype_func() -> u64 {
    CTYPE.as_ptr() as u64 + 2
}

/// `_wctype`-style classes for 0..=255 (Latin-1 is not classified by the
/// C locale's wide functions either beyond ASCII).
extern "win64" fn __pwctype_func() -> u64 {
    CTYPE.as_ptr() as u64 + 2
}

/// The UCRT's locale lock.
static LOCALE_LOCK: RLock = RLock::new();

extern "win64" fn _lock_locales() {
    LOCALE_LOCK.lock()
}

extern "win64" fn _unlock_locales() {
    LOCALE_LOCK.unlock();
}

/// `struct lconv` of the C locale (both locales share its conventions).
#[repr(C)]
struct Lconv {
    strs: [*const u8; 10],
    chars: [u8; 8],
    wstrs: [*const u16; 8],
}

// SAFETY: immutable data pointing at static strings.
unsafe impl Sync for Lconv {}

static DOT: &[u8] = b".\0";
static EMPTY: &[u8] = b"\0";
static WDOT: [u16; 2] = [b'.' as u16, 0];
static WEMPTY: [u16; 1] = [0];

static LCONV: Lconv = Lconv {
    strs: [
        DOT.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
        EMPTY.as_ptr(),
    ],
    chars: [127; 8], // CHAR_MAX: not available
    wstrs: [
        WDOT.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
        WEMPTY.as_ptr(),
    ],
};

extern "win64" fn localeconv() -> u64 {
    &LCONV as *const Lconv as u64
}

// ---- multibyte conversion ---------------------------------------------------------

/// Appends the multibyte form of a UTF-16 string. Err: a unit the
/// LC_CTYPE code page cannot represent (EILSEQ).
pub fn wide_to_mb(units: &[u16], out: &mut Vec<u8>) -> Result<(), ()> {
    if ctype_utf8() {
        for c in char::decode_utf16(units.iter().copied()) {
            let c = c.map_err(|_| ())?;
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
    } else {
        for &u in units {
            out.push(u8::try_from(u).map_err(|_| ())?);
        }
    }
    Ok(())
}

/// Appends the UTF-16 form of a multibyte string. Err: an invalid or
/// incomplete sequence (EILSEQ).
pub fn mb_to_wide(bytes: &[u8], out: &mut Vec<u16>) -> Result<(), ()> {
    if ctype_utf8() {
        let s = core::str::from_utf8(bytes).map_err(|_| ())?;
        out.extend(s.encode_utf16());
    } else {
        out.extend(bytes.iter().map(|&b| b as u16));
    }
    Ok(())
}

/// Length of the multibyte character at the start of `bytes` (0 for
/// NUL), or None if it is invalid or incomplete.
pub fn mb_char(bytes: &[u8]) -> Option<(u32, usize)> {
    let b0 = *bytes.first()?;
    if !ctype_utf8() || b0 < 0x80 {
        return Some((b0 as u32, if b0 == 0 { 0 } else { 1 }));
    }
    let n = match b0 {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    let s = core::str::from_utf8(bytes.get(..n)?).ok()?;
    Some((s.chars().next()? as u32, n))
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        setlocale,
        _wsetlocale,
        _configthreadlocale,
        ___mb_cur_max_func,
        ___mb_cur_max_l_func,
        ___lc_codepage_func,
        ___lc_collate_cp_func,
        ___lc_locale_name_func,
        __pctype_func,
        __pwctype_func,
        _lock_locales,
        _unlock_locales,
        localeconv,
    ]
}
