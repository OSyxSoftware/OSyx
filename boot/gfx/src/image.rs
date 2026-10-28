//! Straight-alpha RGBA images in the `OSXI` format with bilinear scaling.

use crate::text::{read_u32, AssetError};
use crate::{rgb, Rect, Surface};

#[derive(Clone, Copy)]
pub struct Image<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
    /// Every texel has alpha 255, so scaling can skip blending and
    /// un-premultiplication.
    opaque: bool,
}

impl<'a> Image<'a> {
    const HEADER: usize = 12;

    pub fn parse(bytes: &'a [u8]) -> Result<Self, AssetError> {
        if bytes.get(0..4) != Some(b"OSXI") {
            return Err(AssetError::BadMagic);
        }
        let width = read_u32(bytes, 4)?;
        let height = read_u32(bytes, 8)?;
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(AssetError::BadDimensions);
        }
        let len = width as usize * height as usize * 4;
        let rgba = bytes.get(Self::HEADER..Self::HEADER + len).ok_or(AssetError::Truncated)?;
        let opaque = rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
        Ok(Image { width, height, rgba, opaque })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Premultiplied sample at integer coordinates (clamped to the edge).
    #[inline]
    fn sample(&self, x: i32, y: i32) -> [u32; 4] {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        let i = (y * self.width as usize + x) * 4;
        let p = &self.rgba[i..i + 4];
        let a = p[3] as u32;
        [p[0] as u32 * a, p[1] as u32 * a, p[2] as u32 * a, a]
    }

    /// Draws the image scaled into `dst` with bilinear filtering. `alpha`
    /// (0..=255) multiplies the image's own alpha. Returns the touched area.
    pub fn draw_scaled(&self, s: &mut Surface, dst: Rect, alpha: u32) -> Rect {
        if dst.is_empty() || alpha == 0 {
            return Rect::EMPTY;
        }
        let area = dst.intersect(&s.bounds());
        // Source step per destination pixel in 16.16 fixed point.
        let step_x = ((self.width as u64) << 16) / dst.w as u64;
        let step_y = ((self.height as u64) << 16) / dst.h as u64;
        if self.opaque && alpha == 255 {
            return self.draw_scaled_opaque(s, dst, area, step_x, step_y);
        }
        for y in area.y..area.y + area.h as i32 {
            // Map the destination pixel centre to source space, minus half a texel.
            let sy = ((y - dst.y) as i64 * step_y as i64) + (step_y as i64 >> 1) - 0x8000;
            let y0 = (sy >> 16) as i32;
            let fy = ((sy >> 8) & 0xFF) as u32;
            for x in area.x..area.x + area.w as i32 {
                let sx = ((x - dst.x) as i64 * step_x as i64) + (step_x as i64 >> 1) - 0x8000;
                let x0 = (sx >> 16) as i32;
                let fx = ((sx >> 8) & 0xFF) as u32;
                let p00 = self.sample(x0, y0);
                let p10 = self.sample(x0 + 1, y0);
                let p01 = self.sample(x0, y0 + 1);
                let p11 = self.sample(x0 + 1, y0 + 1);
                let w00 = (256 - fx) * (256 - fy);
                let w10 = fx * (256 - fy);
                let w01 = (256 - fx) * fy;
                let w11 = fx * fy;
                let mut acc = [0u64; 4];
                for c in 0..4 {
                    acc[c] = p00[c] as u64 * w00 as u64
                        + p10[c] as u64 * w10 as u64
                        + p01[c] as u64 * w01 as u64
                        + p11[c] as u64 * w11 as u64;
                }
                let a_sum = acc[3]; // alpha * 65536
                if a_sum == 0 {
                    continue;
                }
                // Un-premultiply: colour = Σ(c·a·w) / Σ(a·w).
                let r = (acc[0] / a_sum).min(255) as u8;
                let g = (acc[1] / a_sum).min(255) as u8;
                let b = (acc[2] / a_sum).min(255) as u8;
                let a = ((a_sum >> 16) as u32 * alpha + 127) / 255;
                s.blend_px(x, y, rgb(r, g, b), a);
            }
        }
        area
    }

    /// Fully opaque image and no extra alpha: every texel contributes its
    /// colour with weight 65536 in total, so a shift replaces the divisions
    /// and the destination pixel is simply overwritten.
    fn draw_scaled_opaque(
        &self,
        s: &mut Surface,
        dst: Rect,
        area: Rect,
        step_x: u64,
        step_y: u64,
    ) -> Rect {
        let sw = self.width as i32;
        let sh = self.height as i32;
        let stride = self.width as usize * 4;
        for y in area.y..area.y + area.h as i32 {
            let sy = ((y - dst.y) as i64 * step_y as i64) + (step_y as i64 >> 1) - 0x8000;
            let y0 = (sy >> 16) as i32;
            let fy = ((sy >> 8) & 0xFF) as u32;
            let row0 = {
                let at = y0.clamp(0, sh - 1) as usize * stride;
                &self.rgba[at..at + stride]
            };
            let row1 = {
                let at = (y0 + 1).clamp(0, sh - 1) as usize * stride;
                &self.rgba[at..at + stride]
            };
            let wy0 = 256 - fy;
            let wy1 = fy;
            let row = y as usize * s.stride as usize + area.x as usize;
            for (at, x) in (row..).zip(area.x..area.x + area.w as i32) {
                let sx = ((x - dst.x) as i64 * step_x as i64) + (step_x as i64 >> 1) - 0x8000;
                let x0 = (sx >> 16) as i32;
                let fx = ((sx >> 8) & 0xFF) as u32;
                let c0 = x0.clamp(0, sw - 1) as usize * 4;
                let c1 = (x0 + 1).clamp(0, sw - 1) as usize * 4;
                let w00 = (256 - fx) * wy0;
                let w10 = fx * wy0;
                let w01 = (256 - fx) * wy1;
                let w11 = fx * wy1;
                let r = (row0[c0] as u32 * w00 + row0[c1] as u32 * w10
                    + row1[c0] as u32 * w01
                    + row1[c1] as u32 * w11)
                    >> 16;
                let g = (row0[c0 + 1] as u32 * w00 + row0[c1 + 1] as u32 * w10
                    + row1[c0 + 1] as u32 * w01
                    + row1[c1 + 1] as u32 * w11)
                    >> 16;
                let b = (row0[c0 + 2] as u32 * w00 + row0[c1 + 2] as u32 * w10
                    + row1[c0 + 2] as u32 * w01
                    + row1[c1 + 2] as u32 * w11)
                    >> 16;
                s.pixels[at] = rgb(r as u8, g as u8, b as u8);
            }
        }
        area
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    static LOGO: &[u8] = include_bytes!("../../assets/logo.osxi");

    fn make(w: u32, h: u32, px: &[[u8; 4]]) -> std::vec::Vec<u8> {
        let mut v = b"OSXI".to_vec();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        for p in px {
            v.extend_from_slice(p);
        }
        v
    }

    #[test]
    fn parses_logo() {
        let img = Image::parse(LOGO).unwrap();
        assert_eq!((img.width(), img.height()), (320, 320));
    }

    #[test]
    fn rejects_malformed_images() {
        assert_eq!(Image::parse(b"OSXIxxxx").err(), Some(AssetError::Truncated));
        assert_eq!(Image::parse(&make(0, 1, &[])).err(), Some(AssetError::BadDimensions));
        assert_eq!(Image::parse(&make(2, 2, &[[0; 4]; 3])).err(), Some(AssetError::Truncated));
        assert_eq!(Image::parse(b"PNG?").err(), Some(AssetError::BadMagic));
    }

    #[test]
    fn identity_scale_of_opaque_image_is_exact() {
        let px = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [9, 8, 7, 255]];
        let bytes = make(2, 2, &px);
        let img = Image::parse(&bytes).unwrap();
        let mut buf = vec![0u32; 4];
        let mut s = Surface::new(&mut buf, 2, 2, 2).unwrap();
        img.draw_scaled(&mut s, Rect::new(0, 0, 2, 2), 255);
        assert_eq!(buf, vec![0xFF0000, 0x00FF00, 0x0000FF, 0x090807]);
    }

    #[test]
    fn transparent_texels_do_not_bleed_colour() {
        // A fully transparent red texel next to opaque white must not tint the
        // filtered result (this is what premultiplication guarantees).
        let px = [[255, 0, 0, 0], [255, 255, 255, 255]];
        let bytes = make(2, 1, &px);
        let img = Image::parse(&bytes).unwrap();
        let mut buf = vec![0u32; 8];
        let mut s = Surface::new(&mut buf, 8, 1, 8).unwrap();
        img.draw_scaled(&mut s, Rect::new(0, 0, 8, 1), 255);
        for &p in &buf {
            let (r, g, b) = crate::channels(p);
            assert!(r == g && g == b, "pixel {p:06x} is tinted");
        }
    }
}
