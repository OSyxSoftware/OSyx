//! Rasterisation for the PDF renderer: paths in device space, anti-aliased
//! filling with the non-zero and even-odd rules (8 sub-scanlines, exact
//! horizontal coverage), clip masks, and strokes turned into polygons
//! (joins, caps and dash patterns).

#[allow(unused_imports)]
use super::Float;
use alloc::vec;
use alloc::vec::Vec;

const SUB: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix(pub [f64; 6]);

impl Matrix {
    pub const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// `self` then `m` (row vectors: p * self * m).
    pub fn then(&self, m: &Matrix) -> Matrix {
        let a = &self.0;
        let b = &m.0;
        Matrix([
            a[0] * b[0] + a[1] * b[2],
            a[0] * b[1] + a[1] * b[3],
            a[2] * b[0] + a[3] * b[2],
            a[2] * b[1] + a[3] * b[3],
            a[4] * b[0] + a[5] * b[2] + b[4],
            a[4] * b[1] + a[5] * b[3] + b[5],
        ])
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
    }

    pub fn invert(&self) -> Option<Matrix> {
        let m = &self.0;
        let det = m[0] * m[3] - m[1] * m[2];
        if det.abs() < 1e-12 {
            return None;
        }
        let a = m[3] / det;
        let b = -m[1] / det;
        let c = -m[2] / det;
        let d = m[0] / det;
        Some(Matrix([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)]))
    }

    /// How much the matrix scales lengths (geometric mean of the axes).
    pub fn scale(&self) -> f64 {
        let m = &self.0;
        libm::sqrt((m[0] * m[3] - m[1] * m[2]).abs())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    Cubic(f64, f64, f64, f64, f64, f64),
    Close,
}

/// A path in some coordinate space.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub segs: Vec<Seg>,
}

impl Path {
    pub fn transform(&self, m: &Matrix) -> Path {
        let segs = self
            .segs
            .iter()
            .map(|s| match *s {
                Seg::Move(x, y) => {
                    let (x, y) = m.apply(x, y);
                    Seg::Move(x, y)
                }
                Seg::Line(x, y) => {
                    let (x, y) = m.apply(x, y);
                    Seg::Line(x, y)
                }
                Seg::Cubic(a, b, c, d, e, f) => {
                    let (a, b) = m.apply(a, b);
                    let (c, d) = m.apply(c, d);
                    let (e, f) = m.apply(e, f);
                    Seg::Cubic(a, b, c, d, e, f)
                }
                Seg::Close => Seg::Close,
            })
            .collect();
        Path { segs }
    }

    /// Polylines (closed flag) with curves flattened to `tol` device units.
    pub fn flatten(&self, tol: f64) -> Vec<(Vec<(f64, f64)>, bool)> {
        let mut out = Vec::new();
        let mut cur: Vec<(f64, f64)> = Vec::new();
        let mut start = (0.0, 0.0);
        for s in &self.segs {
            match *s {
                Seg::Move(x, y) => {
                    if cur.len() > 1 {
                        out.push((core::mem::take(&mut cur), false));
                    }
                    cur.clear();
                    cur.push((x, y));
                    start = (x, y);
                }
                Seg::Line(x, y) => {
                    if cur.is_empty() {
                        cur.push(start);
                    }
                    cur.push((x, y));
                }
                Seg::Cubic(x1, y1, x2, y2, x3, y3) => {
                    if cur.is_empty() {
                        cur.push(start);
                    }
                    let (x0, y0) = *cur.last().unwrap();
                    let dd = ((x0 - 2.0 * x1 + x2).abs() + (y0 - 2.0 * y1 + y2).abs())
                        .max((x1 - 2.0 * x2 + x3).abs() + (y1 - 2.0 * y2 + y3).abs());
                    let n = (libm::sqrt(dd * 0.75 / tol).ceil() as usize).clamp(1, 256);
                    for i in 1..=n {
                        let t = i as f64 / n as f64;
                        let u = 1.0 - t;
                        let x = u * u * u * x0 + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x3;
                        let y = u * u * u * y0 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y3;
                        cur.push((x, y));
                    }
                }
                Seg::Close => {
                    if cur.len() > 1 {
                        out.push((core::mem::take(&mut cur), true));
                    }
                    cur.clear();
                    cur.push(start);
                }
            }
        }
        if cur.len() > 1 {
            out.push((cur, false));
        }
        out
    }

    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut b: Option<(f64, f64, f64, f64)> = None;
        let mut add = |x: f64, y: f64| {
            b = Some(match b {
                None => (x, y, x, y),
                Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
            });
        };
        for s in &self.segs {
            match *s {
                Seg::Move(x, y) | Seg::Line(x, y) => add(x, y),
                Seg::Cubic(a, b2, c, d, e, f) => {
                    add(a, b2);
                    add(c, d);
                    add(e, f);
                }
                Seg::Close => {}
            }
        }
        b
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    dir: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    NonZero,
    EvenOdd,
}

/// Coverage (0..=255) of a shape over the canvas, one byte per pixel.
#[derive(Clone)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<u8>,
    /// Rows `y0..y1` may be non-zero.
    pub y0: usize,
    pub y1: usize,
}

/// Computes the coverage of filled polygons, calling `span(y, x0, cov)` for
/// each row with the coverage of pixels from `x0` on.
fn coverage(
    polys: &[(Vec<(f64, f64)>, bool)],
    rule: Rule,
    w: usize,
    h: usize,
    mut row: impl FnMut(usize, usize, &[f32]),
) {
    let mut edges = Vec::new();
    for (pts, _) in polys {
        let n = pts.len();
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            if a.1 == b.1 || !(a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite()) {
                continue;
            }
            let (dir, (x0, y0), (x1, y1)) = if a.1 < b.1 { (1, a, b) } else { (-1, b, a) };
            edges.push(Edge { x0, y0, x1, y1, dir });
        }
    }
    if edges.is_empty() {
        return;
    }
    edges.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap_or(core::cmp::Ordering::Equal));
    let ymin = edges[0].y0.max(0.0).floor() as usize;
    let ymax = edges.iter().fold(0.0f64, |m, e| m.max(e.y1)).min(h as f64).ceil() as usize;
    let mut next = 0;
    let mut active: Vec<Edge> = Vec::new();
    let mut acc = vec![0.0f32; w + 2];
    let mut xs: Vec<(f64, i32)> = Vec::new();
    for y in ymin..ymax {
        let (mut lo, mut hi) = (w, 0usize);
        for s in 0..SUB {
            let sy = y as f64 + (s as f64 + 0.5) / SUB as f64;
            while next < edges.len() && edges[next].y0 <= sy {
                active.push(edges[next]);
                next += 1;
            }
            active.retain(|e| e.y1 > sy);
            xs.clear();
            for e in &active {
                if e.y0 <= sy {
                    let t = (sy - e.y0) / (e.y1 - e.y0);
                    xs.push((e.x0 + (e.x1 - e.x0) * t, e.dir));
                }
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
            let mut wind = 0;
            for i in 0..xs.len() {
                wind += xs[i].1;
                let inside = match rule {
                    Rule::NonZero => wind != 0,
                    Rule::EvenOdd => wind & 1 != 0,
                };
                if inside && i + 1 < xs.len() {
                    let (a, b) = (xs[i].0.clamp(0.0, w as f64), xs[i + 1].0.clamp(0.0, w as f64));
                    if b <= a {
                        continue;
                    }
                    let (ia, ib) = (a as usize, b as usize);
                    let unit = 1.0 / SUB as f32;
                    if ia == ib {
                        acc[ia] += ((b - a) as f32) * unit;
                    } else {
                        acc[ia] += ((ia + 1) as f64 - a) as f32 * unit;
                        for px in acc.iter_mut().take(ib).skip(ia + 1) {
                            *px += unit;
                        }
                        if ib < w {
                            acc[ib] += (b - ib as f64) as f32 * unit;
                        }
                    }
                    lo = lo.min(ia);
                    hi = hi.max(ib + 1);
                }
            }
        }
        if lo < hi {
            let hi = hi.min(w);
            row(y, lo, &acc[lo..hi]);
            for v in &mut acc[lo..hi + 1] {
                *v = 0.0;
            }
        }
    }
}

impl Mask {
    pub fn full(w: usize, h: usize) -> Mask {
        Mask { w, h, a: vec![255; w * h], y0: 0, y1: h }
    }

    pub fn from_path(path: &Path, rule: Rule, w: usize, h: usize) -> Mask {
        let polys = path.flatten(0.2);
        let mut m = Mask { w, h, a: vec![0; w * h], y0: h, y1: 0 };
        coverage(&polys, rule, w, h, |y, x0, cov| {
            m.y0 = m.y0.min(y);
            m.y1 = m.y1.max(y + 1);
            for (i, c) in cov.iter().enumerate() {
                m.a[y * w + x0 + i] = (c.min(1.0) * 255.0 + 0.5) as u8;
            }
        });
        m
    }

    pub fn intersect(&self, other: &Mask) -> Mask {
        let a = self.a.iter().zip(&other.a).map(|(&x, &y)| ((x as u32 * y as u32 + 127) / 255) as u8).collect();
        Mask { w: self.w, h: self.h, a, y0: self.y0.max(other.y0), y1: self.y1.min(other.y1) }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.a[y * self.w + x]
    }
}

/// The page being drawn: opaque RGB.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 3]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![[255; 3]; w * h] }
    }

    #[inline]
    pub fn blend(&mut self, x: usize, y: usize, c: [u8; 3], a: u32) {
        if a == 0 {
            return;
        }
        let p = &mut self.px[y * self.w + x];
        if a >= 255 {
            *p = c;
            return;
        }
        for i in 0..3 {
            p[i] = ((c[i] as u32 * a + p[i] as u32 * (255 - a) + 127) / 255) as u8;
        }
    }

    /// Fills `path` (device space) with a colour from `paint(x, y)`.
    pub fn fill(
        &mut self,
        path: &Path,
        rule: Rule,
        clip: Option<&Mask>,
        alpha: f32,
        paint: &mut dyn FnMut(usize, usize) -> Option<[u8; 3]>,
    ) {
        let polys = path.flatten(0.2);
        self.fill_polys(&polys, rule, clip, alpha, paint);
    }

    fn fill_polys(
        &mut self,
        polys: &[(Vec<(f64, f64)>, bool)],
        rule: Rule,
        clip: Option<&Mask>,
        alpha: f32,
        paint: &mut dyn FnMut(usize, usize) -> Option<[u8; 3]>,
    ) {
        let (w, h) = (self.w, self.h);
        coverage(polys, rule, w, h, |y, x0, cov| {
            if let Some(c) = clip {
                if y < c.y0 || y >= c.y1 {
                    return;
                }
            }
            for (i, &c) in cov.iter().enumerate() {
                let x = x0 + i;
                let mut a = c.min(1.0) * alpha * 255.0;
                if let Some(m) = clip {
                    a = a * m.at(x, y) as f32 / 255.0;
                }
                let a = (a + 0.5) as u32;
                if a > 0 {
                    if let Some(col) = paint(x, y) {
                        self.blend(x, y, col, a);
                    }
                }
            }
        });
    }

    pub fn stroke(
        &mut self,
        path: &Path,
        style: &StrokeStyle,
        clip: Option<&Mask>,
        alpha: f32,
        paint: &mut dyn FnMut(usize, usize) -> Option<[u8; 3]>,
    ) {
        let polys = stroke_polygons(path, style);
        self.fill_polys(&polys, Rule::NonZero, clip, alpha, paint);
    }
}

#[derive(Clone, Debug)]
pub struct StrokeStyle {
    /// Device units.
    pub width: f64,
    /// 0 butt, 1 round, 2 projecting square.
    pub cap: u8,
    /// 0 miter, 1 round, 2 bevel.
    pub join: u8,
    pub miter_limit: f64,
    /// Dash lengths and phase, device units.
    pub dash: Vec<f64>,
    pub phase: f64,
}

/// Splits polylines by the dash pattern.
fn dashed(lines: Vec<(Vec<(f64, f64)>, bool)>, dash: &[f64], phase: f64) -> Vec<(Vec<(f64, f64)>, bool)> {
    let total: f64 = dash.iter().sum();
    // Sub-pixel patterns would make millions of pieces: draw them solid.
    if dash.is_empty() || total < 0.1 || dash.iter().any(|d| *d < 0.0) {
        return lines;
    }
    let mut out = Vec::new();
    for (mut pts, closed) in lines {
        if closed {
            pts.push(pts[0]);
        }
        // Where in the pattern the line starts.
        let mut idx = 0;
        let mut left = dash[0];
        let mut p = phase % total;
        while p > 0.0 {
            if p >= left {
                p -= left;
                idx = (idx + 1) % dash.len();
                left = dash[idx];
            } else {
                left -= p;
                p = 0.0;
            }
        }
        let mut on = idx % 2 == 0;
        let mut cur: Vec<(f64, f64)> = if on { vec![pts[0]] } else { Vec::new() };
        for i in 1..pts.len() {
            let (mut a, b) = (pts[i - 1], pts[i]);
            let mut seg = libm::hypot(b.0 - a.0, b.1 - a.1);
            while seg > 0.0 {
                if seg <= left {
                    left -= seg;
                    if on {
                        cur.push(b);
                    }
                    seg = 0.0;
                } else {
                    let t = left / seg;
                    let m = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                    if on {
                        cur.push(m);
                        out.push((core::mem::take(&mut cur), false));
                    } else {
                        cur = vec![m];
                    }
                    on = !on;
                    seg -= left;
                    a = m;
                    idx = (idx + 1) % dash.len();
                    left = dash[idx];
                }
            }
        }
        if on && cur.len() > 1 {
            out.push((cur, false));
        }
    }
    out
}

/// Polygons whose non-zero union is the stroke.
pub fn stroke_polygons(path: &Path, st: &StrokeStyle) -> Vec<(Vec<(f64, f64)>, bool)> {
    let hw = (st.width / 2.0).max(0.5);
    let mut lines = path.flatten(0.2);
    if !st.dash.is_empty() {
        lines = dashed(lines, &st.dash, st.phase);
    }
    let mut polys: Vec<(Vec<(f64, f64)>, bool)> = Vec::new();
    // Counter-clockwise in device space (y down) for every piece, so the
    // non-zero rule unions them.
    let mut push = |mut p: Vec<(f64, f64)>| {
        let area: f64 = (0..p.len())
            .map(|i| {
                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        if area < 0.0 {
            p.reverse();
        }
        if p.len() >= 3 {
            polys.push((p, true));
        }
    };
    let circle = |c: (f64, f64), r: f64| -> Vec<(f64, f64)> {
        let n = ((r * 2.0) as usize).clamp(8, 64);
        (0..n)
            .map(|i| {
                let a = i as f64 / n as f64 * core::f64::consts::TAU;
                (c.0 + r * libm::cos(a), c.1 + r * libm::sin(a))
            })
            .collect()
    };
    for (mut pts, closed) in lines {
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        if pts.len() == 1 {
            // A zero-length subpath: only round or square caps draw a dot.
            match st.cap {
                1 => push(circle(pts[0], hw)),
                2 => {
                    let (x, y) = pts[0];
                    push(vec![(x - hw, y - hw), (x + hw, y - hw), (x + hw, y + hw), (x - hw, y + hw)]);
                }
                _ => {}
            }
            continue;
        }
        let n = pts.len();
        let segs = if closed { n } else { n - 1 };
        for i in 0..segs {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = libm::hypot(dx, dy);
            if len == 0.0 {
                continue;
            }
            let (ux, uy) = (dx / len, dy / len);
            let (nx, ny) = (-uy * hw, ux * hw);
            let (mut a2, mut b2) = (a, b);
            if !closed && st.cap == 2 {
                if i == 0 {
                    a2 = (a.0 - ux * hw, a.1 - uy * hw);
                }
                if i == segs - 1 {
                    b2 = (b.0 + ux * hw, b.1 + uy * hw);
                }
            }
            push(vec![(a2.0 + nx, a2.1 + ny), (b2.0 + nx, b2.1 + ny), (b2.0 - nx, b2.1 - ny), (a2.0 - nx, a2.1 - ny)]);
        }
        // Joins at inner vertices.
        let joints: Vec<usize> = if closed { (0..n).collect() } else { (1..n - 1).collect() };
        for &j in &joints {
            let p = pts[j];
            let prev = pts[(j + n - 1) % n];
            let next = pts[(j + 1) % n];
            let (d0x, d0y) = (p.0 - prev.0, p.1 - prev.1);
            let (d1x, d1y) = (next.0 - p.0, next.1 - p.1);
            let (l0, l1) = (libm::hypot(d0x, d0y), libm::hypot(d1x, d1y));
            if l0 == 0.0 || l1 == 0.0 {
                continue;
            }
            match st.join {
                1 => push(circle(p, hw)),
                _ => {
                    let cross = d0x * d1y - d0y * d1x;
                    let s = if cross > 0.0 { -1.0 } else { 1.0 };
                    let n0 = (-d0y / l0 * hw * s, d0x / l0 * hw * s);
                    let n1 = (-d1y / l1 * hw * s, d1x / l1 * hw * s);
                    let a = (p.0 + n0.0, p.1 + n0.1);
                    let b = (p.0 + n1.0, p.1 + n1.1);
                    let mut poly = vec![p, a];
                    if st.join == 0 {
                        // Miter tip, within the limit.
                        let cos = (d0x * d1x + d0y * d1y) / (l0 * l1);
                        let half = libm::sqrt(((1.0 - cos) / 2.0).max(1e-12));
                        let ratio = 1.0 / half;
                        if ratio <= st.miter_limit {
                            let (mx, my) = (n0.0 + n1.0, n0.1 + n1.1);
                            let ml = libm::hypot(mx, my);
                            if ml > 0.0 {
                                let dist = hw / libm::sqrt(((1.0 + cos) / 2.0).max(1e-12));
                                poly.push((p.0 + mx / ml * dist, p.1 + my / ml * dist));
                            }
                        }
                    }
                    poly.push(b);
                    push(poly);
                }
            }
        }
        if !closed && st.cap == 1 {
            push(circle(pts[0], hw));
            push(circle(pts[n - 1], hw));
        }
    }
    polys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Path {
        Path { segs: vec![Seg::Move(x0, y0), Seg::Line(x1, y0), Seg::Line(x1, y1), Seg::Line(x0, y1), Seg::Close] }
    }

    #[test]
    fn rectangle_coverage_is_exact() {
        let mut c = Canvas::new(10, 10);
        c.fill(&rect(2.0, 2.0, 5.5, 4.0), Rule::NonZero, None, 1.0, &mut |_, _| Some([0, 0, 0]));
        assert_eq!(c.px[3 * 10 + 3], [0, 0, 0]);
        assert_eq!(c.px[3 * 10 + 5], [127, 127, 127], "half covered");
        assert_eq!(c.px[3 * 10 + 6], [255; 3]);
        assert_eq!(c.px[5 * 10 + 3], [255; 3]);
    }

    #[test]
    fn even_odd_leaves_a_hole() {
        let mut p = rect(0.0, 0.0, 10.0, 10.0);
        p.segs.extend(rect(3.0, 3.0, 7.0, 7.0).segs);
        let mut c = Canvas::new(10, 10);
        c.fill(&p, Rule::EvenOdd, None, 1.0, &mut |_, _| Some([0, 0, 0]));
        assert_eq!(c.px[5 * 10 + 5], [255; 3]);
        assert_eq!(c.px[1 * 10 + 1], [0; 3]);
        let mut c = Canvas::new(10, 10);
        c.fill(&p, Rule::NonZero, None, 1.0, &mut |_, _| Some([0, 0, 0]));
        assert_eq!(c.px[5 * 10 + 5], [0; 3], "same direction: no hole");
    }

    #[test]
    fn strokes_cover_their_width() {
        let p = Path { segs: vec![Seg::Move(1.0, 5.0), Seg::Line(9.0, 5.0)] };
        let st = StrokeStyle { width: 2.0, cap: 0, join: 0, miter_limit: 10.0, dash: Vec::new(), phase: 0.0 };
        let mut c = Canvas::new(10, 10);
        c.stroke(&p, &st, None, 1.0, &mut |_, _| Some([0, 0, 0]));
        assert_eq!(c.px[4 * 10 + 5], [0; 3]);
        assert_eq!(c.px[5 * 10 + 5], [0; 3]);
        assert_eq!(c.px[3 * 10 + 5], [255; 3]);
        assert_eq!(c.px[5 * 10 + 0], [255; 3], "butt cap");
    }

    #[test]
    fn matrices() {
        let m = Matrix([2.0, 0.0, 0.0, 3.0, 10.0, 20.0]);
        let inv = m.invert().unwrap();
        let (x, y) = m.then(&inv).apply(5.0, 7.0);
        assert!((x - 5.0).abs() < 1e-9 && (y - 7.0).abs() < 1e-9);
    }
}
