//! Glyph rasterization in fixed point (winrun is soft-float): outlines are
//! flattened into lines in 24.8 pixel coordinates (y down) and filled with
//! the non-zero winding rule. Coverage is exact horizontally and sampled
//! on 16 sub-scanlines per pixel row.
use super::ttf::{Outline, Point};
use alloc::vec::Vec;

/// 1.0 in 24.8 fixed point.
pub const ONE: i32 = 256;
const SUB: i32 = 16;

/// A transform from font units to 24.8 pixels: x' = (a*x + c*y) >> 16 +
/// dx, y' = (b*x + d*y) >> 16 + dy, coefficients in 8.16 pixels per unit
/// times 256 (so a = scale * 65536 * 256 / 256).
#[derive(Clone, Copy, Debug)]
pub struct Xform {
    pub a: i64,
    pub b: i64,
    pub c: i64,
    pub d: i64,
    pub dx: i64,
    pub dy: i64,
}

impl Xform {
    fn apply(&self, p: Point) -> (i32, i32) {
        (
            ((self.a * p.x as i64 + self.c * p.y as i64) >> 16).saturating_add(self.dx) as i32,
            ((self.b * p.x as i64 + self.d * p.y as i64) >> 16).saturating_add(self.dy) as i32,
        )
    }
}

/// Line segments (x0, y0, x1, y1) in 24.8 pixels.
pub type Lines = Vec<(i32, i32, i32, i32)>;

fn isqrt(v: i64) -> i64 {
    if v <= 0 {
        return 0;
    }
    // Newton's method from above: decreases to floor(sqrt(v)).
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

fn quad(out: &mut Lines, p0: (i32, i32), p1: (i32, i32), p2: (i32, i32)) {
    // Segments so the flattened curve stays within ~1/8 pixel.
    let ddx = (p0.0 - 2 * p1.0 + p2.0) as i64;
    let ddy = (p0.1 - 2 * p1.1 + p2.1) as i64;
    let dev = isqrt(ddx * ddx + ddy * ddy);
    let n = (1 + isqrt(dev * 2)).clamp(1, 64);
    let mut prev = p0;
    for i in 1..=n {
        let (t, u) = (i, n - i);
        let nn = n * n;
        let x = (u * u * p0.0 as i64 + 2 * u * t * p1.0 as i64 + t * t * p2.0 as i64) / nn;
        let y = (u * u * p0.1 as i64 + 2 * u * t * p1.1 as i64 + t * t * p2.1 as i64) / nn;
        let next = (x as i32, y as i32);
        out.push((prev.0, prev.1, next.0, next.1));
        prev = next;
    }
}

/// Flattens the outline under `m` into closed polylines.
pub fn flatten(outline: &Outline, m: &Xform) -> Lines {
    let mut out = Lines::new();
    for contour in outline {
        if contour.len() < 2 {
            continue;
        }
        let pts: Vec<((i32, i32), bool)> = contour.iter().map(|&p| (m.apply(p), p.on)).collect();
        let n = pts.len();
        // Start at an on-curve point (or the midpoint of two off ones).
        let first_on = pts.iter().position(|p| p.1);
        let start = match first_on {
            Some(i) => pts[i].0,
            None => mid(pts[0].0, pts[1].0),
        };
        let s = first_on.unwrap_or(0);
        let mut cur = start;
        let mut ctrl: Option<(i32, i32)> = None;
        for k in 1..=n {
            let (p, on) = pts[(s + k) % n];
            match (on, ctrl) {
                (true, None) => {
                    out.push((cur.0, cur.1, p.0, p.1));
                    cur = p;
                }
                (true, Some(c)) => {
                    quad(&mut out, cur, c, p);
                    cur = p;
                    ctrl = None;
                }
                (false, None) => ctrl = Some(p),
                (false, Some(c)) => {
                    let m2 = mid(c, p);
                    quad(&mut out, cur, c, m2);
                    cur = m2;
                    ctrl = Some(p);
                }
            }
        }
        match ctrl {
            Some(c) => quad(&mut out, cur, c, start),
            None if cur != start => out.push((cur.0, cur.1, start.0, start.1)),
            None => {}
        }
    }
    out
}

fn mid(a: (i32, i32), b: (i32, i32)) -> (i32, i32) {
    ((a.0 + b.0) / 2, (a.1 + b.1) / 2)
}

/// The integer pixel box covering the lines: (left, top, right, bottom).
pub fn bounds(lines: &Lines) -> Option<(i32, i32, i32, i32)> {
    let mut b: Option<(i32, i32, i32, i32)> = None;
    for &(x0, y0, x1, y1) in lines {
        let (l, t, r, bt) = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        b = Some(match b {
            None => (l, t, r, bt),
            Some(o) => (o.0.min(l), o.1.min(t), o.2.max(r), o.3.max(bt)),
        });
    }
    let (l, t, r, bt) = b?;
    let (l, t, r, bt) = (l.div_euclid(ONE), t.div_euclid(ONE), (r + ONE - 1).div_euclid(ONE), (bt + ONE - 1).div_euclid(ONE));
    (r > l && bt > t).then_some((l, t, r, bt))
}

/// 8-bit coverage of the lines over the pixel box `(left, top, right,
/// bottom)`, row by row.
pub fn coverage(lines: &Lines, bx: (i32, i32, i32, i32)) -> Vec<u8> {
    let (left, top, right, bottom) = bx;
    let (w, h) = ((right - left) as usize, (bottom - top) as usize);
    let mut out = alloc::vec![0u8; w * h];
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return out;
    }
    // Per row: accumulated covered length (24.8) per pixel, over SUB rows.
    let mut acc = alloc::vec![0i32; w + 1];
    let mut xs: Vec<(i32, i32)> = Vec::new();
    for row in 0..h {
        acc.iter_mut().for_each(|a| *a = 0);
        for s in 0..SUB {
            // Sample at the centre of the sub-scanline.
            let y = (top + row as i32) * ONE + (s * ONE + ONE / 2) / SUB;
            xs.clear();
            for &(x0, y0, x1, y1) in lines {
                if y0 == y1 {
                    continue;
                }
                let (dir, ya, yb, xa, xb) = if y0 < y1 { (1, y0, y1, x0, x1) } else { (-1, y1, y0, x1, x0) };
                if y < ya || y >= yb {
                    continue;
                }
                let x = xa as i64 + (xb - xa) as i64 * (y - ya) as i64 / (yb - ya) as i64;
                xs.push((x as i32, dir));
            }
            xs.sort_unstable_by_key(|e| e.0);
            let mut wind = 0;
            let mut span_start = 0;
            for &(x, dir) in &xs {
                let was = wind != 0;
                wind += dir;
                if !was && wind != 0 {
                    span_start = x;
                } else if was && wind == 0 {
                    span(&mut acc, span_start - left * ONE, x - left * ONE, w);
                }
            }
        }
        let line = &mut out[row * w..row * w + w];
        for (px, a) in line.iter_mut().zip(&acc) {
            // Full coverage is SUB * ONE.
            *px = ((*a as i64 * 255 + (SUB * ONE) as i64 / 2) / (SUB * ONE) as i64).clamp(0, 255) as u8;
        }
    }
    out
}

/// Adds the covered length of [x0, x1) (24.8, relative to the box) to the
/// pixels it crosses.
fn span(acc: &mut [i32], x0: i32, x1: i32, w: usize) {
    let (x0, x1) = (x0.clamp(0, w as i32 * ONE), x1.clamp(0, w as i32 * ONE));
    if x1 <= x0 {
        return;
    }
    let (p0, p1) = ((x0 / ONE) as usize, ((x1 - 1) / ONE) as usize);
    if p0 == p1 {
        acc[p0] += x1 - x0;
        return;
    }
    acc[p0] += (p0 as i32 + 1) * ONE - x0;
    for a in &mut acc[p0 + 1..p1] {
        *a += ONE;
    }
    acc[p1] += x1 - p1 as i32 * ONE;
}
