//! The video player built into Files: WebM and Matroska with VP8 video and
//! Vorbis or Opus sound (`osyx_media::video`). One thread decodes pictures
//! a few frames ahead; the audio player's thread plays the sound, and the
//! frames written to the sound card are the clock the pictures follow. A
//! movie without sound follows the system clock.
//!
//! Keys: Space plays or pauses, Left/Right seek 5 s, Up/Down change the
//! volume, Home goes to the start, Escape closes.

use super::audio_view::{self, clock, Shared};
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use os_std::thread;
use os_std::time::{Duration, Instant};
use osyx_gfx::{rgb, Rect};
use osyx_input::{button, key};
use osyx_media::video::{Movie, Picture};
use osyx_ui::layout::contains;
use osyx_ui::widget::{Button, ButtonStyle};
use osyx_ui::{Align, Event as Ui, FontKind, Painter, Response};

/// Pictures decoded ahead of the clock.
const AHEAD: usize = 6;

/// State shared by the view and the picture thread.
struct Frames {
    queue: Mutex<VecDeque<Picture>>,
    /// A seek request (milliseconds + 1; 0 for none).
    seek: AtomicU64,
    quit: AtomicBool,
    /// The stream has no more pictures.
    ended: AtomicBool,
    error: Mutex<Option<String>>,
}

/// The time base: the sound's position, or the system clock.
enum Clock {
    Sound { shared: Arc<Shared>, rate: u64 },
    Wall { base_ms: u64, since: Option<Instant> },
}

pub struct VideoView {
    pub path: String,
    describe: String,
    duration_ms: u64,
    frames: Arc<Frames>,
    clock: Clock,
    /// The picture on screen.
    current: Option<Picture>,
    play: Button,
    back: Button,
    forward: Button,
    close: Button,
    quieter: Button,
    louder: Button,
    bar: Rect,
    screen: Rect,
    dragging: Option<u64>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl VideoView {
    /// Opens `path` and starts playing it.
    pub fn open(path: &str) -> Result<VideoView, String> {
        let data = os_std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let movie = Movie::open(data)?;
        let video = movie.video().ok_or("no video track")??;
        let frames = Arc::new(Frames {
            queue: Mutex::new(VecDeque::new()),
            seek: AtomicU64::new(0),
            quit: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let mut threads = Vec::new();
        let f = frames.clone();
        threads.push(thread::spawn(move || decode(video, f)));
        let clock = match movie.audio() {
            Some(Ok(dec)) => {
                let shared = Shared::new();
                let rate = dec.info().rate as u64;
                let s = shared.clone();
                threads.push(thread::spawn(move || audio_view::play(dec, s)));
                Clock::Sound { shared, rate }
            }
            // Sound this player cannot decode: the pictures still play.
            Some(Err(e)) => {
                println!("files: {path}: {e}; playing without sound");
                Clock::Wall { base_ms: 0, since: Some(Instant::now()) }
            }
            None => Clock::Wall { base_ms: 0, since: Some(Instant::now()) },
        };
        let describe = movie.describe();
        println!("files: playing video {path} ({describe})");
        Ok(VideoView {
            path: String::from(path),
            describe,
            duration_ms: movie.duration_ms.unwrap_or(0),
            frames,
            clock,
            current: None,
            play: Button::new(0, "Pause", ButtonStyle::Primary),
            back: Button::new(1, "-10 s", ButtonStyle::Secondary),
            forward: Button::new(2, "+10 s", ButtonStyle::Secondary),
            close: Button::new(3, "Close", ButtonStyle::Secondary),
            quieter: Button::new(4, "-", ButtonStyle::Secondary),
            louder: Button::new(5, "+", ButtonStyle::Secondary),
            bar: Rect::EMPTY,
            screen: Rect::EMPTY,
            dragging: None,
            threads,
        })
    }

    fn paused(&self) -> bool {
        match &self.clock {
            Clock::Sound { shared, .. } => shared.paused.load(Ordering::Relaxed),
            Clock::Wall { since, .. } => since.is_none(),
        }
    }

    fn finished(&self) -> bool {
        self.frames.ended.load(Ordering::Relaxed)
            && self.frames.queue.lock().unwrap().is_empty()
            && match &self.clock {
                Clock::Sound { shared, .. } => shared.finished.load(Ordering::Relaxed),
                Clock::Wall { .. } => true,
            }
    }

    /// The time being shown, in milliseconds.
    fn position(&self) -> u64 {
        if let Some(d) = self.dragging {
            return d;
        }
        match &self.clock {
            Clock::Sound { shared, rate } => {
                let w = shared.written.load(Ordering::Relaxed);
                if shared.finished.load(Ordering::Relaxed) {
                    // The sound ended first: the pictures run on the last
                    // one's time.
                    return self.current.as_ref().map_or(0, |p| p.ms).max(w * 1000 / rate.max(&1));
                }
                // What was written, less the card's queue.
                w.saturating_sub(rate / 5) * 1000 / rate.max(&1)
            }
            Clock::Wall { base_ms, since } => base_ms + since.map_or(0, |s| s.elapsed().as_millis() as u64),
        }
    }

    fn seek_to(&mut self, ms: u64) {
        let ms = if self.duration_ms > 0 { ms.min(self.duration_ms) } else { ms };
        self.frames.queue.lock().unwrap().clear();
        self.frames.ended.store(false, Ordering::Relaxed);
        self.frames.seek.store(ms + 1, Ordering::Relaxed);
        self.current = None;
        match &mut self.clock {
            Clock::Sound { shared, rate } => {
                let f = ms * *rate / 1000;
                shared.seek.store(f + 1, Ordering::Relaxed);
                shared.written.store(f, Ordering::Relaxed);
                if shared.finished.swap(false, Ordering::Relaxed) {
                    shared.paused.store(false, Ordering::Relaxed);
                }
            }
            Clock::Wall { base_ms, since } => {
                *base_ms = ms;
                if since.is_some() {
                    *since = Some(Instant::now());
                }
            }
        }
    }

    fn seek_by(&mut self, ms: i64) {
        let to = (self.position() as i64 + ms).max(0) as u64;
        self.seek_to(to);
    }

    fn toggle(&mut self) {
        if self.finished() {
            self.seek_to(0);
            if let Clock::Wall { since, .. } = &mut self.clock {
                *since = Some(Instant::now());
            }
            return;
        }
        match &mut self.clock {
            Clock::Sound { shared, .. } => {
                let p = shared.paused.load(Ordering::Relaxed);
                shared.paused.store(!p, Ordering::Relaxed);
            }
            Clock::Wall { base_ms, since } => match since.take() {
                Some(s) => *base_ms += s.elapsed().as_millis() as u64,
                None => *since = Some(Instant::now()),
            },
        }
    }

    fn change_volume(&mut self, delta: i32) {
        if let Clock::Sound { shared, .. } = &self.clock {
            let v = (shared.volume.load(Ordering::Relaxed) as i32 + delta).clamp(0, 150) as u32;
            shared.volume.store(v, Ordering::Relaxed);
        }
    }

    fn ms_at(&self, x: i32) -> u64 {
        let rel = (x - self.bar.x).clamp(0, self.bar.w as i32) as u64;
        rel * self.duration_ms / self.bar.w.max(1) as u64
    }

    pub fn layout(&mut self, r: Rect) {
        let (head_h, foot_h, pad) = (44u32, 96u32, 16);
        self.close.rect = Rect::new(r.right() as i32 - 96, r.y + 6, 84, 32);
        self.screen = Rect::new(r.x, r.y + head_h as i32, r.w, r.h.saturating_sub(head_h + foot_h));
        let fy = self.screen.bottom() as i32;
        let w = r.w.saturating_sub(2 * pad as u32);
        self.bar = Rect::new(r.x + pad, fy + 16, w, 8);
        let by = fy + 48;
        self.back.rect = Rect::new(r.x + pad, by, 80, 36);
        self.play.rect = Rect::new(r.x + pad + 90, by, 110, 36);
        self.forward.rect = Rect::new(r.x + pad + 210, by, 80, 36);
        let vx = r.right() as i32 - pad - 150;
        self.quieter.rect = Rect::new(vx, by, 36, 36);
        self.louder.rect = Rect::new(vx + 114, by, 36, 36);
    }

    /// True to redraw; `close` is set when the view should go away.
    pub fn event(&mut self, ev: &Ui, close: &mut bool) -> bool {
        let mut redraw = false;
        for id in 0..6 {
            let b = match id {
                0 => &mut self.play,
                1 => &mut self.back,
                2 => &mut self.forward,
                3 => &mut self.close,
                4 => &mut self.quieter,
                _ => &mut self.louder,
            };
            match b.event(ev, false) {
                Response::Activated(_) => {
                    match id {
                        0 => self.toggle(),
                        1 => self.seek_by(-10_000),
                        2 => self.seek_by(10_000),
                        3 => *close = true,
                        4 => self.change_volume(-10),
                        _ => self.change_volume(10),
                    }
                    return true;
                }
                r => redraw |= r.needs_redraw(),
            }
        }
        let grab = Rect::new(self.bar.x - 6, self.bar.y - 10, self.bar.w + 12, self.bar.h + 20);
        match *ev {
            Ui::PointerDown { x, y, button: b, .. }
                if b == button::LEFT && contains(&grab, x, y) && self.duration_ms > 0 =>
            {
                self.dragging = Some(self.ms_at(x));
                return true;
            }
            Ui::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&self.screen, x, y) => {
                self.toggle();
                return true;
            }
            Ui::PointerMove { x, .. } if self.dragging.is_some() => {
                self.dragging = Some(self.ms_at(x));
                return true;
            }
            Ui::PointerUp { x, .. } if self.dragging.is_some() => {
                let ms = self.ms_at(x);
                self.dragging = None;
                self.seek_to(ms);
                return true;
            }
            Ui::Key(k) if k.down => match k.key {
                key::SPACE => {
                    self.toggle();
                    return true;
                }
                key::LEFT => {
                    self.seek_by(-5000);
                    return true;
                }
                key::RIGHT => {
                    self.seek_by(5000);
                    return true;
                }
                key::UP => {
                    self.change_volume(10);
                    return true;
                }
                key::DOWN => {
                    self.change_volume(-10);
                    return true;
                }
                key::HOME => {
                    self.seek_to(0);
                    return true;
                }
                key::ESCAPE => {
                    *close = true;
                    return true;
                }
                _ => {}
            },
            _ => {}
        }
        redraw
    }

    pub fn error(&self) -> Option<String> {
        if let Some(e) = self.frames.error.lock().unwrap().clone() {
            return Some(e);
        }
        match &self.clock {
            Clock::Sound { shared, .. } => shared.error.lock().unwrap().clone(),
            Clock::Wall { .. } => None,
        }
    }

    /// Moves to the newest picture due by now. True if it changed.
    fn advance(&mut self) -> bool {
        let now = self.position();
        let mut q = self.frames.queue.lock().unwrap();
        let mut changed = false;
        while q.front().is_some_and(|p| p.ms <= now || self.current.is_none()) {
            self.current = q.pop_front();
            changed = true;
            if self.current.as_ref().is_some_and(|p| p.ms > now) {
                break;
            }
        }
        changed
    }

    pub fn draw(&mut self, p: &mut Painter, r: Rect) {
        self.advance();
        let t = *p.theme;
        let (head, _) = osyx_ui::layout::split_top(r, 44);
        p.fill(head, t.surface);
        p.hline(head.x, head.bottom() as i32 - 1, head.w, t.border);
        osyx_ui::icon::draw(p, osyx_ui::Icon::Volume, Rect::new(head.x + 14, head.y + 12, 20, 20), t.text_dim);
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        let title = format!("{name}  ·  {}", self.describe);
        p.text_in(FontKind::Bold, Rect::new(head.x + 44, head.y, head.w.saturating_sub(160), head.h), &title, t.text, Align::Left);
        self.close.draw(p, false);
        // The picture, fitted and centred on black.
        let s = self.screen;
        p.fill(s, rgb(0, 0, 0));
        if let Some(pic) = &self.current {
            let scale = (s.w as f64 / pic.w.max(1) as f64).min(s.h as f64 / pic.h.max(1) as f64);
            let (dw, dh) = ((pic.w as f64 * scale) as i32, (pic.h as f64 * scale) as i32);
            let (x0, y0) = (s.x + (s.w as i32 - dw) / 2, s.y + (s.h as i32 - dh) / 2);
            let old = p.push_clip(s);
            draw_picture(p, pic, x0, y0, dw.max(1) as u32, dh.max(1) as u32);
            p.restore_clip(old);
        }
        let foot = Rect::new(r.x, s.bottom() as i32, r.w, (r.bottom() - s.bottom()).max(0) as u32);
        p.fill(foot, t.window);
        if let Some(e) = self.error() {
            p.text_in(FontKind::Ui, Rect::new(s.x + 16, s.bottom() as i32 - 30, s.w - 32, 24), &e, t.danger_hover, Align::Left);
        }
        let pos = self.position().min(self.duration_ms.max(self.position()));
        let b = self.bar;
        p.rounded(b, b.h / 2, t.raised, 255);
        let done = if self.duration_ms == 0 { 0 } else { (pos.min(self.duration_ms) * b.w as u64 / self.duration_ms) as u32 };
        if done > 0 {
            p.rounded(Rect::new(b.x, b.y, done.max(b.h), b.h), b.h / 2, t.accent, 255);
        }
        p.disc(b.x + done as i32, b.y + b.h as i32 / 2, 7, t.accent, 255);
        let times = format!("{} / {}", clock(pos), clock(self.duration_ms));
        let fwd = self.forward.rect;
        p.text_in(FontKind::Ui, Rect::new(fwd.right() as i32 + 16, fwd.y, 180, fwd.h), &times, t.text_dim, Align::Left);
        let finished = self.finished();
        self.play.label = String::from(if finished {
            "Replay"
        } else if self.paused() {
            "Play"
        } else {
            "Pause"
        });
        self.play.draw(p, false);
        self.back.draw(p, false);
        self.forward.draw(p, false);
        if let Clock::Sound { shared, .. } = &self.clock {
            self.quieter.draw(p, false);
            self.louder.draw(p, false);
            let vol = format!("{}%", shared.volume.load(Ordering::Relaxed));
            let q = self.quieter.rect;
            p.text_in(FontKind::Ui, Rect::new(q.right() as i32, q.y, 78, q.h), &vol, t.text, Align::Center);
        }
    }

    /// Redraws while playing (pictures change) and when they are due.
    pub fn needs_redraw(&mut self) -> bool {
        let due = self.frames.queue.lock().unwrap().front().is_some_and(|p| p.ms <= self.position());
        due || (!self.paused() && !self.finished())
    }
}

impl Drop for VideoView {
    fn drop(&mut self) {
        self.frames.quit.store(true, Ordering::Relaxed);
        if let Clock::Sound { shared, .. } = &self.clock {
            shared.quit.store(true, Ordering::Relaxed);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Draws `pic` scaled into `w` by `h` at (x0, y0), nearest pixels.
fn draw_picture(p: &mut Painter, pic: &Picture, x0: i32, y0: i32, w: u32, h: u32) {
    let clip = p.clip();
    let xs: Vec<(i32, usize)> = (0..w as i32)
        .map(|dx| (x0 + dx, (dx as usize * pic.w / w as usize).min(pic.w - 1)))
        .filter(|&(x, _)| x >= clip.x && x < clip.right() as i32)
        .collect();
    for dy in 0..h as i32 {
        let y = y0 + dy;
        if y < clip.y || y >= clip.bottom() as i32 {
            continue;
        }
        let sy = (dy as usize * pic.h / h as usize).min(pic.h - 1);
        let src = &pic.px[sy * pic.w..(sy + 1) * pic.w];
        let row = p.s.row_mut(y as u32);
        for &(x, sx) in &xs {
            let c = src[sx];
            row[x as usize] = rgb(c[0], c[1], c[2]);
        }
    }
}

/// The picture thread: decodes ahead, follows seeks.
fn decode(mut v: osyx_media::video::VideoStream, f: Arc<Frames>) {
    loop {
        if f.quit.load(Ordering::Relaxed) {
            break;
        }
        let seek = f.seek.swap(0, Ordering::Relaxed);
        if seek != 0 {
            v.seek(seek - 1);
            f.queue.lock().unwrap().clear();
            f.ended.store(false, Ordering::Relaxed);
        }
        if f.ended.load(Ordering::Relaxed) || f.queue.lock().unwrap().len() >= AHEAD {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        match v.next_picture() {
            Some(Ok(p)) => {
                // A seek may have come in while decoding.
                if f.seek.load(Ordering::Relaxed) == 0 {
                    f.queue.lock().unwrap().push_back(p);
                }
            }
            Some(Err(e)) => {
                println!("files: video stopped: {e}");
                *f.error.lock().unwrap() = Some(e);
                f.ended.store(true, Ordering::Relaxed);
            }
            None => f.ended.store(true, Ordering::Relaxed),
        }
    }
}
