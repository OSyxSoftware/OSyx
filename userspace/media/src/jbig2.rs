//! JBIG2 bi-level images (ITU-T T.88): files and the embedded streams of
//! PDF's JBIG2Decode. Generic regions (arithmetic with templates 0 to 3,
//! typical prediction and adaptive pixels, or MMR), generic refinement,
//! symbol dictionaries and text regions (arithmetic or Huffman, with
//! refinement and aggregation, context reuse), pattern dictionaries and
//! halftone regions, standard and custom Huffman tables, and page
//! composition with all combination operators and striping.

use crate::bits::MsbReader;
use crate::mq::{Cx, Mq};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, &'static str>;

/// Largest bitmap, in pixels.
const MAX_PIXELS: usize = 1 << 27;

/// A bi-level bitmap, one byte per pixel: 1 is black.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Bitmap {
    pub fn new(w: usize, h: usize, value: u8) -> R<Bitmap> {
        if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
            return Err("JBIG2 bitmap too large");
        }
        Ok(Bitmap { w, h, px: vec![value; w * h] })
    }

    #[inline]
    fn get(&self, x: isize, y: isize) -> u8 {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            0
        } else {
            self.px[y as usize * self.w + x as usize]
        }
    }

    /// Combines `src` into this bitmap with its top left at (x, y).
    fn compose(&mut self, src: &Bitmap, x: i64, y: i64, op: u8) {
        for sy in 0..src.h {
            let ty = y + sy as i64;
            if ty < 0 || ty >= self.h as i64 {
                continue;
            }
            let x0 = x.max(0);
            let x1 = (x + src.w as i64).min(self.w as i64);
            if x0 >= x1 {
                continue;
            }
            let dst = &mut self.px[ty as usize * self.w..][x0 as usize..x1 as usize];
            let s = &src.px[sy * src.w + (x0 - x) as usize..][..dst.len()];
            match op {
                0 => dst.iter_mut().zip(s).for_each(|(d, &v)| *d |= v),
                1 => dst.iter_mut().zip(s).for_each(|(d, &v)| *d &= v),
                2 => dst.iter_mut().zip(s).for_each(|(d, &v)| *d ^= v),
                3 => dst.iter_mut().zip(s).for_each(|(d, &v)| *d = 1 ^ (*d ^ v)),
                _ => dst.copy_from_slice(s),
            }
        }
    }

    fn sub(&self, x: usize, y: usize, w: usize, h: usize) -> Bitmap {
        let mut b = Bitmap { w, h, px: vec![0; w * h] };
        for r in 0..h {
            for c in 0..w {
                b.px[r * w + c] = self.get((x + c) as isize, (y + r) as isize);
            }
        }
        b
    }

    /// Grows the height, filling new rows with `value`.
    fn grow(&mut self, h: usize, value: u8) -> R<()> {
        if h > self.h {
            if self.w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
                return Err("JBIG2 page too large");
            }
            self.px.resize(self.w * h, value);
            self.h = h;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Arithmetic integer decoding (Annex A).

struct IntCx([Cx; 512]);

impl IntCx {
    fn new() -> IntCx {
        IntCx([0; 512])
    }

    /// A.2; None is OOB.
    fn decode(&mut self, mq: &mut Mq) -> Option<i32> {
        let mut prev = 1usize;
        let mut bits = |n: u32, cx: &mut [Cx; 512]| -> u32 {
            let mut v = 0u32;
            for _ in 0..n {
                let b = mq.bit(&mut cx[prev]);
                prev = if prev < 256 { (prev << 1) | b as usize } else { (((prev << 1) | b as usize) & 511) | 256 };
                v = (v << 1) | b;
            }
            v
        };
        let cx = &mut self.0;
        let sign = bits(1, cx);
        let v: i64 = if bits(1, cx) == 0 {
            bits(2, cx) as i64
        } else if bits(1, cx) == 0 {
            bits(4, cx) as i64 + 4
        } else if bits(1, cx) == 0 {
            bits(6, cx) as i64 + 20
        } else if bits(1, cx) == 0 {
            bits(8, cx) as i64 + 84
        } else if bits(1, cx) == 0 {
            bits(12, cx) as i64 + 340
        } else {
            bits(32, cx) as i64 + 4436
        };
        if sign == 1 && v == 0 {
            return None;
        }
        let v = if sign == 1 { -v } else { v };
        i32::try_from(v).ok()
    }
}

/// A.3: symbol IDs of `len` bits.
struct IdCx {
    len: u32,
    cx: Vec<Cx>,
}

impl IdCx {
    fn new(len: u32) -> R<IdCx> {
        if len > 24 {
            return Err("too many JBIG2 symbols");
        }
        Ok(IdCx { len, cx: vec![0; 1 << (len + 1)] })
    }

    fn decode(&mut self, mq: &mut Mq) -> u32 {
        let mut prev = 1usize;
        for _ in 0..self.len {
            let b = mq.bit(&mut self.cx[prev]);
            prev = (prev << 1) | b as usize;
        }
        (prev - (1 << self.len)) as u32
    }
}

/// The statistics of one decoding run (a region or a dictionary).
struct Stats {
    gb: Vec<Cx>,
    gr: Vec<Cx>,
    iadh: IntCx,
    iadw: IntCx,
    iaex: IntCx,
    iaai: IntCx,
    iadt: IntCx,
    iafs: IntCx,
    iads: IntCx,
    iait: IntCx,
    iari: IntCx,
    iardw: IntCx,
    iardh: IntCx,
    iardx: IntCx,
    iardy: IntCx,
}

impl Stats {
    fn new() -> Stats {
        Stats {
            gb: vec![0; 1 << 16],
            gr: vec![0; 1 << 13],
            iadh: IntCx::new(),
            iadw: IntCx::new(),
            iaex: IntCx::new(),
            iaai: IntCx::new(),
            iadt: IntCx::new(),
            iafs: IntCx::new(),
            iads: IntCx::new(),
            iait: IntCx::new(),
            iari: IntCx::new(),
            iardw: IntCx::new(),
            iardh: IntCx::new(),
            iardx: IntCx::new(),
            iardy: IntCx::new(),
        }
    }
}

fn ceil_log2(n: usize) -> u32 {
    if n <= 1 {
        0
    } else {
        usize::BITS - (n - 1).leading_zeros()
    }
}

// ---------------------------------------------------------------------------
// Generic region decoding (6.2).

type At = (i8, i8);

/// A context pixel: (dx, dy, n), where n > 0 takes adaptive pixel n instead.
type Tap = (i8, i8, u8);

/// Context pixels, most significant first.
const TEMPLATES: [&[Tap]; 4] = [
    &[
        (0, 0, 4),
        (-1, -2, 0),
        (0, -2, 0),
        (1, -2, 0),
        (0, 0, 3),
        (0, 0, 2),
        (-2, -1, 0),
        (-1, -1, 0),
        (0, -1, 0),
        (1, -1, 0),
        (2, -1, 0),
        (0, 0, 1),
        (-4, 0, 0),
        (-3, 0, 0),
        (-2, 0, 0),
        (-1, 0, 0),
    ],
    &[
        (-1, -2, 0),
        (0, -2, 0),
        (1, -2, 0),
        (2, -2, 0),
        (-2, -1, 0),
        (-1, -1, 0),
        (0, -1, 0),
        (1, -1, 0),
        (2, -1, 0),
        (0, 0, 1),
        (-3, 0, 0),
        (-2, 0, 0),
        (-1, 0, 0),
    ],
    &[
        (-1, -2, 0),
        (0, -2, 0),
        (1, -2, 0),
        (-2, -1, 0),
        (-1, -1, 0),
        (0, -1, 0),
        (1, -1, 0),
        (0, 0, 1),
        (-2, 0, 0),
        (-1, 0, 0),
    ],
    &[
        (-3, -1, 0),
        (-2, -1, 0),
        (-1, -1, 0),
        (0, -1, 0),
        (1, -1, 0),
        (0, 0, 1),
        (-4, 0, 0),
        (-3, 0, 0),
        (-2, 0, 0),
        (-1, 0, 0),
    ],
];

/// The contexts of the SLTP pseudo-pixel (Figures 8 to 11).
const SLTP: [usize; 4] = [0x9B25, 0x0795, 0x00E5, 0x0195];

struct GenericParams<'a> {
    w: usize,
    h: usize,
    template: u8,
    tpgdon: bool,
    at: &'a [At],
    skip: Option<&'a Bitmap>,
}

fn decode_generic(mq: &mut Mq, gb: &mut [Cx], p: &GenericParams) -> R<Bitmap> {
    let mut b = Bitmap::new(p.w, p.h, 0)?;
    let tpl: Vec<(isize, isize)> = TEMPLATES[p.template as usize & 3]
        .iter()
        .map(|&(x, y, a)| {
            if a == 0 {
                (x as isize, y as isize)
            } else {
                let (ax, ay) = p.at.get(a as usize - 1).copied().unwrap_or((0, 0));
                (ax as isize, ay as isize)
            }
        })
        .collect();
    let n = tpl.len();
    // Whether every context pixel of a column lies inside the bitmap, so
    // rows can be read without bounds checks.
    let min_x = tpl.iter().map(|t| t.0).min().unwrap_or(0).min(0);
    let max_x = tpl.iter().map(|t| t.0).max().unwrap_or(0).max(0);
    let min_y = tpl.iter().map(|t| t.1).min().unwrap_or(0).min(0);
    let w = p.w as isize;
    let mut ltp = false;
    for y in 0..p.h {
        if p.tpgdon {
            if mq.bit(&mut gb[SLTP[p.template as usize & 3]]) == 1 {
                ltp = !ltp;
            }
            if ltp {
                if y > 0 {
                    let (a, b2) = b.px.split_at_mut(y * p.w);
                    b2[..p.w].copy_from_slice(&a[(y - 1) * p.w..]);
                }
                continue;
            }
        }
        let yi = y as isize;
        for x in 0..p.w {
            if let Some(s) = p.skip {
                if s.px[y * s.w + x] != 0 {
                    continue;
                }
            }
            let xi = x as isize;
            let mut ctx = 0usize;
            if xi + min_x >= 0 && xi + max_x < w && yi + min_y >= 0 {
                for &(dx, dy) in &tpl {
                    let v = b.px[((yi + dy) * w + xi + dx) as usize];
                    ctx = (ctx << 1) | v as usize;
                }
            } else {
                for &(dx, dy) in &tpl {
                    ctx = (ctx << 1) | b.get(xi + dx, yi + dy) as usize;
                }
            }
            debug_assert!(ctx < 1 << n);
            b.px[y * p.w + x] = mq.bit(&mut gb[ctx]) as u8;
        }
    }
    Ok(b)
}

fn decode_mmr_bitmap(data: &[u8], w: usize, h: usize) -> R<(Bitmap, usize)> {
    let mut b = Bitmap::new(w, h, 0)?;
    let (rows, used) = crate::pdf::ccitt::decode_mmr(data, w, h);
    let rb = w.div_ceil(8);
    for y in 0..h {
        for x in 0..w {
            if rows.get(y * rb + x / 8).is_some_and(|&v| v & (0x80 >> (x % 8)) != 0) {
                b.px[y * w + x] = 1;
            }
        }
    }
    Ok((b, used))
}

// ---------------------------------------------------------------------------
// Generic refinement (6.3).

/// Reference pixels (dx, dy, adaptive), most significant first, then the
/// decoded-bitmap pixels.
const REF_TEMPLATES: [(&[Tap], &[Tap]); 2] = [
    (
        &[(0, 0, 2), (0, -1, 0), (1, -1, 0), (-1, 0, 0), (0, 0, 0), (1, 0, 0), (-1, 1, 0), (0, 1, 0), (1, 1, 0)],
        &[(0, 0, 1), (0, -1, 0), (1, -1, 0), (-1, 0, 0)],
    ),
    (
        &[(0, -1, 0), (-1, 0, 0), (0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0)],
        &[(-1, -1, 0), (0, -1, 0), (1, -1, 0), (-1, 0, 0)],
    ),
];

/// The SLTP contexts: only the reference pixel under the current one set.
const REF_SLTP: [usize; 2] = [0x100, 0x080];

struct RefineParams<'a> {
    w: usize,
    h: usize,
    template: u8,
    reference: &'a Bitmap,
    dx: i32,
    dy: i32,
    tpgron: bool,
    at: [At; 2],
}

fn decode_refinement(mq: &mut Mq, gr: &mut [Cx], p: &RefineParams) -> R<Bitmap> {
    let mut b = Bitmap::new(p.w, p.h, 0)?;
    let t = (p.template & 1) as usize;
    let pick = |&(x, y, a): &Tap| -> (isize, isize) {
        if a == 0 {
            (x as isize, y as isize)
        } else {
            let (ax, ay) = p.at[a as usize - 1];
            (ax as isize, ay as isize)
        }
    };
    let rt: Vec<(isize, isize)> = REF_TEMPLATES[t].0.iter().map(pick).collect();
    let ct: Vec<(isize, isize)> = REF_TEMPLATES[t].1.iter().map(pick).collect();
    let r = p.reference;
    let mut ltp = false;
    for y in 0..p.h {
        if p.tpgron && mq.bit(&mut gr[REF_SLTP[t]]) == 1 {
            ltp = !ltp;
        }
        let yi = y as isize;
        let ry = yi - p.dy as isize;
        for x in 0..p.w {
            let xi = x as isize;
            let rx = xi - p.dx as isize;
            if ltp {
                // Typical prediction: a 3 x 3 uniform neighbourhood in the
                // reference sets the pixel.
                let v = r.get(rx - 1, ry - 1);
                let uniform = (-1..=1).all(|j| (-1..=1).all(|i| r.get(rx + i, ry + j) == v));
                if uniform {
                    b.px[y * p.w + x] = v;
                    continue;
                }
            }
            let mut ctx = 0usize;
            for &(dx, dy) in &rt {
                ctx = (ctx << 1) | r.get(rx + dx, ry + dy) as usize;
            }
            for &(dx, dy) in &ct {
                ctx = (ctx << 1) | b.get(xi + dx, yi + dy) as usize;
            }
            b.px[y * p.w + x] = mq.bit(&mut gr[ctx]) as u8;
        }
    }
    Ok(b)
}

// ---------------------------------------------------------------------------
// Huffman tables (Annex B).

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    Lower,
    Upper,
    Oob,
}

#[derive(Clone, Copy)]
struct Line {
    low: i32,
    plen: u8,
    rlen: u8,
    kind: Kind,
}

pub struct Table {
    lines: Vec<Line>,
    /// (prefix length, prefix code) to line.
    codes: BTreeMap<(u8, u32), usize>,
}

impl Table {
    /// B.3: assigns the prefix codes in line order.
    fn new(lines: Vec<Line>) -> R<Table> {
        let max = lines.iter().map(|l| l.plen).max().unwrap_or(0) as usize;
        if max > 32 {
            return Err("JBIG2 Huffman code too long");
        }
        let mut count = vec![0u32; max + 2];
        for l in &lines {
            count[l.plen as usize] += 1;
        }
        count[0] = 0;
        let mut codes = BTreeMap::new();
        let mut first = 0u32;
        for len in 1..=max {
            first = (first + count[len - 1]) << 1;
            let mut code = first;
            for (i, l) in lines.iter().enumerate() {
                if l.plen as usize == len {
                    codes.insert((len as u8, code), i);
                    code += 1;
                }
            }
        }
        Ok(Table { lines, codes })
    }

    /// The next value, `None` for OOB.
    fn decode(&self, r: &mut MsbReader) -> R<Option<i32>> {
        let mut code = 0u32;
        for len in 1..=32u8 {
            code = (code << 1) | r.read(1).ok_or("JBIG2 Huffman data ends early")?;
            if let Some(&i) = self.codes.get(&(len, code)) {
                let l = self.lines[i];
                return Ok(match l.kind {
                    Kind::Oob => None,
                    Kind::Lower => Some(l.low.wrapping_sub(r.read(32).ok_or("JBIG2 data ends early")? as i32)),
                    Kind::Upper => Some(l.low.wrapping_add(r.read(32).ok_or("JBIG2 data ends early")? as i32)),
                    Kind::Normal => {
                        let off = if l.rlen == 0 { 0 } else { r.read(l.rlen as u32).ok_or("JBIG2 data ends early")? };
                        Some(l.low.wrapping_add(off as i32))
                    }
                });
            }
        }
        Err("bad JBIG2 Huffman code")
    }

    fn value(&self, r: &mut MsbReader) -> R<i32> {
        self.decode(r)?.ok_or("unexpected OOB in JBIG2 data")
    }
}

fn normal(low: i32, plen: u8, rlen: u8) -> Line {
    Line { low, plen, rlen, kind: Kind::Normal }
}

/// Standard table B.n as (low, prefix length, range length, kind);
/// prefix codes follow from B.3.
fn standard(n: u8) -> Table {
    use Kind::*;
    let l = |low: i32, plen: u8, rlen: u8, kind: Kind| Line { low, plen, rlen, kind };
    let lines: Vec<Line> = match n {
        1 => vec![l(0, 1, 4, Normal), l(16, 2, 8, Normal), l(272, 3, 16, Normal), l(65808, 3, 32, Upper)],
        2 => vec![
            l(0, 1, 0, Normal),
            l(1, 2, 0, Normal),
            l(2, 3, 0, Normal),
            l(3, 4, 3, Normal),
            l(11, 5, 6, Normal),
            l(75, 6, 32, Upper),
            l(0, 6, 0, Oob),
        ],
        3 => vec![
            l(-256, 8, 8, Normal),
            l(0, 1, 0, Normal),
            l(1, 2, 0, Normal),
            l(2, 3, 0, Normal),
            l(3, 4, 3, Normal),
            l(11, 5, 6, Normal),
            l(-257, 8, 32, Lower),
            l(75, 7, 32, Upper),
            l(0, 6, 0, Oob),
        ],
        4 => vec![
            l(1, 1, 0, Normal),
            l(2, 2, 0, Normal),
            l(3, 3, 0, Normal),
            l(4, 4, 3, Normal),
            l(12, 5, 6, Normal),
            l(76, 5, 32, Upper),
        ],
        5 => vec![
            l(-255, 7, 8, Normal),
            l(1, 1, 0, Normal),
            l(2, 2, 0, Normal),
            l(3, 3, 0, Normal),
            l(4, 4, 3, Normal),
            l(12, 5, 6, Normal),
            l(-256, 7, 32, Lower),
            l(76, 6, 32, Upper),
        ],
        6 => vec![
            l(-2048, 5, 10, Normal),
            l(-1024, 4, 9, Normal),
            l(-512, 4, 8, Normal),
            l(-256, 4, 7, Normal),
            l(-128, 5, 6, Normal),
            l(-64, 5, 5, Normal),
            l(-32, 4, 5, Normal),
            l(0, 2, 7, Normal),
            l(128, 3, 7, Normal),
            l(256, 3, 8, Normal),
            l(512, 4, 9, Normal),
            l(1024, 4, 10, Normal),
            l(-2049, 6, 32, Lower),
            l(2048, 6, 32, Upper),
        ],
        7 => vec![
            l(-1024, 4, 9, Normal),
            l(-512, 3, 8, Normal),
            l(-256, 4, 7, Normal),
            l(-128, 5, 6, Normal),
            l(-64, 5, 5, Normal),
            l(-32, 4, 5, Normal),
            l(0, 4, 5, Normal),
            l(32, 5, 5, Normal),
            l(64, 5, 6, Normal),
            l(128, 4, 7, Normal),
            l(256, 3, 8, Normal),
            l(512, 3, 9, Normal),
            l(1024, 3, 10, Normal),
            l(-1025, 5, 32, Lower),
            l(2048, 5, 32, Upper),
        ],
        8 => vec![
            l(-15, 8, 3, Normal),
            l(-7, 9, 1, Normal),
            l(-5, 8, 1, Normal),
            l(-3, 9, 0, Normal),
            l(-2, 7, 0, Normal),
            l(-1, 4, 0, Normal),
            l(0, 2, 1, Normal),
            l(2, 5, 0, Normal),
            l(3, 6, 0, Normal),
            l(4, 3, 4, Normal),
            l(20, 6, 1, Normal),
            l(22, 4, 4, Normal),
            l(38, 4, 5, Normal),
            l(70, 5, 6, Normal),
            l(134, 5, 7, Normal),
            l(262, 6, 7, Normal),
            l(390, 7, 8, Normal),
            l(646, 6, 10, Normal),
            l(-16, 9, 32, Lower),
            l(1670, 9, 32, Upper),
            l(0, 2, 0, Oob),
        ],
        9 => vec![
            l(-31, 8, 4, Normal),
            l(-15, 9, 2, Normal),
            l(-11, 8, 2, Normal),
            l(-7, 9, 1, Normal),
            l(-5, 7, 1, Normal),
            l(-3, 4, 1, Normal),
            l(-1, 3, 1, Normal),
            l(1, 3, 1, Normal),
            l(3, 5, 1, Normal),
            l(5, 6, 1, Normal),
            l(7, 3, 5, Normal),
            l(39, 6, 2, Normal),
            l(43, 4, 5, Normal),
            l(75, 4, 6, Normal),
            l(139, 5, 7, Normal),
            l(267, 5, 8, Normal),
            l(523, 6, 8, Normal),
            l(779, 7, 9, Normal),
            l(1291, 6, 11, Normal),
            l(-32, 9, 32, Lower),
            l(3339, 9, 32, Upper),
            l(0, 2, 0, Oob),
        ],
        10 => vec![
            l(-21, 7, 4, Normal),
            l(-5, 8, 0, Normal),
            l(-4, 7, 0, Normal),
            l(-3, 5, 0, Normal),
            l(-2, 2, 2, Normal),
            l(2, 5, 0, Normal),
            l(3, 6, 0, Normal),
            l(4, 7, 0, Normal),
            l(5, 8, 0, Normal),
            l(6, 2, 6, Normal),
            l(70, 5, 5, Normal),
            l(102, 6, 5, Normal),
            l(134, 6, 6, Normal),
            l(198, 6, 7, Normal),
            l(326, 6, 8, Normal),
            l(582, 6, 9, Normal),
            l(1094, 6, 10, Normal),
            l(2118, 7, 11, Normal),
            l(-22, 8, 32, Lower),
            l(4166, 8, 32, Upper),
            l(0, 2, 0, Oob),
        ],
        11 => vec![
            l(1, 1, 0, Normal),
            l(2, 2, 1, Normal),
            l(4, 4, 0, Normal),
            l(5, 4, 1, Normal),
            l(7, 5, 1, Normal),
            l(9, 5, 2, Normal),
            l(13, 6, 2, Normal),
            l(17, 7, 2, Normal),
            l(21, 7, 3, Normal),
            l(29, 7, 4, Normal),
            l(45, 7, 5, Normal),
            l(77, 7, 6, Normal),
            l(141, 7, 32, Upper),
        ],
        12 => vec![
            l(1, 1, 0, Normal),
            l(2, 2, 0, Normal),
            l(3, 3, 1, Normal),
            l(5, 5, 0, Normal),
            l(6, 5, 1, Normal),
            l(8, 6, 1, Normal),
            l(10, 7, 0, Normal),
            l(11, 7, 1, Normal),
            l(13, 7, 2, Normal),
            l(17, 7, 3, Normal),
            l(25, 7, 4, Normal),
            l(41, 8, 5, Normal),
            l(73, 8, 32, Upper),
        ],
        13 => vec![
            l(1, 1, 0, Normal),
            l(2, 3, 0, Normal),
            l(3, 4, 0, Normal),
            l(4, 5, 0, Normal),
            l(5, 4, 1, Normal),
            l(7, 3, 3, Normal),
            l(15, 6, 1, Normal),
            l(17, 6, 2, Normal),
            l(21, 6, 3, Normal),
            l(29, 6, 4, Normal),
            l(45, 6, 5, Normal),
            l(77, 7, 6, Normal),
            l(141, 7, 32, Upper),
        ],
        14 => vec![l(-2, 3, 0, Normal), l(-1, 3, 0, Normal), l(0, 1, 0, Normal), l(1, 3, 0, Normal), l(2, 3, 0, Normal)],
        _ => vec![
            l(-24, 7, 4, Normal),
            l(-8, 6, 2, Normal),
            l(-4, 5, 1, Normal),
            l(-2, 4, 0, Normal),
            l(-1, 3, 0, Normal),
            l(0, 1, 0, Normal),
            l(1, 3, 0, Normal),
            l(2, 4, 0, Normal),
            l(3, 5, 1, Normal),
            l(5, 6, 2, Normal),
            l(9, 7, 4, Normal),
            l(-25, 7, 32, Lower),
            l(25, 7, 32, Upper),
        ],
    };
    Table::new(lines).expect("standard table")
}

/// A tables segment (7.4.13, B.2).
fn custom_table(d: &[u8]) -> R<Table> {
    let flags = *d.first().ok_or("short JBIG2 table")?;
    let low = be32(d, 1)? as i32;
    let high = be32(d, 5)? as i32;
    let ps = ((flags >> 1) & 7) as u32 + 1;
    let rs = ((flags >> 4) & 7) as u32 + 1;
    let mut r = MsbReader::new(&d[9..]);
    let mut rd = |n| r.read(n).ok_or("short JBIG2 table");
    let mut lines = Vec::new();
    let mut cur = low as i64;
    while cur < high as i64 {
        let plen = rd(ps)? as u8;
        let rlen = rd(rs)? as u8;
        if rlen > 32 || lines.len() > 1 << 16 {
            return Err("bad JBIG2 table");
        }
        lines.push(normal(cur as i32, plen, rlen));
        cur += 1i64 << rlen;
    }
    let plen = rd(ps)? as u8;
    lines.push(Line { low: low.wrapping_sub(1), plen, rlen: 32, kind: Kind::Lower });
    let plen = rd(ps)? as u8;
    lines.push(Line { low: high, plen, rlen: 32, kind: Kind::Upper });
    if flags & 1 != 0 {
        let plen = rd(ps)? as u8;
        lines.push(Line { low: 0, plen, rlen: 0, kind: Kind::Oob });
    }
    Table::new(lines)
}

// ---------------------------------------------------------------------------
// Symbol dictionaries and text regions (6.4, 6.5).

fn be32(d: &[u8], at: usize) -> R<u32> {
    Ok(u32::from_be_bytes(d.get(at..at + 4).ok_or("short JBIG2 segment")?.try_into().unwrap()))
}

fn be16(d: &[u8], at: usize) -> R<u16> {
    Ok(u16::from_be_bytes(d.get(at..at + 2).ok_or("short JBIG2 segment")?.try_into().unwrap()))
}

/// Huffman tables of a text region (SBHUFF = 1).
struct TextTables {
    ids: Table,
    fs: Rc<Table>,
    ds: Rc<Table>,
    dt: Rc<Table>,
    rdw: Rc<Table>,
    rdh: Rc<Table>,
    rdx: Rc<Table>,
    rdy: Rc<Table>,
    rsize: Rc<Table>,
    /// Symbol IDs read as raw bits of this length instead of `ids` (inside a
    /// symbol dictionary).
    raw_ids: Option<u32>,
}

struct TextParams<'a> {
    w: usize,
    h: usize,
    refine: bool,
    log_strips: u32,
    instances: u32,
    symbols: &'a [Rc<Bitmap>],
    transposed: bool,
    ds_offset: i32,
    corner: u8,
    defpixel: u8,
    combop: u8,
    rtemplate: u8,
    rat: [At; 2],
}

/// Where the coded data of a region comes from.
enum Coder<'a, 'b, 'c> {
    Arith(&'a mut Mq<'b>, &'a mut IdCx),
    Huff(&'a mut MsbReader<'c>, &'a TextTables, &'c [u8]),
}

fn decode_text(coder: &mut Coder, st: &mut Stats, p: &TextParams) -> R<Bitmap> {
    let mut reg = Bitmap::new(p.w, p.h, p.defpixel)?;
    let strips = 1i64 << p.log_strips;
    let mut strip_t: i64 = match coder {
        Coder::Arith(mq, _) => st.iadt.decode(mq).ok_or("JBIG2 OOB")? as i64,
        Coder::Huff(r, t, _) => t.dt.value(r)? as i64,
    } * -strips;
    let mut first_s: i64 = 0;
    let mut n = 0u32;
    while n < p.instances {
        let dt = match coder {
            Coder::Arith(mq, _) => st.iadt.decode(mq).ok_or("JBIG2 OOB")? as i64,
            Coder::Huff(r, t, _) => t.dt.value(r)? as i64,
        };
        strip_t += dt * strips;
        let mut first = true;
        let mut cur_s: i64 = 0;
        loop {
            if first {
                let dfs = match coder {
                    Coder::Arith(mq, _) => st.iafs.decode(mq).ok_or("JBIG2 OOB")? as i64,
                    Coder::Huff(r, t, _) => t.fs.value(r)? as i64,
                };
                first_s += dfs;
                cur_s = first_s;
                first = false;
            } else {
                let ids = match coder {
                    Coder::Arith(mq, _) => st.iads.decode(mq),
                    Coder::Huff(r, t, _) => t.ds.decode(r)?,
                };
                let Some(ids) = ids else { break };
                cur_s += ids as i64 + p.ds_offset as i64;
            }
            let cur_t: i64 = if strips == 1 {
                0
            } else {
                match coder {
                    Coder::Arith(mq, _) => st.iait.decode(mq).ok_or("JBIG2 OOB")? as i64,
                    Coder::Huff(r, _, _) => r.read(p.log_strips).ok_or("JBIG2 data ends early")? as i64,
                }
            };
            let t_i = strip_t + cur_t;
            let id = match coder {
                Coder::Arith(mq, ids) => ids.decode(mq) as usize,
                Coder::Huff(r, t, _) => match t.raw_ids {
                    Some(len) => r.read(len).ok_or("JBIG2 data ends early")? as usize,
                    None => t.ids.value(r)? as usize,
                },
            };
            let sym = p.symbols.get(id).ok_or("JBIG2 symbol out of range")?;
            let ri = p.refine
                && match coder {
                    Coder::Arith(mq, _) => st.iari.decode(mq).ok_or("JBIG2 OOB")? != 0,
                    Coder::Huff(r, _, _) => r.read(1).ok_or("JBIG2 data ends early")? != 0,
                };
            let refined;
            let ib: &Bitmap = if ri {
                refined = match coder {
                    Coder::Arith(mq, _) => {
                        let rdw = st.iardw.decode(mq).ok_or("JBIG2 OOB")?;
                        let rdh = st.iardh.decode(mq).ok_or("JBIG2 OOB")?;
                        let rdx = st.iardx.decode(mq).ok_or("JBIG2 OOB")?;
                        let rdy = st.iardy.decode(mq).ok_or("JBIG2 OOB")?;
                        refine_symbol(mq, &mut st.gr, sym, rdw, rdh, rdx, rdy, p)?
                    }
                    Coder::Huff(r, t, data) => {
                        let rdw = t.rdw.value(r)?;
                        let rdh = t.rdh.value(r)?;
                        let rdx = t.rdx.value(r)?;
                        let rdy = t.rdy.value(r)?;
                        let size = t.rsize.value(r)? as usize;
                        r.align();
                        let at = r.byte_pos();
                        let chunk = data.get(at..at + size).ok_or("JBIG2 refinement data out of range")?;
                        let mut mq = Mq::new(chunk);
                        let b = refine_symbol(&mut mq, &mut st.gr, sym, rdw, rdh, rdx, rdy, p)?;
                        r.seek_bits((at + size) * 8);
                        b
                    }
                };
                &refined
            } else {
                sym
            };
            let (wi, hi) = (ib.w as i64, ib.h as i64);
            let right = p.corner & 2 != 0;
            let bottom = p.corner & 1 == 0;
            if !p.transposed && right {
                cur_s += wi - 1;
            }
            if p.transposed && bottom {
                cur_s += hi - 1;
            }
            let (mut sx, mut sy) = if p.transposed { (t_i, cur_s) } else { (cur_s, t_i) };
            if right {
                sx -= wi - 1;
            }
            if bottom {
                sy -= hi - 1;
            }
            reg.compose(ib, sx, sy, p.combop);
            if !p.transposed && !right {
                cur_s += wi - 1;
            }
            if p.transposed && !bottom {
                cur_s += hi - 1;
            }
            // The strip still ends with OOB after the last instance.
            n += 1;
        }
    }
    Ok(reg)
}

#[allow(clippy::too_many_arguments)]
fn refine_symbol(
    mq: &mut Mq,
    gr: &mut [Cx],
    sym: &Bitmap,
    rdw: i32,
    rdh: i32,
    rdx: i32,
    rdy: i32,
    p: &TextParams,
) -> R<Bitmap> {
    let w = sym.w as i64 + rdw as i64;
    let h = sym.h as i64 + rdh as i64;
    if w < 0 || h < 0 {
        return Err("negative JBIG2 symbol size");
    }
    decode_refinement(
        mq,
        gr,
        &RefineParams {
            w: w as usize,
            h: h as usize,
            template: p.rtemplate,
            reference: sym,
            dx: (rdw >> 1) + rdx,
            dy: (rdh >> 1) + rdy,
            tpgron: false,
            at: p.rat,
        },
    )
}

/// The symbol ID Huffman table of a text region (7.4.3.1.7).
fn symbol_id_table(r: &mut MsbReader, n: usize) -> R<Table> {
    let mut run = Vec::new();
    for i in 0..35 {
        run.push(normal(i, r.read(4).ok_or("short JBIG2 text region")? as u8, 0));
    }
    let runs = Table::new(run)?;
    let mut lens: Vec<u8> = Vec::with_capacity(n);
    while lens.len() < n {
        let c = runs.value(r)?;
        let (len, count) = match c {
            0..=31 => (c as u8, 1),
            32 => (*lens.last().ok_or("JBIG2 run code without a previous length")?, r.read(2).ok_or("short")? + 3),
            33 => (0, r.read(3).ok_or("short")? + 3),
            34 => (0, r.read(7).ok_or("short")? + 11),
            _ => return Err("bad JBIG2 run code"),
        };
        for _ in 0..count {
            lens.push(len);
        }
    }
    lens.truncate(n);
    r.align();
    Table::new(lens.iter().enumerate().map(|(i, &l)| normal(i as i32, l, 0)).collect())
}

struct SymbolDict {
    huff: bool,
    refagg: bool,
    dh: u8,
    dw: u8,
    bmsize: u8,
    agginst: u8,
    ctx_used: bool,
    ctx_retained: bool,
    template: u8,
    rtemplate: u8,
    at: Vec<At>,
    rat: [At; 2],
    new_symbols: u32,
}

// ---------------------------------------------------------------------------
// Segments and pages (7).

struct RegionInfo {
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    combop: u8,
}

fn region_info(d: &[u8]) -> R<RegionInfo> {
    Ok(RegionInfo {
        w: be32(d, 0)?,
        h: be32(d, 4)?,
        x: be32(d, 8)?,
        y: be32(d, 12)?,
        combop: *d.get(16).ok_or("short JBIG2 region")? & 7,
    })
}

fn read_at(d: &[u8], p: &mut usize, n: usize) -> R<Vec<At>> {
    let mut v = Vec::new();
    for _ in 0..n {
        let b = d.get(*p..*p + 2).ok_or("short JBIG2 segment")?;
        v.push((b[0] as i8, b[1] as i8));
        *p += 2;
    }
    Ok(v)
}

struct Header {
    number: u32,
    kind: u8,
    referred: Vec<u32>,
    length: u32,
}

/// A segment header at `d[p..]`; returns it and the offset after it.
fn segment_header(d: &[u8], p: usize) -> R<(Header, usize)> {
    let number = be32(d, p)?;
    let flags = *d.get(p + 4).ok_or("short JBIG2 header")?;
    let kind = flags & 0x3F;
    let rf = *d.get(p + 5).ok_or("short JBIG2 header")?;
    let mut q = p + 6;
    let mut count = (rf >> 5) as usize;
    if count == 7 {
        count = (be32(d, p + 5)? & 0x1FFF_FFFF) as usize;
        q = p + 9 + count.div_ceil(8).max(1);
        if count > 1 << 16 {
            return Err("too many JBIG2 referred-to segments");
        }
    } else if count > 4 {
        return Err("bad JBIG2 referred-to count");
    }
    let size = if number <= 256 {
        1
    } else if number <= 65536 {
        2
    } else {
        4
    };
    let mut referred = Vec::with_capacity(count);
    for _ in 0..count {
        let v = match size {
            1 => *d.get(q).ok_or("short JBIG2 header")? as u32,
            2 => be16(d, q)? as u32,
            _ => be32(d, q)?,
        };
        referred.push(v);
        q += size;
    }
    q += if flags & 0x40 != 0 { 4 } else { 1 };
    let length = be32(d, q)?;
    q += 4;
    Ok((Header { number, kind, referred, length }, q))
}

struct Page {
    bitmap: Bitmap,
    default_pixel: u8,
    combop: u8,
    override_combop: bool,
    unknown_height: bool,
}

#[derive(Default)]
struct State {
    symbols: BTreeMap<u32, Vec<Rc<Bitmap>>>,
    patterns: BTreeMap<u32, Vec<Bitmap>>,
    tables: BTreeMap<u32, Rc<Table>>,
    regions: BTreeMap<u32, (u32, u32, Bitmap)>,
    retained: BTreeMap<u32, (Vec<Cx>, Vec<Cx>)>,
    page: Option<Page>,
    pages: Vec<Bitmap>,
}

impl State {
    fn finish_page(&mut self) {
        if let Some(p) = self.page.take() {
            self.pages.push(p.bitmap);
        }
    }

    /// Combines a region onto the page.
    fn place(&mut self, info: &RegionInfo, b: &Bitmap) -> R<()> {
        let Some(page) = self.page.as_mut() else { return Ok(()) };
        if page.unknown_height {
            let bottom = info.y as usize + b.h;
            page.bitmap.grow(bottom, page.default_pixel)?;
        }
        let op = if page.override_combop { info.combop } else { page.combop };
        page.bitmap.compose(b, info.x as i64, info.y as i64, op);
        Ok(())
    }

    fn referred_symbols(&self, h: &Header) -> Vec<Rc<Bitmap>> {
        let mut v = Vec::new();
        for r in &h.referred {
            if let Some(s) = self.symbols.get(r) {
                v.extend(s.iter().cloned());
            }
        }
        v
    }

    fn referred_tables(&self, h: &Header) -> Vec<Rc<Table>> {
        h.referred.iter().filter_map(|r| self.tables.get(r).cloned()).collect()
    }

    fn segment(&mut self, h: &Header, d: &[u8]) -> R<()> {
        match h.kind {
            0 => self.symbol_dictionary(h, d),
            4 | 6 | 7 => self.text_region(h, d),
            16 => self.pattern_dictionary(h, d),
            20 | 22 | 23 => self.halftone_region(h, d),
            36 | 38 | 39 => self.generic_region(h, d),
            40 | 42 | 43 => self.refinement_region(h, d),
            48 => {
                self.finish_page();
                let w = be32(d, 0)? as usize;
                let hh = be32(d, 4)?;
                let flags = *d.get(16).ok_or("short JBIG2 page")?;
                let default_pixel = (flags >> 2) & 1;
                let unknown_height = hh == 0xFFFF_FFFF;
                let bitmap = Bitmap::new(w, if unknown_height { 0 } else { hh as usize }, default_pixel)?;
                self.page = Some(Page {
                    bitmap,
                    default_pixel,
                    combop: (flags >> 3) & 3,
                    override_combop: flags & 0x40 != 0,
                    unknown_height,
                });
                Ok(())
            }
            49 => {
                self.finish_page();
                Ok(())
            }
            50 => {
                let y = be32(d, 0)? as usize;
                if let Some(p) = self.page.as_mut() {
                    if p.unknown_height {
                        p.bitmap.grow(y + 1, p.default_pixel)?;
                    }
                }
                Ok(())
            }
            53 => {
                self.tables.insert(h.number, Rc::new(custom_table(d)?));
                Ok(())
            }
            // End of file, profiles, extensions and colour palettes.
            _ => Ok(()),
        }
    }

    fn generic_region(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let info = region_info(d)?;
        let flags = *d.get(17).ok_or("short JBIG2 region")?;
        let mmr = flags & 1 != 0;
        let template = (flags >> 1) & 3;
        let tpgdon = flags & 8 != 0;
        if flags & 0x10 != 0 {
            return Err("JBIG2 extended templates are not supported");
        }
        let mut p = 18;
        let at = if mmr { Vec::new() } else { read_at(d, &mut p, if template == 0 { 4 } else { 1 })? };
        let data = &d[p.min(d.len())..];
        let mut height = info.h as usize;
        // An unknown data length (7.2.7) ends with a row count.
        if h.length == 0xFFFF_FFFF && data.len() >= 4 {
            height = be32(data, data.len() - 4)? as usize;
        }
        let b = if mmr {
            decode_mmr_bitmap(data, info.w as usize, height)?.0
        } else {
            let mut st = vec![0; 1 << 16];
            let mut mq = Mq::new(data);
            decode_generic(
                &mut mq,
                &mut st,
                &GenericParams { w: info.w as usize, h: height, template, tpgdon, at: &at, skip: None },
            )?
        };
        self.store_or_place(h, &info, b)
    }

    fn store_or_place(&mut self, h: &Header, info: &RegionInfo, b: Bitmap) -> R<()> {
        if matches!(h.kind, 4 | 20 | 36 | 40) {
            self.regions.insert(h.number, (info.x, info.y, b));
            Ok(())
        } else {
            self.place(info, &b)
        }
    }

    fn refinement_region(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let info = region_info(d)?;
        let flags = *d.get(17).ok_or("short JBIG2 region")?;
        let template = flags & 1;
        let tpgron = flags & 2 != 0;
        let mut p = 18;
        let at = if template == 0 { read_at(d, &mut p, 2)? } else { vec![(0, 0), (0, 0)] };
        let data = &d[p.min(d.len())..];
        let referred = h.referred.iter().find_map(|r| self.regions.remove(r));
        let reference = match referred {
            Some((_, _, b)) => b,
            None => {
                let page = self.page.as_ref().ok_or("JBIG2 refinement without a page")?;
                page.bitmap.sub(info.x as usize, info.y as usize, info.w as usize, info.h as usize)
            }
        };
        let mut gr = vec![0; 1 << 13];
        let mut mq = Mq::new(data);
        let b = decode_refinement(
            &mut mq,
            &mut gr,
            &RefineParams {
                w: info.w as usize,
                h: info.h as usize,
                template,
                reference: &reference,
                dx: 0,
                dy: 0,
                tpgron,
                at: [at[0], at[1]],
            },
        )?;
        self.store_or_place(h, &info, b)
    }

    fn pattern_dictionary(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let flags = *d.first().ok_or("short JBIG2 pattern dictionary")?;
        let mmr = flags & 1 != 0;
        let template = (flags >> 1) & 3;
        let pw = *d.get(1).ok_or("short")? as usize;
        let ph = *d.get(2).ok_or("short")? as usize;
        let max = be32(d, 3)? as usize;
        if pw == 0 || ph == 0 || max >= 1 << 20 {
            return Err("bad JBIG2 pattern dictionary");
        }
        let data = &d[7.min(d.len())..];
        let cw = (max + 1) * pw;
        let all = if mmr {
            decode_mmr_bitmap(data, cw, ph)?.0
        } else {
            let mut at: Vec<At> = vec![(-(pw as i32) as i8, 0)];
            if template == 0 {
                at.extend_from_slice(&[(-3, -1), (2, -2), (-2, -2)]);
            }
            let mut gb = vec![0; 1 << 16];
            let mut mq = Mq::new(data);
            decode_generic(&mut mq, &mut gb, &GenericParams { w: cw, h: ph, template, tpgdon: false, at: &at, skip: None })?
        };
        let pats = (0..=max).map(|i| all.sub(i * pw, 0, pw, ph)).collect();
        self.patterns.insert(h.number, pats);
        Ok(())
    }

    fn halftone_region(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let info = region_info(d)?;
        let flags = *d.get(17).ok_or("short JBIG2 region")?;
        let mmr = flags & 1 != 0;
        let template = (flags >> 1) & 3;
        let enable_skip = flags & 8 != 0;
        let combop = (flags >> 4) & 7;
        let defpixel = (flags >> 7) & 1;
        let gw = be32(d, 18)? as usize;
        let gh = be32(d, 22)? as usize;
        let gx = be32(d, 26)? as i32 as i64;
        let gy = be32(d, 30)? as i32 as i64;
        let rx = be16(d, 34)? as i64;
        let ry = be16(d, 36)? as i64;
        let data = &d[38.min(d.len())..];
        let pats = h
            .referred
            .iter()
            .find_map(|r| self.patterns.get(r))
            .ok_or("JBIG2 halftone without a pattern dictionary")?;
        let (pw, ph) = (pats[0].w as i64, pats[0].h as i64);
        let bpp = ceil_log2(pats.len());
        let mut reg = Bitmap::new(info.w as usize, info.h as usize, defpixel)?;
        if gw.checked_mul(gh).is_none_or(|n| n > MAX_PIXELS) {
            return Err("JBIG2 halftone grid too large");
        }
        // 6.6.5.1: grid cells whose pattern falls outside the region.
        let skip = if enable_skip {
            let mut s = Bitmap::new(gw, gh, 0)?;
            for mg in 0..gh as i64 {
                for ng in 0..gw as i64 {
                    let x = (gx + mg * ry + ng * rx) >> 8;
                    let y = (gy + mg * rx - ng * ry) >> 8;
                    if x + pw <= 0 || x >= info.w as i64 || y + ph <= 0 || y >= info.h as i64 {
                        s.px[mg as usize * gw + ng as usize] = 1;
                    }
                }
            }
            Some(s)
        } else {
            None
        };
        // Annex C: gray-scale image from bit planes, most significant first.
        let mut planes: Vec<Bitmap> = Vec::with_capacity(bpp as usize);
        let mut at: Vec<At> = vec![(if template <= 1 { 3 } else { 2 }, -1)];
        if template == 0 {
            at.extend_from_slice(&[(-3, -1), (2, -2), (-2, -2)]);
        }
        let mut gb = vec![0; 1 << 16];
        let mut mq = Mq::new(data);
        let mut pos = 0;
        for _ in 0..bpp {
            let b = if mmr {
                let (b, used) = decode_mmr_bitmap(&data[pos.min(data.len())..], gw, gh)?;
                pos += used;
                b
            } else {
                decode_generic(
                    &mut mq,
                    &mut gb,
                    &GenericParams { w: gw, h: gh, template, tpgdon: false, at: &at, skip: skip.as_ref() },
                )?
            };
            planes.push(b);
        }
        // Gray code to binary: each plane XORs the one above it.
        for j in 1..planes.len() {
            let (a, b) = planes.split_at_mut(j);
            for (v, &u) in b[0].px.iter_mut().zip(&a[j - 1].px) {
                *v ^= u;
            }
        }
        for mg in 0..gh {
            for ng in 0..gw {
                let mut idx = 0usize;
                for pl in &planes {
                    idx = (idx << 1) | pl.px[mg * gw + ng] as usize;
                }
                let pat = &pats[idx.min(pats.len() - 1)];
                let x = (gx + mg as i64 * ry + ng as i64 * rx) >> 8;
                let y = (gy + mg as i64 * rx - ng as i64 * ry) >> 8;
                reg.compose(pat, x, y, combop);
            }
        }
        self.store_or_place(h, &info, reg)
    }

    fn text_region(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let info = region_info(d)?;
        let flags = be16(d, 17)?;
        let mut p = 19;
        let huff = flags & 1 != 0;
        let refine = flags & 2 != 0;
        let log_strips = ((flags >> 2) & 3) as u32;
        let corner = ((flags >> 4) & 3) as u8;
        let transposed = flags & 0x40 != 0;
        let combop = ((flags >> 7) & 3) as u8;
        let defpixel = ((flags >> 9) & 1) as u8;
        let ds = ((flags >> 10) & 0x1F) as i32;
        let ds_offset = if ds >= 16 { ds - 32 } else { ds };
        let rtemplate = ((flags >> 15) & 1) as u8;
        let hflags = if huff {
            p += 2;
            be16(d, 19)?
        } else {
            0
        };
        let rat = if refine && rtemplate == 0 { read_at(d, &mut p, 2)? } else { vec![(0, 0), (0, 0)] };
        let instances = be32(d, p)?;
        p += 4;
        let data = &d[p.min(d.len())..];
        let symbols = self.referred_symbols(h);
        let params = TextParams {
            w: info.w as usize,
            h: info.h as usize,
            refine,
            log_strips,
            instances,
            symbols: &symbols,
            transposed,
            ds_offset,
            corner,
            defpixel,
            combop,
            rtemplate,
            rat: [rat[0], rat[1]],
        };
        let mut st = Stats::new();
        let b = if huff {
            let customs = self.referred_tables(h);
            let mut ci = customs.iter();
            let mut pick = |sel: u16, std: &[u8]| -> R<Rc<Table>> {
                match std.get(sel as usize) {
                    Some(&n) => Ok(Rc::new(standard(n))),
                    None => ci.next().cloned().ok_or("missing JBIG2 custom table"),
                }
            };
            let fs = pick(hflags & 3, &[6, 7])?;
            let ds = pick((hflags >> 2) & 3, &[8, 9, 10])?;
            let dt = pick((hflags >> 4) & 3, &[11, 12, 13])?;
            let rdw = pick((hflags >> 6) & 3, &[14, 15])?;
            let rdh = pick((hflags >> 8) & 3, &[14, 15])?;
            let rdx = pick((hflags >> 10) & 3, &[14, 15])?;
            let rdy = pick((hflags >> 12) & 3, &[14, 15])?;
            let rsize = pick((hflags >> 14) & 1, &[1])?;
            let mut r = MsbReader::new(data);
            let ids = symbol_id_table(&mut r, symbols.len())?;
            let tables = TextTables { ids, fs, ds, dt, rdw, rdh, rdx, rdy, rsize, raw_ids: None };
            decode_text(&mut Coder::Huff(&mut r, &tables, data), &mut st, &params)?
        } else {
            let mut mq = Mq::new(data);
            let mut ids = IdCx::new(ceil_log2(symbols.len()))?;
            decode_text(&mut Coder::Arith(&mut mq, &mut ids), &mut st, &params)?
        };
        self.store_or_place(h, &info, b)
    }

    fn symbol_dictionary(&mut self, h: &Header, d: &[u8]) -> R<()> {
        let flags = be16(d, 0)?;
        let mut p = 2;
        let huff = flags & 1 != 0;
        let refagg = flags & 2 != 0;
        let template = ((flags >> 10) & 3) as u8;
        let rtemplate = ((flags >> 12) & 1) as u8;
        let at = if huff { Vec::new() } else { read_at(d, &mut p, if template == 0 { 4 } else { 1 })? };
        let rat = if refagg && rtemplate == 0 { read_at(d, &mut p, 2)? } else { vec![(0, 0), (0, 0)] };
        let sd = SymbolDict {
            huff,
            refagg,
            dh: ((flags >> 2) & 3) as u8,
            dw: ((flags >> 4) & 3) as u8,
            bmsize: ((flags >> 6) & 1) as u8,
            agginst: ((flags >> 7) & 1) as u8,
            ctx_used: flags & 0x100 != 0,
            ctx_retained: flags & 0x200 != 0,
            template,
            rtemplate,
            at,
            rat: [rat[0], rat[1]],
            // SDNUMEXSYMS is not needed: the export flags decide.
            new_symbols: be32(d, p + 4)?,
        };
        p += 8;
        let data = &d[p.min(d.len())..];
        let input = self.referred_symbols(h);
        let mut st = Stats::new();
        if sd.ctx_used {
            let last = h.referred.iter().rev().find(|r| self.symbols.contains_key(r));
            if let Some((gb, gr)) = last.and_then(|r| self.retained.get(r)) {
                st.gb.copy_from_slice(gb);
                st.gr.copy_from_slice(gr);
            }
        }
        let customs = self.referred_tables(h);
        let out = decode_symbols(&sd, &input, &customs, data, &mut st)?;
        if sd.ctx_retained {
            self.retained.insert(h.number, (st.gb, st.gr));
        }
        self.symbols.insert(h.number, out);
        Ok(())
    }
}

/// 6.5.5: the exported symbols of a dictionary.
fn decode_symbols(
    sd: &SymbolDict,
    input: &[Rc<Bitmap>],
    customs: &[Rc<Table>],
    data: &[u8],
    st: &mut Stats,
) -> R<Vec<Rc<Bitmap>>> {
    let total = input.len() + sd.new_symbols as usize;
    if total > 1 << 20 {
        return Err("too many JBIG2 symbols");
    }
    let code_len = ceil_log2(total);
    let mut ci = customs.iter();
    let mut pick = |sel: u8, std: &[u8]| -> R<Rc<Table>> {
        match std.get(sel as usize) {
            Some(&n) => Ok(Rc::new(standard(n))),
            None => ci.next().cloned().ok_or("missing JBIG2 custom table"),
        }
    };
    let mut huff = None;
    if sd.huff {
        let dh = pick(sd.dh, &[4, 5])?;
        let dw = pick(sd.dw, &[2, 3])?;
        let bmsize = pick(sd.bmsize, &[1])?;
        let agginst = pick(sd.agginst, &[1])?;
        huff = Some((dh, dw, bmsize, agginst));
    }
    let mut mq = Mq::new(data);
    let mut r = MsbReader::new(data);
    let mut ids = IdCx::new(code_len)?;
    let mut new: Vec<Rc<Bitmap>> = Vec::with_capacity(sd.new_symbols as usize);
    let mut widths: Vec<usize> = Vec::new();
    let mut height: i64 = 0;
    while new.len() < sd.new_symbols as usize {
        let dh = match &huff {
            Some(t) => t.0.value(&mut r)? as i64,
            None => st.iadh.decode(&mut mq).ok_or("JBIG2 OOB")? as i64,
        };
        height += dh;
        if !(0..=1 << 16).contains(&height) {
            return Err("bad JBIG2 symbol height");
        }
        let mut width: i64 = 0;
        let mut total_width: usize = 0;
        widths.clear();
        loop {
            let dw = match &huff {
                Some(t) => t.1.decode(&mut r)?,
                None => st.iadw.decode(&mut mq),
            };
            let Some(dw) = dw else { break };
            if new.len() + widths.len() >= sd.new_symbols as usize {
                return Err("too many JBIG2 symbols in a height class");
            }
            width += dw as i64;
            if !(0..=1 << 16).contains(&width) {
                return Err("bad JBIG2 symbol width");
            }
            total_width += width as usize;
            let (w, hh) = (width as usize, height as usize);
            if sd.refagg {
                let all: Vec<Rc<Bitmap>> = input.iter().chain(new.iter()).cloned().collect();
                let n = match &huff {
                    Some(t) => t.3.value(&mut r)?,
                    None => st.iaai.decode(&mut mq).ok_or("JBIG2 OOB")?,
                };
                let b = if n > 1 {
                    let params = TextParams {
                        w,
                        h: hh,
                        refine: true,
                        log_strips: 0,
                        instances: n as u32,
                        symbols: &all,
                        transposed: false,
                        ds_offset: 0,
                        corner: 1,
                        defpixel: 0,
                        combop: 0,
                        rtemplate: sd.rtemplate,
                        rat: sd.rat,
                    };
                    match &huff {
                        Some(_) => {
                            let tables = TextTables {
                                ids: Table::new(Vec::new())?,
                                fs: Rc::new(standard(6)),
                                ds: Rc::new(standard(8)),
                                dt: Rc::new(standard(11)),
                                rdw: Rc::new(standard(15)),
                                rdh: Rc::new(standard(15)),
                                rdx: Rc::new(standard(15)),
                                rdy: Rc::new(standard(15)),
                                rsize: Rc::new(standard(1)),
                                raw_ids: Some(code_len.max(1)),
                            };
                            decode_text(&mut Coder::Huff(&mut r, &tables, data), st, &params)?
                        }
                        None => decode_text(&mut Coder::Arith(&mut mq, &mut ids), st, &params)?,
                    }
                } else {
                    // One refined symbol (6.5.8.2.2).
                    let (id, rdx, rdy, chunk_mq);
                    match &huff {
                        Some(_) => {
                            id = r.read(code_len.max(1)).ok_or("JBIG2 data ends early")? as usize;
                            let b15 = standard(15);
                            rdx = b15.value(&mut r)?;
                            rdy = b15.value(&mut r)?;
                            // Table 18 sizes the refinement data with B.1.
                            let size = standard(1).value(&mut r)? as usize;
                            r.align();
                            let at = r.byte_pos();
                            let chunk = data.get(at..at + size).ok_or("JBIG2 refinement data out of range")?;
                            chunk_mq = Some(Mq::new(chunk));
                            r.seek_bits((at + size) * 8);
                        }
                        None => {
                            id = ids.decode(&mut mq) as usize;
                            rdx = st.iardx.decode(&mut mq).ok_or("JBIG2 OOB")?;
                            rdy = st.iardy.decode(&mut mq).ok_or("JBIG2 OOB")?;
                            chunk_mq = None;
                        }
                    }
                    let reference = all.get(id).ok_or("JBIG2 symbol out of range")?;
                    let params = RefineParams {
                        w,
                        h: hh,
                        template: sd.rtemplate,
                        reference,
                        dx: rdx,
                        dy: rdy,
                        tpgron: false,
                        at: sd.rat,
                    };
                    match chunk_mq {
                        Some(mut m) => decode_refinement(&mut m, &mut st.gr, &params)?,
                        None => decode_refinement(&mut mq, &mut st.gr, &params)?,
                    }
                };
                new.push(Rc::new(b));
            } else if huff.is_some() {
                widths.push(w);
            } else {
                let b = decode_generic(
                    &mut mq,
                    &mut st.gb,
                    &GenericParams { w, h: hh, template: sd.template, tpgdon: false, at: &sd.at, skip: None },
                )?;
                new.push(Rc::new(b));
            }
        }
        // 6.5.9: a height class of Huffman-coded symbols is one bitmap.
        if let (Some(t), false) = (&huff, sd.refagg) {
            let size = t.2.value(&mut r)? as usize;
            r.align();
            let at = r.byte_pos();
            let hh = height as usize;
            let all = if size == 0 {
                let mut b = Bitmap::new(total_width, hh, 0)?;
                for y in 0..hh {
                    for x in 0..total_width {
                        b.px[y * total_width + x] = r.read(1).ok_or("JBIG2 data ends early")? as u8;
                    }
                    r.align();
                }
                b
            } else {
                let chunk = data.get(at..at + size).ok_or("JBIG2 collective bitmap out of range")?;
                r.seek_bits((at + size) * 8);
                decode_mmr_bitmap(chunk, total_width, hh)?.0
            };
            let mut x = 0;
            for &w in &widths {
                new.push(Rc::new(all.sub(x, 0, w, hh)));
                x += w;
            }
        }
    }
    // 6.5.10: export flags as alternating runs.
    let mut out = Vec::new();
    let mut flag = false;
    let mut i = 0usize;
    while i < total {
        let run = match &huff {
            Some(_) => standard(1).value(&mut r)?,
            None => st.iaex.decode(&mut mq).ok_or("JBIG2 OOB")?,
        };
        if run < 0 || i + run as usize > total {
            return Err("bad JBIG2 export run");
        }
        if flag {
            for j in i..i + run as usize {
                let s = if j < input.len() { input[j].clone() } else { new[j - input.len()].clone() };
                out.push(s);
            }
        }
        i += run as usize;
        flag = !flag;
    }
    Ok(out)
}

/// Decodes JBIG2 data: a file (with its header) or, `embedded`, the
/// segments of a PDF stream, after the segments of `globals`. Returns the
/// pages.
pub fn decode(data: &[u8], globals: Option<&[u8]>, embedded: bool) -> R<Vec<Bitmap>> {
    let mut st = State::default();
    if let Some(g) = globals {
        run_sequential(&mut st, g)?;
    }
    if embedded {
        run_sequential(&mut st, data)?;
    } else {
        const ID: [u8; 8] = [0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A];
        if data.get(..8) != Some(&ID) {
            return Err("not a JBIG2 file");
        }
        let flags = *data.get(8).ok_or("short JBIG2 file")?;
        let mut p = 9;
        if flags & 2 == 0 {
            p += 4;
        }
        if flags & 1 != 0 {
            run_sequential(&mut st, &data[p.min(data.len())..])?;
        } else {
            // Random access: all headers, then all data.
            let mut headers = Vec::new();
            loop {
                if p >= data.len() {
                    break;
                }
                let (h, q) = segment_header(data, p)?;
                p = q;
                let end = h.kind == 51;
                headers.push(h);
                if end {
                    break;
                }
            }
            for h in headers {
                let len = h.length as usize;
                let d = data.get(p..p.checked_add(len).ok_or("bad JBIG2 length")?).ok_or("JBIG2 segment past the end")?;
                p += len;
                st.segment(&h, d)?;
            }
        }
    }
    st.finish_page();
    Ok(st.pages)
}

fn run_sequential(st: &mut State, data: &[u8]) -> R<()> {
    let mut p = 0;
    while p < data.len() {
        let (h, q) = segment_header(data, p)?;
        let len = if h.length == 0xFFFF_FFFF {
            unknown_length(&h, data, q)?
        } else {
            h.length as usize
        };
        let end = q.checked_add(len).ok_or("bad JBIG2 length")?;
        let d = data.get(q..end).ok_or("JBIG2 segment past the end")?;
        st.segment(&h, d)?;
        p = end;
        if h.kind == 51 {
            break;
        }
    }
    Ok(())
}

/// 7.2.7: an immediate generic region of unknown length ends with
/// 0xFF 0xAC (or 0x00 0x00 for MMR) and the row count.
fn unknown_length(h: &Header, data: &[u8], q: usize) -> R<usize> {
    if h.kind != 38 {
        return Err("JBIG2 segment of unknown length");
    }
    let info = data.get(q..).ok_or("short JBIG2 segment")?;
    let flags = *info.get(17).ok_or("short JBIG2 region")?;
    let mmr = flags & 1 != 0;
    let marker: [u8; 2] = if mmr { [0, 0] } else { [0xFF, 0xAC] };
    let start = 18;
    let mut i = start;
    while i + 6 <= info.len() {
        if info[i..i + 2] == marker {
            let rows = be32(info, i + 2)?;
            let known = be32(info, 4)?;
            if known == 0xFFFF_FFFF || rows == known || rows <= known {
                return Ok(i + 6);
            }
        }
        i += 1;
    }
    Err("end of JBIG2 region not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_tables_build() {
        for n in 1..=15 {
            let t = standard(n);
            assert!(!t.codes.is_empty(), "B.{n}");
        }
        // B.1: "0" + 4 bits is 0..15, "10" + 8 bits from 16.
        let t = standard(1);
        let data = [0b0011_1100, 0b0000_0010, 0];
        let mut r = MsbReader::new(&data);
        assert_eq!(t.value(&mut r).unwrap(), 7);
        assert_eq!(t.value(&mut r).unwrap(), 16 + 1);
    }
}
