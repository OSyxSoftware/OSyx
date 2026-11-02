//! The CELT layer of Opus (RFC 6716 section 4.3, with the RFC 8251
//! corrections): energy envelope, bit allocation, PVQ shapes, the low-overlap
//! inverse MDCT, the pitch post-filter and the packet loss concealment used
//! for mode transitions. A port of the reference decoder's floating-point
//! build.
//!
//! Integer expressions keep the reference's shape, shifts and all.

#![allow(clippy::precedence)]

use super::celt_tables::*;
use super::range::{ilog, RangeDecoder, BITRES};
use crate::audio::mdct::{Fft, C as Cpx};
use alloc::vec;
use alloc::vec::Vec;

pub const N_BANDS: usize = 21;
pub const EBANDS: [i32; 22] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100];
const OVERLAP: usize = 120;
const SHORT_MDCT: usize = 120;
const MAX_LM: usize = 3;
const DECODE_BUFFER: usize = 2048;
const MAX_PERIOD: usize = 1024;
const LPC_ORDER: usize = 24;
const PREEMPH: f32 = 0.850_006_1;
const COMB_MIN_PERIOD: i32 = 15;

const SPREAD_NONE: usize = 0;
const SPREAD_NORMAL: usize = 2;
const SPREAD_AGGRESSIVE: usize = 3;

const ALLOC_STEPS: i32 = 6;
const FINE_OFFSET: i32 = 21;
const MAX_FINE_BITS: i32 = 8;
const LOG_MAX_PSEUDO: i32 = 6;
const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;

const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
const TAPSET_ICDF: [u8; 3] = [2, 1, 0];
const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];
const TF_SELECT: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];
const LOG2_FRAC: [u8; 24] =
    [0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37];
const PRED_COEF: [f32; 4] = [29440.0 / 32768.0, 26112.0 / 32768.0, 21248.0 / 32768.0, 16384.0 / 32768.0];
const BETA_COEF: [f32; 4] = [30147.0 / 32768.0, 22282.0 / 32768.0, 12124.0 / 32768.0, 6554.0 / 32768.0];
const BETA_INTRA: f32 = 4915.0 / 32768.0;
const ORDERY: [usize; 30] =
    [1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5];
const BIT_INTERLEAVE: [u32; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
const BIT_DEINTERLEAVE: [u32; 16] =
    [0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF];
const COMB_GAINS: [[f32; 3]; 3] =
    [[0.306_640_63, 0.217_041_02, 0.129_638_67], [0.463_867_2, 0.268_066_4, 0.0], [0.799_804_7, 0.100_097_66, 0.0]];

fn exp2(x: f32) -> f32 {
    libm::exp(core::f64::consts::LN_2 * x as f64) as f32
}

fn lcg(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

/// FRAC_MUL16 of the reference: a Q15 product of two 16-bit values.
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15
}

fn bitexact_cos(x: i32) -> i32 {
    let tmp = (4096 + x * x) >> 13;
    let x2 = tmp;
    let x2 = (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)));
    1 + x2
}

fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ilog(icos as u32) as i32;
    let ls = ilog(isin as u32) as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

fn isqrt32(mut val: u32) -> u32 {
    let mut g = 0u32;
    let mut bshift = (ilog(val) as i32 - 1) >> 1;
    let mut b = 1u32 << bshift;
    loop {
        let t = ((g << 1) + b) << bshift;
        if t <= val {
            g += b;
            val -= t;
        }
        b >>= 1;
        bshift -= 1;
        if bshift < 0 {
            return g;
        }
    }
}

fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

fn cache(lm: i32, band: usize) -> &'static [u8] {
    &CACHE_BITS[CACHE_INDEX[(lm + 1) as usize * N_BANDS + band] as usize..]
}

fn bits2pulses(band: usize, lm: i32, bits: i32) -> i32 {
    let c = cache(lm, band);
    let (mut lo, mut hi) = (0i32, c[0] as i32);
    let bits = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if c[mid as usize] as i32 >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let low = if lo == 0 { -1 } else { c[lo as usize] as i32 };
    if bits - low <= c[hi as usize] as i32 - bits {
        lo
    } else {
        hi
    }
}

fn pulses2bits(band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        0
    } else {
        cache(lm, band)[pulses as usize] as i32 + 1
    }
}

// ---------------------------------------------------------------------------
// PVQ codewords (cwrs.c)

fn unext(u: &mut [u32], len: usize, mut u0: u32) {
    let mut j = 1;
    loop {
        let u1 = u[j].wrapping_add(u[j - 1]).wrapping_add(u0);
        u[j - 1] = u0;
        u0 = u1;
        j += 1;
        if j >= len {
            break;
        }
    }
    u[j - 1] = u0;
}

fn uprev(u: &mut [u32], n: usize, mut u0: u32) {
    let mut j = 1;
    loop {
        let u1 = u[j].wrapping_sub(u[j - 1]).wrapping_sub(u0);
        u[j - 1] = u0;
        u0 = u1;
        j += 1;
        if j >= n {
            break;
        }
    }
    u[j - 1] = u0;
}

/// V(n, k), leaving U(n, 0..=k+1) in `u`.
fn ncwrs_urow(n: usize, k: usize, u: &mut [u32]) -> u32 {
    let len = k + 2;
    u[0] = 0;
    u[1] = 1;
    for (kk, v) in u.iter_mut().enumerate().take(len).skip(2) {
        *v = ((kk as u32) << 1) - 1;
    }
    for _ in 2..n {
        unext(&mut u[1..], k + 1, 1);
    }
    u[k].wrapping_add(u[k + 1])
}

fn cwrsi(n: usize, mut k: usize, mut i: u32, y: &mut [i32], u: &mut [u32]) {
    for yj in y.iter_mut().take(n) {
        let p = u[k + 1];
        let s: i32 = if i >= p { -1 } else { 0 };
        if s != 0 {
            i -= p;
        }
        let k0 = k;
        let mut p = u[k];
        while p > i {
            k -= 1;
            p = u[k];
        }
        i -= p;
        let v = (k0 - k) as i32;
        *yj = (v + s) ^ s;
        uprev(u, k + 2, 0);
    }
}

fn decode_pulses(y: &mut [i32], n: usize, k: usize, ec: &mut RangeDecoder) {
    let mut u = vec![0u32; k + 2];
    let v = ncwrs_urow(n, k, &mut u);
    let i = ec.uint(v);
    cwrsi(n, k, i, y, &mut u);
}

// ---------------------------------------------------------------------------
// Vector quantization (vq.c)

fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    if len <= stride {
        return;
    }
    for i in 0..len - stride {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
    if len < 2 * stride + 1 {
        return;
    }
    for i in (0..=len - 2 * stride - 1).rev() {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
}

fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: usize, spread: usize) {
    const FACTOR: [usize; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = FACTOR[spread - 1];
    let gain = len as f32 / (len + factor * k) as f32;
    let theta = 0.5 * gain * gain;
    let c = libm::cos(0.5 * core::f64::consts::PI * theta as f64) as f32;
    let s = libm::cos(0.5 * core::f64::consts::PI * (1.0 - theta) as f64) as f32;
    let mut stride2 = 0;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride;
    for i in 0..stride {
        let x = &mut x[i * len..(i + 1) * len];
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(x, len, stride2, s, c);
            }
            exp_rotation1(x, len, 1, c, s);
        } else {
            exp_rotation1(x, len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(x, len, stride2, s, -c);
            }
        }
    }
}

fn alg_unquant(x: &mut [f32], n: usize, k: usize, spread: usize, b: usize, ec: &mut RangeDecoder, gain: f32) -> u32 {
    let mut iy = vec![0i32; n];
    decode_pulses(&mut iy, n, k, ec);
    let ryy: f32 = iy.iter().map(|&v| (v * v) as f32).sum();
    let g = gain / libm::sqrtf(ryy);
    for (o, &v) in x.iter_mut().zip(&iy) {
        *o = g * v as f32;
    }
    exp_rotation(x, n, -1, b, k, spread);
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut mask = 0;
    for i in 0..b {
        if iy[i * n0..(i + 1) * n0].iter().any(|&v| v != 0) {
            mask |= 1 << i;
        }
    }
    mask
}

fn renormalise(x: &mut [f32], gain: f32) {
    let e = 1e-15 + x.iter().map(|v| v * v).sum::<f32>();
    let g = gain / libm::sqrtf(e);
    x.iter_mut().for_each(|v| *v *= g);
}

// ---------------------------------------------------------------------------
// Band shapes (bands.c)

fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    for i in 0..stride {
        let row = if hadamard { ORDERY[stride - 2 + i] } else { i };
        for j in 0..n0 {
            tmp[row * n0 + j] = x[j * stride + i];
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    for i in 0..stride {
        let row = if hadamard { ORDERY[stride - 2 + i] } else { i };
        for j in 0..n0 {
            tmp[j * stride + i] = x[row * n0 + j];
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let r = core::f32::consts::FRAC_1_SQRT_2;
    for i in 0..stride {
        for j in 0..n0 >> 1 {
            let t1 = r * x[stride * 2 * j + i];
            let t2 = r * x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = t1 + t2;
            x[stride * (2 * j + 1) + i] = t1 - t2;
        }
    }
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let qb = (b - pulse_cap - (4 << BITRES)).min((b + n2 * offset) / n2);
    let qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let qn = EXP2_TABLE8[(qb & 7) as usize] >> (14 - (qb >> BITRES));
        ((qn + 1) >> 1) << 1
    }
}

fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let mut xp = 0.0f32;
    let mut side = 0.0f32;
    for j in 0..n {
        xp += x[j] * y[j];
        side += y[j] * y[j];
    }
    let xp = mid * xp;
    let el = mid * mid + side - 2.0 * xp;
    let er = mid * mid + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        y[..n].copy_from_slice(&x[..n]);
        return;
    }
    let lgain = 1.0 / libm::sqrtf(el);
    let rgain = 1.0 / libm::sqrtf(er);
    for j in 0..n {
        let l = mid * x[j];
        let r = y[j];
        x[j] = lgain * (l - r);
        y[j] = rgain * (l + r);
    }
}

/// What stays the same while one band is coded.
struct BandCtx<'a, 'b> {
    ec: &'a mut RangeDecoder<'b>,
    band: usize,
    intensity: usize,
    spread: usize,
    tf_change: i32,
    remaining: i32,
    seed: u32,
}

impl BandCtx<'_, '_> {
    /// The sign of a one-sample band, when bits are left for it.
    fn sign_bit(&mut self) -> f32 {
        let mut neg = false;
        if self.remaining >= 1 << BITRES {
            neg = self.ec.bits(1) != 0;
            self.remaining -= 1 << BITRES;
        }
        if neg {
            -1.0
        } else {
            1.0
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quant_band(
        &mut self,
        x: &mut [f32],
        mut y: Option<&mut [f32]>,
        n: usize,
        mut b: i32,
        bb: usize,
        mut lowband: Option<&mut [f32]>,
        mut lm: i32,
        lowband_out: Option<&mut [f32]>,
        level: i32,
        gain: f32,
        mut fill: u32,
    ) -> u32 {
        let i = self.band;
        let n0 = n;
        let mut n = n;
        let mut n_b = n;
        let mut big_b = bb;
        let b0_orig = bb;
        let mut time_divide = 0;
        let mut recombine = 0;
        let mut inv = false;
        let long_blocks = b0_orig == 1;
        let mut cm: u32 = 0;
        let stereo = y.is_some();
        let mut split = stereo;
        n_b /= big_b;
        let mut b0 = big_b;
        let mut n_b0 = n_b;

        if n == 1 {
            x[0] = self.sign_bit();
            if let Some(y) = y {
                y[0] = self.sign_bit();
            }
            if let Some(o) = lowband_out {
                o[0] = x[0];
            }
            return 1;
        }

        let mut tf_change = self.tf_change;
        if !stereo && level == 0 {
            if tf_change > 0 {
                recombine = tf_change;
            }
            for k in 0..recombine {
                if let Some(l) = lowband.as_deref_mut() {
                    haar1(l, n >> k, 1 << k);
                }
                fill = BIT_INTERLEAVE[(fill & 0xF) as usize] | (BIT_INTERLEAVE[(fill >> 4) as usize] << 2);
            }
            big_b >>= recombine;
            n_b <<= recombine;
            while (n_b & 1) == 0 && tf_change < 0 {
                if let Some(l) = lowband.as_deref_mut() {
                    haar1(l, n_b, big_b);
                }
                fill |= fill << big_b;
                big_b <<= 1;
                n_b >>= 1;
                time_divide += 1;
                tf_change += 1;
            }
            b0 = big_b;
            n_b0 = n_b;
            if b0 > 1 {
                if let Some(l) = lowband.as_deref_mut() {
                    deinterleave_hadamard(l, n_b >> recombine, b0 << recombine, long_blocks);
                }
            }
        }

        // Split the band in two if it needs more bits than one codebook holds.
        let c = cache(lm, i);
        if !stereo && lm != -1 && b > c[c[0] as usize] as i32 + 12 && n > 2 {
            n >>= 1;
            split = true;
            lm -= 1;
            if big_b == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            big_b = (big_b + 1) >> 1;
        }

        if split {
            let pulse_cap = LOGN[i] as i32 + lm * (1 << BITRES);
            let offset = (pulse_cap >> 1) - if stereo && n == 2 { QTHETA_OFFSET_TWOPHASE } else { QTHETA_OFFSET };
            let mut qn = compute_qn(n as i32, b, offset, pulse_cap, stereo);
            if stereo && i >= self.intensity {
                qn = 1;
            }
            let tell = self.ec.tell_frac() as i32;
            let mut itheta: i32 = 0;
            if qn != 1 {
                if stereo && n > 2 {
                    let p0 = 3;
                    let x0 = qn / 2;
                    let ft = p0 * (x0 + 1) + x0;
                    let fs = self.ec.decode(ft as u32) as i32;
                    let xv = if fs < (x0 + 1) * p0 { fs / p0 } else { x0 + 1 + (fs - (x0 + 1) * p0) };
                    let (fl, fh) = if xv <= x0 {
                        (p0 * xv, p0 * (xv + 1))
                    } else {
                        ((xv - 1 - x0) + (x0 + 1) * p0, (xv - x0) + (x0 + 1) * p0)
                    };
                    self.ec.update(fl as u32, fh as u32, ft as u32);
                    itheta = xv;
                } else if b0 > 1 || stereo {
                    itheta = self.ec.uint((qn + 1) as u32) as i32;
                } else {
                    let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
                    let fm = self.ec.decode(ft as u32) as i32;
                    let (fl, fs);
                    if fm < ((qn >> 1) * ((qn >> 1) + 1) >> 1) {
                        itheta = (isqrt32(8 * fm as u32 + 1) as i32 - 1) >> 1;
                        fs = itheta + 1;
                        fl = itheta * (itheta + 1) >> 1;
                    } else {
                        itheta = (2 * (qn + 1) - isqrt32(8 * (ft - fm - 1) as u32 + 1) as i32) >> 1;
                        fs = qn + 1 - itheta;
                        fl = ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
                    }
                    self.ec.update(fl as u32, (fl + fs) as u32, ft as u32);
                }
                itheta = itheta * 16384 / qn;
            } else if stereo {
                if b > 2 << BITRES && self.remaining > 2 << BITRES {
                    inv = self.ec.bit_logp(2);
                }
                itheta = 0;
            }
            let qalloc = self.ec.tell_frac() as i32 - tell;
            b -= qalloc;

            let orig_fill = fill;
            let (imid, iside, mut delta);
            if itheta == 0 {
                imid = 32767;
                iside = 0;
                fill &= (1 << big_b) - 1;
                delta = -16384;
            } else if itheta == 16384 {
                imid = 0;
                iside = 32767;
                fill &= ((1 << big_b) - 1) << big_b;
                delta = 16384;
            } else {
                imid = bitexact_cos(itheta);
                iside = bitexact_cos(16384 - itheta);
                delta = frac_mul16(((n as i32) - 1) << 7, bitexact_log2tan(iside, imid));
            }
            let mid = imid as f32 / 32768.0;
            let side = iside as f32 / 32768.0;

            if n == 2 && stereo {
                let y = y.as_deref_mut().unwrap();
                let mut mbits = b;
                let mut sbits = 0;
                if itheta != 0 && itheta != 16384 {
                    sbits = 1 << BITRES;
                }
                mbits -= sbits;
                let c = itheta > 8192;
                self.remaining -= qalloc + sbits;
                let mut sign = 0;
                if sbits != 0 {
                    sign = self.ec.bits(1) as i32;
                }
                let sign = (1 - 2 * sign) as f32;
                let (x2, y2): (&mut [f32], &mut [f32]) = if c { (&mut *y, &mut *x) } else { (&mut *x, &mut *y) };
                cm = self.quant_band(x2, None, n, mbits, big_b, lowband, lm, lowband_out, level, gain, orig_fill);
                y2[0] = -sign * x2[1];
                y2[1] = sign * x2[0];
                x[0] *= mid;
                x[1] *= mid;
                y[0] *= side;
                y[1] *= side;
                let t = x[0];
                x[0] = t - y[0];
                y[0] += t;
                let t = x[1];
                x[1] = t - y[1];
                y[1] += t;
                if inv {
                    y[0] = -y[0];
                    y[1] = -y[1];
                }
                return cm;
            }

            if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                if itheta > 8192 {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
                }
            }
            let mut mbits = 0.max(b.min((b - delta) / 2));
            let mut sbits = b - mbits;
            self.remaining -= qalloc;

            let next_level = if stereo { 0 } else { level + 1 };
            let mut rebalance = self.remaining;
            // Mono: the halves of X; stereo: X and Y.
            let (xs, ys): (&mut [f32], &mut [f32]) = match y.as_deref_mut() {
                Some(y) => (&mut x[..n], &mut y[..n]),
                None => {
                    let (a, b) = x.split_at_mut(n);
                    (a, &mut b[..n])
                }
            };
            let (low_mid, low_side): (Option<&mut [f32]>, Option<&mut [f32]>) = match lowband {
                Some(l) if !stereo => {
                    let (a, b) = l.split_at_mut(n.min(l.len()));
                    (Some(a), if b.is_empty() { None } else { Some(b) })
                }
                other => (other, None),
            };
            let (out_mid, mut out_rest) = if stereo { (lowband_out, None) } else { (None, lowband_out) };
            let shift = if stereo { 0 } else { (b0 >> 1) as u32 };
            let mid_gain = if stereo { 1.0 } else { gain * mid };
            if mbits >= sbits {
                cm = self.quant_band(xs, None, n, mbits, big_b, low_mid, lm, out_mid, next_level, mid_gain, fill);
                rebalance = mbits - (rebalance - self.remaining);
                if rebalance > 3 << BITRES && itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                cm |= self.quant_band(
                    ys,
                    None,
                    n,
                    sbits,
                    big_b,
                    low_side,
                    lm,
                    None,
                    next_level,
                    gain * side,
                    fill >> big_b,
                ) << shift;
            } else {
                cm = self.quant_band(
                    ys,
                    None,
                    n,
                    sbits,
                    big_b,
                    low_side,
                    lm,
                    None,
                    next_level,
                    gain * side,
                    fill >> big_b,
                ) << shift;
                rebalance = sbits - (rebalance - self.remaining);
                if rebalance > 3 << BITRES && itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                cm |= self.quant_band(xs, None, n, mbits, big_b, low_mid, lm, out_mid, next_level, mid_gain, fill);
            }
            if stereo {
                let y = y.unwrap();
                if n != 2 {
                    stereo_merge(x, y, mid, n);
                }
                if inv {
                    y[..n].iter_mut().for_each(|v| *v = -*v);
                }
                return cm;
            }
            return self.finish_mono(x, n0, n_b0, b0, recombine, time_divide, long_blocks, level, out_rest.take(), cm);
        }

        // No split: code the pulses of the band.
        let mut q = bits2pulses(i, lm, b);
        let mut curr = pulses2bits(i, lm, q);
        self.remaining -= curr;
        while self.remaining < 0 && q > 0 {
            self.remaining += curr;
            q -= 1;
            curr = pulses2bits(i, lm, q);
            self.remaining -= curr;
        }
        if q != 0 {
            let k = get_pulses(q) as usize;
            cm = alg_unquant(&mut x[..n], n, k, self.spread, big_b, self.ec, gain);
        } else {
            let mask = ((1u64 << big_b) - 1) as u32;
            fill &= mask;
            if fill == 0 {
                x[..n].fill(0.0);
            } else {
                match lowband.as_deref() {
                    None => {
                        for v in x[..n].iter_mut() {
                            self.seed = lcg(self.seed);
                            *v = ((self.seed as i32) >> 20) as f32;
                        }
                        cm = mask;
                    }
                    Some(l) => {
                        for (j, v) in x[..n].iter_mut().enumerate() {
                            self.seed = lcg(self.seed);
                            let t = if self.seed & 0x8000 != 0 { 1.0 / 256.0 } else { -1.0 / 256.0 };
                            *v = l[j] + t;
                        }
                        cm = fill;
                    }
                }
                renormalise(&mut x[..n], gain);
            }
        }
        if stereo {
            return cm;
        }
        self.finish_mono(x, n0, n_b0, b0, recombine, time_divide, long_blocks, level, lowband_out, cm)
    }

    /// Undoes the time-frequency reorganisation of a mono band at level 0
    /// and keeps it for folding.
    #[allow(clippy::too_many_arguments)]
    fn finish_mono(
        &mut self,
        x: &mut [f32],
        n0: usize,
        n_b0: usize,
        b0: usize,
        recombine: i32,
        time_divide: i32,
        long_blocks: bool,
        level: i32,
        lowband_out: Option<&mut [f32]>,
        mut cm: u32,
    ) -> u32 {
        if level != 0 {
            return cm;
        }
        if b0 > 1 {
            interleave_hadamard(x, n_b0 >> recombine, b0 << recombine, long_blocks);
        }
        let mut n_b = n_b0;
        let mut big_b = b0;
        for _ in 0..time_divide {
            big_b >>= 1;
            n_b <<= 1;
            cm |= cm >> big_b;
            haar1(x, n_b, big_b);
        }
        for k in 0..recombine {
            cm = BIT_DEINTERLEAVE[cm as usize & 0xF];
            haar1(x, n0 >> k, 1 << k);
        }
        big_b <<= recombine;
        if let Some(o) = lowband_out {
            let s = libm::sqrtf(n0 as f32);
            for j in 0..n0 {
                o[j] = s * x[j];
            }
        }
        cm & ((1u64 << big_b) - 1) as u32
    }
}

// ---------------------------------------------------------------------------
// Bit allocation (rate.c)

struct Allocation {
    coded_bands: usize,
    intensity: usize,
    dual_stereo: bool,
    balance: i32,
    pulses: [i32; N_BANDS],
    fine_quant: [i32; N_BANDS],
    fine_priority: [i32; N_BANDS],
}

#[allow(clippy::too_many_arguments)]
fn compute_allocation(
    start: usize,
    end: usize,
    offsets: &[i32; N_BANDS],
    cap: &[i32; N_BANDS],
    alloc_trim: i32,
    total: i32,
    c: i32,
    lm: i32,
    ec: &mut RangeDecoder,
) -> Allocation {
    let mut total = total.max(0);
    let len = N_BANDS;
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let (mut intensity_rsv, mut dual_stereo_rsv) = (0, 0);
    if c == 2 {
        intensity_rsv = LOG2_FRAC[end - start] as i32;
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut thresh = [0i32; N_BANDS];
    let mut trim_offset = [0i32; N_BANDS];
    let width = |j: usize| EBANDS[j + 1] - EBANDS[j];
    for j in start..end {
        thresh[j] = (c << BITRES).max((3 * width(j) << lm << BITRES) >> 4);
        trim_offset[j] =
            c * width(j) * (alloc_trim - 5 - lm) * (end as i32 - j as i32 - 1) * (1 << (lm + BITRES as i32)) >> 6;
        if width(j) << lm == 1 {
            trim_offset[j] -= c << BITRES;
        }
    }
    let (mut lo, mut hi) = (1i32, ALLOC.len() as i32 / len as i32 - 1);
    let nb_vectors = ALLOC.len() / len;
    loop {
        let mut done = false;
        let mut psum = 0;
        let mid = (lo + hi) >> 1;
        for j in (start..end).rev() {
            let mut bitsj = c * width(j) * ALLOC[mid as usize * len + j] as i32 * (1 << lm) >> 2;
            if bitsj > 0 {
                bitsj = 0.max(bitsj + trim_offset[j]);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= c << BITRES {
                psum += c << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    let mut bits1 = [0i32; N_BANDS];
    let mut bits2 = [0i32; N_BANDS];
    for j in start..end {
        let mut b1 = c * width(j) * ALLOC[lo as usize * len + j] as i32 * (1 << lm) >> 2;
        let mut b2 = if hi as usize >= nb_vectors {
            cap[j]
        } else {
            c * width(j) * ALLOC[hi as usize * len + j] as i32 * (1 << lm) >> 2
        };
        if b1 > 0 {
            b1 = 0.max(b1 + trim_offset[j]);
        }
        if b2 > 0 {
            b2 = 0.max(b2 + trim_offset[j]);
        }
        if lo > 0 {
            b1 += offsets[j];
        }
        b2 += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        b2 = 0.max(b2 - b1);
        bits1[j] = b1;
        bits2[j] = b2;
    }
    interp_bits2pulses(
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        skip_rsv,
        intensity_rsv,
        dual_stereo_rsv,
        c,
        lm,
        ec,
    )
}

#[allow(clippy::too_many_arguments)]
fn interp_bits2pulses(
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; N_BANDS],
    bits2: &[i32; N_BANDS],
    thresh: &[i32; N_BANDS],
    cap: &[i32; N_BANDS],
    mut total: i32,
    skip_rsv: i32,
    mut intensity_rsv: i32,
    mut dual_stereo_rsv: i32,
    c: i32,
    lm: i32,
    ec: &mut RangeDecoder,
) -> Allocation {
    let alloc_floor = c << BITRES;
    let stereo = (c > 1) as i32;
    let log_m = lm << BITRES;
    let (mut lo, mut hi) = (0, 1 << ALLOC_STEPS);
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + (mid * bits2[j] >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut bits = [0i32; N_BANDS];
    let mut psum = 0;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + (lo * bits2[j] >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }
    let width = |a: usize, b: usize| EBANDS[b] - EBANDS[a];
    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let percoeff = left / width(start, coded_bands);
        left -= width(start, coded_bands) * percoeff;
        let rem = 0.max(left - width(start, j));
        let band_width = width(j, coded_bands);
        let mut band_bits = bits[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if ec.bit_logp(1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = LOG2_FRAC[j - start] as i32;
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded_bands -= 1;
    }
    let mut intensity = 0;
    if intensity_rsv > 0 {
        intensity = start + ec.uint((coded_bands + 1 - start) as u32) as usize;
    }
    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = dual_stereo_rsv > 0 && ec.bit_logp(1);
    let mut left = total - psum;
    let percoeff = left / width(start, coded_bands);
    left -= width(start, coded_bands) * percoeff;
    for j in start..coded_bands {
        bits[j] += percoeff * width(j, j + 1);
    }
    for j in start..coded_bands {
        let tmp = left.min(width(j, j + 1));
        bits[j] += tmp;
        left -= tmp;
    }
    let mut ebits = [0i32; N_BANDS];
    let mut fine_priority = [0i32; N_BANDS];
    let mut balance = 0;
    let mut j = start;
    while j < coded_bands {
        let n0 = width(j, j + 1);
        let n = n0 << lm;
        bits[j] += balance;
        let mut excess;
        if n > 1 {
            excess = 0.max(bits[j] - cap[j]);
            bits[j] -= excess;
            let den = c * n + if c == 2 && n > 2 && !dual_stereo && j < intensity { 1 } else { 0 };
            let nclogn = den * (LOGN[j] as i32 + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if bits[j] + offset < den * 2 << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < den * 3 << BITRES {
                offset += nclogn >> 3;
            }
            ebits[j] = 0.max((bits[j] + offset + (den << (BITRES - 1))) / (den << BITRES));
            if c * ebits[j] > bits[j] >> BITRES {
                ebits[j] = bits[j] >> stereo >> BITRES;
            }
            ebits[j] = ebits[j].min(MAX_FINE_BITS);
            fine_priority[j] = (ebits[j] * (den << BITRES) >= bits[j] + offset) as i32;
            bits[j] -= c * ebits[j] << BITRES;
        } else {
            excess = 0.max(bits[j] - (c << BITRES));
            bits[j] -= excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + BITRES as i32)).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = extra_fine * c << BITRES;
            fine_priority[j] = (extra_bits >= excess - balance) as i32;
            excess -= extra_bits;
        }
        balance = excess;
        j += 1;
    }
    while j < end {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        fine_priority[j] = (ebits[j] < 1) as i32;
        j += 1;
    }
    Allocation { coded_bands, intensity, dual_stereo, balance, pulses: bits, fine_quant: ebits, fine_priority }
}

// ---------------------------------------------------------------------------
// The inverse MDCT of the reference (mdct.c), on our FFT.

struct CeltMdct {
    trig: Vec<f32>,
    ffts: Vec<Fft>,
    a: Vec<Cpx>,
    b: Vec<Cpx>,
    f2: Vec<f32>,
}

const MDCT_N: usize = 2 * SHORT_MDCT << MAX_LM;

impl CeltMdct {
    fn new() -> CeltMdct {
        let n4 = MDCT_N / 4;
        let trig = (0..=n4).map(|i| libm::cos(2.0 * core::f64::consts::PI * i as f64 / MDCT_N as f64) as f32).collect();
        let ffts = (0..=MAX_LM).map(|s| Fft::new(MDCT_N >> s >> 2)).collect();
        CeltMdct { trig, ffts, a: vec![Cpx::default(); n4], b: vec![Cpx::default(); n4], f2: vec![0.0; MDCT_N / 2] }
    }

    /// clt_mdct_backward: `input[off + k * stride]` are the coefficients,
    /// the windowed result is added at `out[base..]` (base may be negative).
    #[allow(clippy::too_many_arguments)]
    fn backward(&mut self, input: &[f32], off: usize, stride: usize, out: &mut [f32], base: isize, shift: usize) {
        let n = MDCT_N >> shift;
        let n2 = n >> 1;
        let n4 = n >> 2;
        let t = &self.trig;
        let sine = 2.0 * core::f32::consts::PI * 0.125 / n as f32;
        for i in 0..n4 {
            let x1 = input[off + 2 * i * stride];
            let x2 = input[off + stride * (n2 - 1 - 2 * i)];
            let yr = -x2 * t[i << shift] + x1 * t[(n4 - i) << shift];
            let yi = -x2 * t[(n4 - i) << shift] - x1 * t[i << shift];
            self.a[i] = Cpx { re: yr - yi * sine, im: yi + yr * sine };
        }
        self.ffts[shift].run_inverse(&mut self.a[..n4], &mut self.b[..n4]);
        for i in 0..n4 {
            let (re, im) = (self.b[i].re, self.b[i].im);
            let yr = re * t[i << shift] - im * t[(n4 - i) << shift];
            let yi = im * t[i << shift] + re * t[(n4 - i) << shift];
            self.b[i] = Cpx { re: yr - yi * sine, im: yi + yr * sine };
        }
        // De-shuffle: f2[2i] = -f[2i], f2[2i + 1] = f[n2 - 1 - 2i].
        let flat = |k: usize| if k & 1 == 0 { self.b[k >> 1].re } else { self.b[k >> 1].im };
        for i in 0..n4 {
            self.f2[2 * i] = -flat(2 * i);
            self.f2[2 * i + 1] = flat(n2 - 1 - 2 * i);
        }
        let f2 = &self.f2;
        let base = base - ((n2 - OVERLAP) >> 1) as isize;
        let at = |k: isize| (base + k) as usize;
        let ov2 = OVERLAP / 2;
        // Mirror on both sides for TDAC.
        let mut fp1 = n4 as isize - 1;
        let mut xp1 = n2 as isize - 1;
        let mut yp1 = (n4 - ov2) as isize;
        for _ in 0..n4 - ov2 {
            out[at(xp1)] = f2[fp1 as usize];
            xp1 -= 1;
            fp1 -= 1;
        }
        for k in 0..ov2 {
            let x1 = f2[fp1 as usize];
            fp1 -= 1;
            out[at(yp1)] += -WINDOW[k] * x1;
            out[at(xp1)] += WINDOW[OVERLAP - 1 - k] * x1;
            yp1 += 1;
            xp1 -= 1;
        }
        let mut fp2 = n4;
        let mut xp2 = n2 as isize;
        let mut yp2 = (n - 1 - (n4 - ov2)) as isize;
        for _ in 0..n4 - ov2 {
            out[at(xp2)] = f2[fp2];
            xp2 += 1;
            fp2 += 1;
        }
        for k in 0..ov2 {
            let x2 = f2[fp2];
            fp2 += 1;
            out[at(yp2)] = WINDOW[k] * x2;
            out[at(xp2)] = WINDOW[OVERLAP - 1 - k] * x2;
            yp2 -= 1;
            xp2 += 1;
        }
    }
}

/// The comb post-filter, in place on `buf[at..at + n]` (it reads back up to
/// the longest period plus two samples before `at`).
#[allow(clippy::too_many_arguments)]
fn comb_filter(
    buf: &mut [f32],
    at: usize,
    t0: i32,
    t1: i32,
    n: usize,
    g0: f32,
    g1: f32,
    tap0: usize,
    tap1: usize,
    overlap: usize,
) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    let (t0, t1) = (t0 as usize, t1 as usize);
    let g00 = g0 * COMB_GAINS[tap0][0];
    let g01 = g0 * COMB_GAINS[tap0][1];
    let g02 = g0 * COMB_GAINS[tap0][2];
    let g10 = g1 * COMB_GAINS[tap1][0];
    let g11 = g1 * COMB_GAINS[tap1][1];
    let g12 = g1 * COMB_GAINS[tap1][2];
    for i in 0..overlap.min(n) {
        let f = WINDOW[i] * WINDOW[i];
        let x = |k: usize| buf[at + i - k];
        let v = buf[at + i]
            + (1.0 - f) * g00 * x(t0)
            + (1.0 - f) * g01 * (x(t0 + 1) + x(t0 - 1))
            + (1.0 - f) * g02 * (x(t0 + 2) + x(t0 - 2))
            + f * g10 * x(t1)
            + f * g11 * (x(t1 + 1) + x(t1 - 1))
            + f * g12 * (x(t1 + 2) + x(t1 - 2));
        buf[at + i] = v;
    }
    for i in overlap.min(n)..n {
        let x = |k: usize| buf[at + i - k];
        let v = buf[at + i] + g10 * x(t1) + g11 * (x(t1 + 1) + x(t1 - 1)) + g12 * (x(t1 + 2) + x(t1 - 2));
        buf[at + i] = v;
    }
}

pub struct Celt {
    /// Output channels.
    channels: usize,
    /// Channels coded in the current packet.
    pub stream_channels: usize,
    pub start: usize,
    pub end: usize,
    pub rng: u32,
    last_pitch_index: i32,
    loss_count: i32,
    pf_period: i32,
    pf_period_old: i32,
    pf_gain: f32,
    pf_gain_old: f32,
    pf_tapset: usize,
    pf_tapset_old: usize,
    preemph_mem: [f32; 2],
    decode_mem: [Vec<f32>; 2],
    lpc: [[f32; LPC_ORDER]; 2],
    old_band_e: [f32; 2 * N_BANDS],
    old_log_e: [f32; 2 * N_BANDS],
    old_log_e2: [f32; 2 * N_BANDS],
    background_log_e: [f32; 2 * N_BANDS],
    mdct: CeltMdct,
}

impl Celt {
    pub fn new(channels: usize) -> Celt {
        let mut c = Celt {
            channels,
            stream_channels: channels,
            start: 0,
            end: N_BANDS,
            rng: 0,
            last_pitch_index: 0,
            loss_count: 0,
            pf_period: 0,
            pf_period_old: 0,
            pf_gain: 0.0,
            pf_gain_old: 0.0,
            pf_tapset: 0,
            pf_tapset_old: 0,
            preemph_mem: [0.0; 2],
            decode_mem: [vec![0.0; DECODE_BUFFER + OVERLAP], vec![0.0; DECODE_BUFFER + OVERLAP]],
            lpc: [[0.0; LPC_ORDER]; 2],
            old_band_e: [0.0; 2 * N_BANDS],
            old_log_e: [0.0; 2 * N_BANDS],
            old_log_e2: [0.0; 2 * N_BANDS],
            background_log_e: [0.0; 2 * N_BANDS],
            mdct: CeltMdct::new(),
        };
        c.reset();
        c
    }

    /// OPUS_RESET_STATE.
    pub fn reset(&mut self) {
        self.rng = 0;
        self.last_pitch_index = 0;
        self.loss_count = 0;
        self.pf_period = 0;
        self.pf_period_old = 0;
        self.pf_gain = 0.0;
        self.pf_gain_old = 0.0;
        self.pf_tapset = 0;
        self.pf_tapset_old = 0;
        self.preemph_mem = [0.0; 2];
        for m in &mut self.decode_mem {
            m.fill(0.0);
        }
        self.lpc = [[0.0; LPC_ORDER]; 2];
        self.old_band_e = [0.0; 2 * N_BANDS];
        self.old_log_e = [-28.0; 2 * N_BANDS];
        self.old_log_e2 = [-28.0; 2 * N_BANDS];
        self.background_log_e = [0.0; 2 * N_BANDS];
    }

    fn log2_amp(&self, band_e: &mut [f32; 2 * N_BANDS], log_e: &[f32; 2 * N_BANDS], c: usize) {
        for ch in 0..c {
            for i in 0..N_BANDS {
                band_e[ch * N_BANDS + i] = if i < self.start || i >= self.end {
                    0.0
                } else {
                    let lg = (log_e[ch * N_BANDS + i] + E_MEANS[i]).min(32.0);
                    exp2(lg)
                };
            }
        }
    }

    /// The inverse MDCTs of `freq` (channel c at c * n) into the output
    /// buffers, with overlap-add.
    fn inverse_mdcts(&mut self, freq: &[f32], short_blocks: usize, n: usize, lm: usize) {
        let cc = self.channels;
        let mut x = vec![0.0f32; n + OVERLAP];
        for c in 0..cc {
            let (n2, b, shift) =
                if short_blocks > 0 { (SHORT_MDCT, short_blocks, MAX_LM) } else { (n, 1, MAX_LM - lm) };
            x[..OVERLAP].fill(0.0);
            for blk in 0..b {
                self.mdct.backward(freq, blk + c * n2 * b, b, &mut x, (n2 * blk) as isize, shift);
            }
            let mem = &mut self.decode_mem[c];
            let out = DECODE_BUFFER - n;
            for j in 0..OVERLAP {
                mem[out + j] = x[j] + mem[DECODE_BUFFER + j];
            }
            mem[out + OVERLAP..out + n].copy_from_slice(&x[OVERLAP..n]);
            mem[DECODE_BUFFER..DECODE_BUFFER + OVERLAP].copy_from_slice(&x[n..n + OVERLAP]);
        }
    }

    /// De-emphasis of the last `n` samples into interleaved `pcm`.
    fn deemphasis(&mut self, pcm: &mut [f32], n: usize) {
        let cc = self.channels;
        for c in 0..cc {
            let mut m = self.preemph_mem[c];
            let x = &self.decode_mem[c][DECODE_BUFFER - n..DECODE_BUFFER];
            for (j, &v) in x.iter().enumerate() {
                let tmp = v + m;
                m = PREEMPH * tmp;
                pcm[j * cc + c] = tmp * (1.0 / 32768.0);
            }
            self.preemph_mem[c] = m;
        }
    }

    /// Decodes one CELT frame of `frame_size` samples (120 << LM) into the
    /// interleaved `pcm`. `ec` is shared with SILK in hybrid frames; `len`
    /// is the size of the frame in bytes. With no data, the frame is
    /// concealed.
    pub fn decode(
        &mut self,
        ec: Option<&mut RangeDecoder>,
        data: Option<&[u8]>,
        len: usize,
        pcm: &mut [f32],
        frame_size: usize,
    ) -> Result<usize, &'static str> {
        let lm = (0..=MAX_LM).find(|&l| SHORT_MDCT << l == frame_size).ok_or("bad CELT frame size")?;
        if len > 1275 {
            return Err("CELT frame too long");
        }
        let Some(data) = data.filter(|_| len > 1) else {
            self.decode_lost(pcm, frame_size, lm);
            return Ok(frame_size);
        };
        match ec {
            Some(ec) => self.decode_frame(ec, len, pcm, lm)?,
            None => self.decode_frame(&mut RangeDecoder::new(&data[..len.min(data.len())]), len, pcm, lm)?,
        }
        Ok(frame_size)
    }

    fn decode_frame(
        &mut self,
        ec: &mut RangeDecoder,
        len: usize,
        pcm: &mut [f32],
        lm: usize,
    ) -> Result<(), &'static str> {
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let c = self.stream_channels;
        let cc = self.channels;
        let start = self.start;
        let end = self.end;
        let eff_end = end.min(N_BANDS);

        if c == 1 {
            for i in 0..N_BANDS {
                self.old_band_e[i] = self.old_band_e[i].max(self.old_band_e[N_BANDS + i]);
            }
        }
        let mut total_bits = (len * 8) as i32;
        let mut tell = ec.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            ec.bit_logp(15)
        } else {
            false
        };
        if silence {
            // Pretend all the remaining bits were read.
            tell = (len * 8) as i32;
            ec.skip_to(tell);
        }
        let (mut pf_gain, mut pf_pitch, mut pf_tapset) = (0.0f32, 0i32, 0usize);
        if start == 0 && tell + 16 <= total_bits {
            if ec.bit_logp(1) {
                let octave = ec.uint(6);
                pf_pitch = ((16 << octave) + ec.bits(4 + octave)) as i32 - 1;
                let qg = ec.bits(3);
                if ec.tell() + 2 <= total_bits {
                    pf_tapset = ec.icdf(&TAPSET_ICDF, 2);
                }
                pf_gain = 0.09375 * (qg + 1) as f32;
            }
            tell = ec.tell();
        }
        let mut is_transient = false;
        if lm > 0 && tell + 3 <= total_bits {
            is_transient = ec.bit_logp(3);
            tell = ec.tell();
        }
        let short_blocks = if is_transient { m } else { 0 };
        let intra = if tell + 3 <= total_bits { ec.bit_logp(3) } else { false };

        self.unquant_coarse_energy(intra, ec, c, lm);

        // tf_decode
        let mut tf_res = [0i32; N_BANDS];
        {
            let mut budget = (ec.storage() * 8) as i32;
            let mut tell = ec.tell();
            let mut logp = if is_transient { 2 } else { 4 };
            let rsv = lm > 0 && tell + logp < budget;
            budget -= rsv as i32;
            let (mut changed, mut curr) = (0, 0);
            for r in tf_res.iter_mut().take(end).skip(start) {
                if tell + logp <= budget {
                    curr ^= ec.bit_logp(logp as u32) as i32;
                    tell = ec.tell();
                    changed |= curr;
                }
                *r = curr;
                logp = if is_transient { 4 } else { 5 };
            }
            let t = 4 * is_transient as usize;
            let mut select = 0;
            if rsv && TF_SELECT[lm][t + changed as usize] != TF_SELECT[lm][t + 2 + changed as usize] {
                select = ec.bit_logp(1) as usize;
            }
            for r in tf_res.iter_mut().take(end).skip(start) {
                *r = TF_SELECT[lm][t + 2 * select + *r as usize] as i32;
            }
        }

        tell = ec.tell();
        let mut spread = SPREAD_NORMAL;
        if tell + 4 <= total_bits {
            spread = ec.icdf(&SPREAD_ICDF, 5);
        }

        let mut cap = [0i32; N_BANDS];
        for (i, cp) in cap.iter_mut().enumerate() {
            let w = (EBANDS[i + 1] - EBANDS[i]) << lm;
            *cp = (CACHE_CAPS[N_BANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * w >> 2;
        }
        let mut offsets = [0i32; N_BANDS];
        let mut dynalloc_logp = 6;
        total_bits <<= BITRES;
        let mut tellf = ec.tell_frac() as i32;
        for i in start..end {
            let width = (c as i32 * (EBANDS[i + 1] - EBANDS[i])) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tellf + (loop_logp << BITRES) < total_bits && boost < cap[i] {
                let flag = ec.bit_logp(loop_logp as u32);
                tellf = ec.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits -= quanta;
                loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }
        let alloc_trim = if tellf + (6 << BITRES) <= total_bits { ec.icdf(&TRIM_ICDF, 7) as i32 } else { 5 };

        let mut bits = (((len * 8) as i32) << BITRES) - ec.tell_frac() as i32 - 1;
        let anti_collapse_rsv =
            if is_transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) { 1 << BITRES } else { 0 };
        bits -= anti_collapse_rsv;
        let alloc = compute_allocation(start, end, &offsets, &cap, alloc_trim, bits, c as i32, lm as i32, ec);

        self.unquant_fine_energy(&alloc.fine_quant, ec, c);

        // Shapes.
        let mut x = vec![0.0f32; n];
        let mut y = vec![0.0f32; if c == 2 { n } else { 0 }];
        let mut collapse = [0u8; 2 * N_BANDS];
        self.quant_all_bands(
            &mut x,
            if c == 2 { Some(&mut y) } else { None },
            &mut collapse,
            &alloc,
            short_blocks,
            spread,
            &tf_res,
            ((len * (8 << BITRES)) as i32) - anti_collapse_rsv,
            ec,
            lm,
        );
        let mut anti_collapse_on = false;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = ec.bits(1) != 0;
        }
        let left = (len * 8) as i32 - ec.tell();
        self.unquant_energy_finalise(&alloc.fine_quant, &alloc.fine_priority, left, ec, c);
        if anti_collapse_on {
            self.anti_collapse(&mut x, &mut y, &collapse, lm, c, &alloc.pulses);
        }
        let mut band_e = [0.0f32; 2 * N_BANDS];
        let old = self.old_band_e;
        self.log2_amp(&mut band_e, &old, c);
        if silence {
            band_e = [0.0; 2 * N_BANDS];
            for v in self.old_band_e.iter_mut().take(c * N_BANDS) {
                *v = -28.0;
            }
        }

        // Synthesis.
        let mut freq = vec![0.0f32; cc.max(c) * n];
        for ch in 0..c {
            let src = if ch == 0 { &x } else { &y };
            let f = &mut freq[ch * n..(ch + 1) * n];
            for i in 0..eff_end {
                let g = band_e[ch * N_BANDS + i];
                for j in (m * EBANDS[i] as usize)..(m * EBANDS[i + 1] as usize) {
                    f[j] = src[j] * g;
                }
            }
            f[..m * EBANDS[start] as usize].fill(0.0);
            f[m * EBANDS[eff_end] as usize..].fill(0.0);
        }
        for mem in self.decode_mem.iter_mut().take(cc) {
            mem.copy_within(n..DECODE_BUFFER, 0);
        }
        if cc == 2 && c == 1 {
            freq.copy_within(0..n, n);
        }
        if cc == 1 && c == 2 {
            for i in 0..n {
                freq[i] = 0.5 * (freq[i] + freq[n + i]);
            }
        }
        self.inverse_mdcts(&freq, short_blocks, n, lm);

        let out = DECODE_BUFFER - n;
        for ch in 0..cc {
            self.pf_period = self.pf_period.max(COMB_MIN_PERIOD);
            self.pf_period_old = self.pf_period_old.max(COMB_MIN_PERIOD);
            comb_filter(
                &mut self.decode_mem[ch],
                out,
                self.pf_period_old,
                self.pf_period,
                SHORT_MDCT,
                self.pf_gain_old,
                self.pf_gain,
                self.pf_tapset_old,
                self.pf_tapset,
                OVERLAP,
            );
            if lm != 0 {
                comb_filter(
                    &mut self.decode_mem[ch],
                    out + SHORT_MDCT,
                    self.pf_period,
                    pf_pitch,
                    n - SHORT_MDCT,
                    self.pf_gain,
                    pf_gain,
                    self.pf_tapset,
                    pf_tapset,
                    OVERLAP,
                );
            }
        }
        self.pf_period_old = self.pf_period;
        self.pf_gain_old = self.pf_gain;
        self.pf_tapset_old = self.pf_tapset;
        self.pf_period = pf_pitch;
        self.pf_gain = pf_gain;
        self.pf_tapset = pf_tapset;
        if lm != 0 {
            self.pf_period_old = self.pf_period;
            self.pf_gain_old = self.pf_gain;
            self.pf_tapset_old = self.pf_tapset;
        }
        if c == 1 {
            let (a, b) = self.old_band_e.split_at_mut(N_BANDS);
            b.copy_from_slice(a);
        }
        if !is_transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
            for i in 0..2 * N_BANDS {
                self.background_log_e[i] = (self.background_log_e[i] + m as f32 * 0.001).min(self.old_band_e[i]);
            }
        } else {
            for i in 0..2 * N_BANDS {
                self.old_log_e[i] = self.old_log_e[i].min(self.old_band_e[i]);
            }
        }
        for ch in 0..2 {
            for i in (0..start).chain(end..N_BANDS) {
                self.old_band_e[ch * N_BANDS + i] = 0.0;
                self.old_log_e[ch * N_BANDS + i] = -28.0;
                self.old_log_e2[ch * N_BANDS + i] = -28.0;
            }
        }
        self.rng = ec.range();
        self.deemphasis(pcm, n);
        self.loss_count = 0;
        if ec.tell() > 8 * len as i32 {
            return Err("CELT frame overrun");
        }
        Ok(())
    }

    fn unquant_coarse_energy(&mut self, intra: bool, ec: &mut RangeDecoder, c: usize, lm: usize) {
        let prob = &E_PROB_MODEL[(lm * 2 + intra as usize) * 42..];
        let mut prev = [0.0f32; 2];
        let (coef, beta) = if intra { (0.0, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
        let budget = (ec.storage() * 8) as i32;
        for i in self.start..self.end {
            for (ch, p) in prev.iter_mut().enumerate().take(c) {
                let tell = ec.tell();
                let qi = if budget - tell >= 15 {
                    let pi = 2 * i.min(20);
                    ec.laplace((prob[pi] as u32) << 7, (prob[pi + 1] as u32) << 6)
                } else if budget - tell >= 2 {
                    let q = ec.icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                    (q >> 1) ^ -(q & 1)
                } else if budget - tell >= 1 {
                    -(ec.bit_logp(1) as i32)
                } else {
                    -1
                };
                let q = qi as f32;
                let o = &mut self.old_band_e[i + ch * N_BANDS];
                *o = o.max(-9.0);
                let tmp = coef * *o + *p + q;
                *o = tmp;
                *p = *p + q - beta * q;
            }
        }
    }

    fn unquant_fine_energy(&mut self, fine: &[i32; N_BANDS], ec: &mut RangeDecoder, c: usize) {
        for i in self.start..self.end {
            if fine[i] <= 0 {
                continue;
            }
            for ch in 0..c {
                let q2 = ec.bits(fine[i] as u32);
                let offset = (q2 as f32 + 0.5) * (1 << (14 - fine[i])) as f32 * (1.0 / 16384.0) - 0.5;
                self.old_band_e[i + ch * N_BANDS] += offset;
            }
        }
    }

    fn unquant_energy_finalise(
        &mut self,
        fine: &[i32; N_BANDS],
        prio: &[i32; N_BANDS],
        mut left: i32,
        ec: &mut RangeDecoder,
        c: usize,
    ) {
        for p in 0..2 {
            let mut i = self.start;
            while i < self.end && left >= c as i32 {
                if fine[i] >= MAX_FINE_BITS || prio[i] != p {
                    i += 1;
                    continue;
                }
                for ch in 0..c {
                    let q2 = ec.bits(1);
                    let offset = (q2 as f32 - 0.5) * (1 << (14 - fine[i] - 1)) as f32 * (1.0 / 16384.0);
                    self.old_band_e[i + ch * N_BANDS] += offset;
                    left -= 1;
                }
                i += 1;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quant_all_bands(
        &mut self,
        x_all: &mut [f32],
        mut y_all: Option<&mut [f32]>,
        collapse: &mut [u8; 2 * N_BANDS],
        alloc: &Allocation,
        short_blocks: usize,
        spread: usize,
        tf_res: &[i32; N_BANDS],
        total_bits: i32,
        ec: &mut RangeDecoder,
        lm: usize,
    ) {
        let start = self.start;
        let end = self.end;
        let m = 1usize << lm;
        let c = if y_all.is_some() { 2 } else { 1 };
        let bb = if short_blocks > 0 { m } else { 1 };
        let total = m * EBANDS[N_BANDS] as usize;
        let mut norm = vec![0.0f32; total];
        let mut norm2 = vec![0.0f32; if c == 2 { total } else { 0 }];
        let mut lowband_offset = 0usize;
        let mut update_lowband = true;
        let mut balance = alloc.balance;
        let mut dual_stereo = alloc.dual_stereo;
        let mut ctx =
            BandCtx { ec, band: 0, intensity: alloc.intensity, spread, tf_change: 0, remaining: 0, seed: self.rng };
        let mut lowband = Vec::new();
        let mut lowband2 = Vec::new();
        for i in start..end {
            let off = m * EBANDS[i] as usize;
            let n = m * EBANDS[i + 1] as usize - off;
            let tell = ctx.ec.tell_frac() as i32;
            if i != start {
                balance -= tell;
            }
            ctx.remaining = total_bits - tell - 1;
            let b = if i < alloc.coded_bands {
                let curr_balance = balance / 3.min(alloc.coded_bands as i32 - i as i32);
                0.max(16383.min((ctx.remaining + 1).min(alloc.pulses[i] + curr_balance)))
            } else {
                0
            };
            if (off as i32 - n as i32 >= (m * EBANDS[start] as usize) as i32 || i == start + 1)
                && (update_lowband || lowband_offset == 0)
            {
                lowband_offset = i;
            }
            if i == start + 1 {
                // RFC 8251: repeat part of the first band so the second one
                // can always fold.
                let n1 = m * (EBANDS[start + 1] - EBANDS[start]) as usize;
                let n2 = m * (EBANDS[start + 2] - EBANDS[start + 1]) as usize;
                let o = m * EBANDS[start] as usize;
                if n2 > n1 {
                    norm.copy_within(o + 2 * n1 - n2..o + n1, o + n1);
                    if c == 2 {
                        norm2.copy_within(o + 2 * n1 - n2..o + n1, o + n1);
                    }
                }
            }
            ctx.band = i;
            ctx.tf_change = tf_res[i];
            let mut effective_lowband: isize = -1;
            let (mut x_cm, mut y_cm);
            if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || bb > 1 || ctx.tf_change < 0) {
                effective_lowband = ((m * EBANDS[start] as usize) as isize)
                    .max((m * EBANDS[lowband_offset] as usize) as isize - n as isize);
                let mut fold_start = lowband_offset;
                loop {
                    fold_start -= 1;
                    if ((m * EBANDS[fold_start] as usize) as isize) <= effective_lowband {
                        break;
                    }
                }
                let mut fold_end = lowband_offset - 1;
                loop {
                    fold_end += 1;
                    if !(fold_end < i && ((m * EBANDS[fold_end] as usize) as isize) < effective_lowband + n as isize) {
                        break;
                    }
                }
                x_cm = 0u32;
                y_cm = 0u32;
                let mut f = fold_start;
                loop {
                    x_cm |= collapse[f * c] as u32;
                    y_cm |= collapse[f * c + c - 1] as u32;
                    f += 1;
                    if f >= fold_end {
                        break;
                    }
                }
            } else {
                x_cm = (1 << bb) - 1;
                y_cm = x_cm;
            }
            if dual_stereo && i == ctx.intensity {
                dual_stereo = false;
                for j in m * EBANDS[start] as usize..off {
                    norm[j] = 0.5 * (norm[j] + norm2[j]);
                }
            }
            let take_low = |src: &[f32], dst: &mut Vec<f32>| -> bool {
                if effective_lowband < 0 {
                    return false;
                }
                let s = effective_lowband as usize;
                dst.clear();
                dst.extend_from_slice(&src[s..(s + n).min(src.len())]);
                dst.resize(n, 0.0);
                true
            };
            let x = &mut x_all[off..off + n];
            if dual_stereo {
                let y = &mut y_all.as_deref_mut().unwrap()[off..off + n];
                let has = take_low(&norm, &mut lowband);
                x_cm = ctx.quant_band(
                    x,
                    None,
                    n,
                    b / 2,
                    bb,
                    if has { Some(&mut lowband[..]) } else { None },
                    lm as i32,
                    Some(&mut norm[off..off + n]),
                    0,
                    1.0,
                    x_cm,
                );
                let has = take_low(&norm2, &mut lowband2);
                y_cm = ctx.quant_band(
                    y,
                    None,
                    n,
                    b / 2,
                    bb,
                    if has { Some(&mut lowband2[..]) } else { None },
                    lm as i32,
                    Some(&mut norm2[off..off + n]),
                    0,
                    1.0,
                    y_cm,
                );
            } else {
                let y = y_all.as_deref_mut().map(|y| &mut y[off..off + n]);
                let has = take_low(&norm, &mut lowband);
                x_cm = ctx.quant_band(
                    x,
                    y,
                    n,
                    b,
                    bb,
                    if has { Some(&mut lowband[..]) } else { None },
                    lm as i32,
                    Some(&mut norm[off..off + n]),
                    0,
                    1.0,
                    x_cm | y_cm,
                );
                y_cm = x_cm;
            }
            collapse[i * c] = x_cm as u8;
            collapse[i * c + c - 1] = y_cm as u8;
            balance += alloc.pulses[i] + tell;
            update_lowband = b > (n as i32) << BITRES;
        }
        self.rng = ctx.seed;
    }

    fn anti_collapse(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        collapse: &[u8; 2 * N_BANDS],
        lm: usize,
        c: usize,
        pulses: &[i32; N_BANDS],
    ) {
        let mut seed = self.rng;
        for i in self.start..self.end {
            let n0 = (EBANDS[i + 1] - EBANDS[i]) as usize;
            let depth = (1 + pulses[i]) / ((n0 as i32) << lm);
            let thresh = 0.5 * exp2(-0.125 * depth as f32);
            let sqrt_1 = 1.0 / libm::sqrtf((n0 << lm) as f32);
            for ch in 0..c {
                let mut prev1 = self.old_log_e[ch * N_BANDS + i];
                let mut prev2 = self.old_log_e2[ch * N_BANDS + i];
                if c == 1 {
                    prev1 = prev1.max(self.old_log_e[N_BANDS + i]);
                    prev2 = prev2.max(self.old_log_e2[N_BANDS + i]);
                }
                let ediff = (self.old_band_e[ch * N_BANDS + i] - prev1.min(prev2)).max(0.0);
                let mut r = 2.0 * exp2(-ediff);
                if lm == 3 {
                    r *= core::f32::consts::SQRT_2;
                }
                r = thresh.min(r) * sqrt_1;
                let band = &mut (if ch == 0 { &mut *x } else { &mut *y })[(EBANDS[i] as usize) << lm..][..n0 << lm];
                let mut renorm = false;
                for k in 0..1 << lm {
                    if collapse[i * c + ch] & (1 << k) == 0 {
                        for j in 0..n0 {
                            seed = lcg(seed);
                            band[(j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                        }
                        renorm = true;
                    }
                }
                if renorm {
                    renormalise(band, 1.0);
                }
            }
        }
        self.rng = seed;
    }

    /// Packet loss concealment (celt_decode_lost), used for mode transitions.
    fn decode_lost(&mut self, pcm: &mut [f32], n: usize, lm: usize) {
        let cc = self.channels;
        let out = DECODE_BUFFER - MAX_PERIOD;
        let len = n + OVERLAP;
        if self.loss_count >= 5 || self.start != 0 {
            let mut band_e = [0.0f32; 2 * N_BANDS];
            if self.loss_count >= 5 {
                let bg = self.background_log_e;
                self.log2_amp(&mut band_e, &bg, cc);
            } else {
                let decay = if self.loss_count == 0 { 1.5 } else { 0.5 };
                for ch in 0..cc {
                    for i in self.start..self.end {
                        self.old_band_e[ch * N_BANDS + i] -= decay;
                    }
                }
                let old = self.old_band_e;
                self.log2_amp(&mut band_e, &old, cc);
            }
            let mut seed = self.rng;
            let mut freq = vec![0.0f32; cc * n];
            for ch in 0..cc {
                let mut xn = vec![0.0f32; n];
                for i in self.start..N_BANDS {
                    let o = (EBANDS[i] as usize) << lm;
                    let blen = ((EBANDS[i + 1] - EBANDS[i]) as usize) << lm;
                    for v in &mut xn[o..o + blen] {
                        seed = lcg(seed);
                        *v = ((seed as i32) >> 20) as f32;
                    }
                    renormalise(&mut xn[o..o + blen], 1.0);
                }
                let f = &mut freq[ch * n..(ch + 1) * n];
                for i in 0..N_BANDS {
                    let g = band_e[ch * N_BANDS + i];
                    for j in (EBANDS[i] as usize) << lm..(EBANDS[i + 1] as usize) << lm {
                        f[j] = xn[j] * g;
                    }
                }
                f[..(EBANDS[self.start] as usize) << lm].fill(0.0);
                f[(EBANDS[self.end.min(N_BANDS)] as usize) << lm..].fill(0.0);
            }
            self.rng = seed;
            for mem in self.decode_mem.iter_mut().take(cc) {
                mem.copy_within(n..DECODE_BUFFER, 0);
            }
            self.inverse_mdcts(&freq, 0, n, lm);
        } else {
            let mut fade = 1.0f32;
            let pitch_index;
            if self.loss_count == 0 {
                let mut pitch_buf = vec![0.0f32; DECODE_BUFFER >> 1];
                self.pitch_downsample(&mut pitch_buf, cc);
                let poffset = 720;
                let pi = pitch_search(&pitch_buf[poffset >> 1..], &pitch_buf, DECODE_BUFFER - poffset, poffset - 100);
                pitch_index = poffset as i32 - pi;
                self.last_pitch_index = pitch_index;
            } else {
                pitch_index = self.last_pitch_index;
                fade = 0.8;
            }
            let pitch = pitch_index as usize;
            for ch in 0..cc {
                let mem = &mut self.decode_mem[ch];
                let mut exc = [0.0f32; MAX_PERIOD];
                exc.copy_from_slice(&mem[out..out + MAX_PERIOD]);
                if self.loss_count == 0 {
                    let mut ac = [0.0f32; LPC_ORDER + 1];
                    autocorr(&exc, &mut ac, Some(&WINDOW[..]), OVERLAP, LPC_ORDER, MAX_PERIOD);
                    ac[0] *= 1.0001;
                    for (i, a) in ac.iter_mut().enumerate().skip(1) {
                        *a -= *a * (0.008 * i as f32) * (0.008 * i as f32);
                    }
                    lpc(&mut self.lpc[ch], &ac, LPC_ORDER);
                }
                let mut fmem = [0.0f32; LPC_ORDER];
                for i in 0..LPC_ORDER {
                    fmem[i] = mem[out + MAX_PERIOD - 1 - i];
                }
                let src = exc;
                fir(&src, &self.lpc[ch], &mut exc, MAX_PERIOD, LPC_ORDER, &mut fmem);
                let period = if pitch <= MAX_PERIOD / 2 { pitch } else { MAX_PERIOD / 2 };
                let (mut e1, mut e2) = (1.0f32, 1.0f32);
                for i in 0..period {
                    let a = exc[MAX_PERIOD - period + i];
                    let b = exc[MAX_PERIOD - 2 * period + i];
                    e1 += a * a;
                    e2 += b * b;
                }
                if e1 > e2 {
                    e1 = e2;
                }
                let mut decay = libm::sqrtf(e1 / e2);
                let mut e = vec![0.0f32; MAX_PERIOD + 2 * OVERLAP];
                let mut offset = MAX_PERIOD as isize - pitch as isize;
                let mut s1 = 0.0f32;
                for i in 0..len + OVERLAP {
                    if offset + i as isize >= MAX_PERIOD as isize {
                        offset -= pitch as isize;
                        decay *= decay;
                    }
                    let k = (offset + i as isize) as usize;
                    e[i] = decay * exc[k];
                    let t = mem[out + k];
                    s1 += t * t;
                }
                for i in 0..LPC_ORDER {
                    fmem[i] = mem[out + MAX_PERIOD - 1 - i];
                }
                for v in e.iter_mut().take(len + OVERLAP) {
                    *v *= fade;
                }
                iir(&mut e[..len + OVERLAP], &self.lpc[ch], LPC_ORDER, &mut fmem);
                let s2: f32 = e[..len + OVERLAP].iter().map(|v| v * v).sum();
                // Written this way to catch NaN too.
                #[allow(clippy::neg_cmp_op_on_partial_ord)]
                if !(s1 > 0.2 * s2) {
                    e[..len + OVERLAP].fill(0.0);
                } else if s1 < s2 {
                    let ratio = libm::sqrtf((s1 + 1.0) / (s2 + 1.0));
                    e[..len + OVERLAP].iter_mut().for_each(|v| *v *= ratio);
                }
                // Post-filter the MDCT overlap of the previous frame.
                comb_filter(
                    mem,
                    out + MAX_PERIOD,
                    self.pf_period,
                    self.pf_period,
                    OVERLAP,
                    self.pf_gain,
                    self.pf_gain,
                    self.pf_tapset,
                    self.pf_tapset,
                    0,
                );
                mem.copy_within(out + n..out + MAX_PERIOD + OVERLAP, out);
                for i in 0..OVERLAP / 2 {
                    let tmp = WINDOW[i] * e[n + OVERLAP - 1 - i] + WINDOW[OVERLAP - i - 1] * e[n + i];
                    mem[out + MAX_PERIOD + i] = WINDOW[OVERLAP - i - 1] * tmp;
                    mem[out + MAX_PERIOD + OVERLAP - i - 1] = WINDOW[i] * tmp;
                }
                mem[out + MAX_PERIOD - n..out + MAX_PERIOD].copy_from_slice(&e[..n]);
                // Pre-filter the overlap for the next frame (the post-filter
                // runs then): e = x - comb(x), with x the new overlap.
                let mut tmp = vec![0.0f32; MAX_PERIOD + OVERLAP];
                tmp.copy_from_slice(&mem[out..out + MAX_PERIOD + OVERLAP]);
                comb_filter_to(&mut e[..OVERLAP], &tmp, MAX_PERIOD, self.pf_period, -self.pf_gain, self.pf_tapset);
                mem[out + MAX_PERIOD..out + MAX_PERIOD + OVERLAP].copy_from_slice(&e[..OVERLAP]);
            }
        }
        self.deemphasis(pcm, n);
        self.loss_count += 1;
    }

    fn pitch_downsample(&self, x_lp: &mut [f32], c: usize) {
        let len = DECODE_BUFFER;
        for ch in 0..c {
            let x = &self.decode_mem[ch];
            for i in 1..len >> 1 {
                let v = 0.5 * (0.5 * (x[2 * i - 1] + x[2 * i + 1]) + x[2 * i]);
                if ch == 0 {
                    x_lp[i] = v;
                } else {
                    x_lp[i] += v;
                }
            }
            let v = 0.5 * (0.5 * x[1] + x[0]);
            if ch == 0 {
                x_lp[0] = v;
            } else {
                x_lp[0] += v;
            }
        }
        let mut ac = [0.0f32; 5];
        autocorr(x_lp, &mut ac, None, 0, 4, len >> 1);
        ac[0] *= 1.0001;
        for (i, a) in ac.iter_mut().enumerate().skip(1) {
            *a -= *a * (0.008 * i as f32) * (0.008 * i as f32);
        }
        let mut l = [0.0f32; 4];
        lpc(&mut l, &ac, 4);
        let mut tmp = 1.0f32;
        for v in l.iter_mut() {
            tmp *= 0.9;
            *v *= tmp;
        }
        let mut mem = [0.0f32; 4];
        let src = x_lp[..len >> 1].to_vec();
        fir(&src, &l, x_lp, len >> 1, 4, &mut mem);
        let mut mem = [0.0f32; 1];
        let src = x_lp[..len >> 1].to_vec();
        fir(&src, &[0.8f32], x_lp, len >> 1, 1, &mut mem);
    }
}

/// The comb filter with separate input and output (only the gain-1 part,
/// the period constant): y[i] += g * taps around x[at + i - t].
fn comb_filter_to(y: &mut [f32], x: &[f32], at: usize, t: i32, g: f32, tap: usize) {
    let t = t as usize;
    let g0 = g * COMB_GAINS[tap][0];
    let g1 = g * COMB_GAINS[tap][1];
    let g2 = g * COMB_GAINS[tap][2];
    for (i, v) in y.iter_mut().enumerate() {
        let xi = |k: usize| x[at + i - k];
        *v = xi(0) + g0 * xi(t) + g1 * (xi(t + 1) + xi(t - 1)) + g2 * (xi(t + 2) + xi(t - 2));
    }
}

fn autocorr(x: &[f32], ac: &mut [f32], window: Option<&[f32]>, overlap: usize, lag: usize, n: usize) {
    let mut xx = x[..n].to_vec();
    if let Some(w) = window {
        for i in 0..overlap {
            xx[i] = x[i] * w[i];
            xx[n - i - 1] = x[n - i - 1] * w[i];
        }
    }
    for l in (0..=lag).rev() {
        let mut d = 0.0f32;
        for i in l..n {
            d += xx[i] * xx[i - l];
        }
        ac[l] = d;
    }
    ac[0] += 10.0;
}

fn lpc(out: &mut [f32], ac: &[f32], p: usize) {
    let mut error = ac[0];
    out[..p].fill(0.0);
    if ac[0] == 0.0 {
        return;
    }
    for i in 0..p {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += out[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -rr / error;
        out[i] = r;
        for j in 0..(i + 1) >> 1 {
            let (t1, t2) = (out[j], out[i - 1 - j]);
            out[j] = t1 + r * t2;
            out[i - 1 - j] = t2 + r * t1;
        }
        error -= r * r * error;
        if error < 0.001 * ac[0] {
            break;
        }
    }
}

fn fir(x: &[f32], num: &[f32], y: &mut [f32], n: usize, ord: usize, mem: &mut [f32]) {
    for i in 0..n {
        let mut sum = x[i];
        for j in 0..ord {
            sum += num[j] * mem[j];
        }
        for j in (1..ord).rev() {
            mem[j] = mem[j - 1];
        }
        mem[0] = x[i];
        y[i] = sum;
    }
}

fn iir(x: &mut [f32], den: &[f32], ord: usize, mem: &mut [f32]) {
    for v in x.iter_mut() {
        let mut sum = *v;
        for j in 0..ord {
            sum -= den[j] * mem[j];
        }
        for j in (1..ord).rev() {
            mem[j] = mem[j - 1];
        }
        mem[0] = sum;
        *v = sum;
    }
}

fn find_best_pitch(xcorr: &[f32], y: &[f32], len: usize, max_pitch: usize, best: &mut [usize; 2]) {
    let mut syy = 1.0f32;
    let mut best_num = [-1.0f32; 2];
    let mut best_den = [0.0f32; 2];
    *best = [0, 1];
    for v in &y[..len] {
        syy += v * v;
    }
    for i in 0..max_pitch {
        if xcorr[i] > 0.0 {
            let num = xcorr[i] * xcorr[i];
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best[1] = best[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best[0] = i;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best[1] = i;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        syy = syy.max(1.0);
    }
}

fn pitch_search(x_lp: &[f32], y: &[f32], len: usize, max_pitch: usize) -> i32 {
    let lag = len + max_pitch;
    let x4: Vec<f32> = (0..len >> 2).map(|j| x_lp[2 * j]).collect();
    let y4: Vec<f32> = (0..lag >> 2).map(|j| y[2 * j]).collect();
    let mut xcorr = vec![0.0f32; max_pitch >> 1];
    for i in 0..max_pitch >> 2 {
        let s: f32 = (0..len >> 2).map(|j| x4[j] * y4[i + j]).sum();
        xcorr[i] = s.max(-1.0);
    }
    let mut best = [0usize; 2];
    find_best_pitch(&xcorr, &y4, len >> 2, max_pitch >> 2, &mut best);
    for i in 0..max_pitch >> 1 {
        xcorr[i] = 0.0;
        if (i as i32 - 2 * best[0] as i32).abs() > 2 && (i as i32 - 2 * best[1] as i32).abs() > 2 {
            continue;
        }
        let s: f32 = (0..len >> 1).map(|j| x_lp[j] * y[i + j]).sum();
        xcorr[i] = s.max(-1.0);
    }
    find_best_pitch(&xcorr, y, len >> 1, max_pitch >> 1, &mut best);
    let mut offset = 0;
    if best[0] > 0 && best[0] < (max_pitch >> 1) - 1 {
        let (a, b, c) = (xcorr[best[0] - 1], xcorr[best[0]], xcorr[best[0] + 1]);
        if c - a > 0.7 * (b - a) {
            offset = 1;
        } else if a - c > 0.7 * (b - c) {
            offset = -1;
        }
    }
    2 * best[0] as i32 - offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pvq_codewords_round_trip() {
        // V(n, k) counts every vector with sum |y| = k; decoding each index
        // must give distinct vectors with the right norm.
        for (n, k) in [(2usize, 3usize), (3, 2), (4, 3), (5, 4), (8, 2)] {
            let mut u = vec![0u32; k + 2];
            let v = ncwrs_urow(n, k, &mut u);
            let mut seen = alloc::collections::BTreeSet::new();
            for i in 0..v {
                let mut u = vec![0u32; k + 2];
                ncwrs_urow(n, k, &mut u);
                let mut y = vec![0i32; n];
                cwrsi(n, k, i, &mut y, &mut u);
                assert_eq!(y.iter().map(|v| v.unsigned_abs() as usize).sum::<usize>(), k, "{n} {k} {i}");
                assert!(seen.insert(y));
            }
        }
    }

    #[test]
    fn isqrt() {
        for v in [1u32, 2, 3, 4, 15, 16, 17, 1000, 65535, 1 << 30, u32::MAX] {
            let r = isqrt32(v) as u64;
            assert!(r * r <= v as u64 && (r + 1) * (r + 1) > v as u64, "{v}");
        }
    }
}
