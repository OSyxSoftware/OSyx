//! user32.dll and gdi32.dll on PRISM.
//!
//! Every top-level Win32 window is a PRISM window: PRISM draws the frame
//! and title bar, the window's client area is the PRISM surface, so the
//! client size is the PRISM content size (AdjustWindowRect is therefore an
//! identity). Messages come from PRISM events (configure → WM_SIZE, close
//! button → WM_CLOSE, keyboard → WM_KEYDOWN/UP and WM_CHAR, pointer →
//! WM_MOUSEMOVE / WM_xBUTTONDOWN/UP / WM_MOUSEWHEEL) plus the program's own
//! posted messages, timers and WM_PAINT for invalid areas. GDI draws with
//! OSyx's software renderer into the surface; EndPaint/ReleaseDC present
//! the damaged rectangle to PRISM. Child windows (controls) are not
//! implemented: CreateWindowEx with WS_CHILD fails with ERROR_NOT_SUPPORTED.
//!
//! Window procedures are never called while the GUI lock is held.

#![allow(non_snake_case)]

use crate::error::*;
use crate::kernel32::{fail, out_a, out_w, Bool, FALSE, TRUE};
use crate::{path, teb};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;
use os_std::sync::Mutex;
use os_std::time::Instant;
use osyx_gfx::prop::PropFont;
use osyx_gfx::text::Font as MonoFont;
use osyx_gfx::{Color, Rect, Surface};
use prism_client::{proto, Connection, Event, Window, WindowSpec};

static SANS14: &[u8] = include_bytes!("../../../graphics/assets/sans14.osxp");
static SANS14B: &[u8] = include_bytes!("../../../graphics/assets/sans14b.osxp");
static SANS20: &[u8] = include_bytes!("../../../graphics/assets/sans20.osxp");
static MONO15: &[u8] = include_bytes!("../../../graphics/assets/mono15.osxf");

// ---- messages ------------------------------------------------------------------------

pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_ACTIVATE: u32 = 0x0006;
pub const WM_SETFOCUS: u32 = 0x0007;
pub const WM_KILLFOCUS: u32 = 0x0008;
pub const WM_SETTEXT: u32 = 0x000C;
pub const WM_GETTEXTLENGTH: u32 = 0x000E;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_SHOWWINDOW: u32 = 0x0018;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCDESTROY: u32 = 0x0082;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MOUSEWHEEL: u32 = 0x020A;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;

type WndProc = extern "win64" fn(u64, u32, u64, i64) -> i64;
type TimerProc = extern "win64" fn(u64, u32, u64, u32);

#[derive(Clone, Copy)]
struct Msg {
    hwnd: u64,
    msg: u32,
    wparam: u64,
    lparam: i64,
    time: u32,
    pt: (i32, i32),
}

struct Class {
    name: String,
    wndproc: u64,
    background: u64,
    style: u32,
}

struct Win {
    class: usize,
    wndproc: u64,
    title: String,
    style: u32,
    win: Window,
    pos: (i32, i32),
    size: (u32, u32),
    /// Last size reported with WM_SIZE.
    reported: Option<(u32, u32)>,
    visible: bool,
    invalid: Option<Rect>,
    erase: bool,
    user_data: i64,
    /// Rectangle drawn since the last present (BeginPaint/GetDC).
    drawn: Option<Rect>,
}

struct Timer {
    hwnd: u64,
    id: u64,
    interval: u64,
    due: Instant,
    proc_: u64,
}

#[derive(Clone, Copy)]
enum FontKind {
    Sans,
    SansBold,
    Large,
    Mono,
}

#[derive(Clone, Copy)]
enum Gdi {
    Brush(Color),
    NullBrush,
    Pen(Color, u32),
    NullPen,
    Font(FontKind),
}

struct Dc {
    hwnd: u64,
    paint: Rect,
    text: Color,
    bk: Color,
    opaque: bool,
    brush: u64,
    pen: u64,
    font: u64,
    pos: (i32, i32),
}

struct Gui {
    conn: Option<Connection>,
    classes: Vec<Class>,
    windows: BTreeMap<u64, Win>,
    /// PRISM window id → HWND.
    by_prism: BTreeMap<u32, u64>,
    queue: VecDeque<Msg>,
    timers: Vec<Timer>,
    quit: Option<i32>,
    focus: u64,
    /// Characters of key presses waiting for TranslateMessage (vk, char).
    chars: VecDeque<(u32, char)>,
    gdi: BTreeMap<u64, Gdi>,
    dcs: BTreeMap<u64, Dc>,
    next: u64,
    keys: [bool; 256],
    pointer: (i32, i32),
    last_click: (u64, u32, Instant),
    start: Instant,
}

static GUI: Mutex<Option<Gui>> = Mutex::new(None);

// Stock objects: fixed handle values.
const STOCK_BASE: u64 = 0x9000;
const WHITE_BRUSH: u32 = 0;
const LTGRAY_BRUSH: u32 = 1;
const GRAY_BRUSH: u32 = 2;
const DKGRAY_BRUSH: u32 = 3;
const BLACK_BRUSH: u32 = 4;
const NULL_BRUSH: u32 = 5;
const WHITE_PEN: u32 = 6;
const BLACK_PEN: u32 = 7;
const NULL_PEN: u32 = 8;
const ANSI_FIXED_FONT: u32 = 11;
const DEFAULT_GUI_FONT: u32 = 17;
const SYSTEM_FONT: u32 = 13;

fn with<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
    let mut g = GUI.lock().unwrap();
    let gui = g.get_or_insert_with(|| {
        let mut gdi = BTreeMap::new();
        for (i, o) in [
            (WHITE_BRUSH, Gdi::Brush(rgb(255, 255, 255))),
            (LTGRAY_BRUSH, Gdi::Brush(rgb(192, 192, 192))),
            (GRAY_BRUSH, Gdi::Brush(rgb(128, 128, 128))),
            (DKGRAY_BRUSH, Gdi::Brush(rgb(64, 64, 64))),
            (BLACK_BRUSH, Gdi::Brush(rgb(0, 0, 0))),
            (NULL_BRUSH, Gdi::NullBrush),
            (WHITE_PEN, Gdi::Pen(rgb(255, 255, 255), 1)),
            (BLACK_PEN, Gdi::Pen(rgb(0, 0, 0), 1)),
            (NULL_PEN, Gdi::NullPen),
            (ANSI_FIXED_FONT, Gdi::Font(FontKind::Mono)),
            (12, Gdi::Font(FontKind::Sans)),
            (SYSTEM_FONT, Gdi::Font(FontKind::Sans)),
            (14, Gdi::Font(FontKind::Sans)),
            (15, Gdi::Font(FontKind::Sans)),
            (16, Gdi::Font(FontKind::Mono)),
            (DEFAULT_GUI_FONT, Gdi::Font(FontKind::Sans)),
        ] {
            gdi.insert(STOCK_BASE + i as u64, o);
        }
        Gui {
            conn: None,
            classes: Vec::new(),
            windows: BTreeMap::new(),
            by_prism: BTreeMap::new(),
            queue: VecDeque::new(),
            timers: Vec::new(),
            quit: None,
            focus: 0,
            chars: VecDeque::new(),
            gdi,
            dcs: BTreeMap::new(),
            next: 0x2_0000,
            keys: [false; 256],
            pointer: (0, 0),
            last_click: (0, 0, Instant::now()),
            start: Instant::now(),
        }
    });
    f(gui)
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    osyx_gfx::rgb(r, g, b)
}

/// COLORREF (0x00BBGGRR) → OSyx color.
fn colorref(c: u32) -> Color {
    rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8)
}

fn to_colorref(c: Color) -> u32 {
    let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
    r | g << 8 | b << 16
}

/// GetSysColor: a light Windows-like palette.
fn sys_color(i: u32) -> Color {
    match i {
        5 => rgb(255, 255, 255),  // COLOR_WINDOW
        8 => rgb(0, 0, 0),        // COLOR_WINDOWTEXT
        15 => rgb(240, 240, 240), // COLOR_BTNFACE
        16 => rgb(160, 160, 160), // COLOR_BTNSHADOW
        18 => rgb(0, 0, 0),       // COLOR_BTNTEXT
        13 => rgb(0, 120, 215),   // COLOR_HIGHLIGHT
        14 => rgb(255, 255, 255), // COLOR_HIGHLIGHTTEXT
        17 => rgb(109, 109, 109), // COLOR_GRAYTEXT
        1 => rgb(0, 0, 0),        // COLOR_BACKGROUND
        _ => rgb(240, 240, 240),
    }
}

impl Gui {
    fn handle(&mut self) -> u64 {
        self.next += 4;
        self.next
    }

    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn conn(&mut self) -> Result<&mut Connection, u32> {
        if self.conn.is_none() {
            let name = alloc::format!(
                "win32:{}",
                crate::kernel32::with_process(|p| p.exe_path.rsplit('/').next().unwrap_or("app").to_string())
            );
            match Connection::connect(&name, false) {
                Ok(c) => self.conn = Some(c),
                Err(e) => {
                    os_std::eprintln!("winrun: cannot reach PRISM (no desktop session?): {e}");
                    return Err(ERROR_NOT_SUPPORTED);
                }
            }
        }
        Ok(self.conn.as_mut().expect("connected"))
    }

    fn post(&mut self, hwnd: u64, msg: u32, wparam: u64, lparam: i64) {
        let time = self.now_ms();
        let pt = self.pointer;
        self.queue.push_back(Msg { hwnd, msg, wparam, lparam, time, pt });
    }

    fn invalidate(&mut self, hwnd: u64, r: Option<Rect>, erase: bool) {
        if let Some(w) = self.windows.get_mut(&hwnd) {
            let full = Rect::new(0, 0, w.size.0, w.size.1);
            let r = r.unwrap_or(full).intersect(&full);
            w.invalid = Some(match w.invalid {
                Some(o) => o.union(&r),
                None => r,
            });
            w.erase |= erase;
        }
    }

    /// Applies one PRISM event: window bookkeeping and queued messages.
    fn on_event(&mut self, ev: Event) {
        let target = match &ev {
            Event::Configure { window, .. }
            | Event::FocusChanged { window, .. }
            | Event::Key { window, .. }
            | Event::Pointer { window, .. }
            | Event::CloseRequest { window } => self.by_prism.get(window).copied(),
            Event::BufferRelease { surface } | Event::FrameDone { surface, .. } => self.by_prism.get(surface).copied(),
            _ => None,
        };
        let Some(hwnd) = target else { return };
        if let Some(w) = self.windows.get_mut(&hwnd) {
            w.win.on_event(&ev);
        }
        match ev {
            Event::Configure { width, height, .. } => {
                let Some(w) = self.windows.get_mut(&hwnd) else { return };
                w.size = (width, height);
                if w.reported != Some((width, height)) {
                    w.reported = Some((width, height));
                    self.post(hwnd, WM_SIZE, 0, (width as i64 & 0xFFFF) | ((height as i64 & 0xFFFF) << 16));
                    self.invalidate(hwnd, None, true);
                }
            }
            Event::FocusChanged { focused, .. } => {
                if focused {
                    self.focus = hwnd;
                    self.post(hwnd, WM_SETFOCUS, 0, 0);
                    self.post(hwnd, WM_ACTIVATE, 1, 0);
                } else {
                    self.post(hwnd, WM_KILLFOCUS, 0, 0);
                    self.post(hwnd, WM_ACTIVATE, 0, 0);
                }
            }
            Event::CloseRequest { .. } => self.post(hwnd, WM_CLOSE, 0, 0),
            Event::Key { usage, state, modifiers, text, .. } => {
                let vk = vk_of(usage);
                if vk == 0 {
                    return;
                }
                let down = state != proto::key_state::UP;
                self.keys[vk as usize] = down;
                let alt = modifiers & osyx_input::modifier::ALT != 0;
                let repeat = state == proto::key_state::REPEAT;
                let lparam = 1
                    | ((usage as i64 & 0xFF) << 16)
                    | if repeat { 1 << 30 } else { 0 }
                    | if down { 0 } else { (1 << 30) | (1 << 31) };
                let msg = match (down, alt) {
                    (true, false) => WM_KEYDOWN,
                    (false, false) => WM_KEYUP,
                    (true, true) => WM_SYSKEYDOWN,
                    (false, true) => WM_SYSKEYUP,
                };
                self.post(hwnd, msg, vk as u64, lparam);
                if down {
                    let ch = match vk {
                        0x08 => Some('\u{8}'),
                        0x09 => Some('\t'),
                        0x0D => Some('\r'),
                        0x1B => Some('\u{1b}'),
                        _ => text.filter(|_| modifiers & osyx_input::modifier::CTRL == 0),
                    };
                    if let Some(c) = ch {
                        self.chars.push_back((vk, c));
                    }
                }
            }
            Event::Pointer { kind, x, y, button, pressed, wheel_v, .. } => {
                self.pointer = (x, y);
                let lp = (x as i64 & 0xFFFF) | ((y as i64 & 0xFFFF) << 16);
                let mk = self.mouse_keys();
                match kind {
                    proto::pointer::MOTION => self.post(hwnd, WM_MOUSEMOVE, mk, lp),
                    proto::pointer::BUTTON => {
                        let (vk, down_msg, up_msg) = match button {
                            osyx_input::button::LEFT => (0x01, WM_LBUTTONDOWN, WM_LBUTTONUP),
                            osyx_input::button::RIGHT => (0x02, WM_RBUTTONDOWN, WM_RBUTTONUP),
                            osyx_input::button::MIDDLE => (0x04, WM_MBUTTONDOWN, WM_MBUTTONUP),
                            _ => return,
                        };
                        self.keys[vk] = pressed;
                        let mk = self.mouse_keys();
                        if pressed {
                            let dbl = down_msg == WM_LBUTTONDOWN
                                && self.last_click.0 == hwnd
                                && self.last_click.2.elapsed() < Duration::from_millis(500);
                            self.last_click = (hwnd, down_msg, Instant::now());
                            let class_dbl = self
                                .windows
                                .get(&hwnd)
                                .and_then(|w| self.classes.get(w.class))
                                .is_some_and(|c| c.style & 0x8 != 0); // CS_DBLCLKS
                            self.post(hwnd, if dbl && class_dbl { WM_LBUTTONDBLCLK } else { down_msg }, mk, lp);
                        } else {
                            self.post(hwnd, up_msg, mk, lp);
                        }
                    }
                    proto::pointer::WHEEL if wheel_v != 0 => {
                        // Windows reports 120 per detent, positive away from the user.
                        let delta = (wheel_v * 120) as i16 as u16 as u64;
                        self.post(hwnd, WM_MOUSEWHEEL, mk | delta << 16, lp);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn mouse_keys(&self) -> u64 {
        let mut m = 0;
        if self.keys[0x01] {
            m |= 0x1;
        }
        if self.keys[0x02] {
            m |= 0x2;
        }
        if self.keys[0x10] {
            m |= 0x4;
        }
        if self.keys[0x11] {
            m |= 0x8;
        }
        if self.keys[0x04] {
            m |= 0x10;
        }
        m
    }

    /// Reads every pending PRISM event.
    fn pump(&mut self) {
        let mut evs = Vec::new();
        if let Some(c) = self.conn.as_mut() {
            while let Ok(Some(ev)) = c.poll() {
                evs.push(ev);
            }
        }
        for ev in evs {
            self.on_event(ev);
        }
    }

    /// Waits up to `timeout` for PRISM events, then reads them.
    fn wait(&mut self, timeout: Duration) {
        if let Some(c) = self.conn.as_mut() {
            let _ = c.wait(&mut [], Some(timeout));
        } else {
            os_std::thread::sleep(timeout);
        }
        self.pump();
    }

    /// Next message for `get/peek`: queue, quit, timers, paint.
    fn next_message(&mut self, filter: u64) -> Option<Msg> {
        self.pump();
        let matches = |m: &Msg| filter == 0 || m.hwnd == filter;
        if let Some(i) = self.queue.iter().position(matches) {
            return self.queue.remove(i);
        }
        if let Some(code) = self.quit.take() {
            return Some(Msg {
                hwnd: 0,
                msg: WM_QUIT,
                wparam: code as u64,
                lparam: 0,
                time: self.now_ms(),
                pt: self.pointer,
            });
        }
        let now = Instant::now();
        if let Some(t) = self.timers.iter_mut().filter(|t| filter == 0 || t.hwnd == filter).find(|t| t.due <= now) {
            t.due = now + Duration::from_millis(t.interval);
            let m = Msg { hwnd: t.hwnd, msg: WM_TIMER, wparam: t.id, lparam: t.proc_ as i64, time: 0, pt: (0, 0) };
            return Some(Msg { time: self.now_ms(), pt: self.pointer, ..m });
        }
        let paint = self
            .windows
            .iter()
            .find(|(h, w)| (filter == 0 || **h == filter) && w.visible && w.invalid.is_some() && w.win.is_configured())
            .map(|(h, _)| *h);
        paint.map(|h| Msg { hwnd: h, msg: WM_PAINT, wparam: 0, lparam: 0, time: self.now_ms(), pt: self.pointer })
    }

    fn next_timer_in(&self) -> Duration {
        let now = Instant::now();
        self.timers
            .iter()
            .map(|t| if t.due > now { t.due - now } else { Duration::ZERO })
            .min()
            .unwrap_or(Duration::from_millis(500))
            .min(Duration::from_millis(500))
    }
}

/// USB HID usage → Windows virtual-key code (0: none).
fn vk_of(u: u16) -> u32 {
    match u {
        0x04..=0x1D => (b'A' as u32) + (u as u32 - 0x04),
        0x1E..=0x26 => (b'1' as u32) + (u as u32 - 0x1E),
        0x27 => b'0' as u32,
        0x28 | 0x58 => 0x0D,
        0x29 => 0x1B,
        0x2A => 0x08,
        0x2B => 0x09,
        0x2C => 0x20,
        0x2D => 0xBD,
        0x2E => 0xBB,
        0x2F => 0xDB,
        0x30 => 0xDD,
        0x31 => 0xDC,
        0x33 => 0xBA,
        0x34 => 0xDE,
        0x35 => 0xC0,
        0x36 => 0xBC,
        0x37 => 0xBE,
        0x38 => 0xBF,
        0x39 => 0x14,
        0x3A..=0x45 => 0x70 + (u as u32 - 0x3A),
        0x49 => 0x2D,
        0x4A => 0x24,
        0x4B => 0x21,
        0x4C => 0x2E,
        0x4D => 0x23,
        0x4E => 0x22,
        0x4F => 0x27,
        0x50 => 0x25,
        0x51 => 0x28,
        0x52 => 0x26,
        0xE0 | 0xE4 => 0x11,
        0xE1 | 0xE5 => 0x10,
        0xE2 | 0xE6 => 0x12,
        0xE3 | 0xE7 => 0x5B,
        _ => 0,
    }
}

fn call(proc_: u64, hwnd: u64, msg: u32, w: u64, l: i64) -> i64 {
    if proc_ == 0 {
        return DefWindowProcA(hwnd, msg, w, l);
    }
    // SAFETY: a window procedure registered by the program (or ours).
    let f: WndProc = unsafe { core::mem::transmute(proc_ as usize) };
    f(hwnd, msg, w, l)
}

fn wndproc_of(hwnd: u64) -> Option<u64> {
    with(|g| g.windows.get(&hwnd).map(|w| w.wndproc))
}

// ---- classes and windows ------------------------------------------------------------

/// WNDCLASSA/W (72 bytes): style, lpfnWndProc @8, cbClsExtra, cbWndExtra,
/// hInstance @24, hIcon, hCursor, hbrBackground @48, lpszMenuName,
/// lpszClassName @64.
fn register_class(wc: *const u8, wide: bool) -> u16 {
    if wc.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's WNDCLASS.
    let (style, proc_, bg, name_p): (u32, u64, u64, u64) =
        unsafe { (teb::get(wc as u64, 0), teb::get(wc as u64, 8), teb::get(wc as u64, 48), teb::get(wc as u64, 64)) };
    // SAFETY: a NUL-terminated class name.
    let name = unsafe {
        if wide {
            path::wide_to_string(name_p as *const u16)
        } else {
            path::ansi_to_string(name_p as *const u8)
        }
    };
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, 0) };
    with(|g| {
        if g.classes.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
            return fail(ERROR_CLASS_ALREADY_EXISTS, 0);
        }
        g.classes.push(Class { name, wndproc: proc_, background: bg, style });
        0xC000 + g.classes.len() as u16
    })
}

extern "win64" fn RegisterClassA(wc: *const u8) -> u16 {
    register_class(wc, false)
}

extern "win64" fn RegisterClassW(wc: *const u8) -> u16 {
    register_class(wc, true)
}

/// WNDCLASSEX: cbSize, then the WNDCLASS fields shifted by 4 (style at 4,
/// lpfnWndProc at 8 ...): the same offsets for what is read here except
/// style and the extra hIconSm at the end.
extern "win64" fn RegisterClassExA(wc: *const u8) -> u16 {
    register_class_ex(wc, false)
}

extern "win64" fn RegisterClassExW(wc: *const u8) -> u16 {
    register_class_ex(wc, true)
}

fn register_class_ex(wc: *const u8, wide: bool) -> u16 {
    if wc.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // WNDCLASSEX: cbSize@0 style@4 proc@8 cls@16 wnd@20 inst@24 icon@32
    // cursor@40 bg@48 menu@56 name@64 iconsm@72.
    let mut tmp = [0u8; 72];
    // SAFETY: the caller's WNDCLASSEX (80 bytes).
    unsafe { core::ptr::copy_nonoverlapping(wc, tmp.as_mut_ptr(), 72) };
    let style = u32::from_le_bytes([tmp[4], tmp[5], tmp[6], tmp[7]]);
    tmp[0..4].copy_from_slice(&style.to_le_bytes());
    register_class(tmp.as_ptr(), wide)
}

extern "win64" fn UnregisterClassA(_name: *const u8, _inst: u64) -> Bool {
    TRUE
}

fn class_name(p: u64, wide: bool) -> Option<String> {
    if p < 0x10000 {
        // An atom from RegisterClass.
        return with(|g| g.classes.get((p as usize).checked_sub(0xC001)?).map(|c| c.name.clone()));
    }
    // SAFETY: a NUL-terminated class name.
    unsafe {
        if wide {
            path::wide_to_string(p as *const u16)
        } else {
            path::ansi_to_string(p as *const u8)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn create_window(
    ex_style: u32,
    class: Option<String>,
    title: Option<String>,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: u64,
    param: u64,
    class_p: u64,
    title_p: u64,
) -> u64 {
    let Some(class) = class else { return fail(ERROR_CANNOT_FIND_WND_CLASS, 0) };
    if style & WS_CHILD != 0 || parent != 0 && style & WS_CHILD != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0); // controls / child windows
    }
    let title = title.unwrap_or_default();
    let created = with(|g| -> Result<u64, u32> {
        let ci =
            g.classes.iter().position(|c| c.name.eq_ignore_ascii_case(&class)).ok_or(ERROR_CANNOT_FIND_WND_CLASS)?;
        let (cw, ch) = g.conn().map(|c| (c.screen_width, c.screen_height))?;
        let width = if w == CW_USEDEFAULT || w <= 0 { (cw * 2 / 3).max(320) } else { w as u32 };
        let height = if h == CW_USEDEFAULT || h <= 0 { (ch * 2 / 3).max(200) } else { h as u32 };
        let mut spec = WindowSpec::normal(&title, width, height);
        spec.min_width = 120;
        spec.min_height = 80;
        if x != CW_USEDEFAULT {
            spec.x = x;
            spec.y = y;
        }
        let win = g.conn()?.create_window(spec).map_err(|_| ERROR_NOT_ENOUGH_MEMORY)?;
        let hwnd = g.handle();
        g.by_prism.insert(win.id, hwnd);
        let wndproc = g.classes[ci].wndproc;
        g.windows.insert(
            hwnd,
            Win {
                class: ci,
                wndproc,
                title: title.clone(),
                style,
                win,
                pos: (x.max(0), y.max(0)),
                size: (width, height),
                reported: None,
                visible: false,
                invalid: None,
                erase: true,
                user_data: 0,
                drawn: None,
            },
        );
        Ok(hwnd)
    });
    let hwnd = match created {
        Ok(h) => h,
        Err(e) => return fail(e, 0),
    };
    crate::winsta::note_gui();
    let wndproc = wndproc_of(hwnd).unwrap_or(0);
    // CREATESTRUCT (80 bytes) for WM_NCCREATE / WM_CREATE.
    let mut cs = [0u8; 80];
    let put = |cs: &mut [u8; 80], off: usize, v: &[u8]| cs[off..off + v.len()].copy_from_slice(v);
    put(&mut cs, 0, &param.to_le_bytes());
    put(&mut cs, 24, &parent.to_le_bytes());
    put(&mut cs, 32, &h.to_le_bytes());
    put(&mut cs, 36, &w.to_le_bytes());
    put(&mut cs, 40, &y.to_le_bytes());
    put(&mut cs, 44, &x.to_le_bytes());
    put(&mut cs, 48, &style.to_le_bytes());
    put(&mut cs, 56, &title_p.to_le_bytes());
    put(&mut cs, 64, &class_p.to_le_bytes());
    put(&mut cs, 72, &ex_style.to_le_bytes());
    let csp = cs.as_ptr() as i64;
    if call(wndproc, hwnd, WM_NCCREATE, 0, csp) == 0 || call(wndproc, hwnd, WM_CREATE, 0, csp) == -1 {
        destroy(hwnd, false);
        return fail(ERROR_GEN_FAILURE, 0);
    }
    if style & WS_VISIBLE != 0 {
        ShowWindow(hwnd, 1);
    }
    hwnd
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateWindowExA(
    ex: u32,
    class: *const u8,
    title: *const u8,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: u64,
    _menu: u64,
    _inst: u64,
    param: u64,
) -> u64 {
    // SAFETY: NUL-terminated title (or NULL).
    let t = unsafe { path::ansi_to_string(title) };
    create_window(ex, class_name(class as u64, false), t, style, x, y, w, h, parent, param, class as u64, title as u64)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateWindowExW(
    ex: u32,
    class: *const u16,
    title: *const u16,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: u64,
    _menu: u64,
    _inst: u64,
    param: u64,
) -> u64 {
    // SAFETY: NUL-terminated title (or NULL).
    let t = unsafe { path::wide_to_string(title) };
    create_window(ex, class_name(class as u64, true), t, style, x, y, w, h, parent, param, class as u64, title as u64)
}

fn destroy(hwnd: u64, notify: bool) -> bool {
    let Some(proc_) = wndproc_of(hwnd) else { return false };
    if notify {
        call(proc_, hwnd, WM_DESTROY, 0, 0);
        call(proc_, hwnd, WM_NCDESTROY, 0, 0);
    }
    with(|g| {
        if let Some(w) = g.windows.remove(&hwnd) {
            g.by_prism.remove(&w.win.id);
            if let Some(c) = g.conn.as_mut() {
                let _ = w.win.destroy(c);
            }
        }
        g.timers.retain(|t| t.hwnd != hwnd);
        g.queue.retain(|m| m.hwnd != hwnd);
        if g.focus == hwnd {
            g.focus = 0;
        }
    });
    true
}

extern "win64" fn DestroyWindow(hwnd: u64) -> Bool {
    if destroy(hwnd, true) {
        TRUE
    } else {
        fail(ERROR_INVALID_WINDOW_HANDLE, FALSE)
    }
}

extern "win64" fn IsWindow(hwnd: u64) -> Bool {
    with(|g| g.windows.contains_key(&hwnd)) as Bool
}

extern "win64" fn IsWindowVisible(hwnd: u64) -> Bool {
    with(|g| g.windows.get(&hwnd).is_some_and(|w| w.visible)) as Bool
}

extern "win64" fn ShowWindow(hwnd: u64, cmd: i32) -> Bool {
    let r = with(|g| {
        let w = g.windows.get_mut(&hwnd)?;
        let was = w.visible;
        let action = match cmd {
            0 => None,                                       // SW_HIDE
            2 | 6 | 7 | 11 => Some(proto::Action::Minimize), // SW_SHOWMINIMIZED, SW_MINIMIZE, ...
            3 => Some(proto::Action::Maximize),              // SW_MAXIMIZE
            9 => Some(proto::Action::Restore),               // SW_RESTORE
            _ => None,
        };
        w.visible = cmd != 0;
        if let (Some(a), Some(c)) = (action, g.conn.as_mut()) {
            let _ = g.windows.get_mut(&hwnd)?.win.action(c, a);
        }
        if !was && cmd != 0 {
            g.invalidate(hwnd, None, true);
        }
        Some(was)
    });
    match r {
        Some(was) => {
            if !was && cmd != 0 {
                if let Some(p) = wndproc_of(hwnd) {
                    call(p, hwnd, WM_SHOWWINDOW, 1, 0);
                }
            }
            was as Bool
        }
        None => fail(ERROR_INVALID_WINDOW_HANDLE, FALSE),
    }
}

extern "win64" fn UpdateWindow(hwnd: u64) -> Bool {
    // Wait (bounded) for PRISM to configure a new window, then paint now.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let ready = with(|g| {
            g.pump();
            g.windows.get(&hwnd).map(|w| (w.win.is_configured(), w.invalid.is_some() && w.visible))
        });
        match ready {
            None => return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE),
            Some((true, true)) => {
                if let Some(p) = wndproc_of(hwnd) {
                    call(p, hwnd, WM_PAINT, 0, 0);
                }
                return TRUE;
            }
            Some((true, false)) => return TRUE,
            Some((false, _)) if Instant::now() < deadline => with(|g| g.wait(Duration::from_millis(20))),
            Some((false, _)) => return TRUE,
        }
    }
}

extern "win64" fn InvalidateRect(hwnd: u64, r: *const i32, erase: Bool) -> Bool {
    let rect = read_rect(r);
    with(|g| {
        if hwnd == 0 {
            let all: Vec<u64> = g.windows.keys().copied().collect();
            for h in all {
                g.invalidate(h, rect, erase != 0);
            }
        } else {
            g.invalidate(hwnd, rect, erase != 0);
        }
    });
    TRUE
}

extern "win64" fn ValidateRect(hwnd: u64, _r: *const i32) -> Bool {
    with(|g| {
        if let Some(w) = g.windows.get_mut(&hwnd) {
            w.invalid = None;
        }
    });
    TRUE
}

/// RECT {left, top, right, bottom} → Rect.
fn read_rect(r: *const i32) -> Option<Rect> {
    if r.is_null() {
        return None;
    }
    // SAFETY: the caller's RECT.
    let (l, t, rr, b) = unsafe { (*r, *r.add(1), *r.add(2), *r.add(3)) };
    Some(Rect::new(l, t, (rr - l).max(0) as u32, (b - t).max(0) as u32))
}

fn write_rect(r: *mut i32, x: Rect) {
    if !r.is_null() {
        // SAFETY: the caller's RECT.
        unsafe {
            *r = x.x;
            *r.add(1) = x.y;
            *r.add(2) = x.x + x.w as i32;
            *r.add(3) = x.y + x.h as i32;
        }
    }
}

extern "win64" fn GetClientRect(hwnd: u64, r: *mut i32) -> Bool {
    match with(|g| g.windows.get(&hwnd).map(|w| w.size)) {
        Some((w, h)) => {
            write_rect(r, Rect::new(0, 0, w, h));
            TRUE
        }
        None => fail(ERROR_INVALID_WINDOW_HANDLE, FALSE),
    }
}

extern "win64" fn GetWindowRect(hwnd: u64, r: *mut i32) -> Bool {
    match with(|g| g.windows.get(&hwnd).map(|w| (w.pos, w.size))) {
        Some(((x, y), (w, h))) => {
            write_rect(r, Rect::new(x, y, w, h));
            TRUE
        }
        None => fail(ERROR_INVALID_WINDOW_HANDLE, FALSE),
    }
}

/// The client area is the whole PRISM surface: no frame to add.
extern "win64" fn AdjustWindowRect(r: *mut i32, _style: u32, _menu: Bool) -> Bool {
    (!r.is_null()) as Bool
}

extern "win64" fn AdjustWindowRectEx(r: *mut i32, _style: u32, _menu: Bool, _ex: u32) -> Bool {
    (!r.is_null()) as Bool
}

extern "win64" fn SetWindowPos(hwnd: u64, _after: u64, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> Bool {
    let r = with(|g| {
        let w = g.windows.get_mut(&hwnd)?;
        if flags & 0x2 == 0 {
            w.pos = (x, y);
        }
        let (nw, nh) = if flags & 0x1 == 0 { (cx.max(1) as u32, cy.max(1) as u32) } else { w.size };
        let (px, py) = w.pos;
        if flags & 0x40 != 0 {
            w.visible = true;
        }
        if flags & 0x80 != 0 {
            w.visible = false;
        }
        let c = g.conn.as_mut()?;
        let _ = g.windows.get_mut(&hwnd)?.win.set_geometry(
            c,
            if flags & 0x2 == 0 { px } else { -1 },
            if flags & 0x2 == 0 { py } else { -1 },
            nw,
            nh,
        );
        Some(())
    });
    r.map_or_else(|| fail(ERROR_INVALID_WINDOW_HANDLE, FALSE), |_| TRUE)
}

extern "win64" fn MoveWindow(hwnd: u64, x: i32, y: i32, w: i32, h: i32, repaint: Bool) -> Bool {
    let r = SetWindowPos(hwnd, 0, x, y, w, h, 0x4);
    if repaint != 0 {
        InvalidateRect(hwnd, core::ptr::null(), TRUE);
    }
    r
}

fn set_title(hwnd: u64, t: String) -> Bool {
    let ok = with(|g| {
        let w = g.windows.get_mut(&hwnd)?;
        w.title = t.clone();
        let c = g.conn.as_mut()?;
        let _ = g.windows.get_mut(&hwnd)?.win.set_title(c, &t);
        Some(())
    });
    ok.map_or_else(|| fail(ERROR_INVALID_WINDOW_HANDLE, FALSE), |_| TRUE)
}

extern "win64" fn SetWindowTextA(hwnd: u64, t: *const u8) -> Bool {
    // SAFETY: a NUL-terminated string.
    set_title(hwnd, unsafe { path::ansi_to_string(t) }.unwrap_or_default())
}

extern "win64" fn SetWindowTextW(hwnd: u64, t: *const u16) -> Bool {
    // SAFETY: a NUL-terminated string.
    set_title(hwnd, unsafe { path::wide_to_string(t) }.unwrap_or_default())
}

/// Returns the length copied (truncating like Windows), 0 on error.
extern "win64" fn GetWindowTextA(hwnd: u64, buf: *mut u8, size: i32) -> i32 {
    let Some(t) = with(|g| g.windows.get(&hwnd).map(|w| w.title.clone())) else {
        return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
    };
    if size <= 0 {
        return 0;
    }
    let mut s = t;
    while s.len() + 1 > size as usize {
        s.pop();
    }
    out_a(&s, buf, size as u32) as i32
}

extern "win64" fn GetWindowTextW(hwnd: u64, buf: *mut u16, size: i32) -> i32 {
    let Some(t) = with(|g| g.windows.get(&hwnd).map(|w| w.title.clone())) else {
        return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
    };
    if size <= 0 {
        return 0;
    }
    let mut s = t;
    while s.encode_utf16().count() + 1 > size as usize {
        s.pop();
    }
    out_w(&s, buf, size as u32) as i32
}

extern "win64" fn GetWindowTextLengthA(hwnd: u64) -> i32 {
    with(|g| g.windows.get(&hwnd).map_or(0, |w| w.title.len() as i32))
}

const GWLP_WNDPROC: i32 = -4;
const GWL_STYLE: i32 = -16;
const GWLP_USERDATA: i32 = -21;

extern "win64" fn GetWindowLongPtrA(hwnd: u64, idx: i32) -> i64 {
    with(|g| {
        let Some(w) = g.windows.get(&hwnd) else { return fail(ERROR_INVALID_WINDOW_HANDLE, 0) };
        match idx {
            GWLP_WNDPROC => w.wndproc as i64,
            GWL_STYLE => w.style as i64,
            GWLP_USERDATA => w.user_data,
            _ => fail(ERROR_INVALID_PARAMETER, 0),
        }
    })
}

extern "win64" fn SetWindowLongPtrA(hwnd: u64, idx: i32, v: i64) -> i64 {
    with(|g| {
        let Some(w) = g.windows.get_mut(&hwnd) else { return fail(ERROR_INVALID_WINDOW_HANDLE, 0) };
        match idx {
            GWLP_WNDPROC => core::mem::replace(&mut w.wndproc, v as u64) as i64,
            GWL_STYLE => core::mem::replace(&mut w.style, v as u32) as i64,
            GWLP_USERDATA => core::mem::replace(&mut w.user_data, v),
            _ => fail(ERROR_INVALID_PARAMETER, 0),
        }
    })
}

extern "win64" fn GetWindowLongA(hwnd: u64, idx: i32) -> i32 {
    GetWindowLongPtrA(hwnd, idx) as i32
}

extern "win64" fn SetWindowLongA(hwnd: u64, idx: i32, v: i32) -> i32 {
    SetWindowLongPtrA(hwnd, idx, v as i64) as i32
}

// ---- messages -------------------------------------------------------------------------

/// MSG (48 bytes): hwnd, message @8, wParam @16, lParam @24, time @32, pt @36.
fn write_msg(p: *mut u8, m: &Msg) {
    // SAFETY: the caller's MSG.
    unsafe {
        teb::put(p as u64, 0, m.hwnd);
        teb::put(p as u64, 8, m.msg);
        teb::put(p as u64, 16, m.wparam);
        teb::put(p as u64, 24, m.lparam);
        teb::put(p as u64, 32, m.time);
        teb::put(p as u64, 36, m.pt.0);
        teb::put(p as u64, 40, m.pt.1);
    }
}

fn read_msg(p: *const u8) -> Msg {
    // SAFETY: the caller's MSG.
    unsafe {
        Msg {
            hwnd: teb::get(p as u64, 0),
            msg: teb::get(p as u64, 8),
            wparam: teb::get(p as u64, 16),
            lparam: teb::get(p as u64, 24),
            time: teb::get(p as u64, 32),
            pt: (teb::get(p as u64, 36), teb::get(p as u64, 40)),
        }
    }
}

extern "win64" fn GetMessageA(msg: *mut u8, hwnd: u64, _min: u32, _max: u32) -> Bool {
    if msg.is_null() {
        return fail(ERROR_INVALID_PARAMETER, -1);
    }
    loop {
        let r = with(|g| {
            if let Some(m) = g.next_message(hwnd) {
                return Ok(m);
            }
            Err(g.next_timer_in())
        });
        match r {
            Ok(m) => {
                write_msg(msg, &m);
                return (m.msg != WM_QUIT) as Bool;
            }
            Err(t) => {
                crate::winsta::note_idle();
                with(|g| g.wait(t))
            }
        }
    }
}

extern "win64" fn GetMessageW(msg: *mut u8, hwnd: u64, min: u32, max: u32) -> Bool {
    GetMessageA(msg, hwnd, min, max)
}

const PM_REMOVE: u32 = 1;

extern "win64" fn PeekMessageA(msg: *mut u8, hwnd: u64, _min: u32, _max: u32, flags: u32) -> Bool {
    let m = with(|g| {
        let m = g.next_message(hwnd)?;
        if flags & PM_REMOVE == 0 && !matches!(m.msg, WM_PAINT | WM_TIMER | WM_QUIT) {
            g.queue.push_front(m);
        } else if flags & PM_REMOVE == 0 && m.msg == WM_QUIT {
            g.quit = Some(m.wparam as i32);
        }
        Some(m)
    });
    match m {
        Some(m) => {
            if !msg.is_null() {
                write_msg(msg, &m);
            }
            TRUE
        }
        None => FALSE,
    }
}

extern "win64" fn PeekMessageW(msg: *mut u8, hwnd: u64, min: u32, max: u32, flags: u32) -> Bool {
    PeekMessageA(msg, hwnd, min, max, flags)
}

extern "win64" fn TranslateMessage(msg: *const u8) -> Bool {
    if msg.is_null() {
        return FALSE;
    }
    let m = read_msg(msg);
    if m.msg != WM_KEYDOWN && m.msg != WM_SYSKEYDOWN {
        return FALSE;
    }
    with(|g| {
        if let Some(i) = g.chars.iter().position(|(vk, _)| *vk as u64 == m.wparam) {
            let (_, c) = g.chars.remove(i).expect("found");
            let time = m.time;
            g.queue.push_front(Msg { hwnd: m.hwnd, msg: WM_CHAR, wparam: c as u64, lparam: m.lparam, time, pt: m.pt });
            TRUE
        } else {
            FALSE
        }
    })
}

extern "win64" fn DispatchMessageA(msg: *const u8) -> i64 {
    if msg.is_null() {
        return 0;
    }
    let m = read_msg(msg);
    if m.msg == WM_TIMER && m.lparam != 0 {
        // SAFETY: a TIMERPROC passed to SetTimer.
        let f: TimerProc = unsafe { core::mem::transmute(m.lparam as usize) };
        f(m.hwnd, WM_TIMER, m.wparam, m.time);
        return 0;
    }
    match wndproc_of(m.hwnd) {
        Some(p) => call(p, m.hwnd, m.msg, m.wparam, m.lparam),
        None => 0,
    }
}

extern "win64" fn DispatchMessageW(msg: *const u8) -> i64 {
    DispatchMessageA(msg)
}

extern "win64" fn SendMessageA(hwnd: u64, msg: u32, w: u64, l: i64) -> i64 {
    match wndproc_of(hwnd) {
        Some(p) => call(p, hwnd, msg, w, l),
        None => fail(ERROR_INVALID_WINDOW_HANDLE, 0),
    }
}

extern "win64" fn SendMessageW(hwnd: u64, msg: u32, w: u64, l: i64) -> i64 {
    SendMessageA(hwnd, msg, w, l)
}

extern "win64" fn PostMessageA(hwnd: u64, msg: u32, w: u64, l: i64) -> Bool {
    with(|g| {
        if hwnd != 0 && !g.windows.contains_key(&hwnd) {
            return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
        }
        g.post(hwnd, msg, w, l);
        TRUE
    })
}

extern "win64" fn PostMessageW(hwnd: u64, msg: u32, w: u64, l: i64) -> Bool {
    PostMessageA(hwnd, msg, w, l)
}

extern "win64" fn PostQuitMessage(code: i32) {
    with(|g| g.quit = Some(code));
}

extern "win64" fn DefWindowProcA(hwnd: u64, msg: u32, _w: u64, l: i64) -> i64 {
    match msg {
        WM_NCCREATE => 1,
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_PAINT => {
            let mut ps = [0u8; 72];
            BeginPaint(hwnd, ps.as_mut_ptr());
            EndPaint(hwnd, ps.as_ptr());
            0
        }
        WM_ERASEBKGND => {
            let dc = _w;
            let r = with(|g| g.windows.get(&hwnd).map(|w| (w.size, g.classes[w.class].background)));
            if let Some(((w, h), bg)) = r {
                if bg != 0 {
                    fill(dc, Rect::new(0, 0, w, h), bg);
                }
            }
            1
        }
        WM_GETTEXTLENGTH => GetWindowTextLengthA(hwnd) as i64,
        WM_SETTEXT => {
            // SAFETY: lParam is a NUL-terminated string.
            set_title(hwnd, unsafe { path::ansi_to_string(l as *const u8) }.unwrap_or_default()) as i64
        }
        _ => 0,
    }
}

extern "win64" fn DefWindowProcW(hwnd: u64, msg: u32, w: u64, l: i64) -> i64 {
    DefWindowProcA(hwnd, msg, w, l)
}

extern "win64" fn SetTimer(hwnd: u64, id: u64, ms: u32, proc_: u64) -> u64 {
    with(|g| {
        if hwnd != 0 && !g.windows.contains_key(&hwnd) {
            return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
        }
        let id = if hwnd == 0 { g.handle() } else { id };
        let interval = (ms as u64).max(10);
        g.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
        g.timers.push(Timer { hwnd, id, interval, due: Instant::now() + Duration::from_millis(interval), proc_ });
        if id == 0 {
            1
        } else {
            id
        }
    })
}

extern "win64" fn KillTimer(hwnd: u64, id: u64) -> Bool {
    with(|g| {
        let n = g.timers.len();
        g.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
        (g.timers.len() != n) as Bool
    })
}

extern "win64" fn GetFocus() -> u64 {
    with(|g| g.focus)
}

extern "win64" fn SetFocus(hwnd: u64) -> u64 {
    with(|g| core::mem::replace(&mut g.focus, hwnd))
}

extern "win64" fn GetActiveWindow() -> u64 {
    with(|g| g.focus)
}

extern "win64" fn SetCapture(hwnd: u64) -> u64 {
    hwnd // PRISM delivers a pressed button's motion to its window anyway
}

extern "win64" fn ReleaseCapture() -> Bool {
    TRUE
}

extern "win64" fn GetKeyState(vk: i32) -> i16 {
    with(|g| if g.keys.get(vk as usize).copied().unwrap_or(false) { -0x8000i16 } else { 0 })
}

extern "win64" fn GetAsyncKeyState(vk: i32) -> i16 {
    GetKeyState(vk)
}

extern "win64" fn GetCursorPos(p: *mut i32) -> Bool {
    let (x, y) = with(|g| g.pointer);
    if !p.is_null() {
        // SAFETY: the caller's POINT.
        unsafe {
            *p = x;
            *p.add(1) = y;
        }
    }
    TRUE
}

extern "win64" fn ScreenToClient(hwnd: u64, p: *mut i32) -> Bool {
    let Some((x, y)) = with(|g| g.windows.get(&hwnd).map(|w| w.pos)) else { return FALSE };
    if !p.is_null() {
        // SAFETY: the caller's POINT.
        unsafe {
            *p -= x;
            *p.add(1) -= y;
        }
    }
    TRUE
}

extern "win64" fn ClientToScreen(hwnd: u64, p: *mut i32) -> Bool {
    let Some((x, y)) = with(|g| g.windows.get(&hwnd).map(|w| w.pos)) else { return FALSE };
    if !p.is_null() {
        // SAFETY: the caller's POINT.
        unsafe {
            *p += x;
            *p.add(1) += y;
        }
    }
    TRUE
}

extern "win64" fn GetSystemMetrics(i: i32) -> i32 {
    with(|g| {
        let (w, h) = g.conn().map(|c| (c.screen_width as i32, c.screen_height as i32)).unwrap_or((1024, 768));
        match i {
            0 | 16 | 78 => w, // SM_CXSCREEN, SM_CXFULLSCREEN, SM_CXVIRTUALSCREEN
            1 | 17 | 79 => h,
            4 => 30,       // SM_CYCAPTION
            5 | 6 => 1,    // SM_CXBORDER / SM_CYBORDER
            11 | 12 => 32, // SM_CXICON / SM_CYICON
            13 | 14 => 32, // SM_CXCURSOR
            80 => 1,       // SM_CMONITORS
            _ => 0,
        }
    })
}

/// The system-wide settings a program reads. OSyx has one configuration, so
/// the queries answer from it and a change request is refused: the desktop
/// keeps its own appearance, and `SystemParametersInfo` is how Windows
/// programs learn about it rather than how they set it.
extern "win64" fn SystemParametersInfoW(action: u32, param: u32, value: *mut u8, _ini: u32) -> Bool {
    system_parameters(action, param, value)
}

extern "win64" fn SystemParametersInfoA(action: u32, param: u32, value: *mut u8, _ini: u32) -> Bool {
    system_parameters(action, param, value)
}

const SPI_GETWORKAREA: u32 = 0x0030;

/// A BOOL or integer setting, as Windows reports it on a stock desktop.
fn parameter_value(action: u32) -> Option<i32> {
    Some(match action {
        0x000A => 31,  // SPI_GETKEYBOARDSPEED
        0x000D => 75,  // SPI_ICONHORIZONTALSPACING
        0x000E => 0,   // SPI_GETSCREENSAVETIMEOUT
        0x0010 => 0,   // SPI_GETSCREENSAVEACTIVE
        0x0016 => 1,   // SPI_GETKEYBOARDDELAY
        0x0018 => 75,  // SPI_ICONVERTICALSPACING
        0x0019 => 1,   // SPI_GETICONTITLEWRAP
        0x004A => 0,   // SPI_GETFONTSMOOTHING: the renderer has no ClearType
        0x0066 => 400, // SPI_GETMOUSEHOVERTIME
        0x0068 => 3,   // SPI_GETWHEELSCROLLLINES
        0x006A => 400, // SPI_GETMENUSHOWDELAY
        0x0070 => 10,  // SPI_GETMOUSESPEED
        0x0072 => 2,   // SPI_GETMOUSEWHEELROUTING: to the window with focus
        0x1002 => 0,   // SPI_GETMENUANIMATION
        0x1004 => 0,   // SPI_GETCOMBOBOXANIMATION
        0x1008 => 0,   // SPI_GETGRADIENTCAPTIONS
        0x100A => 1,   // SPI_GETKEYBOARDCUES
        0x1012 => 0,   // SPI_GETSELECTIONFADE
        0x1014 => 0,   // SPI_GETTOOLTIPANIMATION
        0x1016 => 0,   // SPI_GETTOOLTIPFADE
        0x1018 => 0,   // SPI_GETCURSORSHADOW
        0x101E => 0,   // SPI_GETMENUFADE
        0x1022 => 1,   // SPI_GETFLATMENU
        0x1024 => 3,   // SPI_GETWHEELSCROLLCHARS
        0x103E => 1,   // SPI_GETUIEFFECTS
        0x1042 => 1,   // SPI_GETCLIENTAREAANIMATION
        _ => return None,
    })
}

fn system_parameters(action: u32, _param: u32, value: *mut u8) -> Bool {
    if value.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if action == SPI_GETWORKAREA {
        let (w, h) = with(|g| g.conn().map(|c| (c.screen_width as i32, c.screen_height as i32)).unwrap_or((1024, 768)));
        // SAFETY: the caller's RECT, 16 bytes.
        unsafe {
            teb::put(value as u64, 0, 0i32);
            teb::put(value as u64, 4, 0i32);
            teb::put(value as u64, 8, w);
            teb::put(value as u64, 12, h);
        }
        return TRUE;
    }
    match parameter_value(action) {
        Some(v) => {
            // SAFETY: the caller's BOOL or integer, 4 bytes.
            unsafe { teb::put(value as u64, 0, v) };
            TRUE
        }
        None => fail(ERROR_INVALID_PARAMETER, FALSE),
    }
}

/// Predefined cursors and icons: the resource id is the handle (PRISM
/// draws its own pointer).
extern "win64" fn LoadCursorA(_inst: u64, id: u64) -> u64 {
    if id < 0x10000 {
        0x1_0000 + id
    } else {
        fail(ERROR_NOT_SUPPORTED, 0)
    }
}

extern "win64" fn LoadCursorW(inst: u64, id: u64) -> u64 {
    LoadCursorA(inst, id)
}

extern "win64" fn LoadIconA(inst: u64, id: u64) -> u64 {
    LoadCursorA(inst, id)
}

extern "win64" fn LoadIconW(inst: u64, id: u64) -> u64 {
    LoadCursorA(inst, id)
}

extern "win64" fn SetCursor(c: u64) -> u64 {
    c
}

extern "win64" fn GetSysColor(i: i32) -> u32 {
    to_colorref(sys_color(i as u32))
}

extern "win64" fn GetSysColorBrush(i: i32) -> u64 {
    with(|g| {
        let h = g.handle();
        g.gdi.insert(h, Gdi::Brush(sys_color(i as u32)));
        h
    })
}

extern "win64" fn MessageBeep(_t: u32) -> Bool {
    TRUE
}

// ---- painting -------------------------------------------------------------------------

/// Waits (bounded) until PRISM released the window's buffer.
fn wait_drawable(hwnd: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let ok = with(|g| {
            g.pump();
            g.windows.get(&hwnd).map(|w| w.win.can_draw())
        });
        match ok {
            Some(true) => return true,
            Some(false) if Instant::now() < deadline => with(|g| g.wait(Duration::from_millis(5))),
            _ => return false,
        }
    }
}

fn new_dc(g: &mut Gui, hwnd: u64, paint: Rect) -> u64 {
    let h = g.handle();
    g.dcs.insert(
        h,
        Dc {
            hwnd,
            paint,
            text: rgb(0, 0, 0),
            bk: rgb(255, 255, 255),
            opaque: true,
            brush: STOCK_BASE + WHITE_BRUSH as u64,
            pen: STOCK_BASE + BLACK_PEN as u64,
            font: STOCK_BASE + SYSTEM_FONT as u64,
            pos: (0, 0),
        },
    );
    h
}

/// PAINTSTRUCT (72 bytes): hdc, fErase @8, rcPaint @12, ...
extern "win64" fn BeginPaint(hwnd: u64, ps: *mut u8) -> u64 {
    if !wait_drawable(hwnd) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
    }
    let r = with(|g| {
        let w = g.windows.get_mut(&hwnd)?;
        let full = Rect::new(0, 0, w.size.0, w.size.1);
        let paint = w.invalid.take().unwrap_or(full);
        let erase = core::mem::replace(&mut w.erase, false);
        let dc = new_dc(g, hwnd, paint);
        Some((dc, paint, erase))
    });
    let Some((dc, paint, erase)) = r else { return fail(ERROR_INVALID_WINDOW_HANDLE, 0) };
    if erase {
        if let Some(p) = wndproc_of(hwnd) {
            call(p, hwnd, WM_ERASEBKGND, dc, 0);
        }
    }
    if !ps.is_null() {
        // SAFETY: the caller's PAINTSTRUCT.
        unsafe {
            core::ptr::write_bytes(ps, 0, 72);
            teb::put(ps as u64, 0, dc);
            teb::put(ps as u64, 8, erase as i32);
        }
        write_rect((ps as usize + 12) as *mut i32, paint);
    }
    dc
}

fn present(hwnd: u64, r: Option<Rect>) {
    with(|g| {
        let Some(w) = g.windows.get_mut(&hwnd) else { return };
        let full = Rect::new(0, 0, w.size.0, w.size.1);
        let damage = r.or(w.drawn.take()).unwrap_or(full).intersect(&full);
        w.drawn = None;
        if damage.w == 0 || damage.h == 0 {
            return;
        }
        let rect = proto::Rect { x: damage.x, y: damage.y, w: damage.w, h: damage.h };
        let Some(c) = g.conn.as_mut() else { return };
        let _ = g.windows.get_mut(&hwnd).map(|w| w.win.present(c, Some(rect)));
    });
}

extern "win64" fn EndPaint(hwnd: u64, ps: *const u8) -> Bool {
    if ps.is_null() {
        return FALSE;
    }
    // SAFETY: the caller's PAINTSTRUCT from BeginPaint.
    let dc: u64 = unsafe { teb::get(ps as u64, 0) };
    let paint = with(|g| g.dcs.remove(&dc).map(|d| d.paint));
    present(hwnd, paint);
    TRUE
}

extern "win64" fn GetDC(hwnd: u64) -> u64 {
    if hwnd == 0 || !wait_drawable(hwnd) {
        return fail(ERROR_NOT_SUPPORTED, 0); // no drawing on the whole screen
    }
    with(|g| {
        let Some(size) = g.windows.get(&hwnd).map(|w| w.size) else { return 0 };
        new_dc(g, hwnd, Rect::new(0, 0, size.0, size.1))
    })
}

extern "win64" fn ReleaseDC(hwnd: u64, dc: u64) -> i32 {
    let had = with(|g| g.dcs.remove(&dc).is_some());
    if had {
        present(hwnd, None);
    }
    had as i32
}

/// Runs `f` on the DC's window surface, clipped to its paint rectangle;
/// records what was drawn for the next present.
fn draw<R>(dc: u64, f: impl FnOnce(&mut Surface, &Dc, &BTreeMap<u64, Gdi>, Rect) -> (R, Rect)) -> Option<R> {
    with(|g| {
        let d = g.dcs.get(&dc)?;
        let w = g.windows.get_mut(&d.hwnd)?;
        let full = Rect::new(0, 0, w.size.0, w.size.1);
        let clip = d.paint.intersect(&full);
        let mut s = w.win.surface().ok()?;
        let (r, touched) = f(&mut s, d, &g.gdi, clip);
        let touched = touched.intersect(&clip);
        let w = g.windows.get_mut(&g.dcs.get(&dc)?.hwnd)?;
        w.drawn = Some(match w.drawn {
            Some(o) => o.union(&touched),
            None => touched,
        });
        Some(r)
    })
}

/// A brush handle, or a system color index + 1 (as in WNDCLASS.hbrBackground).
fn brush_color(gdi: &BTreeMap<u64, Gdi>, b: u64) -> Option<Color> {
    match gdi.get(&b) {
        Some(Gdi::Brush(c)) => Some(*c),
        Some(_) => None,
        None if (1..=31).contains(&b) => Some(sys_color(b as u32 - 1)),
        None => None,
    }
}

fn fill(dc: u64, r: Rect, brush: u64) -> bool {
    draw(dc, |s, _, gdi, clip| match brush_color(gdi, brush) {
        Some(c) => {
            let r2 = r.intersect(&clip);
            s.fill_rect(r2, c);
            (true, r2)
        }
        None => (false, Rect::EMPTY),
    })
    .unwrap_or(false)
}

extern "win64" fn FillRect(dc: u64, r: *const i32, brush: u64) -> i32 {
    match read_rect(r) {
        Some(rect) => fill(dc, rect, brush) as i32,
        None => 0,
    }
}

extern "win64" fn FrameRect(dc: u64, r: *const i32, brush: u64) -> i32 {
    let Some(r) = read_rect(r) else { return 0 };
    draw(dc, |s, _, gdi, clip| {
        let Some(c) = brush_color(gdi, brush) else { return (0, Rect::EMPTY) };
        for e in [
            Rect::new(r.x, r.y, r.w, 1),
            Rect::new(r.x, r.y + r.h as i32 - 1, r.w, 1),
            Rect::new(r.x, r.y, 1, r.h),
            Rect::new(r.x + r.w as i32 - 1, r.y, 1, r.h),
        ] {
            s.fill_rect(e.intersect(&clip), c);
        }
        (1, r)
    })
    .unwrap_or(0)
}

// ---- fonts and text -----------------------------------------------------------------

struct Fonts {
    sans: PropFont<'static>,
    bold: PropFont<'static>,
    large: PropFont<'static>,
    mono: MonoFont<'static>,
}

fn fonts() -> &'static Fonts {
    static F: Mutex<Option<&'static Fonts>> = Mutex::new(None);
    let mut g = F.lock().unwrap();
    g.get_or_insert_with(|| {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(Fonts {
            sans: PropFont::parse(SANS14).expect("embedded sans14"),
            bold: PropFont::parse(SANS14B).expect("embedded sans14b"),
            large: PropFont::parse(SANS20).expect("embedded sans20"),
            mono: MonoFont::parse(MONO15).expect("embedded mono15"),
        }))
    })
}

fn font_of(gdi: &BTreeMap<u64, Gdi>, h: u64) -> FontKind {
    match gdi.get(&h) {
        Some(Gdi::Font(k)) => *k,
        _ => FontKind::Sans,
    }
}

fn line_height(k: FontKind) -> u32 {
    let f = fonts();
    match k {
        FontKind::Sans => f.sans.line_height(),
        FontKind::SansBold => f.bold.line_height(),
        FontKind::Large => f.large.line_height(),
        FontKind::Mono => f.mono.line_height(),
    }
}

fn measure(k: FontKind, t: &str) -> u32 {
    let f = fonts();
    match k {
        FontKind::Sans => f.sans.measure(t),
        FontKind::SansBold => f.bold.measure(t),
        FontKind::Large => f.large.measure(t),
        FontKind::Mono => f.mono.measure(t),
    }
}

fn draw_text(s: &mut Surface, k: FontKind, x: i32, y: i32, t: &str, c: Color, clip: Rect) -> Rect {
    let f = fonts();
    match k {
        FontKind::Sans => f.sans.draw_clipped(s, x, y, t, c, 255, clip),
        FontKind::SansBold => f.bold.draw_clipped(s, x, y, t, c, 255, clip),
        FontKind::Large => f.large.draw_clipped(s, x, y, t, c, 255, clip),
        FontKind::Mono => {
            // The cell font has no clipped variant: draw into a clipped view.
            let r = Rect::new(x, y, f.mono.measure(t), f.mono.line_height()).intersect(&clip);
            if r.w > 0 && r.h > 0 {
                f.mono.draw(s, x, y, t, c, 255);
            }
            r
        }
    }
}

fn text_out(dc: u64, x: i32, y: i32, t: &str) -> Bool {
    draw(dc, |s, d, gdi, clip| {
        let k = font_of(gdi, d.font);
        let r = Rect::new(x, y, measure(k, t), line_height(k));
        if d.opaque {
            s.fill_rect(r.intersect(&clip), d.bk);
        }
        draw_text(s, k, x, y, t, d.text, clip);
        (TRUE, r)
    })
    .unwrap_or(FALSE)
}

extern "win64" fn TextOutA(dc: u64, x: i32, y: i32, s: *const u8, n: i32) -> Bool {
    // SAFETY: the caller's buffer of `n` bytes.
    let b = unsafe { core::slice::from_raw_parts(s, n.max(0) as usize) };
    text_out(dc, x, y, &String::from_utf8_lossy(b))
}

extern "win64" fn TextOutW(dc: u64, x: i32, y: i32, s: *const u16, n: i32) -> Bool {
    // SAFETY: the caller's buffer of `n` units.
    let u = unsafe { core::slice::from_raw_parts(s, n.max(0) as usize) };
    let t: String = char::decode_utf16(u.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    text_out(dc, x, y, &t)
}

const DT_CENTER: u32 = 0x1;
const DT_RIGHT: u32 = 0x2;
const DT_VCENTER: u32 = 0x4;
const DT_BOTTOM: u32 = 0x8;
const DT_WORDBREAK: u32 = 0x10;
const DT_SINGLELINE: u32 = 0x20;
const DT_CALCRECT: u32 = 0x400;

fn draw_text_impl(dc: u64, text: &str, r: *mut i32, fmt: u32) -> i32 {
    let Some(area) = read_rect(r) else { return 0 };
    let res = draw(dc, |s, d, gdi, clip| {
        let k = font_of(gdi, d.font);
        let lh = line_height(k) as i32;
        // Lines: explicit breaks, then word wrapping inside the width.
        let mut lines: Vec<String> = Vec::new();
        let paras: Vec<&str> = if fmt & DT_SINGLELINE != 0 { alloc::vec![text] } else { text.split('\n').collect() };
        for p in paras {
            let p = p.trim_end_matches('\r');
            if fmt & DT_WORDBREAK == 0 {
                lines.push(p.to_string());
                continue;
            }
            let mut cur = String::new();
            for word in p.split(' ') {
                let cand = if cur.is_empty() { word.to_string() } else { alloc::format!("{cur} {word}") };
                if measure(k, &cand) > area.w && !cur.is_empty() {
                    lines.push(core::mem::take(&mut cur));
                    cur = word.to_string();
                } else {
                    cur = cand;
                }
            }
            lines.push(cur);
        }
        let height = lh * lines.len() as i32;
        let width = lines.iter().map(|l| measure(k, l)).max().unwrap_or(0);
        if fmt & DT_CALCRECT != 0 {
            return ((height, Some((width, height))), Rect::EMPTY);
        }
        let mut y = area.y;
        if fmt & DT_SINGLELINE != 0 && fmt & DT_VCENTER != 0 {
            y = area.y + (area.h as i32 - lh) / 2;
        } else if fmt & DT_SINGLELINE != 0 && fmt & DT_BOTTOM != 0 {
            y = area.y + area.h as i32 - lh;
        }
        let clip2 = clip.intersect(&area);
        for l in &lines {
            let w = measure(k, l) as i32;
            let x = if fmt & DT_CENTER != 0 {
                area.x + (area.w as i32 - w) / 2
            } else if fmt & DT_RIGHT != 0 {
                area.x + area.w as i32 - w
            } else {
                area.x
            };
            if d.opaque {
                s.fill_rect(Rect::new(x, y, w as u32, lh as u32).intersect(&clip2), d.bk);
            }
            draw_text(s, k, x, y, l, d.text, clip2);
            y += lh;
        }
        ((height, None), area)
    });
    match res {
        Some((h, Some((w, hh)))) => {
            write_rect(r, Rect::new(area.x, area.y, w, hh as u32));
            h
        }
        Some((h, None)) => h,
        None => 0,
    }
}

extern "win64" fn DrawTextA(dc: u64, s: *const u8, n: i32, r: *mut i32, fmt: u32) -> i32 {
    let t = if n < 0 {
        // SAFETY: NUL-terminated text.
        unsafe { path::ansi_to_string(s) }.unwrap_or_default()
    } else {
        // SAFETY: the caller's buffer of `n` bytes.
        String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(s, n as usize) }).into_owned()
    };
    draw_text_impl(dc, &t, r, fmt)
}

extern "win64" fn DrawTextW(dc: u64, s: *const u16, n: i32, r: *mut i32, fmt: u32) -> i32 {
    let t = if n < 0 {
        // SAFETY: NUL-terminated text.
        unsafe { path::wide_to_string(s) }.unwrap_or_default()
    } else {
        // SAFETY: the caller's buffer of `n` units.
        let u = unsafe { core::slice::from_raw_parts(s, n as usize) };
        char::decode_utf16(u.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    };
    draw_text_impl(dc, &t, r, fmt)
}

fn extent(dc: u64, t: &str, out: *mut i32) -> Bool {
    let k = with(|g| g.dcs.get(&dc).map(|d| font_of(&g.gdi, d.font)));
    let Some(k) = k else { return FALSE };
    if !out.is_null() {
        // SAFETY: the caller's SIZE.
        unsafe {
            *out = measure(k, t) as i32;
            *out.add(1) = line_height(k) as i32;
        }
    }
    TRUE
}

extern "win64" fn GetTextExtentPoint32A(dc: u64, s: *const u8, n: i32, out: *mut i32) -> Bool {
    // SAFETY: the caller's buffer of `n` bytes.
    let b = unsafe { core::slice::from_raw_parts(s, n.max(0) as usize) };
    extent(dc, &String::from_utf8_lossy(b), out)
}

extern "win64" fn GetTextExtentPoint32W(dc: u64, s: *const u16, n: i32, out: *mut i32) -> Bool {
    // SAFETY: the caller's buffer of `n` units.
    let u = unsafe { core::slice::from_raw_parts(s, n.max(0) as usize) };
    let t: String = char::decode_utf16(u.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    extent(dc, &t, out)
}

/// TEXTMETRICA (56 bytes): tmHeight, tmAscent, tmDescent, tmInternalLeading,
/// tmExternalLeading, tmAveCharWidth, tmMaxCharWidth, ...
extern "win64" fn GetTextMetricsA(dc: u64, tm: *mut u8) -> Bool {
    let k = with(|g| g.dcs.get(&dc).map(|d| font_of(&g.gdi, d.font)));
    let (Some(k), false) = (k, tm.is_null()) else { return FALSE };
    let h = line_height(k) as i32;
    let avg = measure(k, "x") as i32;
    let max = measure(k, "W") as i32;
    // SAFETY: the caller's TEXTMETRIC (at least 56 bytes).
    unsafe {
        core::ptr::write_bytes(tm, 0, 56);
        teb::put(tm as u64, 0, h);
        teb::put(tm as u64, 4, h * 4 / 5);
        teb::put(tm as u64, 8, h - h * 4 / 5);
        teb::put(tm as u64, 20, avg);
        teb::put(tm as u64, 24, max);
        teb::put(tm as u64, 28, if matches!(k, FontKind::SansBold) { 700i32 } else { 400 });
    }
    TRUE
}

extern "win64" fn GetTextMetricsW(dc: u64, tm: *mut u8) -> Bool {
    GetTextMetricsA(dc, tm)
}

fn dc_set<R>(dc: u64, f: impl FnOnce(&mut Dc) -> R) -> Option<R> {
    with(|g| g.dcs.get_mut(&dc).map(f))
}

extern "win64" fn SetTextColor(dc: u64, c: u32) -> u32 {
    dc_set(dc, |d| to_colorref(core::mem::replace(&mut d.text, colorref(c)))).unwrap_or(0xFFFF_FFFF)
}

extern "win64" fn SetBkColor(dc: u64, c: u32) -> u32 {
    dc_set(dc, |d| to_colorref(core::mem::replace(&mut d.bk, colorref(c)))).unwrap_or(0xFFFF_FFFF)
}

extern "win64" fn SetBkMode(dc: u64, mode: i32) -> i32 {
    dc_set(dc, |d| {
        let old = if d.opaque { 2 } else { 1 };
        d.opaque = mode == 2;
        old
    })
    .unwrap_or(0)
}

extern "win64" fn CreateSolidBrush(c: u32) -> u64 {
    with(|g| {
        let h = g.handle();
        g.gdi.insert(h, Gdi::Brush(colorref(c)));
        h
    })
}

extern "win64" fn CreatePen(style: i32, width: i32, c: u32) -> u64 {
    with(|g| {
        let h = g.handle();
        g.gdi.insert(h, if style == 5 { Gdi::NullPen } else { Gdi::Pen(colorref(c), width.max(1) as u32) });
        h
    })
}

fn create_font(height: i32, weight: i32, pitch: u32, face: Option<String>) -> u64 {
    let fixed = pitch & 0x3 == 1
        || pitch & 0xF0 == 0x30
        || face.as_deref().is_some_and(|f| {
            let f = f.to_ascii_lowercase();
            f.contains("consol") || f.contains("courier") || f.contains("mono")
        });
    let kind = if fixed {
        FontKind::Mono
    } else if height.unsigned_abs() >= 20 {
        FontKind::Large
    } else if weight >= 600 {
        FontKind::SansBold
    } else {
        FontKind::Sans
    };
    with(|g| {
        let h = g.handle();
        g.gdi.insert(h, Gdi::Font(kind));
        h
    })
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateFontA(
    h: i32,
    _w: i32,
    _esc: i32,
    _or: i32,
    weight: i32,
    _it: u32,
    _ul: u32,
    _so: u32,
    _cs: u32,
    _op: u32,
    _cp: u32,
    _q: u32,
    pitch: u32,
    face: *const u8,
) -> u64 {
    // SAFETY: NULL or a NUL-terminated face name.
    create_font(h, weight, pitch, unsafe { path::ansi_to_string(face) })
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateFontW(
    h: i32,
    _w: i32,
    _esc: i32,
    _or: i32,
    weight: i32,
    _it: u32,
    _ul: u32,
    _so: u32,
    _cs: u32,
    _op: u32,
    _cp: u32,
    _q: u32,
    pitch: u32,
    face: *const u16,
) -> u64 {
    // SAFETY: as above.
    create_font(h, weight, pitch, unsafe { path::wide_to_string(face) })
}

/// LOGFONTW: lfHeight @0, lfWeight @16, lfPitchAndFamily @27, lfFaceName @28.
extern "win64" fn CreateFontIndirectW(lf: *const u8) -> u64 {
    if lf.is_null() {
        return 0;
    }
    // SAFETY: the caller's LOGFONTW.
    unsafe {
        create_font(
            teb::get(lf as u64, 0),
            teb::get(lf as u64, 16),
            teb::get::<u8>(lf as u64, 27) as u32,
            path::wide_to_string(lf.add(28) as *const u16),
        )
    }
}

extern "win64" fn GetStockObject(i: i32) -> u64 {
    if with(|g| g.gdi.contains_key(&(STOCK_BASE + i as u64))) {
        STOCK_BASE + i as u64
    } else {
        0
    }
}

extern "win64" fn SelectObject(dc: u64, obj: u64) -> u64 {
    with(|g| {
        let kind = g.gdi.get(&obj).copied();
        let d = g.dcs.get_mut(&dc)?;
        Some(match kind? {
            Gdi::Brush(_) | Gdi::NullBrush => core::mem::replace(&mut d.brush, obj),
            Gdi::Pen(..) | Gdi::NullPen => core::mem::replace(&mut d.pen, obj),
            Gdi::Font(_) => core::mem::replace(&mut d.font, obj),
        })
    })
    .unwrap_or(0)
}

extern "win64" fn DeleteObject(obj: u64) -> Bool {
    if (STOCK_BASE..STOCK_BASE + 32).contains(&obj) {
        return TRUE; // stock objects are never deleted
    }
    with(|g| g.gdi.remove(&obj).is_some()) as Bool
}

extern "win64" fn MoveToEx(dc: u64, x: i32, y: i32, old: *mut i32) -> Bool {
    dc_set(dc, |d| {
        if !old.is_null() {
            // SAFETY: the caller's POINT.
            unsafe {
                *old = d.pos.0;
                *old.add(1) = d.pos.1;
            }
        }
        d.pos = (x, y);
    })
    .is_some() as Bool
}

extern "win64" fn LineTo(dc: u64, x: i32, y: i32) -> Bool {
    let r = draw(dc, |s, d, gdi, clip| {
        let (x0, y0) = d.pos;
        let touched = Rect::new(x0.min(x), y0.min(y), (x0 - x).unsigned_abs() + 1, (y0 - y).unsigned_abs() + 1);
        if let Some(Gdi::Pen(c, width)) = gdi.get(&d.pen) {
            for o in 0..*width as i32 {
                line_clipped(
                    s,
                    x0 + o * (y0 != y) as i32,
                    y0 + o * (y0 == y) as i32,
                    x + o * (y0 != y) as i32,
                    y + o * (y0 == y) as i32,
                    *c,
                    clip,
                );
            }
        }
        ((), touched)
    });
    dc_set(dc, |d| d.pos = (x, y));
    r.is_some() as Bool
}

fn line_clipped(s: &mut Surface, x0: i32, y0: i32, x1: i32, y1: i32, c: Color, clip: Rect) {
    // Bresenham with per-pixel clipping.
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut e) = (x0, y0, dx + dy);
    loop {
        if x >= clip.x && y >= clip.y && x < clip.x + clip.w as i32 && y < clip.y + clip.h as i32 {
            s.put(x, y, c);
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * e;
        if e2 >= dy {
            e += dy;
            x += sx;
        }
        if e2 <= dx {
            e += dx;
            y += sy;
        }
    }
}

extern "win64" fn Rectangle(dc: u64, l: i32, t: i32, r: i32, b: i32) -> Bool {
    let rect = Rect::new(l, t, (r - l).max(0) as u32, (b - t).max(0) as u32);
    draw(dc, |s, d, gdi, clip| {
        if let Some(c) = brush_color(gdi, d.brush) {
            s.fill_rect(rect.intersect(&clip), c);
        }
        if let Some(Gdi::Pen(c, _)) = gdi.get(&d.pen) {
            for e in [
                Rect::new(l, t, rect.w, 1),
                Rect::new(l, b - 1, rect.w, 1),
                Rect::new(l, t, 1, rect.h),
                Rect::new(r - 1, t, 1, rect.h),
            ] {
                s.fill_rect(e.intersect(&clip), *c);
            }
        }
        (TRUE, rect)
    })
    .unwrap_or(FALSE)
}

extern "win64" fn RoundRect(dc: u64, l: i32, t: i32, r: i32, b: i32, w: i32, _h: i32) -> Bool {
    let rect = Rect::new(l, t, (r - l).max(0) as u32, (b - t).max(0) as u32);
    draw(dc, |s, d, gdi, clip| {
        if let Some(c) = brush_color(gdi, d.brush) {
            if clip.intersect(&rect) == rect {
                s.rounded_rect(rect, (w / 2).max(0) as u32, c, 255);
            } else {
                s.fill_rect(rect.intersect(&clip), c);
            }
        }
        (TRUE, rect)
    })
    .unwrap_or(FALSE)
}

extern "win64" fn SetPixel(dc: u64, x: i32, y: i32, c: u32) -> u32 {
    draw(dc, |s, _, _, clip| {
        if x >= clip.x && y >= clip.y && x < clip.x + clip.w as i32 && y < clip.y + clip.h as i32 {
            s.put(x, y, colorref(c));
        }
        (c, Rect::new(x, y, 1, 1))
    })
    .unwrap_or(0xFFFF_FFFF)
}

/// A PRISM dialog implementing MB_OK and MB_OKCANCEL. Other button sets
/// fail explicitly; Escape/close cancel a dialog that offers Cancel.
fn message_box(owner: u64, text: String, caption: String, flags: u32) -> i32 {
    let cancel = match flags & 15 { 0 => false, 1 => true, _ => return fail(ERROR_NOT_SUPPORTED, 0) };
    let _ = owner;
    let lines: Vec<&str> = text.split('\n').collect();
    let k = FontKind::Sans;
    let width = (lines.iter().map(|l| measure(k, l)).max().unwrap_or(100) + 48).clamp(260, 900);
    let height = 70 + line_height(k) * lines.len() as u32 + 40;
    let made = with(|g| -> Option<Window> {
        let mut spec = WindowSpec::normal(&caption, width, height);
        spec.flags = proto::window_flags::DECORATED;
        spec.min_width = width;
        spec.min_height = height;
        g.conn().ok()?.create_window(spec).ok()
    });
    let Some(mut win) = made else { return fail(ERROR_NOT_SUPPORTED, 0) };
    let mut drawn = false;
    let button = Rect::new(width as i32 - if cancel { 200 } else { 104 }, height as i32 - 44, 88, 30);
    let cancel_button = Rect::new(width as i32 - 104, height as i32 - 44, 88, 30);
    let mut result = 1;
    loop {
        let evs: Vec<Event> = with(|g| {
            let mut v = Vec::new();
            if let Some(c) = g.conn.as_mut() {
                let _ = c.wait(&mut [], Some(Duration::from_millis(100)));
                while let Ok(Some(e)) = c.poll() {
                    v.push(e);
                }
            }
            v
        });
        let mut done = false;
        let mut other = Vec::new();
        for ev in evs {
            if !win.on_event(&ev) {
                other.push(ev);
                continue;
            }
            match ev {
                Event::CloseRequest { .. } => { result = if cancel { 2 } else { 1 }; done = true; },
                Event::Key { usage, state, .. }
                    if state == proto::key_state::DOWN && (usage == 0x28 || usage == 0x29 || usage == 0x58) =>
                {
                    result = if cancel && usage == 0x29 { 2 } else { 1 };
                    done = true
                }
                Event::Pointer { kind, x, y, pressed, .. }
                    if kind == proto::pointer::BUTTON
                        && pressed
                        && x >= button.x
                        && y >= button.y
                        && x < button.x + 88
                        && y < button.y + 30 =>
                {
                    done = true
                }
                Event::Pointer { kind, x, y, pressed, .. }
                    if cancel && kind == proto::pointer::BUTTON && pressed
                        && x >= cancel_button.x && y >= cancel_button.y
                        && x < cancel_button.x + 88 && y < cancel_button.y + 30 =>
                { result = 2; done = true; }
                _ => {}
            }
        }
        with(|g| {
            for e in other {
                g.on_event(e);
            }
        });
        if done {
            break;
        }
        if !drawn && win.can_draw() {
            if let Ok(mut s) = win.surface() {
                s.fill(rgb(243, 243, 243));
                let lh = line_height(k) as i32;
                for (i, l) in lines.iter().enumerate() {
                    draw_text(&mut s, k, 24, 24 + i as i32 * lh, l, rgb(20, 20, 20), Rect::new(0, 0, width, height));
                }
                s.fill_rect(Rect::new(0, height as i32 - 60, width, 60), rgb(230, 230, 230));
                s.rounded_rect(button, 4, rgb(0, 95, 184), 255);
                if cancel {
                    s.rounded_rect(cancel_button, 4, rgb(210, 210, 210), 255);
                    draw_text(&mut s, k, cancel_button.x + 20, cancel_button.y + 6, "Cancel", rgb(20,20,20), cancel_button);
                }
                let tw = measure(k, "OK") as i32;
                draw_text(
                    &mut s,
                    k,
                    button.x + (88 - tw) / 2,
                    button.y + (30 - lh) / 2,
                    "OK",
                    rgb(255, 255, 255),
                    button,
                );
            }
            with(|g| {
                if let Some(c) = g.conn.as_mut() {
                    let _ = win.present(c, None);
                }
            });
            drawn = true;
        }
    }
    with(|g| {
        if let Some(c) = g.conn.as_mut() {
            let _ = win.destroy(c);
        }
    });
    result
}

extern "win64" fn MessageBoxA(owner: u64, text: *const u8, caption: *const u8, flags: u32) -> i32 {
    // SAFETY: NUL-terminated strings (or NULL).
    let (t, c) = unsafe { (path::ansi_to_string(text), path::ansi_to_string(caption)) };
    message_box(owner, t.unwrap_or_default(), c.unwrap_or_else(|| String::from("Error")), flags)
}

extern "win64" fn MessageBoxW(owner: u64, text: *const u16, caption: *const u16, flags: u32) -> i32 {
    // SAFETY: as above.
    let (t, c) = unsafe { (path::wide_to_string(text), path::wide_to_string(caption)) };
    message_box(owner, t.unwrap_or_default(), c.unwrap_or_else(|| String::from("Error")), flags)
}

// ---- window geometry, caret, queries, monitors ---------------------------------------------

/// A RECT as Windows lays it out: four LONGs.
type Rect4 = (i32, i32, i32, i32);

fn rect_at(p: *const u8) -> Rect4 {
    // SAFETY: the caller's RECT (16 bytes).
    unsafe { (teb::get(p as u64, 0), teb::get(p as u64, 4), teb::get(p as u64, 8), teb::get(p as u64, 12)) }
}

fn put_rect(p: *mut u8, r: Rect4) {
    // SAFETY: the caller's RECT (16 bytes).
    unsafe {
        teb::put(p as u64, 0, r.0);
        teb::put(p as u64, 4, r.1);
        teb::put(p as u64, 8, r.2);
        teb::put(p as u64, 12, r.3);
    }
}

fn empty(r: Rect4) -> bool {
    r.2 <= r.0 || r.3 <= r.1
}

extern "win64" fn EqualRect(a: *const u8, b: *const u8) -> Bool {
    if a.is_null() || b.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    (rect_at(a) == rect_at(b)) as Bool
}

extern "win64" fn InflateRect(p: *mut u8, dx: i32, dy: i32) -> Bool {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (l, t, r, b) = rect_at(p);
    put_rect(p, (l - dx, t - dy, r + dx, b + dy));
    TRUE
}

extern "win64" fn OffsetRect(p: *mut u8, dx: i32, dy: i32) -> Bool {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (l, t, r, b) = rect_at(p);
    put_rect(p, (l + dx, t + dy, r + dx, b + dy));
    TRUE
}

extern "win64" fn IntersectRect(dst: *mut u8, a: *const u8, b: *const u8) -> Bool {
    if dst.is_null() || a.is_null() || b.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (al, at, ar, ab) = rect_at(a);
    let (bl, bt, br, bb) = rect_at(b);
    let r = (al.max(bl), at.max(bt), ar.min(br), ab.min(bb));
    put_rect(dst, r);
    (!empty(r)) as Bool
}

extern "win64" fn IsRectEmpty(p: *const u8) -> Bool {
    if p.is_null() {
        return TRUE;
    }
    empty(rect_at(p)) as Bool
}

/// `PtInRect`: the POINT travels in one register, x in the low half.
extern "win64" fn PtInRect(p: *const u8, point: u64) -> Bool {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (l, t, r, b) = rect_at(p);
    let x = point as u32 as i32;
    let y = (point >> 32) as u32 as i32;
    (x >= l && x < r && y >= t && y < b) as Bool
}

extern "win64" fn SetRect(p: *mut u8, l: i32, t: i32, r: i32, b: i32) -> Bool {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    put_rect(p, (l, t, r, b));
    TRUE
}

extern "win64" fn SetRectEmpty(p: *mut u8) -> Bool {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    put_rect(p, (0, 0, 0, 0));
    TRUE
}

/// The caret blinks at Windows' default rate: 530 ms.
extern "win64" fn GetCaretBlinkTime() -> u32 {
    530
}

extern "win64" fn GetDoubleClickTime() -> u32 {
    500
}

/// The caret is drawn by the program into its own window; the runtime only
/// keeps Windows' bookkeeping answers.
extern "win64" fn CreateCaret(_wnd: u64, _bitmap: u64, _w: i32, _h: i32) -> Bool {
    TRUE
}

extern "win64" fn DestroyCaret() -> Bool {
    TRUE
}

extern "win64" fn ShowCaret(_wnd: u64) -> Bool {
    TRUE
}

extern "win64" fn HideCaret(_wnd: u64) -> Bool {
    TRUE
}

extern "win64" fn SetCaretPos(_x: i32, _y: i32) -> Bool {
    TRUE
}

/// One pointer, always visible: PRISM draws it from the pointer position.
extern "win64" fn ShowCursor(_show: Bool) -> i32 {
    0
}

extern "win64" fn SetCursorPos(x: i32, y: i32) -> Bool {
    with(|g| g.pointer = (x, y));
    TRUE
}

extern "win64" fn ClipCursor(_rect: *const u8) -> Bool {
    TRUE
}

extern "win64" fn GetCursorInfo(out: *mut u8) -> Bool {
    // CURSORINFO: cbSize, flags, hCursor, ptScreenPos.
    if out.is_null() || unsafe { teb::get::<u32>(out as u64, 0) } < 20 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (x, y) = with(|g| g.pointer);
    // SAFETY: the caller's CURSORINFO of at least 20 bytes.
    unsafe {
        teb::put(out as u64, 4, 1u32); // CURSOR_SHOWING
        teb::put(out as u64, 8, 0x1_0000u64); // the arrow's id, as LoadCursor hands it out
        teb::put(out as u64, 16, x);
        teb::put(out as u64, 20, y);
    }
    TRUE
}

extern "win64" fn GetMessagePos() -> u32 {
    with(|g| ((g.pointer.1 as u32) << 16) | (g.pointer.0 as u32 & 0xffff))
}

extern "win64" fn GetMessageTime() -> i32 {
    with(|g| g.now_ms() as i32)
}

extern "win64" fn GetMessageExtraInfo() -> i64 {
    0
}

extern "win64" fn GetQueueStatus(_flags: u32) -> u32 {
    0 // no posted messages waiting for a peek
}

extern "win64" fn ReplyMessage(_result: i64) -> Bool {
    TRUE
}

/// OSyx never delivers a message from another process's SendMessage in a way
/// the receiver could answer, so every thread is outside a send.
extern "win64" fn InSendMessage() -> Bool {
    FALSE
}

extern "win64" fn InSendMessageEx(_reserved: u64) -> u32 {
    0 // ISMEX_NOSEND
}

extern "win64" fn CallNextHookEx(_hook: u64, _code: i32, _w: u64, _l: i64) -> i64 {
    0
}

extern "win64" fn SetWinEventHook(_min: u32, _max: u32, _module: u64, _proc_: u64, _process: u32, _thread: u32, _flags: u32) -> u64 {
    0 // no accessibility events are raised
}

extern "win64" fn UnhookWinEvent(_hook: u64) -> Bool {
    TRUE
}

extern "win64" fn NotifyWinEvent(_event: u32, _wnd: u64, _object: i32, _child: i32) {}

extern "win64" fn SetWindowsHookExW(_id: i32, _proc_: u64, _module: u64, _thread: u32) -> u64 {
    0 // no hooks are installed or called
}

extern "win64" fn UnhookWindowsHookEx(_hook: u64) -> Bool {
    TRUE
}

extern "win64" fn GetPropW(_wnd: u64, _name: *const u16) -> u64 {
    0
}

extern "win64" fn SetPropW(_wnd: u64, _name: *const u16, _data: u64) -> Bool {
    TRUE
}

extern "win64" fn RemovePropW(_wnd: u64, _name: *const u16) -> u64 {
    0
}

/// OSyx's windows are all top-level: no window has a parent.
extern "win64" fn GetParent(_wnd: u64) -> u64 {
    0
}

extern "win64" fn GetAncestor(wnd: u64, flags: u32) -> u64 {
    match flags {
        1 | 2 => wnd, // GA_PARENT (top level) and GA_ROOT
        3 => wnd,     // GA_ROOTOWNER
        _ => 0,
    }
}

extern "win64" fn IsChild(_parent: u64, _wnd: u64) -> Bool {
    FALSE
}

extern "win64" fn GetWindow(_wnd: u64, _cmd: u32) -> u64 {
    0
}

extern "win64" fn GetTopWindow(_wnd: u64) -> u64 {
    0
}

extern "win64" fn SetParent(wnd: u64, _parent: u64) -> u64 {
    wnd
}

extern "win64" fn SetForegroundWindow(_wnd: u64) -> Bool {
    TRUE
}

extern "win64" fn GetForegroundWindow() -> u64 {
    with(|g| g.focus)
}

extern "win64" fn BringWindowToTop(_wnd: u64) -> Bool {
    TRUE
}

extern "win64" fn IsIconic(wnd: u64) -> Bool {
    (with(|g| g.windows.contains_key(&wnd))) as Bool
}

extern "win64" fn IsWindowEnabled(wnd: u64) -> Bool {
    (with(|g| g.windows.contains_key(&wnd))) as Bool
}

extern "win64" fn EnableWindow(_wnd: u64, _enable: Bool) -> Bool {
    TRUE // enabled
}

extern "win64" fn GetWindowThreadProcessId(wnd: u64, pid_out: *mut u32) -> u32 {
    if !pid_out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *pid_out = crate::kernel32::pid() as u32 };
    }
    if with(|g| g.windows.contains_key(&wnd)) { os_sys::gettid() as u32 } else { 0 }
}

extern "win64" fn GetClassNameW(wnd: u64, buf: *mut u16, size: i32) -> i32 {
    if buf.is_null() || size <= 0 {
        return 0;
    }
    let name = with(|g| g.windows.get(&wnd).and_then(|w| g.classes.get(w.class)).map(|c| c.name.clone()));
    match name {
        Some(n) => {
            let units: Vec<u16> = n.encode_utf16().collect();
            let n = units.len().min(size as usize - 1);
            // SAFETY: the caller's buffer of `size` units.
            unsafe {
                core::ptr::copy_nonoverlapping(units.as_ptr(), buf, n);
                *buf.add(n) = 0;
            }
            n as i32
        }
        None => 0,
    }
}

// ---- monitors ------------------------------------------------------------------------------

fn screen_rect() -> Rect4 {
    let (w, h) = with(|g| g.conn().map(|c| (c.screen_width as i32, c.screen_height as i32)).unwrap_or((1024, 768)));
    (0, 0, w, h)
}

/// MONITOR_DEFAULTTONULL is 0: a rectangle that touches nothing then has no
/// monitor. OSyx has one screen, so everything else belongs to it.
fn monitor_from(r: Rect4, flags: u32) -> u64 {
    if !empty(r) {
        let s = screen_rect();
        if r.0 < s.2 && r.2 > s.0 && r.1 < s.3 && r.3 > s.1 {
            return crate::minordlls::PRIMARY_MONITOR;
        }
    }
    if flags == 0 {
        0
    } else {
        crate::minordlls::PRIMARY_MONITOR
    }
}

extern "win64" fn MonitorFromPoint(point: u64, flags: u32) -> u64 {
    let x = point as u32 as i32;
    let y = (point >> 32) as u32 as i32;
    monitor_from((x, y, x + 1, y + 1), flags)
}

extern "win64" fn MonitorFromRect(rect: *const u8, flags: u32) -> u64 {
    if rect.is_null() {
        return crate::minordlls::PRIMARY_MONITOR;
    }
    monitor_from(rect_at(rect), flags)
}

extern "win64" fn MonitorFromWindow(wnd: u64, flags: u32) -> u64 {
    let r = with(|g| g.windows.get(&wnd).map(|w| (w.pos.0, w.pos.1, w.pos.0 + w.size.0 as i32, w.pos.1 + w.size.1 as i32)));
    match r {
        Some(r) => monitor_from(r, flags),
        None => crate::minordlls::PRIMARY_MONITOR,
    }
}

extern "win64" fn GetMonitorInfoW(monitor: u64, out: *mut u8) -> Bool {
    if monitor != crate::minordlls::PRIMARY_MONITOR || out.is_null() || unsafe { teb::get::<u32>(out as u64, 0) } < 40 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let s = screen_rect();
    // MONITORINFO: cbSize, rcMonitor, rcWork, dwFlags.
    // SAFETY: the caller's MONITORINFO of at least 40 bytes.
    unsafe {
        put_rect(out.add(4), s);
        put_rect(out.add(20), s);
        teb::put(out as u64, 36, 1u32); // MONITORINFOF_PRIMARY
    }
    TRUE
}

extern "win64" fn GetMonitorInfoA(monitor: u64, out: *mut u8) -> Bool {
    GetMonitorInfoW(monitor, out)
}

/// Messages a program registers by name: the same name gets the same id for
/// the life of the process, in the range Windows uses for them.
static REGISTERED_MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());

extern "win64" fn RegisterWindowMessageW(name: *const u16) -> u32 {
    const BASE: u32 = 0xC000;
    if name.is_null() {
        return 0;
    }
    let mut text = String::new();
    let mut i = 0;
    // SAFETY: a NUL-terminated string from the caller.
    loop {
        let c = unsafe { *name.add(i) };
        if c == 0 {
            break;
        }
        text.push(c as u8 as char);
        i += 1;
    }
    let mut list = REGISTERED_MESSAGES.lock().unwrap();
    match list.iter().position(|n| n.eq_ignore_ascii_case(&text)) {
        Some(i) => BASE + i as u32,
        None => {
            list.push(text);
            BASE + (list.len() - 1) as u32
        }
    }
}

// ---- window classes, enumeration and the small window API ----------------------------------

fn wide(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut s = String::new();
    let mut i = 0;
    // SAFETY: a NUL-terminated string from the caller.
    loop {
        let c = unsafe { *p.add(i) };
        if c == 0 {
            return Some(s);
        }
        s.push(char::from_u32(c as u32).unwrap_or('\u{fffd}'));
        i += 1;
    }
}

extern "win64" fn GetClassInfoW(_inst: u64, name: *const u16, out: *mut u8) -> Bool {
    let Some(want) = wide(name) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let found = with(|g| {
        g.classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&want))
            .map(|c| (c.wndproc, c.background, c.style))
    });
    let Some((wndproc, background, style)) = found else { return fail(ERROR_CANNOT_FIND_WND_CLASS, FALSE) };
    // WNDCLASSW (72 bytes): style, wndproc, cbClsExtra, cbWndExtra, instance,
    // icon, cursor, background, menu name, class name. The string pointers are
    // left null: the names live in this runtime, not in the caller's memory.
    // SAFETY: the caller's WNDCLASSW.
    unsafe {
        core::ptr::write_bytes(out, 0, 72);
        teb::put(out as u64, 0, style);
        teb::put(out as u64, 8, wndproc);
        teb::put(out as u64, 48, background);
    }
    TRUE
}

extern "win64" fn GetClassInfoA(inst: u64, name: *const u8, out: *mut u8) -> Bool {
    // The class names are ASCII in practice; widen the caller's name.
    if name.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let mut units = Vec::new();
    let mut i = 0;
    // SAFETY: a NUL-terminated string from the caller.
    loop {
        let c = unsafe { *name.add(i) };
        units.push(c as u16);
        if c == 0 {
            break;
        }
        i += 1;
    }
    GetClassInfoW(inst, units.as_ptr(), out)
}

extern "win64" fn UnregisterClassW(name: *const u16, _inst: u64) -> Bool {
    if wide(name).is_none() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    TRUE // the class table lives as long as the process
}

extern "win64" fn GetClassLongPtrW(_wnd: u64, _index: i32) -> u64 {
    0
}

extern "win64" fn SetClassLongPtrW(_wnd: u64, _index: i32, _value: u64) -> u64 {
    0
}

extern "win64" fn GetClassWord(_wnd: u64, _index: i32) -> u16 {
    0
}

extern "win64" fn FindWindowW(class: *const u16, title: *const u16) -> u64 {
    let (c, t) = (wide(class), wide(title));
    with(|g| {
        g.windows
            .iter()
            .find(|(_, w)| {
                let class_ok = c.as_ref().is_none_or(|want| g.classes.get(w.class).is_some_and(|k| k.name.eq_ignore_ascii_case(want)));
                let title_ok = t.as_ref().is_none_or(|want| w.title == *want);
                class_ok && title_ok
            })
            .map(|(h, _)| *h)
            .unwrap_or(0)
    })
}

extern "win64" fn FindWindowExW(_parent: u64, _after: u64, class: *const u16, title: *const u16) -> u64 {
    FindWindowW(class, title)
}

extern "win64" fn EnumWindows(proc_: u64, param: u64) -> Bool {
    if proc_ == 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let list: Vec<u64> = with(|g| g.windows.keys().copied().collect());
    // SAFETY: a caller-supplied WNDENUMPROC of the Windows x64 ABI.
    let f: extern "win64" fn(u64, i64) -> i32 = unsafe { core::mem::transmute(proc_ as usize) };
    for h in list {
        if f(h, param as i64) == 0 {
            break;
        }
    }
    TRUE
}

extern "win64" fn GetUpdateRect(_wnd: u64, rect: *mut u8, _erase: Bool) -> Bool {
    if !rect.is_null() {
        put_rect(rect, (0, 0, 0, 0));
    }
    FALSE // nothing is waiting to be painted outside WM_PAINT
}

extern "win64" fn RedrawWindow(wnd: u64, _rect: *const u8, _rgn: u64, flags: u32) -> Bool {
    if flags & 0x1 != 0 {
        // RDW_INVALIDATE: queue a paint for the whole window.
        with(|g| {
            if let Some(w) = g.windows.get_mut(&wnd) {
                w.invalid = Some(Rect { x: 0, y: 0, w: w.size.0, h: w.size.1 });
            }
        });
    }
    TRUE
}

extern "win64" fn AnimateWindow(_wnd: u64, _time: u32, _flags: u32) -> Bool {
    TRUE // no animation: the window is simply shown
}

extern "win64" fn FlashWindowEx(_info: *const u8) -> Bool {
    FALSE
}

extern "win64" fn GetMenu(_wnd: u64) -> u64 {
    0 // no window here has a menu bar
}

extern "win64" fn GetSystemMenu(_wnd: u64, _revert: Bool) -> u64 {
    0
}

extern "win64" fn SetMenuItemInfoW(_menu: u64, _item: u32, _by_pos: Bool, _info: *const u8) -> Bool {
    FALSE
}

extern "win64" fn TrackPopupMenu(_menu: u64, _flags: u32, _x: i32, _y: i32, _reserved: i32, _wnd: u64, _rect: *const u8) -> Bool {
    FALSE
}

extern "win64" fn GetWindowTextLengthW(wnd: u64) -> i32 {
    with(|g| g.windows.get(&wnd).map(|w| w.title.encode_utf16().count() as i32).unwrap_or(0))
}

extern "win64" fn GetWindowPlacement(wnd: u64, out: *mut u8) -> Bool {
    if out.is_null() || unsafe { teb::get::<u32>(out as u64, 0) } < 44 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (pos, size, visible) = with(|g| {
        g.windows.get(&wnd).map(|w| (w.pos, (w.size.0 as i32, w.size.1 as i32), w.visible)).unwrap_or(((0, 0), (0, 0), false))
    });
    // WINDOWPLACEMENT: length, flags, showCmd, ptMin, ptMax, rcNormal.
    // SAFETY: the caller's WINDOWPLACEMENT of at least 44 bytes.
    unsafe {
        teb::put(out as u64, 8, if visible { 1u32 } else { 7u32 }); // SW_SHOWNORMAL / SW_SHOWMINNOACTIVE
        put_rect(out.add(28), (pos.0, pos.1, pos.0 + size.0, pos.1 + size.1));
    }
    TRUE
}

extern "win64" fn SetWindowPlacement(wnd: u64, place: *const u8) -> Bool {
    if place.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let r = rect_at(unsafe { place.add(28) });
    with(|g| {
        if let Some(w) = g.windows.get_mut(&wnd) {
            w.pos = (r.0, r.1);
            w.size = ((r.2 - r.0).max(0) as u32, (r.3 - r.1).max(0) as u32);
        }
    });
    TRUE
}

extern "win64" fn CharLowerW(s: *mut u16) -> *mut u16 {
    if s.is_null() {
        return s;
    }
    let mut i = 0;
    // SAFETY: a NUL-terminated string from the caller, modified in place.
    unsafe {
        while *s.add(i) != 0 {
            let c = *s.add(i);
            if (0x41..=0x5a).contains(&c) {
                *s.add(i) = c + 0x20;
            }
            i += 1;
        }
    }
    s
}

extern "win64" fn IsGUIThread(_convert: Bool) -> Bool {
    TRUE // winrun's threads are user-interface threads
}

extern "win64" fn GetGUIThreadInfo(_thread: u32, out: *mut u8) -> Bool {
    if out.is_null() || unsafe { teb::get::<u32>(out as u64, 0) } < 72 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let focus = with(|g| g.focus);
    // GUITHREADINFO: cbSize, flags, hwndActive, hwndFocus, hwndCapture,
    // hwndMenuOwner, hwndMoveSize, hwndCaret, rcCaret.
    // SAFETY: the caller's GUITHREADINFO of at least 72 bytes.
    unsafe {
        core::ptr::write_bytes(out, 0, 72);
        teb::put(out as u64, 8, focus);
        teb::put(out as u64, 16, focus);
    }
    TRUE
}

extern "win64" fn SetSysColors(_count: i32, _elements: *const i32, _colors: *const u32) -> Bool {
    TRUE // the desktop's palette is fixed
}

extern "win64" fn OpenInputDesktop(_flags: u32, _inherit: Bool, _access: u32) -> u64 {
    0x20_0001 // one input desktop, always open
}

extern "win64" fn SetThreadDesktop(_desktop: u64) -> Bool {
    TRUE
}

/// Device and power notifications are accepted and never delivered: OSyx
/// reports no device arrival and has no power events to send.
extern "win64" fn RegisterDeviceNotificationW(_recipient: u64, _filter: *const u8, _flags: u32) -> u64 {
    0x30_0001
}

extern "win64" fn UnregisterDeviceNotification(_notify: u64) -> Bool {
    TRUE
}

extern "win64" fn RegisterPowerSettingNotification(_recipient: u64, _guid: *const u8, _flags: u32) -> u64 {
    0x30_0002
}

extern "win64" fn UnregisterPowerSettingNotification(_notify: u64) -> Bool {
    TRUE
}

extern "win64" fn SendNotifyMessageW(wnd: u64, msg: u32, wparam: u64, lparam: i64) -> Bool {
    with(|g| g.post(wnd, msg, wparam, lparam));
    TRUE
}

/// `SendMessageTimeoutW`: the message is delivered directly and the answer
/// written out; nothing here can block for long enough to hit the timeout.
extern "win64" fn SendMessageTimeoutW(wnd: u64, msg: u32, wparam: u64, lparam: i64, _flags: u32, _timeout: u32, out: *mut u64) -> i64 {
    let result = match wndproc_of(wnd) {
        Some(p) => call(p, wnd, msg, wparam, lparam),
        None => 0,
    };
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *out = result as u64 };
    }
    1
}

extern "win64" fn CallWindowProcW(proc_: u64, wnd: u64, msg: u32, wparam: u64, lparam: i64) -> i64 {
    if proc_ == 0 {
        return 0;
    }
    // SAFETY: a caller-supplied window procedure of the Windows x64 ABI.
    let f: extern "win64" fn(u64, u32, u64, i64) -> i64 = unsafe { core::mem::transmute(proc_ as usize) };
    f(wnd, msg, wparam, lparam)
}

/// OSyx has one screen described by MonitorFromWindow/GetMonitorInfo; the
/// DisplayConfig path model (paths, sources, targets, modes) is not built, so
/// these report ERROR_NOT_SUPPORTED and callers fall back to the monitor API.
const ERROR_NOT_SUPPORTED_WIN: u32 = 50;

extern "win64" fn GetDisplayConfigBufferSizes(_flags: u32, paths: *mut u32, modes: *mut u32) -> u32 {
    if paths.is_null() || modes.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the caller's out pointers.
    unsafe {
        *paths = 0;
        *modes = 0;
    }
    ERROR_NOT_SUPPORTED_WIN
}

extern "win64" fn QueryDisplayConfig(_flags: u32, paths: *mut u32, _path: *mut u8, modes: *mut u32, _mode: *mut u8, _topology: *mut u32) -> u32 {
    if paths.is_null() || modes.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    ERROR_NOT_SUPPORTED_WIN
}

extern "win64" fn DisplayConfigGetDeviceInfo(_packet: *mut u8) -> u32 {
    ERROR_NOT_SUPPORTED_WIN
}

extern "win64" fn EnumDisplayMonitors(_dc: u64, _clip: *const u8, proc_: u64, data: u64) -> Bool {
    if proc_ == 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let r = screen_rect();
    let mut rect = [0i32; 4];
    rect.copy_from_slice(&[r.0, r.1, r.2, r.3]);
    // SAFETY: a caller-supplied MONITORENUMPROC of the Windows x64 ABI.
    let f: extern "win64" fn(u64, u64, *const u8, i64) -> i32 = unsafe { core::mem::transmute(proc_ as usize) };
    f(crate::minordlls::PRIMARY_MONITOR, _dc, rect.as_ptr() as *const u8, data as i64);
    TRUE
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        RegisterClassA,
        RegisterClassW,
        RegisterClassExA,
        RegisterClassExW,
        UnregisterClassA,
        CreateWindowExA,
        CreateWindowExW,
        DestroyWindow,
        IsWindow,
        IsWindowVisible,
        ShowWindow,
        UpdateWindow,
        InvalidateRect,
        ValidateRect,
        GetClientRect,
        GetWindowRect,
        AdjustWindowRect,
        AdjustWindowRectEx,
        SetWindowPos,
        MoveWindow,
        SetWindowTextA,
        SetWindowTextW,
        GetWindowTextA,
        GetWindowTextW,
        GetWindowTextLengthA,
        GetWindowLongPtrA,
        SetWindowLongPtrA,
        GetWindowLongA,
        SetWindowLongA,
        GetMessageA,
        GetMessageW,
        PeekMessageA,
        PeekMessageW,
        TranslateMessage,
        DispatchMessageA,
        DispatchMessageW,
        SendMessageA,
        SendMessageW,
        PostMessageA,
        PostMessageW,
        PostQuitMessage,
        DefWindowProcA,
        DefWindowProcW,
        SetTimer,
        KillTimer,
        GetFocus,
        SetFocus,
        GetActiveWindow,
        SetCapture,
        ReleaseCapture,
        GetKeyState,
        GetAsyncKeyState,
        GetCursorPos,
        ScreenToClient,
        ClientToScreen,
        GetSystemMetrics,
        SystemParametersInfoA,
        SystemParametersInfoW,
        EqualRect,
        InflateRect,
        OffsetRect,
        IntersectRect,
        IsRectEmpty,
        PtInRect,
        SetRect,
        SetRectEmpty,
        GetCaretBlinkTime,
        GetDoubleClickTime,
        CreateCaret,
        DestroyCaret,
        ShowCaret,
        HideCaret,
        SetCaretPos,
        ShowCursor,
        SetCursorPos,
        ClipCursor,
        GetCursorInfo,
        GetMessagePos,
        GetMessageTime,
        GetMessageExtraInfo,
        GetQueueStatus,
        ReplyMessage,
        InSendMessage,
        InSendMessageEx,
        CallNextHookEx,
        SetWinEventHook,
        UnhookWinEvent,
        NotifyWinEvent,
        SetWindowsHookExW,
        UnhookWindowsHookEx,
        GetPropW,
        SetPropW,
        RemovePropW,
        GetParent,
        GetAncestor,
        IsChild,
        GetWindow,
        GetTopWindow,
        SetParent,
        SetForegroundWindow,
        GetForegroundWindow,
        BringWindowToTop,
        IsIconic,
        IsWindowEnabled,
        EnableWindow,
        GetWindowThreadProcessId,
        GetClassNameW,
        MonitorFromPoint,
        MonitorFromRect,
        MonitorFromWindow,
        GetMonitorInfoW,
        GetMonitorInfoA,
        RegisterWindowMessageW,
        GetClassInfoW,
        GetClassInfoA,
        UnregisterClassW,
        GetClassLongPtrW,
        SetClassLongPtrW,
        GetClassWord,
        FindWindowW,
        FindWindowExW,
        EnumWindows,
        GetUpdateRect,
        RedrawWindow,
        AnimateWindow,
        FlashWindowEx,
        GetMenu,
        GetSystemMenu,
        SetMenuItemInfoW,
        TrackPopupMenu,
        GetWindowTextLengthW,
        GetWindowPlacement,
        SetWindowPlacement,
        CharLowerW,
        IsGUIThread,
        GetGUIThreadInfo,
        SetSysColors,
        OpenInputDesktop,
        SetThreadDesktop,
        RegisterDeviceNotificationW,
        UnregisterDeviceNotification,
        RegisterPowerSettingNotification,
        UnregisterPowerSettingNotification,
        SendNotifyMessageW,
        SendMessageTimeoutW,
        CallWindowProcW,
        GetDisplayConfigBufferSizes,
        QueryDisplayConfig,
        DisplayConfigGetDeviceInfo,
        EnumDisplayMonitors,
        LoadCursorA,
        LoadCursorW,
        LoadIconA,
        LoadIconW,
        SetCursor,
        GetSysColor,
        GetSysColorBrush,
        MessageBeep,
        BeginPaint,
        EndPaint,
        GetDC,
        ReleaseDC,
        FillRect,
        FrameRect,
        DrawTextA,
        DrawTextW,
        MessageBoxA,
        MessageBoxW,
    ]
    .into_iter()
    .chain([
        ("GetWindowLongPtrW", GetWindowLongPtrA as *const () as usize as u64),
        ("SetWindowLongPtrW", SetWindowLongPtrA as *const () as usize as u64),
        ("GetWindowLongW", GetWindowLongA as *const () as usize as u64),
        ("SetWindowLongW", SetWindowLongA as *const () as usize as u64),
    ])
    .collect()
}

// ---- device contexts -----------------------------------------------------------------------

/// `GetDeviceCaps`: OSyx's screens are 32-bit colour at 96 DPI, which is the
/// one configuration the software compositor draws.
extern "win64" fn GetDeviceCaps(_dc: u64, index: i32) -> i32 {
    let (w, h) = {
        let r = screen_rect();
        (r.2 - r.0, r.3 - r.1)
    };
    match index {
        2 => 1,                       // TECHNOLOGY: DT_RASDISPLAY
        4 => w * 254 / 960,           // HORZSIZE, from 96 DPI
        6 => h * 254 / 960,           // VERTSIZE
        8 => w,                       // HORZRES
        10 => h,                      // VERTRES
        12 => 32,                     // BITSPIXEL
        14 => 1,                      // PLANES
        24 => -1,                     // NUMCOLORS: more than 256 (true colour)
        38 => 0x0001 | 0x0100,        // RASTERCAPS: RC_BITBLT | RC_BITMAP64
        40 | 42 => 36,                // ASPECTX / ASPECTY
        88 | 90 => 96,                // LOGPIXELSX / LOGPIXELSY
        104 => 0,                     // SIZEPALETTE
        110 => w,                     // PHYSICALWIDTH
        111 => h,                     // PHYSICALHEIGHT
        112 | 113 => 0,               // PHYSICALOFFSETX / Y
        116 => 60,                    // VREFRESH
        117 => h,                     // DESKTOPVERTRES
        118 => w,                     // DESKTOPHORZRES
        _ => 0,
    }
}

extern "win64" fn GdiFlush() -> Bool {
    TRUE // drawing reaches PRISM in order already
}

/// Each save returns a new level; restore is accepted for any level, since
/// the device contexts here keep no nested state to go back to.
extern "win64" fn SaveDC(_dc: u64) -> i32 {
    with(|g| {
        g.next += 1;
        (g.next & 0x7fff_ffff) as i32
    })
}

extern "win64" fn RestoreDC(_dc: u64, _level: i32) -> Bool {
    TRUE
}

extern "win64" fn SetMapMode(_dc: u64, _mode: i32) -> i32 {
    1 // MM_TEXT
}

extern "win64" fn SetTextAlign(_dc: u64, _align: u32) -> u32 {
    0 // TA_LEFT | TA_TOP
}

extern "win64" fn SetPolyFillMode(_dc: u64, _mode: i32) -> i32 {
    1 // ALTERNATE
}

extern "win64" fn SetStretchBltMode(_dc: u64, _mode: i32) -> i32 {
    1 // BLACKONWHITE
}

extern "win64" fn SetMiterLimit(_dc: u64, _limit: f32, old: *mut f32) -> Bool {
    if !old.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *old = 10.0 };
    }
    TRUE
}

extern "win64" fn GetGraphicsMode(_dc: u64) -> i32 {
    1 // GM_COMPATIBLE
}

extern "win64" fn SetGraphicsMode(_dc: u64, _mode: i32) -> i32 {
    1
}

extern "win64" fn GetCurrentObject(dc: u64, kind: u32) -> u64 {
    with(|g| g.dcs.get(&dc).map(|d| match kind {
        2 | 8 => d.pen,  // OBJ_PEN, OBJ_EXTPEN
        4 | 5 => d.font, // OBJ_FONT, OBJ_TEXT
        _ => d.brush,
    }).unwrap_or(0))
}

extern "win64" fn GetClipBox(dc: u64, out: *mut u8) -> i32 {
    if out.is_null() {
        return 0;
    }
    let r = with(|g| g.dcs.get(&dc).map(|d| (d.paint.x, d.paint.y, d.paint.x + d.paint.w as i32, d.paint.y + d.paint.h as i32)));
    put_rect(out, r.unwrap_or_else(screen_rect));
    2 // COMPLEXREGION
}

extern "win64" fn IntersectClipRect(_dc: u64, _l: i32, _t: i32, _r: i32, _b: i32) -> i32 {
    2 // a region, which is what Windows returns for a successful clip
}

/// Printing goes nowhere: there is no spooler, but a program that opens a
/// job and ends it sees the calls succeed rather than fail mid-print.
extern "win64" fn StartDocW(_dc: u64, _doc: *const u8) -> i32 {
    1
}

extern "win64" fn EndDoc(_dc: u64) -> i32 {
    1
}

extern "win64" fn StartPage(_dc: u64) -> i32 {
    1
}

extern "win64" fn EndPage(_dc: u64) -> i32 {
    1
}

extern "win64" fn AbortDoc(_dc: u64) -> i32 {
    1
}

pub fn gdi_exports() -> Vec<(&'static str, u64)> {
    export_table![
        TextOutA,
        TextOutW,
        GetTextExtentPoint32A,
        GetTextExtentPoint32W,
        GetTextMetricsA,
        GetTextMetricsW,
        SetTextColor,
        SetBkColor,
        SetBkMode,
        CreateSolidBrush,
        CreatePen,
        CreateFontA,
        CreateFontW,
        CreateFontIndirectW,
        GetStockObject,
        SelectObject,
        DeleteObject,
        MoveToEx,
        LineTo,
        Rectangle,
        RoundRect,
        SetPixel,
        GetDeviceCaps,
        GdiFlush,
        SaveDC,
        RestoreDC,
        SetMapMode,
        SetTextAlign,
        SetPolyFillMode,
        SetStretchBltMode,
        SetMiterLimit,
        GetGraphicsMode,
        SetGraphicsMode,
        GetCurrentObject,
        GetClipBox,
        IntersectClipRect,
        StartDocW,
        EndDoc,
        StartPage,
        EndPage,
        AbortDoc,
    ]
}
