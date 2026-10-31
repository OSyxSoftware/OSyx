//! Line icons drawn from primitives, so they scale to any size and follow
//! the theme. Each icon fills a square box.

use crate::paint::Painter;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use osyx_gfx::{rgb, Color, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Folder,
    File,
    TextFile,
    Program,
    Terminal,
    Info,
    Power,
    Restart,
    Apps,
    Up,
    NewFolder,
    Rename,
    Trash,
    Refresh,
    Home,
    Drive,
    Close,
    Bell,
    Link,
    Device,
    Browser,
    Network,
    Wifi,
    Bluetooth,
    Volume,
    Settings,
    Lock,
    /// An application's own picture, see [`register_bitmap`].
    Bitmap(u16),
}

/// Unit directions (x, y in 1/100, y down) every 15 degrees from straight
/// right (index 0) through straight up (6) to straight left (12).
const DIRS: [(i32, i32); 13] = [
    (100, 0), (97, -26), (87, -50), (71, -71), (50, -87), (26, -97), (0, -100),
    (-26, -97), (-50, -87), (-71, -71), (-87, -50), (-97, -26), (-100, 0),
];

const FOLDER_BACK: Color = rgb(58, 104, 196);
const FOLDER_FRONT: Color = rgb(86, 142, 240);
const PAPER: Color = rgb(196, 205, 222);
const PAPER_FOLD: Color = rgb(140, 152, 176);

/// Draws `icon` into the square `r` (its smaller side) in colour `c` (line
/// icons) or its own colours (file type icons).
pub fn draw(p: &mut Painter, icon: Icon, r: Rect, c: Color) {
    if let Icon::Bitmap(id) = icon {
        return draw_bitmap(p, id, r);
    }
    let s = r.w.min(r.h) as i32;
    let x0 = r.x + (r.w as i32 - s) / 2;
    let y0 = r.y + (r.h as i32 - s) / 2;
    // Fixed-point (1/256 px) point at fraction (fx, fy) of the box, in 1/100.
    let pt = |fx: i32, fy: i32| ((x0 << 8) + s * fx * 256 / 100, (y0 << 8) + s * fy * 256 / 100);
    let px = |f: i32| s * f / 100;
    let stroke = (s * 256 / 11).max(300);
    let sub = |fx: i32, fy: i32, fw: i32, fh: i32| Rect::new(x0 + px(fx), y0 + px(fy), px(fw).max(1) as u32, px(fh).max(1) as u32);
    match icon {
        Icon::Bitmap(_) => {} // drawn above
        Icon::Browser => {
            let ink = rgb(244, 220, 186);
            p.disc(x0 + s / 2, y0 + s / 2, (s * 47 / 100) as u32, rgb(47, 43, 40), 255);
            p.ring(x0 + s / 2, y0 + s / 2, (s * 36 / 100) as u32, (s / 16).max(1) as u32, ink, 255, false);
            p.segment(pt(31, 32), pt(69, 32), stroke, ink, 255);
            p.segment(pt(69, 32), pt(31, 68), stroke, ink, 255);
            p.segment(pt(31, 68), pt(69, 68), stroke, ink, 255);
        }
        Icon::Folder | Icon::NewFolder => {
            p.rounded(sub(6, 16, 40, 20), (s / 12) as u32, FOLDER_BACK, 255);
            p.rounded(sub(6, 24, 88, 62), (s / 10) as u32, FOLDER_BACK, 255);
            p.rounded(sub(6, 32, 88, 54), (s / 10) as u32, FOLDER_FRONT, 255);
            if icon == Icon::NewFolder {
                p.segment(pt(50, 46), pt(50, 74), stroke, rgb(255, 255, 255), 255);
                p.segment(pt(36, 60), pt(64, 60), stroke, rgb(255, 255, 255), 255);
            }
        }
        Icon::File | Icon::TextFile | Icon::Program | Icon::Link | Icon::Device => {
            p.rounded(sub(18, 6, 64, 88), (s / 14) as u32, PAPER, 255);
            p.rounded(sub(56, 6, 26, 26), (s / 20) as u32, PAPER_FOLD, 255);
            match icon {
                Icon::TextFile => {
                    for (i, w) in [44, 52, 36, 48].iter().enumerate() {
                        p.fill(sub(28, 42 + i as i32 * 12, *w, 5), PAPER_FOLD);
                    }
                }
                Icon::Program => {
                    let a = rgb(46, 92, 196);
                    p.segment(pt(32, 48), pt(46, 60), stroke, a, 255);
                    p.segment(pt(46, 60), pt(32, 72), stroke, a, 255);
                    p.segment(pt(52, 74), pt(68, 74), stroke, a, 255);
                }
                Icon::Link => {
                    let a = rgb(46, 92, 196);
                    p.segment(pt(34, 72), pt(64, 44), stroke, a, 255);
                    p.segment(pt(48, 44), pt(64, 44), stroke, a, 255);
                    p.segment(pt(64, 44), pt(64, 60), stroke, a, 255);
                }
                Icon::Device => {
                    p.ring(x0 + px(50), y0 + px(60), px(18) as u32, (s / 12).max(1) as u32, rgb(46, 92, 196), 255, false);
                }
                _ => {}
            }
        }
        Icon::Terminal => {
            p.card(sub(6, 14, 88, 72), (s / 8) as u32, rgb(10, 13, 19), c);
            p.segment(pt(22, 38), pt(38, 52), stroke, c, 255);
            p.segment(pt(38, 52), pt(22, 66), stroke, c, 255);
            p.segment(pt(46, 68), pt(70, 68), stroke, c, 255);
        }
        Icon::Info => {
            p.ring(x0 + s / 2, y0 + s / 2, (s * 45 / 100) as u32, (s / 11).max(1) as u32, c, 255, false);
            p.segment(pt(50, 46), pt(50, 72), stroke, c, 255);
            p.disc(x0 + s / 2, y0 + px(30), (s / 14).max(1) as u32, c, 255);
        }
        Icon::Power => {
            p.ring(x0 + s / 2, y0 + px(54), (s * 38 / 100) as u32, (s / 11).max(1) as u32, c, 255, true);
            p.segment(pt(50, 8), pt(50, 46), stroke, c, 255);
        }
        Icon::Restart | Icon::Refresh => {
            p.ring(x0 + s / 2, y0 + s / 2, (s * 38 / 100) as u32, (s / 11).max(1) as u32, c, 255, true);
            // Arrow head at the end of the arc, pointing into the gap.
            p.segment(pt(66, 10), pt(68, 30), stroke, c, 255);
            p.segment(pt(68, 30), pt(48, 30), stroke, c, 255);
        }
        Icon::Apps => {
            let cell = s * 22 / 100;
            let gap = (s - 3 * cell) / 4;
            for i in 0..3 {
                for j in 0..3 {
                    let r = Rect::new(x0 + gap + i * (cell + gap), y0 + gap + j * (cell + gap), cell as u32, cell as u32);
                    p.rounded(r, (cell / 3) as u32, c, 255);
                }
            }
        }
        Icon::Up => {
            p.segment(pt(50, 18), pt(50, 82), stroke, c, 255);
            p.segment(pt(50, 18), pt(26, 42), stroke, c, 255);
            p.segment(pt(50, 18), pt(74, 42), stroke, c, 255);
        }
        Icon::Rename => {
            p.segment(pt(24, 76), pt(72, 28), stroke * 3 / 2, c, 255);
            p.segment(pt(16, 86), pt(26, 76), stroke, c, 180);
            p.segment(pt(44, 86), pt(86, 86), stroke, c, 255);
        }
        Icon::Trash => {
            p.segment(pt(16, 26), pt(84, 26), stroke, c, 255);
            p.segment(pt(40, 14), pt(60, 14), stroke, c, 255);
            p.segment(pt(26, 34), pt(32, 86), stroke, c, 255);
            p.segment(pt(74, 34), pt(68, 86), stroke, c, 255);
            p.segment(pt(32, 86), pt(68, 86), stroke, c, 255);
            p.segment(pt(44, 40), pt(44, 74), stroke * 3 / 4, c, 255);
            p.segment(pt(56, 40), pt(56, 74), stroke * 3 / 4, c, 255);
        }
        Icon::Home => {
            p.segment(pt(12, 48), pt(50, 14), stroke, c, 255);
            p.segment(pt(50, 14), pt(88, 48), stroke, c, 255);
            p.segment(pt(24, 42), pt(24, 86), stroke, c, 255);
            p.segment(pt(76, 42), pt(76, 86), stroke, c, 255);
            p.segment(pt(24, 86), pt(76, 86), stroke, c, 255);
            p.segment(pt(44, 86), pt(44, 62), stroke, c, 255);
            p.segment(pt(44, 62), pt(56, 62), stroke, c, 255);
            p.segment(pt(56, 62), pt(56, 86), stroke, c, 255);
        }
        Icon::Drive => {
            p.card(sub(8, 28, 84, 44), (s / 10) as u32, rgb(36, 44, 60), c);
            p.disc(x0 + px(74), y0 + px(50), (s / 14).max(1) as u32, rgb(96, 214, 150), 255);
            p.fill(sub(20, 48, 34, 4), c);
        }
        Icon::Close => {
            p.segment(pt(26, 26), pt(74, 74), stroke, c, 255);
            p.segment(pt(74, 26), pt(26, 74), stroke, c, 255);
        }
        Icon::Bell => {
            p.rounded(sub(26, 20, 48, 54), (s / 4) as u32, c, 255);
            p.fill(sub(16, 66, 68, 10), c);
            p.disc(x0 + s / 2, y0 + px(84), (s / 10).max(1) as u32, c, 255);
        }
        Icon::Network => {
            // Three linked boxes: one above, two below.
            let boxes = [(38, 10, 24, 22), (8, 66, 24, 22), (68, 66, 24, 22)];
            for (bx, by, bw, bh) in boxes {
                p.segment(pt(bx, by), pt(bx + bw, by), stroke, c, 255);
                p.segment(pt(bx + bw, by), pt(bx + bw, by + bh), stroke, c, 255);
                p.segment(pt(bx + bw, by + bh), pt(bx, by + bh), stroke, c, 255);
                p.segment(pt(bx, by + bh), pt(bx, by), stroke, c, 255);
            }
            p.segment(pt(50, 32), pt(50, 50), stroke, c, 255);
            p.segment(pt(20, 50), pt(80, 50), stroke, c, 255);
            p.segment(pt(20, 50), pt(20, 66), stroke, c, 255);
            p.segment(pt(80, 50), pt(80, 66), stroke, c, 255);
        }
        Icon::Wifi => {
            for r in [24, 46, 68] {
                arc(p, pt(50, 84), s * r / 100, 3, 9, stroke, c);
            }
            p.disc(x0 + s / 2, y0 + px(82), (s / 12).max(1) as u32, c, 255);
        }
        Icon::Bluetooth => {
            p.segment(pt(48, 8), pt(48, 92), stroke, c, 255);
            p.segment(pt(48, 8), pt(72, 30), stroke, c, 255);
            p.segment(pt(72, 30), pt(26, 70), stroke, c, 255);
            p.segment(pt(26, 30), pt(72, 70), stroke, c, 255);
            p.segment(pt(72, 70), pt(48, 92), stroke, c, 255);
        }
        Icon::Volume => {
            p.segment(pt(10, 38), pt(26, 38), stroke, c, 255);
            p.segment(pt(26, 38), pt(48, 18), stroke, c, 255);
            p.segment(pt(48, 18), pt(48, 82), stroke, c, 255);
            p.segment(pt(48, 82), pt(26, 62), stroke, c, 255);
            p.segment(pt(26, 62), pt(10, 62), stroke, c, 255);
            p.segment(pt(10, 62), pt(10, 38), stroke, c, 255);
            for r in [18, 34] {
                arc_right(p, pt(54, 50), s * r / 100, stroke, c);
            }
        }
        Icon::Lock => {
            // Padlock: shackle (an open-bottom ring) over a rounded body.
            p.ring(x0 + s / 2, y0 + px(40), (s * 24 / 100) as u32, (s / 10).max(1) as u32, c, 255, false);
            p.rounded(sub(18, 42, 64, 48), (s / 10) as u32, c, 255);
        }
        Icon::Settings => {
            p.ring(x0 + s / 2, y0 + s / 2, (s * 30 / 100) as u32, (s / 10).max(1) as u32, c, 255, false);
            let all = [(100, 0), (71, 71), (0, 100), (-71, 71), (-100, 0), (-71, -71), (0, -100), (71, -71)];
            for (dx, dy) in all {
                let a = pt(50 + 30 * dx / 100, 50 + 30 * dy / 100);
                let b = pt(50 + 44 * dx / 100, 50 + 44 * dy / 100);
                p.segment(a, b, stroke * 3 / 2, c, 255);
            }
        }
    }
}

/// Polyline arc of radius `r` pixels around `centre` (1/256 px) through
/// `DIRS[from..=to]`.
fn arc(p: &mut Painter, centre: (i32, i32), r: i32, from: usize, to: usize, width: i32, c: Color) {
    let at = |(dx, dy): (i32, i32)| (centre.0 + r * dx * 256 / 100, centre.1 + r * dy * 256 / 100);
    for i in from..to {
        p.segment(at(DIRS[i]), at(DIRS[i + 1]), width, c, 255);
    }
}

/// The right-facing half of `arc` from 45 degrees up to 45 degrees down
/// (sound waves).
fn arc_right(p: &mut Painter, centre: (i32, i32), r: i32, width: i32, c: Color) {
    let at = |(dx, dy): (i32, i32)| (centre.0 + r * dx * 256 / 100, centre.1 + r * dy * 256 / 100);
    let pts: [(i32, i32); 7] = [(71, -71), (87, -50), (97, -26), (100, 0), (97, 26), (87, 50), (71, 71)];
    for w in pts.windows(2) {
        p.segment(at(w[0]), at(w[1]), width, c, 255);
    }
}

// ---- bitmap icons ---------------------------------------------------------------------

/// An application's own icon: 32-bit pixels, `0xAARRGGBB`, row by row.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

/// Most bitmap icons that can be registered (an [`Icon::Bitmap`] holds a
/// 16-bit number).
const MAX_BITMAPS: usize = 512;

/// The registered bitmaps. The shell registers icons from its one thread;
/// the lock only keeps the table sound if that ever changes.
struct Registry {
    locked: AtomicBool,
    items: UnsafeCell<Vec<Bitmap>>,
}

// SAFETY: `items` is only touched with `locked` held.
unsafe impl Sync for Registry {}

static REGISTRY: Registry = Registry { locked: AtomicBool::new(false), items: UnsafeCell::new(Vec::new()) };

fn with_registry<R>(f: impl FnOnce(&mut Vec<Bitmap>) -> R) -> R {
    while REGISTRY.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    // SAFETY: the lock is held, so nothing else has the table.
    let result = f(unsafe { &mut *REGISTRY.items.get() });
    REGISTRY.locked.store(false, Ordering::Release);
    result
}

/// Registers `bitmap` and returns the icon that draws it, or `None` when the
/// bitmap is empty, inconsistent or the table is full.
pub fn register_bitmap(bitmap: Bitmap) -> Option<Icon> {
    if bitmap.width == 0 || bitmap.height == 0 || bitmap.pixels.len() != bitmap.width as usize * bitmap.height as usize {
        return None;
    }
    with_registry(|items| {
        if items.len() >= MAX_BITMAPS {
            return None;
        }
        items.push(bitmap);
        Some(Icon::Bitmap((items.len() - 1) as u16))
    })
}

/// Draws a registered bitmap into the square `r`, scaled (box filter down,
/// nearest-neighbour blend up) and alpha blended.
fn draw_bitmap(p: &mut Painter, id: u16, r: Rect) {
    let s = r.w.min(r.h) as i32;
    if s <= 0 {
        return;
    }
    let x0 = r.x + (r.w as i32 - s) / 2;
    let y0 = r.y + (r.h as i32 - s) / 2;
    let clip = p.clip();
    with_registry(|items| {
        let Some(b) = items.get(id as usize) else { return };
        let (bw, bh) = (b.width as i64, b.height as i64);
        for dy in 0..s {
            for dx in 0..s {
                let (x, y) = (x0 + dx, y0 + dy);
                if x < clip.x || y < clip.y || x >= clip.right() as i32 || y >= clip.bottom() as i32 {
                    continue;
                }
                // The source box of this destination pixel, at least one pixel.
                let sx0 = dx as i64 * bw / s as i64;
                let sx1 = (((dx as i64 + 1) * bw + s as i64 - 1) / s as i64).max(sx0 + 1).min(bw);
                let sy0 = dy as i64 * bh / s as i64;
                let sy1 = (((dy as i64 + 1) * bh + s as i64 - 1) / s as i64).max(sy0 + 1).min(bh);
                let (mut a, mut rr, mut gg, mut bb, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let px = b.pixels[(sy * bw + sx) as usize];
                        let alpha = px >> 24;
                        a += alpha;
                        // Premultiplied, so transparent pixels add no colour.
                        rr += ((px >> 16) & 0xFF) * alpha;
                        gg += ((px >> 8) & 0xFF) * alpha;
                        bb += (px & 0xFF) * alpha;
                        n += 1;
                    }
                }
                if a == 0 || n == 0 {
                    continue;
                }
                let colour = rgb((rr / a) as u8, (gg / a) as u8, (bb / a) as u8);
                p.s.blend_px(x, y, colour, a / n);
            }
        }
    });
}

/// The pixels of an icon image in a `.ico` / PE resource: a device
/// independent bitmap (BITMAPINFOHEADER, colour data, then a 1-bit mask), as
/// found in `RT_ICON` resources. PNG-compressed images and unusual formats
/// give `None`.
pub fn decode_dib(data: &[u8]) -> Option<Bitmap> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let header = u32_at(0)? as usize;
    if header < 40 || u32_at(16)? != 0 {
        return None; // not BI_RGB (PNG data also fails here)
    }
    let width = u32_at(4)?;
    let height2 = u32_at(8)? as i32;
    let (planes, bpp) = (u16_at(12)?, u16_at(14)?);
    if planes != 1 || width == 0 || width > 256 || height2 <= 0 || height2 % 2 != 0 {
        return None;
    }
    let height = (height2 / 2) as u32;
    if height == 0 || height > 256 {
        return None;
    }
    let colours = match bpp {
        1 | 4 | 8 => {
            let used = u32_at(32)? as usize;
            if used == 0 { 1usize << bpp } else { used.min(1 << bpp) }
        }
        24 | 32 => 0,
        _ => return None,
    };
    let palette_at = header;
    let xor_at = palette_at + colours * 4;
    let xor_stride = ((width as usize * bpp as usize + 31) / 32) * 4;
    let and_at = xor_at + xor_stride * height as usize;
    let and_stride = ((width as usize + 31) / 32) * 4;
    if data.len() < xor_at + xor_stride * height as usize {
        return None;
    }
    let mask = |x: u32, y: u32| -> bool {
        // 1 = transparent; a missing mask means opaque.
        data.get(and_at + y as usize * and_stride + x as usize / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0)
    };
    let mut pixels = Vec::with_capacity((width * height) as usize);
    let mut any_alpha = false;
    for row in 0..height {
        let y = height - 1 - row; // bottom-up
        let line = xor_at + y as usize * xor_stride;
        for x in 0..width {
            let argb = match bpp {
                32 => {
                    let o = line + x as usize * 4;
                    let (b, g, r, a) = (*data.get(o)? as u32, *data.get(o + 1)? as u32, *data.get(o + 2)? as u32, *data.get(o + 3)? as u32);
                    any_alpha |= a != 0;
                    a << 24 | r << 16 | g << 8 | b
                }
                24 => {
                    let o = line + x as usize * 3;
                    let (b, g, r) = (*data.get(o)? as u32, *data.get(o + 1)? as u32, *data.get(o + 2)? as u32);
                    0xFF00_0000 | r << 16 | g << 8 | b
                }
                _ => {
                    let index = match bpp {
                        8 => *data.get(line + x as usize)? as usize,
                        4 => (*data.get(line + x as usize / 2)? as usize >> if x % 2 == 0 { 4 } else { 0 }) & 0xF,
                        _ => (*data.get(line + x as usize / 8)? as usize >> (7 - x % 8)) & 1,
                    };
                    let o = palette_at + index.min(colours.saturating_sub(1)) * 4;
                    let (b, g, r) = (*data.get(o)? as u32, *data.get(o + 1)? as u32, *data.get(o + 2)? as u32);
                    0xFF00_0000 | r << 16 | g << 8 | b
                }
            };
            pixels.push(argb);
        }
    }
    // Without per-pixel alpha (or with an all-zero alpha channel) the mask
    // says which pixels are transparent.
    if bpp != 32 || !any_alpha {
        for row in 0..height {
            for x in 0..width {
                let i = (row * width + x) as usize;
                let y = height - 1 - row;
                pixels[i] = if mask(x, y) { 0 } else { pixels[i] | 0xFF00_0000 };
            }
        }
    }
    Some(Bitmap { width, height, pixels })
}
