//! The PDF viewer built into Files: pages rendered by `osyx_media::pdf` on
//! a thread of their own, shown one under the other with scrolling,
//! page navigation and zoom. Fonts a document does not embed are drawn
//! with the system fonts in /Windows/Fonts.
//!
//! Keys: Up/Down and the wheel scroll, Page Up/Page Down move by a page,
//! Home/End go to the first/last page, + and - zoom, 0 fits the width.

use os_std::mpsc::{self, Sender};
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicU64, Ordering};
use os_std::thread;
use osyx_gfx::{rgb, Rect};
use osyx_input::key;
use osyx_media::pdf::{Bitmap, Pdf};
use osyx_ui::layout::{contains, split_top};
use osyx_ui::widget::{Button, ButtonStyle};
use osyx_ui::{Align, Event as Ui, FontKind, Painter, Response};

/// Space between pages and around them, in pixels.
const GAP: i64 = 14;
/// Rendered pages kept in memory.
const KEEP: usize = 8;
const FONT_DIR: &str = "/Windows/Fonts";

/// A page to render at a zoom (pixels per point times 1000).
type Key = (usize, u32);

struct Shared {
    done: Mutex<BTreeMap<Key, Arc<Bitmap>>>,
    /// Bumped whenever a page is finished.
    generation: AtomicU64,
    error: Mutex<Option<String>>,
}

pub struct PdfView {
    pub path: String,
    title: Option<String>,
    sizes: Vec<(f64, f64)>,
    shared: Arc<Shared>,
    tx: Sender<Key>,
    requested: BTreeSet<Key>,
    seen: u64,
    /// Pixels per point; `fit` recomputes it from the width.
    zoom: f64,
    fit: bool,
    scroll: i64,
    view: Rect,
    close: Button,
    prev: Button,
    next: Button,
    smaller: Button,
    larger: Button,
    fit_btn: Button,
}

fn render_thread(data: Vec<u8>, rx: mpsc::Receiver<Key>, shared: Arc<Shared>) {
    let pdf = match Pdf::open(data) {
        Ok(p) => p,
        Err(e) => {
            *shared.error.lock().unwrap() = Some(e);
            return;
        }
    };
    // System fonts are read once, when a document first needs them.
    let fonts: core::cell::RefCell<BTreeMap<String, Option<Vec<u8>>>> = core::cell::RefCell::new(BTreeMap::new());
    let lookup = |name: &str| -> Option<Vec<u8>> {
        fonts
            .borrow_mut()
            .entry(String::from(name))
            .or_insert_with(|| os_std::fs::read(format!("{FONT_DIR}/{name}")).ok())
            .clone()
    };
    while let Ok(key) = rx.recv() {
        // Requests for pages scrolled past may already be done.
        if shared.done.lock().unwrap().contains_key(&key) {
            continue;
        }
        let start = os_std::time::Instant::now();
        let bmp = match pdf.render(key.0, key.1 as f64 / 1000.0, &lookup) {
            Ok(b) => b,
            Err(e) => {
                println!("files: page {} not rendered: {e}", key.0 + 1);
                *shared.error.lock().unwrap() = Some(e);
                shared.generation.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        println!("files: rendered page {} at {}% in {} ms", key.0 + 1, key.1 / 10, start.elapsed().as_millis());
        shared.done.lock().unwrap().insert(key, Arc::new(bmp));
        shared.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl PdfView {
    pub fn open(path: &str) -> Result<PdfView, String> {
        let data = os_std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let pdf = Pdf::open(data.clone())?;
        let sizes = (0..pdf.pages()).map(|i| pdf.page_size(i)).collect();
        let title = pdf.title();
        drop(pdf);
        let shared = Arc::new(Shared {
            done: Mutex::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel();
        let s = shared.clone();
        thread::spawn(move || render_thread(data, rx, s));
        println!("files: opened PDF {path} ({} pages)", (&sizes as &Vec<(f64, f64)>).len());
        Ok(PdfView {
            path: String::from(path),
            title,
            sizes,
            shared,
            tx,
            requested: BTreeSet::new(),
            seen: 0,
            zoom: 1.0,
            fit: true,
            scroll: 0,
            view: Rect::EMPTY,
            close: Button::new(0, "Close", ButtonStyle::Secondary),
            prev: Button::new(1, "<", ButtonStyle::Secondary),
            next: Button::new(2, ">", ButtonStyle::Secondary),
            smaller: Button::new(3, "-", ButtonStyle::Secondary),
            larger: Button::new(4, "+", ButtonStyle::Secondary),
            fit_btn: Button::new(5, "Fit", ButtonStyle::Secondary),
        })
    }

    fn zoom_key(&self) -> u32 {
        (self.zoom * 1000.0) as u32
    }

    /// Top of each page and the total height, at the current zoom.
    fn layout_pages(&self) -> (Vec<i64>, i64) {
        let mut tops = Vec::with_capacity(self.sizes.len());
        let mut y = GAP;
        for &(_, h) in &self.sizes {
            tops.push(y);
            y += (h * self.zoom) as i64 + GAP;
        }
        (tops, y)
    }

    fn max_scroll(&self) -> i64 {
        (self.layout_pages().1 - self.view.h as i64).max(0)
    }

    fn current_page(&self) -> usize {
        let (tops, _) = self.layout_pages();
        let mid = self.scroll + self.view.h as i64 / 3;
        tops.iter().rposition(|&t| t <= mid).unwrap_or(0)
    }

    fn go_to(&mut self, page: usize) {
        let (tops, _) = self.layout_pages();
        if let Some(&t) = tops.get(page.min(self.sizes.len().saturating_sub(1))) {
            self.scroll = (t - GAP).clamp(0, self.max_scroll());
        }
    }

    fn set_zoom(&mut self, z: f64) {
        // Keep the page in view at the same place.
        let page = self.current_page();
        self.zoom = z.clamp(0.2, 6.0);
        self.go_to(page);
    }

    fn fit_zoom(&mut self) {
        let widest = self.sizes.iter().fold(1.0f64, |m, s| m.max(s.0));
        let z = (self.view.w as f64 - 2.0 * GAP as f64) / widest;
        if z > 0.0 {
            // Round so equal views reuse rendered pages.
            self.zoom = (z.clamp(0.2, 6.0) * 100.0) as i64 as f64 / 100.0;
        }
    }

    pub fn layout(&mut self, r: Rect) {
        let (head, body) = split_top(r, 44);
        self.view = body;
        let right = head.right() as i32;
        self.close.rect = Rect::new(right - 96, head.y + 6, 84, 32);
        let mut x = right - 96 - 10;
        for b in [&mut self.fit_btn, &mut self.larger, &mut self.smaller] {
            let w = if b.label == "Fit" { 48 } else { 34 };
            x -= w;
            b.rect = Rect::new(x, head.y + 6, w as u32, 32);
            x -= 6;
        }
        x -= 120;
        self.prev.rect = Rect::new(x, head.y + 6, 34, 32);
        self.next.rect = Rect::new(x + 86, head.y + 6, 34, 32);
        if self.fit {
            let page = self.current_page();
            self.fit_zoom();
            self.go_to(page);
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll());
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
                _ => &mut self.fit_btn,
            };
            match b.event(ev, false) {
                Response::Activated(_) => {
                    match id {
                        0 => *close = true,
                        1 => {
                            let p = self.current_page();
                            self.go_to(p.saturating_sub(1));
                        }
                        2 => {
                            let p = self.current_page();
                            self.go_to(p + 1);
                        }
                        3 => {
                            self.fit = false;
                            self.set_zoom(self.zoom / 1.25);
                        }
                        4 => {
                            self.fit = false;
                            self.set_zoom(self.zoom * 1.25);
                        }
                        _ => {
                            self.fit = true;
                            let page = self.current_page();
                            self.fit_zoom();
                            self.go_to(page);
                        }
                    }
                    return true;
                }
                r => redraw |= r.needs_redraw(),
            }
        }
        let step = 48;
        let page_step = self.view.h as i64 - 40;
        let max = self.max_scroll();
        let before = self.scroll;
        match *ev {
            Ui::Wheel { x, y, dy } if contains(&self.view, x, y) => self.scroll -= dy as i64 * step * 2,
            Ui::Key(k) if k.down => match k.key {
                key::DOWN => self.scroll += step,
                key::UP => self.scroll -= step,
                key::PAGE_DOWN | key::SPACE => self.scroll += page_step,
                key::PAGE_UP => self.scroll -= page_step,
                key::HOME => self.scroll = 0,
                key::END => self.scroll = max,
                key::ESCAPE => {
                    *close = true;
                    return true;
                }
                key::EQUAL | key::KP_PLUS => {
                    self.fit = false;
                    self.set_zoom(self.zoom * 1.25);
                    return true;
                }
                key::MINUS | key::KP_MINUS => {
                    self.fit = false;
                    self.set_zoom(self.zoom / 1.25);
                    return true;
                }
                key::DIGIT0 => {
                    self.fit = true;
                    let page = self.current_page();
                    self.fit_zoom();
                    self.go_to(page);
                    return true;
                }
                _ => {}
            },
            _ => {}
        }
        self.scroll = self.scroll.clamp(0, max);
        redraw || self.scroll != before
    }

    /// New pages finished rendering since the last draw.
    pub fn needs_redraw(&mut self) -> bool {
        let g = self.shared.generation.load(Ordering::Relaxed);
        let changed = g != self.seen;
        self.seen = g;
        changed
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    pub fn draw(&mut self, p: &mut Painter, r: Rect) {
        let t = *p.theme;
        let (head, body) = split_top(r, 44);
        p.fill(head, t.surface);
        p.hline(head.x, head.bottom() as i32 - 1, head.w, t.border);
        osyx_ui::icon::draw(p, osyx_ui::Icon::TextFile, Rect::new(head.x + 14, head.y + 12, 20, 20), t.text_dim);
        let name = self.title.clone().unwrap_or_else(|| self.path.clone());
        let title_w = (self.prev.rect.x - head.x - 56).max(40) as u32;
        p.text_in(FontKind::Bold, Rect::new(head.x + 44, head.y, title_w, head.h), &name, t.text, Align::Left);
        let page = self.current_page();
        let label = format!("{} / {}", page + 1, self.sizes.len());
        p.text_in(
            FontKind::Ui,
            Rect::new(self.prev.rect.right() as i32, head.y, 52, head.h),
            &label,
            t.text,
            Align::Center,
        );
        for b in [&self.close, &self.prev, &self.next, &self.smaller, &self.larger, &self.fit_btn] {
            b.draw(p, false);
        }
        p.fill(body, t.field_border);
        if let Some(e) = self.error() {
            p.text_in(FontKind::Ui, body, &format!("Cannot show this PDF: {e}"), t.danger_hover, Align::Center);
            return;
        }
        let (tops, _) = self.layout_pages();
        let zk = self.zoom_key();
        let old = p.push_clip(body);
        let mut visible = Vec::new();
        for (i, &(pw, ph)) in self.sizes.iter().enumerate() {
            let (w, h) = ((pw * self.zoom) as i64, (ph * self.zoom) as i64);
            let y = body.y as i64 + tops[i] - self.scroll;
            if y > body.bottom() as i64 || y + h < body.y as i64 {
                continue;
            }
            visible.push(i);
            let x = body.x as i64 + ((body.w as i64 - w) / 2).max(GAP);
            let page_rect = Rect::new(x as i32, y as i32, w as u32, h as u32);
            // A soft shadow under the page.
            p.blend(Rect::new(page_rect.x + 2, page_rect.y + 3, page_rect.w, page_rect.h), rgb(0, 0, 0), 40);
            let bmp = self.shared.done.lock().unwrap().get(&(i, zk)).cloned();
            match bmp {
                Some(b) => blit(p, &b, page_rect.x, page_rect.y),
                None => {
                    p.fill(page_rect, rgb(255, 255, 255));
                    p.text_in(FontKind::Ui, page_rect, "Rendering...", rgb(140, 140, 140), Align::Center);
                }
            }
        }
        p.restore_clip(old);
        // Ask for the visible pages and the next one, drop far ones.
        if let Some(&last) = visible.last() {
            if last + 1 < self.sizes.len() {
                visible.push(last + 1);
            }
        }
        for &i in &visible {
            let k = (i, zk);
            if !self.shared.done.lock().unwrap().contains_key(&k) && !self.requested.contains(&k) {
                self.requested.insert(k);
                let _ = self.tx.send(k);
            }
        }
        let mut done = self.shared.done.lock().unwrap();
        if done.len() > KEEP {
            let keep: BTreeSet<Key> = visible.iter().map(|&i| (i, zk)).collect();
            let drop: Vec<Key> = done.keys().filter(|k| !keep.contains(k)).copied().collect();
            for k in drop.into_iter().take(done.len() - KEEP) {
                done.remove(&k);
                self.requested.remove(&k);
            }
        }
    }
}

/// Copies a rendered page onto the window at (x, y), clipped.
fn blit(p: &mut Painter, b: &Bitmap, x: i32, y: i32) {
    let clip = p.clip();
    let y0 = y.max(clip.y);
    let y1 = (y + b.h as i32).min(clip.bottom() as i32);
    let x0 = x.max(clip.x);
    let x1 = (x + b.w as i32).min(clip.right() as i32);
    if x0 >= x1 {
        return;
    }
    for yy in y0..y1 {
        let row = p.s.row_mut(yy as u32);
        let src = &b.px[(yy - y) as usize * b.w..][..b.w];
        for xx in x0..x1 {
            let c = src[(xx - x) as usize];
            row[xx as usize] = rgb(c[0], c[1], c[2]);
        }
    }
}
