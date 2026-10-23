//! Fonts and the painter widgets draw with.

use crate::theme::Theme;
use osyx_gfx::{blend, isqrt, Color, Font, PropFont, Rect, Surface};

static SANS14: &[u8] = include_bytes!("../../assets/sans14.osxp");
static SANS14B: &[u8] = include_bytes!("../../assets/sans14b.osxp");
static SANS20: &[u8] = include_bytes!("../../assets/sans20.osxp");
static SANS32L: &[u8] = include_bytes!("../../assets/sans32l.osxp");
static MONO15: &[u8] = include_bytes!("../../assets/mono15.osxf");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    /// Body text (Cantarell 14 px).
    Ui,
    /// Emphasis, headings in lists and dialogs.
    Bold,
    /// Section headings.
    Large,
    /// Large light display text (clock, titles).
    Display,
    /// Source Code Pro 15 px, one cell per character (terminal).
    Mono,
}

/// The embedded type faces (Cantarell and Source Code Pro, with Polish letters).
#[derive(Clone, Copy)]
pub struct Fonts {
    pub ui: PropFont<'static>,
    pub bold: PropFont<'static>,
    pub large: PropFont<'static>,
    pub display: PropFont<'static>,
    pub mono: Font<'static>,
}

impl Fonts {
    pub fn load() -> Fonts {
        Fonts {
            ui: PropFont::parse(SANS14).expect("embedded sans14"),
            bold: PropFont::parse(SANS14B).expect("embedded sans14b"),
            large: PropFont::parse(SANS20).expect("embedded sans20"),
            display: PropFont::parse(SANS32L).expect("embedded sans32l"),
            mono: Font::parse(MONO15).expect("embedded mono15"),
        }
    }

    fn prop(&self, f: FontKind) -> Option<&PropFont<'static>> {
        match f {
            FontKind::Ui => Some(&self.ui),
            FontKind::Bold => Some(&self.bold),
            FontKind::Large => Some(&self.large),
            FontKind::Display => Some(&self.display),
            FontKind::Mono => None,
        }
    }

    pub fn line_height(&self, f: FontKind) -> u32 {
        self.prop(f).map_or(self.mono.line_height(), |p| p.line_height())
    }

    pub fn measure(&self, f: FontKind, text: &str) -> u32 {
        self.prop(f).map_or_else(|| self.mono.measure(text), |p| p.measure(text))
    }

    /// Byte index of the character boundary nearest to pixel `x` of `text`.
    pub fn hit(&self, f: FontKind, text: &str, x: i32) -> usize {
        match self.prop(f) {
            Some(p) => p.hit(text, x),
            None => {
                let cell = self.mono.cell_width() as i32;
                let n = ((x + cell / 2) / cell).max(0) as usize;
                text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Draws into a surface, clipped to a rectangle.
pub struct Painter<'a, 'b> {
    pub s: &'a mut Surface<'b>,
    pub theme: &'a Theme,
    pub fonts: &'a Fonts,
    clip: Rect,
}

impl<'a, 'b> Painter<'a, 'b> {
    pub fn new(s: &'a mut Surface<'b>, theme: &'a Theme, fonts: &'a Fonts) -> Painter<'a, 'b> {
        let clip = s.bounds();
        Painter { s, theme, fonts, clip }
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    /// Narrows the clip to `r` (within the surface); returns the old one
    /// for [`Painter::restore_clip`].
    pub fn push_clip(&mut self, r: Rect) -> Rect {
        let old = self.clip;
        self.clip = self.clip.intersect(&r);
        old
    }

    pub fn restore_clip(&mut self, old: Rect) {
        self.clip = old.intersect(&self.s.bounds());
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let r = r.intersect(&self.clip);
        self.s.fill_rect(r, c);
    }

    pub fn blend(&mut self, r: Rect, c: Color, alpha: u32) {
        let r = r.intersect(&self.clip);
        self.s.blend_rect(r, c, alpha);
    }

    pub fn vertical_gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let span = r.h.max(2) - 1;
        let area = r.intersect(&self.clip);
        for y in area.y..area.y + area.h as i32 {
            let t = ((y - r.y) as u32 * 256) / span;
            self.s.fill_rect(Rect::new(area.x, y, area.w, 1), osyx_gfx::lerp(top, bottom, t));
        }
    }

    /// Anti-aliased rounded rectangle, `alpha` 0..=255.
    pub fn rounded(&mut self, r: Rect, radius: u32, c: Color, alpha: u32) {
        let area = r.intersect(&self.clip);
        if area.is_empty() || alpha == 0 {
            return;
        }
        let rad = radius.min(r.w / 2).min(r.h / 2) as i32;
        let rad_fx = (rad as i64) << 8;
        for y in area.y..area.y + area.h as i32 {
            let ly = y - r.y;
            let cy = if ly < rad {
                rad - ly
            } else if ly >= r.h as i32 - rad {
                ly - (r.h as i32 - rad) + 1
            } else {
                0
            };
            let row = self.s.row_mut(y as u32);
            for x in area.x..area.x + area.w as i32 {
                let lx = x - r.x;
                let cx = if lx < rad {
                    rad - lx
                } else if lx >= r.w as i32 - rad {
                    lx - (r.w as i32 - rad) + 1
                } else {
                    0
                };
                let cov = if cx > 0 && cy > 0 {
                    let dx = ((cx as i64) << 8) - 128;
                    let dy = ((cy as i64) << 8) - 128;
                    let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
                    (rad_fx + 128 - dist).clamp(0, 256) as u32
                } else {
                    256
                };
                let a = (alpha * cov) >> 8;
                if a > 0 {
                    let p = &mut row[x as usize];
                    *p = blend(*p, c, a);
                }
            }
        }
    }

    /// A rounded card: `border` one pixel wide around `fill`.
    pub fn card(&mut self, r: Rect, radius: u32, fill: Color, border: Color) {
        self.rounded(r, radius, border, 255);
        self.rounded(crate::layout::inset(r, 1), radius.saturating_sub(1), fill, 255);
    }

    pub fn hline(&mut self, x: i32, y: i32, w: u32, c: Color) {
        self.fill(Rect::new(x, y, w, 1), c);
    }

    pub fn vline(&mut self, x: i32, y: i32, h: u32, c: Color) {
        self.fill(Rect::new(x, y, 1, h), c);
    }

    /// Anti-aliased disc centred on the pixel corner (`cx`, `cy`).
    pub fn disc(&mut self, cx: i32, cy: i32, radius: u32, c: Color, alpha: u32) {
        let r = radius as i32;
        let area = Rect::new(cx - r, cy - r, radius * 2, radius * 2).intersect(&self.clip);
        let r_fx = (r as i64) << 8;
        for y in area.y..area.y + area.h as i32 {
            let dy = (((y - cy) as i64) << 8) + 128;
            let row = self.s.row_mut(y as u32);
            for x in area.x..area.x + area.w as i32 {
                let dx = (((x - cx) as i64) << 8) + 128;
                let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
                let cov = (r_fx + 128 - dist).clamp(0, 256) as u32;
                if cov > 0 {
                    let p = &mut row[x as usize];
                    *p = blend(*p, c, (alpha * cov) >> 8);
                }
            }
        }
    }

    /// Anti-aliased thick segment between points given in 1/256 pixel units,
    /// `width` in 1/256 pixels (round caps).
    pub fn segment(&mut self, a: (i32, i32), b: (i32, i32), width: i32, c: Color, alpha: u32) {
        let half = (width / 2).max(64) as i64;
        let x0 = (a.0.min(b.0) as i64 - half) >> 8;
        let y0 = (a.1.min(b.1) as i64 - half) >> 8;
        let x1 = ((a.0.max(b.0) as i64 + half) >> 8) + 1;
        let y1 = ((a.1.max(b.1) as i64 + half) >> 8) + 1;
        let area = Rect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&self.clip);
        let (ax, ay) = (a.0 as i64, a.1 as i64);
        let (dx, dy) = ((b.0 - a.0) as i64, (b.1 - a.1) as i64);
        let len2 = (dx * dx + dy * dy).max(1);
        for y in area.y..area.y + area.h as i32 {
            let py = ((y as i64) << 8) + 128;
            let row = self.s.row_mut(y as u32);
            for x in area.x..area.x + area.w as i32 {
                let px = ((x as i64) << 8) + 128;
                // Closest point of the segment, then the distance to it.
                let t = (((px - ax) * dx + (py - ay) * dy) << 8) / len2;
                let t = t.clamp(0, 256);
                let qx = ax + ((dx * t) >> 8);
                let qy = ay + ((dy * t) >> 8);
                let dist = isqrt(((px - qx) * (px - qx) + (py - qy) * (py - qy)) as u64) as i64;
                let cov = (half + 128 - dist).clamp(0, 256) as u32;
                if cov > 0 {
                    let p = &mut row[x as usize];
                    *p = blend(*p, c, (alpha * cov) >> 8);
                }
            }
        }
    }

    /// Anti-aliased ring of outer radius `r` and thickness `t` (pixels)
    /// around (`cx`, `cy`); with `gap_top` the top sixth is left open (the
    /// power symbol).
    #[allow(clippy::too_many_arguments)]
    pub fn ring(&mut self, cx: i32, cy: i32, r: u32, t: u32, c: Color, alpha: u32, gap_top: bool) {
        let ri = r as i32;
        let area = Rect::new(cx - ri, cy - ri, r * 2, r * 2).intersect(&self.clip);
        let outer = (r as i64) << 8;
        let inner = (r.saturating_sub(t) as i64) << 8;
        for y in area.y..area.y + area.h as i32 {
            let dy = (((y - cy) as i64) << 8) + 128;
            let row = self.s.row_mut(y as u32);
            for x in area.x..area.x + area.w as i32 {
                let dx = (((x - cx) as i64) << 8) + 128;
                if gap_top && dy < 0 && dx.abs() * 2 < -dy {
                    continue;
                }
                let dist = isqrt((dx * dx + dy * dy) as u64) as i64;
                let cov_out = (outer + 128 - dist).clamp(0, 256);
                let cov_in = (inner + 128 - dist).clamp(0, 256);
                let cov = (cov_out - cov_in).max(0) as u32;
                if cov > 0 {
                    let p = &mut row[x as usize];
                    *p = blend(*p, c, (alpha * cov) >> 8);
                }
            }
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Color) {
        let clip = self.clip;
        // Bresenham on the clipped surface, one pixel at a time.
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if crate::layout::contains(&clip, x, y) {
                self.s.put(x, y, c);
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Text with the top of its line at (`x`, `y`). Returns the advance box.
    pub fn text(&mut self, font: FontKind, x: i32, y: i32, text: &str, c: Color) -> Rect {
        self.text_alpha(font, x, y, text, c, 255)
    }

    pub fn text_alpha(&mut self, font: FontKind, x: i32, y: i32, text: &str, c: Color, alpha: u32) -> Rect {
        let clip = self.clip;
        match self.fonts.prop(font) {
            Some(p) => p.draw_clipped(self.s, x, y, text, c, alpha, clip),
            None => {
                let m = self.fonts.mono;
                let (cw, ch) = (m.cell_width(), m.line_height());
                if y + ch as i32 <= clip.y || y as i64 >= clip.bottom() {
                    return Rect::new(x, y, m.measure(text), ch);
                }
                // Cells are drawn whole: only those inside the clip.
                let mut pen = x;
                let mut buf = [0u8; 4];
                for chr in text.chars() {
                    let cell = Rect::new(pen, y, cw, ch);
                    if chr != ' ' && cell.intersect(&clip) == cell {
                        m.draw(self.s, pen, y, chr.encode_utf8(&mut buf), c, alpha);
                    }
                    pen += cw as i32;
                }
                Rect::new(x, y, (pen - x) as u32, ch)
            }
        }
    }

    /// One line of text in `r`, vertically centred, aligned and cut with an
    /// ellipsis if too long.
    pub fn text_in(&mut self, font: FontKind, r: Rect, text: &str, c: Color, align: Align) {
        let lh = self.fonts.line_height(font);
        let y = r.y + (r.h as i32 - lh as i32) / 2;
        let old = self.push_clip(r);
        match self.fonts.prop(font) {
            Some(p) => {
                let p = *p;
                let (shown, cut) = p.ellipsize(text, r.w);
                let w = p.measure(shown) + if cut { p.advance('…') } else { 0 };
                let x = match align {
                    Align::Left => r.x,
                    Align::Center => r.x + (r.w as i32 - w as i32) / 2,
                    Align::Right => r.right() as i32 - w as i32,
                };
                let end = self.text(font, x, y, shown, c);
                if cut {
                    self.text(font, end.x + end.w as i32, y, "…", c);
                }
            }
            None => {
                let w = self.fonts.mono.measure(text) as i32;
                let x = match align {
                    Align::Left => r.x,
                    Align::Center => r.x + (r.w as i32 - w) / 2,
                    Align::Right => r.right() as i32 - w,
                };
                self.text(font, x, y, text, c);
            }
        }
        self.restore_clip(old);
    }

    /// The focus indicator around a control.
    pub fn focus_ring(&mut self, r: Rect, radius: u32) {
        // A 2 px ring: the outer rounded rect drawn only in four edge bands.
        let outer = r.inflate(2);
        let accent = self.theme.accent;
        let old = self.clip;
        for band in [
            Rect::new(outer.x, outer.y, outer.w, 2),
            Rect::new(outer.x, outer.bottom() as i32 - 2, outer.w, 2),
            Rect::new(outer.x, outer.y + 2, 2, outer.h.saturating_sub(4)),
            Rect::new(outer.right() as i32 - 2, outer.y + 2, 2, outer.h.saturating_sub(4)),
        ] {
            self.clip = old.intersect(&band);
            self.rounded(outer, radius + 2, accent, 200);
        }
        self.clip = old;
    }
}
