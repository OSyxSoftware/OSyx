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

// Per-coefficient flags: which neighbours are significant (and negative),
// then the coefficient's own state.
const W: u16 = 1;
const E: u16 = 1 << 1;
const N: u16 = 1 << 2;
const S: u16 = 1 << 3;
const NW: u16 = 1 << 4;
const NE: u16 = 1 << 5;
const SW: u16 = 1 << 6;
const SE: u16 = 1 << 7;
const W_NEG: u16 = 1 << 8;
const E_NEG: u16 = 1 << 9;
const N_NEG: u16 = 1 << 10;
const S_NEG: u16 = 1 << 11;
const SIG: u16 = 1 << 12;
const NEG: u16 = 1 << 13;
const VISITED: u16 = 1 << 14;
const REFINED: u16 = 1 << 15;
/// The neighbours below, which a vertically causal block ignores on the
/// last row of a stripe.
const BELOW: u16 = S | SW | SE | S_NEG;

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

/// Table D.1: zero coding contexts by the eight neighbour bits.
fn zc_table(orient: Orient) -> [u8; 256] {
    fn hv(h: u32, v: u32, d: u32) -> u8 {
        match h {
            2.. => 8,
            1 if v >= 1 => 7,
            1 if d >= 1 => 6,
            1 => 5,
            _ if v == 2 => 4,
            _ if v == 1 => 3,
            _ if d >= 2 => 2,
            _ => d as u8,
        }
    }
    let mut t = [0u8; 256];
    for (f, e) in t.iter_mut().enumerate() {
        let f = f as u16;
        let bit = |m: u16| (f & m != 0) as u32;
        let (h, v, d) = (bit(W) + bit(E), bit(N) + bit(S), bit(NW) + bit(NE) + bit(SW) + bit(SE));
        *e = match orient {
            Orient::LL | Orient::LH => hv(h, v, d),
            Orient::HL => hv(v, h, d),
            Orient::HH => match (d, h + v) {
                (3.., _) => 8,
                (2, 1..) => 7,
                (2, _) => 6,
                (1, 2..) => 5,
                (1, 1) => 4,
                (1, _) => 3,
                (_, hv) => hv.min(2) as u8,
            },
        };
    }
    t
}

/// Table D.3: sign contexts and XOR bits by the four direct neighbours'
/// significance (low nibble) and signs (high nibble).
fn sc_table() -> [(u8, u8); 256] {
    let mut t = [(0, 0); 256];
    for (f, e) in t.iter_mut().enumerate() {
        let c = |sig: usize, neg: usize| -> i32 {
            match (f & sig != 0, f & neg != 0) {
                (true, false) => 1,
                (true, true) => -1,
                _ => 0,
            }
        };
        let h = (c(1, 16) + c(2, 32)).clamp(-1, 1);
        let v = (c(4, 64) + c(8, 128)).clamp(-1, 1);
        *e = match (h, v) {
            (1, 1) => (13, 0),
            (1, 0) => (12, 0),
            (1, _) => (11, 0),
            (0, 1) => (10, 0),
            (0, 0) => (9, 0),
            (0, _) => (10, 1),
            (_, 1) => (11, 1),
            (_, 0) => (12, 1),
            _ => (13, 1),
        };
    }
    t
}

struct Block {
    w: usize,
    h: usize,
    stride: usize,
    flags: Vec<u16>,
    mag: Vec<i32>,
    vcausal: bool,
    ctx: [Cx; 19],
    zc: [u8; 256],
    sc: [(u8, u8); 256],
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
    fn idx(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + x + 1
    }

    /// The flags of coefficient `i` on row `y`, without the neighbours a
    /// vertically causal block may not look at.
    #[inline]
    fn seen(&self, i: usize, y: usize) -> u16 {
        let f = self.flags[i];
        if self.vcausal && y % 4 == 3 {
            f & !BELOW
        } else {
            f
        }
    }

    /// Marks coefficient `i` significant and tells its neighbours.
    #[inline]
    fn set_sig(&mut self, i: usize, neg: bool, x: usize, y: usize, p: u32) {
        let s = self.stride;
        let f = &mut self.flags;
        f[i] |= SIG | if neg { NEG } else { 0 };
        f[i - 1] |= E | if neg { E_NEG } else { 0 };
        f[i + 1] |= W | if neg { W_NEG } else { 0 };
        f[i - s] |= S | if neg { S_NEG } else { 0 };
        f[i + s] |= N | if neg { N_NEG } else { 0 };
        f[i - s - 1] |= SE;
        f[i - s + 1] |= SW;
        f[i + s - 1] |= NE;
        f[i + s + 1] |= NW;
        self.mag[y * self.w + x] = 3 << p;
    }

    #[inline]
    fn sign_mq(&mut self, m: &mut Mq, f: u16) -> bool {
        let (k, xor) = self.sc[((f & 0xF) | ((f >> 4) & 0xF0)) as usize];
        m.bit(&mut self.ctx[k as usize]) ^ xor as u32 != 0
    }

    fn propagation(&mut self, c: &mut Coder, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                for y in y0..(y0 + 4).min(self.h) {
                    let i = self.idx(x, y);
                    let f = self.seen(i, y);
                    if f & SIG != 0 || f & 0xFF == 0 {
                        continue;
                    }
                    let neg = match c {
                        Coder::Mq(m) => {
                            if m.bit(&mut self.ctx[self.zc[(f & 0xFF) as usize] as usize]) == 0 {
                                None
                            } else {
                                Some(self.sign_mq(m, f))
                            }
                        }
                        Coder::Raw(r) => (r.bit() != 0).then(|| r.bit() != 0),
                    };
                    self.flags[i] |= VISITED;
                    if let Some(neg) = neg {
                        self.set_sig(i, neg, x, y, p);
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
                    let f = self.seen(i, y);
                    if f & (SIG | VISITED) != SIG {
                        continue;
                    }
                    let bit = match c {
                        Coder::Mq(m) => {
                            let k = if f & REFINED != 0 {
                                16
                            } else if f & 0xFF != 0 {
                                15
                            } else {
                                14
                            };
                            m.bit(&mut self.ctx[k])
                        }
                        Coder::Raw(r) => r.bit(),
                    };
                    let v = &mut self.mag[y * self.w + x];
                    if bit != 0 {
                        *v += 1 << p;
                    } else {
                        *v -= 1 << p;
                    }
                    self.flags[i] |= REFINED;
                }
            }
        }
    }

    fn cleanup(&mut self, m: &mut Mq, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            let rows = (self.h - y0).min(4);
            for x in 0..self.w {
                let mut first = y0;
                // Run-length mode: a full column of four insignificant,
                // unvisited coefficients without significant neighbours.
                let quiet =
                    rows == 4 && (y0..y0 + 4).all(|y| self.seen(self.idx(x, y), y) & (SIG | VISITED | 0xFF) == 0);
                if quiet {
                    if m.bit(&mut self.ctx[RL]) == 0 {
                        continue;
                    }
                    let r = (m.bit(&mut self.ctx[UNI]) << 1 | m.bit(&mut self.ctx[UNI])) as usize;
                    let y = y0 + r;
                    let i = self.idx(x, y);
                    let f = self.seen(i, y);
                    let neg = self.sign_mq(m, f);
                    self.set_sig(i, neg, x, y, p);
                    first = y + 1;
                }
                for y in first..y0 + rows {
                    let i = self.idx(x, y);
                    let f = self.seen(i, y);
                    if f & (SIG | VISITED) != 0 {
                        continue;
                    }
                    if m.bit(&mut self.ctx[self.zc[(f & 0xFF) as usize] as usize]) != 0 {
                        let neg = self.sign_mq(m, f);
                        self.set_sig(i, neg, x, y, p);
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
        vcausal: style & VCAUSAL != 0,
        ctx: initial_contexts(),
        zc: zc_table(orient),
        sc: sc_table(),
    };
    if zero >= planes || planes > 30 {
        return b.mag;
    }
    let mut p = (planes - 1 - zero) as i32;
    let mut k = 0u32;
    'segments: for (data, passes) in segments {
        let mut coder =
            if is_raw(k, style) { Coder::Raw(Raw { data, pos: 0, c: 0, ct: 0 }) } else { Coder::Mq(Mq::new(data)) };
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
