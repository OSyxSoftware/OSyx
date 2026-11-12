//! A small VP8 encoder for screen recordings (RFC 6386).
//!
//! Key frames predict each macroblock from its neighbours (DC, vertical,
//! horizontal or TrueMotion, whichever is nearest) and inter frames predict
//! it from the same place in the previous frame (the zero motion vector);
//! a macroblock that did not change costs a fraction of a bit. There is no
//! motion search, no 4x4 prediction and no loop filter, and the default
//! coefficient probabilities are used as they are: the stream is larger
//! than libvpx would make it and every decoder plays it.
//!
//! The encoder reconstructs each frame exactly as a decoder will, so the
//! prediction never drifts.

use super::tables::*;
use super::{ac_q, dc_q, idct_add, walsh, ABOVE_CTX, BANDS, CAT_BASE, CAT_PROBS, LEFT_CTX, ZIGZAG};
use alloc::vec;
use alloc::vec::Vec;

/// The boolean entropy encoder (RFC 6386, 7.3).
struct BoolEnc {
    out: Vec<u8>,
    range: u32,
    bottom: u32,
    bit_count: i32,
}

impl BoolEnc {
    fn new() -> BoolEnc {
        BoolEnc { out: Vec::new(), range: 255, bottom: 0, bit_count: 24 }
    }

    /// Propagates a carry into the bytes already written.
    fn carry(&mut self) {
        for b in self.out.iter_mut().rev() {
            if *b == 255 {
                *b = 0;
            } else {
                *b += 1;
                break;
            }
        }
    }

    #[inline]
    fn put(&mut self, prob: u8, bit: bool) {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        if bit {
            self.bottom = self.bottom.wrapping_add(split);
            if self.bottom < split {
                // The addition overflowed 32 bits.
                self.carry();
            }
            self.range -= split;
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.range <<= 1;
            if self.bottom & (1 << 31) != 0 {
                self.carry();
            }
            self.bottom <<= 1;
            self.bit_count -= 1;
            if self.bit_count == 0 {
                self.out.push((self.bottom >> 24) as u8);
                self.bottom &= (1 << 24) - 1;
                self.bit_count = 8;
            }
        }
    }

    fn bit(&mut self, bit: bool) {
        self.put(128, bit);
    }

    fn uint(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.bit(value >> i & 1 != 0);
        }
    }

    /// Writes the leaf `value` of `tree` (the decoder's `Bool::tree`).
    fn tree(&mut self, tree: &[i16], probs: &[u8], value: u8) {
        // (node, bit) pairs from the root to the leaf.
        fn path(tree: &[i16], node: usize, value: u8, out: &mut Vec<(usize, bool)>) -> bool {
            for bit in 0..2 {
                let next = tree[node + bit];
                out.push((node, bit == 1));
                if next <= 0 {
                    if (-next) as u8 == value {
                        return true;
                    }
                } else if path(tree, next as usize, value, out) {
                    return true;
                }
                out.pop();
            }
            false
        }
        let mut steps = Vec::new();
        let found = path(tree, 0, value, &mut steps);
        debug_assert!(found, "value is not a leaf of the tree");
        for (node, bit) in steps {
            self.put(probs[node >> 1], bit);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        let mut c = self.bit_count;
        let mut v = self.bottom;
        if v & (1 << (32 - c)) != 0 {
            self.carry();
        }
        v <<= c & 7;
        c >>= 3;
        while c > 0 {
            v <<= 8;
            c -= 1;
        }
        for _ in 0..4 {
            self.out.push((v >> 24) as u8);
            v <<= 8;
        }
        self.out
    }
}

/// The forward 4x4 DCT of libvpx (`vp8_short_fdct4x4_c`); the inverse is
/// `idct_add`.
fn fdct(input: &[i16; 16]) -> [i16; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = &input[4 * i..4 * i + 4];
        let a1 = (ip[0] as i32 + ip[3] as i32) * 8;
        let b1 = (ip[1] as i32 + ip[2] as i32) * 8;
        let c1 = (ip[1] as i32 - ip[2] as i32) * 8;
        let d1 = (ip[0] as i32 - ip[3] as i32) * 8;
        t[4 * i] = a1 + b1;
        t[4 * i + 2] = a1 - b1;
        t[4 * i + 1] = (c1 * 2217 + d1 * 5352 + 14500) >> 12;
        t[4 * i + 3] = (d1 * 2217 - c1 * 5352 + 7500) >> 12;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let a1 = t[i] + t[12 + i];
        let b1 = t[4 + i] + t[8 + i];
        let c1 = t[4 + i] - t[8 + i];
        let d1 = t[i] - t[12 + i];
        out[i] = ((a1 + b1 + 7) >> 4) as i16;
        out[8 + i] = ((a1 - b1 + 7) >> 4) as i16;
        out[4 + i] = (((c1 * 2217 + d1 * 5352 + 12000) >> 16) + (d1 != 0) as i32) as i16;
        out[12 + i] = ((d1 * 2217 - c1 * 5352 + 51000) >> 16) as i16;
    }
    out
}

/// The forward Walsh-Hadamard transform of the sixteen luma DC values
/// (`vp8_short_walsh4x4_c`); the inverse is `walsh`.
fn fwht(input: &[i16; 16]) -> [i16; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = &input[4 * i..4 * i + 4];
        let a1 = (ip[0] as i32 + ip[2] as i32) * 4;
        let d1 = (ip[1] as i32 + ip[3] as i32) * 4;
        let c1 = (ip[1] as i32 - ip[3] as i32) * 4;
        let b1 = (ip[0] as i32 - ip[2] as i32) * 4;
        t[4 * i] = a1 + d1 + (a1 != 0) as i32;
        t[4 * i + 1] = b1 + c1;
        t[4 * i + 2] = b1 - c1;
        t[4 * i + 3] = a1 - d1;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let a1 = t[i] + t[8 + i];
        let d1 = t[4 + i] + t[12 + i];
        let c1 = t[4 + i] - t[12 + i];
        let b1 = t[i] - t[8 + i];
        let fix = |v: i32| ((v + (v < 0) as i32 + 3) >> 3) as i16;
        out[i] = fix(a1 + d1);
        out[4 + i] = fix(b1 + c1);
        out[8 + i] = fix(b1 - c1);
        out[12 + i] = fix(a1 - d1);
    }
    out
}

/// Quantizes `c` (natural order) from index `first` on; returns whether any
/// level is not zero. `exact` rounds to the nearest level, for a picture
/// that changed; otherwise small coefficients round down more than up, so
/// that the error the last frame's quantization left in a still picture is
/// not coded again and again.
fn quantize(c: &mut [i16; 16], first: usize, dq: [i32; 2], exact: bool) -> bool {
    let mut any = false;
    for (i, v) in c.iter_mut().enumerate() {
        if i < first {
            *v = 0;
            continue;
        }
        let q = dq[(i > 0) as usize];
        let level = ((*v as i32).abs() + if exact { q / 2 } else { q / 4 }) / q;
        let level = level.min(2047);
        *v = if *v < 0 { -level } else { level } as i16;
        any |= level != 0;
    }
    any
}

/// Writes one block of quantized levels (natural order) as tokens, the
/// mirror of the decoder's `block_tokens`; returns whether the block has a
/// coefficient.
fn write_block(b: &mut BoolEnc, probs: &[[[u8; 11]; 3]; 8], ctx: usize, first: usize, levels: &[i16; 16]) -> bool {
    // One past the last coefficient that is not zero, in zigzag order.
    let end = (first..16).rev().find(|&c| levels[ZIGZAG[c]] != 0).map_or(first, |c| c + 1);
    let mut p = &probs[BANDS[first]][ctx];
    let mut check_eob = true;
    for c in first..16 {
        if c == end {
            // End of block.
            b.put(p[0], false);
            return end > first;
        }
        if check_eob {
            b.put(p[0], true);
        }
        let v = levels[ZIGZAG[c]] as i32;
        if v == 0 {
            b.put(p[1], false);
            p = &probs[BANDS[c + 1]][0];
            check_eob = false;
            continue;
        }
        b.put(p[1], true);
        let a = v.abs();
        let next_ctx = if a == 1 {
            b.put(p[2], false);
            1
        } else {
            b.put(p[2], true);
            if a <= 4 {
                b.put(p[3], false);
                if a == 2 {
                    b.put(p[4], false);
                } else {
                    b.put(p[4], true);
                    b.put(p[5], a == 4);
                }
            } else {
                b.put(p[3], true);
                if a <= 10 {
                    b.put(p[6], false);
                    if a <= 6 {
                        b.put(p[7], false);
                        b.put(159, a == 6);
                    } else {
                        b.put(p[7], true);
                        b.put(165, (a - 7) >> 1 != 0);
                        b.put(145, (a - 7) & 1 != 0);
                    }
                } else {
                    b.put(p[6], true);
                    let cat = (2..6).rev().find(|&k| a >= CAT_BASE[k]).unwrap_or(2);
                    if cat < 4 {
                        b.put(p[8], false);
                        b.put(p[9], cat == 3);
                    } else {
                        b.put(p[8], true);
                        b.put(p[10], cat == 5);
                    }
                    let extra = a - CAT_BASE[cat];
                    let n = CAT_PROBS[cat].len();
                    for (k, &pr) in CAT_PROBS[cat].iter().enumerate() {
                        b.put(pr, extra >> (n - 1 - k) & 1 != 0);
                    }
                }
            }
            2
        };
        b.bit(v < 0);
        if c < 15 {
            p = &probs[BANDS[c + 1]][next_ctx];
        }
        check_eob = true;
    }
    true
}

const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const H_PRED: u8 = 2;
const TM_PRED: u8 = 3;
const ZEROMV: u8 = 7;

/// One plane, as wide and high as whole macroblocks.
struct Plane {
    px: Vec<u8>,
    stride: usize,
}

impl Plane {
    fn new(w: usize, h: usize) -> Plane {
        Plane { px: vec![0; w * h], stride: w }
    }

    /// The intra prediction of the `n` x `n` block at (`x`, `y`) by `mode`
    /// from the reconstructed pixels above and to the left.
    fn predict(&self, x: usize, y: usize, n: usize, mode: u8, out: &mut [u8]) {
        let s = self.stride;
        let at = y * s + x;
        match mode {
            V_PRED => {
                for r in 0..n {
                    out[r * n..r * n + n].copy_from_slice(&self.px[at - s..at - s + n]);
                }
            }
            H_PRED => {
                for r in 0..n {
                    out[r * n..r * n + n].fill(self.px[at + r * s - 1]);
                }
            }
            TM_PRED => {
                let corner = self.px[at - s - 1] as i32;
                for r in 0..n {
                    let left = self.px[at + r * s - 1] as i32;
                    for c in 0..n {
                        out[r * n + c] = (left + self.px[at - s + c] as i32 - corner).clamp(0, 255) as u8;
                    }
                }
            }
            _ => {
                let shift = n.trailing_zeros();
                let (mut sum, mut count) = (0u32, 0u32);
                if y > 0 {
                    sum += self.px[at - s..at - s + n].iter().map(|&v| v as u32).sum::<u32>();
                    count += 1;
                }
                if x > 0 {
                    sum += (0..n).map(|r| self.px[at + r * s - 1] as u32).sum::<u32>();
                    count += 1;
                }
                let dc = match count {
                    2 => (sum + n as u32) >> (shift + 1),
                    1 => (sum + (n as u32 >> 1)) >> shift,
                    _ => 128,
                };
                out[..n * n].fill(dc as u8);
            }
        }
    }

    fn copy_out(&self, x: usize, y: usize, n: usize, out: &mut [u8]) {
        for r in 0..n {
            let at = (y + r) * self.stride + x;
            out[r * n..r * n + n].copy_from_slice(&self.px[at..at + n]);
        }
    }
}

fn sad(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u32).sum()
}

/// What was decided for one macroblock.
#[derive(Clone, Copy, Default)]
struct Mb {
    y_mode: u8,
    uv_mode: u8,
    skip: bool,
}

/// One encoded frame.
pub struct Packet {
    pub data: Vec<u8>,
    pub key: bool,
}

/// Encodes a sequence of pictures of one size.
pub struct Encoder {
    w: usize,
    h: usize,
    mb_cols: usize,
    mb_rows: usize,
    q_index: u8,
    /// The source picture being encoded, and the previous one.
    src: [Plane; 3],
    prev: [Plane; 3],
    /// What a decoder shows after the frames so far.
    recon: [Plane; 3],
    /// Macroblocks whose reconstruction needed no correction last time.
    settled: Vec<bool>,
    have_ref: bool,
}

impl Encoder {
    /// An encoder for `width` x `height` pictures. `q_index` (0 to 127) is
    /// VP8's quantizer index: lower is sharper and larger.
    pub fn new(width: usize, height: usize, q_index: u8) -> Option<Encoder> {
        if width == 0 || height == 0 || width > 16383 || height > 16383 {
            return None;
        }
        let (mb_cols, mb_rows) = (width.div_ceil(16), height.div_ceil(16));
        let planes = || [Plane::new(mb_cols * 16, mb_rows * 16), Plane::new(mb_cols * 8, mb_rows * 8), Plane::new(mb_cols * 8, mb_rows * 8)];
        Some(Encoder {
            w: width,
            h: height,
            mb_cols,
            mb_rows,
            q_index: q_index.min(127),
            src: planes(),
            prev: planes(),
            recon: planes(),
            settled: vec![false; mb_cols * mb_rows],
            have_ref: false,
        })
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    /// The picture a decoder shows after the last frame, as Y, U and V
    /// planes with their strides (for tests).
    pub fn reconstruction(&self) -> [(&[u8], usize); 3] {
        [(&self.recon[0].px[..], self.recon[0].stride), (&self.recon[1].px[..], self.recon[1].stride), (&self.recon[2].px[..], self.recon[2].stride)]
    }

    /// Loads `pixels` (0x00RRGGBB, rows of `stride`) as BT.601 limited-range
    /// 4:2:0, repeating the last column and row up to whole macroblocks.
    fn load(&mut self, pixels: &[u32], stride: usize) {
        core::mem::swap(&mut self.src, &mut self.prev);
        let (pw, ph) = (self.mb_cols * 16, self.mb_rows * 16);
        let (w, h) = (self.w, self.h);
        let [py, pu, pv] = &mut self.src;
        let mut sums = vec![[0i32; 3]; pw / 2];
        for y in 0..ph {
            let row = &pixels[y.min(h - 1) * stride..];
            if y & 1 == 0 {
                sums.fill([0; 3]);
            }
            let out = &mut py.px[y * pw..(y + 1) * pw];
            for (x, o) in out.iter_mut().enumerate() {
                let p = row[x.min(w - 1)];
                let (r, g, b) = ((p >> 16 & 255) as i32, (p >> 8 & 255) as i32, (p & 255) as i32);
                *o = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
                let s = &mut sums[x / 2];
                s[0] += r;
                s[1] += g;
                s[2] += b;
            }
            if y & 1 == 1 {
                let at = (y / 2) * (pw / 2);
                for (x, s) in sums.iter().enumerate() {
                    // Sums of four pixels: the weights below are a quarter
                    // of the usual ones' scale.
                    let (r, g, b) = (s[0], s[1], s[2]);
                    pu.px[at + x] = (((-38 * r - 74 * g + 112 * b + 512) >> 10) + 128) as u8;
                    pv.px[at + x] = (((112 * r - 94 * g - 18 * b + 512) >> 10) + 128) as u8;
                }
            }
        }
    }

    /// Encodes the next picture: `pixels` are 0x00RRGGBB in rows of
    /// `stride`. The first frame is a key frame, and so is any frame with
    /// `key` set; the others depend on the frame before them.
    pub fn encode(&mut self, pixels: &[u32], stride: usize, key: bool) -> Packet {
        self.load(pixels, stride);
        let key = key || !self.have_ref;
        let q = self.q_index as i32;
        // Factors for Y1, UV and Y2, as the decoder derives them.
        let dq = [[dc_q(q), ac_q(q)], [dc_q(q).min(132), ac_q(q)], [dc_q(q) * 2, (ac_q(q) * 155 / 100).max(8)]];
        let probs = &K_DEFAULT_COEFF_PROBS;
        let mut tokens = BoolEnc::new();
        let mut mbs = vec![Mb::default(); self.mb_cols * self.mb_rows];
        let mut above_ctx = vec![[0u8; 9]; self.mb_cols];
        let (mut pred_y, mut pred_u, mut pred_v) = ([0u8; 256], [0u8; 64], [0u8; 64]);
        let (mut src_y, mut src_u, mut src_v) = ([0u8; 256], [0u8; 64], [0u8; 64]);
        let mut trial = [0u8; 256];
        for row in 0..self.mb_rows {
            let mut left_ctx = [0u8; 9];
            for col in 0..self.mb_cols {
                let i = row * self.mb_cols + col;
                let (x, y) = (col * 16, row * 16);
                self.src[0].copy_out(x, y, 16, &mut src_y);
                self.src[1].copy_out(x / 2, y / 2, 8, &mut src_u);
                self.src[2].copy_out(x / 2, y / 2, 8, &mut src_v);
                let mut mb = Mb { y_mode: ZEROMV, uv_mode: ZEROMV, skip: false };
                // Whether this part of the picture is new: then it is coded
                // as exactly as the quantizer allows.
                let mut exact = true;
                if key {
                    // The nearest of the modes whose neighbours exist.
                    let allowed: &[u8] = match (row > 0, col > 0) {
                        (true, true) => &[DC_PRED, V_PRED, H_PRED, TM_PRED],
                        (true, false) => &[DC_PRED, V_PRED],
                        (false, true) => &[DC_PRED, H_PRED],
                        (false, false) => &[DC_PRED],
                    };
                    let mut best = u32::MAX;
                    for &mode in allowed {
                        self.recon[0].predict(x, y, 16, mode, &mut trial);
                        let d = sad(&trial, &src_y);
                        if d < best {
                            best = d;
                            mb.y_mode = mode;
                            pred_y = trial;
                        }
                    }
                    let mut best = u32::MAX;
                    for &mode in allowed {
                        let (mut tu, mut tv) = ([0u8; 64], [0u8; 64]);
                        self.recon[1].predict(x / 2, y / 2, 8, mode, &mut tu);
                        self.recon[2].predict(x / 2, y / 2, 8, mode, &mut tv);
                        let d = sad(&tu, &src_u) + sad(&tv, &src_v);
                        if d < best {
                            best = d;
                            mb.uv_mode = mode;
                            pred_u = tu;
                            pred_v = tv;
                        }
                    }
                } else {
                    // An unchanged macroblock that needed nothing last time
                    // needs nothing now.
                    let same = |p: usize, x: usize, y: usize, n: usize| {
                        (0..n).all(|r| {
                            let at = (y + r) * self.src[p].stride + x;
                            self.src[p].px[at..at + n] == self.prev[p].px[at..at + n]
                        })
                    };
                    let still = same(0, x, y, 16) && same(1, x / 2, y / 2, 8) && same(2, x / 2, y / 2, 8);
                    exact = !still;
                    if self.settled[i] && still {
                        mb.skip = true;
                        mbs[i] = mb;
                        left_ctx = [0; 9];
                        above_ctx[col] = [0; 9];
                        continue;
                    }
                    self.recon[0].copy_out(x, y, 16, &mut pred_y);
                    self.recon[1].copy_out(x / 2, y / 2, 8, &mut pred_u);
                    self.recon[2].copy_out(x / 2, y / 2, 8, &mut pred_v);
                }
                // Transform and quantize: 16 luma blocks without their DC,
                // the block of those DCs, then 4 + 4 chroma blocks.
                let mut levels = [[0i16; 16]; 25];
                let mut any = false;
                let mut dcs = [0i16; 16];
                let residual = |src: &[u8], pred: &[u8], n: usize, bx: usize, by: usize| {
                    let mut d = [0i16; 16];
                    for r in 0..4 {
                        for c in 0..4 {
                            let at = (by * 4 + r) * n + bx * 4 + c;
                            d[r * 4 + c] = src[at] as i16 - pred[at] as i16;
                        }
                    }
                    d
                };
                for b in 0..16 {
                    let mut c = fdct(&residual(&src_y, &pred_y, 16, b & 3, b >> 2));
                    dcs[b] = c[0];
                    any |= quantize(&mut c, 1, dq[0], exact);
                    levels[b] = c;
                }
                levels[24] = fwht(&dcs);
                any |= quantize(&mut levels[24], 0, dq[2], exact);
                for b in 0..4 {
                    let mut c = fdct(&residual(&src_u, &pred_u, 8, b & 1, b >> 1));
                    any |= quantize(&mut c, 0, dq[1], exact);
                    levels[16 + b] = c;
                    let mut c = fdct(&residual(&src_v, &pred_v, 8, b & 1, b >> 1));
                    any |= quantize(&mut c, 0, dq[1], exact);
                    levels[20 + b] = c;
                }
                mb.skip = !any;
                self.settled[i] = !any;
                mbs[i] = mb;
                if !any {
                    left_ctx = [0; 9];
                    above_ctx[col] = [0; 9];
                } else {
                    let mut run = |b: usize, kind: usize, first: usize| {
                        let ctx = (left_ctx[LEFT_CTX[b]] + above_ctx[col][ABOVE_CTX[b]]) as usize;
                        let nonzero = write_block(&mut tokens, &probs[kind], ctx, first, &levels[b]);
                        left_ctx[LEFT_CTX[b]] = nonzero as u8;
                        above_ctx[col][ABOVE_CTX[b]] = nonzero as u8;
                    };
                    run(24, 1, 0);
                    for b in 0..16 {
                        run(b, 0, 1);
                    }
                    for b in 16..24 {
                        run(b, 2, 0);
                    }
                }
                // Reconstruct as the decoder does.
                let mut y2 = [0i16; 16];
                for (k, v) in levels[24].iter().enumerate() {
                    y2[k] = (*v as i32 * dq[2][(k > 0) as usize]) as i16;
                }
                let mut dc = [0i16; 16];
                walsh(&y2, &mut dc);
                let put = |plane: &mut Plane, px: usize, py: usize, pred: &[u8], n: usize, b: (usize, usize), levels: &[i16; 16], f: [i32; 2], dc: Option<i16>| {
                    let mut c = [0i16; 16];
                    for (k, v) in levels.iter().enumerate() {
                        c[k] = (*v as i32 * f[(k > 0) as usize]) as i16;
                    }
                    if let Some(dc) = dc {
                        c[0] = dc;
                    }
                    let stride = plane.stride;
                    let o = (py + b.1 * 4) * stride + px + b.0 * 4;
                    idct_add(&mut plane.px, o, stride, pred, b.1 * 4 * n + b.0 * 4, n, &c);
                };
                let [ry, ru, rv] = &mut self.recon;
                for b in 0..16 {
                    put(ry, x, y, &pred_y, 16, (b & 3, b >> 2), &levels[b], dq[0], Some(dc[b]));
                }
                for b in 0..4 {
                    put(ru, x / 2, y / 2, &pred_u, 8, (b & 1, b >> 1), &levels[16 + b], dq[1], None);
                    put(rv, x / 2, y / 2, &pred_v, 8, (b & 1, b >> 1), &levels[20 + b], dq[1], None);
                }
            }
        }
        self.have_ref = true;
        // The first partition: the frame header and each macroblock's mode.
        let coded = mbs.iter().filter(|m| !m.skip).count();
        let skip_prob = ((coded * 256) / mbs.len()).clamp(1, 255) as u8;
        let mut b = BoolEnc::new();
        if key {
            b.uint(0, 2); // colour space, clamping needed
        }
        b.bit(false); // no segmentation
        b.bit(false); // normal loop filter
        b.uint(0, 6); // loop filter level: off
        b.uint(0, 3); // sharpness
        b.bit(false); // no loop filter deltas
        b.uint(0, 2); // one token partition
        b.uint(self.q_index as u32, 7);
        for _ in 0..5 {
            b.bit(false); // no quantizer deltas
        }
        if !key {
            b.bit(false); // golden frame not refreshed
            b.bit(false); // alternate frame not refreshed
            b.uint(0, 2); // nothing copied to the golden frame
            b.uint(0, 2); // nothing copied to the alternate frame
            b.bit(false); // sign bias, golden
            b.bit(false); // sign bias, alternate
        }
        b.bit(false); // probabilities are not kept for later frames
        if !key {
            b.bit(true); // this frame becomes the last frame
        }
        for i in 0..4 {
            for j in 0..8 {
                for k in 0..3 {
                    for l in 0..11 {
                        b.put(K_COEFF_ENTROPY_UPDATE_PROBS[i][j][k][l], false);
                    }
                }
            }
        }
        b.bit(true); // macroblocks may be skipped
        b.uint(skip_prob as u32, 8);
        if !key {
            b.uint(1, 8); // every macroblock is predicted from a frame,
            b.uint(255, 8); // the last one
            b.uint(128, 8);
            b.bit(false); // luma mode probabilities unchanged
            b.bit(false); // chroma mode probabilities unchanged
            for i in 0..2 {
                for j in 0..19 {
                    b.put(K_MV_ENTROPY_UPDATE_PROBS[i][j], false);
                }
            }
        }
        for row in 0..self.mb_rows {
            for col in 0..self.mb_cols {
                let mb = mbs[row * self.mb_cols + col];
                b.put(skip_prob, mb.skip);
                if key {
                    b.tree(&KF_Y_MODE_TREE, &KF_Y_MODE_PROBS, mb.y_mode);
                    b.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS, mb.uv_mode);
                } else {
                    b.put(1, true); // predicted from a frame
                    b.put(255, false); // the last one
                    // The zero vector's probability depends on how many
                    // neighbours used it: all that exist did.
                    let count = 2 * (row > 0) as usize + 2 * (col > 0) as usize + (row > 0 && col > 0) as usize;
                    b.put(MV_COUNTS_TO_PROBS[count][0], false);
                }
            }
        }
        let first = b.finish();
        let tokens = tokens.finish();
        let mut data = Vec::with_capacity(10 + first.len() + tokens.len());
        // Frame tag: key frame bit (0 for a key frame), version 0, shown,
        // size of the first partition.
        let tag = (!key) as u32 | 1 << 4 | (first.len() as u32) << 5;
        data.extend_from_slice(&tag.to_le_bytes()[..3]);
        if key {
            data.extend_from_slice(&[0x9d, 0x01, 0x2a]);
            data.extend_from_slice(&(self.w as u16).to_le_bytes());
            data.extend_from_slice(&(self.h as u16).to_le_bytes());
        }
        data.extend_from_slice(&first);
        data.extend_from_slice(&tokens);
        Packet { data, key }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vp8::Decoder;

    /// A picture like a desktop: a gradient, flat panels, sharp text-like
    /// stripes, and a box that moves with `t`.
    fn picture(w: usize, h: usize, t: usize) -> Vec<u32> {
        let mut px = vec![0u32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut c = ((x * 255 / w) as u32) << 16 | ((y * 255 / h) as u32) << 8 | 0x60;
                if y > h / 2 && x < w / 2 {
                    c = if (x / 3 + y / 5) % 2 == 0 { 0x00f0_f0f0 } else { 0x0020_2428 };
                }
                let (bx, by) = (10 + t * 7, 8 + t * 3);
                if x >= bx && x < bx + 40 && y >= by && y < by + 24 {
                    c = 0x004c_8dff;
                }
                px[y * w + x] = c;
            }
        }
        px
    }

    /// Encodes `frames` pictures and checks that the decoder shows exactly
    /// what the encoder reconstructed; returns the sizes of the frames.
    fn round_trip(w: usize, h: usize, q: u8, frames: usize, change: bool) -> Vec<usize> {
        let mut enc = Encoder::new(w, h, q).unwrap();
        let mut dec = Decoder::new();
        let mut sizes = Vec::new();
        for t in 0..frames {
            let src = picture(w, h, if change { t } else { 0 });
            let packet = enc.encode(&src, w, false);
            assert_eq!(packet.key, t == 0);
            dec.decode(&packet.data).unwrap_or_else(|e| panic!("frame {t}: {e}"));
            let f = dec.frame().unwrap();
            assert_eq!((f.w, f.h), (w, h));
            let [(ry, rys), (ru, rus), (rv, rvs)] = enc.reconstruction();
            for y in 0..h {
                assert_eq!(&f.y[y * f.y_stride..y * f.y_stride + w], &ry[y * rys..y * rys + w], "frame {t}, luma row {y}");
            }
            for y in 0..h.div_ceil(2) {
                let n = w.div_ceil(2);
                assert_eq!(&f.u[y * f.uv_stride..y * f.uv_stride + n], &ru[y * rus..y * rus + n], "frame {t}, U row {y}");
                assert_eq!(&f.v[y * f.uv_stride..y * f.uv_stride + n], &rv[y * rvs..y * rvs + n], "frame {t}, V row {y}");
            }
            // The picture is close to the source.
            let rgba = crate::vp8::to_rgba(&f);
            let mut err = 0u64;
            for (p, s) in rgba.iter().zip(&src) {
                for (k, shift) in [16, 8, 0].iter().enumerate() {
                    let d = p[k] as i64 - (s >> shift & 255) as i64;
                    err += (d * d) as u64;
                }
            }
            let mse = err as f64 / (w * h * 3) as f64;
            let psnr = 10.0 * libm::log10(255.0 * 255.0 / mse.max(1e-9));
            // (A one-pixel picture is all rounding, a tiny one mostly chroma
            // subsampling of steep colour changes; the coarsest quantizer
            // keeps little of the stripes.)
            let by_q = if q <= 20 { 30.0 } else if q <= 60 { 22.0 } else { 14.0 };
            let floor = if w * h < 256 { 5.0 } else if w * h < 4096 { by_q - 10.0 } else { by_q };
            assert!(psnr > floor, "{w}x{h} frame {t}: {psnr:.1} dB at q {q}");
            sizes.push(packet.data.len());
        }
        sizes
    }

    #[test]
    fn the_decoder_shows_what_the_encoder_reconstructed() {
        // Sizes that are and are not whole macroblocks, sharp and coarse.
        for (w, h) in [(160, 96), (150, 90), (17, 33), (16, 16), (1, 1)] {
            for q in [4, 20, 60, 127] {
                round_trip(w, h, q, 6, true);
            }
        }
    }

    #[test]
    fn a_still_picture_costs_almost_nothing_after_the_first_frames() {
        let sizes = round_trip(320, 192, 20, 8, false);
        // A settled frame is its header (the unchanged probabilities) and
        // 240 skip flags.
        assert!(sizes[7] < 200, "sizes {sizes:?}");
        assert!(sizes[0] > 20 * sizes[7], "sizes {sizes:?}");
        // A small moving box costs far less than a key frame.
        let moving = round_trip(320, 192, 20, 8, true);
        assert!(moving[7] * 4 < moving[0], "sizes {moving:?}");
    }

    /// A dark interface: panels a few levels apart must not merge, and
    /// what one window leaves behind must not show through the next.
    #[test]
    fn faint_edges_survive_and_leave_no_ghosts() {
        let (w, h) = (160, 96);
        let dark = |t: usize| -> Vec<u32> {
            let mut px = vec![0x0012_161f_u32; w * h];
            for y in 0..h {
                for x in 0..w {
                    // A slightly lighter panel, in another place each frame.
                    if x >= 20 + t * 40 && x < 70 + t * 40 && y >= 10 && y < 70 {
                        px[y * w + x] = 0x0017_1c27;
                    }
                }
            }
            px
        };
        // The screen recorder's quantizer.
        let mut enc = Encoder::new(w, h, 4).unwrap();
        let mut dec = Decoder::new();
        for t in [0, 1, 1, 1, 2, 2] {
            let src = dark(t);
            dec.decode(&enc.encode(&src, w, false).data).unwrap();
            let f = dec.frame().unwrap();
            let mut worst = 0;
            for y in 0..h {
                for x in 0..w {
                    let p = src[y * w + x];
                    let (r, g, b) = ((p >> 16 & 255) as i32, (p >> 8 & 255) as i32, (p & 255) as i32);
                    let luma = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
                    worst = worst.max((f.y[y * f.y_stride + x] as i32 - luma).abs());
                }
            }
            assert!(worst <= 2, "frame with the panel at {t}: luma is off by {worst}");
        }
    }

    #[test]
    fn a_requested_key_frame_stands_alone() {
        let (w, h) = (96, 64);
        let mut enc = Encoder::new(w, h, 20).unwrap();
        enc.encode(&picture(w, h, 0), w, false);
        enc.encode(&picture(w, h, 1), w, false);
        let packet = enc.encode(&picture(w, h, 2), w, true);
        assert!(packet.key);
        let mut dec = Decoder::new();
        dec.decode(&packet.data).unwrap();
        let f = dec.frame().unwrap();
        let (ry, rys) = enc.reconstruction()[0];
        for y in 0..h {
            assert_eq!(&f.y[y * f.y_stride..y * f.y_stride + w], &ry[y * rys..y * rys + w]);
        }
        assert!(Encoder::new(0, 10, 20).is_none());
        assert!(Encoder::new(20000, 10, 20).is_none());
    }

    #[test]
    fn transforms_invert() {
        let mut seed = 12345u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as i32
        };
        for _ in 0..200 {
            let mut d = [0i16; 16];
            for v in d.iter_mut() {
                *v = (next() % 511 - 255) as i16;
            }
            let c = fdct(&d);
            let pred = [128u8; 16];
            let mut out = [0u8; 16];
            idct_add(&mut out, 0, 4, &pred, 0, 4, &c);
            for k in 0..16 {
                let want = (128 + d[k] as i32).clamp(0, 255);
                assert!((out[k] as i32 - want).abs() <= 1, "dct: {} for {}", out[k], want);
            }
            let w = fwht(&d);
            let mut back = [0i16; 16];
            walsh(&w, &mut back);
            for k in 0..16 {
                assert!((back[k] - d[k]).abs() <= 1, "walsh: {} for {}", back[k], d[k]);
            }
        }
    }
}
