//! Mathematics (math contract). Logarithms, exponentials and powers are
//! computed in double-double arithmetic (about 104 bits) and rounded
//! once, so exact cases (powers of two and ten, integral powers) come out
//! exact; lgamma uses Taylor series at its zeros 1 and 2, Stirling's
//! series elsewhere and reflection for negative arguments. Domain, pole
//! and range errors set errno (EDOM/ERANGE) or go to the program's
//! `_matherr` installed through `__setusermatherr`.

use super::*;

// ---- double-double arithmetic ---------------------------------------------------------

#[derive(Clone, Copy)]
struct Dd {
    hi: f64,
    lo: f64,
}

fn dd(x: f64) -> Dd {
    Dd { hi: x, lo: 0.0 }
}

fn two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    let bb = s - a;
    Dd { hi: s, lo: (a - (s - bb)) + (b - bb) }
}

fn quick_two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    Dd { hi: s, lo: b - (s - a) }
}

fn split(a: f64) -> (f64, f64) {
    let t = 134_217_729.0 * a; // 2^27 + 1
    let hi = t - (t - a);
    (hi, a - hi)
}

fn two_prod(a: f64, b: f64) -> Dd {
    let p = a * b;
    let (ah, al) = split(a);
    let (bh, bl) = split(b);
    Dd { hi: p, lo: ((ah * bh - p) + ah * bl + al * bh) + al * bl }
}

fn add(a: Dd, b: Dd) -> Dd {
    let s = two_sum(a.hi, b.hi);
    let t = two_sum(a.lo, b.lo);
    let s = quick_two_sum(s.hi, s.lo + t.hi);
    quick_two_sum(s.hi, s.lo + t.lo)
}

fn neg(a: Dd) -> Dd {
    Dd { hi: -a.hi, lo: -a.lo }
}

fn sub(a: Dd, b: Dd) -> Dd {
    add(a, neg(b))
}

fn mul(a: Dd, b: Dd) -> Dd {
    let p = two_prod(a.hi, b.hi);
    quick_two_sum(p.hi, p.lo + (a.hi * b.lo + a.lo * b.hi))
}

fn div(a: Dd, b: Dd) -> Dd {
    let q1 = a.hi / b.hi;
    let r = sub(a, mul(b, dd(q1)));
    let q2 = r.hi / b.hi;
    let r = sub(r, mul(b, dd(q2)));
    let q3 = r.hi / b.hi;
    add(quick_two_sum(q1, q2), dd(q3))
}

/// Multiplies by 2^k exactly (no overflow in the uses here).
fn scale(a: Dd, k: i32) -> Dd {
    let f = pow2(k);
    Dd { hi: a.hi * f, lo: a.lo * f }
}

fn pow2(k: i32) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// 2 atanh(s) = ln((1 + s) / (1 - s)) for |s| <= 0.18 (or 1/3).
fn atanh2(s: Dd) -> Dd {
    let s2 = mul(s, s);
    let mut sum = dd(0.0);
    for n in (0..40).rev() {
        sum = add(mul(sum, s2), div(dd(1.0), dd((2 * n + 1) as f64)));
    }
    scale(mul(s, sum), 1)
}

/// ln 2 and ln 10 (derived once from the series).
fn consts() -> (Dd, Dd, Dd) {
    static C: Mutex<Option<(Dd, Dd, Dd)>> = Mutex::new(None);
    let mut g = C.lock().unwrap();
    *g.get_or_insert_with(|| {
        let ln2 = atanh2(div(dd(1.0), dd(3.0)));
        // ln 10 = 3 ln 2 + ln 1.25, ln 1.25 = 2 atanh(1/9).
        let ln10 = add(mul(ln2, dd(3.0)), atanh2(div(dd(1.0), dd(9.0))));
        // ln(2 pi) with pi as a double-double.
        let pi = Dd { hi: core::f64::consts::PI, lo: 1.224_646_799_147_353_2e-16 };
        let ln2pi = ln_dd(scale(pi, 1), ln2);
        (ln2, ln10, ln2pi)
    })
}

/// ln of a positive finite double-double.
fn ln_dd(x: Dd, ln2: Dd) -> Dd {
    let mut x = x;
    let mut k = 0i32;
    if x.hi < f64::MIN_POSITIVE {
        x = scale(x, 64);
        k -= 64;
    }
    let e = ((x.hi.to_bits() >> 52) & 0x7FF) as i32 - 1023;
    let mut m = scale(x, -e);
    k += e;
    if m.hi > core::f64::consts::SQRT_2 {
        m = scale(m, -1);
        k += 1;
    }
    let s = div(add(m, dd(-1.0)), add(m, dd(1.0)));
    add(mul(ln2, dd(k as f64)), atanh2(s))
}

/// e^x for a double-double argument, rounded to double. Over- and
/// underflow give inf and 0 (the caller reports them).
fn exp_dd(x: Dd, ln2: Dd) -> f64 {
    if x.hi > 709.8 {
        return f64::INFINITY;
    }
    if x.hi < -745.2 {
        return 0.0;
    }
    let n = round_half_even(x.hi / ln2.hi);
    let r = scale(sub(x, mul(ln2, dd(n))), -8);
    let mut sum = dd(1.0);
    for k in (1..=12).rev() {
        sum = add(dd(1.0), div(mul(sum, r), dd(k as f64)));
    }
    for _ in 0..8 {
        sum = mul(sum, sum);
    }
    let n = n as i32;
    let v = sum.hi + sum.lo;
    if n > 1000 {
        v * pow2(1000) * pow2(n - 1000)
    } else if n < -1000 {
        v * pow2(-1000) * pow2(n + 1000)
    } else {
        v * pow2(n)
    }
}

// ---- errors ----------------------------------------------------------------------------

const DOMAIN: i32 = 1;
const SING: i32 = 2;
const OVERFLOW: i32 = 3;
const UNDERFLOW: i32 = 4;

static MATHERR: AtomicU64 = AtomicU64::new(0);

extern "win64" fn __setusermatherr(f: u64) {
    MATHERR.store(f, Ordering::SeqCst)
}

#[repr(C)]
struct Exception {
    kind: i32,
    name: *const u8,
    arg1: f64,
    arg2: f64,
    retval: f64,
}

/// Reports a math error: the program's `_matherr` may take it (and
/// replace the result); otherwise errno is set.
fn error(kind: i32, name: &'static [u8], a1: f64, a2: f64, ret: f64) -> f64 {
    let h = MATHERR.load(Ordering::SeqCst);
    if h != 0 {
        let mut e = Exception { kind, name: name.as_ptr(), arg1: a1, arg2: a2, retval: ret };
        // SAFETY: the `_matherr` the program installed.
        let f: extern "win64" fn(*mut Exception) -> i32 = unsafe { core::mem::transmute(h as usize) };
        if f(&mut e) != 0 {
            return e.retval;
        }
    }
    set_errno(if kind == DOMAIN { EDOM } else { ERANGE });
    ret
}

/// The default NaN of an invalid operation (the "indeterminate" NaN).
const IND: f64 = f64::from_bits(0xFFF8_0000_0000_0000);

// ---- classification and bits ---------------------------------------------------------------

extern "win64" fn _dclass_soft(x: f64) -> i16 {
    if x.is_nan() {
        2
    } else if x.is_infinite() {
        1
    } else if x == 0.0 {
        0
    } else if x.is_subnormal() {
        -2
    } else {
        -1
    }
}

extern "win64" fn _fdclass_soft(x: f32) -> i16 {
    _dclass_soft(x as f64)
}

extern "win64" fn _dsign_soft(x: f64) -> i32 {
    x.is_sign_negative() as i32
}

extern "win64" fn _fdsign_soft(x: f32) -> i32 {
    x.is_sign_negative() as i32
}

extern "win64" fn copysign_soft(x: f64, y: f64) -> f64 {
    f64::from_bits((x.to_bits() & !(1 << 63)) | (y.to_bits() & (1 << 63)))
}

extern "win64" fn copysignf_soft(x: f32, y: f32) -> f32 {
    f32::from_bits((x.to_bits() & !(1 << 31)) | (y.to_bits() & (1 << 31)))
}

extern "win64" fn fabs_soft(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}

/// Rounds toward zero by clearing fraction bits.
fn trunc_f(x: f64) -> f64 {
    let e = ((x.to_bits() >> 52) & 0x7FF) as i32 - 1023;
    if e >= 52 {
        return x; // integral, inf or NaN
    }
    if e < 0 {
        return copysign_soft(0.0, x);
    }
    f64::from_bits(x.to_bits() & !((1u64 << (52 - e)) - 1))
}

fn floor_f(x: f64) -> f64 {
    let t = trunc_f(x);
    if t > x { t - 1.0 } else { t }
}

fn ceil_f(x: f64) -> f64 {
    let t = trunc_f(x);
    if t < x { t + 1.0 } else { t }
}

extern "win64" fn trunc_soft(x: f64) -> f64 {
    trunc_f(x)
}

extern "win64" fn floor_soft(x: f64) -> f64 {
    floor_f(x)
}

extern "win64" fn ceil_soft(x: f64) -> f64 {
    ceil_f(x)
}

extern "win64" fn floorf_soft(x: f32) -> f32 {
    floor_f(x as f64) as f32
}

extern "win64" fn ceilf_soft(x: f32) -> f32 {
    ceil_f(x as f64) as f32
}

extern "win64" fn truncf_soft(x: f32) -> f32 {
    trunc_f(x as f64) as f32
}

/// round: halfway cases away from zero.
extern "win64" fn round_soft(x: f64) -> f64 {
    let t = trunc_f(x);
    if fabs_soft(x - t) >= 0.5 { t + copysign_soft(1.0, x) } else { t }
}

extern "win64" fn sqrt_soft(x: f64) -> f64 {
    if x < 0.0 {
        return error(DOMAIN, b"sqrt\0", x, 0.0, IND);
    }
    use core::arch::x86_64::{_mm_cvtsd_f64, _mm_set_sd, _mm_sqrt_pd};
    // SAFETY: SSE2 is part of x86-64.
    unsafe { _mm_cvtsd_f64(_mm_sqrt_pd(_mm_set_sd(x))) }
}

extern "win64" fn frexp_soft(x: f64, e: *mut i32) -> f64 {
    let put = |v: i32| {
        if !e.is_null() {
            // SAFETY: the caller's out pointer.
            unsafe { *e = v };
        }
    };
    if x.is_nan() || x.is_infinite() {
        put(-1); // what the UCRT stores for these
        return x;
    }
    if x == 0.0 {
        put(0);
        return x;
    }
    let (mut x, mut adj) = (x, 0);
    if x.is_subnormal() {
        x *= pow2(64);
        adj = -64;
    }
    let ex = ((x.to_bits() >> 52) & 0x7FF) as i32 - 1022;
    put(ex + adj);
    f64::from_bits((x.to_bits() & !(0x7FFu64 << 52)) | (1022u64 << 52))
}

/// x * 2^n with a single rounding (subnormal results included).
fn scalbn(x: f64, n: i32) -> f64 {
    let (mut y, mut n) = (x, n);
    if n > 1023 {
        y *= pow2(1023);
        n -= 1023;
        if n > 1023 {
            y *= pow2(1023);
            n = (n - 1023).min(1023);
        }
    } else if n < -1022 {
        // Scale by 2^-1022 * 2^53 so only the last step can round.
        y *= pow2(-1022) * pow2(53);
        n += 1022 - 53;
        if n < -1022 {
            y *= pow2(-1022) * pow2(53);
            n = (n + 1022 - 53).max(-1022);
        }
    }
    y * pow2(n)
}

extern "win64" fn ldexp_soft(x: f64, n: i32) -> f64 {
    let r = scalbn(x, n);
    if r.is_infinite() && x.is_finite() {
        return error(OVERFLOW, b"ldexp\0", x, n as f64, r);
    }
    r
}

/// fmod: exact (the remainder of doubles is representable).
extern "win64" fn fmod_soft(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x.is_infinite() || y == 0.0 {
        return error(DOMAIN, b"fmod\0", x, y, IND);
    }
    if y.is_infinite() || x == 0.0 {
        return x;
    }
    let (ax, ay) = (fabs_soft(x), fabs_soft(y));
    if ax < ay {
        return x;
    }
    // Long division on the binary representations: subtract the largest
    // ay * 2^k not above r (exact) until r < ay.
    let mut r = ax;
    let (_, ey) = frexp_parts(ay);
    while r >= ay {
        let (_, er) = frexp_parts(r);
        let mut t = scalbn(ay, er - ey);
        if t > r {
            t = scalbn(ay, er - ey - 1);
        }
        r -= t;
    }
    copysign_soft(r, x)
}

fn frexp_parts(x: f64) -> (f64, i32) {
    let mut e = 0;
    let m = frexp_soft(x, &mut e);
    (m, e)
}

// ---- logarithms and powers ---------------------------------------------------------------

fn log_common(x: f64, name: &'static [u8], base10: bool) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 0.0 {
        return error(DOMAIN, name, x, 0.0, IND);
    }
    if x == 0.0 {
        return error(SING, name, x, 0.0, f64::NEG_INFINITY);
    }
    if x.is_infinite() {
        return x;
    }
    let (ln2, ln10, _) = consts();
    let l = ln_dd(dd(x), ln2);
    let r = if base10 { div(l, ln10) } else { l };
    r.hi + r.lo
}

extern "win64" fn log10_soft(x: f64) -> f64 {
    log_common(x, b"log10\0", true)
}

extern "win64" fn log_soft(x: f64) -> f64 {
    log_common(x, b"log\0", false)
}

extern "win64" fn log10f_soft(x: f32) -> f32 {
    log10_soft(x as f64) as f32
}

extern "win64" fn logf_soft(x: f32) -> f32 {
    log_soft(x as f64) as f32
}

extern "win64" fn exp_soft(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return if x > 0.0 { x } else { 0.0 };
    }
    let (ln2, _, _) = consts();
    let r = exp_dd(dd(x), ln2);
    if r.is_infinite() {
        return error(OVERFLOW, b"exp\0", x, 0.0, r);
    }
    if r == 0.0 {
        return error(UNDERFLOW, b"exp\0", x, 0.0, r);
    }
    r
}

fn is_int(y: f64) -> bool {
    y.is_finite() && trunc_f(y) == y
}

fn is_odd_int(y: f64) -> bool {
    is_int(y) && fabs_soft(y) < 9_007_199_254_740_992.0 && (y as i64) & 1 == 1
}

fn pow_f(x: f64, y: f64) -> f64 {
    const NAME: &[u8] = b"pow\0";
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == 0.0 {
        let odd = is_odd_int(y);
        return if y < 0.0 {
            error(SING, NAME, x, y, if odd { copysign_soft(f64::INFINITY, x) } else { f64::INFINITY })
        } else if odd {
            x
        } else {
            0.0
        };
    }
    if y.is_infinite() {
        let ax = fabs_soft(x);
        return if ax == 1.0 {
            1.0
        } else if (ax < 1.0) == (y < 0.0) {
            f64::INFINITY
        } else {
            0.0
        };
    }
    if x.is_infinite() {
        let odd = is_odd_int(y);
        let mag = if y < 0.0 { 0.0 } else { f64::INFINITY };
        return if x < 0.0 && odd { -mag } else { mag };
    }
    let mut sign = 1.0;
    if x < 0.0 {
        if !is_int(y) {
            return error(DOMAIN, NAME, x, y, IND);
        }
        if is_odd_int(y) {
            sign = -1.0;
        }
    }
    let (ln2, _, _) = consts();
    let l = ln_dd(dd(fabs_soft(x)), ln2);
    let t = mul(l, dd(y));
    let r = exp_dd(t, ln2);
    if r.is_infinite() {
        return error(OVERFLOW, NAME, x, y, sign * r);
    }
    if r == 0.0 {
        return error(UNDERFLOW, NAME, x, y, sign * r);
    }
    sign * r
}

extern "win64" fn pow_soft(x: f64, y: f64) -> f64 {
    pow_f(x, y)
}

extern "win64" fn powf_soft(x: f32, y: f32) -> f32 {
    let r = pow_f(x as f64, y as f64);
    let f = r as f32;
    if f.is_infinite() && r.is_finite() {
        return error(OVERFLOW, b"powf\0", x as f64, y as f64, f as f64) as f32;
    }
    f
}

// ---- lgamma ------------------------------------------------------------------------------

/// ζ(k) for k = 2..=9; higher values are summed directly.
const ZETA: [f64; 8] = [
    1.644_934_066_848_226_4,
    1.202_056_903_159_594_3,
    1.082_323_233_711_138_2,
    1.036_927_755_143_37,
    1.017_343_061_984_449,
    1.008_349_277_381_922_8,
    1.004_077_356_197_944_3,
    1.002_008_392_826_082_2,
];
const EULER_GAMMA: f64 = core::f64::consts::EULER_GAMMA;

fn zeta(k: u32) -> f64 {
    if k <= 9 {
        return ZETA[k as usize - 2];
    }
    // Sum from the small terms up for accuracy.
    let mut s = 0.0;
    for n in (2..=64u32).rev() {
        let mut t = 1.0;
        for _ in 0..k {
            t /= n as f64;
        }
        s += t;
    }
    1.0 + s
}

/// lgamma(1 + z) (at2: lgamma(2 + z)) for |z| <= 0.25 by the Taylor
/// series with ζ coefficients.
fn lgamma_taylor(z: f64, at2: bool) -> f64 {
    let mut sum = 0.0;
    for k in (2..=40u32).rev() {
        let c = (zeta(k) - if at2 { 1.0 } else { 0.0 }) / k as f64;
        let c = if k % 2 == 0 { c } else { -c };
        sum = (sum + c) * z;
    }
    let c1 = if at2 { 1.0 - EULER_GAMMA } else { -EULER_GAMMA };
    (sum + c1) * z
}

/// lgamma of x >= 12 (a double-double) by Stirling's series.
fn stirling(x: Dd, ln2: Dd, ln2pi: Dd) -> Dd {
    // B_2k / (2k (2k - 1)), k = 1..8.
    const B: [(f64, f64); 8] = [
        (1.0, 12.0),
        (-1.0, 360.0),
        (1.0, 1260.0),
        (-1.0, 1680.0),
        (1.0, 1188.0),
        (-691.0, 360_360.0),
        (1.0, 156.0),
        (-3617.0, 122_400.0),
    ];
    let lnx = ln_dd(x, ln2);
    let main = sub(mul(add(x, dd(-0.5)), lnx), x);
    let inv = div(dd(1.0), x);
    let inv2 = mul(inv, inv);
    let mut series = dd(0.0);
    for &(n, d) in B.iter().rev() {
        series = add(mul(series, inv2), div(dd(n), dd(d)));
    }
    add(add(main, scale(ln2pi, -1)), mul(series, inv))
}

/// sin(pi x) for finite x.
fn sinpi(x: f64) -> f64 {
    // Reduce to r in [-1, 1] (exact), then to the first octant.
    let r = x - 2.0 * round_half_even(x / 2.0);
    let (s, r) = if r < 0.0 { (-1.0, -r) } else { (1.0, r) };
    let r = if r > 0.5 { 1.0 - r } else { r };
    // r in [0, 0.5]: sin(pi r) = cos(pi (0.5 - r)) for r > 0.25.
    let t = core::f64::consts::PI * if r > 0.25 { 0.5 - r } else { r };
    let t2 = t * t;
    let mut acc = 0.0;
    if r > 0.25 {
        for k in (1..=12).rev() {
            acc = 1.0 - acc * t2 / ((2 * k - 1) * (2 * k)) as f64;
        }
        s * acc
    } else {
        for k in (1..=12).rev() {
            acc = 1.0 - acc * t2 / ((2 * k) * (2 * k + 1)) as f64;
        }
        s * t * acc
    }
}

fn round_half_even(x: f64) -> f64 {
    let t = trunc_f(x);
    let d = x - t;
    if fabs_soft(d) > 0.5 || (fabs_soft(d) == 0.5 && (t / 2.0) != trunc_f(t / 2.0)) { t + copysign_soft(1.0, x) } else { t }
}

fn lgamma_pos(x: f64) -> f64 {
    let (ln2, _, ln2pi) = consts();
    if x < pow2(-70) {
        let l = ln_dd(dd(x), ln2);
        return -(l.hi + l.lo);
    }
    if fabs_soft(x - 1.0) <= 0.25 {
        return lgamma_taylor(x - 1.0, false);
    }
    if fabs_soft(x - 2.0) <= 0.25 {
        return lgamma_taylor(x - 2.0, true);
    }
    if x > 1e17 {
        let l = ln_dd(dd(x), ln2);
        return x * (l.hi - 1.0);
    }
    // Shift up to 12 by lgamma(x) = lgamma(x + n) - ln(x (x+1) ... ).
    let mut xs = dd(x);
    let mut prod = dd(1.0);
    while xs.hi < 12.0 {
        prod = mul(prod, xs);
        xs = add(xs, dd(1.0));
    }
    let mut r = stirling(xs, ln2, ln2pi);
    if prod.hi != 1.0 || prod.lo != 0.0 {
        r = sub(r, ln_dd(prod, ln2));
    }
    r.hi + r.lo
}

fn lgamma_f(x: f64, name: &'static [u8]) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return f64::INFINITY;
    }
    if x <= 0.0 && is_int(x) {
        return error(SING, name, x, 0.0, f64::INFINITY);
    }
    let r = if x > 0.0 {
        lgamma_pos(x)
    } else {
        // Reflection: lgamma(x) = ln(pi / |sin(pi x)|) - lgamma(1 - x).
        let (ln2, _, _) = consts();
        let s = fabs_soft(sinpi(x));
        let pi = Dd { hi: core::f64::consts::PI, lo: 1.224_646_799_147_353_2e-16 };
        let l = sub(ln_dd(pi, ln2), ln_dd(dd(s), ln2));
        let r = sub(l, dd(lgamma_pos(1.0 - x)));
        r.hi + r.lo
    };
    if r.is_infinite() {
        return error(OVERFLOW, name, x, 0.0, r);
    }
    r
}

extern "win64" fn lgamma_soft(x: f64) -> f64 {
    lgamma_f(x, b"lgamma\0")
}

extern "win64" fn lgammaf_soft(x: f32) -> f32 {
    let r = lgamma_f(x as f64, b"lgammaf\0");
    let f = r as f32;
    if f.is_infinite() && r.is_finite() {
        return error(OVERFLOW, b"lgammaf\0", x as f64, 0.0, f as f64) as f32;
    }
    f
}

// ---- NaN ------------------------------------------------------------------------------

/// nan("n-char-sequence"): the UCRT gives the default quiet NaN whatever
/// the sequence (its meaning is implementation-defined).
extern "win64" fn nan_soft(_s: *const u8) -> f64 {
    f64::from_bits(0x7FF8_0000_0000_0000)
}

extern "win64" fn nanf_soft(_s: *const u8) -> f32 {
    f32::from_bits(0x7FC0_0000)
}
pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![
        __setusermatherr, _dclass, _fdclass, _dsign, _fdsign, copysign, copysignf, fabs, trunc, floor, ceil, floorf,
        ceilf, truncf, round, sqrt, frexp, ldexp, fmod, log10, log, log10f, logf, exp, pow, powf, lgamma, lgammaf,
        nan, nanf,
    ];
    v.extend([("lgammal", lgamma as *const () as u64), ("_copysign", copysign as *const () as u64)]);
    v
}

xmm_abi! {
    _dclass => _dclass_soft ["movq rcx, xmm0"] "";
    _fdclass => _fdclass_soft ["movd ecx, xmm0"] "";
    _dsign => _dsign_soft ["movq rcx, xmm0"] "";
    _fdsign => _fdsign_soft ["movd ecx, xmm0"] "";
    copysign => copysign_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    copysignf => copysignf_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    fabs => fabs_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    trunc => trunc_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    floor => floor_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    ceil => ceil_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    floorf => floorf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    ceilf => ceilf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    truncf => truncf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    round => round_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    sqrt => sqrt_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    frexp => frexp_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    ldexp => ldexp_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    fmod => fmod_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    log10 => log10_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    log => log_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    log10f => log10f_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    logf => logf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    exp => exp_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    pow => pow_soft ["movq rcx, xmm0", "movq rdx, xmm1"] "movq xmm0, rax";
    powf => powf_soft ["movd ecx, xmm0", "movd edx, xmm1"] "movd xmm0, eax";
    lgamma => lgamma_soft ["movq rcx, xmm0"] "movq xmm0, rax";
    lgammaf => lgammaf_soft ["movd ecx, xmm0"] "movd xmm0, eax";
    nan => nan_soft [] "movq xmm0, rax";
    nanf => nanf_soft [] "movd xmm0, eax";
}
