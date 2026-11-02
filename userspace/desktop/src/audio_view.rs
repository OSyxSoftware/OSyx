//! The audio player built into Files: decodes WAV, FLAC, MP3, AAC and Ogg Vorbis
//! with `osyx_media` on a thread of its own and writes 48 kHz stereo frames
//! to `/dev/audio`. The view shows the file, its format, the position on a
//! seek bar, and Play/Pause, Back 10 s, Forward 10 s and a volume control.
//!
//! Keys: Space plays or pauses, Left/Right seek 5 s, Up/Down change the
//! volume, Home goes to the start.

use os_std::fs::OpenOptions;
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use os_std::thread;
use os_std::time::Duration;
use osyx_gfx::Rect;
use osyx_input::{button, key};
use osyx_media::audio::resample::Converter;
use osyx_media::audio::{self, Decoder, Info};
use osyx_ui::layout::contains;
use osyx_ui::widget::{Button, ButtonStyle};
use osyx_ui::{Align, Event as Ui, FontKind, Painter, Response};

/// Frames converted per write: small enough that pausing and seeking feel
/// immediate, large enough to keep the card's 200 ms queue topped up.
const CHUNK_FRAMES: usize = 4096;

/// State shared by the view and the playback thread.
struct Shared {
    paused: AtomicBool,
    quit: AtomicBool,
    /// A seek request (source frames + 1; 0 for none).
    seek: AtomicU64,
    /// Source frames handed to the sound card.
    written: AtomicU64,
    finished: AtomicBool,
    /// Volume in percent.
    volume: AtomicU32,
    error: Mutex<Option<String>>,
}

pub struct AudioView {
    pub path: String,
    info: Info,
    shared: Arc<Shared>,
    play: Button,
    back: Button,
    forward: Button,
    close: Button,
    quieter: Button,
    louder: Button,
    bar: Rect,
    /// The pointer is dragging the position (the value shown meanwhile).
    dragging: Option<u64>,
    /// Paused, finished and failed as last drawn.
    shown: (bool, bool, bool),
    thread: Option<thread::JoinHandle<()>>,
}

/// m:ss or h:mm:ss.
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn channels_name(n: usize) -> String {
    match n {
        1 => String::from("mono"),
        2 => String::from("stereo"),
        n => format!("{n} channels"),
    }
}

impl AudioView {
    /// Opens `path` and starts playing it.
    pub fn open(path: &str) -> Result<AudioView, String> {
        let data = os_std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let dec = audio::open(data)?;
        let info = dec.info().clone();
        let shared = Arc::new(Shared {
            paused: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            seek: AtomicU64::new(0),
            written: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            volume: AtomicU32::new(100),
            error: Mutex::new(None),
        });
        let s = shared.clone();
        let thread = Some(thread::spawn(move || play(dec, s)));
        println!("files: playing {path} ({}, {} Hz, {})", info.codec, info.rate, channels_name(info.channels));
        Ok(AudioView {
            path: String::from(path),
            info,
            shared,
            play: Button::new(0, "Pause", ButtonStyle::Primary),
            back: Button::new(1, "-10 s", ButtonStyle::Secondary),
            forward: Button::new(2, "+10 s", ButtonStyle::Secondary),
            close: Button::new(3, "Close", ButtonStyle::Secondary),
            quieter: Button::new(4, "-", ButtonStyle::Secondary),
            louder: Button::new(5, "+", ButtonStyle::Secondary),
            bar: Rect::EMPTY,
            dragging: None,
            shown: (false, false, false),
            thread,
        })
    }

    fn total(&self) -> u64 {
        self.info.frames.unwrap_or(0)
    }

    /// The frame being heard: what was written, less the card's queue.
    fn position(&self) -> u64 {
        if let Some(d) = self.dragging {
            return d;
        }
        let w = self.shared.written.load(Ordering::Relaxed);
        if self.shared.finished.load(Ordering::Relaxed) {
            return self.total().max(w);
        }
        w.saturating_sub(self.info.rate as u64 / 5)
    }

    fn seek_to(&mut self, frame: u64) {
        let f = self.info.frames.map_or(frame, |t| frame.min(t));
        self.shared.seek.store(f + 1, Ordering::Relaxed);
        self.shared.written.store(f, Ordering::Relaxed);
        if self.shared.finished.swap(false, Ordering::Relaxed) {
            // Playback had ended: it starts again from the new position.
            self.shared.paused.store(false, Ordering::Relaxed);
        }
    }

    fn seek_by(&mut self, ms: i64) {
        let rate = self.info.rate as i64;
        let to = (self.position() as i64 + ms * rate / 1000).max(0) as u64;
        self.seek_to(to);
    }

    fn toggle(&mut self) {
        if self.shared.finished.load(Ordering::Relaxed) {
            self.seek_to(0);
            return;
        }
        let p = self.shared.paused.load(Ordering::Relaxed);
        self.shared.paused.store(!p, Ordering::Relaxed);
    }

    fn change_volume(&mut self, delta: i32) {
        let v = (self.shared.volume.load(Ordering::Relaxed) as i32 + delta).clamp(0, 150) as u32;
        self.shared.volume.store(v, Ordering::Relaxed);
    }

    fn frame_at(&self, x: i32) -> u64 {
        let rel = (x - self.bar.x).clamp(0, self.bar.w as i32) as u64;
        rel * self.total() / self.bar.w.max(1) as u64
    }

    pub fn layout(&mut self, r: Rect) {
        let (head_h, pad) = (44, 24);
        self.close.rect = Rect::new(r.right() as i32 - 96, r.y + 6, 84, 32);
        let body_y = r.y + head_h as i32;
        let cx = r.x + pad;
        let w = r.w.saturating_sub(2 * pad as u32);
        self.bar = Rect::new(cx, body_y + 150, w, 10);
        let by = body_y + 190;
        self.back.rect = Rect::new(cx, by, 80, 36);
        self.play.rect = Rect::new(cx + 90, by, 110, 36);
        self.forward.rect = Rect::new(cx + 210, by, 80, 36);
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
            // Without a known length the bar cannot map to positions.
            Ui::PointerDown { x, y, button: b, .. }
                if b == button::LEFT && contains(&grab, x, y) && self.total() > 0 =>
            {
                self.dragging = Some(self.frame_at(x));
                return true;
            }
            Ui::PointerMove { x, .. } if self.dragging.is_some() => {
                self.dragging = Some(self.frame_at(x));
                return true;
            }
            Ui::PointerUp { x, .. } if self.dragging.is_some() => {
                let f = self.frame_at(x);
                self.dragging = None;
                self.seek_to(f);
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
        self.shared.error.lock().unwrap().clone()
    }

    pub fn draw(&mut self, p: &mut Painter, r: Rect) {
        let t = *p.theme;
        let (head, body) = osyx_ui::layout::split_top(r, 44);
        p.fill(head, t.surface);
        p.hline(head.x, head.bottom() as i32 - 1, head.w, t.border);
        osyx_ui::icon::draw(p, osyx_ui::Icon::Volume, Rect::new(head.x + 14, head.y + 12, 20, 20), t.text_dim);
        p.text_in(
            FontKind::Bold,
            Rect::new(head.x + 44, head.y, head.w.saturating_sub(160), head.h),
            &self.path,
            t.text,
            Align::Left,
        );
        self.close.draw(p, false);
        p.fill(body, t.window);
        let x = body.x + 24;
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        p.text(FontKind::Large, x, body.y + 28, name, t.text);
        let detail = format!("{}  ·  {} Hz  ·  {}", self.info.codec, self.info.rate, channels_name(self.info.channels));
        p.text(FontKind::Ui, x, body.y + 70, &detail, t.text_dim);
        let total_ms = self.info.duration_ms().unwrap_or(0);
        let pos = self.position();
        let pos_ms = pos * 1000 / self.info.rate.max(1) as u64;
        let paused = self.shared.paused.load(Ordering::Relaxed);
        let finished = self.shared.finished.load(Ordering::Relaxed);
        let state = if let Some(e) = self.error() {
            e
        } else if finished {
            String::from("Finished")
        } else if paused {
            String::from("Paused")
        } else {
            String::from("Playing")
        };
        p.text(FontKind::Ui, x, body.y + 100, &state, if self.error().is_some() { t.danger_hover } else { t.accent });
        // Seek bar.
        let b = self.bar;
        p.rounded(b, b.h / 2, t.raised, 255);
        let done = if total_ms == 0 { 0 } else { (pos.min(self.total()) * b.w as u64 / self.total().max(1)) as u32 };
        if done > 0 {
            p.rounded(Rect::new(b.x, b.y, done.max(b.h), b.h), b.h / 2, t.accent, 255);
        }
        p.disc(b.x + done as i32, b.y + b.h as i32 / 2, 8, t.accent, 255);
        p.text_in(FontKind::Ui, Rect::new(b.x, b.y + 14, 120, 20), &clock(pos_ms), t.text_dim, Align::Left);
        p.text_in(
            FontKind::Ui,
            Rect::new(b.right() as i32 - 120, b.y + 14, 120, 20),
            &clock(total_ms),
            t.text_dim,
            Align::Right,
        );
        self.play.label = String::from(if finished {
            "Replay"
        } else if paused {
            "Play"
        } else {
            "Pause"
        });
        self.play.draw(p, false);
        self.back.draw(p, false);
        self.forward.draw(p, false);
        self.quieter.draw(p, false);
        self.louder.draw(p, false);
        let vol = format!("{}%", self.shared.volume.load(Ordering::Relaxed));
        let q = self.quieter.rect;
        p.text_in(FontKind::Ui, Rect::new(q.right() as i32, q.y, 78, q.h), &vol, t.text, Align::Center);
        p.text_in(FontKind::Ui, Rect::new(q.x - 80, q.y, 74, q.h), "Volume", t.text_dim, Align::Right);
    }

    /// The view redraws while sound is playing, and once when playback
    /// pauses, ends or fails.
    pub fn needs_redraw(&mut self) -> bool {
        let state = (
            self.shared.paused.load(Ordering::Relaxed),
            self.shared.finished.load(Ordering::Relaxed),
            self.shared.error.lock().unwrap().is_some(),
        );
        let changed = state != self.shown;
        self.shown = state;
        changed || !(state.0 || state.1)
    }
}

impl Drop for AudioView {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        // Wait for the thread so no old sound is queued after a new file
        // starts (a blocked write returns within the card's 200 ms queue).
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The playback thread.
fn play(mut dec: Box<dyn Decoder>, s: Arc<Shared>) {
    let info = dec.info().clone();
    let mut conv = Converter::new(info.rate, info.channels);
    let mut card = None;
    let mut queue: Vec<f32> = Vec::new();
    let fail = |s: &Shared, e: String| {
        println!("files: playback stopped: {e}");
        *s.error.lock().unwrap() = Some(e);
        s.finished.store(true, Ordering::Relaxed);
    };
    loop {
        if s.quit.load(Ordering::Relaxed) {
            break;
        }
        let seek = s.seek.swap(0, Ordering::Relaxed);
        if seek != 0 {
            match dec.seek(seek - 1) {
                Ok(f) => s.written.store(f, Ordering::Relaxed),
                Err(e) => fail(&s, e),
            }
            queue.clear();
            conv.reset();
        }
        if s.paused.load(Ordering::Relaxed) || s.finished.load(Ordering::Relaxed) {
            // Closing the card lets it stop (and other programs use it).
            card = None;
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if card.is_none() {
            match OpenOptions::new().write(true).open("/dev/audio") {
                Ok(f) => card = Some(f),
                Err(e) => {
                    fail(&s, format!("cannot open the sound card: {e}"));
                    continue;
                }
            }
        }
        while queue.len() < CHUNK_FRAMES * info.channels {
            match dec.next_block() {
                Ok(Some(b)) => queue.extend_from_slice(&b),
                Ok(None) => break,
                Err(e) => {
                    fail(&s, e);
                    break;
                }
            }
        }
        let take = queue.len().min(CHUNK_FRAMES * info.channels);
        if take == 0 {
            let tail = conv.finish();
            write_pcm(&mut card, &tail);
            // Let the queue drain before reporting the end.
            card = None;
            s.finished.store(true, Ordering::Relaxed);
            continue;
        }
        conv.gain = s.volume.load(Ordering::Relaxed) as f32 / 100.0;
        let chunk: Vec<f32> = queue.drain(..take).collect();
        let pcm = conv.push(&chunk);
        if !write_pcm(&mut card, &pcm) {
            fail(&s, String::from("the sound card stopped accepting data"));
            continue;
        }
        s.written.fetch_add((take / info.channels) as u64, Ordering::Relaxed);
    }
}

fn write_pcm(card: &mut Option<os_std::fs::File>, pcm: &[i16]) -> bool {
    let Some(f) = card else { return false };
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    f.write_all(&bytes).is_ok()
}
