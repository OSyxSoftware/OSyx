//! VP8 video (RFC 6386), key frames and inter frames: a port of the RFC's
//! reference decoder ("dixie"). WebP's lossy pictures are single VP8 key
//! frames; WebM video carries whole streams.

// Index loops mirror the reference decoder.
#![allow(clippy::needless_range_loop)]

pub mod encode;
mod tables;

use alloc::vec;
use alloc::vec::Vec;
use tables::*;

type R<T> = Result<T, &'static str>;

/// Pixels of border around each plane: room for the out-of-frame pixels
/// intra prediction writes and for motion vectors that point just outside.
const BORDER: usize = 32;

/// Largest frame, in macroblocks.
const MAX_MBS: usize = 1 << 16;

const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const H_PRED: u8 = 2;
const TM_PRED: u8 = 3;
const B_PRED: u8 = 4;
const NEARESTMV: u8 = 5;
const NEARMV: u8 = 6;
const ZEROMV: u8 = 7;
const NEWMV: u8 = 8;
const SPLITMV: u8 = 9;

const B_DC_PRED: u8 = 0;
const B_TM_PRED: u8 = 1;
const B_VE_PRED: u8 = 2;
const B_HE_PRED: u8 = 3;
const LEFT4X4: u8 = 10;
const ABOVE4X4: u8 = 11;
const ZERO4X4: u8 = 12;

const CURRENT: usize = 0;
const LAST: usize = 1;
const GOLDEN: usize = 2;
const ALTREF: usize = 3;

// ---------------------------------------------------------------------------
// Boolean entropy decoder (RFC 6386, 7).

struct Bool<'a> {
    data: &'a [u8],
    pos: usize,
    value: u32,
    range: u32,
    bit_count: u32,
}

impl<'a> Bool<'a> {
    fn new(data: &'a [u8]) -> Bool<'a> {
        let mut b = Bool { data, pos: 0, value: 0, range: 255, bit_count: 0 };
        if data.len() >= 2 {
            b.value = (data[0] as u32) << 8 | data[1] as u32;
            b.pos = 2;
        } else {
            b.pos = data.len();
        }
        b
    }

    #[inline]
    fn get(&mut self, prob: u8) -> bool {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let big = split << 8;
        let bit = if self.value >= big {
            self.range -= split;
            self.value -= big;
            true
        } else {
            self.range = split;
            false
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                if let Some(&b) = self.data.get(self.pos) {
                    self.value |= b as u32;
                    self.pos += 1;
                }
            }
        }
        bit
    }

    fn bit(&mut self) -> bool {
        self.get(128)
    }

    fn uint(&mut self, bits: u32) -> u32 {
        (0..bits).fold(0, |v, _| (v << 1) | self.bit() as u32)
    }

    fn int(&mut self, bits: u32) -> i32 {
        let v = self.uint(bits) as i32;
        if self.bit() {
            -v
        } else {
            v
        }
    }

    fn maybe_int(&mut self, bits: u32) -> i32 {
        if self.bit() {
            self.int(bits)
        } else {
            0
        }
    }

    fn tree(&mut self, t: &[i16], p: &[u8]) -> u8 {
        let mut i = 0i16;
        loop {
            i = t[i as usize + self.get(p[i as usize >> 1]) as usize];
            if i <= 0 {
                return (-i) as u8;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Frame state.

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Mv {
    x: i16,
    y: i16,
}

impl Mv {
    fn is_zero(self) -> bool {
        self.x == 0 && self.y == 0
    }
}

#[derive(Clone, Copy, Default)]
struct MbInfo {
    y_mode: u8,
    uv_mode: u8,
    segment: u8,
    ref_frame: u8,
    skip: bool,
    need_mc_border: bool,
    mv: Mv,
    eob_mask: u32,
    modes: [u8; 16],
    mvs: [Mv; 16],
}

#[derive(Clone, Default)]
struct Segmentation {
    enabled: bool,
    update_map: bool,
    abs: bool,
    tree_probs: [u8; 3],
    lf_level: [i32; 4],
    quant_idx: [i32; 4],
}

#[derive(Clone, Default)]
struct LoopFilter {
    simple: bool,
    level: i32,
    sharpness: i32,
    delta_enabled: bool,
    ref_delta: [i32; 4],
    mode_delta: [i32; 4],
}

#[derive(Clone, Default)]
struct Quant {
    q_index: i32,
    y1_dc: i32,
    y2_dc: i32,
    y2_ac: i32,
    uv_dc: i32,
    uv_ac: i32,
}

#[derive(Clone)]
struct Entropy {
    coeff: [[[[u8; 11]; 3]; 8]; 4],
    mv: [[u8; 19]; 2],
    skip_enabled: bool,
    skip_prob: u8,
    y_mode: [u8; 4],
    uv_mode: [u8; 3],
    prob_inter: u8,
    prob_last: u8,
    prob_gf: u8,
}

impl Default for Entropy {
    fn default() -> Entropy {
        Entropy {
            coeff: K_DEFAULT_COEFF_PROBS,
            mv: K_DEFAULT_MV_PROBS,
            skip_enabled: false,
            skip_prob: 0,
            y_mode: K_DEFAULT_Y_MODE_PROBS,
            uv_mode: K_DEFAULT_UV_MODE_PROBS,
            prob_inter: 0,
            prob_last: 0,
            prob_gf: 0,
        }
    }
}

/// One frame buffer: Y, U and V with borders.
#[derive(Clone, Default)]
struct Img {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// A decoded picture in 4:2:0 YUV.
pub struct Yuv<'a> {
    pub w: usize,
    pub h: usize,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

#[derive(Default)]
pub struct Decoder {
    w: usize,
    h: usize,
    mb_cols: usize,
    mb_rows: usize,
    version: u32,
    shown: bool,
    keyframe: bool,
    seg: Segmentation,
    lf: LoopFilter,
    quant: Quant,
    entropy: Entropy,
    sign_bias: [bool; 4],
    mbi: Vec<MbInfo>,
    frames: [Img; 4],
    ref_cnt: [u32; 4],
    /// Buffer index of each reference (current, last, golden, altref).
    refs: [Option<usize>; 4],
    have_frame: bool,
}

/// Factors for (Y1, UV, Y2) x (DC, AC).
type Dequant = [[i32; 2]; 3];

fn dc_q(q: i32) -> i32 {
    DC_Q_LOOKUP[q.clamp(0, 127) as usize]
}

fn ac_q(q: i32) -> i32 {
    AC_Q_LOOKUP[q.clamp(0, 127) as usize]
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    fn ys(&self) -> usize {
        self.mb_cols * 16 + 2 * BORDER
    }

    fn uvs(&self) -> usize {
        self.mb_cols * 8 + BORDER
    }

    fn y_origin(&self) -> usize {
        BORDER * self.ys() + BORDER
    }

    fn uv_origin(&self) -> usize {
        BORDER / 2 * self.uvs() + BORDER / 2
    }

    /// Whether the last frame decoded is meant to be shown.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// The last decoded frame.
    pub fn frame(&self) -> Option<Yuv<'_>> {
        let i = self.refs[CURRENT].or(self.refs[LAST])?;
        let f = &self.frames[i];
        Some(Yuv {
            w: self.w,
            h: self.h,
            y: &f.y[self.y_origin()..],
            u: &f.u[self.uv_origin()..],
            v: &f.v[self.uv_origin()..],
            y_stride: self.ys(),
            uv_stride: self.uvs(),
        })
    }

    /// Decodes one compressed frame.
    pub fn decode(&mut self, data: &[u8]) -> R<()> {
        if data.len() < 3 {
            return Err("short VP8 frame");
        }
        let raw = data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16;
        self.keyframe = raw & 1 == 0;
        let version = (raw >> 1) & 7;
        self.shown = (raw >> 4) & 1 != 0;
        let part0 = (raw >> 5) as usize;
        let mut p = 3;
        if self.keyframe {
            if data.len() < 10 || data[3..6] != [0x9D, 0x01, 0x2A] {
                return Err("bad VP8 key frame");
            }
            let w = (u16::from_le_bytes([data[6], data[7]]) & 0x3FFF) as usize;
            let h = (u16::from_le_bytes([data[8], data[9]]) & 0x3FFF) as usize;
            if w == 0 || h == 0 {
                return Err("bad VP8 frame size");
            }
            if (w, h) != (self.w, self.h) || version != self.version || !self.have_frame {
                let shown = self.shown;
                self.resize(w, h, version)?;
                self.keyframe = true;
                self.shown = shown;
            }
            p = 10;
        } else if !self.have_frame {
            return Err("VP8 inter frame without a key frame");
        }
        if version > 3 {
            return Err("unsupported VP8 version");
        }
        let first = data.get(p..p + part0).ok_or("VP8 first partition past the end")?;
        let mut b = Bool::new(first);
        if self.keyframe && b.uint(2) != 0 {
            // Colour space and clamping type: both must be 0 in practice;
            // the clamping bit only allows skipping clamping.
        }
        self.segmentation_header(&mut b);
        self.loopfilter_header(&mut b);
        // Token partitions.
        let nparts = 1usize << b.uint(2);
        let rest = &data[p + part0..];
        let sizes_len = 3 * (nparts - 1);
        if rest.len() < sizes_len {
            return Err("VP8 partition sizes past the end");
        }
        let mut parts: Vec<Bool> = Vec::with_capacity(nparts);
        let mut off = sizes_len;
        for i in 0..nparts {
            let len = if i + 1 < nparts {
                let s = &rest[3 * i..];
                s[0] as usize | (s[1] as usize) << 8 | (s[2] as usize) << 16
            } else {
                rest.len().saturating_sub(off)
            };
            let end = (off + len).min(rest.len());
            parts.push(Bool::new(&rest[off.min(end)..end]));
            off = end;
        }
        // Quantizer.
        self.quant.q_index = b.uint(7) as i32;
        self.quant.y1_dc = b.maybe_int(4);
        self.quant.y2_dc = b.maybe_int(4);
        self.quant.y2_ac = b.maybe_int(4);
        self.quant.uv_dc = b.maybe_int(4);
        self.quant.uv_ac = b.maybe_int(4);
        // References.
        let key = self.keyframe;
        let refresh_gf = key || b.bit();
        let refresh_arf = key || b.bit();
        let copy_gf = if !key && !refresh_gf { b.uint(2) } else { 0 };
        let copy_arf = if !key && !refresh_arf { b.uint(2) } else { 0 };
        self.sign_bias[GOLDEN] = !key && b.bit();
        self.sign_bias[ALTREF] = !key && b.bit();
        let refresh_entropy = b.bit();
        let refresh_last = key || b.bit();
        if key {
            self.entropy = Entropy::default();
        }
        let saved = (!refresh_entropy).then(|| self.entropy.clone());
        self.entropy_header(&mut b);
        // Dequantization factors per segment.
        let mut dq: [Dequant; 4] = [[[0; 2]; 3]; 4];
        for (i, f) in dq.iter_mut().enumerate() {
            let mut q = self.quant.q_index;
            if self.seg.enabled {
                q = if self.seg.abs { self.seg.quant_idx[i] } else { q + self.seg.quant_idx[i] };
            }
            let qh = &self.quant;
            f[0] = [dc_q(q + qh.y1_dc), ac_q(q)];
            f[1] = [dc_q(q + qh.uv_dc).min(132), ac_q(q + qh.uv_ac)];
            f[2] = [dc_q(q + qh.y2_dc) * 2, (ac_q(q + qh.y2_ac) * 155 / 100).max(8)];
        }
        // A free buffer to decode into.
        if let Some(c) = self.refs[CURRENT].take() {
            self.ref_cnt[c] -= 1;
        }
        let cur = (0..4).find(|&i| self.ref_cnt[i] == 0).ok_or("no free VP8 frame buffer")?;
        self.ref_cnt[cur] = 1;
        self.refs[CURRENT] = Some(cur);
        let mut img = core::mem::take(&mut self.frames[cur]);
        let mut above_ctx = vec![[0u8; 9]; self.mb_cols];
        let mut coeffs = [0i16; 400];
        for row in 0..self.mb_rows {
            let mut left_ctx = [0u8; 9];
            let part = &mut parts[row & (nparts - 1)];
            let mut bounds = MvBounds {
                to_left: -(1 << 7),
                to_right: (self.mb_cols as i32) << 7,
                to_top: -(((row + 1) as i32) << 7),
                to_bottom: ((self.mb_rows - row) as i32) << 7,
            };
            for col in 0..self.mb_cols {
                self.mode_mv(&mut b, row, col, &bounds);
                if !self.keyframe {
                    bounds.to_left -= 16 << 3;
                    bounds.to_right -= 16 << 3;
                }
                let idx = self.mbi_index(row as isize, col as isize);
                let mut mbi = self.mbi[idx];
                coeffs.fill(0);
                if mbi.skip {
                    // The second order context survives modes without Y2.
                    let keep = mbi.y_mode == B_PRED || mbi.y_mode == SPLITMV;
                    let (l8, a8) = (left_ctx[8], above_ctx[col][8]);
                    left_ctx = [0; 9];
                    above_ctx[col] = [0; 9];
                    if keep {
                        left_ctx[8] = l8;
                        above_ctx[col][8] = a8;
                    }
                    mbi.eob_mask = 0;
                } else {
                    let f = &dq[mbi.segment as usize & 3];
                    mbi.eob_mask = tokens(
                        part,
                        &mut left_ctx,
                        &mut above_ctx[col],
                        &mut coeffs,
                        mbi.y_mode,
                        &self.entropy.coeff,
                        f,
                    );
                }
                self.mbi[idx].eob_mask = mbi.eob_mask;
                self.predict(&mut img, &mbi, row, col, &mut coeffs);
            }
            // Extend the last row of each macroblock row by four pixels for
            // the above-right of the next row's last subblocks.
            let ys = self.ys();
            let at = self.y_origin() + (row * 16 + 15) * ys + self.mb_cols * 16;
            let v = img.y[at - 1];
            img.y[at..at + 4].fill(v);
        }
        if self.lf.level != 0 {
            self.loop_filter(&mut img);
        }
        self.frames[cur] = img;
        if let Some(e) = saved {
            self.entropy = e;
        }
        // Reference updates.
        let set = |s: &mut Decoder, slot: usize, from: usize| {
            if let Some(i) = s.refs[slot].take() {
                s.ref_cnt[i] -= 1;
            }
            if let Some(i) = s.refs[from] {
                s.ref_cnt[i] += 1;
                s.refs[slot] = Some(i);
            }
        };
        match copy_arf {
            1 => set(self, ALTREF, LAST),
            2 => set(self, ALTREF, GOLDEN),
            _ => {}
        }
        match copy_gf {
            1 => set(self, GOLDEN, LAST),
            2 => set(self, GOLDEN, ALTREF),
            _ => {}
        }
        if refresh_gf {
            set(self, GOLDEN, CURRENT);
        }
        if refresh_arf {
            set(self, ALTREF, CURRENT);
        }
        if refresh_last {
            set(self, LAST, CURRENT);
        }
        self.have_frame = true;
        Ok(())
    }

    fn resize(&mut self, w: usize, h: usize, version: u32) -> R<()> {
        let (mb_cols, mb_rows) = (w.div_ceil(16), h.div_ceil(16));
        if mb_cols * mb_rows > MAX_MBS {
            return Err("VP8 frame too large");
        }
        *self = Decoder { w, h, mb_cols, mb_rows, version, ..Decoder::default() };
        let (ys, uvs) = (self.ys(), self.uvs());
        let yh = mb_rows * 16 + 2 * BORDER;
        let uvh = mb_rows * 8 + BORDER;
        for f in self.frames.iter_mut() {
            *f = Img { y: vec![0; ys * yh], u: vec![0; uvs * uvh], v: vec![0; uvs * uvh] };
        }
        self.mbi = vec![MbInfo::default(); (mb_cols + 1) * (mb_rows + 1)];
        Ok(())
    }

    fn mbi_index(&self, row: isize, col: isize) -> usize {
        ((row + 1) * (self.mb_cols as isize + 1) + col + 1) as usize
    }

    fn segmentation_header(&mut self, b: &mut Bool) {
        if self.keyframe {
            self.seg = Segmentation::default();
        }
        self.seg.enabled = b.bit();
        if self.seg.enabled {
            self.seg.update_map = b.bit();
            let update_data = b.bit();
            if update_data {
                self.seg.abs = b.bit();
                for i in 0..4 {
                    self.seg.quant_idx[i] = b.maybe_int(7);
                }
                for i in 0..4 {
                    self.seg.lf_level[i] = b.maybe_int(6);
                }
            }
            if self.seg.update_map {
                for i in 0..3 {
                    self.seg.tree_probs[i] = if b.bit() { b.uint(8) as u8 } else { 255 };
                }
            }
        } else {
            self.seg.update_map = false;
        }
    }

    fn loopfilter_header(&mut self, b: &mut Bool) {
        if self.keyframe {
            self.lf = LoopFilter::default();
        }
        self.lf.simple = b.bit();
        self.lf.level = b.uint(6) as i32;
        self.lf.sharpness = b.uint(3) as i32;
        self.lf.delta_enabled = b.bit();
        if self.lf.delta_enabled && b.bit() {
            for i in 0..4 {
                self.lf.ref_delta[i] = b.maybe_int(6);
            }
            for i in 0..4 {
                self.lf.mode_delta[i] = b.maybe_int(6);
            }
        }
    }

    fn entropy_header(&mut self, b: &mut Bool) {
        let e = &mut self.entropy;
        for i in 0..4 {
            for j in 0..8 {
                for k in 0..3 {
                    for l in 0..11 {
                        if b.get(K_COEFF_ENTROPY_UPDATE_PROBS[i][j][k][l]) {
                            e.coeff[i][j][k][l] = b.uint(8) as u8;
                        }
                    }
                }
            }
        }
        e.skip_enabled = b.bit();
        if e.skip_enabled {
            e.skip_prob = b.uint(8) as u8;
        }
        if !self.keyframe {
            e.prob_inter = b.uint(8) as u8;
            e.prob_last = b.uint(8) as u8;
            e.prob_gf = b.uint(8) as u8;
            if b.bit() {
                for i in 0..4 {
                    e.y_mode[i] = b.uint(8) as u8;
                }
            }
            if b.bit() {
                for i in 0..3 {
                    e.uv_mode[i] = b.uint(8) as u8;
                }
            }
            for i in 0..2 {
                for j in 0..19 {
                    if b.get(K_MV_ENTROPY_UPDATE_PROBS[i][j]) {
                        let x = b.uint(7) as u8;
                        e.mv[i][j] = if x != 0 { x << 1 } else { 1 };
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Modes and motion vectors (RFC 6386, 11, 16, 17).

struct MvBounds {
    to_left: i32,
    to_right: i32,
    to_top: i32,
    to_bottom: i32,
}

fn clamp_mv(m: Mv, b: &MvBounds) -> Mv {
    Mv {
        x: (m.x as i32).clamp(b.to_left, b.to_right.max(b.to_left)) as i16,
        y: (m.y as i32).clamp(b.to_top, b.to_bottom.max(b.to_top)) as i16,
    }
}

/// The 4x4 mode of a 16x16 intra mode, for the contexts of B_PRED.
fn implied_b_mode(y_mode: u8) -> u8 {
    match y_mode {
        V_PRED => B_VE_PRED,
        H_PRED => B_HE_PRED,
        TM_PRED => B_TM_PRED,
        _ => B_DC_PRED,
    }
}

fn read_mv_component(b: &mut Bool, p: &[u8; 19]) -> i16 {
    const IS_SHORT: usize = 0;
    const SIGN: usize = 1;
    const SHORT: usize = 2;
    const BITS: usize = 9;
    let mut x: i32;
    if b.get(p[IS_SHORT]) {
        x = 0;
        for i in 0..3 {
            x += (b.get(p[BITS + i]) as i32) << i;
        }
        for i in (4..10).rev() {
            x += (b.get(p[BITS + i]) as i32) << i;
        }
        if x & 0xFFF0 == 0 || b.get(p[BITS + 3]) {
            x += 8;
        }
    } else {
        x = b.tree(&SMALL_MV_TREE, &p[SHORT..]) as i32;
    }
    if x != 0 && b.get(p[SIGN]) {
        x = -x;
    }
    (x << 1) as i16
}

fn read_mv(b: &mut Bool, p: &[[u8; 19]; 2]) -> Mv {
    let y = read_mv_component(b, &p[0]);
    let x = read_mv_component(b, &p[1]);
    Mv { x, y }
}

fn need_mc_border(mv: Mv, l: i32, t: i32, bw: i32, w: i32, h: i32) -> bool {
    let l = l + (mv.x as i32 >> 3);
    let t = t + (mv.y as i32 >> 3);
    let r = w - (l + bw);
    let b = h - (t + bw);
    l >> 1 < 2 || r >> 1 < 3 || t >> 1 < 2 || b >> 1 < 3
}

impl Decoder {
    fn mode_mv(&mut self, b: &mut Bool, row: usize, col: usize, bounds: &MvBounds) {
        let (r, c) = (row as isize, col as isize);
        let idx = self.mbi_index(r, c);
        let left = self.mbi[self.mbi_index(r, c - 1)];
        let above = self.mbi[self.mbi_index(r - 1, c)];
        let above_left = self.mbi[self.mbi_index(r - 1, c - 1)];
        let mut m = self.mbi[idx];
        if self.seg.update_map {
            let p = self.seg.tree_probs;
            m.segment = if b.get(p[0]) { 2 + b.get(p[2]) as u8 } else { b.get(p[1]) as u8 };
        }
        m.skip = self.entropy.skip_enabled && b.get(self.entropy.skip_prob);
        if self.keyframe {
            if !self.seg.update_map {
                m.segment = 0;
            }
            m.y_mode = b.tree(&KF_Y_MODE_TREE, &KF_Y_MODE_PROBS);
            if m.y_mode == B_PRED {
                for i in 0..16 {
                    let a = if i < 4 {
                        if above.y_mode == B_PRED {
                            above.modes[i + 12]
                        } else {
                            implied_b_mode(above.y_mode)
                        }
                    } else {
                        m.modes[i - 4]
                    };
                    let l = if i & 3 == 0 {
                        if left.y_mode == B_PRED {
                            left.modes[i + 3]
                        } else {
                            implied_b_mode(left.y_mode)
                        }
                    } else {
                        m.modes[i - 1]
                    };
                    m.modes[i] = b.tree(&B_MODE_TREE, &KF_B_MODE_PROBS[a as usize][l as usize]);
                }
            }
            m.uv_mode = b.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS);
            m.mv = Mv::default();
            m.ref_frame = CURRENT as u8;
        } else if b.get(self.entropy.prob_inter) {
            self.decode_mvs(b, &mut m, &left, &above, &above_left, bounds);
        } else {
            m.y_mode = b.tree(&Y_MODE_TREE, &self.entropy.y_mode);
            if m.y_mode == B_PRED {
                for i in 0..16 {
                    m.modes[i] = b.tree(&B_MODE_TREE, &DEFAULT_B_MODE_PROBS);
                }
            }
            m.uv_mode = b.tree(&UV_MODE_TREE, &self.entropy.uv_mode);
            m.mv = Mv::default();
            m.ref_frame = CURRENT as u8;
        }
        self.mbi[idx] = m;
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_mvs(
        &self,
        b: &mut Bool,
        m: &mut MbInfo,
        left: &MbInfo,
        above: &MbInfo,
        above_left: &MbInfo,
        bounds: &MvBounds,
    ) {
        m.ref_frame = if b.get(self.entropy.prob_last) { 2 + b.get(self.entropy.prob_gf) as u8 } else { 1 };
        // find_near_mvs
        let bias = |n: &MbInfo, mv: Mv| -> Mv {
            if self.sign_bias[n.ref_frame as usize] ^ self.sign_bias[m.ref_frame as usize] {
                Mv { x: -mv.x, y: -mv.y }
            } else {
                mv
            }
        };
        let mut near = [Mv::default(); 4];
        let mut cnt = [0i32; 4];
        let mut k = 0usize;
        if above.ref_frame != CURRENT as u8 {
            if !above.mv.is_zero() {
                k += 1;
                near[k] = bias(above, above.mv);
            }
            cnt[k] += 2;
        }
        if left.ref_frame != CURRENT as u8 {
            if !left.mv.is_zero() {
                let this = bias(left, left.mv);
                if this != near[k] {
                    k += 1;
                    near[k] = this;
                }
                cnt[k] += 2;
            } else {
                cnt[0] += 2;
            }
        }
        if above_left.ref_frame != CURRENT as u8 {
            if !above_left.mv.is_zero() {
                let this = bias(above_left, above_left.mv);
                if this != near[k] {
                    k += 1;
                    near[k] = this;
                }
                cnt[k] += 1;
            } else {
                cnt[0] += 1;
            }
        }
        if cnt[3] != 0 && near[k] == near[1] {
            cnt[1] += 1;
        }
        cnt[3] = ((above.y_mode == SPLITMV) as i32 + (left.y_mode == SPLITMV) as i32) * 2
            + (above_left.y_mode == SPLITMV) as i32;
        if cnt[2] > cnt[1] {
            cnt.swap(1, 2);
            near.swap(1, 2);
        }
        if cnt[1] >= cnt[0] {
            near[0] = near[1];
        }
        let probs = [
            MV_COUNTS_TO_PROBS[cnt[0] as usize][0],
            MV_COUNTS_TO_PROBS[cnt[1] as usize][1],
            MV_COUNTS_TO_PROBS[cnt[2] as usize][2],
            MV_COUNTS_TO_PROBS[cnt[3] as usize][3],
        ];
        m.y_mode = b.tree(&MV_REF_TREE, &probs);
        m.uv_mode = m.y_mode;
        m.need_mc_border = false;
        let x = (-bounds.to_left - 128) >> 3;
        let y = (-bounds.to_top - 128) >> 3;
        let (w, h) = ((self.mb_cols * 16) as i32, (self.mb_rows * 16) as i32);
        match m.y_mode {
            NEARESTMV => m.mv = clamp_mv(near[1], bounds),
            NEARMV => m.mv = clamp_mv(near[2], bounds),
            ZEROMV => {
                m.mv = Mv::default();
                return;
            }
            NEWMV => {
                let best = clamp_mv(near[0], bounds);
                let d = read_mv(b, &self.entropy.mv);
                m.mv = Mv { x: d.x.wrapping_add(best.x), y: d.y.wrapping_add(best.y) };
            }
            _ => {
                // SPLITMV
                let best = clamp_mv(near[0], bounds);
                let pid = b.tree(&SPLIT_MV_TREE, &SPLIT_MV_PROBS) as usize;
                let part = &MV_PARTITIONS[pid];
                let mut mask = 0u32;
                let mut j = 0u8;
                while mask < 65535 {
                    let Some(k) = (0..16).find(|&k| part[k] == j) else { break };
                    let lmv = if k & 3 == 0 {
                        if left.y_mode == SPLITMV {
                            left.mvs[k + 3]
                        } else {
                            left.mv
                        }
                    } else {
                        m.mvs[k - 1]
                    };
                    let amv = if k < 4 {
                        if above.y_mode == SPLITMV {
                            above.mvs[k + 12]
                        } else {
                            above.mv
                        }
                    } else {
                        m.mvs[k - 4]
                    };
                    let lez = lmv.is_zero();
                    let aez = amv.is_zero();
                    let lea = lmv == amv;
                    let ctx = if lea && lez {
                        4
                    } else if lea {
                        3
                    } else if aez {
                        2
                    } else if lez {
                        1
                    } else {
                        0
                    };
                    let mv = match b.tree(&SUBMV_REF_TREE, &SUBMV_REF_PROBS2[ctx]) {
                        LEFT4X4 => lmv,
                        ABOVE4X4 => amv,
                        ZERO4X4 => Mv::default(),
                        _ => {
                            let d = read_mv(b, &self.entropy.mv);
                            Mv { x: d.x.wrapping_add(best.x), y: d.y.wrapping_add(best.y) }
                        }
                    };
                    for kk in k..16 {
                        if part[kk] == j {
                            m.mvs[kk] = mv;
                            mask |= 1 << kk;
                        }
                    }
                    j += 1;
                }
                m.mv = m.mvs[15];
                let mut chroma = [(0i32, 0i32); 4];
                for bi in 0..16 {
                    let c = &mut chroma[(bi >> 1 & 1) + (bi >> 2 & 2)];
                    c.0 += m.mvs[bi].x as i32;
                    c.1 += m.mvs[bi].y as i32;
                    if need_mc_border(m.mvs[bi], x + (bi as i32 & 3) * 4, y + (bi as i32 & !3), 4, w, h) {
                        m.need_mc_border = true;
                        break;
                    }
                }
                for (bi, c) in chroma.iter().enumerate() {
                    let cx = (c.0 + 4 + 8 * (c.0 >> 31)) / 4;
                    let cy = (c.1 + 4 + 8 * (c.1 >> 31)) / 4;
                    let mv = Mv { x: cx as i16, y: cy as i16 };
                    if need_mc_border(mv, x + (bi as i32 & 1) * 8, y + (bi as i32 >> 1) * 8, 16, w, h) {
                        m.need_mc_border = true;
                        break;
                    }
                }
                return;
            }
        }
        if need_mc_border(m.mv, x, y, 16, w, h) {
            m.need_mc_border = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Coefficient tokens (RFC 6386, 13).

const BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const LEFT_CTX: [usize; 25] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8];
const ABOVE_CTX: [usize; 25] = [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 8];
const CAT_PROBS: [&[u8]; 6] = [
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];
const CAT_BASE: [i32; 6] = [5, 7, 11, 19, 35, 67];

/// One block's tokens into `out` (dequantized, natural order); returns the
/// index after the last decoded coefficient (dixie's `c`).
fn block_tokens(
    b: &mut Bool,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    first: usize,
    out: &mut [i16],
    dq: [i32; 2],
) -> usize {
    let mut c = first;
    let mut p = &probs[BANDS[c]][ctx];
    // After a zero token there is no end-of-block check.
    let mut check_eob = true;
    loop {
        if check_eob && !b.get(p[0]) {
            return c;
        }
        if !b.get(p[1]) {
            // A zero.
            if c >= 15 {
                return c;
            }
            c += 1;
            p = &probs[BANDS[c]][0];
            check_eob = false;
            continue;
        }
        let (v, next_ctx): (i32, usize) = if !b.get(p[2]) {
            (1, 1)
        } else {
            let v = if !b.get(p[3]) {
                if !b.get(p[4]) {
                    2
                } else if !b.get(p[5]) {
                    3
                } else {
                    4
                }
            } else if !b.get(p[6]) {
                if !b.get(p[7]) {
                    5 + b.get(159) as i32
                } else {
                    7 + 2 * b.get(165) as i32 + b.get(145) as i32
                }
            } else {
                let cat = if !b.get(p[8]) { 2 + b.get(p[9]) as usize } else { 4 + b.get(p[10]) as usize };
                let mut v = 0;
                for &pr in CAT_PROBS[cat] {
                    v = (v << 1) | b.get(pr) as i32;
                }
                CAT_BASE[cat] + v
            };
            (v, 2)
        };
        let v = if b.bit() { -v } else { v };
        out[ZIGZAG[c]] = (v * dq[(c > 0) as usize]) as i16;
        if c >= 15 {
            return 16;
        }
        c += 1;
        p = &probs[BANDS[c]][next_ctx];
        check_eob = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn tokens(
    b: &mut Bool,
    left: &mut [u8; 9],
    above: &mut [u8; 9],
    coeffs: &mut [i16; 400],
    mode: u8,
    probs: &[[[[u8; 11]; 3]; 8]; 4],
    f: &Dequant,
) -> u32 {
    let mut eob_mask = 0u32;
    let has_y2 = mode != B_PRED && mode != SPLITMV;
    let mut run = |i: usize, kind: usize, first: usize, dq: [i32; 2], out: &mut [i16]| {
        let ctx = (left[LEFT_CTX[i]] + above[ABOVE_CTX[i]]) as usize;
        let c = block_tokens(b, &probs[kind], ctx, first, out, dq);
        let nonzero = c != first;
        eob_mask |= ((c > 1) as u32) << i;
        eob_mask |= (nonzero as u32) << 31;
        left[LEFT_CTX[i]] = nonzero as u8;
        above[ABOVE_CTX[i]] = nonzero as u8;
    };
    let (yk, yfirst) = if has_y2 {
        run(24, 1, 0, f[2], &mut coeffs[384..400]);
        (0, 1)
    } else {
        (3, 0)
    };
    for i in 0..16 {
        run(i, yk, yfirst, f[0], &mut coeffs[i * 16..i * 16 + 16]);
    }
    for i in 16..24 {
        run(i, 2, 0, f[1], &mut coeffs[i * 16..i * 16 + 16]);
    }
    eob_mask
}

// ---------------------------------------------------------------------------
// Transforms (RFC 6386, 14).

fn walsh(input: &[i16], output: &mut [i16; 16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| input[i + k] as i32;
        let a1 = ip(0) + ip(12);
        let b1 = ip(4) + ip(8);
        let c1 = ip(4) - ip(8);
        let d1 = ip(0) - ip(12);
        t[i] = a1 + b1;
        t[4 + i] = c1 + d1;
        t[8 + i] = a1 - b1;
        t[12 + i] = d1 - c1;
    }
    for i in 0..4 {
        let ip = &t[4 * i..4 * i + 4];
        let a1 = ip[0] + ip[3];
        let b1 = ip[1] + ip[2];
        let c1 = ip[1] - ip[2];
        let d1 = ip[0] - ip[3];
        output[4 * i] = ((a1 + b1 + 3) >> 3) as i16;
        output[4 * i + 1] = ((c1 + d1 + 3) >> 3) as i16;
        output[4 * i + 2] = ((a1 - b1 + 3) >> 3) as i16;
        output[4 * i + 3] = ((d1 - c1 + 3) >> 3) as i16;
    }
}

const C1: i32 = 20091;
const S1: i32 = 35468;

/// Inverse DCT of `coeffs`, added to the prediction at `pred` (stride
/// `ps`) and written to `out` at `o` (stride `stride`).
#[allow(clippy::too_many_arguments)]
fn idct_add(out: &mut [u8], o: usize, stride: usize, pred: &[u8], po: usize, ps: usize, coeffs: &[i16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| coeffs[i + k] as i32;
        let a1 = ip(0) + ip(8);
        let b1 = ip(0) - ip(8);
        let c1 = ((ip(4) * S1) >> 16) - (ip(12) + ((ip(12) * C1) >> 16));
        let d1 = (ip(4) + ((ip(4) * C1) >> 16)) + ((ip(12) * S1) >> 16);
        t[i] = a1 + d1;
        t[12 + i] = a1 - d1;
        t[4 + i] = b1 + c1;
        t[8 + i] = b1 - c1;
    }
    for i in 0..4 {
        let c = &t[4 * i..4 * i + 4];
        let a1 = c[0] + c[2];
        let b1 = c[0] - c[2];
        let c1 = ((c[1] * S1) >> 16) - (c[3] + ((c[3] * C1) >> 16));
        let d1 = (c[1] + ((c[1] * C1) >> 16)) + ((c[3] * S1) >> 16);
        let r = [a1 + d1, b1 + c1, b1 - c1, a1 - d1];
        for k in 0..4 {
            let pv = pred[po + i * ps + k] as i32;
            out[o + i * stride + k] = (pv + ((r[k] + 4) >> 3)).clamp(0, 255) as u8;
        }
    }
}

/// `idct_add` with the prediction already in place.
fn idct_in_place(buf: &mut [u8], o: usize, stride: usize, coeffs: &[i16]) {
    let mut pred = [0u8; 16];
    for r in 0..4 {
        pred[r * 4..r * 4 + 4].copy_from_slice(&buf[o + r * stride..o + r * stride + 4]);
    }
    idct_add(buf, o, stride, &pred, 0, 4, coeffs);
}

// ---------------------------------------------------------------------------
// Prediction (RFC 6386, 12, 18).

fn predict_dc(p: &mut [u8], o: usize, s: usize, n: usize) {
    let mut dc = 0u32;
    for i in 0..n {
        dc += p[o + i * s - 1] as u32 + p[o - s + i] as u32;
    }
    let shift = n.trailing_zeros() + 1;
    let dc = ((dc + (1 << (shift - 1))) >> shift) as u8;
    for i in 0..n {
        p[o + i * s..o + i * s + n].fill(dc);
    }
}

fn predict_v(p: &mut [u8], o: usize, s: usize, n: usize) {
    for i in 0..n {
        p.copy_within(o - s..o - s + n, o + i * s);
    }
}

fn predict_h(p: &mut [u8], o: usize, s: usize, n: usize) {
    for i in 0..n {
        let v = p[o + i * s - 1];
        p[o + i * s..o + i * s + n].fill(v);
    }
}

fn predict_tm(p: &mut [u8], o: usize, s: usize, n: usize) {
    let corner = p[o - s - 1] as i32;
    for i in 0..n {
        let l = p[o + i * s - 1] as i32;
        for j in 0..n {
            p[o + i * s + j] = (l + p[o - s + j] as i32 - corner).clamp(0, 255) as u8;
        }
    }
}

/// The ten 4x4 intra predictors.
fn predict_b(p: &mut [u8], o: usize, s: usize, mode: u8) {
    let a = |i: isize| p[(o as isize - s as isize + i) as usize] as u32;
    let l = |i: usize| p[o + i * s - 1] as u32;
    let mut b = [[0u32; 4]; 4];
    let avg3 = |x: u32, y: u32, z: u32| (x + 2 * y + z + 2) >> 2;
    let avg2 = |x: u32, y: u32| (x + y + 1) >> 1;
    match mode {
        0 => {
            let mut dc = 0;
            for i in 0..4 {
                dc += a(i as isize) + l(i);
            }
            b = [[(dc + 4) >> 3; 4]; 4];
        }
        1 => {
            let c = a(-1) as i32;
            for (r, row) in b.iter_mut().enumerate() {
                for (k, v) in row.iter_mut().enumerate() {
                    *v = (l(r) as i32 + a(k as isize) as i32 - c).clamp(0, 255) as u32;
                }
            }
        }
        2 => {
            let row = [avg3(a(-1), a(0), a(1)), avg3(a(0), a(1), a(2)), avg3(a(1), a(2), a(3)), avg3(a(2), a(3), a(4))];
            b = [row; 4];
        }
        3 => {
            let rows =
                [avg3(a(-1), l(0), l(1)), avg3(l(0), l(1), l(2)), avg3(l(1), l(2), l(3)), avg3(l(2), l(3), l(3))];
            for r in 0..4 {
                b[r] = [rows[r]; 4];
            }
        }
        4 => {
            // LD: down-left from the above row.
            let e: [u32; 8] = core::array::from_fn(|i| a(i as isize));
            for r in 0..4 {
                for c in 0..4 {
                    let i = r + c;
                    b[r][c] = if i == 6 { avg3(e[6], e[7], e[7]) } else { avg3(e[i], e[i + 1], e[i + 2]) };
                }
            }
        }
        5 => {
            // RD: edge E = L3 L2 L1 L0 P A0 A1 A2 A3.
            let e = [l(3), l(2), l(1), l(0), a(-1), a(0), a(1), a(2), a(3)];
            for r in 0..4 {
                for c in 0..4 {
                    let i = 3 - r + c;
                    b[r][c] = avg3(e[i], e[i + 1], e[i + 2]);
                }
            }
        }
        6 => {
            // VR
            let e = [l(3), l(2), l(1), l(0), a(-1), a(0), a(1), a(2), a(3)];
            b[3][0] = avg3(e[1], e[2], e[3]);
            b[2][0] = avg3(e[2], e[3], e[4]);
            b[3][1] = avg3(e[3], e[4], e[5]);
            b[1][0] = b[3][1];
            b[2][1] = avg2(e[4], e[5]);
            b[0][0] = b[2][1];
            b[3][2] = avg3(e[4], e[5], e[6]);
            b[1][1] = b[3][2];
            b[2][2] = avg2(e[5], e[6]);
            b[0][1] = b[2][2];
            b[3][3] = avg3(e[5], e[6], e[7]);
            b[1][2] = b[3][3];
            b[2][3] = avg2(e[6], e[7]);
            b[0][2] = b[2][3];
            b[1][3] = avg3(e[6], e[7], e[8]);
            b[0][3] = avg2(e[7], e[8]);
        }
        7 => {
            // VL
            let e: [u32; 8] = core::array::from_fn(|i| a(i as isize));
            b[0][0] = avg2(e[0], e[1]);
            b[1][0] = avg3(e[0], e[1], e[2]);
            b[2][0] = avg2(e[1], e[2]);
            b[0][1] = b[2][0];
            b[1][1] = avg3(e[1], e[2], e[3]);
            b[3][0] = b[1][1];
            b[2][1] = avg2(e[2], e[3]);
            b[0][2] = b[2][1];
            b[3][1] = avg3(e[2], e[3], e[4]);
            b[1][2] = b[3][1];
            b[2][2] = avg2(e[3], e[4]);
            b[0][3] = b[2][2];
            b[3][2] = avg3(e[3], e[4], e[5]);
            b[1][3] = b[3][2];
            b[2][3] = avg3(e[4], e[5], e[6]);
            b[3][3] = avg3(e[5], e[6], e[7]);
        }
        8 => {
            // HD
            let e = [l(3), l(2), l(1), l(0), a(-1), a(0), a(1), a(2), a(3)];
            b[3][0] = avg2(e[0], e[1]);
            b[3][1] = avg3(e[0], e[1], e[2]);
            b[2][0] = avg2(e[1], e[2]);
            b[3][2] = b[2][0];
            b[2][1] = avg3(e[1], e[2], e[3]);
            b[3][3] = b[2][1];
            b[2][2] = avg2(e[2], e[3]);
            b[1][0] = b[2][2];
            b[2][3] = avg3(e[2], e[3], e[4]);
            b[1][1] = b[2][3];
            b[1][2] = avg2(e[3], e[4]);
            b[0][0] = b[1][2];
            b[1][3] = avg3(e[3], e[4], e[5]);
            b[0][1] = b[1][3];
            b[0][2] = avg3(e[4], e[5], e[6]);
            b[0][3] = avg3(e[5], e[6], e[7]);
        }
        _ => {
            // HU
            let e = [l(0), l(1), l(2), l(3)];
            b[0][0] = avg2(e[0], e[1]);
            b[0][1] = avg3(e[0], e[1], e[2]);
            b[0][2] = avg2(e[1], e[2]);
            b[1][0] = b[0][2];
            b[0][3] = avg3(e[1], e[2], e[3]);
            b[1][1] = b[0][3];
            b[1][2] = avg2(e[2], e[3]);
            b[2][0] = b[1][2];
            b[1][3] = avg3(e[2], e[3], e[3]);
            b[2][1] = b[1][3];
            b[2][2] = e[3];
            b[2][3] = e[3];
            b[3] = [e[3]; 4];
        }
    }
    for r in 0..4 {
        for c in 0..4 {
            p[o + r * s + c] = b[r][c] as u8;
        }
    }
}

const SIXTAP: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

const BILINEAR: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, 0, 112, 16, 0, 0],
    [0, 0, 96, 32, 0, 0],
    [0, 0, 80, 48, 0, 0],
    [0, 0, 64, 64, 0, 0],
    [0, 0, 48, 80, 0, 0],
    [0, 0, 32, 96, 0, 0],
    [0, 0, 16, 112, 0, 0],
];

/// The 4x4 inter prediction at `src[o]` (stride `s`) moved by `mv`: a
/// 6-tap (or bilinear) sub-pixel filter, first horizontally over 9 rows,
/// then vertically.
fn filter_block(src: &[u8], o: isize, s: usize, mv: Mv, filters: &[[i32; 6]; 8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let base = o + (mv.y as isize >> 3) * s as isize + (mv.x as isize >> 3);
    let (mx, my) = ((mv.x & 7) as usize, (mv.y & 7) as usize);
    let at = |i: isize| src[i as usize] as i32;
    if mx == 0 && my == 0 {
        for r in 0..4 {
            for c in 0..4 {
                out[r * 4 + c] = at(base + (r * s + c) as isize) as u8;
            }
        }
        return out;
    }
    let (fh, fv) = (&filters[mx], &filters[my]);
    let mut tmp = [0u8; 9 * 4];
    for r in 0..9 {
        let row = base + (r as isize - 2) * s as isize;
        for c in 0..4 {
            let p = row + c as isize;
            let v = at(p - 2) * fh[0]
                + at(p - 1) * fh[1]
                + at(p) * fh[2]
                + at(p + 1) * fh[3]
                + at(p + 2) * fh[4]
                + at(p + 3) * fh[5]
                + 64;
            tmp[r * 4 + c] = (v >> 7).clamp(0, 255) as u8;
        }
    }
    for r in 0..4 {
        for c in 0..4 {
            let t = |k: usize| tmp[(r + k) * 4 + c] as i32;
            let v = t(0) * fv[0] + t(1) * fv[1] + t(2) * fv[2] + t(3) * fv[3] + t(4) * fv[4] + t(5) * fv[5] + 64;
            out[r * 4 + c] = (v >> 7).clamp(0, 255) as u8;
        }
    }
    out
}

/// Copies a 9 x 9 block around (x, y) from a plane of `w` by `h` visible
/// samples (at `org`, stride `s`), replicating the edges.
fn edge_block(src: &[u8], org: usize, s: usize, x: i32, y: i32, w: i32, h: i32) -> [u8; 81] {
    let mut out = [0u8; 81];
    for r in 0..9 {
        let sy = (y + r).clamp(0, h - 1) as usize;
        for c in 0..9 {
            let sx = (x + c).clamp(0, w - 1) as usize;
            out[(r * 9 + c) as usize] = src[org + sy * s + sx];
        }
    }
    out
}

impl Decoder {
    fn predict(&self, img: &mut Img, m: &MbInfo, row: usize, col: usize, coeffs: &mut [i16; 400]) {
        let (ys, uvs) = (self.ys(), self.uvs());
        let yo = self.y_origin() + row * 16 * ys + col * 16;
        let uo = self.uv_origin() + row * 8 * uvs + col * 8;
        // Out-of-frame pixels.
        if col == 0 {
            fixup_left(&mut img.y, yo, ys, 16, row, m.y_mode);
            fixup_left(&mut img.u, uo, uvs, 8, row, m.uv_mode);
            fixup_left(&mut img.v, uo, uvs, 8, row, m.uv_mode);
            if row == 0 {
                img.y[yo - ys - 1] = 127;
            }
        }
        if row == 0 {
            fixup_above(&mut img.y, yo, ys, 16, col, m.y_mode);
            fixup_above(&mut img.u, uo, uvs, 8, col, m.uv_mode);
            fixup_above(&mut img.v, uo, uvs, 8, col, m.uv_mode);
        }
        if m.y_mode <= B_PRED {
            if m.y_mode == B_PRED {
                // Copy the above-right of subblock 3 down to 7, 11 and 15.
                let src = yo + 16 - ys;
                for k in 1..4 {
                    img.y.copy_within(src..src + 4, src + 4 * k * ys);
                }
                for i in 0..16 {
                    let o = yo + (i / 4) * 4 * ys + (i & 3) * 4;
                    predict_b(&mut img.y, o, ys, m.modes[i]);
                    idct_in_place(&mut img.y, o, ys, &coeffs[i * 16..i * 16 + 16]);
                }
            } else {
                match m.y_mode {
                    DC_PRED => predict_dc(&mut img.y, yo, ys, 16),
                    V_PRED => predict_v(&mut img.y, yo, ys, 16),
                    H_PRED => predict_h(&mut img.y, yo, ys, 16),
                    _ => predict_tm(&mut img.y, yo, ys, 16),
                }
                fixup_dc(coeffs);
                for i in 0..16 {
                    let o = yo + (i / 4) * 4 * ys + (i & 3) * 4;
                    idct_in_place(&mut img.y, o, ys, &coeffs[i * 16..i * 16 + 16]);
                }
            }
            for (plane, base) in [(&mut img.u, 16), (&mut img.v, 20)] {
                match m.uv_mode {
                    DC_PRED => predict_dc(plane, uo, uvs, 8),
                    V_PRED => predict_v(plane, uo, uvs, 8),
                    H_PRED => predict_h(plane, uo, uvs, 8),
                    _ => predict_tm(plane, uo, uvs, 8),
                }
                for i in 0..4 {
                    let o = uo + (i / 2) * 4 * uvs + (i & 1) * 4;
                    idct_in_place(plane, o, uvs, &coeffs[(base + i) * 16..(base + i) * 16 + 16]);
                }
            }
        } else {
            if m.y_mode != SPLITMV {
                fixup_dc(coeffs);
            }
            self.predict_inter(img, m, row, col, coeffs);
        }
    }

    fn predict_inter(&self, img: &mut Img, m: &MbInfo, row: usize, col: usize, coeffs: &[i16; 400]) {
        let (ys, uvs) = (self.ys(), self.uvs());
        let Some(ri) = self.refs[m.ref_frame as usize & 3] else { return };
        let r = &self.frames[ri];
        let filters = if self.version == 0 { &SIXTAP } else { &BILINEAR };
        let full_pixel = self.version == 3;
        let mut chroma = [Mv::default(); 4];
        if m.y_mode != SPLITMV {
            let mut uv = m.mv;
            let f = |v: i16| -> i16 {
                let v = v as i32;
                ((v + 1 + (v >> 31) * 2) / 2) as i16
            };
            uv.x = f(uv.x);
            uv.y = f(uv.y);
            if full_pixel {
                uv.x &= !7;
                uv.y &= !7;
            }
            chroma = [uv; 4];
        } else {
            for (k, &b) in [0usize, 2, 8, 10].iter().enumerate() {
                let sum = |g: fn(Mv) -> i16| -> i16 {
                    let t: i32 = [b, b + 1, b + 4, b + 5].iter().map(|&i| g(m.mvs[i]) as i32).sum();
                    let t = if t < 0 { t - 4 } else { t + 4 };
                    (t / 8) as i16
                };
                let mut mv = Mv { x: sum(|v| v.x), y: sum(|v| v.y) };
                if full_pixel {
                    mv.x &= !7;
                    mv.y &= !7;
                }
                chroma[k] = mv;
            }
        }
        let (w, h) = ((self.mb_cols * 16) as i32, (self.mb_rows * 16) as i32);
        // One 4x4 block of plane `src` (origin `org`, stride `s`, size
        // `pw` x `ph`) at (x, y), moved by `mv`.
        let block = |src: &[u8], org: usize, s: usize, x: i32, y: i32, mv: Mv, pw: i32, ph: i32| -> [u8; 16] {
            let bx = x + (mv.x as i32 >> 3);
            let by = y + (mv.y as i32 >> 3);
            if m.need_mc_border && (bx < 2 || bx + 3 + 3 >= pw || by < 2 || by + 3 + 3 >= ph) {
                let e = edge_block(src, org, s, bx - 2, by - 2, pw, ph);
                let local = Mv { x: mv.x & 7, y: mv.y & 7 };
                filter_block(&e, 2 * 9 + 2, 9, local, filters)
            } else {
                filter_block(src, (org + y as usize * s + x as usize) as isize, s, mv, filters)
            }
        };
        let (x0, y0) = ((col * 16) as i32, (row * 16) as i32);
        let yorg = self.y_origin();
        for b in 0..16 {
            let mv = if m.y_mode == SPLITMV { m.mvs[b] } else { m.mv };
            let (bx, by) = (x0 + (b as i32 & 3) * 4, y0 + (b as i32 >> 2) * 4);
            let pred = block(&r.y, yorg, ys, bx, by, mv, w, h);
            let o = yorg + by as usize * ys + bx as usize;
            idct_add(&mut img.y, o, ys, &pred, 0, 4, &coeffs[b * 16..b * 16 + 16]);
        }
        let uvorg = self.uv_origin();
        let (cx0, cy0) = (x0 / 2, y0 / 2);
        for b in 0..4 {
            let (bx, by) = (cx0 + (b as i32 & 1) * 4, cy0 + (b as i32 >> 1) * 4);
            let o = uvorg + by as usize * uvs + bx as usize;
            let pu = block(&r.u, uvorg, uvs, bx, by, chroma[b], w / 2, h / 2);
            idct_add(&mut img.u, o, uvs, &pu, 0, 4, &coeffs[(16 + b) * 16..(17 + b) * 16]);
            let pv = block(&r.v, uvorg, uvs, bx, by, chroma[b], w / 2, h / 2);
            idct_add(&mut img.v, o, uvs, &pv, 0, 4, &coeffs[(20 + b) * 16..(21 + b) * 16]);
        }
    }
}

/// The second order (Y2) block's inverse Walsh-Hadamard transform gives
/// the DC of each luma block.
fn fixup_dc(coeffs: &mut [i16; 400]) {
    let mut y2 = [0i16; 16];
    walsh(&coeffs[384..400], &mut y2);
    for i in 0..16 {
        coeffs[i * 16] = y2[i];
    }
}

/// The out-of-frame column left of a macroblock: 129, or for DC_PRED below
/// the first row a copy of the row above (so only that counts).
fn fixup_left(p: &mut [u8], o: usize, s: usize, n: usize, row: usize, mode: u8) {
    if mode == DC_PRED && row > 0 {
        for i in 0..n {
            p[o + i * s - 1] = p[o - s + i];
        }
    } else {
        p[o - s - 1] = 129;
        for i in 0..n {
            p[o + i * s - 1] = 129;
        }
    }
}

/// The out-of-frame row above a macroblock: 127, or for DC_PRED after the
/// first column a copy of the left column; plus 127 above-right.
fn fixup_above(p: &mut [u8], o: usize, s: usize, n: usize, col: usize, mode: u8) {
    if mode == DC_PRED && col > 0 {
        for i in 0..n {
            p[o - s + i] = p[o + i * s - 1];
        }
    } else {
        p[o - s - 1..o - s + n].fill(127);
    }
    p[o - s + n..o - s + n + 4].fill(127);
}

// ---------------------------------------------------------------------------
// Loop filter (RFC 6386, 15).

#[inline]
fn s8(x: i32) -> i32 {
    x.clamp(-128, 127)
}

#[inline]
fn u8c(x: i32) -> u8 {
    x.clamp(0, 255) as u8
}

/// Pixels across an edge: `p[o + k * step]` for k in -4..4.
struct Edge<'a> {
    p: &'a mut [u8],
    o: usize,
    step: usize,
}

impl Edge<'_> {
    #[inline]
    fn at(&self, k: isize) -> i32 {
        self.p[(self.o as isize + k * self.step as isize) as usize] as i32
    }

    #[inline]
    fn set(&mut self, k: isize, v: u8) {
        self.p[(self.o as isize + k * self.step as isize) as usize] = v;
    }

    fn simple_threshold(&self, limit: i32) -> bool {
        (self.at(-1) - self.at(0)).abs() * 2 + ((self.at(-2) - self.at(1)).abs() >> 1) <= limit
    }

    fn normal_threshold(&self, e: i32, i: i32) -> bool {
        self.simple_threshold(2 * e + i)
            && (self.at(-4) - self.at(-3)).abs() <= i
            && (self.at(-3) - self.at(-2)).abs() <= i
            && (self.at(-2) - self.at(-1)).abs() <= i
            && (self.at(3) - self.at(2)).abs() <= i
            && (self.at(2) - self.at(1)).abs() <= i
            && (self.at(1) - self.at(0)).abs() <= i
    }

    fn hev(&self, t: i32) -> bool {
        (self.at(-2) - self.at(-1)).abs() > t || (self.at(1) - self.at(0)).abs() > t
    }

    fn common(&mut self, outer: bool) {
        let (p1, p0, q0, q1) = (self.at(-2), self.at(-1), self.at(0), self.at(1));
        let mut a = 3 * (q0 - p0);
        if outer {
            a += s8(p1 - q1);
        }
        let a = s8(a);
        let f1 = (a + 4).min(127) >> 3;
        let f2 = (a + 3).min(127) >> 3;
        self.set(-1, u8c(p0 + f2));
        self.set(0, u8c(q0 - f1));
        if !outer {
            let a = (f1 + 1) >> 1;
            self.set(-2, u8c(p1 + a));
            self.set(1, u8c(q1 - a));
        }
    }

    fn mb(&mut self) {
        let (p2, p1, p0, q0, q1, q2) = (self.at(-3), self.at(-2), self.at(-1), self.at(0), self.at(1), self.at(2));
        let w = s8(s8(p1 - q1) + 3 * (q0 - p0));
        let a = (27 * w + 63) >> 7;
        self.set(-1, u8c(p0 + a));
        self.set(0, u8c(q0 - a));
        let a = (18 * w + 63) >> 7;
        self.set(-2, u8c(p1 + a));
        self.set(1, u8c(q1 - a));
        let a = (9 * w + 63) >> 7;
        self.set(-3, u8c(p2 + a));
        self.set(2, u8c(q2 - a));
    }
}

/// Filters `n` positions along an edge starting at `o`: `across` steps
/// over the edge, `along` moves along it.
#[allow(clippy::too_many_arguments)]
fn filter_edge(p: &mut [u8], o: usize, across: usize, along: usize, n: usize, mb_edge: bool, e: i32, i: i32, hev: i32) {
    for k in 0..n {
        let mut ed = Edge { p, o: o + k * along, step: across };
        if ed.normal_threshold(e, i) {
            if mb_edge {
                if ed.hev(hev) {
                    ed.common(true);
                } else {
                    ed.mb();
                }
            } else {
                let h = ed.hev(hev);
                ed.common(h);
            }
        }
    }
}

fn filter_simple(p: &mut [u8], o: usize, across: usize, along: usize, limit: i32) {
    for k in 0..16 {
        let mut ed = Edge { p, o: o + k * along, step: across };
        if ed.simple_threshold(limit) {
            ed.common(true);
        }
    }
}

impl Decoder {
    fn filter_params(&self, m: &MbInfo) -> (i32, i32, i32) {
        let mut level = self.lf.level;
        if self.seg.enabled {
            let s = self.seg.lf_level[m.segment as usize & 3];
            level = if self.seg.abs { s } else { level + s };
        }
        level = level.clamp(0, 63);
        if self.lf.delta_enabled {
            level += self.lf.ref_delta[m.ref_frame as usize & 3];
            if m.ref_frame == CURRENT as u8 {
                if m.y_mode == B_PRED {
                    level += self.lf.mode_delta[0];
                }
            } else if m.y_mode == ZEROMV {
                level += self.lf.mode_delta[1];
            } else if m.y_mode == SPLITMV {
                level += self.lf.mode_delta[3];
            } else {
                level += self.lf.mode_delta[2];
            }
        }
        let level = level.clamp(0, 63);
        let mut interior = level;
        if self.lf.sharpness > 0 {
            interior >>= if self.lf.sharpness > 4 { 2 } else { 1 };
            interior = interior.min(9 - self.lf.sharpness);
        }
        let interior = interior.max(1);
        let mut hev = (level >= 15) as i32;
        if level >= 40 {
            hev += 1;
        }
        if level >= 20 && !self.keyframe {
            hev += 1;
        }
        (level, interior, hev)
    }

    fn loop_filter(&self, img: &mut Img) {
        let (ys, uvs) = (self.ys(), self.uvs());
        for row in 0..self.mb_rows {
            for col in 0..self.mb_cols {
                let m = &self.mbi[self.mbi_index(row as isize, col as isize)];
                let (e, i, hev) = self.filter_params(m);
                if e == 0 {
                    continue;
                }
                let inner = m.eob_mask != 0 || m.y_mode == SPLITMV || m.y_mode == B_PRED;
                let yo = self.y_origin() + row * 16 * ys + col * 16;
                let uo = self.uv_origin() + row * 8 * uvs + col * 8;
                if self.lf.simple {
                    let mbl = (e + 2) * 2 + i;
                    let bl = e * 2 + i;
                    if col > 0 {
                        filter_simple(&mut img.y, yo, 1, ys, mbl);
                    }
                    if inner {
                        for k in [4, 8, 12] {
                            filter_simple(&mut img.y, yo + k, 1, ys, bl);
                        }
                    }
                    if row > 0 {
                        filter_simple(&mut img.y, yo, ys, 1, mbl);
                    }
                    if inner {
                        for k in [4, 8, 12] {
                            filter_simple(&mut img.y, yo + k * ys, ys, 1, bl);
                        }
                    }
                    continue;
                }
                if col > 0 {
                    filter_edge(&mut img.y, yo, 1, ys, 16, true, e + 2, i, hev);
                    filter_edge(&mut img.u, uo, 1, uvs, 8, true, e + 2, i, hev);
                    filter_edge(&mut img.v, uo, 1, uvs, 8, true, e + 2, i, hev);
                }
                if inner {
                    for k in [4, 8, 12] {
                        filter_edge(&mut img.y, yo + k, 1, ys, 16, false, e, i, hev);
                    }
                    filter_edge(&mut img.u, uo + 4, 1, uvs, 8, false, e, i, hev);
                    filter_edge(&mut img.v, uo + 4, 1, uvs, 8, false, e, i, hev);
                }
                if row > 0 {
                    filter_edge(&mut img.y, yo, ys, 1, 16, true, e + 2, i, hev);
                    filter_edge(&mut img.u, uo, uvs, 1, 8, true, e + 2, i, hev);
                    filter_edge(&mut img.v, uo, uvs, 1, 8, true, e + 2, i, hev);
                }
                if inner {
                    for k in [4, 8, 12] {
                        filter_edge(&mut img.y, yo + k * ys, ys, 1, 16, false, e, i, hev);
                    }
                    filter_edge(&mut img.u, uo + 4 * uvs, uvs, 1, 8, false, e, i, hev);
                    filter_edge(&mut img.v, uo + 4 * uvs, uvs, 1, 8, false, e, i, hev);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Colour conversion, as libwebp does it: "fancy" upsampling of the chroma
// planes and 14-bit fixed point YUV to RGB.

#[inline]
fn clip8(v: i32) -> u8 {
    if v & !16383 == 0 {
        (v >> 6) as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

#[inline]
fn mult_hi(v: i32, c: i32) -> i32 {
    (v * c) >> 8
}

#[inline]
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let (y, u, v) = (y as i32, u as i32, v as i32);
    [
        clip8(mult_hi(y, 19077) + mult_hi(v, 26149) - 14234),
        clip8(mult_hi(y, 19077) - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708),
        clip8(mult_hi(y, 19077) + mult_hi(u, 33050) - 17685),
    ]
}

/// Converts a frame to RGBA (opaque).
pub fn to_rgba(f: &Yuv) -> Vec<[u8; 4]> {
    let (w, h) = (f.w, f.h);
    let mut out = vec![[0, 0, 0, 255]; w * h];
    // One luma row with chroma rows `top` and `cur` (weights 3:1 towards the
    // nearer one), interpolating horizontally too.
    let mut row = |y: usize, top: usize, cur: usize, near_top: bool| {
        let ty = &f.y[y * f.y_stride..];
        let (tu, tv) = (&f.u[top * f.uv_stride..], &f.v[top * f.uv_stride..]);
        let (cu, cv) = (&f.u[cur * f.uv_stride..], &f.v[cur * f.uv_stride..]);
        let o = &mut out[y * w..(y + 1) * w];
        let load = |u: &[u8], v: &[u8], x: usize| u[x] as u32 | (v[x] as u32) << 16;
        let (mut tl, mut l) = (load(tu, tv, 0), load(cu, cv, 0));
        // For a row nearer `top`, (a, b) = (top, cur), else swapped.
        let pick = |t: u32, c: u32| if near_top { (t, c) } else { (c, t) };
        let put = |o: &mut [[u8; 4]], x: usize, uv: u32| {
            let rgb = yuv_to_rgb(ty[x], uv as u8, (uv >> 16) as u8);
            o[x] = [rgb[0], rgb[1], rgb[2], 255];
        };
        let (a, b) = pick(tl, l);
        put(o, 0, (3 * a + b + 0x0002_0002) >> 2);
        for x in 1..=(w - 1) / 2 {
            let (t, c) = (load(tu, tv, x), load(cu, cv, x));
            let avg = tl + t + l + c + 0x0008_0008;
            let diag_12 = (avg + 2 * (t + l)) >> 3;
            let diag_03 = (avg + 2 * (tl + c)) >> 3;
            let (uv0, uv1) = if near_top {
                ((diag_12 + tl) >> 1, (diag_03 + t) >> 1)
            } else {
                ((diag_03 + l) >> 1, (diag_12 + c) >> 1)
            };
            put(o, 2 * x - 1, uv0 & 0x00FF_00FF);
            put(o, 2 * x, uv1 & 0x00FF_00FF);
            tl = t;
            l = c;
        }
        if w % 2 == 0 {
            let (a, b) = pick(tl, l);
            put(o, w - 1, (3 * a + b + 0x0002_0002) >> 2);
        }
    };
    row(0, 0, 0, true);
    let mut y = 1;
    while y + 1 < h {
        let k = y.div_ceil(2);
        row(y, k - 1, k, true);
        row(y + 1, k - 1, k, false);
        y += 2;
    }
    if y < h {
        let k = (h - 1) / 2;
        row(y, k, k, true);
    }
    out
}
