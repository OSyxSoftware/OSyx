//! Tier-1 decoding (T.800 Annex D): the bit planes of one code-block with
//! the significance propagation, magnitude refinement and cleanup passes,
//! in MQ-coded or raw (bypass) codeword segments.
//!
//! Coefficients come out doubled with a half step of the lowest decoded bit
//! plane added, as OpenJPEG keeps them: `s = 2 * |q| + 1` for a fully
//! decoded magnitude `q`, so `s / 2` is the value and `s * step / 2` the
//! midpoint reconstruction of a truncated one.

use crate::mq::{cx, Cx, Mq};
use alloc::vec;
use alloc::vec::Vec;

/// Code-block style bits (SPcod).
pub const BYPASS: u8 = 1;
pub const RESET: u8 = 2;
pub const TERMALL: u8 = 4;
pub const VCAUSAL: u8 = 8;
pub const SEGSYM: u8 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orient {
    LL,
    HL,
    LH,
    HH,
}

const SIG: u8 = 1;
const NEG: u8 = 2;
const VISITED: u8 = 4;
const REFINED: u8 = 8;

const RL: usize = 17;
const UNI: usize = 18;

/// How many passes the codeword segment starting with pass `k` holds.
pub fn segment_len(k: u32, style: u8) -> u32 {
    if style & TERMALL != 0 {
        1
    } else if style & BYPASS != 0 {
        if k < 10 {
            10 - k
        } else {
            // Two raw passes (propagation, refinement), then one MQ cleanup.
            match (k - 10) % 3 {
                0 => 2,
                _ => 1,
            }
        }
    } else {
        u32::MAX
    }
}

/// Pass `k` is raw-coded.
fn is_raw(k: u32, style: u8) -> bool {
    style & BYPASS != 0 && k >= 10 && !k.is_multiple_of(3)
}

/// Raw bits with the bit stuffing after 0xFF (D.6).
struct Raw<'a> {
    data: &'a [u8],
    pos: usize,
    c: u32,
    ct: u32,
}

impl Raw<'_> {
    fn bit(&mut self) -> u32 {
        if self.ct == 0 {
            let b = *self.data.get(self.pos).unwrap_or(&0xFF) as u32;
            if self.c == 0xFF {
                if b > 0x8F {
                    self.c = 0xFF;
                    self.ct = 8;
                } else {
                    self.c = b;
                    self.pos += 1;
                    self.ct = 7;
                }
            } else {
                self.c = b;
                self.pos += 1;
                self.ct = 8;
            }
        }
        self.ct -= 1;
        (self.c >> self.ct) & 1
    }
}

enum Coder<'a> {
    Mq(Mq<'a>),
    Raw(Raw<'a>),
}

struct Block {
    w: usize,
    h: usize,
    stride: usize,
    flags: Vec<u8>,
    mag: Vec<i32>,
    orient: Orient,
    vcausal: bool,
    ctx: [Cx; 19],
}

fn initial_contexts() -> [Cx; 19] {
    let mut c = [0; 19];
    c[0] = cx(4);
    c[RL] = cx(3);
    c[UNI] = cx(46);
    c
}

impl Block {
    #[inline]
    fn sig(&self, i: usize) -> u32 {
        (self.flags[i] & SIG) as u32
    }

    /// Significant horizontal, vertical and diagonal neighbours; `last`:
    /// the bottom row of a stripe, whose lower neighbours a vertically
    /// causal block ignores.
    #[inline]
    fn neighbours(&self, i: usize, last: bool) -> (u32, u32, u32) {
        let s = self.stride;
        let h = self.sig(i - 1) + self.sig(i + 1);
        let mut v = self.sig(i - s);
        let mut d = self.sig(i - s - 1) + self.sig(i - s + 1);
        if !(last && self.vcausal) {
            v += self.sig(i + s);
            d += self.sig(i + s - 1) + self.sig(i + s + 1);
        }
        (h, v, d)
    }

    /// Table D.1.
    fn zc(&self, h: u32, v: u32, d: u32) -> usize {
        fn hv(h: u32, v: u32, d: u32) -> usize {
            match h {
                2.. => 8,
                1 if v >= 1 => 7,
                1 if d >= 1 => 6,
                1 => 5,
                _ if v == 2 => 4,
                _ if v == 1 => 3,
                _ if d >= 2 => 2,
                _ => d as usize,
            }
        }
        match self.orient {
            Orient::LL | Orient::LH => hv(h, v, d),
            Orient::HL => hv(v, h, d),
            Orient::HH => {
                let hv = h + v;
                match d {
                    3.. => 8,
                    2 if hv >= 1 => 7,
                    2 => 6,
                    1 if hv >= 2 => 5,
                    1 if hv == 1 => 4,
                    1 => 3,
                    _ => hv.min(2) as usize,
                }
            }
        }
    }

    /// Table D.3: the context and the XOR bit of a sign.
    fn sc(&self, i: usize, last: bool) -> (usize, u32) {
        let s = self.stride;
        let c = |j: usize| -> i32 {
            match self.flags[j] & (SIG | NEG) {
                SIG => 1,
                f if f & SIG != 0 => -1,
                _ => 0,
            }
        };
        let h = (c(i - 1) + c(i + 1)).clamp(-1, 1);
        let below = if last && self.vcausal { 0 } else { c(i + s) };
        let v = (c(i - s) + below).clamp(-1, 1);
        match (h, v) {
            (1, 1) => (13, 0),
            (1, 0) => (12, 0),
            (1, _) => (11, 0),
            (0, 1) => (10, 0),
            (0, 0) => (9, 0),
            (0, _) => (10, 1),
            (_, 1) => (11, 1),
            (_, 0) => (12, 1),
            _ => (13, 1),
        }
    }

    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + x + 1
    }

    fn decode_sign(&mut self, c: &mut Coder, i: usize, last: bool) {
        let neg = match c {
            Coder::Mq(m) => {
                let (k, xor) = self.sc(i, last);
                m.bit(&mut self.ctx[k]) ^ xor
            }
            Coder::Raw(r) => r.bit(),
        };
        self.flags[i] |= SIG | if neg != 0 { NEG } else { 0 };
    }

    fn propagation(&mut self, c: &mut Coder, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                for y in y0..(y0 + 4).min(self.h) {
                    let i = self.idx(x, y);
                    if self.flags[i] & SIG != 0 {
                        continue;
                    }
                    let last = y % 4 == 3;
                    let (h, v, d) = self.neighbours(i, last);
                    if h + v + d == 0 {
                        continue;
                    }
                    let bit = match c {
                        Coder::Mq(m) => {
                            let k = self.zc(h, v, d);
                            m.bit(&mut self.ctx[k])
                        }
                        Coder::Raw(r) => r.bit(),
                    };
                    self.flags[i] |= VISITED;
                    if bit != 0 {
                        self.decode_sign(c, i, last);
                        self.mag[y * self.w + x] = 3 << p;
                    }
                }
            }
        }
    }

    fn refinement(&mut self, c: &mut Coder, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                for y in y0..(y0 + 4).min(self.h) {
                    let i = self.idx(x, y);
                    if self.flags[i] & (SIG | VISITED) != SIG {
                        continue;
                    }
                    let bit = match c {
                        Coder::Mq(m) => {
                            let k = if self.flags[i] & REFINED != 0 {
                                16
                            } else {
                                let (h, v, d) = self.neighbours(i, y % 4 == 3);
                                if h + v + d > 0 {
                                    15
                                } else {
                                    14
                                }
                            };
                            m.bit(&mut self.ctx[k])
                        }
                        Coder::Raw(r) => r.bit(),
                    };
                    let m = &mut self.mag[y * self.w + x];
                    if bit != 0 {
                        *m += 1 << p;
                    } else {
                        *m -= 1 << p;
                    }
                    self.flags[i] |= REFINED;
                }
            }
        }
    }

    fn mq_sign(&mut self, m: &mut Mq, i: usize, last: bool) {
        let (k, xor) = self.sc(i, last);
        let neg = m.bit(&mut self.ctx[k]) ^ xor;
        self.flags[i] |= SIG | if neg != 0 { NEG } else { 0 };
    }

    fn cleanup(&mut self, m: &mut Mq, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            let rows = (self.h - y0).min(4);
            for x in 0..self.w {
                let mut first = y0;
                // Run-length mode: a full column of four insignificant,
                // unvisited coefficients without significant neighbours.
                let quiet = rows == 4
                    && (y0..y0 + 4).all(|y| {
                        let i = self.idx(x, y);
                        if self.flags[i] & (SIG | VISITED) != 0 {
                            return false;
                        }
                        let (h, v, d) = self.neighbours(i, y % 4 == 3);
                        h + v + d == 0
                    });
                if quiet {
                    if m.bit(&mut self.ctx[RL]) == 0 {
                        continue;
                    }
                    let r = (m.bit(&mut self.ctx[UNI]) << 1 | m.bit(&mut self.ctx[UNI])) as usize;
                    let y = y0 + r;
                    let i = self.idx(x, y);
                    self.mq_sign(m, i, y % 4 == 3);
                    self.mag[y * self.w + x] = 3 << p;
                    first = y + 1;
                }
                for y in first..y0 + rows {
                    let i = self.idx(x, y);
                    if self.flags[i] & (SIG | VISITED) != 0 {
                        continue;
                    }
                    let last = y % 4 == 3;
                    let (h, v, d) = self.neighbours(i, last);
                    let k = self.zc(h, v, d);
                    if m.bit(&mut self.ctx[k]) != 0 {
                        self.mq_sign(m, i, last);
                        self.mag[y * self.w + x] = 3 << p;
                    }
                }
            }
        }
        for f in self.flags.iter_mut() {
            *f &= !VISITED;
        }
    }
}

/// Decodes a code-block of `w` by `h` with `planes` magnitude bit planes
/// (Mb plus any region of interest shift), of which the first `zero` are
/// empty, from codeword segments (data, passes). Returns signed doubled
/// magnitudes, row by row.
pub fn decode(
    w: usize,
    h: usize,
    orient: Orient,
    style: u8,
    planes: u32,
    zero: u32,
    segments: &[(Vec<u8>, u32)],
) -> Vec<i32> {
    let mut b = Block {
        w,
        h,
        stride: w + 2,
        flags: vec![0; (w + 2) * (h + 2)],
        mag: vec![0; w * h],
        orient,
        vcausal: style & VCAUSAL != 0,
        ctx: initial_contexts(),
    };
    if zero >= planes || planes > 30 {
        return b.mag;
    }
    let mut p = (planes - 1 - zero) as i32;
    let mut k = 0u32;
    'segments: for (data, passes) in segments {
        let mut coder = if is_raw(k, style) {
            Coder::Raw(Raw { data, pos: 0, c: 0, ct: 0 })
        } else {
            Coder::Mq(Mq::new(data))
        };
        for _ in 0..*passes {
            if p < 0 {
                break 'segments;
            }
            let pp = p as u32;
            match k % 3 {
                0 => {
                    let Coder::Mq(m) = &mut coder else { break 'segments };
                    b.cleanup(m, pp);
                    if style & SEGSYM != 0 {
                        for _ in 0..4 {
                            m.bit(&mut b.ctx[UNI]);
                        }
                    }
                    p -= 1;
                }
                1 => b.propagation(&mut coder, pp),
                _ => b.refinement(&mut coder, pp),
            }
            if style & RESET != 0 {
                b.ctx = initial_contexts();
            }
            k += 1;
        }
    }
    for (y, row) in b.mag.chunks_mut(w.max(1)).enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            if b.flags[(y + 1) * b.stride + x + 1] & NEG != 0 {
                *v = -*v;
            }
        }
    }
    b.mag
}
