//! The __stdio_common_*scanf engine. Stream input uses one lookahead unit,
//! returned with ungetc on completion; bounded strings never read past n.
//! Windows x64 va_list entries are eight-byte slots, including the unsigned
//! buffer capacities of secure character and string conversions.
use super::printf::Ch;
use super::stdio::{self, File};
use super::*;

const SECURE: u64 = 1;
const LEGACY_WIDE: u64 = 2;

enum Source<C> {
    Text(*const C, usize),
    Stream(*mut File),
}

struct Input<C: Ch> {
    source: Source<C>,
    count: usize,
    look: Option<u32>,
    loaded: bool,
}

impl<C: Ch> Input<C> {
    fn peek(&mut self) -> Option<u32> {
        if !self.loaded {
            self.look = match self.source {
                Source::Text(p, n) if self.count < n => {
                    // SAFETY: bounded by n and stopped at the first NUL.
                    let c = unsafe { (*p.add(self.count)).code() };
                    (c != 0).then_some(c)
                }
                Source::Text(..) => None,
                Source::Stream(f) => {
                    if C::WIDE {
                        let c = stdio::fgetwc(f);
                        (c != 0xffff).then_some(c as u32)
                    } else {
                        let c = stdio::fgetc(f);
                        (c != -1).then_some(c as u32)
                    }
                }
            };
            self.loaded = true;
        }
        self.look
    }

    fn take(&mut self) -> Option<u32> {
        let c = self.peek()?;
        self.count += 1;
        self.loaded = false;
        Some(c)
    }

    fn spaces(&mut self) {
        while self.peek().is_some_and(space) {
            self.take();
        }
    }
}

impl<C: Ch> Drop for Input<C> {
    fn drop(&mut self) {
        if let Source::Stream(f) = self.source {
            if self.loaded {
                if let Some(c) = self.look {
                    if C::WIDE {
                        stdio::ungetwc(c as u16, f);
                    } else {
                        stdio::ungetc(c as i32, f);
                    }
                }
            }
        }
    }
}

fn space(c: u32) -> bool {
    c == 32 || (9..=13).contains(&c)
}

fn digit(c: u32) -> Option<u32> {
    match c {
        48..=57 => Some(c - 48),
        65..=70 => Some(c - 65 + 10),
        97..=102 => Some(c - 97 + 10),
        _ => None,
    }
}

struct Args(*const u64);
impl Args {
    fn next(&mut self) -> u64 {
        // SAFETY: the format and va_list agree, as required by C.
        unsafe {
            let v = *self.0;
            self.0 = self.0.add(1);
            v
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Size {
    Default,
    Byte,
    Short,
    Long,
    LongLong,
    Pointer,
    Wide,
    Double,
}

fn store_integer(p: u64, n: u64, size: Size) {
    // SAFETY: the caller supplies a destination matching the conversion.
    unsafe {
        match size {
            Size::Byte => (p as *mut u8).write(n as u8),
            Size::Short => (p as *mut u16).write(n as u16),
            Size::LongLong | Size::Pointer => (p as *mut u64).write(n),
            _ => (p as *mut u32).write(n as u32),
        }
    }
}

/// A field width applies to input units, including a sign and 0x prefix.
struct Field<'a, C: Ch> {
    input: &'a mut Input<C>,
    left: usize,
}
impl<C: Ch> Field<'_, C> {
    fn peek(&mut self) -> Option<u32> {
        if self.left == 0 {
            None
        } else {
            self.input.peek()
        }
    }
    fn take(&mut self) -> Option<u32> {
        let c = self.peek()?;
        self.left -= 1;
        self.input.take();
        Some(c)
    }
    fn accept(&mut self, pred: impl FnOnce(u32) -> bool) -> bool {
        if self.peek().is_some_and(pred) {
            self.take();
            true
        } else {
            false
        }
    }
}

fn integer<C: Ch>(f: &mut Field<C>, mut base: u32) -> Option<u64> {
    let neg = f.accept(|c| c == 45);
    if !neg {
        f.accept(|c| c == 43);
    }
    let (mut value, mut any) = (0u64, false);
    if (base == 0 || base == 16) && f.accept(|c| c == 48) {
        any = true;
        if f.accept(|c| c == 120 || c == 88) {
            base = 16;
        } else if base == 0 {
            base = 8;
        }
    }
    if base == 0 {
        base = 10;
    }
    while let Some(d) = f.peek().and_then(digit).filter(|&d| d < base) {
        f.take();
        any = true;
        value = value.wrapping_mul(base as u64).wrapping_add(d as u64);
    }
    any.then_some(if neg { value.wrapping_neg() } else { value })
}

fn float<C: Ch>(f: &mut Field<C>, single: bool) -> Option<f64> {
    let mut text = Vec::new();
    if f.peek().is_some_and(|c| c == 43 || c == 45) {
        text.push(f.take()? as u8);
    }
    if f.peek().is_some_and(|c| c == 105 || c == 73 || c == 110 || c == 78) {
        let inf = f.peek()? | 32 == 105;
        for &c in if inf { b"inf" } else { b"nan" } {
            if !f.peek().is_some_and(|u| u | 32 == c as u32) {
                return None;
            }
            text.push(f.take()? as u8);
        }
        if inf && f.peek().is_some_and(|c| c | 32 == 105) {
            for &c in b"inity" {
                if !f.peek().is_some_and(|u| u | 32 == c as u32) {
                    return None;
                }
                text.push(f.take()? as u8);
            }
        } else if !inf && f.accept(|c| c == 40) {
            text.push(b'(');
            while f.peek().is_some_and(|c| c == 95 || (c < 128 && (c as u8).is_ascii_alphanumeric())) {
                text.push(f.take()? as u8);
            }
            if !f.accept(|c| c == 41) {
                return None;
            }
            text.push(b')');
        }
    } else {
        let mut hex = false;
        if f.accept(|c| c == 48) {
            text.push(b'0');
            if f.peek().is_some_and(|c| c | 32 == 120) {
                text.push(f.take()? as u8);
                hex = true;
            }
        }
        let mut point = false;
        loop {
            if f.peek().and_then(digit).is_some_and(|d| d < if hex { 16 } else { 10 }) {
                text.push(f.take()? as u8);
            } else if !point && f.accept(|c| c == 46) {
                text.push(b'.');
                point = true;
            } else {
                break;
            }
        }
        let exp = if hex { 112 } else { 101 };
        if f.peek().is_some_and(|c| c | 32 == exp) {
            text.push(f.take()? as u8);
            if f.peek().is_some_and(|c| c == 43 || c == 45) {
                text.push(f.take()? as u8);
            }
            while f.peek().is_some_and(|c| (48..=57).contains(&c)) {
                text.push(f.take()? as u8);
            }
        }
    }
    text.push(0);
    convert::scan_float(&text, single)
}

fn in_set(c: u32, set: &[u32]) -> bool {
    let mut i = 0;
    while i < set.len() {
        if i + 2 < set.len() && set[i + 1] == 45 {
            // MS scanf accepts descending ranges as well as ascending ones.
            if (set[i].min(set[i + 2])..=set[i].max(set[i + 2])).contains(&c) {
                return true;
            }
            i += 3;
        } else {
            if set[i] == c {
                return true;
            }
            i += 1;
        }
    }
    false
}

/// Convert one character across narrow/wide boundaries. In UTF-8 mode a
/// narrow input character may consume several bytes, counted by %n.
fn character<C: Ch>(f: &mut Field<C>, wide: bool, out: &mut Vec<u16>) -> bool {
    let Some(c) = f.take() else { return false };
    if C::WIDE || !wide {
        out.push(c as u16);
        return true;
    }
    let mut bytes = alloc::vec![c as u8];
    if locale::ctype_utf8() && c >= 128 {
        let n = match c {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if n == 0 {
            set_errno(EILSEQ);
            return false;
        }
        for _ in 1..n {
            // A string field's width counts characters, not UTF-8
            // continuation bytes. %n still counts every input byte.
            let Some(b) = f.input.take() else {
                set_errno(EILSEQ);
                return false;
            };
            bytes.push(b as u8);
        }
    }
    if locale::mb_to_wide(&bytes, out).is_err() {
        set_errno(EILSEQ);
        false
    } else {
        true
    }
}

fn scan<C: Ch>(opts: u64, input: &mut Input<C>, fmt: *const C, args: *const u64) -> i32 {
    let mut args = Args(args);
    let (mut i, mut assigned) = (0usize, 0i32);
    // SAFETY: caller provides a NUL-terminated format string.
    let at = |i: usize| unsafe { (*fmt.add(i)).code() };
    while at(i) != 0 {
        let c = at(i);
        i += 1;
        if space(c) {
            input.spaces();
            continue;
        }
        if c != 37 || at(i) == 37 {
            if c == 37 {
                i += 1;
            }
            match input.peek() {
                Some(x) if x == c => {
                    input.take();
                }
                Some(_) => return assigned,
                None => return if assigned == 0 { -1 } else { assigned },
            }
            continue;
        }
        let suppress = at(i) == 42;
        if suppress {
            i += 1;
        }
        let mut width = 0usize;
        let has_width = (48..=57).contains(&at(i));
        while (48..=57).contains(&at(i)) {
            width = width.saturating_mul(10).saturating_add((at(i) - 48) as usize);
            i += 1;
        }
        if has_width && width == 0 {
            return assigned;
        }
        if !has_width {
            width = usize::MAX;
        }
        let size = match at(i) {
            104 => {
                i += 1;
                if at(i) == 104 {
                    i += 1;
                    Size::Byte
                } else {
                    Size::Short
                }
            }
            108 => {
                i += 1;
                if at(i) == 108 {
                    i += 1;
                    Size::LongLong
                } else {
                    Size::Long
                }
            }
            76 => {
                i += 1;
                Size::Double
            }
            119 => {
                i += 1;
                Size::Wide
            }
            106 | 122 | 116 => {
                i += 1;
                Size::Pointer
            }
            73 => {
                i += 1;
                if at(i) == 54 && at(i + 1) == 52 {
                    i += 2;
                    Size::LongLong
                } else if at(i) == 51 && at(i + 1) == 50 {
                    i += 2;
                    Size::Default
                } else {
                    Size::Pointer
                }
            }
            _ => Size::Default,
        };
        let spec = at(i);
        if spec == 0 {
            return invalid(EINVAL, assigned);
        }
        i += 1;
        if !matches!(
            spec,
            99 | 67
                | 115
                | 83
                | 91
                | 110
                | 100
                | 105
                | 111
                | 117
                | 120
                | 88
                | 112
                | 97
                | 65
                | 101
                | 69
                | 102
                | 70
                | 103
                | 71
        ) {
            return invalid(EINVAL, assigned);
        }
        let mut set = Vec::new();
        let mut inverse = false;
        if spec == 91 {
            if at(i) == 94 {
                inverse = true;
                i += 1;
            }
            if at(i) == 93 {
                set.push(93);
                i += 1;
            }
            while at(i) != 0 && at(i) != 93 {
                set.push(at(i));
                i += 1;
            }
            if at(i) != 93 {
                return invalid(EINVAL, assigned);
            }
            i += 1;
        }
        if !matches!(spec, 99 | 67 | 91 | 110) {
            input.spaces();
        }
        let p = if suppress { 0 } else { args.next() };
        if !suppress && p == 0 {
            return invalid(EINVAL, -1);
        }
        if spec == 110 {
            if !suppress {
                store_integer(p, input.count as u64, size);
            }
            continue;
        }
        let string = matches!(spec, 99 | 67 | 115 | 83 | 91);
        let cap = if string && !suppress && opts & SECURE != 0 { args.next() as u32 as usize } else { usize::MAX };
        if cap == 0 {
            return invalid(EINVAL, -1);
        }
        if input.peek().is_none() {
            return if assigned == 0 { -1 } else { assigned };
        }
        let mut field = Field { input, left: width };
        let matched = match spec {
            100 | 105 | 111 | 117 | 120 | 88 | 112 => {
                let base = match spec {
                    105 => 0,
                    111 => 8,
                    120 | 88 | 112 => 16,
                    _ => 10,
                };
                if let Some(n) = integer(&mut field, base) {
                    if !suppress {
                        store_integer(p, n, if spec == 112 { Size::Pointer } else { size });
                    }
                    true
                } else {
                    false
                }
            }
            97 | 65 | 101 | 69 | 102 | 70 | 103 | 71 => {
                let double = matches!(size, Size::Long | Size::LongLong | Size::Double);
                if let Some(v) = float(&mut field, !double) {
                    if !suppress {
                        // SAFETY: the destination type follows the conversion.
                        unsafe {
                            if double {
                                (p as *mut f64).write(v);
                            } else {
                                (p as *mut f32).write(v as f32);
                            }
                        }
                    }
                    true
                } else {
                    false
                }
            }
            99 | 67 | 115 | 83 | 91 => {
                let chars = spec == 99 || spec == 67;
                if chars && !has_width {
                    field.left = 1;
                }
                let wide = match size {
                    Size::Long | Size::Wide => true,
                    Size::Short | Size::Byte => false,
                    _ => {
                        if C::WIDE && opts & LEGACY_WIDE != 0 {
                            spec != 67 && spec != 83
                        } else {
                            spec == 67 || spec == 83
                        }
                    }
                };
                let mut out = Vec::new();
                let mut count = 0;
                let mut bad = false;
                while let Some(c) = field.peek() {
                    if !chars && (if spec == 91 { in_set(c, &set) == inverse } else { space(c) }) {
                        break;
                    }
                    if !character(&mut field, wide, &mut out) {
                        bad = true;
                        break;
                    }
                    count += 1;
                }
                if bad || count == 0 {
                    false
                } else {
                    let mut bytes = Vec::new();
                    if !wide && C::WIDE {
                        if locale::wide_to_mb(&out, &mut bytes).is_err() {
                            set_errno(EILSEQ);
                            return assigned;
                        }
                    } else if !wide {
                        bytes.extend(out.iter().map(|&c| c as u8));
                    }
                    let n = if wide { out.len() } else { bytes.len() };
                    if !suppress {
                        if n.saturating_add(usize::from(!chars)) > cap {
                            // A secure conversion never writes a partial token.
                            if !chars {
                                // SAFETY: cap > 0 and p was validated.
                                unsafe {
                                    if wide {
                                        *(p as *mut u16) = 0;
                                    } else {
                                        *(p as *mut u8) = 0;
                                    }
                                }
                            }
                            set_errno(ENOMEM);
                            return assigned;
                        }
                        // SAFETY: cap was checked; ordinary scanf requires the caller to size the buffer.
                        unsafe {
                            if wide {
                                core::ptr::copy_nonoverlapping(out.as_ptr(), p as *mut u16, n);
                                if !chars {
                                    *(p as *mut u16).add(n) = 0;
                                }
                            } else {
                                core::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, n);
                                if !chars {
                                    *(p as *mut u8).add(n) = 0;
                                }
                            }
                        }
                    }
                    true
                }
            }
            _ => return invalid(EINVAL, assigned),
        };
        if !matched {
            return if assigned == 0 && input.peek().is_none() { -1 } else { assigned };
        }
        if !suppress {
            assigned += 1;
        }
    }
    assigned
}

fn text<C: Ch>(o: u64, s: *const C, n: usize, fmt: *const C, a: *const u64) -> i32 {
    validate!(!s.is_null() && !fmt.is_null(), EINVAL, -1);
    scan(o, &mut Input { source: Source::Text(s, n), count: 0, look: None, loaded: false }, fmt, a)
}

fn stream<C: Ch>(o: u64, f: *mut File, fmt: *const C, a: *const u64) -> i32 {
    validate!(!f.is_null() && !fmt.is_null(), EINVAL, -1);
    // SAFETY: caller supplies a FILE of this runtime. The lock is recursive.
    unsafe { (*f).lock() };
    let result = scan(o, &mut Input { source: Source::Stream(f), count: 0, look: None, loaded: false }, fmt, a);
    // SAFETY: same stream, held across scanning and lookahead restoration.
    unsafe { (*f).unlock() };
    result
}

extern "win64" fn __stdio_common_vsscanf(o: u64, s: *const u8, n: usize, f: *const u8, _l: u64, a: *const u64) -> i32 {
    text(o, s, n, f, a)
}
extern "win64" fn __stdio_common_vswscanf(
    o: u64,
    s: *const u16,
    n: usize,
    f: *const u16,
    _l: u64,
    a: *const u64,
) -> i32 {
    text(o, s, n, f, a)
}
extern "win64" fn __stdio_common_vfscanf(o: u64, s: *mut File, f: *const u8, _l: u64, a: *const u64) -> i32 {
    stream(o, s, f, a)
}
extern "win64" fn __stdio_common_vfwscanf(o: u64, s: *mut File, f: *const u16, _l: u64, a: *const u64) -> i32 {
    stream(o, s, f, a)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![__stdio_common_vsscanf, __stdio_common_vswscanf, __stdio_common_vfscanf, __stdio_common_vfwscanf]
}
