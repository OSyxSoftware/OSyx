//! The PRISM client library.
//!
//! ```ignore
//! let mut conn = Connection::connect("files", false)?;
//! let mut win = conn.create_window(WindowSpec::normal("Files", 720, 480))?;
//! loop {
//!     conn.wait(&mut [], None)?;
//!     while let Some(ev) = conn.poll()? {
//!         win.on_event(&ev);
//!         // ... translate with `ui::Adapter`, update widgets ...
//!     }
//!     if win.can_draw() {
//!         draw(&mut win.surface());
//!         win.present(&mut conn, None)?;
//!     }
//! }
//! ```
//!
//! A [`Connection`] is one kernel channel to the compositor's `prism`
//! service. A [`Window`] owns a shared memory buffer that the client draws
//! into; `present` attaches it (passing the memory handle the first time),
//! reports damage and commits. PRISM copies the committed pixels and sends
//! `BUFFER_RELEASE`, after which the buffer may be drawn into again.

#![no_std]

extern crate alloc;

pub mod ui;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;
use os_std::error::{Error, ErrorKind, Result};
use os_std::ipc::{self, Channel, Mapping, SharedMemory, WaitItem, WAIT_HANGUP, WAIT_READABLE, WAIT_WRITABLE};
use os_std::time::Instant;
use osyx_gfx::Surface;
pub use prism_proto::{self as proto, window_flags, Action, Event, Rect, Request, Role};

/// How long [`Connection::connect`] retries while PRISM is starting.
const CONNECT_PATIENCE: Duration = Duration::from_secs(10);
/// Largest shared memory object the kernel creates (`osyx_abi::ipc::MAX_SHM`).
const MAX_BUFFER: u64 = 64 * 1024 * 1024;

pub struct Connection {
    chan: Channel,
    pub client_id: u32,
    pub screen_width: u32,
    pub screen_height: u32,
    /// The shell role was granted.
    pub shell: bool,
    next_id: u32,
    serial: u32,
    queue: VecDeque<Event>,
    buf: Vec<u8>,
}

impl Connection {
    /// Connects and says HELLO. `shell` asks for the desktop shell role
    /// (PRISM grants it only to the process it started as the shell).
    pub fn connect(name: &str, shell: bool) -> Result<Connection> {
        let start = Instant::now();
        let chan = loop {
            match Channel::connect(proto::SERVICE) {
                Ok(c) => break c,
                Err(e) if e.kind() == ErrorKind::NotFound && start.elapsed() < CONNECT_PATIENCE => {
                    os_std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        };
        let mut c = Connection {
            chan,
            client_id: 0,
            screen_width: 0,
            screen_height: 0,
            shell: false,
            next_id: 1,
            serial: 0,
            queue: VecDeque::new(),
            buf: alloc::vec![0; proto::MAX_MESSAGE],
        };
        let flags = if shell { proto::hello::SHELL } else { 0 };
        c.send(&Request::Hello { min_version: proto::MIN_VERSION, max_version: proto::VERSION, flags, name: String::from(name) }, &[])?;
        let asked = Instant::now();
        loop {
            c.wait_inner(&mut [], Some(Duration::from_millis(500)))?;
            while let Some(ev) = c.recv()? {
                match ev {
                    Event::Hello { client_id, flags, screen_width, screen_height, .. } => {
                        c.client_id = client_id;
                        c.shell = flags & proto::hello::SHELL != 0;
                        c.screen_width = screen_width;
                        c.screen_height = screen_height;
                        return Ok(c);
                    }
                    Event::Error { message, .. } => {
                        os_std::eprintln!("prism-client: PRISM refused the connection: {message}");
                        return Err(Error::new(ErrorKind::PermissionDenied, "PRISM refused the connection"));
                    }
                    other => c.queue.push_back(other),
                }
            }
            if asked.elapsed() > Duration::from_secs(5) {
                return Err(Error::new(ErrorKind::TimedOut, "PRISM did not answer HELLO"));
            }
        }
    }

    pub fn handle(&self) -> u64 {
        self.chan.handle()
    }

    /// A fresh id for a surface, window or buffer.
    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Sends a request; waits (bounded) while PRISM's queue for us is full.
    pub fn send(&mut self, req: &Request, handles: &[u64]) -> Result<()> {
        self.serial = self.serial.wrapping_add(1);
        let msg = req.encode(self.serial);
        let start = Instant::now();
        loop {
            match self.chan.send(&msg, handles) {
                Err(e) if e.kind() == ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(2) => {
                    let mut it = [ipc::item(self.chan.handle(), WAIT_WRITABLE)];
                    ipc::wait(&mut it, Some(Duration::from_millis(20)))?;
                }
                r => return r,
            }
        }
    }

    fn recv(&mut self) -> Result<Option<Event>> {
        match self.chan.recv(&mut self.buf)? {
            Some((n, _handles)) => match Event::decode(&self.buf[..n]) {
                Ok(e) => Ok(Some(e)),
                Err(_) => Err(Error::new(ErrorKind::InvalidData, "malformed message from PRISM")),
            },
            None => Ok(None),
        }
    }

    /// The next event without blocking. `BrokenPipe` once PRISM is gone.
    pub fn poll(&mut self) -> Result<Option<Event>> {
        if let Some(e) = self.queue.pop_front() {
            return Ok(Some(e));
        }
        self.recv()
    }

    fn wait_inner(&mut self, extra: &mut [WaitItem], timeout: Option<Duration>) -> Result<()> {
        let mut items: Vec<WaitItem> = Vec::with_capacity(extra.len() + 1);
        items.push(ipc::item(self.chan.handle(), WAIT_READABLE));
        items.extend_from_slice(extra);
        ipc::wait(&mut items, timeout)?;
        for (dst, src) in extra.iter_mut().zip(&items[1..]) {
            dst.revents = src.revents;
        }
        if items[0].revents & WAIT_HANGUP != 0 && items[0].revents & WAIT_READABLE == 0 {
            return Err(Error::from(ErrorKind::BrokenPipe));
        }
        Ok(())
    }

    /// Blocks until an event is queued, one of `extra` is ready, or the
    /// timeout passes. `revents` of `extra` are filled in.
    pub fn wait(&mut self, extra: &mut [WaitItem], timeout: Option<Duration>) -> Result<()> {
        if !self.queue.is_empty() {
            return Ok(());
        }
        self.wait_inner(extra, timeout)
    }

    pub fn create_window(&mut self, spec: WindowSpec) -> Result<Window> {
        let id = self.alloc_id();
        self.send(&Request::CreateSurface { surface: id }, &[])?;
        self.send(
            &Request::CreateWindow {
                window: id,
                surface: id,
                parent: spec.parent,
                role: spec.role,
                flags: spec.flags,
                x: spec.x,
                y: spec.y,
                width: spec.width,
                height: spec.height,
                min_width: spec.min_width,
                min_height: spec.min_height,
                title: spec.title,
            },
            &[],
        )?;
        let buffer = self.alloc_id();
        Ok(Window {
            id,
            buffer,
            shm: None,
            fresh_buffer: false,
            width: spec.width.max(1),
            height: spec.height.max(1),
            states: 0,
            configured: false,
            busy: false,
            dirty: true,
            alpha: spec.alpha,
        })
    }

    pub fn notify(&mut self, title: &str, body: &str) -> Result<()> {
        self.send(&Request::Notify { title: String::from(title), body: String::from(body) }, &[])
    }

    /// Shell only (`global`): acts on any window by its compositor id.
    pub fn window_action(&mut self, target: u32, action: Action, global: bool) -> Result<()> {
        self.send(&Request::WindowAction { target, action, global }, &[])
    }
}

#[derive(Clone, Debug)]
pub struct WindowSpec {
    pub role: Role,
    pub flags: u32,
    /// Placement hint (normal windows: negative = PRISM chooses).
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub min_width: u32,
    pub min_height: u32,
    /// Local id of the parent window (popups), 0 = none.
    pub parent: u32,
    pub title: String,
    /// The content has (premultiplied) alpha.
    pub alpha: bool,
}

impl WindowSpec {
    /// A decorated, resizable application window placed by PRISM.
    pub fn normal(title: &str, width: u32, height: u32) -> WindowSpec {
        WindowSpec {
            role: Role::Normal,
            flags: window_flags::DECORATED | window_flags::RESIZABLE,
            x: -1,
            y: -1,
            width,
            height,
            min_width: 320,
            min_height: 200,
            parent: 0,
            title: String::from(title),
            alpha: false,
        }
    }

    pub fn with_role(role: Role, x: i32, y: i32, width: u32, height: u32) -> WindowSpec {
        WindowSpec { role, flags: 0, x, y, width, height, min_width: 0, min_height: 0, parent: 0, title: String::new(), alpha: false }
    }
}

pub struct Window {
    /// Local id of the window (and of its surface).
    pub id: u32,
    buffer: u32,
    shm: Option<(SharedMemory, Mapping)>,
    /// The buffer was (re)allocated: the next attach passes its handle.
    fresh_buffer: bool,
    pub width: u32,
    pub height: u32,
    /// `proto::state` bits of the last CONFIGURE.
    pub states: u32,
    configured: bool,
    /// Committed and not yet released by PRISM.
    busy: bool,
    /// The application wants to redraw.
    pub dirty: bool,
    alpha: bool,
}

impl Window {
    /// Applies an event addressed to this window (CONFIGURE, buffer
    /// release). Returns true if the event was about this window.
    pub fn on_event(&mut self, ev: &Event) -> bool {
        match *ev {
            Event::Configure { window, width, height, states } if window == self.id => {
                if (width, height) != (self.width, self.height) || !self.configured {
                    self.dirty = true;
                }
                self.width = width.max(1);
                self.height = height.max(1);
                self.states = states;
                self.configured = true;
                true
            }
            Event::BufferRelease { surface } if surface == self.id => {
                self.busy = false;
                true
            }
            Event::FrameDone { surface, .. } if surface == self.id => true,
            Event::FocusChanged { window, .. }
            | Event::Key { window, .. }
            | Event::Pointer { window, .. }
            | Event::CloseRequest { window } => window == self.id,
            _ => false,
        }
    }

    pub fn is_focused(&self) -> bool {
        self.states & proto::state::ACTIVATED != 0
    }

    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// The buffer may be drawn into (configured and not held by PRISM).
    pub fn can_draw(&self) -> bool {
        self.configured && !self.busy
    }

    fn ensure_buffer(&mut self) -> Result<()> {
        let need = self.width as u64 * self.height as u64 * 4;
        if self.shm.as_ref().is_some_and(|(s, _)| s.size() >= need) {
            return Ok(());
        }
        // Grow with headroom so interactive resizing does not reallocate
        // on every step.
        let size = (need + need / 4).next_multiple_of(4096).min(MAX_BUFFER);
        let shm = SharedMemory::create(size)?;
        let map = shm.map(true)?;
        self.shm = Some((shm, map));
        self.fresh_buffer = true;
        Ok(())
    }

    /// The drawing surface (`width` x `height`), allocating the buffer if needed.
    pub fn surface(&mut self) -> Result<Surface<'_>> {
        self.ensure_buffer()?;
        let (w, h) = (self.width, self.height);
        let (_, map) = self.shm.as_mut().expect("buffer allocated");
        let words = &mut map.words_mut()[..(w * h) as usize];
        Surface::new(words, w, h, w).map_err(|_| Error::from(ErrorKind::InvalidInput))
    }

    /// Hands the buffer to PRISM: attach, damage (`None` = everything), commit.
    pub fn present(&mut self, conn: &mut Connection, damage: Option<Rect>) -> Result<()> {
        self.ensure_buffer()?;
        let (w, h) = (self.width, self.height);
        let format = if self.alpha { proto::format::ARGB8888 } else { proto::format::XRGB8888 };
        let attach = Request::AttachBuffer { surface: self.id, buffer: self.buffer, width: w, height: h, stride: w * 4, format, offset: 0 };
        if self.fresh_buffer {
            let handle = self.shm.as_ref().map(|(s, _)| s.handle()).expect("buffer allocated");
            conn.send(&attach, &[handle])?;
            self.fresh_buffer = false;
        } else {
            conn.send(&attach, &[])?;
        }
        let rect = damage.unwrap_or(Rect { x: 0, y: 0, w, h });
        conn.send(&Request::Damage { surface: self.id, rect }, &[])?;
        conn.send(&Request::Commit { surface: self.id }, &[])?;
        self.busy = true;
        self.dirty = false;
        Ok(())
    }

    pub fn set_title(&mut self, conn: &mut Connection, title: &str) -> Result<()> {
        conn.send(&Request::SetTitle { window: self.id, title: String::from(title) }, &[])
    }

    pub fn set_geometry(&mut self, conn: &mut Connection, x: i32, y: i32, w: u32, h: u32) -> Result<()> {
        conn.send(&Request::SetGeometry { window: self.id, x, y, width: w, height: h }, &[])
    }

    pub fn action(&mut self, conn: &mut Connection, action: Action) -> Result<()> {
        conn.send(&Request::WindowAction { target: self.id, action, global: false }, &[])
    }

    /// Destroys the window and its surface and buffer.
    pub fn destroy(self, conn: &mut Connection) -> Result<()> {
        conn.send(&Request::DestroyWindow { window: self.id }, &[])?;
        conn.send(&Request::DestroySurface { surface: self.id }, &[])?;
        if self.shm.is_some() && !self.fresh_buffer {
            conn.send(&Request::DestroyBuffer { buffer: self.buffer }, &[])?;
        }
        Ok(())
    }
}

