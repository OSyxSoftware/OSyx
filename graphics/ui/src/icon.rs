//! Line icons drawn from primitives, so they scale to any size and follow
//! the theme. Each icon fills a square box.

use crate::paint::Painter;
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
}

const FOLDER_BACK: Color = rgb(58, 104, 196);
const FOLDER_FRONT: Color = rgb(86, 142, 240);
const PAPER: Color = rgb(196, 205, 222);
const PAPER_FOLD: Color = rgb(140, 152, 176);

/// Draws `icon` into the square `r` (its smaller side) in colour `c` (line
/// icons) or its own colours (file type icons).
pub fn draw(p: &mut Painter, icon: Icon, r: Rect, c: Color) {
    let s = r.w.min(r.h) as i32;
    let x0 = r.x + (r.w as i32 - s) / 2;
    let y0 = r.y + (r.h as i32 - s) / 2;
    // Fixed-point (1/256 px) point at fraction (fx, fy) of the box, in 1/100.
    let pt = |fx: i32, fy: i32| ((x0 << 8) + s * fx * 256 / 100, (y0 << 8) + s * fy * 256 / 100);
    let px = |f: i32| s * f / 100;
    let stroke = (s * 256 / 11).max(300);
    let sub = |fx: i32, fy: i32, fw: i32, fh: i32| Rect::new(x0 + px(fx), y0 + px(fy), px(fw).max(1) as u32, px(fh).max(1) as u32);
    match icon {
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
    }
}
