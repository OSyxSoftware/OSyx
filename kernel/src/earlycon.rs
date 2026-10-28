//! Early framebuffer console: every kernel log line, mirrored onto the GOP
//! framebuffer as a scrolling text log from the first line on.
//!
//! On real hardware nothing but this shows where boot stops (COM1 is rarely
//! wired up), so it needs nothing the kernel sets up later: no heap, no
//! kernel page tables, no timer. The framebuffer is reached through the boot
//! loader's HHDM, a text grid and one line of pixels live in static memory,
//! and each line is rendered into that pixel row and copied out. When the
//! screen is full the oldest third of the rows is dropped and the rest is
//! redrawn (copying within the framebuffer would read video memory, which is
//! very slow on real cards).
//!
//! The log stays until `display::init` draws the status screen, or with
//! `log.screen` until a user-space process takes the display.

use crate::sync::IrqSpinLock;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use osyx_bootinfo::{FramebufferInfo, PixelFormat};
use osyx_gfx::splash;
use osyx_gfx::{Color, Font, Rect, Surface};

static MONO16: &[u8] = include_bytes!("../../boot/assets/mono16.osxf");
static MONO22: &[u8] = include_bytes!("../../boot/assets/mono22.osxf");

const MAX_COLS: usize = 240;
const MAX_ROWS: usize = 128;
/// Widest line of pixels the renderer handles (wider screens use the left part).
const MAX_WIDTH: usize = 3840;
/// Tallest font cell (mono22 is 13x28).
const MAX_CELL_H: usize = 32;
const MARGIN: u32 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    Title,
    Alert,
}

struct EarlyCon {
    fb: *mut u32,
    stride: u32,
    width: u32,
    height: u32,
    rgb: bool,
    font: Option<Font<'static>>,
    cols: usize,
    rows: usize,
    /// Rows in use (`text[..used]`).
    used: usize,
    col: usize,
    /// A line is being written into row `used - 1`.
    open: bool,
    text: [[u8; MAX_COLS]; MAX_ROWS],
    len: [u8; MAX_ROWS],
    kind: [Kind; MAX_ROWS],
    /// The row continues the line of the row above (wrapped).
    cont: [bool; MAX_ROWS],
    /// One rendered text row (`width` x cell height, packed).
    line: [u32; MAX_WIDTH * MAX_CELL_H],
}

// SAFETY: the framebuffer pointer is only dereferenced under the CON lock.
unsafe impl Send for EarlyCon {}

static CON: IrqSpinLock<EarlyCon> = IrqSpinLock::new(EarlyCon {
    fb: core::ptr::null_mut(),
    stride: 0,
    width: 0,
    height: 0,
    rgb: false,
    font: None,
    cols: 0,
    rows: 0,
    used: 0,
    col: 0,
    open: false,
    text: [[0; MAX_COLS]; MAX_ROWS],
    len: [0; MAX_ROWS],
    kind: [Kind::Normal; MAX_ROWS],
    cont: [false; MAX_ROWS],
    line: [0; MAX_WIDTH * MAX_CELL_H],
});
static ACTIVE: AtomicBool = AtomicBool::new(false);

const BACKGROUND: Color = splash::BG_BOTTOM;

/// Leaves a small hardware-boot checkpoint without depending on the console.
pub fn marker(info: &FramebufferInfo, hhdm_offset: u64, hhdm_size: u64, stage: u8) {
    let rgb = match info.format {
        PixelFormat::Rgb => true,
        PixelFormat::Bgr => false,
        _ => return,
    };
    let needed = info.stride as u64 * info.height as u64 * 4;
    if info.base == 0 || info.width < 80 || info.height < 32 || info.stride < info.width
        || info.base.checked_add(needed).is_none_or(|end| end > hhdm_size)
    {
        return;
    }
    let (left, color) = match stage {
        1 => (0, 0x0020E040),
        2 if active() => (0, 0x002080FF),
        2 => (0, 0x00FFE040),
        3 => (40, 0x0020E0E0),
        4 => (80, 0x00E040E0),
        5 => (120, 0x00FF8040),
        6 => (160, 0x00FFFFFF),
        _ => return,
    };
    let px = osyx_gfx::to_device(color, rgb);
    let fb = (hhdm_offset + info.base) as *mut u32;
    for y in 0..32u32 {
        for x in 0..32u32 {
            unsafe { fb.add((y * info.stride + left + x) as usize).write_volatile(px) };
        }
    }
}

/// Starts the console on the boot framebuffer. `hhdm_offset`/`hhdm_size`
/// describe the boot loader's direct map, which is still the active one.
/// Does nothing if there is no usable linear framebuffer.
pub fn init(info: &FramebufferInfo, hhdm_offset: u64, hhdm_size: u64) {
    let rgb = match info.format {
        PixelFormat::Rgb => true,
        PixelFormat::Bgr => false,
        _ => return,
    };
    let needed = info.stride as u64 * info.height as u64 * 4;
    if info.base == 0 || info.width < 160 || info.height < 100 || info.stride < info.width || info.size < needed {
        return;
    }
    if info.base.checked_add(needed).is_none_or(|end| end > hhdm_size) {
        return;
    }
    let font = if info.height >= 1200 { Font::parse(MONO22) } else { Font::parse(MONO16) };
    let Ok(font) = font else { return };
    if font.line_height() as usize > MAX_CELL_H {
        return;
    }
    let width = info.width.min(MAX_WIDTH as u32);
    let mut c = CON.lock();
    c.fb = (hhdm_offset + info.base) as *mut u32;
    c.stride = info.stride;
    c.width = width;
    c.height = info.height;
    c.rgb = rgb;
    c.cols = (((width - 2 * MARGIN) / font.cell_width()) as usize).min(MAX_COLS);
    c.rows = (((info.height - 2 * MARGIN) / font.line_height()) as usize).min(MAX_ROWS);
    c.font = Some(font);
    c.used = 0;
    c.col = 0;
    c.open = false;
    c.clear_screen();
    let _ = fmt::Write::write_fmt(
        &mut *c,
        format_args!("OSyx kernel {} - early boot log ({}x{})", crate::VERSION, info.width, info.height),
    );
    c.finish_line(Kind::Title);
    drop(c);
    ACTIVE.store(true, Ordering::Release);
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Stops mirroring the log (the status screen or a compositor draws now).
pub fn stop() {
    ACTIVE.store(false, Ordering::Release);
}

/// Rows and columns of the text grid, if the console is running.
pub fn geometry() -> Option<(usize, usize)> {
    if !active() {
        return None;
    }
    let c = CON.try_lock()?;
    Some((c.cols, c.rows))
}

/// Mirrors one log line. Never blocks: a line that arrives while the console
/// is busy (only possible in the panic path) is dropped from the screen.
pub fn print(ms: u64, args: fmt::Arguments) {
    if !active() {
        return;
    }
    let Some(mut c) = CON.try_lock() else { return };
    let _ = fmt::Write::write_fmt(&mut *c, format_args!("[{:5}.{:03}] ", ms / 1000, ms % 1000));
    let _ = fmt::Write::write_fmt(&mut *c, args);
    let kind = c.classify();
    c.finish_line(kind);
}

impl EarlyCon {
    fn clear_screen(&mut self) {
        let px = osyx_gfx::to_device(BACKGROUND, self.rgb);
        for y in 0..self.height {
            // SAFETY: `fb` maps stride * height pixels (checked in `init`).
            unsafe {
                let out = self.fb.add(y as usize * self.stride as usize);
                for x in 0..self.width as usize {
                    out.add(x).write_volatile(px);
                }
            }
        }
    }

    /// Opens the next row, scrolling if the screen is full. `cont` marks the
    /// continuation of a wrapped line.
    fn new_row(&mut self, cont: bool) {
        if self.used == self.rows {
            // Keep the title row, drop the oldest third of the log.
            let drop = (self.rows / 3).max(1);
            for r in 1..self.rows - drop {
                self.text[r] = self.text[r + drop];
                self.len[r] = self.len[r + drop];
                self.kind[r] = self.kind[r + drop];
                self.cont[r] = self.cont[r + drop];
            }
            self.used = self.rows - drop;
            for r in 1..self.rows {
                if r >= self.used {
                    self.len[r] = 0;
                }
                self.draw_row(r);
            }
        }
        let r = self.used;
        self.len[r] = 0;
        self.kind[r] = Kind::Normal;
        self.cont[r] = cont;
        self.used += 1;
        self.col = 0;
        if cont {
            for _ in 0..4.min(self.cols / 2) {
                self.put_raw(b' ');
            }
        }
    }

    fn put(&mut self, b: u8) {
        if self.rows == 0 {
            return;
        }
        if !self.open {
            self.new_row(false);
            self.open = true;
        } else if self.col >= self.cols {
            // Wrapped line: draw the full row now, continue indented below.
            let r = self.used - 1;
            self.draw_row(r);
            self.new_row(true);
        }
        self.put_raw(b);
    }

    fn put_raw(&mut self, b: u8) {
        let r = self.used - 1;
        let n = self.len[r] as usize;
        if n < MAX_COLS {
            self.text[r][n] = b;
            self.len[r] = n as u8 + 1;
        }
        self.col += 1;
    }

    /// Highlights failures so they stand out in a photo of the screen.
    fn classify(&self) -> Kind {
        if self.used == 0 {
            return Kind::Normal;
        }
        const ALERTS: [&[u8]; 6] = [b"FATAL", b"PANIC", b"EXCEPTION", b"WARNING", b"failed", b"FAILED"];
        let mut r = self.used - 1;
        loop {
            let s = &self.text[r][..self.len[r] as usize];
            if ALERTS.iter().any(|a| s.windows(a.len()).any(|w| w == *a)) {
                return Kind::Alert;
            }
            if r <= 1 || !self.cont[r] {
                return Kind::Normal;
            }
            r -= 1;
        }
    }

    /// Ends the current line: colours its rows and draws the last one.
    fn finish_line(&mut self, kind: Kind) {
        if !self.open {
            return;
        }
        self.open = false;
        let r = self.used - 1;
        self.kind[r] = kind;
        if kind != Kind::Normal {
            // Earlier rows of a wrapped line get the same colour.
            let mut p = r;
            while p > 1 && self.cont[p] {
                p -= 1;
                self.kind[p] = kind;
                self.draw_row(p);
            }
        }
        self.draw_row(r);
    }

    fn draw_row(&mut self, r: usize) {
        let Some(font) = self.font else { return };
        let (w, h) = (self.width, font.line_height());
        let color = match self.kind[r] {
            Kind::Normal => splash::TEXT,
            Kind::Title => splash::ACCENT,
            Kind::Alert => splash::ERROR,
        };
        let n = self.len[r] as usize;
        let mut chars = [0u8; MAX_COLS];
        chars[..n].copy_from_slice(&self.text[r][..n]);
        let text = core::str::from_utf8(&chars[..n]).unwrap_or("?");
        let (fb, stride, rgb) = (self.fb, self.stride, self.rgb);
        let pixels = &mut self.line[..(w * h) as usize];
        let Ok(mut s) = Surface::new(pixels, w, h, w) else { return };
        s.fill(BACKGROUND);
        font.draw(&mut s, MARGIN as i32, 0, text, color, 255);
        let y = MARGIN + r as u32 * h;
        // SAFETY: rows are laid out inside the framebuffer (`rows` was derived
        // from its height), which `fb` maps with `stride` pixels per line.
        unsafe { osyx_gfx::present(&s, Rect::new(0, 0, w, h), fb.add(y as usize * stride as usize), stride, rgb) };
    }
}

impl fmt::Write for EarlyCon {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            match ch {
                '\n' => self.col = self.cols,
                '\r' => {}
                c if c.is_ascii() && !c.is_ascii_control() => self.put(c as u8),
                _ => self.put(b'?'),
            }
        }
        Ok(())
    }
}
