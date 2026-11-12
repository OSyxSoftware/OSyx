//! Serve one explicitly selected file over a short-lived LAN HTTP session.
use crate::common::{run_app, App, Ctx};
use os_std::error::{Error, ErrorKind, Result};
use os_std::io::{Read, Seek, SeekFrom, Write};
use os_std::net::{self, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicU32, Ordering};
use os_std::time::{Duration, Instant};
use os_std::{fs::File, thread};
use osyx_abi::net::{IFF_LINK, IFF_LOOPBACK};
use osyx_gfx::{rgb, Rect};
use osyx_ui::{Align, Event, FontKind, Painter};
use prism_client::WindowSpec;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

const SESSION: Duration = Duration::from_secs(300);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const ACTIVE: u32 = 0;
const SENT: u32 = 1;
const EXPIRED: u32 = 2;
const FAILED: u32 = 3;

pub fn run(path: &str) -> i32 {
    let state = Arc::new(AtomicU32::new(ACTIVE));
    let mut app = ShareWindow {
        state: state.clone(),
        shown: ACTIVE,
        url: String::new(),
        error: String::new(),
        modules: Vec::new(),
        side: 0,
    };
    match prepare(path, state) {
        Ok((url, side, modules)) => {
            app.url = url;
            app.side = side;
            app.modules = modules;
        }
        Err(error) => app.error = format!("Cannot share file: {error}"),
    }
    run_app("files-share", WindowSpec::normal("Send to phone", 560, 420), &mut app)
}

fn prepare(path: &str, state: Arc<AtomicU32>) -> Result<(String, i32, Vec<bool>)> {
    if !os_std::fs::metadata(path)?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "select a regular file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "select a regular file"));
    }
    let iface = net::interfaces()?
        .into_iter()
        .filter(|i| i.flags & (IFF_LINK | IFF_LOOPBACK) == IFF_LINK)
        .filter(|i| i.addr != [0; 4] && (1..=32).contains(&i.prefix))
        .max_by_key(|i| (i.gateway != [0; 4], core::cmp::Reverse(i.index)))
        .ok_or_else(|| Error::new(ErrorKind::NotConnected, "connect to an IPv4 network"))?;
    let listener = TcpListener::bind(&SocketAddr::new(Ipv4Addr(iface.addr), 0))?;
    listener.set_accept_timeout(Some(IO_TIMEOUT))?;
    let mut secret = [0u8; 16];
    File::open("/dev/random")?.read_exact(&mut secret)?;
    let mut token = String::from("/");
    for byte in secret {
        use core::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    let url = format!("http://{}{}", listener.local_addr()?, token);
    let mut temp = vec![0; Version::MAX.buffer_len()];
    let mut output = vec![0; Version::MAX.buffer_len()];
    let qr =
        QrCode::encode_text(&url, &mut temp, &mut output, QrCodeEcc::Medium, Version::MIN, Version::MAX, None, true)
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "link is too long for QR"))?;
    let side = qr.size();
    let mut modules = Vec::with_capacity((side * side) as usize);
    for y in 0..side {
        for x in 0..side {
            modules.push(qr.get_module(x, y));
        }
    }
    let name = path.rsplit('/').next().unwrap_or("download");
    let mut encoded_name = String::new();
    for byte in name.bytes() {
        use core::fmt::Write as _;
        let _ = write!(encoded_name, "%{byte:02X}");
    }
    let subnet = u32::from_be_bytes(iface.addr);
    let mask = u32::MAX << (32 - iface.prefix);
    thread::spawn(move || {
        let result = serve(listener, file, &token, &encoded_name, subnet, mask);
        state.store(result, Ordering::Release);
    });
    Ok((url, side, modules))
}

fn serve(listener: TcpListener, mut file: File, token: &str, name: &str, subnet: u32, mask: u32) -> u32 {
    let started = Instant::now();
    while started.elapsed() < SESSION {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if u32::from_be_bytes(peer.ip.octets()) & mask != subnet & mask {
                    continue;
                }
                if matches!(download(&mut stream, &mut file, token, name, started), Ok(true)) {
                    return SENT;
                }
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(_) => return FAILED,
        }
    }
    EXPIRED
}

fn download(stream: &mut TcpStream, file: &mut File, token: &str, name: &str, session_start: Instant) -> Result<bool> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut header = [0u8; 4096];
    let mut used = 0;
    let started = Instant::now();
    while used < header.len() && started.elapsed() < IO_TIMEOUT && session_start.elapsed() < SESSION {
        let n = stream.read(&mut header[used..])?;
        if n == 0 {
            return Ok(false);
        }
        used += n;
        if header[..used].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let request = core::str::from_utf8(&header[..used]).ok();
    let method = request.filter(|s| s.contains("\r\n\r\n")).and_then(|s| s.split("\r\n").next()).and_then(|line| {
        let mut fields = line.split(' ');
        let method = fields.next()?;
        if fields.next() != Some(token)
            || !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1"))
            || fields.next().is_some()
        {
            return None;
        }
        Some(method)
    });
    if !matches!(method, Some("GET" | "HEAD")) {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(false);
    }
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"download\"; filename*=UTF-8''{name}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n")?;
    if method == Some("HEAD") {
        return Ok(false);
    }
    let mut left = length;
    let mut buf = [0u8; 8192];
    while left != 0 && session_start.elapsed() < SESSION {
        let limit = left.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..limit])?;
        if n == 0 {
            return Ok(false);
        }
        let mut sent = 0;
        while sent < n {
            if session_start.elapsed() >= SESSION {
                return Ok(false);
            }
            let written = stream.write(&buf[sent..n])?;
            if written == 0 {
                return Ok(false);
            }
            sent += written;
        }
        left -= n as u64;
    }
    Ok(left == 0)
}

struct ShareWindow {
    state: Arc<AtomicU32>,
    shown: u32,
    url: String,
    error: String,
    modules: Vec<bool>,
    side: i32,
}

impl App for ShareWindow {
    fn layout(&mut self, _: u32, _: u32, _: &mut Ctx) {}
    fn event(&mut self, _: Event, _: &mut Ctx) -> bool {
        false
    }
    fn tick_ms(&self) -> Option<u64> {
        Some(250)
    }
    fn tick(&mut self, _: &mut Ctx) -> bool {
        let next = self.state.load(Ordering::Acquire);
        let changed = next != self.shown;
        self.shown = next;
        changed
    }
    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, _: &Ctx) {
        let white = rgb(255, 255, 255);
        let black = rgb(16, 22, 30);
        p.fill(Rect::new(0, 0, w, h), white);
        let message = if !self.error.is_empty() {
            &self.error
        } else {
            match self.shown {
                SENT => "File sent",
                EXPIRED => "Sharing expired",
                FAILED => "File sharing stopped",
                _ => "Scan with a phone on the same network",
            }
        };
        p.text_in(FontKind::Ui, Rect::new(0, 12, w, 26), message, black, Align::Center);
        if self.side == 0 || self.shown != ACTIVE {
            return;
        }
        let scale = ((w.min(h.saturating_sub(112)) as i32 - 16) / (self.side + 8)).max(1);
        let width = self.side * scale;
        let x = (w as i32 - width) / 2;
        let y = 44 + 4 * scale;
        for row in 0..self.side {
            for col in 0..self.side {
                if self.modules[(row * self.side + col) as usize] {
                    p.fill(Rect::new(x + col * scale, y + row * scale, scale as u32, scale as u32), black);
                }
            }
        }
        let text_y = y + width + 4 * scale;
        p.text_in(FontKind::Ui, Rect::new(0, text_y, w, 23), &self.url, black, Align::Center);
        p.text_in(
            FontKind::Ui,
            Rect::new(0, text_y + 24, w, 23),
            "One download · Five minutes · Close window to stop",
            black,
            Align::Center,
        );
    }
}
