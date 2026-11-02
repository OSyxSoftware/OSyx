//! The image viewer built into Files: PNG, JPEG, GIF (animated), BMP and
//! ICO decoded by `osyx_media::image` on a thread of their own, shown to
//! fit the window or zoomed, with the other pictures of the folder a key
//! away.
//!
//! Keys: Left/Right (or Page Up/Down) for the previous/next picture, + and
//! - zoom, 0 fits the window, 1 shows actual size; dragging pans.

use os_std::prelude::*;
use os_std::sync::atomic::{AtomicU64, Ordering};
use os_std::thread;
use os_std::time::Instant;
use osyx_gfx::{rgb, Rect};
use osyx_input::{button, key};
use osyx_media::image::{self, Picture};
use osyx_ui::layout::{contains, split_top};
use osyx_ui::widget::{Button, ButtonStyle};
use osyx_ui::{Align, Event as Ui, FontKind, Painter, Response};

/// Extensions the viewer decodes (lower case).
pub const EXTENSIONS: [&str; 14] =
    ["png", "jpg", "jpeg", "gif", "bmp", "ico", "jbig2", "jb2", "jp2", "j2k", "j2c", "jpc", "jpf", "jpx"];

struct Loaded {
    /// The picture, or why it cannot be shown.
    picture: Mutex<Option<Result<Arc<Picture>, String>>>,
    generation: AtomicU64,
}

pub struct ImageView {
    pub path: String,
    /// The folder's pictures, sorted, and the one shown.
    siblings: Vec<String>,
    index: usize,
    loaded: Arc<Loaded>,
    seen: u64,
    /// None: fit the window.
    zoom: Option<f64>,
    /// Offset of the picture's centre from the view's centre, pixels.
    pan: (f64, f64),
    drag: Option<(i32, i32, f64, f64)>,
    frame: usize,
    frame_since: Instant,
    view: Rect,
    close: Button,
    prev: Button,
    next: Button,
    smaller: Button,
    larger: Button,
    fit: Button,
}

fn is_picture(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn load(path: String, loaded: Arc<Loaded>) {
    thread::spawn(move || {
        let start = Instant::now();
        let r = os_std::fs::read(&path).map_err(|e| format!("cannot read {path}: {e}")).and_then(|d| image::decode(&d));
        match &r {
            Ok(p) => println!(
                "files: opened picture {path} ({}x{}, {}) in {} ms",
                p.w,
                p.h,
                p.format,
                start.elapsed().as_millis()
            ),
            Err(e) => println!("files: cannot show {path}: {e}"),
        }
        *loaded.picture.lock().unwrap() = Some(r.map(Arc::new));
        loaded.generation.fetch_add(1, Ordering::Relaxed);
    });
}

impl ImageView {
    pub fn open(path: &str) -> ImageView {
        let dir = path.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
        let mut siblings: Vec<String> = os_std::fs::read_dir(dir)
            .map(|it| {
                it.map(|e| String::from(e.file_name()))
                    .filter(|n| is_picture(n))
                    .map(|n| if dir == "/" { format!("/{n}") } else { format!("{dir}/{n}") })
                    .collect()
            })
            .unwrap_or_default();
        siblings.sort_by_key(|s| s.to_ascii_lowercase());
        let index = siblings.iter().position(|s| s == path).unwrap_or_else(|| {
            siblings.push(String::from(path));
            siblings.len() - 1
        });
        let loaded = Arc::new(Loaded { picture: Mutex::new(None), generation: AtomicU64::new(0) });
        load(String::from(path), loaded.clone());
        ImageView {
            path: String::from(path),
            siblings,
            index,
            loaded,
            seen: 0,
            zoom: None,
            pan: (0.0, 0.0),
            drag: None,
            frame: 0,
            frame_since: Instant::now(),
            view: Rect::EMPTY,
            close: Button::new(0, "Close", ButtonStyle::Secondary),
            prev: Button::new(1, "<", ButtonStyle::Secondary),
            next: Button::new(2, ">", ButtonStyle::Secondary),
            smaller: Button::new(3, "-", ButtonStyle::Secondary),
            larger: Button::new(4, "+", ButtonStyle::Secondary),
            fit: Button::new(5, "Fit", ButtonStyle::Secondary),
        }
    }

    fn picture(&self) -> Option<Arc<Picture>> {
        self.loaded.picture.lock().unwrap().as_ref().and_then(|r| r.as_ref().ok().cloned())
    }

    fn go(&mut self, delta: isize) {
        if self.siblings.len() < 2 {
            return;
        }
        let n = self.siblings.len() as isize;
        self.index = ((self.index as isize + delta).rem_euclid(n)) as usize;
        self.path = self.siblings[self.index].clone();
        self.loaded = Arc::new(Loaded { picture: Mutex::new(None), generation: AtomicU64::new(0) });
        self.seen = 0;
        load(self.path.clone(), self.loaded.clone());
        self.zoom = None;
        self.pan = (0.0, 0.0);
        self.frame = 0;
    }

    /// The scale the picture is drawn at.
    fn scale(&self, p: &Picture) -> f64 {
        self.zoom.unwrap_or_else(|| {
            let sx = (self.view.w as f64 - 24.0) / p.w as f64;
            let sy = (self.view.h as f64 - 24.0) / p.h as f64;
            sx.min(sy).min(1.0).max(0.01)
        })
    }

    fn set_zoom(&mut self, z: f64) {
        self.zoom = Some(z.clamp(0.02, 32.0));
    }

    pub fn layout(&mut self, r: Rect) {
        let (head, body) = split_top(r, 44);
        self.view = body;
        let right = head.right() as i32;
        self.close.rect = Rect::new(right - 96, head.y + 6, 84, 32);
        let mut x = right - 96 - 10;
        for b in [&mut self.fit, &mut self.larger, &mut self.smaller] {
            let w = if b.label == "Fit" { 48 } else { 34 };
            x -= w;
            b.rect = Rect::new(x, head.y + 6, w as u32, 32);
            x -= 6;
        }
        x -= 120;
        self.prev.rect = Rect::new(x, head.y + 6, 34, 32);
        self.next.rect = Rect::new(x + 86, head.y + 6, 34, 32);
    }

    /// True to redraw; sets `close` when the view should go away.
    pub fn event(&mut self, ev: &Ui, close: &mut bool) -> bool {
        let mut redraw = false;
        for id in 0..6 {
            let b = match id {
                0 => &mut self.close,
                1 => &mut self.prev,
                2 => &mut self.next,
                3 => &mut self.smaller,
                4 => &mut self.larger,
                _ => &mut self.fit,
            };
            match b.event(ev, false) {
                Response::Activated(_) => {
                    match id {
                        0 => *close = true,
                        1 => self.go(-1),
                        2 => self.go(1),
                        3 | 4 => {
                            if let Some(p) = self.picture() {
                                let s = self.scale(&p);
                                self.set_zoom(if id == 3 { s / 1.25 } else { s * 1.25 });
                            }
                        }
                        _ => {
                            self.zoom = None;
                            self.pan = (0.0, 0.0);
                        }
                    }
                    return true;
                }
                r => redraw |= r.needs_redraw(),
            }
        }
        match *ev {
            Ui::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&self.view, x, y) => {
                self.drag = Some((x, y, self.pan.0, self.pan.1));
                return true;
            }
            Ui::PointerMove { x, y } => {
                if let Some((x0, y0, px, py)) = self.drag {
                    self.pan = (px + (x - x0) as f64, py + (y - y0) as f64);
                    return true;
                }
            }
            Ui::PointerUp { .. } if self.drag.is_some() => {
                self.drag = None;
                return true;
            }
            Ui::Wheel { x, y, dy } if contains(&self.view, x, y) => {
                if let Some(p) = self.picture() {
                    let s = self.scale(&p);
                    self.set_zoom(if dy > 0 { s * 1.15 } else { s / 1.15 });
                    return true;
                }
            }
            Ui::Key(k) if k.down => {
                let step = |s: &mut ImageView, f: f64| {
                    if let Some(p) = s.picture() {
                        let z = s.scale(&p) * f;
                        s.set_zoom(z);
                    }
                };
                match k.key {
                    key::LEFT | key::PAGE_UP => self.go(-1),
                    key::RIGHT | key::PAGE_DOWN | key::SPACE => self.go(1),
                    key::EQUAL | key::KP_PLUS => step(self, 1.25),
                    key::MINUS | key::KP_MINUS => step(self, 1.0 / 1.25),
                    key::DIGIT0 => {
                        self.zoom = None;
                        self.pan = (0.0, 0.0);
                    }
                    key::DIGIT1 => self.zoom = Some(1.0),
                    key::ESCAPE => *close = true,
                    _ => return redraw,
                }
                return true;
            }
            _ => {}
        }
        redraw
    }

    /// The picture arrived, or an animation frame is due.
    pub fn needs_redraw(&mut self) -> bool {
        let g = self.loaded.generation.load(Ordering::Relaxed);
        if g != self.seen {
            self.seen = g;
            self.frame = 0;
            self.frame_since = Instant::now();
            return true;
        }
        if let Some(p) = self.picture() {
            if p.frames.len() > 1 {
                let delay = p.frames[self.frame].delay_ms.max(20) as u128;
                if self.frame_since.elapsed().as_millis() >= delay {
                    self.frame = (self.frame + 1) % p.frames.len();
                    self.frame_since = Instant::now();
                    return true;
                }
            }
        }
        false
    }

    /// Whether frames change over time (for the tick rate).
    pub fn animated(&self) -> bool {
        self.picture().is_some_and(|p| p.frames.len() > 1)
    }

    pub fn draw(&mut self, p: &mut Painter, r: Rect) {
        let t = *p.theme;
        let (head, body) = split_top(r, 44);
        p.fill(head, t.surface);
        p.hline(head.x, head.bottom() as i32 - 1, head.w, t.border);
        osyx_ui::icon::draw(p, osyx_ui::Icon::File, Rect::new(head.x + 14, head.y + 12, 20, 20), t.text_dim);
        let name = self.path.rsplit('/').next().unwrap_or(&self.path).to_string();
        let pic = self.picture();
        let title = match &pic {
            Some(pc) => format!("{name}  ·  {} x {}  ·  {}", pc.w, pc.h, pc.format),
            None => name,
        };
        let title_w = (self.prev.rect.x - head.x - 56).max(40) as u32;
        p.text_in(FontKind::Bold, Rect::new(head.x + 44, head.y, title_w, head.h), &title, t.text, Align::Left);
        let label = format!("{} / {}", self.index + 1, self.siblings.len());
        p.text_in(
            FontKind::Ui,
            Rect::new(self.prev.rect.right() as i32, head.y, 52, head.h),
            &label,
            t.text,
            Align::Center,
        );
        for b in [&self.close, &self.prev, &self.next, &self.smaller, &self.larger, &self.fit] {
            b.draw(p, false);
        }
        p.fill(body, rgb(32, 33, 36));
        let err = self.loaded.picture.lock().unwrap().as_ref().and_then(|r| r.as_ref().err().cloned());
        let Some(pc) = pic else {
            let msg = err.unwrap_or_else(|| String::from("Loading..."));
            p.text_in(
                FontKind::Ui,
                body,
                &msg,
                if msg == "Loading..." { t.text_dim } else { t.danger_hover },
                Align::Center,
            );
            return;
        };
        let s = self.scale(&pc);
        let (dw, dh) = (pc.w as f64 * s, pc.h as f64 * s);
        let cx = body.x as f64 + body.w as f64 / 2.0 + self.pan.0;
        let cy = body.y as f64 + body.h as f64 / 2.0 + self.pan.1;
        let (x0, y0) = (cx - dw / 2.0, cy - dh / 2.0);
        let old = p.push_clip(body);
        let frame = &pc.frames[self.frame.min(pc.frames.len() - 1)];
        blit_scaled(p, &pc, &frame.px, x0, y0, s);
        p.restore_clip(old);
        let pct = format!("{:.0}%", s * 100.0);
        p.text_in(
            FontKind::Ui,
            Rect::new(body.x + 10, body.bottom() as i32 - 26, 80, 20),
            &pct,
            rgb(200, 200, 200),
            Align::Left,
        );
    }
}

fn floor(v: f64) -> i32 {
    let i = v as i32;
    if (i as f64) > v {
        i - 1
    } else {
        i
    }
}

fn ceil(v: f64) -> i32 {
    let i = v as i32;
    if (i as f64) < v {
        i + 1
    } else {
        i
    }
}

/// Draws RGBA `px` at (x0, y0) scaled by `s`: an area average when
/// shrinking, nearest pixels when enlarging, over a checkerboard where
/// the picture is transparent.
fn blit_scaled(p: &mut Painter, pc: &Picture, px: &[[u8; 4]], x0: f64, y0: f64, s: f64) {
    let clip = p.clip();
    let dx0 = floor(x0).max(clip.x);
    let dy0 = floor(y0).max(clip.y);
    let dx1 = ceil(x0 + pc.w as f64 * s).min(clip.right() as i32);
    let dy1 = ceil(y0 + pc.h as f64 * s).min(clip.bottom() as i32);
    if dx0 >= dx1 || dy0 >= dy1 {
        return;
    }
    let inv = 1.0 / s;
    for y in dy0..dy1 {
        let sy0 = ((y as f64 - y0) * inv).max(0.0);
        let sy1 = ((y as f64 + 1.0 - y0) * inv).min(pc.h as f64);
        if sy1 <= 0.0 || sy0 >= pc.h as f64 {
            continue;
        }
        let row = p.s.row_mut(y as u32);
        for x in dx0..dx1 {
            let sx0 = ((x as f64 - x0) * inv).max(0.0);
            let sx1 = ((x as f64 + 1.0 - x0) * inv).min(pc.w as f64);
            if sx1 <= 0.0 || sx0 >= pc.w as f64 {
                continue;
            }
            let (ix0, iy0) = (sx0 as usize, sy0 as usize);
            let c = if s >= 1.0 {
                px[iy0.min(pc.h - 1) * pc.w + ix0.min(pc.w - 1)]
            } else {
                // Average the covered source pixels (at most 8 x 8 samples).
                let (ix1, iy1) = ((ceil(sx1) as usize).min(pc.w), (ceil(sy1) as usize).min(pc.h));
                let stepx = ((ix1 - ix0) / 8).max(1);
                let stepy = ((iy1 - iy0) / 8).max(1);
                let (mut acc, mut n) = ([0u32; 4], 0u32);
                for yy in (iy0..iy1).step_by(stepy) {
                    for xx in (ix0..ix1).step_by(stepx) {
                        let q = px[yy * pc.w + xx];
                        let a = q[3] as u32;
                        for k in 0..3 {
                            acc[k] += q[k] as u32 * a;
                        }
                        acc[3] += a;
                        n += 1;
                    }
                }
                let a = acc[3].max(1);
                [(acc[0] / a) as u8, (acc[1] / a) as u8, (acc[2] / a) as u8, (acc[3] / n.max(1)) as u8]
            };
            let checker = if ((x / 8) + (y / 8)) % 2 == 0 { 0xCC } else { 0x99 };
            let a = c[3] as u32;
            let mix = |v: u8| ((v as u32 * a + checker * (255 - a)) / 255) as u8;
            row[x as usize] = rgb(mix(c[0]), mix(c[1]), mix(c[2]));
        }
    }
}
