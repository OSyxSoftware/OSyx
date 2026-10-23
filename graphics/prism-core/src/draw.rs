//! Composition kernels: blits of client pixels, premultiplied alpha
//! blending, rounded-corner masks, drop shadows and the pointer.
//!
//! All coordinates are screen pixels; every function takes a clip
//! rectangle and touches nothing outside `clip ∩ target`. Offsets into
//! client pixel data are computed with checked arithmetic from validated
//! dimensions (see [`Pixels`]).

use alloc::vec;
use alloc::vec::Vec;
use osyx_gfx::{isqrt, Color, Rect, Surface};

/// Read-only view of client pixels (`0xAARRGGBB`, premultiplied alpha
/// when `alpha` is set).
#[derive(Clone, Copy)]
pub struct Pixels<'a> {
    data: &'a [u32],
    pub width: u32,
    pub height: u32,
    /// In pixels.
    stride: u32,
    pub alpha: bool,
}

impl<'a> Pixels<'a> {
    /// `None` unless `data` holds `stride * height` pixels and `stride >= width`.
    pub fn new(data: &'a [u32], width: u32, height: u32, stride: u32, alpha: bool) -> Option<Pixels<'a>> {
        let needed = (stride as usize).checked_mul(height as usize)?;
        (stride >= width && data.len() >= needed).then_some(Pixels { data, width, height, stride, alpha })
    }

    /// Pixel (`x`, `y`); the caller keeps the coordinates in range.
    pub fn get(&self, x: u32, y: u32) -> u32 {
        self.data[y as usize * self.stride as usize + x as usize]
    }

    fn row(&self, y: u32, x: u32, len: u32) -> &'a [u32] {
        let start = y as usize * self.stride as usize + x as usize;
        &self.data[start..start + len as usize]
    }
}

/// Blends premultiplied `src` (with extra coverage `alpha`, 0..=256) over
/// opaque `dst`.
#[inline]
pub fn over(dst: Color, src: u32, alpha: u32) -> Color {
    let a = ((src >> 24) * alpha) >> 8;
    if a == 0 && alpha == 256 && src & 0x00FF_FFFF == 0 {
        return dst;
    }
    let inv = 255 - a;
    let mix = |shift: u32| {
        let s = (((src >> shift) & 0xFF) * alpha) >> 8;
        let d = (dst >> shift) & 0xFF;
        (s + (d * inv + 127) / 255).min(255)
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Opaque colour `c` with coverage `alpha` (0..=256) over `dst`.
#[inline]
pub fn tint(dst: Color, c: Color, alpha: u32) -> Color {
    if alpha >= 256 {
        return c;
    }
    if alpha == 0 {
        return dst;
    }
    let inv = 256 - alpha;
    let mix = |shift: u32| ((((c >> shift) & 0xFF) * alpha + ((dst >> shift) & 0xFF) * inv) >> 8) & 0xFF;
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Draws `src` with its top-left corner at (`x`, `y`) and global opacity
/// `opacity` (0..=256), clipped to `clip`.
pub fn blit(dst: &mut Surface, x: i32, y: i32, src: &Pixels, opacity: u32, clip: Rect) {
    let area = Rect::new(x, y, src.width, src.height).intersect(&clip).intersect(&dst.bounds());
    if area.is_empty() || opacity == 0 {
        return;
    }
    let sx = (area.x - x) as u32;
    for row in 0..area.h {
        let dy = area.y as u32 + row;
        let sy = (area.y - y) as u32 + row;
        let s = src.row(sy, sx, area.w);
        let d = &mut dst.row_mut(dy)[area.x as usize..area.x as usize + area.w as usize];
        if !src.alpha && opacity >= 256 {
            for (o, &p) in d.iter_mut().zip(s) {
                *o = p & 0x00FF_FFFF;
            }
        } else if !src.alpha {
            for (o, &p) in d.iter_mut().zip(s) {
                *o = tint(*o, p & 0x00FF_FFFF, opacity);
            }
        } else {
            for (o, &p) in d.iter_mut().zip(s) {
                *o = over(*o, p, opacity);
            }
        }
    }
}

/// Corners for [`corner_coverage`].
pub mod corner {
    pub const TOP_LEFT: u8 = 1;
    pub const TOP_RIGHT: u8 = 2;
    pub const BOTTOM_LEFT: u8 = 4;
    pub const BOTTOM_RIGHT: u8 = 8;
    pub const TOP: u8 = TOP_LEFT | TOP_RIGHT;
    pub const BOTTOM: u8 = BOTTOM_LEFT | BOTTOM_RIGHT;
    pub const ALL: u8 = TOP | BOTTOM;
}

/// Coverage (0..=256) of pixel (`lx`, `ly`) of a `w`x`h` rectangle whose
/// `corners` are rounded with radius `r`.
pub fn corner_coverage(lx: i32, ly: i32, w: i32, h: i32, r: i32, corners: u8) -> u32 {
    if r <= 0 || lx < 0 || ly < 0 || lx >= w || ly >= h {
        return if lx >= 0 && ly >= 0 && lx < w && ly < h { 256 } else { 0 };
    }
    let (cx, which_x) = if lx < r { (r - lx, 0) } else if lx >= w - r { (lx - (w - r) + 1, 1) } else { return 256 };
    let (cy, which_y) = if ly < r { (r - ly, 0) } else if ly >= h - r { (ly - (h - r) + 1, 1) } else { return 256 };
    let bit = match (which_x, which_y) {
        (0, 0) => corner::TOP_LEFT,
        (1, 0) => corner::TOP_RIGHT,
        (0, _) => corner::BOTTOM_LEFT,
        _ => corner::BOTTOM_RIGHT,
    };
    if corners & bit == 0 {
        return 256;
    }
    let dx = ((cx as i64) << 8) - 128;
    let dy = ((cy as i64) << 8) - 128;
    let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
    ((r as i64) * 256 + 128 - dist).clamp(0, 256) as u32
}

/// Fills `r` with `c` at coverage `alpha` (0..=256), rounding `corners`.
pub fn fill_rounded(dst: &mut Surface, r: Rect, radius: u32, corners: u8, c: Color, alpha: u32, clip: Rect) {
    let area = r.intersect(&clip).intersect(&dst.bounds());
    if area.is_empty() {
        return;
    }
    let rad = radius.min(r.w / 2).min(r.h / 2) as i32;
    for py in area.y..area.y + area.h as i32 {
        let ly = py - r.y;
        let near_edge = ly < rad || ly >= r.h as i32 - rad;
        let row = dst.row_mut(py as u32);
        for px in area.x..area.x + area.w as i32 {
            let lx = px - r.x;
            let cov = if near_edge && (lx < rad || lx >= r.w as i32 - rad) {
                corner_coverage(lx, ly, r.w as i32, r.h as i32, rad, corners)
            } else {
                256
            };
            let a = (alpha * cov) >> 8;
            let p = &mut row[px as usize];
            *p = tint(*p, c, a);
        }
    }
}

/// A one-pixel outline of a rounded rectangle (for window borders).
pub fn outline_rounded(dst: &mut Surface, r: Rect, radius: u32, c: Color, alpha: u32, clip: Rect) {
    let area = r.intersect(&clip).intersect(&dst.bounds());
    if area.is_empty() || r.w < 3 || r.h < 3 {
        return;
    }
    let rad = radius.min(r.w / 2).min(r.h / 2) as i32;
    let inner = Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
    let irad = (rad - 1).max(0);
    for py in area.y..area.y + area.h as i32 {
        let ly = py - r.y;
        let edge_row = ly < rad + 1 || ly >= r.h as i32 - rad - 1;
        let row = dst.row_mut(py as u32);
        for px in area.x..area.x + area.w as i32 {
            let lx = px - r.x;
            if !edge_row && lx > 0 && lx < r.w as i32 - 1 {
                continue;
            }
            let outer = corner_coverage(lx, ly, r.w as i32, r.h as i32, rad, corner::ALL);
            let inn = corner_coverage(px - inner.x, py - inner.y, inner.w as i32, inner.h as i32, irad, corner::ALL);
            let cov = outer.saturating_sub(inn);
            if cov > 0 {
                let p = &mut row[px as usize];
                *p = tint(*p, c, (alpha * cov) >> 8);
            }
        }
    }
}

/// Precomputed falloff of a soft shadow around rounded rectangles.
pub struct ShadowTables {
    size: u32,
    radius: u32,
    /// Alpha (0..=256) by distance outside the rectangle edge.
    edge: Vec<u16>,
    /// Alpha by (dx, dy) from a corner's arc centre, `n * n` entries.
    corner: Vec<u16>,
    n: u32,
}

impl ShadowTables {
    pub fn new(size: u32, radius: u32) -> ShadowTables {
        let falloff = |d256: i64| -> u16 {
            // d256: distance outside the edge in 1/256 px.
            if d256 <= 0 {
                return 256;
            }
            let s = size as i64 * 256;
            if d256 >= s {
                return 0;
            }
            let t = 256 - d256 * 256 / s; // 256 .. 0
            ((t * t) >> 8) as u16
        };
        let edge = (0..size).map(|d| falloff(d as i64 * 256 + 128)).collect();
        let n = size + radius;
        let mut corner = vec![0u16; (n * n) as usize];
        for dy in 0..n {
            for dx in 0..n {
                let fx = dx as i64 * 256 + 128;
                let fy = dy as i64 * 256 + 128;
                let dist = isqrt((fx * fx + fy * fy) as u64) as i64 - radius as i64 * 256;
                corner[(dy * n + dx) as usize] = falloff(dist);
            }
        }
        ShadowTables { size, radius, edge, corner, n }
    }

    /// Shadow coverage (0..=256) of a pixel relative to the rounded rect `r`
    /// casting it (256 inside).
    fn alpha(&self, px: i32, py: i32, r: &Rect) -> u32 {
        let rad = self.radius.min(r.w / 2).min(r.h / 2) as i32;
        let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w as i32, r.y + r.h as i32);
        let qx = if px < x0 + rad { x0 + rad - 1 - px } else if px >= x1 - rad { px - (x1 - rad) } else { -1 };
        let qy = if py < y0 + rad { y0 + rad - 1 - py } else if py >= y1 - rad { py - (y1 - rad) } else { -1 };
        match (qx >= 0, qy >= 0) {
            (true, true) => {
                // Corner: distance from the arc centre (radius may be clipped).
                let extra = self.radius as i32 - rad;
                let (dx, dy) = ((qx + extra) as u32, (qy + extra) as u32);
                if dx >= self.n || dy >= self.n {
                    0
                } else {
                    self.corner[(dy * self.n + dx) as usize] as u32
                }
            }
            (true, false) if px < x0 || px >= x1 => {
                let d = if px < x0 { x0 - 1 - px } else { px - x1 };
                self.edge.get(d as usize).copied().unwrap_or(0) as u32
            }
            (false, true) if py < y0 || py >= y1 => {
                let d = if py < y0 { y0 - 1 - py } else { py - y1 };
                self.edge.get(d as usize).copied().unwrap_or(0) as u32
            }
            _ => 256,
        }
    }

    /// Draws the shadow cast by rounded rect `r` (black at `strength`
    /// 0..=255), skipping the pixels the window itself (`cover`, a rounded
    /// rect with the same radius) will paint over.
    pub fn draw(&self, dst: &mut Surface, r: Rect, cover: Rect, strength: u32, clip: Rect) {
        let reach = r.inflate(self.size as i32);
        let area = reach.intersect(&clip).intersect(&dst.bounds());
        if area.is_empty() || strength == 0 {
            return;
        }
        let crad = self.radius.min(cover.w / 2).min(cover.h / 2) as i32;
        let (cx0, cx1) = (cover.x, cover.right() as i32);
        for py in area.y..area.y + area.h as i32 {
            let ly = py - cover.y;
            let in_rows = ly >= 0 && ly < cover.h as i32;
            // Rows where the cover is a plain span (no rounded corner).
            let plain = in_rows && ly >= crad && ly < cover.h as i32 - crad;
            let row = dst.row_mut(py as u32);
            let mut px = area.x;
            let end = area.x + area.w as i32;
            while px < end {
                if plain && px >= cx0 && px < cx1 {
                    px = cx1; // jump over the covered span
                    continue;
                }
                if in_rows && px >= cx0 && px < cx1 {
                    let cov = corner_coverage(px - cx0, ly, cover.w as i32, cover.h as i32, crad, corner::ALL);
                    if cov == 256 {
                        px += 1;
                        continue;
                    }
                }
                let a = (self.alpha(px, py, &r) * strength) >> 8;
                if a > 0 {
                    let p = &mut row[px as usize];
                    *p = tint(*p, 0, a);
                }
                px += 1;
            }
        }
    }

    pub fn reach(&self) -> u32 {
        self.size
    }
}

// ---- pointer ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Arrow,
    /// Left/right edge.
    ResizeH,
    /// Top/bottom edge.
    ResizeV,
    /// Top-left / bottom-right corner.
    ResizeNwse,
    /// Top-right / bottom-left corner.
    ResizeNesw,
    Move,
}

const ARROW: [&str; 19] = [
    "#",
    "##",
    "#.#",
    "#..#",
    "#...#",
    "#....#",
    "#.....#",
    "#......#",
    "#.......#",
    "#........#",
    "#.........#",
    "#......####",
    "#...#..#",
    "#..##..#",
    "#.#  #..#",
    "##   #..#",
    "#     #..#",
    "      #..#",
    "       ##",
];

const RESIZE_H: [&str; 9] = [
    "   #     #   ",
    "  ##     ##  ",
    " #.#######.# ",
    "#...........#",
    " #.#######.# ",
    "  ##     ##  ",
    "   #     #   ",
    "",
    "",
];

const MOVE: [&str; 13] = [
    "      #      ",
    "     #.#     ",
    "    #...#    ",
    "     #.#     ",
    "  #  #.#  #  ",
    " ##  #.#  ## ",
    "#...........#",
    " ##  #.#  ## ",
    "  #  #.#  #  ",
    "     #.#     ",
    "    #...#    ",
    "     #.#     ",
    "      #      ",
];

const CURSOR_FILL: Color = 0xF4F6FA;
const CURSOR_EDGE: Color = 0x0B0F18;

impl CursorShape {
    /// The bitmap rows and the hot spot.
    fn bitmap(self) -> (Vec<Vec<u8>>, (i32, i32)) {
        let rows = |src: &[&str]| src.iter().map(|r| r.bytes().collect::<Vec<u8>>()).collect::<Vec<_>>();
        match self {
            CursorShape::Arrow => (rows(&ARROW), (0, 0)),
            CursorShape::ResizeH => (rows(&RESIZE_H[..7]), (6, 3)),
            CursorShape::ResizeV => (transpose(&rows(&RESIZE_H[..7])), (3, 6)),
            CursorShape::Move => (rows(&MOVE), (6, 6)),
            CursorShape::ResizeNwse => (diagonal(false), (5, 5)),
            CursorShape::ResizeNesw => (diagonal(true), (5, 5)),
        }
    }
}

fn transpose(m: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let w = m.iter().map(|r| r.len()).max().unwrap_or(0);
    (0..w).map(|x| m.iter().map(|r| r.get(x).copied().unwrap_or(b' ')).collect()).collect()
}

/// A diagonal double arrow, 11x11.
fn diagonal(mirror: bool) -> Vec<Vec<u8>> {
    let n = 11usize;
    let mut m = vec![vec![b' '; n]; n];
    for i in 0..n {
        m[i][i] = b'.';
        if i > 0 {
            m[i][i - 1] = b'#';
        }
        if i + 1 < n {
            m[i][i + 1] = b'#';
        }
    }
    // Arrow heads at both ends.
    for k in 0..4 {
        m[0][k] = if m[0][k] == b' ' { b'#' } else { m[0][k] };
        m[k][0] = if m[k][0] == b' ' { b'#' } else { m[k][0] };
        m[n - 1][n - 1 - k] = if m[n - 1][n - 1 - k] == b' ' { b'#' } else { m[n - 1][n - 1 - k] };
        m[n - 1 - k][n - 1] = if m[n - 1 - k][n - 1] == b' ' { b'#' } else { m[n - 1 - k][n - 1] };
    }
    if mirror {
        for row in &mut m {
            row.reverse();
        }
    }
    m
}

/// A rasterised pointer image.
pub struct Cursor {
    pub shape: CursorShape,
    /// (dx, dy, colour, alpha 0..=256) relative to the hot spot.
    pixels: Vec<(i8, i8, Color, u16)>,
    bounds: Rect,
}

impl Cursor {
    pub fn new(shape: CursorShape) -> Cursor {
        let (rows, (hx, hy)) = shape.bitmap();
        let mut pixels = Vec::new();
        // A soft shadow one pixel down/right first, then the shape.
        for (y, row) in rows.iter().enumerate() {
            for (x, &b) in row.iter().enumerate() {
                if b != b' ' {
                    pixels.push(((x as i32 - hx + 1) as i8, (y as i32 - hy + 2) as i8, 0, 70));
                }
            }
        }
        for (y, row) in rows.iter().enumerate() {
            for (x, &b) in row.iter().enumerate() {
                let c = match b {
                    b'#' => CURSOR_EDGE,
                    b'.' => CURSOR_FILL,
                    _ => continue,
                };
                pixels.push(((x as i32 - hx) as i8, (y as i32 - hy) as i8, c, 256));
            }
        }
        let bounds = pixels.iter().fold(Rect::EMPTY, |b, &(dx, dy, _, _)| b.union(&Rect::new(dx as i32, dy as i32, 1, 1)));
        Cursor { shape, pixels, bounds }
    }

    /// Screen area the cursor covers with its hot spot at (`x`, `y`).
    pub fn area(&self, x: i32, y: i32) -> Rect {
        Rect::new(self.bounds.x + x, self.bounds.y + y, self.bounds.w, self.bounds.h)
    }

    pub fn draw(&self, dst: &mut Surface, x: i32, y: i32, clip: Rect) {
        let clip = clip.intersect(&dst.bounds());
        for &(dx, dy, c, a) in &self.pixels {
            let (px, py) = (x + dx as i32, y + dy as i32);
            if px >= clip.x && py >= clip.y && (px as i64) < clip.right() && (py as i64) < clip.bottom() {
                let p = &mut dst.row_mut(py as u32)[px as usize];
                *p = tint(*p, c, a as u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(buf: &mut [u32], w: u32, h: u32) -> Surface<'_> {
        Surface::new(buf, w, h, w).unwrap()
    }

    #[test]
    fn premultiplied_blending() {
        // Opaque source replaces.
        assert_eq!(over(0x112233, 0xFF_AA_BB_CC, 256), 0xAABBCC);
        // Fully transparent source keeps the destination.
        assert_eq!(over(0x112233, 0x00_00_00_00, 256), 0x112233);
        // 50% white (premultiplied 0x80808080) over black: ~0x808080.
        let p = over(0x000000, 0x80_80_80_80, 256);
        assert_eq!(p, 0x808080);
        // 50% white over white stays white (no overflow).
        assert_eq!(over(0xFFFFFF, 0x80_80_80_80, 256), 0xFFFFFF);
        // Extra coverage halves the contribution.
        let q = over(0x000000, 0xFF_FF_FF_FF, 128);
        assert_eq!(q, 0x7F7F7F);
        // A premultiplied value can never exceed 255 per channel even if a
        // client sends invalid data (colour > alpha).
        assert_eq!(over(0xFFFFFF, 0x10_FF_FF_FF, 256), 0xFFFFFF);
        assert_eq!(tint(0x000000, 0xFFFFFF, 128), 0x7F7F7F);
        assert_eq!(tint(0x123456, 0xFFFFFF, 0), 0x123456);
    }

    #[test]
    fn blit_clips_to_clip_and_target() {
        let src: Vec<u32> = (0..16).map(|i| 0xFF00_0000 | i).collect();
        let px = Pixels::new(&src, 4, 4, 4, false).unwrap();
        let mut buf = vec![0xEEEEEEu32; 6 * 6];
        let mut s = surface(&mut buf, 6, 6);
        blit(&mut s, -1, 3, &px, 256, Rect::new(0, 0, 2, 6));
        // Visible: source columns 1..3 at x 0..2, rows 0..3 at y 3..6.
        assert_eq!(s.get(0, 3), Some(1));
        assert_eq!(s.get(1, 3), Some(2));
        assert_eq!(s.get(0, 5), Some(9));
        assert_eq!(s.get(2, 3), Some(0xEEEEEE), "outside the clip");
        assert_eq!(s.get(0, 2), Some(0xEEEEEE), "above the source");
        // Entirely outside: nothing happens, no panic.
        let b = s.bounds();
        blit(&mut s, 100, 100, &px, 256, b);
        blit(&mut s, -100, -100, &px, 256, b);
    }

    #[test]
    fn pixels_view_validates_geometry() {
        let data = [0u32; 10];
        assert!(Pixels::new(&data, 4, 2, 5, false).is_some());
        assert!(Pixels::new(&data, 4, 3, 4, false).is_none());
        assert!(Pixels::new(&data, 5, 1, 4, false).is_none());
        assert!(Pixels::new(&data, 1, u32::MAX, u32::MAX, false).is_none(), "overflow is rejected");
    }

    #[test]
    fn rounded_corners_are_masked_and_smooth() {
        assert_eq!(corner_coverage(0, 0, 20, 20, 8, corner::ALL), 0);
        assert_eq!(corner_coverage(10, 10, 20, 20, 8, corner::ALL), 256);
        assert_eq!(corner_coverage(0, 0, 20, 20, 8, corner::BOTTOM), 256, "top corners not rounded");
        assert_eq!(corner_coverage(19, 19, 20, 20, 8, corner::BOTTOM), 0);
        let edge = corner_coverage(1, 3, 20, 20, 8, corner::ALL);
        assert!(edge > 0 && edge < 256);
        assert_eq!(corner_coverage(-1, 5, 20, 20, 8, corner::ALL), 0);
        let mut buf = vec![0u32; 20 * 20];
        let mut s = surface(&mut buf, 20, 20);
        let b = s.bounds();
        fill_rounded(&mut s, Rect::new(0, 0, 20, 20), 8, corner::TOP, 0xFFFFFF, 256, b);
        assert_eq!(s.get(0, 0), Some(0));
        assert_eq!(s.get(0, 19), Some(0xFFFFFF));
        assert_eq!(s.get(10, 10), Some(0xFFFFFF));
    }

    #[test]
    fn shadow_surrounds_but_never_covers_the_rect() {
        let t = ShadowTables::new(10, 6);
        let mut buf = vec![0xFFFFFFu32; 60 * 60];
        let mut s = surface(&mut buf, 60, 60);
        let r = Rect::new(20, 20, 20, 20);
        let b = s.bounds();
        t.draw(&mut s, r, r, 255, b);
        assert_eq!(s.get(30, 30), Some(0xFFFFFF), "inside untouched");
        let near = s.get(30, 41).unwrap() & 0xFF;
        let far = s.get(30, 48).unwrap() & 0xFF;
        assert!(near < far && far < 0xFF, "darker close to the edge: {near} {far}");
        assert_eq!(s.get(30, 55), Some(0xFFFFFF), "beyond the reach");
        assert_eq!(s.get(5, 5), Some(0xFFFFFF));
        // Symmetric left/right.
        assert_eq!(s.get(18, 30), s.get(41, 30));
        // The rounded corner area outside the arc gets shadow too.
        assert_ne!(s.get(20, 20), Some(0xFFFFFF));
        // A clip limits the work.
        let mut buf = vec![0xFFFFFFu32; 60 * 60];
        let mut s = surface(&mut buf, 60, 60);
        t.draw(&mut s, r, r, 255, Rect::new(0, 0, 25, 60));
        assert_eq!(s.get(41, 30), Some(0xFFFFFF));
        // An offset shadow still darkens the band below the window.
        let mut buf = vec![0xFFFFFFu32; 60 * 60];
        let mut s = surface(&mut buf, 60, 60);
        let b = s.bounds();
        t.draw(&mut s, Rect::new(20, 26, 20, 20), r, 255, b);
        assert_ne!(s.get(30, 41), Some(0xFFFFFF));
        assert_eq!(s.get(30, 30), Some(0xFFFFFF));
    }

    #[test]
    fn cursor_shapes_draw_inside_their_area() {
        for shape in [
            CursorShape::Arrow,
            CursorShape::ResizeH,
            CursorShape::ResizeV,
            CursorShape::ResizeNwse,
            CursorShape::ResizeNesw,
            CursorShape::Move,
        ] {
            let c = Cursor::new(shape);
            let mut buf = vec![0x404040u32; 64 * 64];
            let mut s = surface(&mut buf, 64, 64);
            let b = s.bounds();
            c.draw(&mut s, 30, 30, b);
            let area = c.area(30, 30);
            let mut ink = 0;
            for y in 0..64 {
                for x in 0..64 {
                    if s.get(x, y) != Some(0x404040) {
                        ink += 1;
                        assert!(x >= area.x && (x as i64) < area.right() && y >= area.y && (y as i64) < area.bottom(), "{shape:?}");
                    }
                }
            }
            assert!(ink > 20, "{shape:?} is visible");
            // Off-screen drawing is clipped without panicking.
            c.draw(&mut s, -5, 62, b);
        }
        let arrow = Cursor::new(CursorShape::Arrow);
        assert_eq!(arrow.area(0, 0).x, 0, "arrow hot spot is its tip");
    }
}
