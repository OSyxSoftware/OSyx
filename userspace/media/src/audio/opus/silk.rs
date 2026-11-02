//! The SILK layer of Opus (RFC 6716 section 4.2, with the RFC 8251
//! corrections): a port of the reference decoder's fixed-point code, so the
//! output is bit-exact. Indices, excitation pulses with the shell coder,
//! NLSF to LPC, pitch and LTP, the synthesis filters, stereo unmixing, the
//! upsampler to 48 kHz, and the concealment and comfort noise used around
//! lost frames and mode switches.
//!
//! Integer expressions keep the reference's shape and its 32-bit
//! wrap-around.

#![allow(clippy::precedence)]

use super::range::RangeDecoder;
use super::silk_tables::*;

const MAX_LPC_ORDER: usize = 16;
const LTP_ORDER: usize = 5;
const MAX_NB_SUBFR: usize = 4;
const MAX_FRAME_LENGTH: usize = 320;
const MAX_SUB_FRAME_LENGTH: usize = 80;
const LTP_MEM_LENGTH_MS: usize = 20;
const SUB_FRAME_LENGTH_MS: usize = 5;
const SHELL_CODEC_FRAME_LENGTH: usize = 16;
const MAX_PULSES: usize = 16;
const N_RATE_LEVELS: usize = 10;
const NLSF_QUANT_MAX_AMPLITUDE: i32 = 4;
const N_LEVELS_QGAIN: i32 = 64;
const MAX_DELTA_GAIN_QUANT: i32 = 36;
const MIN_DELTA_GAIN_QUANT: i32 = -4;
const QUANT_LEVEL_ADJUST_Q10: i32 = 80;
const MAX_LPC_STABILIZE_ITERATIONS: usize = 16;
const BWE_AFTER_LOSS_Q16: i32 = 63570;
const STEREO_INTERP_LEN_MS: usize = 8;
const PE_MIN_LAG_MS: i32 = 2;
const PE_MAX_LAG_MS: i32 = 18;
const CNG_BUF_MASK_MAX: i32 = 255;
const CNG_GAIN_SMTH_Q16: i32 = 4634;
const CNG_NLSF_SMTH_Q16: i32 = 16348;
const TYPE_NO_VOICE_ACTIVITY: i32 = 0;
const TYPE_VOICED: i32 = 2;
const CODE_INDEPENDENTLY: i32 = 0;
const CODE_INDEPENDENTLY_NO_LTP_SCALING: i32 = 1;
const CODE_CONDITIONALLY: i32 = 2;

// PLC constants (PLC.h).
const BWE_COEF_Q16: i32 = 64881; // SILK_FIX_CONST(0.99, 16)
const V_PITCH_GAIN_START_MIN_Q14: i32 = 11469;
const V_PITCH_GAIN_START_MAX_Q14: i32 = 15565;
const MAX_PITCH_LAG_MS: i32 = 18;
const RAND_BUF_SIZE: usize = 128;
const RAND_BUF_MASK: i32 = RAND_BUF_SIZE as i32 - 1;
const LOG2_INV_LPC_GAIN_HIGH_THRES: i32 = 3;
const LOG2_INV_LPC_GAIN_LOW_THRES: i32 = 8;
const PITCH_DRIFT_FAC_Q16: i32 = 655;
const HARM_ATT_Q15: [i32; 2] = [32440, 31130];
const PLC_RAND_ATTENUATE_V_Q15: [i32; 2] = [31130, 26214];
const PLC_RAND_ATTENUATE_UV_Q15: [i32; 2] = [32440, 29491];

// ---------------------------------------------------------------------------
// Fixed-point helpers (SigProc_FIX.h, macros.h, Inlines.h).

#[inline]
fn smulwb(a: i32, b: i32) -> i32 {
    let b = b as i16 as i32;
    (a >> 16).wrapping_mul(b).wrapping_add(((a & 0xFFFF).wrapping_mul(b)) >> 16)
}

#[inline]
fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}

#[inline]
fn smulbb(a: i32, b: i32) -> i32 {
    (a as i16 as i32) * (b as i16 as i32)
}

#[inline]
fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulbb(b, c))
}

#[inline]
fn rshift_round(a: i32, s: u32) -> i32 {
    if s == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (s - 1)) + 1) >> 1
    }
}

#[inline]
fn rshift_round64(a: i64, s: u32) -> i64 {
    if s == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (s - 1)) + 1) >> 1
    }
}

#[inline]
fn smulww(a: i32, b: i32) -> i32 {
    smulwb(a, b).wrapping_add(a.wrapping_mul(rshift_round(b, 16)))
}

#[inline]
fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    smlawb(a, b, c).wrapping_add(b.wrapping_mul(rshift_round(c, 16)))
}

#[inline]
fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}

#[inline]
fn sat16(a: i32) -> i32 {
    a.clamp(i16::MIN as i32, i16::MAX as i32)
}

fn lshift_sat32(a: i32, shift: u32) -> i32 {
    a.clamp(i32::MIN >> shift, i32::MAX >> shift) << shift
}

#[inline]
fn silk_rand(seed: i32) -> i32 {
    907_633_515i32.wrapping_add(seed.wrapping_mul(196_314_165))
}

#[inline]
fn clz32(a: i32) -> i32 {
    (a as u32).leading_zeros() as i32
}

fn clz_frac(a: i32) -> (i32, i32) {
    let lz = clz32(a);
    let rot = ((24 - lz) as u32) & 31;
    (lz, ((a as u32).rotate_right(rot) & 0x7f) as i32)
}

fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let (lz, frac) = clz_frac(x);
    let mut y = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, smulbb(213, frac))
}

fn div32_varq(a32: i32, b32: i32, qres: i32) -> i32 {
    let a_headrm = clz32(a32.wrapping_abs()) - 1;
    let a_nrm = a32.wrapping_shl(a_headrm as u32);
    let b_headrm = clz32(b32.wrapping_abs()) - 1;
    let b_nrm = b32.wrapping_shl(b_headrm as u32);
    let b_inv = (i32::MAX >> 2) / (b_nrm >> 16);
    let mut result = smulwb(a_nrm, b_inv);
    let a_nrm = a_nrm.wrapping_sub(smmul(b_nrm, result).wrapping_shl(3));
    result = smlawb(result, a_nrm, b_inv);
    let lshift = 29 + a_headrm - b_headrm - qres;
    if lshift < 0 {
        lshift_sat32(result, (-lshift) as u32)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

fn inverse32_varq(b32: i32, qres: i32) -> i32 {
    let b_headrm = clz32(b32.wrapping_abs()) - 1;
    let b_nrm = b32.wrapping_shl(b_headrm as u32);
    let b_inv = (i32::MAX >> 2) / (b_nrm >> 16);
    let mut result = b_inv << 16;
    let err_q32 = ((1 << 29) - smulwb(b_nrm, b_inv)).wrapping_shl(3);
    result = smlaww(result, err_q32, b_inv);
    let lshift = 61 - b_headrm - qres;
    if lshift <= 0 {
        lshift_sat32(result, (-lshift) as u32)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    let out = 1i32 << (in_log_q7 >> 7);
    let frac = in_log_q7 & 0x7F;
    let p = smlawb(frac, smulbb(frac, 128 - frac), -174);
    if in_log_q7 < 2048 {
        out + ((out * p) >> 7)
    } else {
        out.wrapping_add((out >> 7).wrapping_mul(p))
    }
}

fn sum_sqr_shift(x: &[i16]) -> (i32, i32) {
    let len = x.len() as isize - 1;
    let sq = |i: isize| smulbb(x[i as usize] as i32, x[i as usize] as i32);
    let mut nrg: i32 = 0;
    let mut shft: u32 = 0;
    let mut i: isize = 0;
    while i < len {
        nrg = nrg.wrapping_add(sq(i));
        nrg = nrg.wrapping_add(sq(i + 1));
        i += 2;
        if nrg < 0 {
            nrg = ((nrg as u32) >> 2) as i32;
            shft = 2;
            break;
        }
    }
    while i < len {
        let t = sq(i).wrapping_add(sq(i + 1));
        nrg = (nrg as u32).wrapping_add((t as u32) >> shft) as i32;
        if nrg < 0 {
            nrg = ((nrg as u32) >> 2) as i32;
            shft += 2;
        }
        i += 2;
    }
    if i == len {
        nrg = (nrg as u32).wrapping_add((sq(i) as u32) >> shft) as i32;
    }
    if nrg as u32 & 0xC000_0000 != 0 {
        nrg = ((nrg as u32) >> 2) as i32;
        shft += 2;
    }
    (nrg, shft as i32)
}

fn bwexpander(ar: &mut [i16], d: usize, mut chirp_q16: i32) {
    let chirp_minus_one = chirp_q16 - 65536;
    for v in ar.iter_mut().take(d - 1) {
        *v = rshift_round(chirp_q16.wrapping_mul(*v as i32), 16) as i16;
        chirp_q16 += rshift_round(chirp_q16.wrapping_mul(chirp_minus_one), 16);
    }
    ar[d - 1] = rshift_round(chirp_q16.wrapping_mul(ar[d - 1] as i32), 16) as i16;
}

fn bwexpander_32(ar: &mut [i32], d: usize, mut chirp_q16: i32) {
    let chirp_minus_one = chirp_q16 - 65536;
    for v in ar.iter_mut().take(d - 1) {
        *v = smulww(chirp_q16, *v);
        chirp_q16 += rshift_round(chirp_q16.wrapping_mul(chirp_minus_one), 16);
    }
    ar[d - 1] = smulww(chirp_q16, ar[d - 1]);
}

fn lpc_analysis_filter(out: &mut [i16], input: &[i16], b: &[i16], len: usize, d: usize) {
    for ix in d..len {
        let mut acc = smulbb(input[ix - 1] as i32, b[0] as i32);
        for j in 1..d {
            acc = acc.wrapping_add(smulbb(input[ix - 1 - j] as i32, b[j] as i32));
        }
        acc = ((input[ix] as i32) << 12).wrapping_sub(acc);
        out[ix] = sat16(rshift_round(acc, 12)) as i16;
    }
    out[..d].fill(0);
}

// ---------------------------------------------------------------------------
// LPC stability and NLSF to LPC (LPC_inv_pred_gain.c, NLSF2A.c).

const QA_INV: u32 = 24;
const A_LIMIT: i32 = 16_773_022; // SILK_FIX_CONST(0.99975, 24)

fn mul32_frac_q(a: i32, b: i32, q: u32) -> i32 {
    rshift_round64(a as i64 * b as i64, q) as i32
}

fn inverse_pred_gain_qa(a: &mut [[i32; MAX_LPC_ORDER]; 2], order: usize) -> i32 {
    let mut new = order & 1;
    let mut inv_gain_q30: i32 = 1 << 30;
    for k in (1..order).rev() {
        if a[new][k] > A_LIMIT || a[new][k] < -A_LIMIT {
            return 0;
        }
        let rc_q31 = -(a[new][k] << (31 - QA_INV));
        let rc_mult1_q30 = (1i32 << 30) - smmul(rc_q31, rc_q31);
        let mult2q = 32 - clz32(rc_mult1_q30.wrapping_abs());
        let rc_mult2 = inverse32_varq(rc_mult1_q30, mult2q + 30);
        inv_gain_q30 = smmul(inv_gain_q30, rc_mult1_q30) << 2;
        let old = new;
        new = k & 1;
        for n in 0..k {
            let tmp = a[old][n].saturating_sub(mul32_frac_q(a[old][k - n - 1], rc_q31, 31));
            let t64 = rshift_round64(tmp as i64 * rc_mult2 as i64, mult2q as u32);
            if t64 > i32::MAX as i64 || t64 < i32::MIN as i64 {
                return 0;
            }
            a[new][n] = t64 as i32;
        }
    }
    if a[new][0] > A_LIMIT || a[new][0] < -A_LIMIT {
        return 0;
    }
    let rc_q31 = -(a[new][0] << (31 - QA_INV));
    let rc_mult1_q30 = (1i32 << 30) - smmul(rc_q31, rc_q31);
    smmul(inv_gain_q30, rc_mult1_q30) << 2
}

fn lpc_inverse_pred_gain(a_q12: &[i16], order: usize) -> i32 {
    let mut a = [[0i32; MAX_LPC_ORDER]; 2];
    let mut dc = 0i32;
    for k in 0..order {
        dc += a_q12[k] as i32;
        a[order & 1][k] = (a_q12[k] as i32) << (QA_INV - 12);
    }
    if dc >= 4096 {
        return 0;
    }
    inverse_pred_gain_qa(&mut a, order)
}

const QA: u32 = 16;

fn nlsf2a_find_poly(out: &mut [i32], c: &[i32], dd: usize) {
    out[0] = 1 << QA;
    out[1] = -c[0];
    for k in 1..dd {
        let ftmp = c[2 * k];
        out[k + 1] = (out[k - 1] << 1) - rshift_round64(ftmp as i64 * out[k] as i64, QA) as i32;
        for n in (2..=k).rev() {
            out[n] += out[n - 2] - rshift_round64(ftmp as i64 * out[n - 1] as i64, QA) as i32;
        }
        out[1] -= ftmp;
    }
}

fn nlsf2a(a_q12: &mut [i16], nlsf: &[i16], d: usize) {
    const ORDERING16: [usize; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    const ORDERING10: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let ordering: &[usize] = if d == 16 { &ORDERING16 } else { &ORDERING10 };
    let mut cos_lsf = [0i32; MAX_LPC_ORDER];
    for k in 0..d {
        let f_int = (nlsf[k] as i32) >> (15 - 7);
        let f_frac = nlsf[k] as i32 - (f_int << (15 - 7));
        let cos_val = LSF_COS_TAB_Q12[f_int as usize] as i32;
        let delta = LSF_COS_TAB_Q12[f_int as usize + 1] as i32 - cos_val;
        cos_lsf[ordering[k]] = rshift_round((cos_val << 8) + delta * f_frac, 20 - QA);
    }
    let dd = d >> 1;
    let mut p = [0i32; MAX_LPC_ORDER / 2 + 1];
    let mut q = [0i32; MAX_LPC_ORDER / 2 + 1];
    nlsf2a_find_poly(&mut p, &cos_lsf[0..], dd);
    nlsf2a_find_poly(&mut q, &cos_lsf[1..], dd);
    let mut a32 = [0i32; MAX_LPC_ORDER];
    for k in 0..dd {
        let ptmp = p[k + 1] + p[k];
        let qtmp = q[k + 1] - q[k];
        a32[k] = -qtmp - ptmp;
        a32[d - k - 1] = qtmp - ptmp;
    }
    let mut i = 0;
    while i < 10 {
        let mut maxabs = 0i32;
        let mut idx = 0;
        for (k, &v) in a32.iter().enumerate().take(d) {
            let a = v.wrapping_abs();
            if a > maxabs {
                maxabs = a;
                idx = k;
            }
        }
        maxabs = rshift_round(maxabs, QA + 1 - 12);
        if maxabs > i16::MAX as i32 {
            maxabs = maxabs.min(163838);
            let sc_q16 = 65470 - (((maxabs - i16::MAX as i32) << 14) / ((maxabs * (idx as i32 + 1)) >> 2));
            bwexpander_32(&mut a32, d, sc_q16);
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..d {
            a_q12[k] = sat16(rshift_round(a32[k], QA + 1 - 12)) as i16;
            a32[k] = (a_q12[k] as i32) << (QA + 1 - 12);
        }
    } else {
        for k in 0..d {
            a_q12[k] = rshift_round(a32[k], QA + 1 - 12) as i16;
        }
    }
    for i in 0..MAX_LPC_STABILIZE_ITERATIONS {
        // SILK_FIX_CONST(1.0 / MAX_PREDICTION_POWER_GAIN, 30)
        if lpc_inverse_pred_gain(a_q12, d) < 107374 {
            bwexpander_32(&mut a32, d, 65536 - (2 << i));
            for k in 0..d {
                a_q12[k] = rshift_round(a32[k], QA + 1 - 12) as i16;
            }
        } else {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// NLSF codebooks (NLSF_decode.c, NLSF_unpack.c, NLSF_stabilize.c).

struct NlsfCb {
    n_vectors: usize,
    order: usize,
    quant_step_q16: i32,
    cb1_q8: &'static [u8],
    cb1_icdf: &'static [u8],
    pred_q8: &'static [u8],
    ec_sel: &'static [u8],
    ec_icdf: &'static [u8],
    delta_min_q15: &'static [i16],
}

static NLSF_CB_NB_MB: NlsfCb = NlsfCb {
    n_vectors: 32,
    order: 10,
    quant_step_q16: 11796, // SILK_FIX_CONST(0.18, 16)
    cb1_q8: &NLSF_CB1_NB_MB_Q8,
    cb1_icdf: &NLSF_CB1_ICDF_NB_MB,
    pred_q8: &NLSF_PRED_NB_MB_Q8,
    ec_sel: &NLSF_CB2_SELECT_NB_MB,
    ec_icdf: &NLSF_CB2_ICDF_NB_MB,
    delta_min_q15: &NLSF_DELTA_MIN_NB_MB_Q15,
};

static NLSF_CB_WB: NlsfCb = NlsfCb {
    n_vectors: 32,
    order: 16,
    quant_step_q16: 9830, // SILK_FIX_CONST(0.15, 16)
    cb1_q8: &NLSF_CB1_WB_Q8,
    cb1_icdf: &NLSF_CB1_ICDF_WB,
    pred_q8: &NLSF_PRED_WB_Q8,
    ec_sel: &NLSF_CB2_SELECT_WB,
    ec_icdf: &NLSF_CB2_ICDF_WB,
    delta_min_q15: &NLSF_DELTA_MIN_WB_Q15,
};

fn nlsf_unpack(ec_ix: &mut [i16], pred_q8: &mut [u8], cb: &NlsfCb, cb1_index: usize) {
    let sel = &cb.ec_sel[cb1_index * cb.order / 2..];
    for i in (0..cb.order).step_by(2) {
        let entry = sel[i / 2];
        ec_ix[i] = (((entry >> 1) & 7) as i16) * (2 * NLSF_QUANT_MAX_AMPLITUDE as i16 + 1);
        pred_q8[i] = cb.pred_q8[i + (entry & 1) as usize * (cb.order - 1)];
        ec_ix[i + 1] = (((entry >> 5) & 7) as i16) * (2 * NLSF_QUANT_MAX_AMPLITUDE as i16 + 1);
        pred_q8[i + 1] = cb.pred_q8[i + ((entry >> 4) & 1) as usize * (cb.order - 1) + 1];
    }
}

fn nlsf_stabilize(nlsf: &mut [i16], delta_min: &[i16], l: usize) {
    const MAX_LOOPS: usize = 20;
    for _ in 0..MAX_LOOPS {
        let mut min_diff = nlsf[0] as i32 - delta_min[0] as i32;
        let mut im = 0;
        for i in 1..l {
            let diff = nlsf[i] as i32 - (nlsf[i - 1] as i32 + delta_min[i] as i32);
            if diff < min_diff {
                min_diff = diff;
                im = i;
            }
        }
        let diff = (1 << 15) - (nlsf[l - 1] as i32 + delta_min[l] as i32);
        if diff < min_diff {
            min_diff = diff;
            im = l;
        }
        if min_diff >= 0 {
            return;
        }
        if im == 0 {
            nlsf[0] = delta_min[0];
        } else if im == l {
            nlsf[l - 1] = ((1 << 15) - delta_min[l] as i32) as i16;
        } else {
            let mut min_center: i32 = delta_min[..im].iter().map(|&v| v as i32).sum();
            min_center += (delta_min[im] as i32) >> 1;
            let mut max_center: i32 = 1 << 15;
            for k in (im + 1..=l).rev() {
                max_center -= delta_min[k] as i32;
            }
            max_center -= (delta_min[im] as i32) >> 1;
            let c = rshift_round(nlsf[im - 1] as i32 + nlsf[im] as i32, 1);
            let c =
                if min_center > max_center { c.clamp(max_center, min_center) } else { c.clamp(min_center, max_center) };
            nlsf[im - 1] = (c - ((delta_min[im] as i32) >> 1)) as i16;
            nlsf[im] = (nlsf[im - 1] as i32 + delta_min[im] as i32) as i16;
        }
    }
    // Fall back: sort and push apart.
    for i in 1..l {
        let v = nlsf[i];
        let mut j = i;
        while j > 0 && v < nlsf[j - 1] {
            nlsf[j] = nlsf[j - 1];
            j -= 1;
        }
        nlsf[j] = v;
    }
    nlsf[0] = nlsf[0].max(delta_min[0]);
    for i in 1..l {
        // RFC 8251 section 7: a saturating add.
        let floor = (nlsf[i - 1] as i32 + delta_min[i] as i32).clamp(i16::MIN as i32, i16::MAX as i32);
        nlsf[i] = (nlsf[i] as i32).max(floor) as i16;
    }
    nlsf[l - 1] = (nlsf[l - 1] as i32).min((1 << 15) - delta_min[l] as i32) as i16;
    for i in (0..l - 1).rev() {
        nlsf[i] = (nlsf[i] as i32).min(nlsf[i + 1] as i32 - delta_min[i + 1] as i32) as i16;
    }
}

fn nlsf_decode(nlsf: &mut [i16], indices: &[i8], cb: &NlsfCb) {
    let order = cb.order;
    let cb1 = &cb.cb1_q8[indices[0] as usize * order..];
    for i in 0..order {
        nlsf[i] = ((cb1[i] as i32) << 7) as i16;
    }
    let mut ec_ix = [0i16; MAX_LPC_ORDER];
    let mut pred = [0u8; MAX_LPC_ORDER];
    nlsf_unpack(&mut ec_ix, &mut pred, cb, indices[0] as usize);
    // Predictive residual dequantizer.
    let mut res_q10 = [0i32; MAX_LPC_ORDER];
    let mut out_q10 = 0i32;
    for i in (0..order).rev() {
        let pred_q10 = smulbb(out_q10, pred[i] as i32) >> 8;
        out_q10 = (indices[i + 1] as i32) << 10;
        if out_q10 > 0 {
            out_q10 -= 102; // SILK_FIX_CONST(NLSF_QUANT_LEVEL_ADJ, 10)
        } else if out_q10 < 0 {
            out_q10 += 102;
        }
        out_q10 = smlawb(pred_q10, out_q10, cb.quant_step_q16);
        res_q10[i] = out_q10 as i16 as i32;
    }
    // Laroia weights of the codebook vector.
    let mut w = [0i32; MAX_LPC_ORDER];
    let d = order;
    let inv = |x: i32| (1 << (15 + 2)) / x.max(1);
    let mut t2 = inv(nlsf[1] as i32 - nlsf[0] as i32);
    w[0] = (inv(nlsf[0] as i32) + t2).min(i16::MAX as i32);
    let mut k = 1;
    while k < d - 1 {
        let t1 = inv(nlsf[k + 1] as i32 - nlsf[k] as i32);
        w[k] = (t1 + t2).min(i16::MAX as i32);
        t2 = inv(nlsf[k + 2] as i32 - nlsf[k + 1] as i32);
        w[k + 1] = (t1 + t2).min(i16::MAX as i32);
        k += 2;
    }
    w[d - 1] = (inv((1 << 15) - nlsf[d - 1] as i32) + t2).min(i16::MAX as i32);
    for i in 0..order {
        let w_q9 = sqrt_approx(w[i] << (18 - 2));
        let v = nlsf[i] as i32 + (res_q10[i] << 14) / w_q9;
        nlsf[i] = v.clamp(0, 32767) as i16;
    }
    nlsf_stabilize(nlsf, cb.delta_min_q15, order);
}

// ---------------------------------------------------------------------------
// The upsampler to 48 kHz (resampler.c, resampler_private_IIR_FIR.c,
// resampler_private_up2_HQ.c).

const UP2_HQ_0: [i32; 3] = [1746, 14986, 39083 - 65536];
const UP2_HQ_1: [i32; 3] = [6854, 25769, 55542 - 65536];
const FRAC_FIR_12: [[i32; 4]; 12] = [
    [189, -600, 617, 30567],
    [117, -159, -1070, 29704],
    [52, 221, -2392, 28276],
    [-4, 529, -3350, 26341],
    [-48, 758, -3956, 23973],
    [-80, 905, -4235, 21254],
    [-99, 972, -4222, 18278],
    [-107, 967, -3957, 15143],
    [-103, 896, -3487, 11950],
    [-91, 773, -2865, 8798],
    [-71, 611, -2143, 5784],
    [-46, 425, -1375, 2996],
];
const ORDER_FIR_12: usize = 8;

#[derive(Clone)]
struct Resampler {
    s_iir: [i32; 6],
    s_fir: [i16; ORDER_FIR_12],
    delay_buf: [i16; 48],
    batch_size: usize,
    inv_ratio_q16: i32,
    fs_in_khz: usize,
    fs_out_khz: usize,
    input_delay: usize,
}

impl Resampler {
    fn new() -> Resampler {
        Resampler {
            s_iir: [0; 6],
            s_fir: [0; ORDER_FIR_12],
            delay_buf: [0; 48],
            batch_size: 0,
            inv_ratio_q16: 0,
            fs_in_khz: 0,
            fs_out_khz: 0,
            input_delay: 0,
        }
    }

    /// From 8, 12 or 16 kHz to 48 kHz.
    fn init(fs_in: i32) -> Resampler {
        // delay_matrix_dec of the reference, column 48 kHz.
        const DELAY_TO_48: [usize; 3] = [0, 4, 7];
        let mut r = Resampler::new();
        let fs_out = 48000;
        r.input_delay = DELAY_TO_48[(fs_in / 4000 - 2) as usize];
        r.fs_in_khz = (fs_in / 1000) as usize;
        r.fs_out_khz = 48;
        r.batch_size = r.fs_in_khz * 10;
        let up2x = 1;
        r.inv_ratio_q16 = ((fs_in << (14 + up2x)) / fs_out) << 2;
        while smulww(r.inv_ratio_q16, fs_out) < fs_in << up2x {
            r.inv_ratio_q16 += 1;
        }
        r
    }

    fn up2_hq(s: &mut [i32; 6], out: &mut [i16], input: &[i16]) {
        for (k, &x) in input.iter().enumerate() {
            let in32 = (x as i32) << 10;
            let y = in32 - s[0];
            let xx = smulwb(y, UP2_HQ_0[0]);
            let o1 = s[0] + xx;
            s[0] = in32 + xx;
            let y = o1 - s[1];
            let xx = smulwb(y, UP2_HQ_0[1]);
            let o2 = s[1] + xx;
            s[1] = o1 + xx;
            let y = o2 - s[2];
            let xx = smlawb(y, y, UP2_HQ_0[2]);
            let o1 = s[2] + xx;
            s[2] = o2 + xx;
            out[2 * k] = sat16(rshift_round(o1, 10)) as i16;
            let y = in32 - s[3];
            let xx = smulwb(y, UP2_HQ_1[0]);
            let o1 = s[3] + xx;
            s[3] = in32 + xx;
            let y = o1 - s[4];
            let xx = smulwb(y, UP2_HQ_1[1]);
            let o2 = s[4] + xx;
            s[4] = o1 + xx;
            let y = o2 - s[5];
            let xx = smlawb(y, y, UP2_HQ_1[2]);
            let o1 = s[5] + xx;
            s[5] = o2 + xx;
            out[2 * k + 1] = sat16(rshift_round(o1, 10)) as i16;
        }
    }

    fn iir_fir(&mut self, out: &mut [i16], mut input: &[i16]) -> usize {
        let mut buf = [0i16; 2 * 160 + ORDER_FIR_12];
        buf[..ORDER_FIR_12].copy_from_slice(&self.s_fir);
        let inc = self.inv_ratio_q16;
        let mut o = 0;
        let mut n_in;
        loop {
            n_in = input.len().min(self.batch_size);
            Self::up2_hq(&mut self.s_iir, &mut buf[ORDER_FIR_12..ORDER_FIR_12 + 2 * n_in], &input[..n_in]);
            let max_index_q16 = (n_in as i32) << (16 + 1);
            let mut index_q16 = 0;
            while index_q16 < max_index_q16 {
                let t = smulwb(index_q16 & 0xFFFF, 12) as usize;
                let b = &buf[(index_q16 >> 16) as usize..];
                let f = &FRAC_FIR_12[t];
                let g = &FRAC_FIR_12[11 - t];
                let mut r = smulbb(b[0] as i32, f[0]);
                r = smlabb(r, b[1] as i32, f[1]);
                r = smlabb(r, b[2] as i32, f[2]);
                r = smlabb(r, b[3] as i32, f[3]);
                r = smlabb(r, b[4] as i32, g[3]);
                r = smlabb(r, b[5] as i32, g[2]);
                r = smlabb(r, b[6] as i32, g[1]);
                r = smlabb(r, b[7] as i32, g[0]);
                out[o] = sat16(rshift_round(r, 15)) as i16;
                o += 1;
                index_q16 += inc;
            }
            input = &input[n_in..];
            if !input.is_empty() {
                buf.copy_within(n_in << 1..(n_in << 1) + ORDER_FIR_12, 0);
            } else {
                break;
            }
        }
        self.s_fir.copy_from_slice(&buf[n_in << 1..(n_in << 1) + ORDER_FIR_12]);
        o
    }

    /// silk_resampler: `input.len()` samples in, `input.len() * 48 / fs_in`
    /// out.
    fn run(&mut self, out: &mut [i16], input: &[i16]) {
        let d = self.input_delay;
        let n = self.fs_in_khz - d;
        self.delay_buf[d..d + n].copy_from_slice(&input[..n]);
        let first = self.delay_buf;
        let fin = self.fs_in_khz;
        self.iir_fir(&mut out[..], &first[..fin]);
        let fo = self.fs_out_khz;
        let len = input.len();
        self.iir_fir(&mut out[fo..], &input[n..n + len - fin]);
        self.delay_buf[..d].copy_from_slice(&input[len - d..]);
    }
}

// ---------------------------------------------------------------------------
// Decoder state (structs.h).

#[derive(Clone, Copy, Default)]
struct Indices {
    gains: [i8; MAX_NB_SUBFR],
    ltp_index: [i8; MAX_NB_SUBFR],
    nlsf: [i8; MAX_LPC_ORDER + 1],
    lag_index: i16,
    contour_index: i8,
    signal_type: i8,
    quant_offset_type: i8,
    nlsf_interp_coef_q2: i8,
    per_index: i8,
    ltp_scale_index: i8,
    seed: i8,
}

#[derive(Clone, Copy, Default)]
struct DecCtrl {
    pitch_l: [i32; MAX_NB_SUBFR],
    gains_q16: [i32; MAX_NB_SUBFR],
    pred_coef_q12: [[i16; MAX_LPC_ORDER]; 2],
    ltp_coef_q14: [i16; LTP_ORDER * MAX_NB_SUBFR],
    ltp_scale_q14: i32,
}

#[derive(Clone)]
struct Plc {
    pitch_l_q8: i32,
    ltp_coef_q14: [i16; LTP_ORDER],
    prev_lpc_q12: [i16; MAX_LPC_ORDER],
    last_frame_lost: bool,
    rand_seed: i32,
    rand_scale_q14: i32,
    conc_energy: i32,
    conc_energy_shift: i32,
    prev_ltp_scale_q14: i32,
    prev_gain_q16: [i32; 2],
    fs_khz: i32,
    nb_subfr: usize,
    subfr_length: usize,
}

#[derive(Clone)]
struct Cng {
    exc_buf_q14: [i32; MAX_FRAME_LENGTH],
    smth_nlsf_q15: [i16; MAX_LPC_ORDER],
    synth_state: [i32; MAX_LPC_ORDER],
    smth_gain_q16: i32,
    rand_seed: i32,
    fs_khz: i32,
}

#[derive(Clone)]
struct Channel {
    prev_gain_q16: i32,
    exc_q14: [i32; MAX_FRAME_LENGTH],
    slpc_q14_buf: [i32; MAX_LPC_ORDER],
    out_buf: [i16; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
    lag_prev: i32,
    last_gain_index: i32,
    fs_khz: i32,
    fs_api_hz: i32,
    nb_subfr: usize,
    frame_length: usize,
    subfr_length: usize,
    ltp_mem_length: usize,
    lpc_order: usize,
    prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    first_frame_after_reset: bool,
    pitch_lag_low_bits_icdf: &'static [u8],
    pitch_contour_icdf: &'static [u8],
    frames_decoded: usize,
    frames_per_packet: usize,
    ec_prev_signal_type: i32,
    ec_prev_lag_index: i32,
    vad_flags: [bool; 3],
    lbrr_flag: bool,
    lbrr_flags: [bool; 3],
    resampler: Resampler,
    nlsf_cb: &'static NlsfCb,
    indices: Indices,
    cng: Cng,
    loss_cnt: i32,
    prev_signal_type: i32,
    plc: Plc,
}

impl Channel {
    /// silk_init_decoder.
    fn new() -> Channel {
        let mut c = Channel {
            prev_gain_q16: 65536,
            exc_q14: [0; MAX_FRAME_LENGTH],
            slpc_q14_buf: [0; MAX_LPC_ORDER],
            out_buf: [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
            lag_prev: 0,
            last_gain_index: 0,
            fs_khz: 0,
            fs_api_hz: 0,
            nb_subfr: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            first_frame_after_reset: true,
            pitch_lag_low_bits_icdf: &[],
            pitch_contour_icdf: &[],
            frames_decoded: 0,
            frames_per_packet: 0,
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            vad_flags: [false; 3],
            lbrr_flag: false,
            lbrr_flags: [false; 3],
            resampler: Resampler::new(),
            nlsf_cb: &NLSF_CB_NB_MB,
            indices: Indices::default(),
            cng: Cng {
                exc_buf_q14: [0; MAX_FRAME_LENGTH],
                smth_nlsf_q15: [0; MAX_LPC_ORDER],
                synth_state: [0; MAX_LPC_ORDER],
                smth_gain_q16: 0,
                rand_seed: 0,
                fs_khz: 0,
            },
            loss_cnt: 0,
            prev_signal_type: 0,
            plc: Plc {
                pitch_l_q8: 0,
                ltp_coef_q14: [0; LTP_ORDER],
                prev_lpc_q12: [0; MAX_LPC_ORDER],
                last_frame_lost: false,
                rand_seed: 0,
                rand_scale_q14: 0,
                conc_energy: 0,
                conc_energy_shift: 0,
                prev_ltp_scale_q14: 0,
                prev_gain_q16: [0; 2],
                fs_khz: 0,
                nb_subfr: 0,
                subfr_length: 0,
            },
        };
        c.cng_reset();
        c.plc_reset();
        c
    }

    fn cng_reset(&mut self) {
        let step = i16::MAX as i32 / (self.lpc_order as i32 + 1);
        let mut acc = 0;
        for v in self.cng.smth_nlsf_q15.iter_mut().take(self.lpc_order) {
            acc += step;
            *v = acc as i16;
        }
        self.cng.smth_gain_q16 = 0;
        self.cng.rand_seed = 3_176_576;
    }

    fn plc_reset(&mut self) {
        self.plc.pitch_l_q8 = (self.frame_length as i32) << (8 - 1);
        self.plc.prev_gain_q16 = [65536, 65536];
        self.plc.subfr_length = 20;
        self.plc.nb_subfr = 2;
    }

    /// silk_decoder_set_fs (API rate 48 kHz).
    fn set_fs(&mut self, fs_khz: i32) {
        self.subfr_length = SUB_FRAME_LENGTH_MS * fs_khz as usize;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz || self.fs_api_hz != 48000 {
            self.resampler = Resampler::init(fs_khz * 1000);
            self.fs_api_hz = 48000;
        }
        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            self.pitch_contour_icdf = match (fs_khz == 8, self.nb_subfr == MAX_NB_SUBFR) {
                (true, true) => &PITCH_CONTOUR_NB_ICDF,
                (true, false) => &PITCH_CONTOUR_10_MS_NB_ICDF,
                (false, true) => &PITCH_CONTOUR_ICDF,
                (false, false) => &PITCH_CONTOUR_10_MS_ICDF,
            };
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = LTP_MEM_LENGTH_MS * fs_khz as usize;
                if fs_khz == 16 {
                    self.lpc_order = 16;
                    self.nlsf_cb = &NLSF_CB_WB;
                } else {
                    self.lpc_order = 10;
                    self.nlsf_cb = &NLSF_CB_NB_MB;
                }
                self.pitch_lag_low_bits_icdf = match fs_khz {
                    16 => &UNIFORM8_ICDF,
                    12 => &UNIFORM6_ICDF,
                    _ => &UNIFORM4_ICDF,
                };
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
                self.out_buf.fill(0);
                self.slpc_q14_buf.fill(0);
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }

    /// silk_decode_indices.
    fn decode_indices(&mut self, ec: &mut RangeDecoder, frame_index: usize, decode_lbrr: bool, cond: i32) {
        let ix = if decode_lbrr || self.vad_flags[frame_index] {
            ec.icdf(&TYPE_OFFSET_VAD_ICDF, 8) + 2
        } else {
            ec.icdf(&TYPE_OFFSET_NO_VAD_ICDF, 8)
        };
        let ind = &mut self.indices;
        ind.signal_type = (ix >> 1) as i8;
        ind.quant_offset_type = (ix & 1) as i8;
        if cond == CODE_CONDITIONALLY {
            ind.gains[0] = ec.icdf(&DELTA_GAIN_ICDF, 8) as i8;
        } else {
            let st = ind.signal_type as usize;
            ind.gains[0] = (ec.icdf(&GAIN_ICDF[st * 8..st * 8 + 8], 8) << 3) as i8;
            ind.gains[0] += ec.icdf(&UNIFORM8_ICDF, 8) as i8;
        }
        for i in 1..self.nb_subfr {
            ind.gains[i] = ec.icdf(&DELTA_GAIN_ICDF, 8) as i8;
        }
        let cb = self.nlsf_cb;
        let st = (ind.signal_type >> 1) as usize;
        ind.nlsf[0] = ec.icdf(&cb.cb1_icdf[st * cb.n_vectors..], 8) as i8;
        let mut ec_ix = [0i16; MAX_LPC_ORDER];
        let mut pred = [0u8; MAX_LPC_ORDER];
        nlsf_unpack(&mut ec_ix, &mut pred, cb, ind.nlsf[0] as usize);
        for i in 0..cb.order {
            let mut ix = ec.icdf(&cb.ec_icdf[ec_ix[i] as usize..], 8) as i32;
            if ix == 0 {
                ix -= ec.icdf(&NLSF_EXT_ICDF, 8) as i32;
            } else if ix == 2 * NLSF_QUANT_MAX_AMPLITUDE {
                ix += ec.icdf(&NLSF_EXT_ICDF, 8) as i32;
            }
            ind.nlsf[i + 1] = (ix - NLSF_QUANT_MAX_AMPLITUDE) as i8;
        }
        ind.nlsf_interp_coef_q2 =
            if self.nb_subfr == MAX_NB_SUBFR { ec.icdf(&NLSF_INTERPOLATION_FACTOR_ICDF, 8) as i8 } else { 4 };
        if ind.signal_type as i32 == TYPE_VOICED {
            let mut absolute = true;
            if cond == CODE_CONDITIONALLY && self.ec_prev_signal_type == TYPE_VOICED {
                let delta = ec.icdf(&PITCH_DELTA_ICDF, 8) as i32;
                if delta > 0 {
                    ind.lag_index = (self.ec_prev_lag_index + delta - 9) as i16;
                    absolute = false;
                }
            }
            if absolute {
                ind.lag_index = (ec.icdf(&PITCH_LAG_ICDF, 8) as i32 * (self.fs_khz >> 1)) as i16;
                ind.lag_index += ec.icdf(self.pitch_lag_low_bits_icdf, 8) as i16;
            }
            self.ec_prev_lag_index = ind.lag_index as i32;
            ind.contour_index = ec.icdf(self.pitch_contour_icdf, 8) as i8;
            ind.per_index = ec.icdf(&LTP_PER_INDEX_ICDF, 8) as i8;
            let table: &[u8] = match ind.per_index {
                0 => &LTP_GAIN_ICDF_0,
                1 => &LTP_GAIN_ICDF_1,
                _ => &LTP_GAIN_ICDF_2,
            };
            for k in 0..self.nb_subfr {
                ind.ltp_index[k] = ec.icdf(table, 8) as i8;
            }
            ind.ltp_scale_index = if cond == CODE_INDEPENDENTLY { ec.icdf(&LTP_SCALE_ICDF, 8) as i8 } else { 0 };
        }
        self.ec_prev_signal_type = ind.signal_type as i32;
        ind.seed = ec.icdf(&UNIFORM4_ICDF, 8) as i8;
    }

    /// silk_decode_parameters.
    fn decode_parameters(&mut self, ctrl: &mut DecCtrl, cond: i32) {
        // silk_gains_dequant
        const INV_SCALE_Q16: i32 = (65536 * (((88 - 2) * 128) / 6)) / (N_LEVELS_QGAIN - 1);
        const OFFSET: i32 = (2 * 128) / 6 + 16 * 128;
        let mut prev = self.last_gain_index;
        for k in 0..self.nb_subfr {
            if k == 0 && cond != CODE_CONDITIONALLY {
                prev = (self.indices.gains[k] as i32).max(prev - 16);
            } else {
                let ind_tmp = self.indices.gains[k] as i32 + MIN_DELTA_GAIN_QUANT;
                let threshold = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + prev;
                if ind_tmp > threshold {
                    prev += (ind_tmp << 1) - threshold;
                } else {
                    prev += ind_tmp;
                }
            }
            prev = prev.clamp(0, N_LEVELS_QGAIN - 1);
            ctrl.gains_q16[k] = log2lin((smulwb(INV_SCALE_Q16, prev) + OFFSET).min(3967));
        }
        self.last_gain_index = prev;

        let order = self.lpc_order;
        let mut nlsf = [0i16; MAX_LPC_ORDER];
        nlsf_decode(&mut nlsf, &self.indices.nlsf, self.nlsf_cb);
        nlsf2a(&mut ctrl.pred_coef_q12[1], &nlsf, order);
        if self.first_frame_after_reset {
            self.indices.nlsf_interp_coef_q2 = 4;
        }
        if self.indices.nlsf_interp_coef_q2 < 4 {
            let mut nlsf0 = [0i16; MAX_LPC_ORDER];
            let c = self.indices.nlsf_interp_coef_q2 as i32;
            for i in 0..order {
                let d = nlsf[i] as i32 - self.prev_nlsf_q15[i] as i32;
                nlsf0[i] = (self.prev_nlsf_q15[i] as i32 + ((c * d) >> 2)) as i16;
            }
            nlsf2a(&mut ctrl.pred_coef_q12[0], &nlsf0, order);
        } else {
            ctrl.pred_coef_q12[0] = ctrl.pred_coef_q12[1];
        }
        self.prev_nlsf_q15[..order].copy_from_slice(&nlsf[..order]);
        if self.loss_cnt != 0 {
            bwexpander(&mut ctrl.pred_coef_q12[0], order, BWE_AFTER_LOSS_Q16);
            bwexpander(&mut ctrl.pred_coef_q12[1], order, BWE_AFTER_LOSS_Q16);
        }
        if self.indices.signal_type as i32 == TYPE_VOICED {
            // silk_decode_pitch
            let (cb, size): (&[i8], usize) = if self.fs_khz == 8 {
                if self.nb_subfr == MAX_NB_SUBFR {
                    (&CB_LAGS_STAGE2, 11)
                } else {
                    (&CB_LAGS_STAGE2_10_MS, 3)
                }
            } else if self.nb_subfr == MAX_NB_SUBFR {
                (&CB_LAGS_STAGE3, 34)
            } else {
                (&CB_LAGS_STAGE3_10_MS, 12)
            };
            let min_lag = PE_MIN_LAG_MS * self.fs_khz;
            let max_lag = PE_MAX_LAG_MS * self.fs_khz;
            let lag = min_lag + self.indices.lag_index as i32;
            for k in 0..self.nb_subfr {
                let v = lag + cb[k * size + self.indices.contour_index as usize] as i32;
                ctrl.pitch_l[k] = v.clamp(min_lag, max_lag);
            }
            let vq: &[i8] = match self.indices.per_index {
                0 => &LTP_GAIN_VQ_0,
                1 => &LTP_GAIN_VQ_1,
                _ => &LTP_GAIN_VQ_2,
            };
            for k in 0..self.nb_subfr {
                let ix = self.indices.ltp_index[k] as usize;
                for i in 0..LTP_ORDER {
                    ctrl.ltp_coef_q14[k * LTP_ORDER + i] = ((vq[ix * LTP_ORDER + i] as i32) << 7) as i16;
                }
            }
            ctrl.ltp_scale_q14 = LTP_SCALES_Q14[self.indices.ltp_scale_index as usize] as i32;
        } else {
            ctrl.pitch_l[..self.nb_subfr].fill(0);
            ctrl.ltp_coef_q14[..LTP_ORDER * self.nb_subfr].fill(0);
            self.indices.per_index = 0;
            ctrl.ltp_scale_q14 = 0;
        }
    }

    /// silk_decode_core.
    fn decode_core(&mut self, ctrl: &mut DecCtrl, xq: &mut [i16], pulses: &[i32]) {
        let offset_q10 = QUANTIZATION_OFFSETS_Q10
            [(self.indices.signal_type >> 1) as usize * 2 + self.indices.quant_offset_type as usize]
            as i32;
        let nlsf_interpolation = self.indices.nlsf_interp_coef_q2 < 4;
        let mut rand_seed = self.indices.seed as i32;
        for i in 0..self.frame_length {
            rand_seed = silk_rand(rand_seed);
            let mut e = pulses[i] << 14;
            if e > 0 {
                e -= QUANT_LEVEL_ADJUST_Q10 << 4;
            } else if e < 0 {
                e += QUANT_LEVEL_ADJUST_Q10 << 4;
            }
            e += offset_q10 << 4;
            if rand_seed < 0 {
                e = -e;
            }
            self.exc_q14[i] = e;
            rand_seed = rand_seed.wrapping_add(pulses[i]);
        }
        let mut slpc = [0i32; MAX_SUB_FRAME_LENGTH + MAX_LPC_ORDER];
        slpc[..MAX_LPC_ORDER].copy_from_slice(&self.slpc_q14_buf);
        let mut sltp = [0i16; MAX_FRAME_LENGTH];
        let mut sltp_q15 = [0i32; 2 * MAX_FRAME_LENGTH];
        let mut res_q14 = [0i32; MAX_SUB_FRAME_LENGTH];
        let mut sltp_buf_idx = self.ltp_mem_length;
        let order = self.lpc_order;
        let sl = self.subfr_length;
        let mut lag = 0usize;
        for k in 0..self.nb_subfr {
            let a_q12 = ctrl.pred_coef_q12[k >> 1];
            let mut signal_type = self.indices.signal_type as i32;
            let gain_q10 = ctrl.gains_q16[k] >> 6;
            let mut inv_gain_q31 = inverse32_varq(ctrl.gains_q16[k], 47);
            let gain_adj_q16;
            if ctrl.gains_q16[k] != self.prev_gain_q16 {
                gain_adj_q16 = div32_varq(self.prev_gain_q16, ctrl.gains_q16[k], 16);
                for v in slpc.iter_mut().take(MAX_LPC_ORDER) {
                    *v = smulww(gain_adj_q16, *v);
                }
            } else {
                gain_adj_q16 = 1 << 16;
            }
            self.prev_gain_q16 = ctrl.gains_q16[k];
            if self.loss_cnt != 0
                && self.prev_signal_type == TYPE_VOICED
                && self.indices.signal_type as i32 != TYPE_VOICED
                && k < MAX_NB_SUBFR / 2
            {
                let b = &mut ctrl.ltp_coef_q14[k * LTP_ORDER..k * LTP_ORDER + LTP_ORDER];
                b.fill(0);
                b[LTP_ORDER / 2] = 4096; // 0.25 in Q14
                signal_type = TYPE_VOICED;
                ctrl.pitch_l[k] = self.lag_prev;
            }
            let b_q14 = &ctrl.ltp_coef_q14[k * LTP_ORDER..k * LTP_ORDER + LTP_ORDER];
            if signal_type == TYPE_VOICED {
                lag = ctrl.pitch_l[k] as usize;
                if k == 0 || (k == 2 && nlsf_interpolation) {
                    let start = self.ltp_mem_length - lag - order - LTP_ORDER / 2;
                    if k == 2 {
                        let ml = self.ltp_mem_length;
                        self.out_buf[ml..ml + 2 * sl].copy_from_slice(&xq[..2 * sl]);
                    }
                    let len = self.ltp_mem_length - start;
                    lpc_analysis_filter(&mut sltp[start..], &self.out_buf[start + k * sl..], &a_q12, len, order);
                    if k == 0 {
                        inv_gain_q31 = smulwb(inv_gain_q31, ctrl.ltp_scale_q14) << 2;
                    }
                    for i in 0..lag + LTP_ORDER / 2 {
                        sltp_q15[sltp_buf_idx - i - 1] = smulwb(inv_gain_q31, sltp[self.ltp_mem_length - i - 1] as i32);
                    }
                } else if gain_adj_q16 != 1 << 16 {
                    for i in 0..lag + LTP_ORDER / 2 {
                        let v = &mut sltp_q15[sltp_buf_idx - i - 1];
                        *v = smulww(gain_adj_q16, *v);
                    }
                }
            }
            let exc = &self.exc_q14[k * sl..(k + 1) * sl];
            if signal_type == TYPE_VOICED {
                let mut p = sltp_buf_idx - lag + LTP_ORDER / 2;
                for i in 0..sl {
                    let mut ltp = 2i32;
                    ltp = smlawb(ltp, sltp_q15[p], b_q14[0] as i32);
                    ltp = smlawb(ltp, sltp_q15[p - 1], b_q14[1] as i32);
                    ltp = smlawb(ltp, sltp_q15[p - 2], b_q14[2] as i32);
                    ltp = smlawb(ltp, sltp_q15[p - 3], b_q14[3] as i32);
                    ltp = smlawb(ltp, sltp_q15[p - 4], b_q14[4] as i32);
                    p += 1;
                    res_q14[i] = exc[i].wrapping_add(ltp << 1);
                    sltp_q15[sltp_buf_idx] = res_q14[i] << 1;
                    sltp_buf_idx += 1;
                }
            } else {
                res_q14[..sl].copy_from_slice(exc);
            }
            let out = &mut xq[k * sl..(k + 1) * sl];
            for i in 0..sl {
                let mut pred = (order >> 1) as i32;
                for j in 0..order {
                    pred = smlawb(pred, slpc[MAX_LPC_ORDER + i - 1 - j], a_q12[j] as i32);
                }
                slpc[MAX_LPC_ORDER + i] = res_q14[i].wrapping_add(pred.wrapping_shl(4));
                out[i] = sat16(rshift_round(smulww(slpc[MAX_LPC_ORDER + i], gain_q10), 8)) as i16;
            }
            slpc.copy_within(sl..sl + MAX_LPC_ORDER, 0);
        }
        self.slpc_q14_buf.copy_from_slice(&slpc[..MAX_LPC_ORDER]);
    }

    /// silk_PLC.
    fn plc(&mut self, ctrl: &mut DecCtrl, frame: &mut [i16], lost: bool) {
        if self.fs_khz != self.plc.fs_khz {
            self.plc_reset();
            self.plc.fs_khz = self.fs_khz;
        }
        if lost {
            self.plc_conceal(ctrl, frame);
            self.loss_cnt += 1;
        } else {
            self.plc_update(ctrl);
        }
    }

    fn plc_update(&mut self, ctrl: &DecCtrl) {
        let nb = self.nb_subfr;
        self.prev_signal_type = self.indices.signal_type as i32;
        let mut ltp_gain_q14 = 0i32;
        if self.indices.signal_type as i32 == TYPE_VOICED {
            let mut j = 0;
            while j * self.subfr_length < ctrl.pitch_l[nb - 1] as usize {
                if j == nb {
                    break;
                }
                let base = (nb - 1 - j) * LTP_ORDER;
                let t: i32 = ctrl.ltp_coef_q14[base..base + LTP_ORDER].iter().map(|&v| v as i32).sum();
                if t > ltp_gain_q14 {
                    ltp_gain_q14 = t;
                    self.plc.ltp_coef_q14.copy_from_slice(&ctrl.ltp_coef_q14[base..base + LTP_ORDER]);
                    self.plc.pitch_l_q8 = ctrl.pitch_l[nb - 1 - j] << 8;
                }
                j += 1;
            }
            self.plc.ltp_coef_q14 = [0; LTP_ORDER];
            self.plc.ltp_coef_q14[LTP_ORDER / 2] = ltp_gain_q14 as i16;
            if ltp_gain_q14 < V_PITCH_GAIN_START_MIN_Q14 {
                let scale_q10 = (V_PITCH_GAIN_START_MIN_Q14 << 10) / ltp_gain_q14.max(1);
                for v in self.plc.ltp_coef_q14.iter_mut() {
                    *v = (smulbb(*v as i32, scale_q10) >> 10) as i16;
                }
            } else if ltp_gain_q14 > V_PITCH_GAIN_START_MAX_Q14 {
                let scale_q14 = (V_PITCH_GAIN_START_MAX_Q14 << 14) / ltp_gain_q14.max(1);
                for v in self.plc.ltp_coef_q14.iter_mut() {
                    *v = (smulbb(*v as i32, scale_q14) >> 14) as i16;
                }
            }
        } else {
            self.plc.pitch_l_q8 = smulbb(self.fs_khz, 18) << 8;
            self.plc.ltp_coef_q14 = [0; LTP_ORDER];
        }
        let order = self.lpc_order;
        self.plc.prev_lpc_q12[..order].copy_from_slice(&ctrl.pred_coef_q12[1][..order]);
        self.plc.prev_ltp_scale_q14 = ctrl.ltp_scale_q14;
        self.plc.prev_gain_q16 = [ctrl.gains_q16[nb - 2], ctrl.gains_q16[nb - 1]];
        self.plc.subfr_length = self.subfr_length;
        self.plc.nb_subfr = nb;
    }

    fn plc_conceal(&mut self, ctrl: &mut DecCtrl, frame: &mut [i16]) {
        let prev_gain_q10 = [self.plc.prev_gain_q16[0] >> 6, self.plc.prev_gain_q16[1] >> 6];
        if self.first_frame_after_reset {
            self.plc.prev_lpc_q12 = [0; MAX_LPC_ORDER];
        }
        let psl = self.plc.subfr_length;
        let pnb = self.plc.nb_subfr;
        let mut exc_buf = [0i16; 2 * MAX_SUB_FRAME_LENGTH];
        for k in 0..2 {
            for i in 0..psl {
                exc_buf[k * psl + i] =
                    sat16(smulww(self.exc_q14[i + (k + pnb - 2) * psl], prev_gain_q10[k]) >> 8) as i16;
            }
        }
        let (energy1, shift1) = sum_sqr_shift(&exc_buf[..psl]);
        let (energy2, shift2) = sum_sqr_shift(&exc_buf[psl..2 * psl]);
        let rand_off = if energy1 >> shift2 < energy2 >> shift1 {
            ((pnb - 1) * psl).saturating_sub(RAND_BUF_SIZE)
        } else {
            (pnb * psl).saturating_sub(RAND_BUF_SIZE)
        };
        let mut b_q14 = self.plc.ltp_coef_q14;
        let mut rand_scale_q14 = self.plc.rand_scale_q14;
        let att = (self.loss_cnt as usize).min(1);
        let harm_gain_q15 = HARM_ATT_Q15[att];
        let mut rand_gain_q15 = if self.prev_signal_type == TYPE_VOICED {
            PLC_RAND_ATTENUATE_V_Q15[att]
        } else {
            PLC_RAND_ATTENUATE_UV_Q15[att]
        };
        let order = self.lpc_order;
        bwexpander(&mut self.plc.prev_lpc_q12, order, BWE_COEF_Q16);
        let a_q12 = self.plc.prev_lpc_q12;
        if self.loss_cnt == 0 {
            rand_scale_q14 = 1 << 14;
            if self.prev_signal_type == TYPE_VOICED {
                for &b in &b_q14 {
                    rand_scale_q14 -= b as i32;
                }
                rand_scale_q14 = (rand_scale_q14 as i16).max(3277) as i32;
                rand_scale_q14 = (smulbb(rand_scale_q14, self.plc.prev_ltp_scale_q14) >> 14) as i16 as i32;
            } else {
                let inv_gain_q30 = lpc_inverse_pred_gain(&self.plc.prev_lpc_q12, order);
                let mut down = ((1 << 30) >> LOG2_INV_LPC_GAIN_HIGH_THRES).min(inv_gain_q30);
                down = ((1 << 30) >> LOG2_INV_LPC_GAIN_LOW_THRES).max(down);
                down <<= LOG2_INV_LPC_GAIN_HIGH_THRES;
                rand_gain_q15 = smulwb(down, rand_gain_q15) >> 14;
            }
        }
        let mut rand_seed = self.plc.rand_seed;
        let mut lag = rshift_round(self.plc.pitch_l_q8, 8) as usize;
        let mut sltp_buf_idx = self.ltp_mem_length;
        let idx = self.ltp_mem_length - lag - order - LTP_ORDER / 2;
        let mut sltp = [0i16; MAX_FRAME_LENGTH];
        let len = self.ltp_mem_length - idx;
        lpc_analysis_filter(&mut sltp[idx..], &self.out_buf[idx..], &a_q12, len, order);
        let inv_gain_q30 = inverse32_varq(self.plc.prev_gain_q16[1], 46).min(i32::MAX >> 1);
        let mut sltp_q14 = [0i32; 2 * MAX_FRAME_LENGTH];
        for i in idx + order..self.ltp_mem_length {
            sltp_q14[i] = smulwb(inv_gain_q30, sltp[i] as i32);
        }
        let rand_buf: [i32; MAX_FRAME_LENGTH] = self.exc_q14;
        for _ in 0..self.nb_subfr {
            let mut p = sltp_buf_idx - lag + LTP_ORDER / 2;
            for _ in 0..self.subfr_length {
                let mut ltp = 2i32;
                ltp = smlawb(ltp, sltp_q14[p], b_q14[0] as i32);
                ltp = smlawb(ltp, sltp_q14[p - 1], b_q14[1] as i32);
                ltp = smlawb(ltp, sltp_q14[p - 2], b_q14[2] as i32);
                ltp = smlawb(ltp, sltp_q14[p - 3], b_q14[3] as i32);
                ltp = smlawb(ltp, sltp_q14[p - 4], b_q14[4] as i32);
                p += 1;
                rand_seed = silk_rand(rand_seed);
                let ri = ((rand_seed >> 25) & RAND_BUF_MASK) as usize;
                sltp_q14[sltp_buf_idx] = smlawb(ltp, rand_buf[rand_off + ri], rand_scale_q14).wrapping_shl(2);
                sltp_buf_idx += 1;
            }
            for b in b_q14.iter_mut() {
                *b = (smulbb(harm_gain_q15, *b as i32) >> 15) as i16;
            }
            rand_scale_q14 = smulbb(rand_scale_q14, rand_gain_q15) >> 15;
            self.plc.pitch_l_q8 = smlawb(self.plc.pitch_l_q8, self.plc.pitch_l_q8, PITCH_DRIFT_FAC_Q16);
            self.plc.pitch_l_q8 = self.plc.pitch_l_q8.min(smulbb(MAX_PITCH_LAG_MS, self.fs_khz) << 8);
            lag = rshift_round(self.plc.pitch_l_q8, 8) as usize;
        }
        // LPC synthesis on the excitation, in place in sLTP_Q14.
        let base = self.ltp_mem_length - MAX_LPC_ORDER;
        sltp_q14[base..base + MAX_LPC_ORDER].copy_from_slice(&self.slpc_q14_buf);
        for i in 0..self.frame_length {
            let mut pred = (order >> 1) as i32;
            for j in 0..order {
                pred = smlawb(pred, sltp_q14[base + MAX_LPC_ORDER + i - 1 - j], a_q12[j] as i32);
            }
            let v = &mut sltp_q14[base + MAX_LPC_ORDER + i];
            *v = v.wrapping_add(pred.wrapping_shl(4));
            frame[i] = sat16(rshift_round(smulww(*v, prev_gain_q10[1]), 8)) as i16;
        }
        let fl = self.frame_length;
        self.slpc_q14_buf.copy_from_slice(&sltp_q14[base + fl..base + fl + MAX_LPC_ORDER]);
        self.plc.ltp_coef_q14 = b_q14;
        self.plc.rand_seed = rand_seed;
        self.plc.rand_scale_q14 = rand_scale_q14;
        for v in ctrl.pitch_l.iter_mut() {
            *v = lag as i32;
        }
    }

    fn plc_glue_frames(&mut self, frame: &mut [i16]) {
        if self.loss_cnt != 0 {
            let (e, s) = sum_sqr_shift(frame);
            self.plc.conc_energy = e;
            self.plc.conc_energy_shift = s;
            self.plc.last_frame_lost = true;
        } else {
            if self.plc.last_frame_lost {
                let (mut energy, shift) = sum_sqr_shift(frame);
                if shift > self.plc.conc_energy_shift {
                    self.plc.conc_energy >>= shift - self.plc.conc_energy_shift;
                } else if shift < self.plc.conc_energy_shift {
                    energy >>= self.plc.conc_energy_shift - shift;
                }
                if energy > self.plc.conc_energy {
                    let lz = clz32(self.plc.conc_energy) - 1;
                    self.plc.conc_energy <<= lz;
                    energy >>= (24 - lz).max(0);
                    let frac_q24 = self.plc.conc_energy / energy.max(1);
                    let mut gain_q16 = sqrt_approx(frac_q24) << 4;
                    let slope_q16 = (((1 << 16) - gain_q16) / frame.len() as i32) << 2;
                    for v in frame.iter_mut() {
                        *v = smulwb(gain_q16, *v as i32) as i16;
                        gain_q16 += slope_q16;
                        if gain_q16 > 1 << 16 {
                            break;
                        }
                    }
                }
            }
            self.plc.last_frame_lost = false;
        }
    }

    /// silk_CNG.
    fn cng(&mut self, ctrl: &DecCtrl, frame: &mut [i16]) {
        if self.fs_khz != self.cng.fs_khz {
            self.cng_reset();
            self.cng.fs_khz = self.fs_khz;
        }
        let order = self.lpc_order;
        if self.loss_cnt == 0 && self.prev_signal_type == TYPE_NO_VOICE_ACTIVITY {
            for i in 0..order {
                let d = self.prev_nlsf_q15[i] as i32 - self.cng.smth_nlsf_q15[i] as i32;
                self.cng.smth_nlsf_q15[i] = (self.cng.smth_nlsf_q15[i] as i32 + smulwb(d, CNG_NLSF_SMTH_Q16)) as i16;
            }
            let mut max_gain = 0;
            let mut subfr = 0;
            for i in 0..self.nb_subfr {
                if ctrl.gains_q16[i] > max_gain {
                    max_gain = ctrl.gains_q16[i];
                    subfr = i;
                }
            }
            let sl = self.subfr_length;
            self.cng.exc_buf_q14.copy_within(0..(self.nb_subfr - 1) * sl, sl);
            self.cng.exc_buf_q14[..sl].copy_from_slice(&self.exc_q14[subfr * sl..(subfr + 1) * sl]);
            for i in 0..self.nb_subfr {
                self.cng.smth_gain_q16 += smulwb(ctrl.gains_q16[i] - self.cng.smth_gain_q16, CNG_GAIN_SMTH_Q16);
            }
        }
        if self.loss_cnt != 0 {
            let length = frame.len();
            let mut sig = [0i32; MAX_FRAME_LENGTH + MAX_LPC_ORDER];
            // silk_CNG_exc
            let mut mask = CNG_BUF_MASK_MAX;
            while mask > length as i32 {
                mask >>= 1;
            }
            let mut seed = self.cng.rand_seed;
            for i in 0..length {
                seed = silk_rand(seed);
                let idx = ((seed >> 24) & mask) as usize;
                sig[MAX_LPC_ORDER + i] =
                    sat16(smulww(self.cng.exc_buf_q14[idx], self.cng.smth_gain_q16 >> 4)) as i16 as i32;
            }
            self.cng.rand_seed = seed;
            let mut a_q12 = [0i16; MAX_LPC_ORDER];
            let smth = self.cng.smth_nlsf_q15;
            nlsf2a(&mut a_q12, &smth, order);
            sig[..MAX_LPC_ORDER].copy_from_slice(&self.cng.synth_state);
            for i in 0..length {
                let mut sum_q6 = (order >> 1) as i32;
                for j in 0..order {
                    sum_q6 = smlawb(sum_q6, sig[MAX_LPC_ORDER + i - 1 - j], a_q12[j] as i32);
                }
                sig[MAX_LPC_ORDER + i] = sig[MAX_LPC_ORDER + i].wrapping_add(sum_q6.wrapping_shl(4));
                frame[i] = sat16(frame[i] as i32 + rshift_round(sum_q6, 6)) as i16;
            }
            self.cng.synth_state.copy_from_slice(&sig[length..length + MAX_LPC_ORDER]);
        } else {
            self.cng.synth_state[..order].fill(0);
        }
    }

    /// silk_decode_frame: one frame into `out` (frame_length samples).
    fn decode_frame(&mut self, ec: Option<&mut RangeDecoder>, out: &mut [i16], lost: bool, cond: i32) -> usize {
        let mut ctrl = DecCtrl::default();
        let mut l = self.frame_length;
        match ec {
            Some(ec) if !lost => {
                let fi = self.frames_decoded;
                self.decode_indices(ec, fi, false, cond);
                let mut pulses = [0i32; MAX_FRAME_LENGTH];
                decode_pulses(
                    ec,
                    &mut pulses,
                    self.indices.signal_type as i32,
                    self.indices.quant_offset_type as i32,
                    self.frame_length,
                );
                self.decode_parameters(&mut ctrl, cond);
                l = self.frame_length;
                self.decode_core(&mut ctrl, out, &pulses);
                self.plc(&mut ctrl, &mut out[..l], false);
                self.loss_cnt = 0;
                self.prev_signal_type = self.indices.signal_type as i32;
                self.first_frame_after_reset = false;
            }
            _ => self.plc(&mut ctrl, &mut out[..l], true),
        }
        let mv = self.ltp_mem_length - self.frame_length;
        self.out_buf.copy_within(self.frame_length..self.frame_length + mv, 0);
        self.out_buf[mv..mv + self.frame_length].copy_from_slice(&out[..self.frame_length]);
        self.plc_glue_frames(&mut out[..l]);
        self.cng(&ctrl, &mut out[..l]);
        self.lag_prev = ctrl.pitch_l[self.nb_subfr - 1];
        l
    }
}

/// silk_decode_pulses with the shell decoder and signs.
fn decode_pulses(
    ec: &mut RangeDecoder,
    pulses: &mut [i32],
    signal_type: i32,
    quant_offset_type: i32,
    frame_length: usize,
) {
    let rate = ec.icdf(&RATE_LEVELS_ICDF[(signal_type >> 1) as usize * 9..], 8);
    let mut iter = frame_length >> 4;
    if iter * SHELL_CODEC_FRAME_LENGTH < frame_length {
        iter += 1;
    }
    let mut sum = [0i32; MAX_FRAME_LENGTH / SHELL_CODEC_FRAME_LENGTH + 1];
    let mut nls = [0i32; MAX_FRAME_LENGTH / SHELL_CODEC_FRAME_LENGTH + 1];
    for i in 0..iter {
        sum[i] = ec.icdf(&PULSES_PER_BLOCK_ICDF[rate * 18..], 8) as i32;
        while sum[i] == MAX_PULSES as i32 + 1 {
            nls[i] += 1;
            let off = (N_RATE_LEVELS - 1) * 18 + (nls[i] == 10) as usize;
            sum[i] = ec.icdf(&PULSES_PER_BLOCK_ICDF[off..], 8) as i32;
        }
    }
    for i in 0..iter {
        let p = &mut pulses[i * 16..i * 16 + 16];
        if sum[i] > 0 {
            shell_decoder(p, ec, sum[i]);
        } else {
            p.fill(0);
        }
    }
    for i in 0..iter {
        if nls[i] > 0 {
            for v in pulses[i * 16..i * 16 + 16].iter_mut() {
                let mut a = *v;
                for _ in 0..nls[i] {
                    a = (a << 1) + ec.icdf(&LSB_ICDF, 8) as i32;
                }
                *v = a;
            }
            sum[i] |= nls[i] << 5;
        }
    }
    // Signs (silk_decode_signs).
    let base = 7 * (quant_offset_type + (signal_type << 1)) as usize;
    let blocks = (frame_length + SHELL_CODEC_FRAME_LENGTH / 2) >> 4;
    for i in 0..blocks {
        let p = sum[i];
        if p > 0 {
            let icdf = [SIGN_ICDF[base + (p & 0x1F).min(6) as usize], 0];
            for v in pulses[i * 16..i * 16 + 16].iter_mut() {
                if *v > 0 {
                    *v *= ((ec.icdf(&icdf, 8) as i32) << 1) - 1;
                }
            }
        }
    }
}

fn decode_split(ec: &mut RangeDecoder, p: i32, table: &[u8]) -> (i32, i32) {
    if p > 0 {
        let c1 = ec.icdf(&table[SHELL_CODE_TABLE_OFFSETS[p as usize] as usize..], 8) as i32;
        (c1, p - c1)
    } else {
        (0, 0)
    }
}

fn shell_decoder(p0: &mut [i32], ec: &mut RangeDecoder, pulses4: i32) {
    let mut p3 = [0i32; 2];
    let mut p2 = [0i32; 4];
    let mut p1 = [0i32; 8];
    (p3[0], p3[1]) = decode_split(ec, pulses4, &SHELL_CODE_TABLE3);
    (p2[0], p2[1]) = decode_split(ec, p3[0], &SHELL_CODE_TABLE2);
    (p1[0], p1[1]) = decode_split(ec, p2[0], &SHELL_CODE_TABLE1);
    (p0[0], p0[1]) = decode_split(ec, p1[0], &SHELL_CODE_TABLE0);
    (p0[2], p0[3]) = decode_split(ec, p1[1], &SHELL_CODE_TABLE0);
    (p1[2], p1[3]) = decode_split(ec, p2[1], &SHELL_CODE_TABLE1);
    (p0[4], p0[5]) = decode_split(ec, p1[2], &SHELL_CODE_TABLE0);
    (p0[6], p0[7]) = decode_split(ec, p1[3], &SHELL_CODE_TABLE0);
    (p2[2], p2[3]) = decode_split(ec, p3[1], &SHELL_CODE_TABLE2);
    (p1[4], p1[5]) = decode_split(ec, p2[2], &SHELL_CODE_TABLE1);
    (p0[8], p0[9]) = decode_split(ec, p1[4], &SHELL_CODE_TABLE0);
    (p0[10], p0[11]) = decode_split(ec, p1[5], &SHELL_CODE_TABLE0);
    (p1[6], p1[7]) = decode_split(ec, p2[3], &SHELL_CODE_TABLE1);
    (p0[12], p0[13]) = decode_split(ec, p1[6], &SHELL_CODE_TABLE0);
    (p0[14], p0[15]) = decode_split(ec, p1[7], &SHELL_CODE_TABLE0);
}

fn stereo_decode_pred(ec: &mut RangeDecoder) -> [i32; 2] {
    let n = ec.icdf(&STEREO_PRED_JOINT_ICDF, 8) as i32;
    let mut ix = [[0i32; 3]; 2];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for r in ix.iter_mut() {
        r[0] = ec.icdf(&UNIFORM3_ICDF, 8) as i32;
        r[1] = ec.icdf(&UNIFORM5_ICDF, 8) as i32;
    }
    let mut pred = [0i32; 2];
    for n in 0..2 {
        ix[n][0] += 3 * ix[n][2];
        let low = STEREO_PRED_QUANT_Q13[ix[n][0] as usize] as i32;
        // SILK_FIX_CONST(0.5 / STEREO_QUANT_SUB_STEPS, 16)
        let step = smulwb(STEREO_PRED_QUANT_Q13[ix[n][0] as usize + 1] as i32 - low, 6554);
        pred[n] = smlabb(low, step, 2 * ix[n][1] + 1);
    }
    pred[0] -= pred[1];
    pred
}

#[derive(Clone, Copy, Default)]
struct StereoState {
    pred_prev_q13: [i32; 2],
    s_mid: [i16; 2],
    s_side: [i16; 2],
}

fn stereo_ms_to_lr(st: &mut StereoState, x1: &mut [i16], x2: &mut [i16], pred_q13: &[i32; 2], fs_khz: usize, n: usize) {
    x1[..2].copy_from_slice(&st.s_mid);
    x2[..2].copy_from_slice(&st.s_side);
    st.s_mid.copy_from_slice(&x1[n..n + 2]);
    st.s_side.copy_from_slice(&x2[n..n + 2]);
    let mut pred0 = st.pred_prev_q13[0];
    let mut pred1 = st.pred_prev_q13[1];
    let interp = STEREO_INTERP_LEN_MS * fs_khz;
    let denom_q16 = (1 << 16) / interp as i32;
    let delta0 = rshift_round(smulbb(pred_q13[0] - st.pred_prev_q13[0], denom_q16), 16);
    let delta1 = rshift_round(smulbb(pred_q13[1] - st.pred_prev_q13[1], denom_q16), 16);
    for k in 0..n {
        if k < interp {
            pred0 += delta0;
            pred1 += delta1;
        } else if k == interp {
            pred0 = pred_q13[0];
            pred1 = pred_q13[1];
        }
        let sum = ((x1[k] as i32 + x1[k + 2] as i32) + ((x1[k + 1] as i32) << 1)) << 9;
        let sum = smlawb((x2[k + 1] as i32) << 8, sum, pred0);
        let sum = smlawb(sum, (x1[k + 1] as i32) << 11, pred1);
        x2[k + 1] = sat16(rshift_round(sum, 8)) as i16;
    }
    st.pred_prev_q13 = *pred_q13;
    for k in 0..n {
        let s = x1[k + 1] as i32 + x2[k + 1] as i32;
        let d = x1[k + 1] as i32 - x2[k + 1] as i32;
        x1[k + 1] = sat16(s) as i16;
        x2[k + 1] = sat16(d) as i16;
    }
}

/// What the Opus layer tells SILK about the packet.
pub struct DecControl {
    pub channels_api: usize,
    pub channels_internal: usize,
    pub internal_rate: u32,
    pub payload_size_ms: usize,
}

impl DecControl {
    pub fn new(channels: usize) -> DecControl {
        DecControl { channels_api: channels, channels_internal: channels, internal_rate: 16000, payload_size_ms: 20 }
    }
}

/// The SILK decoder (silk_decoder): up to two channels plus stereo state.
pub struct Silk {
    ch: [Channel; 2],
    stereo: StereoState,
    channels_api: usize,
    channels_internal: usize,
    prev_decode_only_middle: bool,
}

impl Default for Silk {
    fn default() -> Self {
        Self::new()
    }
}

impl Silk {
    pub fn new() -> Silk {
        Silk {
            ch: [Channel::new(), Channel::new()],
            stereo: StereoState::default(),
            channels_api: 0,
            channels_internal: 0,
            prev_decode_only_middle: false,
        }
    }

    /// silk_InitDecoder, with the RFC 8251 stereo reset.
    pub fn reset(&mut self) {
        self.ch = [Channel::new(), Channel::new()];
        self.stereo = StereoState::default();
        self.prev_decode_only_middle = false;
    }

    /// silk_Decode: one SILK frame (10 or 20 ms) of the packet into `out`
    /// (interleaved, `ctl.channels_api` channels, 48 kHz). Returns the
    /// samples per channel written.
    pub fn decode(
        &mut self,
        ctl: &mut DecControl,
        lost: bool,
        new_packet: bool,
        ec: Option<&mut RangeDecoder>,
        out: &mut [i16],
    ) -> Result<usize, &'static str> {
        let nci = ctl.channels_internal;
        let nca = ctl.channels_api;
        let mut ec = if lost { None } else { ec };
        if new_packet {
            for c in self.ch.iter_mut().take(nci) {
                c.frames_decoded = 0;
            }
        }
        if nci > self.channels_internal {
            self.ch[1] = Channel::new();
        }
        let stereo_to_mono =
            nci == 1 && self.channels_internal == 2 && ctl.internal_rate == 1000 * self.ch[0].fs_khz as u32;
        if self.ch[0].frames_decoded == 0 {
            for c in self.ch.iter_mut().take(nci) {
                let (fpp, nb) = match ctl.payload_size_ms {
                    0 | 10 => (1, 2),
                    20 => (1, 4),
                    40 => (2, 4),
                    60 => (3, 4),
                    _ => return Err("invalid SILK frame size"),
                };
                c.frames_per_packet = fpp;
                c.nb_subfr = nb;
                let fs = (ctl.internal_rate >> 10) as i32 + 1;
                if fs != 8 && fs != 12 && fs != 16 {
                    return Err("invalid SILK sampling rate");
                }
                c.set_fs(fs);
            }
        }
        if nca == 2 && nci == 2 && (self.channels_api == 1 || self.channels_internal == 1) {
            self.stereo.pred_prev_q13 = [0; 2];
            self.stereo.s_side = [0; 2];
            self.ch[1].resampler = self.ch[0].resampler.clone();
        }
        self.channels_api = nca;
        self.channels_internal = nci;

        let mut ms_pred_q13 = [0i32; 2];
        let mut decode_only_middle = false;
        if let Some(ec) = ec.as_deref_mut() {
            if self.ch[0].frames_decoded == 0 {
                for c in self.ch.iter_mut().take(nci) {
                    for i in 0..c.frames_per_packet {
                        c.vad_flags[i] = ec.bit_logp(1);
                    }
                    c.lbrr_flag = ec.bit_logp(1);
                }
                for c in self.ch.iter_mut().take(nci) {
                    c.lbrr_flags = [false; 3];
                    if c.lbrr_flag {
                        if c.frames_per_packet == 1 {
                            c.lbrr_flags[0] = true;
                        } else {
                            let t: &[u8] =
                                if c.frames_per_packet == 2 { &LBRR_FLAGS_2_ICDF } else { &LBRR_FLAGS_3_ICDF };
                            let sym = ec.icdf(t, 8) + 1;
                            for i in 0..c.frames_per_packet {
                                c.lbrr_flags[i] = (sym >> i) & 1 != 0;
                            }
                        }
                    }
                }
                // Skip the redundant (LBRR) data.
                for i in 0..self.ch[0].frames_per_packet {
                    for n in 0..nci {
                        if self.ch[n].lbrr_flags[i] {
                            if nci == 2 && n == 0 {
                                stereo_decode_pred(ec);
                                if !self.ch[1].lbrr_flags[i] {
                                    ec.icdf(&STEREO_ONLY_CODE_MID_ICDF, 8);
                                }
                            }
                            let cond = if i > 0 && self.ch[n].lbrr_flags[i - 1] {
                                CODE_CONDITIONALLY
                            } else {
                                CODE_INDEPENDENTLY
                            };
                            self.ch[n].decode_indices(ec, i, true, cond);
                            let mut pulses = [0i32; MAX_FRAME_LENGTH];
                            let c = &self.ch[n];
                            decode_pulses(
                                ec,
                                &mut pulses,
                                c.indices.signal_type as i32,
                                c.indices.quant_offset_type as i32,
                                c.frame_length,
                            );
                        }
                    }
                }
            }
        }
        if nci == 2 {
            match ec.as_deref_mut() {
                Some(ec) => {
                    ms_pred_q13 = stereo_decode_pred(ec);
                    let fd = self.ch[0].frames_decoded;
                    if !self.ch[1].vad_flags[fd] {
                        decode_only_middle = ec.icdf(&STEREO_ONLY_CODE_MID_ICDF, 8) != 0;
                    }
                }
                None => ms_pred_q13 = self.stereo.pred_prev_q13,
            }
        }
        if nci == 2 && !decode_only_middle && self.prev_decode_only_middle {
            let c = &mut self.ch[1];
            c.out_buf.fill(0);
            c.slpc_q14_buf.fill(0);
            c.lag_prev = 100;
            c.last_gain_index = 10;
            c.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
            c.first_frame_after_reset = true;
        }
        let has_side = if !lost { !decode_only_middle } else { !self.prev_decode_only_middle };

        let mut tmp = [[0i16; 16 * 20 + 2]; 2];
        let mut n_out_dec = 0;
        for n in 0..nci {
            if n == 0 || has_side {
                let frame_index = self.ch[0].frames_decoded as isize - n as isize;
                let cond = if frame_index <= 0 {
                    CODE_INDEPENDENTLY
                } else if n > 0 && self.prev_decode_only_middle {
                    CODE_INDEPENDENTLY_NO_LTP_SCALING
                } else {
                    CODE_CONDITIONALLY
                };
                n_out_dec = self.ch[n].decode_frame(ec.as_deref_mut(), &mut tmp[n][2..], lost, cond);
            } else {
                tmp[n][2..2 + n_out_dec].fill(0);
            }
            self.ch[n].frames_decoded += 1;
        }
        let [t0, t1] = &mut tmp;
        if nca == 2 && nci == 2 {
            stereo_ms_to_lr(&mut self.stereo, t0, t1, &ms_pred_q13, self.ch[0].fs_khz as usize, n_out_dec);
        } else {
            t0[..2].copy_from_slice(&self.stereo.s_mid);
            self.stereo.s_mid.copy_from_slice(&t0[n_out_dec..n_out_dec + 2]);
        }
        let n_out = n_out_dec * 48 / self.ch[0].fs_khz as usize;
        let mut res = [0i16; 48 * 20];
        for n in 0..nca.min(nci) {
            self.ch[n].resampler.run(&mut res, &tmp[n][1..1 + n_out_dec]);
            for i in 0..n_out {
                out[n + nca * i] = res[i];
            }
        }
        if nca == 2 && nci == 1 {
            if stereo_to_mono {
                self.ch[1].resampler.run(&mut res, &tmp[0][1..1 + n_out_dec]);
                for i in 0..n_out {
                    out[1 + 2 * i] = res[i];
                }
            } else {
                for i in 0..n_out {
                    out[1 + 2 * i] = out[2 * i];
                }
            }
        }
        if lost {
            for c in self.ch.iter_mut().take(self.channels_internal) {
                c.last_gain_index = 10;
            }
        } else {
            self.prev_decode_only_middle = decode_only_middle;
        }
        Ok(n_out)
    }
}
