//! A planned inverse MDCT: twiddles are worked out once per size, and the
//! complex FFT inside handles any length made of the factors 2, 3, 4 and 5
//! (AAC uses 1024 and 128 lines, Opus CELT 120 to 960).

use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

#[derive(Clone, Copy, Default)]
pub(crate) struct C {
    pub re: f32,
    pub im: f32,
}

impl C {
    #[inline]
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }

    #[inline]
    fn add(self, o: C) -> C {
        C { re: self.re + o.re, im: self.im + o.im }
    }

    #[inline]
    fn sub(self, o: C) -> C {
        C { re: self.re - o.re, im: self.im - o.im }
    }

    fn unit(ang: f64) -> C {
        C { re: libm::cos(ang) as f32, im: libm::sin(ang) as f32 }
    }
}

/// A complex FFT of a fixed length, exp(-2 pi i jk / n).
pub(crate) struct Fft {
    n: usize,
    /// (radix, length of each sub-transform), outermost first.
    factors: Vec<(usize, usize)>,
    /// exp(-2 pi i k / n), k < n.
    tw: Vec<C>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut factors = Vec::new();
        let mut m = n;
        while m > 1 {
            // Radix 4 first, then the rest; a lone 2 goes last.
            let p = [4, 2, 3, 5].into_iter().find(|&p| m % p == 0 && !(p == 2 && m % 4 == 0)).expect("FFT length");
            m /= p;
            factors.push((p, m));
        }
        let tw = (0..n).map(|k| C::unit(-2.0 * PI * k as f64 / n as f64)).collect();
        Fft { n, factors, tw }
    }

    /// `out` = DFT of `inp`.
    pub fn run(&self, inp: &[C], out: &mut [C]) {
        self.work(out, inp, 1, 0);
    }

    /// `out` = the unscaled inverse DFT of `inp` (which it conjugates).
    pub fn run_inverse(&self, inp: &mut [C], out: &mut [C]) {
        for v in inp.iter_mut() {
            v.im = -v.im;
        }
        self.work(out, inp, 1, 0);
        for v in out.iter_mut() {
            v.im = -v.im;
        }
    }

    fn work(&self, out: &mut [C], inp: &[C], fstride: usize, level: usize) {
        let (p, m) = self.factors[level];
        if m == 1 {
            for (i, o) in out[..p].iter_mut().enumerate() {
                *o = inp[i * fstride];
            }
        } else {
            for i in 0..p {
                self.work(&mut out[i * m..(i + 1) * m], &inp[i * fstride..], fstride * p, level + 1);
            }
        }
        self.butterfly(&mut out[..p * m], fstride, p, m);
    }

    fn butterfly(&self, out: &mut [C], fstride: usize, p: usize, m: usize) {
        let tw = &self.tw;
        match p {
            2 => {
                for u in 0..m {
                    let t = out[u + m].mul(tw[u * fstride]);
                    out[u + m] = out[u].sub(t);
                    out[u] = out[u].add(t);
                }
            }
            4 => {
                for u in 0..m {
                    let a0 = out[u];
                    let a1 = out[u + m].mul(tw[u * fstride]);
                    let a2 = out[u + 2 * m].mul(tw[2 * u * fstride]);
                    let a3 = out[u + 3 * m].mul(tw[3 * u * fstride]);
                    let (s0, d0) = (a0.add(a2), a0.sub(a2));
                    let (s1, d1) = (a1.add(a3), a1.sub(a3));
                    // -i * d1
                    let r = C { re: d1.im, im: -d1.re };
                    out[u] = s0.add(s1);
                    out[u + 2 * m] = s0.sub(s1);
                    out[u + m] = d0.add(r);
                    out[u + 3 * m] = d0.sub(r);
                }
            }
            _ => {
                // Radix 3 and 5: a direct DFT of p points.
                let n = self.n;
                let mut s = [C::default(); 5];
                for u in 0..m {
                    for (q, v) in s[..p].iter_mut().enumerate() {
                        *v = out[u + q * m].mul(tw[(q * u * fstride) % n]);
                    }
                    for k in 0..p {
                        let mut acc = s[0];
                        for (q, v) in s[1..p].iter().enumerate() {
                            acc = acc.add(v.mul(tw[((q + 1) * k * m * fstride) % n]));
                        }
                        out[u + k * m] = acc;
                    }
                }
            }
        }
    }
}

/// The inverse MDCT of `m` coefficients to `2 m` samples:
/// y[n] = sum X[k] cos(2 pi / N (n + 1/2 + N/4)(k + 1/2)), N = 2 m,
/// times a constant `scale`.
pub struct Imdct {
    m: usize,
    fft: Fft,
    pre: Vec<C>,
    post: Vec<C>,
    a: Vec<C>,
    b: Vec<C>,
}

impl Imdct {
    /// `m` must be even with m / 2 made of the factors 2, 3 and 5.
    pub fn new(m: usize, scale: f32) -> Imdct {
        let h = m / 2;
        let pre = (0..h).map(|k| C::unit(-PI * (4 * k + 1) as f64 / (4 * m) as f64)).collect();
        let post = (0..h)
            .map(|j| {
                let c = C::unit(-PI * j as f64 / m as f64);
                C { re: c.re * scale, im: c.im * scale }
            })
            .collect();
        Imdct { m, fft: Fft::new(h), pre, post, a: vec![C::default(); h], b: vec![C::default(); h] }
    }

    /// `x` holds `m` coefficients, `y` receives `2 m` samples.
    pub fn run(&mut self, x: &[f32], y: &mut [f32]) {
        let m = self.m;
        let h = m / 2;
        // DCT-IV of size m through a complex FFT of size m / 2.
        for k in 0..h {
            self.a[k] = C { re: x[2 * k], im: x[m - 1 - 2 * k] }.mul(self.pre[k]);
        }
        self.fft.run(&self.a, &mut self.b);
        // u = DCT-IV(x); y unfolds it: y[n] = u[n + m/2] for n < m/2,
        // -u[3m/2 - 1 - n] for n < 3m/2, -u[n - 3m/2] after.
        for j in 0..h {
            let t = self.b[j].mul(self.post[j]);
            let (u_even, u_odd) = (t.re, -t.im);
            // u[2j] = u_even, u[m - 1 - 2j] = u_odd
            put(y, m, 2 * j, u_even);
            put(y, m, m - 1 - 2 * j, u_odd);
        }
    }
}

/// Stores u[k] of the DCT-IV at every place of the unfolded output.
#[inline]
fn put(y: &mut [f32], m: usize, k: usize, v: f32) {
    // n + m/2 = k for n < m/2
    if k >= m / 2 {
        y[k - m / 2] = v;
    }
    // 2m - 1 - (n + m/2) = k for m/2 <= n < 3m/2, value -u[k]
    let n = 2 * m - 1 - k - m / 2;
    if (m / 2..3 * m / 2).contains(&n) {
        y[n] = -v;
    }
    // n + m/2 - 2m = k for n >= 3m/2, value -u[k]
    let n = k + 3 * m / 2;
    if n < 2 * m {
        y[n] = -v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(x: &[f32]) -> Vec<f32> {
        let m = x.len();
        let n2 = 2 * m;
        (0..n2)
            .map(|n| {
                x.iter()
                    .enumerate()
                    .map(|(k, &v)| {
                        v as f64
                            * libm::cos(2.0 * PI / n2 as f64 * (n as f64 + 0.5 + m as f64 / 2.0) * (k as f64 + 0.5))
                    })
                    .sum::<f64>() as f32
            })
            .collect()
    }

    #[test]
    fn matches_the_definition() {
        for m in [8usize, 16, 32, 64, 120, 128, 240, 256, 480, 960, 1024, 2048, 4096] {
            let x: Vec<f32> = (0..m).map(|i| ((i * 7919 % 113) as f32 - 56.0) / 57.0).collect();
            let want = direct(&x);
            let mut got = vec![0.0f32; 2 * m];
            Imdct::new(m, 1.0).run(&x, &mut got);
            let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 2e-3 * (m as f32).sqrt(), "m {m}: off by {worst}");
        }
    }
}
