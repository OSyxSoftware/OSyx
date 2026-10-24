//! PRISM, the OSyx compositor (`/system/bin/prism`). Many surfaces, one vision.
//!
//! A ring-3 process that owns the display and the input seat:
//! * takes the framebuffer from the kernel (`display_acquire`, root only),
//! * opens `/dev/input/seat0` (exclusive; raw USB HID events from the
//!   kernel input core),
//! * serves the `prism` IPC service: every client connection is a kernel
//!   channel carrying PRISM protocol messages, with pixel buffers passed as
//!   shared memory handles (mapped read-only here),
//! * starts the desktop shell and grants it the shell role.
//!
//! Window management, protocol validation and composition are done by
//! `prism_core::Compositor`; this program is the platform around it: it
//! waits on all handles at once, feeds messages and input events to the
//! core, renders damaged areas into a back buffer at most 60 times a second
//! and copies them to the framebuffer. A client that breaks the protocol,
//! stops reading its events or crashes is disconnected and its windows are
//! removed; PRISM itself keeps running.
//!
//! Arguments: `prism [shell program]` (default `/system/bin/desktop`).

#![no_std]
#![no_main]

use os_std::fs::File;
use os_std::ipc::{self, Channel, Listener, Mapping, SharedMemory, WaitItem, WAIT_HANGUP, WAIT_READABLE};
use os_std::prelude::*;
use os_std::process::Command;
use os_std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use os_std::time::{Duration, Instant};
use os_std::{env, fs, system, thread};
use osyx_gfx::{Rect, Surface};
use osyx_input::layout::Layout;
use osyx_input::InputEvent;
use prism_core::compositor::WindowSnapshot;
use prism_core::{ClientId, Compositor, Config, PixelSource};

os_std::entry!(main);

const FRAME_MS: u64 = 16;
const MAX_CLIENTS: usize = 48;
/// Events PRISM could not deliver to a client before it is considered hung.
const MAX_UNDELIVERED: u32 = 512;
const SHELL_RESTARTS: u32 = 5;

/// A client buffer: shared memory mapped read-only.
struct ShmBuffer {
    _shm: SharedMemory,
    map: Mapping,
}

impl PixelSource for ShmBuffer {
    fn words(&self) -> &[u32] {
        self.map.words()
    }
}

struct Client {
    id: ClientId,
    chan: Channel,
    pid: u64,
    undelivered: u32,
    /// Events that did not fit the client's queue yet (kept in order).
    backlog: VecDeque<Vec<u8>>,
}

static SHELL_EXITED: AtomicBool = AtomicBool::new(false);
static SHELL_STATUS: AtomicI32 = AtomicI32::new(0);

fn now_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn main() -> i32 {
    let args: Vec<String> = env::args().collect();
    let shell_program = args.get(1).cloned().unwrap_or_else(|| String::from("/system/bin/desktop"));
    let info = match system::display_info() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("prism: no display: {e}");
            return 1;
        }
    };
    let fb = match system::display_acquire() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("prism: cannot take the display: {e}");
            return 1;
        }
    };
    let rgb = info.format == system::pixel::RGBX;
    let gpu = info.backend == system::BACKEND_VIRTIO_GPU;
    println!(
        "prism: display {}x{} (stride {} px, {}) acquired, backend {}",
        info.width,
        info.height,
        info.stride,
        if rgb { "RGBX" } else { "BGRX" },
        if gpu { "virtio-gpu (damage flushed per frame)" } else { "linear framebuffer" }
    );
    let listener = match Listener::listen(prism_proto::SERVICE) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("prism: cannot register the prism service: {e}");
            let _ = system::display_release();
            return 1;
        }
    };
    let seat = match File::open("/dev/input/seat0") {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("prism: no input seat ({e}); running without keyboard and mouse");
            None
        }
    };
    let mut cfg = Config::new(info.width, info.height);
    if let Some(l) = fs::read_to_string("/etc/osyx/keyboard").ok().and_then(|s| Layout::from_name(s.trim())) {
        cfg.layout = l;
    }
    println!("prism: keyboard layout {}", cfg.layout.name());
    let mut comp = Compositor::new(cfg);
    comp.set_cursor_overlay(true);
    println!("prism: cursor overlay follows input independently of the 60 Hz application frame limit");
    let mut prism = Prism {
        start: Instant::now(),
        comp,
        clients: Vec::new(),
        back: vec![0u32; (info.width * info.height) as usize],
        fb,
        stride: info.stride,
        width: info.width,
        height: info.height,
        rgb,
        gpu,
        flush_time_us: 0,
        flushes: 0,
        flushed_pixels: 0,
        last_frame: 0,
        cursor_stamp: None,
        cursor_area: Rect::EMPTY,
        cursor_saved: Vec::with_capacity(1024),
        cursor_updates: 0,
        last_snapshot: Vec::new(),
        last_focus: None,
        geometry_dirty_at: None,
        settled_base: None,
        frame_time_us: 0,
        frames_measured: 0,
        stats_at: 0,
        shell_program,
        shell_pid: None,
        shell_restarts: 0,
    };
    prism.start_shell();
    let r = prism.run(&listener, seat);
    let _ = system::display_release();
    r
}

struct Prism {
    start: Instant,
    comp: Compositor,
    clients: Vec<Client>,
    back: Vec<u32>,
    fb: *mut u32,
    stride: u32,
    width: u32,
    height: u32,
    rgb: bool,
    /// VirtIO-GPU backend: changed rectangles must be flushed.
    gpu: bool,
    flush_time_us: u64,
    flushes: u64,
    flushed_pixels: u64,
    last_frame: u64,
    cursor_stamp: Option<(i32, i32, prism_core::draw::CursorShape)>,
    cursor_area: Rect,
    cursor_saved: Vec<u32>,
    cursor_updates: u64,
    last_snapshot: Vec<WindowSnapshot>,
    last_focus: Option<u32>,
    /// When window geometry last changed (logged once it settles).
    geometry_dirty_at: Option<u64>,
    /// Window states before the unsettled changes.
    settled_base: Option<Vec<WindowSnapshot>>,
    frame_time_us: u64,
    frames_measured: u64,
    stats_at: u64,
    shell_program: String,
    shell_pid: Option<u64>,
    shell_restarts: u32,
}

impl Prism {
    fn start_shell(&mut self) {
        match Command::new(&self.shell_program).env("PRISM", "1").spawn() {
            Ok(child) => {
                let pid = child.id();
                println!("prism: desktop shell {} started as process {}", self.shell_program, pid);
                self.comp.set_shell_pid(Some(pid as u32));
                self.shell_pid = Some(pid);
                SHELL_EXITED.store(false, Ordering::SeqCst);
                // Reap the shell in the background; the main loop restarts it.
                thread::spawn(move || {
                    let status = child.wait().map(|s| s.raw()).unwrap_or(-1);
                    SHELL_STATUS.store(status, Ordering::SeqCst);
                    SHELL_EXITED.store(true, Ordering::SeqCst);
                });
            }
            Err(e) => eprintln!("prism: cannot start the desktop shell {}: {e}", self.shell_program),
        }
    }

    fn run(&mut self, listener: &Listener, mut seat: Option<File>) -> i32 {
        let mut msg = vec![0u8; prism_proto::MAX_MESSAGE];
        let mut input = vec![0u8; InputEvent::SIZE * 64];
        println!("prism: compositor running");
        loop {
            let now = now_ms(self.start);
            // Sleep until something happens, or until the next frame is due.
            let timeout = match self.comp.next_frame_in(now) {
                Some(_) => FRAME_MS.saturating_sub(now.saturating_sub(self.last_frame)),
                None if self.geometry_dirty_at.is_some() => 100,
                None => 1000,
            };
            let mut items: Vec<WaitItem> = Vec::with_capacity(self.clients.len() + 2);
            items.push(ipc::item(listener.handle(), WAIT_READABLE));
            if let Some(s) = &seat {
                items.push(ipc::item(s.handle(), WAIT_READABLE));
            }
            let first_client = items.len();
            for c in &self.clients {
                items.push(ipc::item(c.chan.handle(), WAIT_READABLE));
            }
            if let Err(e) = ipc::wait(&mut items, Some(Duration::from_millis(timeout))) {
                eprintln!("prism: wait failed: {e}");
                thread::sleep(Duration::from_millis(10));
            }
            let now = now_ms(self.start);
            if items[0].revents & WAIT_READABLE != 0 {
                self.accept(listener);
            }
            if let Some(s) = seat.as_mut() {
                if items[1].revents & WAIT_READABLE != 0 {
                    self.read_input(s, &mut input, now);
                }
            }
            // Client messages (the client list may have grown above; only
            // the ones we waited for have revents).
            let ready: Vec<(usize, u32)> =
                items[first_client..].iter().enumerate().map(|(i, it)| (i, it.revents)).collect();
            let mut gone = Vec::new();
            for (i, revents) in ready {
                if revents & (WAIT_READABLE | WAIT_HANGUP) == 0 {
                    continue;
                }
                if !self.read_client(i, &mut msg, now) {
                    gone.push(self.clients[i].id);
                }
            }
            for id in gone {
                self.drop_client(id, "disconnected");
            }
            self.deliver();
            self.reap_dead_clients();
            self.supervise_shell();
            self.log_window_changes(now);
            if self.comp.next_frame_in(now).is_some() && now.saturating_sub(self.last_frame) >= FRAME_MS {
                self.frame(now);
                self.deliver();
            }
            self.paint_cursor(false);
            if now >= self.stats_at + 30_000 {
                self.stats_at = now;
                self.log_stats();
            }
        }
    }

    fn accept(&mut self, listener: &Listener) {
        loop {
            match listener.accept() {
                Ok(Some(chan)) => {
                    let (pid, uid) = chan.peer().unwrap_or((0, u32::MAX));
                    if self.clients.len() >= MAX_CLIENTS {
                        eprintln!("prism: refusing process {pid}: {MAX_CLIENTS} clients connected");
                        continue;
                    }
                    let id = self.comp.connect(pid as u32);
                    println!("prism: client {id} connected (process {pid}, uid {uid})");
                    self.clients.push(Client { id, chan, pid, undelivered: 0, backlog: VecDeque::new() });
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("prism: accept failed: {e}");
                    break;
                }
            }
        }
    }

    fn read_input(&mut self, seat: &mut File, buf: &mut [u8], now: u64) {
        let n = match seat.read(buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("prism: input read failed: {e}");
                return;
            }
        };
        for chunk in buf[..n].as_chunks::<{ InputEvent::SIZE }>().0 {
            let Some(ev) = InputEvent::decode(chunk) else { continue };
            match ev.kind {
                osyx_input::kind::DEVICE => {
                    let what = if ev.code == osyx_input::DEVICE_ADDED { "added" } else { "removed" };
                    println!("prism: input device {} {} (class {})", ev.device, what, ev.value);
                }
                osyx_input::kind::DROPPED => eprintln!("prism: {} input events were dropped", ev.value),
                osyx_input::kind::BUTTON if ev.value != 0 => {
                    let (x, y) = self.comp.pointer();
                    // Where clicks land is evidence for the integration tests.
                    println!("prism: button {} pressed at ({}, {})", ev.code, x, y);
                }
                _ => {}
            }
            self.comp.input(&ev, now);
        }
    }

    /// Handles everything queued on client `i`; false if it must go.
    fn read_client(&mut self, i: usize, buf: &mut [u8], now: u64) -> bool {
        // Bounded per turn so one client cannot starve the others.
        for _ in 0..256 {
            let id = self.clients[i].id;
            match self.clients[i].chan.recv(buf) {
                Ok(Some((n, handles))) => {
                    let buffer = match handles.into_iter().next() {
                        Some(h) => match SharedMemory::from_handle(h).and_then(|s| s.map(false).map(|m| (s, m))) {
                            Ok((shm, map)) => Some(Box::new(ShmBuffer { _shm: shm, map }) as Box<dyn PixelSource>),
                            Err(e) => {
                                eprintln!("prism: client {id}: unusable buffer handle: {e}");
                                return false;
                            }
                        },
                        None => None,
                    };
                    if let Err(e) = self.comp.handle(id, &buf[..n], buffer, now) {
                        eprintln!(
                            "prism: client {id} (process {}) broke the protocol: {}",
                            self.clients[i].pid, e.message
                        );
                        // The ERROR event is delivered before the disconnect.
                        return true;
                    }
                }
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
        true
    }

    fn deliver(&mut self) {
        for (cid, bytes) in self.comp.take_messages() {
            if let Some(c) = self.clients.iter_mut().find(|c| c.id == cid) {
                c.backlog.push_back(bytes);
            }
        }
        let mut hung = Vec::new();
        for c in &mut self.clients {
            while let Some(m) = c.backlog.front() {
                match c.chan.send(m, &[]) {
                    Ok(()) => {
                        c.backlog.pop_front();
                        c.undelivered = 0;
                    }
                    Err(e) if e.kind() == os_std::error::ErrorKind::WouldBlock => {
                        c.undelivered = c.backlog.len() as u32;
                        break;
                    }
                    Err(_) => {
                        c.backlog.clear();
                        break;
                    }
                }
            }
            if c.undelivered > MAX_UNDELIVERED {
                hung.push(c.id);
            }
        }
        for id in hung {
            self.drop_client(id, "stopped reading its events");
        }
    }

    fn reap_dead_clients(&mut self) {
        let dead: Vec<ClientId> =
            self.clients.iter().filter(|c| self.comp.is_dead(c.id) && c.backlog.is_empty()).map(|c| c.id).collect();
        for id in dead {
            self.drop_client(id, "disconnected after a protocol error");
        }
    }

    fn drop_client(&mut self, id: ClientId, why: &str) {
        let Some(pos) = self.clients.iter().position(|c| c.id == id) else { return };
        let c = self.clients.remove(pos);
        let res = self.comp.client_resources(id);
        self.comp.disconnect(id);
        match res {
            Some((s, b, w, px)) => println!(
                "prism: client {id} (process {}) {why}; released {w} window(s), {s} surface(s), {b} buffer(s), {} KiB of pixels",
                c.pid,
                px * 4 / 1024
            ),
            None => println!("prism: client {id} (process {}) {why}", c.pid),
        }
    }

    fn supervise_shell(&mut self) {
        if !SHELL_EXITED.swap(false, Ordering::SeqCst) {
            return;
        }
        let status = SHELL_STATUS.load(Ordering::SeqCst);
        eprintln!("prism: desktop shell (process {:?}) exited with status {status:#x}", self.shell_pid);
        self.comp.set_shell_pid(None);
        self.shell_pid = None;
        if self.shell_restarts < SHELL_RESTARTS {
            self.shell_restarts += 1;
            println!("prism: restarting the desktop shell ({} of {SHELL_RESTARTS})", self.shell_restarts);
            self.start_shell();
        } else {
            eprintln!("prism: the desktop shell keeps failing; not restarting it");
        }
    }

    fn frame(&mut self, now: u64) {
        let t0 = Instant::now();
        let (w, h) = (self.width, self.height);
        let rects = {
            let mut s = Surface::new(&mut self.back, w, h, w).expect("back buffer geometry");
            self.comp.render(&mut s, now)
        };
        for r in &rects {
            self.present(*r);
        }
        if self.gpu {
            let t1 = Instant::now();
            for r in &rects {
                let r = r.intersect(&Rect::new(0, 0, self.width, self.height));
                if r.w == 0 || r.h == 0 {
                    continue;
                }
                if let Err(e) = system::display_flush(r.x as u32, r.y as u32, r.w, r.h) {
                    eprintln!("prism: display flush failed: {e}");
                }
                self.flushes += 1;
                self.flushed_pixels += r.w as u64 * r.h as u64;
            }
            self.flush_time_us += t1.elapsed().as_micros() as u64;
        }
        self.last_frame = now;
        self.paint_cursor(true);
        if !rects.is_empty() {
            self.frame_time_us += t0.elapsed().as_micros() as u64;
            self.frames_measured += 1;
        }
    }

    /// Restore the old cursor from the clean back buffer, draw only the
    /// new cursor rectangle, then restore the clean pixels in RAM. Moving
    /// the pointer does not copy or recompose a full desktop surface.
    fn paint_cursor(&mut self, force: bool) {
        let stamp = self.comp.cursor_state();
        if !force && self.cursor_stamp == Some(stamp) {
            return;
        }
        let old = self.cursor_area;
        if !old.is_empty() {
            self.present(old);
        }
        let area = self.comp.cursor_area();
        self.cursor_saved.clear();
        for y in area.y as u32..area.bottom() as u32 {
            let start = (y * self.width + area.x as u32) as usize;
            self.cursor_saved.extend_from_slice(&self.back[start..start + area.w as usize]);
        }
        {
            let mut surface =
                Surface::new(&mut self.back, self.width, self.height, self.width).expect("cursor geometry");
            self.comp.draw_cursor_overlay(&mut surface);
        }
        self.present(area);
        for (row, y) in (area.y as u32..area.bottom() as u32).enumerate() {
            let start = (y * self.width + area.x as u32) as usize;
            self.back[start..start + area.w as usize]
                .copy_from_slice(&self.cursor_saved[row * area.w as usize..(row + 1) * area.w as usize]);
        }
        if self.gpu {
            for rect in [old, area] {
                if !rect.is_empty() {
                    let _ = system::display_flush(rect.x as u32, rect.y as u32, rect.w, rect.h);
                }
            }
        }
        self.cursor_stamp = Some(stamp);
        self.cursor_area = area;
        self.cursor_updates += 1;
    }

    /// Copies `r` of the back buffer to the framebuffer.
    fn present(&mut self, r: Rect) {
        let r = r.intersect(&Rect::new(0, 0, self.width, self.height));
        for y in r.y..r.y + r.h as i32 {
            let src = &self.back[y as usize * self.width as usize + r.x as usize..][..r.w as usize];
            // SAFETY: `fb` maps `stride * height` pixels (display_info().size
            // bytes) for as long as PRISM owns the display; `r` is inside
            // `width x height` and `width <= stride`.
            let dst = unsafe {
                core::slice::from_raw_parts_mut(
                    self.fb.add(y as usize * self.stride as usize + r.x as usize),
                    r.w as usize,
                )
            };
            if self.rgb {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = osyx_gfx::to_device(*s, true);
                }
            } else {
                dst.copy_from_slice(src);
            }
        }
    }

    /// Logs focus changes at once and geometry changes once they settle
    /// (a drag is reported by its end position, not every step).
    fn log_window_changes(&mut self, now: u64) {
        let focus = self.comp.focused();
        let snap = self.comp.snapshot();
        if focus != self.last_focus {
            let title = snap.iter().find(|w| Some(w.id) == focus).map_or("", |w| w.title.as_str());
            match focus {
                Some(id) => println!("prism: focus -> window {id} \"{title}\""),
                None => println!("prism: focus -> none"),
            }
            self.last_focus = focus;
        }
        let changed = snap.len() != self.last_snapshot.len()
            || snap.iter().zip(&self.last_snapshot).any(|(a, b)| {
                a.id != b.id
                    || a.rect != b.rect
                    || a.minimized != b.minimized
                    || a.maximized != b.maximized
                    || a.mapped != b.mapped
                    || a.title != b.title
            });
        if changed {
            self.geometry_dirty_at = Some(now);
            // Windows that appeared or vanished are reported immediately.
            for w in snap.iter().filter(|w| !self.last_snapshot.iter().any(|o| o.id == w.id)) {
                println!("prism: window {} \"{}\" created ({:?}, client {})", w.id, w.title, w.role, w.client);
            }
            for w in self.last_snapshot.iter().filter(|o| !snap.iter().any(|w| w.id == o.id)) {
                println!("prism: window {} \"{}\" closed", w.id, w.title);
            }
            let pending = core::mem::take(&mut self.last_snapshot);
            self.last_snapshot = snap;
            // Keep the pre-change geometry for the settled report.
            if self.settled_base.is_none() {
                self.settled_base = Some(pending);
            }
        } else if self.geometry_dirty_at.is_some_and(|t| now.saturating_sub(t) >= 250) {
            self.geometry_dirty_at = None;
            let base = self.settled_base.take().unwrap_or_default();
            for w in &self.last_snapshot {
                let r = w.rect;
                let Some(o) = base.iter().find(|o| o.id == w.id) else {
                    // Created and shown within this period.
                    if w.mapped {
                        println!("prism: window {} \"{}\" mapped at ({}, {}) {}x{}", w.id, w.title, r.x, r.y, r.w, r.h);
                    }
                    continue;
                };
                if (o.rect.x, o.rect.y) != (r.x, r.y) && (o.rect.w, o.rect.h) == (r.w, r.h) {
                    println!("prism: window {} \"{}\" moved to ({}, {})", w.id, w.title, r.x, r.y);
                } else if (o.rect.w, o.rect.h) != (r.w, r.h) {
                    println!("prism: window {} \"{}\" resized to {}x{} at ({}, {})", w.id, w.title, r.w, r.h, r.x, r.y);
                }
                if o.minimized != w.minimized {
                    println!(
                        "prism: window {} \"{}\" {}",
                        w.id,
                        w.title,
                        if w.minimized { "minimized" } else { "restored" }
                    );
                }
                if o.maximized != w.maximized {
                    println!(
                        "prism: window {} \"{}\" {}",
                        w.id,
                        w.title,
                        if w.maximized { "maximized" } else { "unmaximized" }
                    );
                }
                if o.mapped != w.mapped && w.mapped {
                    println!("prism: window {} \"{}\" mapped at ({}, {}) {}x{}", w.id, w.title, r.x, r.y, r.w, r.h);
                }
                if o.title != w.title {
                    println!("prism: window {} title \"{}\"", w.id, w.title);
                }
            }
        }
    }

    fn log_stats(&mut self) {
        let s = self.comp.stats();
        let avg = self.frame_time_us.checked_div(self.frames_measured).unwrap_or(0);
        println!(
            "prism: {} frames, {} Mpx composed, {} commits, {} input events, {} clients; frame time avg {}.{:03} ms",
            s.frames,
            s.pixels / 1_000_000,
            s.commits,
            s.input_events,
            self.clients.len(),
            avg / 1000,
            avg % 1000
        );
        println!("prism: {} cursor overlay updates without desktop recomposition", self.cursor_updates);
        if self.gpu {
            let per = self.flush_time_us.checked_div(self.flushes).unwrap_or(0);
            println!(
                "prism: virtio-gpu: {} damage flushes, {} Mpx transferred, avg {} us per flush ({} ms in flushes)",
                self.flushes,
                self.flushed_pixels / 1_000_000,
                per,
                self.flush_time_us / 1000
            );
        }
    }
}
