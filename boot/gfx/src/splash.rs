//! The OSyx boot screen: layout, palette and individually redrawable layers.
//!
//! Callers keep two surfaces of identical size: `bg` holds the static
//! background drawn once by [`draw_background`]; the frame surface is what gets
//! presented. Every dynamic layer first restores its area from `bg` and
//! returns the dirty rectangle, so animation only touches a few thousand
//! pixels per frame.

use crate::text::AssetError;
use crate::{rgb, Color, Font, Image, Rect, Surface};

pub const BG_TOP: Color = rgb(17, 20, 27);
pub const BG_BOTTOM: Color = rgb(7, 9, 13);
pub const GLOW: Color = rgb(36, 78, 190);
pub const TEXT: Color = rgb(206, 213, 226);
pub const TEXT_DIM: Color = rgb(104, 114, 133);
pub const TRACK: Color = rgb(36, 42, 54);
pub const ACCENT: Color = rgb(76, 141, 255);
pub const ERROR: Color = rgb(255, 98, 112);
pub const OK: Color = rgb(96, 214, 150);

static LOGO: &[u8] = include_bytes!("../../assets/logo.osxi");
static MONO16: &[u8] = include_bytes!("../../assets/mono16.osxf");
static MONO22: &[u8] = include_bytes!("../../assets/mono22.osxf");

#[derive(Clone, Copy)]
pub struct Assets<'a> {
    pub logo: Image<'a>,
    pub small: Font<'a>,
    pub large: Font<'a>,
}

impl Assets<'static> {
    /// Parses the assets embedded in this crate.
    pub fn embedded() -> Result<Self, AssetError> {
        Ok(Assets { logo: Image::parse(LOGO)?, small: Font::parse(MONO16)?, large: Font::parse(MONO22)? })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub logo: Rect,
    pub bar: Rect,
    pub status_y: i32,
    pub detail_y: i32,
    pub spinner_cx: i32,
    pub spinner_cy: i32,
    /// UI scale in 8.8 fixed point (256 = 1.0).
    pub scale: u32,
    pub footer_y: i32,
}

impl Layout {
    pub fn new(width: u32, height: u32, assets: &Assets) -> Layout {
        let w = width as i32;
        let h = height as i32;
        let scale = (height * 256 / 800).clamp(192, 768);
        let logo_size = (h * 30 / 100).min(w * 26 / 100).clamp(64, 400);
        let logo = Rect::new(w / 2 - logo_size / 2, h * 40 / 100 - logo_size / 2, logo_size as u32, logo_size as u32);
        let bar_w = (w * 30 / 100).clamp(160.min(w - 16), 440);
        let bar_h = (h / 200).max(4);
        let bar_y = logo.bottom() as i32 + (h * 6 / 100).max(16);
        let bar = Rect::new(w / 2 - bar_w / 2, bar_y, bar_w as u32, bar_h as u32);
        let status_font = Self::status_font_for(height, assets);
        let status_y = bar.bottom() as i32 + (14 * scale as i32) / 256;
        let detail_y = status_y + status_font.line_height() as i32 + 12;
        let footer_y = h - assets.small.line_height() as i32 - 14;
        // Centre the spinner between the (at most three) detail lines and the footer.
        let details_bottom = detail_y + 3 * assets.small.line_height() as i32;
        let spinner_cy = (details_bottom + footer_y) / 2;
        Layout { width, height, logo, bar, status_y, detail_y, spinner_cx: w / 2, spinner_cy, scale, footer_y }
    }

    fn status_font_for<'a>(height: u32, assets: &Assets<'a>) -> Font<'a> {
        if height >= 900 {
            assets.large
        } else {
            assets.small
        }
    }

    pub fn status_font<'a>(&self, assets: &Assets<'a>) -> Font<'a> {
        Self::status_font_for(self.height, assets)
    }

    pub fn spinner_bounds(&self) -> Rect {
        let r = (22 * self.scale as i32) / 256;
        Rect::new(self.spinner_cx - r, self.spinner_cy - r, 2 * r as u32, 2 * r as u32)
    }
}

/// Draws the static background (gradient, glow, footer) into `s`.
pub fn draw_background(s: &mut Surface, l: &Layout, a: &Assets, footer_left: &str, footer_right: &str) {
    let full = s.bounds();
    s.gradient_v(full, BG_TOP, BG_BOTTOM);
    let cx = l.logo.x + l.logo.w as i32 / 2;
    let cy = l.logo.y + l.logo.h as i32 / 2;
    s.radial_glow(cx, cy, l.logo.w * 11 / 10, GLOW, 38);
    a.small.draw(s, 18, l.footer_y, footer_left, TEXT_DIM, 255);
    let rw = a.small.measure(footer_right) as i32;
    a.small.draw(s, l.width as i32 - 18 - rw, l.footer_y, footer_right, TEXT_DIM, 255);
}

/// Logo layer, `alpha` 0..=255 for the fade-in.
pub fn draw_logo(s: &mut Surface, bg: &Surface, l: &Layout, a: &Assets, alpha: u32) -> Rect {
    s.copy_rect_from(bg, l.logo);
    a.logo.draw_scaled(s, l.logo, alpha);
    l.logo
}

/// Progress bar layer, `permille` in 0..=1000.
pub fn draw_progress(s: &mut Surface, bg: &Surface, l: &Layout, permille: u32, fill: Color) -> Rect {
    let area = l.bar.inflate(1);
    s.copy_rect_from(bg, area);
    let radius = l.bar.h / 2;
    s.rounded_rect(l.bar, radius, TRACK, 255);
    let fw = (l.bar.w as u64 * permille.min(1000) as u64 / 1000) as u32;
    if fw > 0 {
        let fw = fw.max(l.bar.h);
        s.rounded_rect(Rect::new(l.bar.x, l.bar.y, fw, l.bar.h), radius, fill, 255);
    }
    area
}

/// Status line layer, centred under the bar.
pub fn draw_status(s: &mut Surface, bg: &Surface, l: &Layout, a: &Assets, text: &str, c: Color) -> Rect {
    let font = l.status_font(a);
    let band = Rect::new(0, l.status_y, l.width, font.line_height());
    s.copy_rect_from(bg, band);
    font.draw_centered(s, l.width as i32 / 2, l.status_y, text, c, 255);
    band
}

/// Up to a few lines of small text under the status line (error details).
pub fn draw_details(s: &mut Surface, bg: &Surface, l: &Layout, a: &Assets, lines: &[&str], c: Color) -> Rect {
    let lh = a.small.line_height() as i32;
    let band = Rect::new(0, l.detail_y, l.width, (lh * lines.len() as i32) as u32);
    s.copy_rect_from(bg, band);
    for (i, line) in lines.iter().enumerate() {
        a.small.draw_centered(s, l.width as i32 / 2, l.detail_y + i as i32 * lh, line, c, 255);
    }
    band
}

/// sin/cos of k·30° scaled by 1024, for k = 0..12.
const DIRS: [(i32, i32); 12] = [
    (0, -1024),
    (512, -887),
    (887, -512),
    (1024, 0),
    (887, 512),
    (512, 887),
    (0, 1024),
    (-512, 887),
    (-887, 512),
    (-1024, 0),
    (-887, -512),
    (-512, -887),
];

/// Twelve-dot spinner. `phase` advances by 16 per dot step (so it can move in
/// sub-steps); `alpha` fades the whole spinner.
pub fn draw_spinner(s: &mut Surface, bg: &Surface, l: &Layout, phase: u32, alpha: u32, c: Color) -> Rect {
    let bounds = l.spinner_bounds();
    s.copy_rect_from(bg, bounds);
    if alpha == 0 {
        return bounds;
    }
    let ring = (15 * l.scale as i32) / 256; // pixels
    let dot = (24 * l.scale as i32) / 10; // 8.8 fixed radius (~2.4 px at 1.0)
    let head = (phase / 16) % 12;
    for (k, &(sx, sy)) in DIRS.iter().enumerate() {
        let behind = (head + 12 - k as u32) % 12;
        let t = 12 - behind; // 12 for the head, 1 for the tail
        let a = (alpha * (24 + 231 * t * t / 144)) / 255;
        let cx = (l.spinner_cx << 8) + 128 + (sx * ring * 256) / 1024;
        let cy = (l.spinner_cy << 8) + 128 + (sy * ring * 256) / 1024;
        s.disc(cx, cy, dot, c, a);
    }
    bounds
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn layout_fits_common_resolutions() {
        let a = Assets::embedded().unwrap();
        for &(w, h) in &[(640, 480), (800, 600), (1024, 768), (1280, 800), (1920, 1080), (3840, 2160)] {
            let l = Layout::new(w, h, &a);
            let screen = Rect::new(0, 0, w, h);
            for r in [l.logo, l.bar, l.spinner_bounds()] {
                assert_eq!(r.intersect(&screen), r, "{w}x{h}: {r:?} off screen");
            }
            assert!(l.logo.bottom() <= l.bar.y as i64, "{w}x{h}: bar overlaps logo");
            assert!(l.detail_y as i64 + 3 * 21 <= l.spinner_bounds().y as i64, "{w}x{h}: details overlap spinner");
            assert!(l.spinner_bounds().bottom() <= l.footer_y as i64, "{w}x{h}: spinner overlaps footer");
        }
    }

    #[test]
    fn layers_only_touch_their_dirty_rects() {
        let a = Assets::embedded().unwrap();
        let (w, h) = (800u32, 600u32);
        let l = Layout::new(w, h, &a);
        let mut bgbuf = vec![0u32; (w * h) as usize];
        let mut bg = Surface::new(&mut bgbuf, w, h, w).unwrap();
        draw_background(&mut bg, &l, &a, "left", "right");
        let mut fbuf = bg.pixels().to_vec();
        let mut f = Surface::new(&mut fbuf, w, h, w).unwrap();
        let dirty = draw_logo(&mut f, &bg, &l, &a, 255)
            .union(&draw_progress(&mut f, &bg, &l, 500, ACCENT))
            .union(&draw_status(&mut f, &bg, &l, &a, "Loading kernel", TEXT))
            .union(&draw_spinner(&mut f, &bg, &l, 37, 255, TEXT));
        let mut changed = 0;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                if f.get(x, y) != bg.get(x, y) {
                    changed += 1;
                    assert!(
                        x >= dirty.x && (x as i64) < dirty.right() && y >= dirty.y && (y as i64) < dirty.bottom(),
                        "pixel ({x},{y}) changed outside dirty rect {dirty:?}"
                    );
                }
            }
        }
        assert!(changed > 1000);
    }
}
