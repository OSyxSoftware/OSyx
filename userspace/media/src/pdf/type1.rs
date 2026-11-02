//! Embedded Type 1 fonts (Adobe Type 1 Font Format): the clear text part
//! (encoding, font matrix), eexec decryption, Subrs and CharStrings, and
//! the charstring interpreter with flex, hint replacement and seac.

use super::encoding::STANDARD;
use super::raster::{Path, Seg};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub struct Type1 {
    /// Glyph names by code from the font's own encoding.
    pub encoding: BTreeMap<u8, String>,
    pub matrix: [f64; 6],
    subrs: Vec<Vec<u8>>,
    charstrings: BTreeMap<String, Vec<u8>>,
}

fn decrypt(data: &[u8], mut r: u16, skip: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
    }
    out.split_off(skip.min(out.len()))
}

fn is_hex(b: &[u8]) -> bool {
    b.len() >= 4 && b[..4].iter().all(|c| c.is_ascii_hexdigit())
}

fn hex_bytes(b: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = b
        .iter()
        .filter_map(|&c| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        })
        .collect();
    digits.chunks(2).map(|p| p[0] << 4 | p.get(1).copied().unwrap_or(0)).collect()
}

/// Splits a PFB file into its clear text and binary parts.
fn unpfb(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if data.first() != Some(&0x80) {
        return None;
    }
    let (mut clear, mut bin) = (Vec::new(), Vec::new());
    let mut p = 0;
    while p + 6 <= data.len() && data[p] == 0x80 {
        let kind = data[p + 1];
        let len = u32::from_le_bytes(data[p + 2..p + 6].try_into().unwrap()) as usize;
        let body = &data[p + 6..(p + 6 + len).min(data.len())];
        match kind {
            1 if bin.is_empty() => clear.extend_from_slice(body),
            2 => bin.extend_from_slice(body),
            _ => {}
        }
        p += 6 + len;
    }
    Some((clear, bin))
}

fn skip_ws(d: &[u8], mut p: usize) -> usize {
    while p < d.len() && d[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

fn read_int(d: &[u8], p: &mut usize) -> Option<i64> {
    *p = skip_ws(d, *p);
    let s = *p;
    while *p < d.len() && (d[*p].is_ascii_digit() || d[*p] == b'-') {
        *p += 1;
    }
    core::str::from_utf8(&d[s..*p]).ok()?.parse().ok()
}

fn read_token<'a>(d: &'a [u8], p: &mut usize) -> &'a [u8] {
    *p = skip_ws(d, *p);
    let s = *p;
    while *p < d.len() && !d[*p].is_ascii_whitespace() && !(*p > s && matches!(d[*p], b'/' | b'[' | b'{')) {
        *p += 1;
    }
    &d[s..*p]
}

impl Type1 {
    /// `len1`: length of the clear text part (Length1), when known.
    pub fn parse(data: &[u8], len1: Option<usize>) -> Option<Type1> {
        let (clear, enc) = match unpfb(data) {
            Some(x) => x,
            None => {
                let split = len1
                    .filter(|&l| l > 0 && l < data.len())
                    .or_else(|| data.windows(5).position(|w| w == b"eexec").map(|e| skip_ws(data, e + 5)))?;
                // Length1 sometimes stops before the newline after eexec.
                let split = if data[..split].ends_with(b"eexec") { skip_ws(data, split) } else { split };
                (data[..split].to_vec(), data[split..].to_vec())
            }
        };
        let mut t = Type1 {
            encoding: BTreeMap::new(),
            matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            subrs: Vec::new(),
            charstrings: BTreeMap::new(),
        };
        t.parse_clear(&clear);
        let enc = if is_hex(&enc) { hex_bytes(&enc) } else { enc };
        let private = decrypt(&enc, 55665, 4);
        t.parse_private(&private);
        (!t.charstrings.is_empty()).then_some(t)
    }

    fn parse_clear(&mut self, d: &[u8]) {
        if let Some(i) = d.windows(11).position(|w| w == b"/FontMatrix") {
            let mut p = i + 11;
            while p < d.len() && d[p] != b'[' && d[p] != b'{' {
                p += 1;
            }
            p += 1;
            let mut m = Vec::new();
            for _ in 0..6 {
                let tok = read_token(d, &mut p);
                if let Some(v) =
                    core::str::from_utf8(tok).ok().and_then(|s| s.trim_end_matches([']', '}']).parse::<f64>().ok())
                {
                    m.push(v);
                }
            }
            if m.len() == 6 {
                self.matrix.copy_from_slice(&m);
            }
        }
        if let Some(i) = d.windows(9).position(|w| w == b"/Encoding") {
            let rest = &d[i + 9..];
            if !rest[skip_ws(rest, 0)..].starts_with(b"StandardEncoding") {
                // "dup <code> /<name> put" entries up to "readonly def" (or a
                // "def" on its own: "/.notdef" in the set-up loop is not it).
                let end =
                    rest.windows(4).position(|w| w[0].is_ascii_whitespace() && &w[1..] == b"def").unwrap_or(rest.len());
                let body = &rest[..end];
                let mut p = 0;
                while let Some(o) = body[p..].windows(3).position(|w| w == b"dup") {
                    p += o + 3;
                    let mut q = p;
                    let Some(code) = read_int(body, &mut q) else { continue };
                    let name = read_token(body, &mut q);
                    if let Some(n) = name.strip_prefix(b"/") {
                        if (0..256).contains(&code) {
                            self.encoding.insert(code as u8, String::from_utf8_lossy(n).into_owned());
                        }
                    }
                    p = q;
                }
            }
        }
    }

    fn parse_private(&mut self, d: &[u8]) {
        let len_iv = d
            .windows(6)
            .position(|w| w == b"/lenIV")
            .and_then(|i| {
                let mut p = i + 6;
                read_int(d, &mut p)
            })
            .unwrap_or(4)
            .max(-1);
        let decode = |cs: &[u8]| if len_iv < 0 { cs.to_vec() } else { decrypt(cs, 4330, len_iv as usize) };
        // Binary blobs are introduced by "n RD " or "n -| ".
        let blob = |d: &[u8], p: &mut usize| -> Option<Vec<u8>> {
            let n = read_int(d, p)? as usize;
            let _rd = read_token(d, p);
            *p += 1;
            let b = d.get(*p..*p + n)?.to_vec();
            *p += n;
            Some(b)
        };
        if let Some(i) = d.windows(6).position(|w| w == b"/Subrs") {
            let mut p = i + 6;
            let count = read_int(d, &mut p).unwrap_or(0).clamp(0, 65535) as usize;
            self.subrs = alloc::vec![Vec::new(); count];
            for _ in 0..count {
                let Some(o) = d[p..].windows(3).position(|w| w == b"dup") else { break };
                p += o + 3;
                let Some(idx) = read_int(d, &mut p) else { break };
                let Some(b) = blob(d, &mut p) else { break };
                if let Some(slot) = self.subrs.get_mut(idx as usize) {
                    *slot = decode(&b);
                }
            }
        }
        if let Some(i) = d.windows(12).position(|w| w == b"/CharStrings") {
            let mut p = i + 12;
            let _count = read_int(d, &mut p);
            // Up to "begin", then "/name n RD <bytes> ND" until "end".
            if let Some(b) = d[p..].windows(5).position(|w| w == b"begin") {
                p += b + 5;
            }
            loop {
                p = skip_ws(d, p);
                if p >= d.len() || d[p..].starts_with(b"end") {
                    break;
                }
                if d[p] != b'/' {
                    p += 1;
                    continue;
                }
                let name = read_token(d, &mut p);
                let name = String::from_utf8_lossy(&name[1..]).into_owned();
                let Some(b) = blob(d, &mut p) else { break };
                self.charstrings.insert(name, decode(&b));
                // "ND", "|-" or "noaccess def".
                let _ = read_token(d, &mut p);
            }
        }
    }

    pub fn has_glyph(&self, name: &str) -> bool {
        self.charstrings.contains_key(name)
    }

    /// The outline of glyph `name` in glyph space (before `matrix`).
    pub fn outline(&self, name: &str) -> Option<Path> {
        let cs = self.charstrings.get(name)?;
        let mut st = State::default();
        let mut path = Path::default();
        self.run(cs, &mut st, &mut path, 0);
        if let Some(seac) = st.seac.take() {
            let (asb, adx, ady, bchar, achar) = seac;
            let mut out = Path::default();
            let base = STANDARD[bchar as usize & 255];
            let accent = STANDARD[achar as usize & 255];
            if let Some(b) = self.charstrings.get(base) {
                let mut s = State::default();
                self.run(b, &mut s, &mut out, 0);
            }
            if let Some(a) = self.charstrings.get(accent) {
                let mut s = State::default();
                let mut ap = Path::default();
                self.run(a, &mut s, &mut ap, 0);
                let (dx, dy) = (adx - asb, ady);
                for seg in ap.segs {
                    out.segs.push(match seg {
                        Seg::Move(x, y) => Seg::Move(x + dx, y + dy),
                        Seg::Line(x, y) => Seg::Line(x + dx, y + dy),
                        Seg::Cubic(a, b, c, d, e, f) => Seg::Cubic(a + dx, b + dy, c + dx, d + dy, e + dx, f + dy),
                        Seg::Close => Seg::Close,
                    });
                }
            }
            return Some(out);
        }
        Some(path)
    }

    fn run(&self, cs: &[u8], st: &mut State, path: &mut Path, depth: u32) -> bool {
        if depth > 10 {
            return false;
        }
        let mut i = 0;
        while i < cs.len() {
            let v = cs[i];
            i += 1;
            match v {
                32..=246 => st.push(v as f64 - 139.0),
                247..=250 => {
                    let w = *cs.get(i).unwrap_or(&0) as f64;
                    i += 1;
                    st.push((v as f64 - 247.0) * 256.0 + w + 108.0);
                }
                251..=254 => {
                    let w = *cs.get(i).unwrap_or(&0) as f64;
                    i += 1;
                    st.push(-(v as f64 - 251.0) * 256.0 - w - 108.0);
                }
                255 => {
                    let b: [u8; 4] = cs.get(i..i + 4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]);
                    i += 4;
                    st.push(i32::from_be_bytes(b) as f64);
                }
                1 | 3 => st.stack.clear(),
                4 => {
                    let dy = st.arg(0);
                    st.move_by(path, 0.0, dy);
                }
                5 => {
                    let (dx, dy) = (st.arg(0), st.arg(1));
                    st.line_by(path, dx, dy);
                }
                6 => {
                    let dx = st.arg(0);
                    st.line_by(path, dx, 0.0);
                }
                7 => {
                    let dy = st.arg(0);
                    st.line_by(path, 0.0, dy);
                }
                8 => {
                    let a: [f64; 6] = core::array::from_fn(|k| st.arg(k));
                    st.curve_by(path, a);
                }
                9 => {
                    path.segs.push(Seg::Close);
                    st.stack.clear();
                }
                10 => {
                    let n = st.stack.pop().unwrap_or(-1.0) as i64;
                    if let Some(sub) = usize::try_from(n).ok().and_then(|n| self.subrs.get(n)) {
                        if !self.run(sub, st, path, depth + 1) {
                            return false;
                        }
                    }
                }
                11 => return true,
                12 => {
                    let e = *cs.get(i).unwrap_or(&0);
                    i += 1;
                    match e {
                        6 => {
                            st.seac = Some((st.arg(0), st.arg(1), st.arg(2), st.arg(3) as i64, st.arg(4) as i64));
                            return false;
                        }
                        7 => {
                            st.x = st.arg(0);
                            st.y = st.arg(1);
                            st.stack.clear();
                        }
                        12 => {
                            let b = st.stack.pop().unwrap_or(1.0);
                            let a = st.stack.pop().unwrap_or(0.0);
                            st.stack.push(if b != 0.0 { a / b } else { 0.0 });
                        }
                        16 => {
                            let other = st.stack.pop().unwrap_or(0.0) as i64;
                            let n = (st.stack.pop().unwrap_or(0.0).max(0.0) as usize).min(st.stack.len());
                            let args: Vec<f64> = (0..n).map(|_| st.stack.pop().unwrap_or(0.0)).collect();
                            match other {
                                1 => {
                                    st.flex = true;
                                    st.flex_pts.clear();
                                }
                                2 => st.flex_pts.push((st.x, st.y)),
                                0 => {
                                    st.flex = false;
                                    let p = &st.flex_pts;
                                    if p.len() >= 7 {
                                        path.segs.push(Seg::Cubic(p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1));
                                        path.segs.push(Seg::Cubic(p[4].0, p[4].1, p[5].0, p[5].1, p[6].0, p[6].1));
                                        st.x = p[6].0;
                                        st.y = p[6].1;
                                    }
                                    st.ps.push(st.y);
                                    st.ps.push(st.x);
                                }
                                _ => {
                                    // Hint replacement (3) and the rest:
                                    // hand the arguments back to "pop".
                                    st.ps.extend(args.iter());
                                }
                            }
                        }
                        17 => {
                            let v = st.ps.pop().unwrap_or(0.0);
                            st.stack.push(v);
                        }
                        33 => {
                            st.x = st.arg(0);
                            st.y = st.arg(1);
                            st.stack.clear();
                        }
                        _ => st.stack.clear(),
                    }
                }
                13 => {
                    st.x = st.arg(0);
                    st.y = 0.0;
                    st.stack.clear();
                }
                14 => {
                    if st.open {
                        path.segs.push(Seg::Close);
                    }
                    return false;
                }
                21 => {
                    let (dx, dy) = (st.arg(0), st.arg(1));
                    st.move_by(path, dx, dy);
                }
                22 => {
                    let dx = st.arg(0);
                    st.move_by(path, dx, 0.0);
                }
                30 => {
                    let (a, b, c, d) = (st.arg(0), st.arg(1), st.arg(2), st.arg(3));
                    st.curve_by(path, [0.0, a, b, c, d, 0.0]);
                }
                31 => {
                    let (a, b, c, d) = (st.arg(0), st.arg(1), st.arg(2), st.arg(3));
                    st.curve_by(path, [a, 0.0, b, c, 0.0, d]);
                }
                _ => st.stack.clear(),
            }
        }
        true
    }
}

#[derive(Default)]
struct State {
    stack: Vec<f64>,
    /// The PostScript operand stack between callothersubr and pop.
    ps: Vec<f64>,
    x: f64,
    y: f64,
    open: bool,
    flex: bool,
    flex_pts: Vec<(f64, f64)>,
    seac: Option<(f64, f64, f64, i64, i64)>,
}

impl State {
    fn push(&mut self, v: f64) {
        if self.stack.len() < 64 {
            self.stack.push(v);
        }
    }

    /// Operand `i` from the bottom; clears the stack after the last read
    /// by the caller through the move/line/curve helpers.
    fn arg(&self, i: usize) -> f64 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn move_by(&mut self, path: &mut Path, dx: f64, dy: f64) {
        self.stack.clear();
        self.x += dx;
        self.y += dy;
        if self.flex {
            return;
        }
        if self.open {
            path.segs.push(Seg::Close);
        }
        path.segs.push(Seg::Move(self.x, self.y));
        self.open = true;
    }

    fn line_by(&mut self, path: &mut Path, dx: f64, dy: f64) {
        self.stack.clear();
        self.x += dx;
        self.y += dy;
        path.segs.push(Seg::Line(self.x, self.y));
    }

    fn curve_by(&mut self, path: &mut Path, a: [f64; 6]) {
        self.stack.clear();
        let (x1, y1) = (self.x + a[0], self.y + a[1]);
        let (x2, y2) = (x1 + a[2], y1 + a[3]);
        let (x3, y3) = (x2 + a[4], y2 + a[5]);
        path.segs.push(Seg::Cubic(x1, y1, x2, y2, x3, y3));
        self.x = x3;
        self.y = y3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eexec_round_trip() {
        // Encrypt with the same recurrence and check decryption.
        let plain = b"abcdHello";
        let mut r: u16 = 55665;
        let enc: Vec<u8> = plain
            .iter()
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
                c
            })
            .collect();
        assert_eq!(decrypt(&enc, 55665, 4), b"Hello");
    }

    #[test]
    fn charstring_square() {
        // hsbw 0 500; 100 100 rmoveto; 200 hlineto; 200 vlineto; -200 hlineto; closepath; endchar
        let n = |v: i32| -> Vec<u8> {
            if (-107..=107).contains(&v) {
                alloc::vec![(v + 139) as u8]
            } else if (108..=1131).contains(&v) {
                let w = v - 108;
                alloc::vec![(w / 256 + 247) as u8, (w % 256) as u8]
            } else {
                let w = -v - 108;
                alloc::vec![(w / 256 + 251) as u8, (w % 256) as u8]
            }
        };
        let mut cs = Vec::new();
        cs.extend(n(0));
        cs.extend(n(500));
        cs.push(13);
        cs.extend(n(100));
        cs.extend(n(100));
        cs.push(21);
        cs.extend(n(200));
        cs.push(6);
        cs.extend(n(200));
        cs.push(7);
        cs.extend(n(-200));
        cs.push(6);
        cs.push(9);
        cs.push(14);
        let mut t = Type1 {
            encoding: BTreeMap::new(),
            matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            subrs: Vec::new(),
            charstrings: BTreeMap::new(),
        };
        t.charstrings.insert(String::from("sq"), cs);
        let p = t.outline("sq").unwrap();
        assert_eq!(p.segs[0], Seg::Move(100.0, 100.0));
        assert_eq!(p.segs[1], Seg::Line(300.0, 100.0));
        assert_eq!(p.segs[2], Seg::Line(300.0, 300.0));
        assert_eq!(p.segs[3], Seg::Line(100.0, 300.0));
    }
}
