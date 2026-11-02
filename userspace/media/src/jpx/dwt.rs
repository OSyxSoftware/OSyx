//! The inverse discrete wavelet transform (T.800 Annex F): the reversible
//! 5/3 filter on integers and the irreversible 9/7 filter on floats, both
//! by lifting with periodic symmetric extension. Rows go first, then
//! columns (2D_SR).

use alloc::vec::Vec;

/// Samples of extension on each side: enough for the four 9/7 steps.
const E: usize = 4;

/// The index into a signal of `n` samples for extended position `k`.
fn reflect(k: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let p = 2 * (n as isize - 1);
    let m = k.rem_euclid(p);
    if m < n as isize {
        m as usize
    } else {
        (p - m) as usize
    }
}

fn extend<T: Copy>(x: &[T], buf: &mut Vec<T>) {
    buf.clear();
    let n = x.len();
    for k in -(E as isize)..(n + E) as isize {
        buf.push(x[reflect(k, n)]);
    }
}

/// `buf[j]` holds absolute position `i0 + j - E`; even positions are
/// low-pass samples. Runs `f(j)` over the positions of `parity` in
/// `from..to`.
fn each(i0: usize, parity: usize, from: usize, to: usize, mut f: impl FnMut(usize)) {
    let mut j = from;
    if (i0 + j) % 2 != parity {
        j += 1;
    }
    while j < to {
        f(j);
        j += 2;
    }
}

/// F.3.8.1 on one signal starting at absolute position `i0`.
fn synth53(x: &mut [i32], i0: usize, buf: &mut Vec<i32>) {
    let n = x.len();
    if n <= 1 {
        if n == 1 && i0 % 2 == 1 {
            x[0] /= 2;
        }
        return;
    }
    extend(x, buf);
    let len = buf.len();
    each(i0, 0, 1, len - 1, |j| buf[j] -= (buf[j - 1] + buf[j + 1] + 2) >> 2);
    each(i0, 1, 2, len - 2, |j| buf[j] += (buf[j - 1] + buf[j + 1]) >> 1);
    x.copy_from_slice(&buf[E..E + n]);
}

const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_117;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_87;
const K: f32 = 1.230_174_1;

/// F.3.8.2 on one signal starting at absolute position `i0`.
fn synth97(x: &mut [f32], i0: usize, buf: &mut Vec<f32>) {
    let n = x.len();
    if n <= 1 {
        if n == 1 && i0 % 2 == 1 {
            x[0] /= 2.0;
        }
        return;
    }
    extend(x, buf);
    let len = buf.len();
    each(i0, 0, 0, len, |j| buf[j] *= K);
    each(i0, 1, 0, len, |j| buf[j] *= 1.0 / K);
    each(i0, 0, 1, len - 1, |j| buf[j] -= DELTA * (buf[j - 1] + buf[j + 1]));
    each(i0, 1, 2, len - 2, |j| buf[j] -= GAMMA * (buf[j - 1] + buf[j + 1]));
    each(i0, 0, 3, len - 3, |j| buf[j] -= BETA * (buf[j - 1] + buf[j + 1]));
    each(i0, 1, 4, len - 4, |j| buf[j] -= ALPHA * (buf[j - 1] + buf[j + 1]));
    x.copy_from_slice(&buf[E..E + n]);
}

/// Interleaves the bands of a resolution spanning [u0, u1) x [v0, v1):
/// LL (the next lower resolution), HL, LH and HH (F.3.3).
#[allow(clippy::too_many_arguments)]
pub fn interleave<T: Copy + Default>(
    ll: &[T],
    hl: &[T],
    lh: &[T],
    hh: &[T],
    u0: usize,
    u1: usize,
    v0: usize,
    v1: usize,
) -> Vec<T> {
    let (w, h) = (u1 - u0, v1 - v0);
    let wl = u1.div_ceil(2) - u0.div_ceil(2);
    let wh = u1 / 2 - u0 / 2;
    let mut out = alloc::vec![T::default(); w * h];
    for v in v0..v1 {
        let (low_row, row) = if v % 2 == 0 { (true, v / 2 - v0.div_ceil(2)) } else { (false, v / 2 - v0 / 2) };
        for u in u0..u1 {
            let val = if u % 2 == 0 {
                let col = u / 2 - u0.div_ceil(2);
                if low_row { ll.get(row * wl + col) } else { lh.get(row * wl + col) }
            } else {
                let col = u / 2 - u0 / 2;
                if low_row { hl.get(row * wh + col) } else { hh.get(row * wh + col) }
            };
            out[(v - v0) * w + (u - u0)] = val.copied().unwrap_or_default();
        }
    }
    out
}

/// 2D_SR on an interleaved resolution of `w` by `h` at (u0, v0).
pub fn synth_int(a: &mut [i32], w: usize, h: usize, u0: usize, v0: usize) {
    let mut buf = Vec::with_capacity(w.max(h) + 2 * E);
    if w > 0 {
        for row in a.chunks_mut(w) {
            synth53(row, u0, &mut buf);
        }
    }
    let mut col = alloc::vec![0; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = a[y * w + x];
        }
        synth53(&mut col, v0, &mut buf);
        for y in 0..h {
            a[y * w + x] = col[y];
        }
    }
}

/// 2D_SR with the 9/7 filter.
pub fn synth_float(a: &mut [f32], w: usize, h: usize, u0: usize, v0: usize) {
    let mut buf = Vec::with_capacity(w.max(h) + 2 * E);
    if w > 0 {
        for row in a.chunks_mut(w) {
            synth97(row, u0, &mut buf);
        }
    }
    let mut col = alloc::vec![0.0; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = a[y * w + x];
        }
        synth97(&mut col, v0, &mut buf);
        for y in 0..h {
            a[y * w + x] = col[y];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 5/3 analysis (F.4.8.1) inverts exactly.
    #[test]
    fn reversible_round_trip() {
        for i0 in 0..2usize {
            for n in 2..12usize {
                let x: Vec<i32> = (0..n as i32).map(|i| (i * 37 + 11) % 23 - 9).collect();
                // Forward lifting on an extended copy.
                let mut buf = Vec::new();
                extend(&x, &mut buf);
                let len = buf.len();
                each(i0, 1, 1, len - 1, |j| buf[j] -= (buf[j - 1] + buf[j + 1]) >> 1);
                each(i0, 0, 2, len - 2, |j| buf[j] += (buf[j - 1] + buf[j + 1] + 2) >> 2);
                let mut y = buf[E..E + n].to_vec();
                synth53(&mut y, i0, &mut buf);
                assert_eq!(y, x, "i0 {i0}, n {n}");
            }
        }
    }
}
