//! WebP lossless (VP8L, RFC 9649 section 3): prefix-coded ARGB with LZ77
//! back references and a colour cache, meta prefix codes, and the
//! predictor, colour, subtract-green and colour-indexing transforms.

use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, &'static str>;

/// Largest picture, in pixels.
const MAX_PIXELS: usize = 1 << 26;

const DISTANCE_MAP: [(i8, i8); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

const CODE_LENGTH_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Bits, least significant first.
pub struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
    /// Bits asked for past the end.
    over: bool,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0, buf: 0, n: 0, over: false }
    }

    #[inline]
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => {
                    if self.n == 0 {
                        self.over = true;
                    }
                    0
                }
            };
            self.pos += 1;
            self.buf |= (b as u64) << self.n;
            self.n += 8;
        }
    }

    #[inline]
    fn peek(&mut self, n: u32) -> u32 {
        if self.n < n {
            self.fill();
        }
        (self.buf & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    fn skip(&mut self, n: u32) {
        self.buf >>= n;
        self.n -= n;
        if self.pos > self.data.len() + 8 {
            self.over = true;
        }
    }

    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    fn check(&self) -> R<()> {
        if self.over {
            Err("WebP lossless data ends early")
        } else {
            Ok(())
        }
    }
}

/// A canonical prefix code.
struct Code {
    /// Fast table over the next 8 bits: symbol << 4 | length, or 0 for a
    /// longer code.
    fast: Vec<u32>,
    /// Codes per length, and symbols sorted by code.
    counts: [u16; 16],
    symbols: Vec<u16>,
    /// The only symbol, for codes that take no bits.
    single: Option<u16>,
}

impl Code {
    fn new(lengths: &[u8]) -> R<Code> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let used: Vec<usize> = (0..lengths.len()).filter(|&s| lengths[s] > 0).collect();
        if used.len() <= 1 {
            return Ok(Code {
                fast: Vec::new(),
                counts,
                symbols: Vec::new(),
                single: Some(used.first().copied().unwrap_or(0) as u16),
            });
        }
        // A complete code is required.
        let mut left = 1i32;
        for &c in &counts[1..] {
            left = left * 2 - c as i32;
            if left < 0 {
                return Err("bad WebP prefix code");
            }
        }
        if left != 0 {
            return Err("incomplete WebP prefix code");
        }
        let mut offs = [0usize; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + counts[l] as usize;
        }
        let mut symbols = vec![0u16; used.len()];
        for &s in &used {
            let l = lengths[s] as usize;
            symbols[offs[l]] = s as u16;
            offs[l] += 1;
        }
        // Fast table: walk the codes in canonical order.
        let mut fast = vec![0u32; 256];
        let mut code = 0u32;
        let mut k = 0usize;
        for len in 1..=15u32 {
            for _ in 0..counts[len as usize] {
                if len <= 8 {
                    let rev = code.reverse_bits() >> (32 - len);
                    let mut i = rev;
                    while i < 256 {
                        fast[i as usize] = (symbols[k] as u32) << 4 | len;
                        i += 1 << len;
                    }
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Ok(Code { fast, counts, symbols, single: None })
    }

    #[inline]
    fn read(&self, b: &mut Bits) -> u16 {
        if let Some(s) = self.single {
            return s;
        }
        let e = self.fast[b.peek(8) as usize];
        if e != 0 {
            b.skip(e & 15);
            return (e >> 4) as u16;
        }
        // Longer codes, one bit at a time.
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.read(1) as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols[(index + code - first) as usize];
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        0
    }
}

fn read_code(b: &mut Bits, alphabet: usize) -> R<Code> {
    let mut lengths = vec![0u8; alphabet];
    if b.read(1) == 1 {
        let n = b.read(1) + 1;
        let first8 = b.read(1);
        let s0 = b.read(1 + 7 * first8) as usize;
        *lengths.get_mut(s0).ok_or("bad WebP symbol")? = 1;
        if n == 2 {
            let s1 = b.read(8) as usize;
            *lengths.get_mut(s1).ok_or("bad WebP symbol")? = 1;
        }
    } else {
        let mut cl = [0u8; 19];
        let n = 4 + b.read(4) as usize;
        for &i in CODE_LENGTH_ORDER.iter().take(n) {
            cl[i] = b.read(3) as u8;
        }
        let clc = Code::new(&cl)?;
        let mut max_symbol = if b.read(1) == 1 {
            let nbits = 2 + 2 * b.read(3);
            let m = 2 + b.read(nbits) as usize;
            if m > alphabet {
                return Err("bad WebP code length count");
            }
            m
        } else {
            alphabet
        };
        let mut prev = 8u8;
        let mut s = 0;
        while s < alphabet {
            if max_symbol == 0 {
                break;
            }
            max_symbol -= 1;
            b.check()?;
            let c = clc.read(b);
            match c {
                0..=15 => {
                    lengths[s] = c as u8;
                    s += 1;
                    if c != 0 {
                        prev = c as u8;
                    }
                }
                16..=18 => {
                    let (n, v) = match c {
                        16 => (3 + b.read(2) as usize, prev),
                        17 => (3 + b.read(3) as usize, 0),
                        _ => (11 + b.read(7) as usize, 0),
                    };
                    if s + n > alphabet {
                        return Err("bad WebP code lengths");
                    }
                    lengths[s..s + n].fill(v);
                    s += n;
                }
                _ => return Err("bad WebP code length symbol"),
            }
        }
    }
    b.check()?;
    Code::new(&lengths)
}

/// The five codes for one block of the picture.
struct Group {
    codes: [Code; 5],
}

fn read_group(b: &mut Bits, cache_size: usize) -> R<Group> {
    Ok(Group {
        codes: [
            read_code(b, 256 + 24 + cache_size)?,
            read_code(b, 256)?,
            read_code(b, 256)?,
            read_code(b, 256)?,
            read_code(b, 40)?,
        ],
    })
}

/// LZ77 length and distance prefix codes (3.5.2.2).
fn prefix_value(b: &mut Bits, sym: u32) -> usize {
    if sym < 4 {
        return sym as usize + 1;
    }
    let extra = (sym - 2) >> 1;
    let offset = (2 + (sym & 1)) << extra;
    (offset + b.read(extra)) as usize + 1
}

fn div_round_up(a: usize, bits: u32) -> usize {
    (a + (1 << bits) - 1) >> bits
}

/// An entropy-coded image of `w` by `h`; `main` allows meta prefix codes.
fn decode_image(b: &mut Bits, w: usize, h: usize, main: bool) -> R<Vec<u32>> {
    if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err("WebP picture too large");
    }
    let cache_bits = if b.read(1) == 1 {
        let c = b.read(4);
        if !(1..=11).contains(&c) {
            return Err("bad WebP colour cache size");
        }
        c
    } else {
        0
    };
    let cache_size = if cache_bits > 0 { 1usize << cache_bits } else { 0 };
    let (meta_bits, meta): (u32, Vec<u32>) = if main && b.read(1) == 1 {
        let bits = b.read(3) + 2;
        let img = decode_image(b, div_round_up(w, bits), div_round_up(h, bits), false)?;
        (bits, img)
    } else {
        (0, Vec::new())
    };
    let ngroups = meta.iter().map(|&p| ((p >> 8) & 0xFFFF) as usize + 1).max().unwrap_or(1);
    if ngroups > 1 << 16 {
        return Err("too many WebP prefix code groups");
    }
    let mut groups = Vec::with_capacity(ngroups);
    for _ in 0..ngroups {
        groups.push(read_group(b, cache_size)?);
    }
    let meta_w = if meta_bits > 0 { div_round_up(w, meta_bits) } else { 1 };
    let mut cache = vec![0u32; cache_size];
    let mut out = vec![0u32; w * h];
    let mut i = 0usize;
    let mask = if meta_bits > 0 { (1usize << meta_bits) - 1 } else { usize::MAX };
    let mut g = &groups[0];
    // Pixels already inserted into the colour cache.
    let mut cached = 0usize;
    let hash = |p: u32| ((p.wrapping_mul(0x1E35_A7BD)) >> (32 - cache_bits)) as usize;
    while i < w * h {
        let (x, y) = (i % w, i / w);
        if meta_bits > 0 && (x & mask) == 0 {
            let m = meta[(y >> meta_bits) * meta_w + (x >> meta_bits)];
            g = &groups[((m >> 8) & 0xFFFF) as usize];
        }
        b.check()?;
        let s = g.codes[0].read(b) as u32;
        if s < 256 {
            let r = g.codes[1].read(b) as u32;
            let bl = g.codes[2].read(b) as u32;
            let a = g.codes[3].read(b) as u32;
            out[i] = a << 24 | r << 16 | s << 8 | bl;
            i += 1;
        } else if s < 256 + 24 {
            let len = prefix_value(b, s - 256);
            let dsym = g.codes[4].read(b) as u32;
            let dcode = prefix_value(b, dsym);
            let dist = if dcode > 120 {
                dcode - 120
            } else {
                let (dx, dy) = DISTANCE_MAP[dcode - 1];
                (dx as isize + dy as isize * w as isize).max(1) as usize
            };
            if dist > i || i + len > w * h {
                return Err("bad WebP back reference");
            }
            for k in 0..len {
                out[i + k] = out[i + k - dist];
            }
            // Meta codes may change inside a copy; the next literal picks
            // the right group again.
            if meta_bits > 0 {
                let (x, y) = ((i + len) % w, (i + len) / w);
                if i + len < w * h {
                    let m = meta[(y >> meta_bits) * meta_w + (x >> meta_bits)];
                    g = &groups[((m >> 8) & 0xFFFF) as usize];
                }
            }
            i += len;
            if cache_bits > 0 {
                while cached < i {
                    cache[hash(out[cached])] = out[cached];
                    cached += 1;
                }
            }
            continue;
        } else {
            let k = (s - 280) as usize;
            if k >= cache_size {
                return Err("bad WebP colour cache index");
            }
            while cached < i {
                cache[hash(out[cached])] = out[cached];
                cached += 1;
            }
            out[i] = cache[k];
            i += 1;
        }
        if cache_bits > 0 {
            while cached < i {
                cache[hash(out[cached])] = out[cached];
                cached += 1;
            }
        }
    }
    b.check()?;
    Ok(out)
}

enum Transform {
    Predictor { bits: u32, data: Vec<u32> },
    Color { bits: u32, data: Vec<u32> },
    SubtractGreen,
    Indexing { bits: u32, table: Vec<u32> },
}

#[inline]
fn add(a: u32, b: u32) -> u32 {
    let ag = (a & 0xFF00_FF00).wrapping_add(b & 0xFF00_FF00) & 0xFF00_FF00;
    let rb = (a & 0x00FF_00FF).wrapping_add(b & 0x00FF_00FF) & 0x00FF_00FF;
    ag | rb
}

#[inline]
fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xFEFE_FEFE) >> 1).wrapping_add(a & b)
}

#[inline]
fn ch(p: u32, k: u32) -> i32 {
    ((p >> k) & 0xFF) as i32
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    let mut pl = 0;
    let mut pt = 0;
    for k in [24, 16, 8, 0] {
        let p = ch(l, k) + ch(t, k) - ch(tl, k);
        pl += (p - ch(l, k)).abs();
        pt += (p - ch(t, k)).abs();
    }
    if pl < pt {
        l
    } else {
        t
    }
}

fn clamp_full(a: u32, b: u32, c: u32) -> u32 {
    let mut o = 0;
    for k in [24, 16, 8, 0] {
        o |= ((ch(a, k) + ch(b, k) - ch(c, k)).clamp(0, 255) as u32) << k;
    }
    o
}

fn clamp_half(a: u32, b: u32) -> u32 {
    let mut o = 0;
    for k in [24, 16, 8, 0] {
        let (x, y) = (ch(a, k), ch(b, k));
        o |= ((x + (x - y) / 2).clamp(0, 255) as u32) << k;
    }
    o
}

fn predict(mode: u32, l: u32, t: u32, tr: u32, tl: u32) -> u32 {
    match mode {
        0 => 0xFF00_0000,
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => avg2(avg2(l, tr), t),
        6 => avg2(l, tl),
        7 => avg2(l, t),
        8 => avg2(tl, t),
        9 => avg2(t, tr),
        10 => avg2(avg2(l, tl), avg2(t, tr)),
        11 => select(l, t, tl),
        12 => clamp_full(l, t, tl),
        13 => clamp_half(avg2(l, t), tl),
        // Modes 14 and 15 decode as black, as libwebp does.
        _ => 0xFF00_0000,
    }
}

#[inline]
fn delta(t: u32, c: u32) -> i32 {
    ((t as u8 as i8 as i32) * (c as u8 as i8 as i32)) >> 5
}

fn inverse(t: &Transform, px: &mut Vec<u32>, w: usize, h: usize) -> R<()> {
    match t {
        Transform::SubtractGreen => {
            for p in px.iter_mut() {
                let g = (*p >> 8) & 0xFF;
                *p = (*p & 0xFF00_FF00) | ((*p & 0x00FF_00FF).wrapping_add(g << 16 | g) & 0x00FF_00FF);
            }
        }
        Transform::Predictor { bits, data } => {
            let tw = div_round_up(w, *bits);
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let pred = if y == 0 {
                        if x == 0 {
                            0xFF00_0000
                        } else {
                            px[i - 1]
                        }
                    } else if x == 0 {
                        px[i - w]
                    } else {
                        let mode = (data[(y >> bits) * tw + (x >> bits)] >> 8) & 0xF;
                        // On the rightmost column this is the first pixel
                        // of the current row, as the format wants.
                        let tr = px[i - w + 1];
                        predict(mode, px[i - 1], px[i - w], tr, px[i - w - 1])
                    };
                    px[i] = add(px[i], pred);
                }
            }
        }
        Transform::Color { bits, data } => {
            let tw = div_round_up(w, *bits);
            for y in 0..h {
                for x in 0..w {
                    let e = data[(y >> bits) * tw + (x >> bits)];
                    let (g2r, g2b, r2b) = (e & 0xFF, (e >> 8) & 0xFF, (e >> 16) & 0xFF);
                    let p = px[y * w + x];
                    let g = (p >> 8) & 0xFF;
                    let r = ((p >> 16) as i32 + delta(g2r, g)) as u32 & 0xFF;
                    let bl = ((p as i32) + delta(g2b, g) + delta(r2b, r)) as u32 & 0xFF;
                    px[y * w + x] = (p & 0xFF00_FF00) | r << 16 | bl;
                }
            }
        }
        Transform::Indexing { bits, table } => {
            // `px` holds the packed indices, `div_round_up(w, bits)` wide.
            let pw = div_round_up(w, *bits);
            let per = 1usize << bits;
            let nb = 8 >> bits;
            let mut out = vec![0u32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let packed = (px[y * pw + x / per] >> 8) & 0xFF;
                    let idx = if *bits == 0 { packed } else { (packed >> ((x % per) * nb)) & ((1 << nb) - 1) };
                    out[y * w + x] = table.get(idx as usize).copied().unwrap_or(0);
                }
            }
            *px = out;
        }
    }
    Ok(())
}

/// Decodes an image stream (transforms and the ARGB image) of `w` by `h`.
pub fn decode_stream(b: &mut Bits, w: usize, h: usize) -> R<Vec<u32>> {
    let mut transforms = Vec::new();
    let mut cw = w;
    let mut seen = 0u8;
    while b.read(1) == 1 {
        let kind = b.read(2);
        if seen & (1 << kind) != 0 {
            return Err("repeated WebP transform");
        }
        seen |= 1 << kind;
        let t = match kind {
            0 | 1 => {
                let bits = b.read(3) + 2;
                let data = decode_image(b, div_round_up(cw, bits), div_round_up(h, bits), false)?;
                if kind == 0 {
                    Transform::Predictor { bits, data }
                } else {
                    Transform::Color { bits, data }
                }
            }
            2 => Transform::SubtractGreen,
            _ => {
                let n = b.read(8) as usize + 1;
                let mut table = decode_image(b, n, 1, false)?;
                for i in 1..n {
                    table[i] = add(table[i], table[i - 1]);
                }
                let bits = match n {
                    0..=2 => 3,
                    3..=4 => 2,
                    5..=16 => 1,
                    _ => 0,
                };
                cw = div_round_up(cw, bits);
                Transform::Indexing { bits, table }
            }
        };
        transforms.push((t, cw));
    }
    let mut px = decode_image(b, cw, h, true)?;
    // Undo in reverse order; each works at the width before it.
    for (k, (t, _)) in transforms.iter().enumerate().rev() {
        let width = if k == 0 { w } else { transforms[k - 1].1 };
        inverse(t, &mut px, width, h)?;
    }
    Ok(px)
}

/// A VP8L chunk: its header, then the image stream. Returns (w, h, alpha
/// used, ARGB).
pub fn decode(data: &[u8]) -> R<(usize, usize, bool, Vec<u32>)> {
    if data.first() != Some(&0x2F) {
        return Err("bad WebP lossless signature");
    }
    let mut b = Bits::new(&data[1..]);
    let w = b.read(14) as usize + 1;
    let h = b.read(14) as usize + 1;
    let alpha = b.read(1) == 1;
    if b.read(3) != 0 {
        return Err("unsupported WebP lossless version");
    }
    let px = decode_stream(&mut b, w, h)?;
    Ok((w, h, alpha, px))
}
