//! Input as widgets see it: pointer positions in surface coordinates, keys
//! with the character the keyboard layout produced (PRISM translates them).

use osyx_input::modifier;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// HID usage (`osyx_input::key`).
    pub key: u16,
    /// The character the layout produced (`None` for releases and keys
    /// without text).
    pub text: Option<char>,
    /// `osyx_input::modifier` bits.
    pub mods: u32,
    pub down: bool,
    pub repeat: bool,
}

impl KeyEvent {
    pub fn pressed(&self, key: u16) -> bool {
        self.down && self.key == key
    }

    pub fn ctrl(&self) -> bool {
        self.mods & modifier::CTRL != 0
    }

    pub fn shift(&self) -> bool {
        self.mods & modifier::SHIFT != 0
    }

    pub fn alt(&self) -> bool {
        self.mods & modifier::LEFT_ALT != 0
    }

    /// Text to insert: printable characters typed without Ctrl or Alt
    /// (AltGr combinations do produce text).
    pub fn insertable(&self) -> Option<char> {
        let c = self.text?;
        if !self.down || c.is_control() || self.ctrl() || self.alt() {
            return None;
        }
        Some(c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PointerMove { x: i32, y: i32 },
    /// `clicks`: 1 for a single click, 2 for a double click, ...
    PointerDown { x: i32, y: i32, button: u16, clicks: u8 },
    PointerUp { x: i32, y: i32, button: u16 },
    PointerLeave,
    /// Wheel detents: `dy` > 0 scrolls up (away from the user).
    Wheel { x: i32, y: i32, dy: i32 },
    Key(KeyEvent),
}

impl Event {
    pub fn position(&self) -> Option<(i32, i32)> {
        match *self {
            Event::PointerMove { x, y } | Event::PointerDown { x, y, .. } | Event::PointerUp { x, y, .. } | Event::Wheel { x, y, .. } => {
                Some((x, y))
            }
            _ => None,
        }
    }
}

/// What a widget made of an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    /// Not for this widget.
    Ignored,
    /// Consumed; nothing visible changed.
    Consumed,
    /// Consumed and the widget must be redrawn.
    Redraw,
    /// A button was clicked or a row / menu item activated.
    Activated(usize),
    /// The value changed (text edited, selection moved, box toggled).
    Changed,
    /// Enter in a text field.
    Submitted,
    /// Escape, or a click outside a menu.
    Cancelled,
}

impl Response {
    pub fn needs_redraw(&self) -> bool {
        !matches!(self, Response::Ignored | Response::Consumed)
    }

    pub fn handled(&self) -> bool {
        !matches!(self, Response::Ignored)
    }
}

/// Turns presses into click counts (double click: same button, within
/// `interval_ms` and `slop` pixels of the previous press).
#[derive(Clone, Copy, Debug)]
pub struct ClickCounter {
    last: Option<(u64, i32, i32, u16)>,
    count: u8,
    pub interval_ms: u64,
    pub slop: i32,
}

impl Default for ClickCounter {
    fn default() -> Self {
        ClickCounter { last: None, count: 0, interval_ms: 400, slop: 4 }
    }
}

impl ClickCounter {
    pub fn press(&mut self, now_ms: u64, x: i32, y: i32, button: u16) -> u8 {
        let repeat = self.last.is_some_and(|(t, lx, ly, b)| {
            b == button && now_ms.saturating_sub(t) <= self.interval_ms && (x - lx).abs() <= self.slop && (y - ly).abs() <= self.slop
        });
        self.count = if repeat { self.count.saturating_add(1) } else { 1 };
        self.last = Some((now_ms, x, y, button));
        self.count
    }
}
