//! Allocation-free 2D rendering used by the OSyx bootloader and kernel.
//!
//! All drawing happens on a [`Surface`] of canonical `0x00RRGGBB` pixels. The
//! crate deliberately uses integer / fixed-point arithmetic only: the UEFI and
//! kernel targets are compiled without SSE, so floating point would be
//! emulated in software.

#![no_std]

pub mod image;
pub mod prop;
pub mod splash;
pub mod text;

pub use image::Image;
pub use prop::PropFont;
pub use text::Font;

/// Canonical pixel: `0x00RRGGBB`.
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

pub const fn channels(c: Color) -> (u32, u32, u32) {
    ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

/// Exact `x / 255` for `x <= 255 * 255`, rounded to nearest.
#[inline]
const fn div255(x: u32) -> u32 {
    (x + 128 + ((x + 128) >> 8)) >> 8
}

/// Blends `src` over `dst` with coverage `alpha` (0 = dst, 255 = src).
#[inline]
pub const fn blend(dst: Color, src: Color, alpha: u32) -> Color {
    if alpha == 0 {
        return dst;
    }
    if alpha >= 255 {
        return src;
    }
    let (dr, dg, db) = channels(dst);
    let (sr, sg, sb) = channels(src);
    let inv = 255 - alpha;
    let r = div255(sr * alpha + dr * inv);
    let g = div255(sg * alpha + dg * inv);
    let b = div255(sb * alpha + db * inv);
    (r << 16) | (g << 8) | b
}

/// Linear interpolation, `t` in `0..=256`.
pub const fn lerp(a: Color, b: Color, t: u32) -> Color {
    let t = if t > 256 { 256 } else { t };
    let (ar, ag, ab) = channels(a);
    let (br, bg, bb) = channels(b);
    let r = (ar * (256 - t) + br * t) >> 8;
    let g = (ag * (256 - t) + bg * t) >> 8;
    let bl = (ab * (256 - t) + bb * t) >> 8;
    (r << 16) | (g << 8) | bl
}

/// Integer square root (floor).
pub const fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const EMPTY: Rect = Rect { x: 0, y: 0, w: 0, h: 0 };

    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub const fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    pub const fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = (self.x as i64).max(o.x as i64);
        let y0 = (self.y as i64).max(o.y as i64);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Rect::EMPTY;
        }
        Rect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = (self.x as i64).min(o.x as i64);
        let y0 = (self.y as i64).min(o.y as i64);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        Rect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    /// Grows the rectangle by `d` pixels on every side.
    pub fn inflate(&self, d: i32) -> Rect {
        let w = (self.w as i64 + 2 * d as i64).max(0) as u32;
        let h = (self.h as i64 + 2 * d as i64).max(0) as u32;
        Rect::new(self.x - d, self.y - d, w, h)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SurfaceError {
    StrideTooSmall,
    BufferTooSmall,
}

/// A mutable view over `height` rows of `stride` pixels each.
pub struct Surface<'a> {
    pixels: &'a mut [Color],
    width: u32,
    height: u32,
    stride: u32,
}

impl<'a> Surface<'a> {
    pub fn new(pixels: &'a mut [Color], width: u32, height: u32, stride: u32) -> Result<Self, SurfaceError> {
        if stride < width {
            return Err(SurfaceError::StrideTooSmall);
        }
        let needed = stride as u64 * height as u64;
        if (pixels.len() as u64) < needed {
            return Err(SurfaceError::BufferTooSmall);
        }
        Ok(Surface { pixels, width, height, stride })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// A view of `r` (clipped to the surface) whose origin is `r`'s top-left
    /// corner; it shares the pixels and the stride. `None` if nothing is left.
    pub fn into_sub(self, r: Rect) -> Option<Surface<'a>> {
        let r = r.intersect(&self.bounds());
        if r.w == 0 || r.h == 0 {
            return None;
        }
        let start = r.y as usize * self.stride as usize + r.x as usize;
        let end = start + (r.h as usize - 1) * self.stride as usize + r.w as usize;
        Some(Surface { pixels: &mut self.pixels[start..end], width: r.w, height: r.h, stride: self.stride })
    }

    pub fn pixels(&self) -> &[Color] {
        self.pixels
    }

    pub fn row(&self, y: u32) -> &[Color] {
        let start = (y * self.stride) as usize;
        &self.pixels[start..start + self.width as usize]
    }

    /// Row `y` (`width` pixels) for in-place writing.
    pub fn row_mut(&mut self, y: u32) -> &mut [Color] {
        let start = y as usize * self.stride as usize;
        &mut self.pixels[start..start + self.width as usize]
    }

    #[inline]
    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        Some(y as usize * self.stride as usize + x as usize)
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Color> {
        self.idx(x, y).map(|i| self.pixels[i])
    }

    #[inline]
    pub fn put(&mut self, x: i32, y: i32, c: Color) {
        if let Some(i) = self.idx(x, y) {
            self.pixels[i] = c;
        }
    }

    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: Color, alpha: u32) {
        if let Some(i) = self.idx(x, y) {
            self.pixels[i] = blend(self.pixels[i], c, alpha);
        }
    }

    pub fn fill(&mut self, c: Color) {
        let r = self.bounds();
        self.fill_rect(r, c);
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        let r = r.intersect(&self.bounds());
        for y in r.y..r.y + r.h as i32 {
            let start = y as usize * self.stride as usize + r.x as usize;
            self.pixels[start..start + r.w as usize].fill(c);
        }
    }

    pub fn blend_rect(&mut self, r: Rect, c: Color, alpha: u32) {
        let r = r.intersect(&self.bounds());
        for y in r.y..r.y + r.h as i32 {
            let start = y as usize * self.stride as usize + r.x as usize;
            for p in &mut self.pixels[start..start + r.w as usize] {
                *p = blend(*p, c, alpha);
            }
        }
    }

    /// Vertical gradient from `top` (first row of `r`) to `bottom` (last row).
    pub fn gradient_v(&mut self, r: Rect, top: Color, bottom: Color) {
        let clipped = r.intersect(&self.bounds());
        let span = r.h.max(2) - 1;
        for y in clipped.y..clipped.y + clipped.h as i32 {
            let t = ((y - r.y) as u32 * 256) / span;
            let c = lerp(top, bottom, t);
            self.fill_rect(Rect::new(clipped.x, y, clipped.w, 1), c);
        }
    }

    /// Soft radial glow: `color` blended with `max_alpha * (1 - d²/R²)`.
    pub fn radial_glow(&mut self, cx: i32, cy: i32, radius: u32, color: Color, max_alpha: u32) {
        let r = radius as i64;
        let r2 = (r * r).max(1);
        let area = Rect::new(cx - radius as i32, cy - radius as i32, radius * 2, radius * 2);
        let area = area.intersect(&self.bounds());
        for y in area.y..area.y + area.h as i32 {
            let dy = (y - cy) as i64;
            for x in area.x..area.x + area.w as i32 {
                let dx = (x - cx) as i64;
                let d2 = dx * dx + dy * dy;
                if d2 >= r2 {
                    continue;
                }
                let t = (256 - (d2 * 256 / r2)) as u32;
                // Square the falloff for a smoother edge.
                let a = (max_alpha * ((t * t) >> 8)) >> 8;
                self.blend_px(x, y, color, a);
            }
        }
    }

    /// Filled rectangle with anti-aliased corners of radius `radius`.
    pub fn rounded_rect(&mut self, r: Rect, radius: u32, c: Color, alpha: u32) {
        if r.is_empty() {
            return;
        }
        let radius = radius.min(r.w / 2).min(r.h / 2) as i32;
        let clipped = r.intersect(&self.bounds());
        let rad_fx = (radius as i64) << 8;
        for y in clipped.y..clipped.y + clipped.h as i32 {
            for x in clipped.x..clipped.x + clipped.w as i32 {
                // Distance of the pixel centre (8.8 fixed) into the corner region.
                let lx = x - r.x;
                let ly = y - r.y;
                let cx = if lx < radius {
                    radius - lx
                } else if lx >= r.w as i32 - radius {
                    lx - (r.w as i32 - radius) + 1
                } else {
                    0
                };
                let cy = if ly < radius {
                    radius - ly
                } else if ly >= r.h as i32 - radius {
                    ly - (r.h as i32 - radius) + 1
                } else {
                    0
                };
                let cov = if cx > 0 && cy > 0 {
                    let dx = ((cx as i64) << 8) - 128;
                    let dy = ((cy as i64) << 8) - 128;
                    let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
                    // Coverage of a 1px wide edge band centred on the radius.
                    (rad_fx + 128 - dist).clamp(0, 256) as u32
                } else {
                    256
                };
                let a = (alpha * cov) >> 8;
                self.blend_px(x, y, c, a);
            }
        }
    }

    /// Anti-aliased filled disc. Centre and radius are in 8.8 fixed point.
    pub fn disc(&mut self, cx_fx: i32, cy_fx: i32, r_fx: i32, c: Color, alpha: u32) {
        let x0 = (cx_fx - r_fx) >> 8;
        let y0 = (cy_fx - r_fx) >> 8;
        let x1 = (cx_fx + r_fx + 255) >> 8;
        let y1 = (cy_fx + r_fx + 255) >> 8;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = ((x << 8) + 128 - cx_fx) as i64;
                let dy = ((y << 8) + 128 - cy_fx) as i64;
                let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
                let cov = (r_fx as i64 + 128 - dist).clamp(0, 256) as u32;
                if cov > 0 {
                    self.blend_px(x, y, c, (alpha * cov) >> 8);
                }
            }
        }
    }

    /// One-pixel Bresenham line, inclusive of both end points.
    pub fn line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, c: Color) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.put(x0, y0, c);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Copies `r` from `src` (which must have the same dimensions) into `self`.
    pub fn copy_rect_from(&mut self, src: &Surface, r: Rect) {
        let r = r.intersect(&self.bounds()).intersect(&src.bounds());
        for y in r.y..r.y + r.h as i32 {
            let d = y as usize * self.stride as usize + r.x as usize;
            let s = y as usize * src.stride as usize + r.x as usize;
            self.pixels[d..d + r.w as usize].copy_from_slice(&src.pixels[s..s + r.w as usize]);
        }
    }
}

/// Converts a canonical pixel to the framebuffer's byte order. Canonical
/// `0x00RRGGBB` already is the BGR (blue byte first) layout.
#[inline]
pub const fn to_device(p: Color, rgb: bool) -> u32 {
    if rgb {
        (p & 0x00FF00) | (p >> 16 & 0xFF) | ((p & 0xFF) << 16)
    } else {
        p
    }
}

/// Copies rectangle `r` of `src` to a linear 32-bpp framebuffer.
///
/// # Safety
/// `dst` must be valid for volatile writes of `dst_stride * src.height()`
/// pixels and `dst_stride >= src.width()`.
pub unsafe fn present(src: &Surface, r: Rect, dst: *mut u32, dst_stride: u32, rgb: bool) {
    let r = r.intersect(&src.bounds());
    for y in r.y..r.y + r.h as i32 {
        let row = &src.row(y as u32)[r.x as usize..r.x as usize + r.w as usize];
        let out = dst.add(y as usize * dst_stride as usize + r.x as usize);
        for (i, &p) in row.iter().enumerate() {
            out.add(i).write_volatile(to_device(p, rgb));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_conversion_and_present() {
        assert_eq!(to_device(0x112233, false), 0x112233);
        assert_eq!(to_device(0x112233, true), 0x332211);
        let mut src = [0x112233u32, 0x445566, 0x778899, 0xAABBCC];
        let s = Surface::new(&mut src, 2, 2, 2).unwrap();
        let mut fb = [0u32; 3 * 2];
        unsafe { present(&s, Rect::new(1, 0, 1, 2), fb.as_mut_ptr(), 3, true) };
        assert_eq!(fb, [0, 0x665544, 0, 0, 0xCCBBAA, 0]);
    }

    #[test]
    fn blend_endpoints_and_midpoint() {
        assert_eq!(blend(0x000000, 0xFFFFFF, 0), 0x000000);
        assert_eq!(blend(0x000000, 0xFFFFFF, 255), 0xFFFFFF);
        assert_eq!(blend(0x000000, 0xFF8000, 128), rgb(128, 64, 0));
        for x in 0..=255u32 * 255 {
            assert_eq!(div255(x), (x + 127) / 255, "x = {x}");
        }
    }

    #[test]
    fn isqrt_is_floor_sqrt() {
        for n in 0..10_000u64 {
            let r = isqrt(n);
            assert!(r * r <= n && (r + 1) * (r + 1) > n, "n = {n}");
        }
        assert_eq!(isqrt(u32::MAX as u64 * u32::MAX as u64), u32::MAX as u64);
    }

    #[test]
    fn surface_rejects_bad_geometry() {
        let mut buf = [0u32; 10];
        assert_eq!(Surface::new(&mut buf, 4, 2, 3).err(), Some(SurfaceError::StrideTooSmall));
        assert_eq!(Surface::new(&mut buf, 4, 3, 4).err(), Some(SurfaceError::BufferTooSmall));
        assert!(Surface::new(&mut buf, 4, 2, 5).is_ok());
    }

    #[test]
    fn fill_rect_clips_and_respects_stride() {
        let mut buf = [0u32; 6 * 4];
        let mut s = Surface::new(&mut buf, 5, 4, 6).unwrap();
        s.fill_rect(Rect::new(-2, 2, 100, 100), 0xABCDEF);
        for y in 0..4 {
            for x in 0..6 {
                let v = buf[y * 6 + x];
                let expect = if y >= 2 && x < 5 { 0xABCDEF } else { 0 };
                assert_eq!(v, expect, "({x},{y})");
            }
        }
    }

    #[test]
    fn rect_algebra() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
        assert_eq!(Rect::EMPTY.union(&b), b);
        assert_eq!(a.inflate(2), Rect::new(-2, -2, 14, 14));
    }

    #[test]
    fn rounded_rect_corners_are_soft() {
        let mut buf = [0u32; 20 * 20];
        let mut s = Surface::new(&mut buf, 20, 20, 20).unwrap();
        s.rounded_rect(Rect::new(0, 0, 20, 20), 8, 0xFFFFFF, 255);
        assert_eq!(s.get(10, 10), Some(0xFFFFFF));
        assert_eq!(s.get(0, 10), Some(0xFFFFFF));
        assert_eq!(s.get(0, 0), Some(0x000000), "outer corner must stay empty");
        let edge = s.get(1, 3).unwrap() & 0xFF;
        assert!(edge > 0 && edge < 255, "corner arc must be anti-aliased, got {edge}");
    }

    #[test]
    fn line_hits_endpoints() {
        let mut buf = [0u32; 16 * 16];
        let mut s = Surface::new(&mut buf, 16, 16, 16).unwrap();
        s.line(1, 2, 14, 9, 1);
        assert_eq!(s.get(1, 2), Some(1));
        assert_eq!(s.get(14, 9), Some(1));
        assert_eq!(buf.iter().filter(|&&p| p == 1).count(), 14);
    }

    #[test]
    fn disc_is_symmetric() {
        let mut buf = [0u32; 21 * 21];
        let mut s = Surface::new(&mut buf, 21, 21, 21).unwrap();
        s.disc(10 * 256 + 128, 10 * 256 + 128, 6 * 256, 0xFFFFFF, 255);
        assert_eq!(s.get(10, 10), Some(0xFFFFFF));
        assert_eq!(s.get(3, 10), s.get(17, 10));
        assert_eq!(s.get(10, 3), s.get(10, 17));
        assert_eq!(s.get(0, 0), Some(0));
    }

    #[test]
    fn sub_surface_shares_pixels_and_clips_to_the_parent() {
        let mut px = [0u32; 6 * 4];
        let s = Surface::new(&mut px, 6, 4, 6).unwrap();
        let mut sub = s.into_sub(Rect::new(4, 2, 5, 5)).unwrap();
        assert_eq!((sub.width(), sub.height()), (2, 2));
        sub.fill(7);
        sub.put(2, 0, 9);
        assert_eq!(px, [
            0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 7, 7,
            0, 0, 0, 0, 7, 7,
        ]);
        let s = Surface::new(&mut px, 6, 4, 6).unwrap();
        assert!(s.into_sub(Rect::new(6, 0, 3, 3)).is_none());
    }
}
