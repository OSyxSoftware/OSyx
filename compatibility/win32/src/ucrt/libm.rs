//! The rest of the math contract: trigonometric and inverse trigonometric
//! functions, hyperbolic functions and their inverses, log1p, expm1, log2,
//! exp2, cbrt, hypot, fma, remainder, nextafter, the rounding family (rint
//! and nearbyint follow the MXCSR rounding mode of the calling thread) and
//! the float variants.
//!
//! As in math.rs, results are evaluated in double-double arithmetic and
//! rounded once. Trigonometric arguments are reduced modulo pi/2 exactly
//! with 1280 bits of 2/pi (Payne-Hanek), so huge arguments are accurate
//! too. fma uses round-to-odd (Boldo and Melquiond) and is correctly
//! rounded except where the exact product underflows. The float variants
//! are computed in double and rounded to float.

use super::math::{
    add, consts, copysign_soft, dd, div, error, exp_dd, fabs_soft, fmod_soft, is_int, ln_dd, mul, neg, quick_two_sum,
    round_half_even, scalbn as scalb, scale, sqrt_soft, sub, trunc_f, two_prod, two_sum, Dd, DOMAIN, IND, OVERFLOW,
    SING, UNDERFLOW,
};
use super::*;

const PI: Dd = Dd { hi: core::f64::consts::PI, lo: 1.224_646_799_147_353_2e-16 };
const PI_2: Dd = Dd { hi: core::f64::consts::FRAC_PI_2, lo: 6.123_233_995_736_766e-17 };

/// 2/pi as a binary fraction, 64 bits per word.
const TWO_OVER_PI: [u64; 20] = [
    0xa2f9836e4e441529,
    0xfc2757d1f534ddc0,
    0xdb6295993c439041,
    0xfe5163abdebbc561,
    0xb7246e3a424dd2e0,
    0x06492eea09d1921c,
    0xfe1deb1cb129a73e,
    0xe88235f52ebb4484,
    0xe99c7026b45f7e41,
    0x3991d639835339f4,
    0x9c845f8bbdf9283b,
    0x1ff897ffde05980f,
    0xef2f118b5a0a6d1f,
    0x6d367ecf27cb09b7,
    0x4f463f669e5fea2d,
    0x7527bac7ebe5f17b,
    0x3d0739f78a5292ea,
    0x6bfb5fb11f8d5d08,
    0x56033046fc7b6bab,
    0xf0cfbc209af4361d,
];

/// 64 bits of 2/pi starting at bit `i` (bit 1 is the 2^-1 place);
/// positions before bit 1 are zero.
fn pio2_bits(i: i64) -> u64 {
    let bit = |k: i64| -> u64 {
        if k < 1 {
            return 0;
        }
        let k = (k - 1) as usize;
        TWO_OVER_PI.get(k / 64).map_or(0, |w| (w >> (63 - k % 64)) & 1)
    };
    // Whole words when aligned, bit by bit otherwise (small tables, rare calls).
    (0..64).fold(0u64, |a, j| a << 1 | bit(i + j))
}

/// |x| (finite, nonzero) as n + r quarter turns with r in [-1/2, 1/2):
/// returns n mod 4 and r * pi/2 as a double-double.
fn reduce(x: f64) -> (u32, Dd) {
    let bits = x.to_bits() & !(1 << 63);
    let (m, e) = if bits >> 52 == 0 {
        (bits, -1074i64) // subnormal
    } else {
        ((bits & ((1 << 52) - 1)) | (1 << 52), ((bits >> 52) as i64) - 1075)
    };
    // x * 2/pi = m 2^e sum b_i 2^-i. Bits i <= e - 2 only add multiples of
    // 4, so take the 192-bit window W of bits e - 1 .. e + 190: then
    // x * 2/pi mod 4 = (m W mod 2^192) / 2^190.
    let i0 = e - 1;
    let w = [pio2_bits(i0), pio2_bits(i0 + 64), pio2_bits(i0 + 128)];
    let p2 = (m as u128) * (w[2] as u128);
    let p1 = (m as u128) * (w[1] as u128) + (p2 >> 64);
    let p0 = (m as u128) * (w[0] as u128) + (p1 >> 64);
    let mut v = [p0 as u64, p1 as u64, p2 as u64];
    let mut n = (v[0] >> 62) as u32;
    v[0] &= (1 << 62) - 1;
    // The fraction f = v / 2^190 in [0, 1); [1/2, 1) becomes f - 1.
    let negative = v[0] >> 61 == 1;
    if negative {
        n = (n + 1) & 3;
        // 2^190 - v, with v = hi 2^64 + lo and 2^190 = 2^126 2^64.
        let hi = (v[0] as u128) << 64 | v[1] as u128;
        let (lo, borrow) = 0u64.overflowing_sub(v[2]);
        let hi = (1u128 << 126) - hi - borrow as u128;
        v = [(hi >> 64) as u64, hi as u64, lo];
    }
    // Normalise (precision must survive cancellation near multiples of
    // pi/2): top holds the 128 bits below the leading one of the 192-bit
    // value U, so U = top 2^(64 - lz) and f = U / 2^190.
    let hi = (v[0] as u128) << 64 | v[1] as u128;
    let lo = v[2];
    let (top, lz) = if hi != 0 {
        let lz = hi.leading_zeros();
        let low = if lz == 0 {
            0
        } else if lz < 64 {
            (lo >> (64 - lz)) as u128
        } else {
            (lo as u128) << (lz - 64)
        };
        (hi << lz | low, lz)
    } else {
        let z = lo.leading_zeros();
        ((lo as u128) << (64 + z), 128 + z)
    };
    let a = (top >> 75) as f64;
    let b = (top & ((1u128 << 75) - 1)) as f64;
    let f = scale_wide(add(scale_wide(dd(a), 75), dd(b)), 64 - lz as i32 - 190);
    let r = mul(f, PI_2);
    (n, if negative { neg(r) } else { r })
}

/// a * 2^k for any k (in steps the exponent field can hold).
fn scale_wide(a: Dd, k: i32) -> Dd {
    let mut a = a;
    let mut k = k;
    while k > 1000 {
        a = scale(a, 1000);
        k -= 1000;
    }
    while k < -1000 {
        a = scale(a, -1000);
        k += 1000;
    }
    scale(a, k)
}
/// sin(r) and cos(r) for |r| <= pi/4 (Taylor series in double-double).
fn sin_cos_kernel(r: Dd) -> (Dd, Dd) {
    let r2 = mul(r, r);
    let (mut s, mut c) = (r, dd(1.0));
    let (mut ts, mut tc) = (r, dd(1.0));
    for k in 1..=14u32 {
        ts = neg(div(mul(ts, r2), dd(((2 * k) * (2 * k + 1)) as f64)));
        tc = neg(div(mul(tc, r2), dd(((2 * k - 1) * (2 * k)) as f64)));
        s = add(s, ts);
        c = add(c, tc);
    }
    (s, c)
}

/// (sin x, cos x) as double-doubles for finite x.
fn sin_cos(x: f64) -> (Dd, Dd) {
    let ax = fabs_soft(x);
    let (n, r) = if ax <= core::f64::consts::FRAC_PI_4 { (0, dd(ax)) } else { reduce(ax) };
    let (s, c) = sin_cos_kernel(r);
    let (s, c) = match n {
        0 => (s, c),
        1 => (c, neg(s)),
        2 => (neg(s), neg(c)),
        _ => (neg(c), s),
    };
    (if x < 0.0 { neg(s) } else { s }, c)
}

fn round(d: Dd) -> f64 {
    d.hi + d.lo
}

fn sin_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    if x.is_infinite() {
        return error(DOMAIN, b"sin\0", x, 0.0, IND);
    }
    round(sin_cos(x).0)
}

fn cos_f(x: f64) -> f64 {
    if x.is_nan() {
        return x + x;
    }
    if x.is_infinite() {
        return error(DOMAIN, b"cos\0", x, 0.0, IND);
    }
    round(sin_cos(x).1)
}

fn tan_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    if x.is_infinite() {
        return error(DOMAIN, b"tan\0", x, 0.0, IND);
    }
    let (s, c) = sin_cos(x);
    round(div(s, c))
}

// ---- inverse trigonometric -------------------------------------------------------------

fn sqrt_dd(a: Dd) -> Dd {
    if a.hi <= 0.0 {
        return dd(0.0);
    }
    let s = sqrt_soft(a.hi);
    let e = sub(a, two_prod(s, s));
    quick_two_sum(s, e.hi / (2.0 * s))
}

/// atan(t) for t >= 0 (a double-double, possibly infinite).
fn atan_dd(t: Dd) -> Dd {
    if t.hi.is_infinite() {
        return PI_2;
    }
    if t.hi > 1.0 {
        return sub(PI_2, atan_dd(div(dd(1.0), t)));
    }
    // Four halvings: atan(t) = 2 atan(t / (1 + sqrt(1 + t^2))).
    let mut u = t;
    for _ in 0..4 {
        u = div(u, add(dd(1.0), sqrt_dd(add(dd(1.0), mul(u, u)))));
    }
    let u2 = mul(u, u);
    let mut sum = dd(0.0);
    for k in (0..16u32).rev() {
        let c = div(dd(1.0), dd((2 * k + 1) as f64));
        sum = add(if k % 2 == 0 { c } else { neg(c) }, mul(sum, u2));
    }
    scale(mul(u, sum), 4)
}

fn atan_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    let r = round(atan_dd(dd(fabs_soft(x))));
    copysign_soft(r, x)
}

fn atan2_f(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let pi = PI.hi;
    if y == 0.0 {
        return if x.is_sign_negative() { copysign_soft(pi, y) } else { y };
    }
    if x == 0.0 {
        return copysign_soft(PI_2.hi, y);
    }
    if x.is_infinite() {
        return if y.is_infinite() {
            copysign_soft(if x > 0.0 { PI.hi / 4.0 } else { 3.0 * PI.hi / 4.0 }, y)
        } else if x > 0.0 {
            copysign_soft(0.0, y)
        } else {
            copysign_soft(pi, y)
        };
    }
    if y.is_infinite() {
        return copysign_soft(PI_2.hi, y);
    }
    let (ay, ax) = (fabs_soft(y), fabs_soft(x));
    // |y/x| = qy/qx 2^d with qy, qx in [1, 2): divide the scaled values in
    // double-double so neither the quotient nor its error term over- or
    // underflows.
    let ey = ((ay.to_bits() >> 52) as i32).max(1) - 1023;
    let ex = ((ax.to_bits() >> 52) as i32).max(1) - 1023;
    let d = ey - ex;
    let mut a = if d > 60 {
        PI_2 // atan of a quotient above 2^60 rounds to pi/2
    } else if d < -1100 {
        dd(0.0)
    } else {
        let q = div(dd(scalb(ay, -ey)), dd(scalb(ax, -ex)));
        if d < -60 {
            dd(scalb(q.hi, d))
        } else {
            atan_dd(scale(q, d))
        }
    };
    if x < 0.0 {
        a = sub(PI, a);
    }
    copysign_soft(round(a), y)
}
fn asin_f(x: f64) -> f64 {
    if x.is_nan() || x == 0.0 {
        return x + x;
    }
    let ax = fabs_soft(x);
    if ax > 1.0 {
        return error(DOMAIN, b"asin\0", x, 0.0, IND);
    }
    if ax == 1.0 {
        return copysign_soft(PI_2.hi, x);
    }
    let w = sqrt_dd(mul(two_sum(1.0, -ax), two_sum(1.0, ax)));
    copysign_soft(round(atan_dd(div(dd(ax), w))), x)
}

fn acos_f(x: f64) -> f64 {
    if x.is_nan() {
        return x + x;
    }
    let ax = fabs_soft(x);
    if ax > 1.0 {
        return error(DOMAIN, b"acos\0", x, 0.0, IND);
    }
    if x == 1.0 {
        return 0.0;
    }
    if x == -1.0 {
        return PI.hi;
    }
    if x == 0.0 {
        return PI_2.hi;
    }
    let w = sqrt_dd(mul(two_sum(1.0, -ax), two_sum(1.0, ax)));
    let a = atan_dd(div(w, dd(ax)));
    round(if x < 0.0 { sub(PI, a) } else { a })
}

// ---- exponentials and logarithms ---------------------------------------------------------

/// e^x as a double-double for |x| <= 709.
fn exp_full(x: Dd) -> Dd {
    let (ln2, _, _) = consts();
    let n = round_half_even(x.hi / ln2.hi);
    let r = scale(sub(x, mul(ln2, dd(n))), -8);
    let mut sum = dd(1.0);
    for k in (1..=14).rev() {
        sum = add(dd(1.0), div(mul(sum, r), dd(k as f64)));
    }
    for _ in 0..8 {
        sum = mul(sum, sum);
    }
    scale(sum, n as i32)
}

/// e^x - 1 as a double-double for |x| <= 709.
fn expm1_dd(x: Dd) -> Dd {
    if fabs_soft(x.hi) < 0.5 {
        let mut term = x;
        let mut sum = x;
        for k in 2..=30 {
            term = div(mul(term, x), dd(k as f64));
            sum = add(sum, term);
        }
        return sum;
    }
    sub(exp_full(x), dd(1.0))
}

fn expm1_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    if x.is_infinite() {
        return if x > 0.0 { x } else { -1.0 };
    }
    if x > 709.0 {
        let (ln2, _, _) = consts();
        let r = exp_dd(dd(x), ln2);
        if r.is_infinite() {
            return error(OVERFLOW, b"expm1\0", x, 0.0, r);
        }
        return r;
    }
    if x < -40.0 {
        return -1.0;
    }
    round(expm1_dd(dd(x)))
}

fn log1p_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    if x < -1.0 {
        return error(DOMAIN, b"log1p\0", x, 0.0, IND);
    }
    if x == -1.0 {
        return error(SING, b"log1p\0", x, 0.0, f64::NEG_INFINITY);
    }
    if x.is_infinite() {
        return x;
    }
    let (ln2, _, _) = consts();
    round(ln_dd(two_sum(1.0, x), ln2))
}

fn log2_f(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 0.0 {
        return error(DOMAIN, b"log2\0", x, 0.0, IND);
    }
    if x == 0.0 {
        return error(SING, b"log2\0", x, 0.0, f64::NEG_INFINITY);
    }
    if x.is_infinite() {
        return x;
    }
    let (ln2, _, _) = consts();
    round(div(ln_dd(dd(x), ln2), ln2))
}

fn exp2_f(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return if x > 0.0 { x } else { 0.0 };
    }
    if x >= 1024.0 {
        return error(OVERFLOW, b"exp2\0", x, 0.0, f64::INFINITY);
    }
    if x < -1075.0 {
        return error(UNDERFLOW, b"exp2\0", x, 0.0, 0.0);
    }
    if is_int(x) {
        return scalb(1.0, x as i32);
    }
    let (ln2, _, _) = consts();
    let r = exp_dd(mul(dd(x), ln2), ln2);
    if r == 0.0 {
        return error(UNDERFLOW, b"exp2\0", x, 0.0, r);
    }
    r
}

fn cbrt_f(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x + x;
    }
    let ax = fabs_soft(x);
    let (ln2, _, _) = consts();
    let y0 = exp_dd(div(ln_dd(dd(ax), ln2), dd(3.0)), ln2);
    // One Newton step in double-double: y - (y^3 - a) / (3 y^2).
    let y = dd(y0);
    let y2 = mul(y, y);
    let y = sub(y, div(sub(mul(y2, y), dd(ax)), mul(dd(3.0), y2)));
    copysign_soft(round(y), x)
}

fn hypot_f(x: f64, y: f64) -> f64 {
    if x.is_infinite() || y.is_infinite() {
        return f64::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let (a, b) = {
        let (a, b) = (fabs_soft(x), fabs_soft(y));
        if a >= b {
            (a, b)
        } else {
            (b, a)
        }
    };
    if b == 0.0 {
        return a;
    }
    let e = ((a.to_bits() >> 52) as i32).max(1) - 1023;
    let (sa, sb) = (scalb(a, -e), scalb(b, -e));
    let s = sqrt_dd(add(two_prod(sa, sa), two_prod(sb, sb)));
    let r = scalb(round(s), e);
    if r.is_infinite() {
        return error(OVERFLOW, b"hypot\0", x, y, r);
    }
    r
}

// ---- hyperbolic ---------------------------------------------------------------------------

fn sinh_f(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x + x;
    }
    let ax = fabs_soft(x);
    let r = if ax < 1.0 {
        let t = expm1_dd(dd(ax));
        scale(add(t, div(t, add(t, dd(1.0)))), -1)
    } else if ax < 20.0 {
        let e = exp_full(dd(ax));
        scale(sub(e, div(dd(1.0), e)), -1)
    } else {
        // Beyond 20, exp(-ax)/2 is below half an ulp of the result.
        // Avoid dividing by a huge double-double: Dekker splitting that
        // denominator would overflow well before sinh itself does.
        let (ln2, _, _) = consts();
        let v = exp_dd(sub(dd(ax), ln2), ln2);
        if v.is_infinite() {
            return error(OVERFLOW, b"sinh\0", x, 0.0, copysign_soft(v, x));
        }
        dd(v)
    };
    copysign_soft(round(r), x)
}

fn cosh_f(x: f64) -> f64 {
    if x.is_nan() {
        return x + x;
    }
    let ax = fabs_soft(x);
    if ax.is_infinite() {
        return ax;
    }
    let r = if ax < 1.0 {
        let t = expm1_dd(dd(ax));
        add(dd(1.0), div(mul(t, t), scale(add(t, dd(1.0)), 1)))
    } else if ax < 20.0 {
        let e = exp_full(dd(ax));
        scale(add(e, div(dd(1.0), e)), -1)
    } else {
        let (ln2, _, _) = consts();
        let v = exp_dd(sub(dd(ax), ln2), ln2);
        if v.is_infinite() {
            return error(OVERFLOW, b"cosh\0", x, 0.0, v);
        }
        dd(v)
    };
    round(r)
}

fn tanh_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    let ax = fabs_soft(x);
    if ax > 22.0 {
        return copysign_soft(1.0, x);
    }
    let t = expm1_dd(dd(2.0 * ax));
    copysign_soft(round(div(t, add(t, dd(2.0)))), x)
}

fn asinh_f(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x + x;
    }
    let ax = fabs_soft(x);
    let (ln2, _, _) = consts();
    let r = if ax > 268_435_456.0 {
        add(ln_dd(dd(ax), ln2), ln2)
    } else {
        ln_dd(add(dd(ax), sqrt_dd(add(two_prod(ax, ax), dd(1.0)))), ln2)
    };
    copysign_soft(round(r), x)
}

fn acosh_f(x: f64) -> f64 {
    if x.is_nan() {
        return x + x;
    }
    if x < 1.0 {
        return error(DOMAIN, b"acosh\0", x, 0.0, IND);
    }
    if x == 1.0 {
        return 0.0;
    }
    if x.is_infinite() {
        return x;
    }
    let (ln2, _, _) = consts();
    let r = if x > 268_435_456.0 {
        add(ln_dd(dd(x), ln2), ln2)
    } else {
        ln_dd(add(dd(x), sqrt_dd(mul(two_sum(x, -1.0), two_sum(x, 1.0)))), ln2)
    };
    round(r)
}

fn atanh_f(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x + x;
    }
    let ax = fabs_soft(x);
    if ax > 1.0 {
        return error(DOMAIN, b"atanh\0", x, 0.0, IND);
    }
    if ax == 1.0 {
        return error(SING, b"atanh\0", x, 0.0, copysign_soft(f64::INFINITY, x));
    }
    let (ln2, _, _) = consts();
    let q = div(two_sum(1.0, ax), two_sum(1.0, -ax));
    copysign_soft(round(scale(ln_dd(q, ln2), -1)), x)
}

// ---- fma, remainder, nextafter -------------------------------------------------------------

/// a + b rounded to odd: exact sums as they are, otherwise the neighbour
/// of the nearest result that has an odd last bit.
fn add_round_odd(a: f64, b: f64) -> f64 {
    let s = two_sum(a, b);
    if s.lo == 0.0 || s.hi.is_infinite() || s.hi.to_bits() & 1 == 1 {
        return s.hi;
    }
    // Step one unit towards the discarded error.
    let up = (s.lo > 0.0) == (s.hi > 0.0);
    f64::from_bits(if up { s.hi.to_bits() + 1 } else { s.hi.to_bits() - 1 })
}

fn fma_f(x: f64, y: f64, z: f64) -> f64 {
    if !x.is_finite() || !y.is_finite() || !z.is_finite() || x == 0.0 || y == 0.0 {
        return x * y + z;
    }
    let p = two_prod(x, y);
    if !p.hi.is_finite() {
        return p.hi + z;
    }
    if z == 0.0 {
        return p.hi;
    }
    let t = two_sum(z, p.hi);
    let v = add_round_odd(t.lo, p.lo);
    t.hi + v
}

fn fmaf_f(x: f32, y: f32, z: f32) -> f32 {
    // x * y is exact in double; round the exact sum to odd at 53 bits, then
    // once more to float (53 >= 24 + 2 makes that correct).
    let p = x as f64 * y as f64;
    if !p.is_finite() || !(z as f64).is_finite() {
        return (p + z as f64) as f32;
    }
    add_round_odd(p, z as f64) as f32
}

fn remainder_f(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x.is_infinite() || y == 0.0 {
        return error(DOMAIN, b"remainder\0", x, y, IND);
    }
    if y.is_infinite() {
        return x;
    }
    let p = fabs_soft(y);
    let mut r = x;
    if p <= f64::MAX / 2.0 {
        r = fmod_soft(r, p + p); // now |r| < 2p
    }
    if fabs_soft(r) == p {
        return 0.0 * x;
    }
    let mut a = fabs_soft(r);
    if p < 2.0 * f64::MIN_POSITIVE {
        if a + a > p {
            a -= p;
            if a + a >= p {
                a -= p;
            }
        }
    } else {
        let half = 0.5 * p;
        if a > half {
            a -= p;
            if a >= half {
                a -= p;
            }
        }
    }
    if r < 0.0 {
        -a
    } else {
        a
    }
}

fn next_after(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        return copysign_soft(f64::from_bits(1), y);
    }
    let b = x.to_bits();
    let r = f64::from_bits(if (y > x) == (x > 0.0) { b + 1 } else { b - 1 });
    if r.is_infinite() {
        set_errno(ERANGE);
    }
    r
}

fn next_after_f(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        return f32::from_bits(1 | (y.to_bits() & (1 << 31)));
    }
    let b = x.to_bits();
    let r = f32::from_bits(if (y > x) == (x > 0.0) { b + 1 } else { b - 1 });
    if r.is_infinite() {
        set_errno(ERANGE);
    }
    r
}

// ---- rounding --------------------------------------------------------------------------

/// The calling thread's MXCSR rounding mode: 0 nearest, 1 down, 2 up, 3 toward zero.
fn rounding_mode() -> u32 {
    let mut csr: u32 = 0;
    // SAFETY: stores this thread's MXCSR into a local.
    unsafe { core::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack)) };
    (csr >> 13) & 3
}

fn rint_mode(x: f64, mode: u32) -> f64 {
    if !x.is_finite() || fabs_soft(x) >= 4_503_599_627_370_496.0 {
        return x;
    }
    let t = trunc_f(x);
    let r = match mode {
        0 => round_half_even(x),
        1 => {
            if t > x {
                t - 1.0
            } else {
                t
            }
        }
        2 => {
            if t < x {
                t + 1.0
            } else {
                t
            }
        }
        _ => t,
    };
    copysign_soft(r, x) // keeps the sign of zero results
}

fn round_away(x: f64) -> f64 {
    let t = trunc_f(x);
    if fabs_soft(x - t) >= 0.5 {
        t + copysign_soft(1.0, x)
    } else {
        t
    }
}

/// Converts an integral double to a 32-bit long; out of range gives
/// LONG_MIN and EDOM-free invalid behaviour, as the UCRT.
fn to_long(r: f64) -> i32 {
    if r.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&r) {
        i32::MIN
    } else {
        r as i32
    }
}

fn to_llong(r: f64) -> i64 {
    if r.is_nan() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&r) {
        i64::MIN
    } else {
        r as i64
    }
}

// ---- exports ----------------------------------------------------------------------------

macro_rules! unary {
    ($($s:ident = $f:ident;)*) => {$(
        extern "win64" fn $s(x: f64) -> f64 {
            $f(x)
        }
    )*};
}

macro_rules! unary_f {
    ($($s:ident = $f:ident, $name:literal;)*) => {$(
        extern "win64" fn $s(x: f32) -> f32 {
            let r = $f(x as f64);
            let f = r as f32;
            if f.is_infinite() && r.is_finite() {
                return error(OVERFLOW, $name, x as f64, 0.0, f as f64) as f32;
            }
            f
        }
    )*};
}

unary! {
    sin_soft = sin_f; cos_soft = cos_f; tan_soft = tan_f; asin_soft = asin_f; acos_soft = acos_f; atan_soft = atan_f;
    sinh_soft = sinh_f; cosh_soft = cosh_f; tanh_soft = tanh_f; asinh_soft = asinh_f; acosh_soft = acosh_f;
    atanh_soft = atanh_f; expm1_soft = expm1_f; log1p_soft = log1p_f; log2_soft = log2_f; exp2_soft = exp2_f;
    cbrt_soft = cbrt_f;
}

fn exp_of(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return if x > 0.0 { x } else { 0.0 };
    }
    let (ln2, _, _) = consts();
    exp_dd(dd(x), ln2)
}

fn sqrt_of(x: f64) -> f64 {
    if x < 0.0 {
        return IND;
    }
    sqrt_soft(x)
}

unary_f! {
    sinf_soft = sin_f, b"sinf\0"; cosf_soft = cos_f, b"cosf\0"; tanf_soft = tan_f, b"tanf\0";
    asinf_soft = asin_f, b"asinf\0"; acosf_soft = acos_f, b"acosf\0"; atanf_soft = atan_f, b"atanf\0";
    sinhf_soft = sinh_f, b"sinhf\0"; coshf_soft = cosh_f, b"coshf\0"; tanhf_soft = tanh_f, b"tanhf\0";
    asinhf_soft = asinh_f, b"asinhf\0"; acoshf_soft = acosh_f, b"acoshf\0"; atanhf_soft = atanh_f, b"atanhf\0";
    log1pf_soft = log1p_f, b"log1pf\0"; log2f_soft = log2_f, b"log2f\0"; exp2f_soft = exp2_f, b"exp2f\0";
    cbrtf_soft = cbrt_f, b"cbrtf\0"; expf_soft = exp_of, b"expf\0";
}

extern "win64" fn sqrtf_soft(x: f32) -> f32 {
    if x < 0.0 {
        return error(DOMAIN, b"sqrtf\0", x as f64, 0.0, IND) as f32;
    }
    sqrt_of(x as f64) as f32
}

extern "win64" fn atan2_soft(y: f64, x: f64) -> f64 {
    atan2_f(y, x)
}

extern "win64" fn atan2f_soft(y: f32, x: f32) -> f32 {
    atan2_f(y as f64, x as f64) as f32
}

extern "win64" fn hypot_soft(x: f64, y: f64) -> f64 {
    hypot_f(x, y)
}

extern "win64" fn hypotf_soft(x: f32, y: f32) -> f32 {
    let r = hypot_f(x as f64, y as f64);
    let f = r as f32;
    if f.is_infinite() && r.is_finite() {
        return error(OVERFLOW, b"hypotf\0", x as f64, y as f64, f as f64) as f32;
    }
    f
}

extern "win64" fn fma_soft(x: f64, y: f64, z: f64) -> f64 {
    fma_f(x, y, z)
}

extern "win64" fn fmaf_soft(x: f32, y: f32, z: f32) -> f32 {
    fmaf_f(x, y, z)
}

extern "win64" fn fmodf_soft(x: f32, y: f32) -> f32 {
    fmod_soft(x as f64, y as f64) as f32
}

extern "win64" fn remainder_soft(x: f64, y: f64) -> f64 {
    remainder_f(x, y)
}

extern "win64" fn nextafter_soft(x: f64, y: f64) -> f64 {
    next_after(x, y)
}

extern "win64" fn nextafterf_soft(x: f32, y: f32) -> f32 {
    next_after_f(x, y)
}

extern "win64" fn rint_soft(x: f64) -> f64 {
    rint_mode(x, rounding_mode())
}

extern "win64" fn rintf_soft(x: f32) -> f32 {
    rint_mode(x as f64, rounding_mode()) as f32
}

extern "win64" fn lrint_soft(x: f64) -> i32 {
    to_long(rint_mode(x, rounding_mode()))
}

extern "win64" fn lrintf_soft(x: f32) -> i32 {
    to_long(rint_mode(x as f64, rounding_mode()))
}

extern "win64" fn llrint_soft(x: f64) -> i64 {
    to_llong(rint_mode(x, rounding_mode()))
}

extern "win64" fn llrintf_soft(x: f32) -> i64 {
    to_llong(rint_mode(x as f64, rounding_mode()))
}

extern "win64" fn lround_soft(x: f64) -> i32 {
    to_long(round_away(x))
}

extern "win64" fn lroundf_soft(x: f32) -> i32 {
    to_long(round_away(x as f64))
}

extern "win64" fn llround_soft(x: f64) -> i64 {
    to_llong(round_away(x))
}

extern "win64" fn roundf_soft(x: f32) -> f32 {
    if !x.is_finite() {
        return x;
    }
    copysign_soft(round_away(x as f64), x as f64) as f32
}

extern "win64" fn modf_soft(x: f64, ip: *mut f64) -> f64 {
    let (i, f) = if x.is_nan() {
        (x, x)
    } else if x.is_infinite() {
        (x, copysign_soft(0.0, x))
    } else {
        let t = trunc_f(x);
        (t, copysign_soft(x - t, x))
    };
    if !ip.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *ip = i };
    }
    f
}

extern "win64" fn modff_soft(x: f32, ip: *mut f32) -> f32 {
    let mut i = 0.0f64;
    let f = modf_soft(x as f64, &mut i);
    if !ip.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *ip = i as f32 };
    }
    f as f32
}

extern "win64" fn scalbn_soft(x: f64, n: i32) -> f64 {
    let r = scalb(x, n);
    if r.is_infinite() && x.is_finite() {
        return error(OVERFLOW, b"scalbn\0", x, n as f64, r);
    }
    r
}

extern "win64" fn scalbnf_soft(x: f32, n: i32) -> f32 {
    let r = scalb(x as f64, n.clamp(-2000, 2000)) as f32;
    if r.is_infinite() && x.is_finite() {
        return error(OVERFLOW, b"scalbnf\0", x as f64, n as f64, r as f64) as f32;
    }
    r
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![
        sin, cos, tan, asin, acos, atan, atan2, sinh, cosh, tanh, asinh, acosh, atanh, expm1, log1p, log2, exp2, cbrt,
        hypot, fma, remainder, nextafter, rint, nearbyint, lrint, llrint, llrintf, lround, llround, modf, scalbn, sinf,
        cosf, tanf, asinf, acosf, atanf, atan2f, sinhf, coshf, tanhf, asinhf, acoshf, atanhf, log1pf, log2f, exp2f,
        cbrtf, expf, sqrtf, hypotf, fmaf, fmodf, nextafterf, rintf, nearbyintf, lrintf, lroundf, roundf, modff,
        scalbnf,
    ];
    v.extend([
        ("_hypot", hypot as *const () as u64),
        ("_hypotf", hypotf as *const () as u64),
        ("nexttoward", nextafter as *const () as u64), // long double is double
        ("_nextafter", nextafter as *const () as u64),
    ]);
    v
}

xmm_abi! {
    sin => sin_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    cos => cos_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    tan => tan_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    asin => asin_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    acos => acos_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    atan => atan_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    atan2 => atan2_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    sinh => sinh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    cosh => cosh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    tanh => tanh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    asinh => asinh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    acosh => acosh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    atanh => atanh_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    expm1 => expm1_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    log1p => log1p_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    log2 => log2_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    exp2 => exp2_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    cbrt => cbrt_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    hypot => hypot_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    fma => fma_soft ["movq rcx, xmm0", "movq rdx, xmm1", "movq r8, xmm2"] "movq xmm0, rax";
    remainder => remainder_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    nextafter => nextafter_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    rint => rint_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    nearbyint => rint_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    lrint => lrint_soft ["movq rcx, xmm0"] "";
    llrint => llrint_soft ["movq rcx, xmm0"] "";
    llrintf => llrintf_soft ["movd ecx, xmm0"] "";
    lround => lround_soft ["movq rcx, xmm0"] "";
    llround => llround_soft ["movq rcx, xmm0"] "";
    modf => modf_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    scalbn => scalbn_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    sinf => sinf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    cosf => cosf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    tanf => tanf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    asinf => asinf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    acosf => acosf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    atanf => atanf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    atan2f => atan2f_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    sinhf => sinhf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    coshf => coshf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    tanhf => tanhf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    asinhf => asinhf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    acoshf => acoshf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    atanhf => atanhf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    log1pf => log1pf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    log2f => log2f_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    exp2f => exp2f_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    cbrtf => cbrtf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    expf => expf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    sqrtf => sqrtf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    hypotf => hypotf_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    fmaf => fmaf_soft ["movd ecx, xmm0", "movd edx, xmm1", "movd r8d, xmm2"] "movd xmm0, eax";
    fmodf => fmodf_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    nextafterf => nextafterf_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    rintf => rintf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    nearbyintf => rintf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    lrintf => lrintf_soft ["movd ecx, xmm0"] "";
    lroundf => lroundf_soft ["movd ecx, xmm0"] "";
    roundf => roundf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    modff => modff_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    scalbnf => scalbnf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
}
