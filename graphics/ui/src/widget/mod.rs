//! The widgets. Each is a plain struct with a `rect` (surface
//! coordinates) set by the application's layout code, an `event` method
//! returning a [`Response`](crate::Response) and a `draw` method.

mod button;
mod dialog;
mod list;
mod menu;
mod scroll;
mod text_field;

pub use button::{Button, ButtonStyle, Checkbox};
pub use dialog::Dialog;
pub use list::{Column, ListItem, ListView};
pub use menu::{Menu, MenuItem};
pub use scroll::{Scroll, ScrollView};
pub use text_field::TextField;

use crate::paint::{Align, FontKind, Fonts, Painter};
use alloc::string::String;
use alloc::vec::Vec;
use osyx_gfx::{Color, Rect};

/// Static text.
#[derive(Clone, Debug)]
pub struct Label {
    pub rect: Rect,
    pub text: String,
    pub font: FontKind,
    /// `None`: the theme's text colour.
    pub color: Option<Color>,
    pub align: Align,
}

impl Label {
    pub fn new(text: &str, font: FontKind) -> Label {
        Label { rect: Rect::EMPTY, text: String::from(text), font, color: None, align: Align::Left }
    }

    pub fn draw(&self, p: &mut Painter) {
        let c = self.color.unwrap_or(p.theme.text);
        p.text_in(self.font, self.rect, &self.text, c, self.align);
    }
}

/// A card that groups other widgets, with an optional heading.
#[derive(Clone, Debug)]
pub struct Panel {
    pub rect: Rect,
    pub title: Option<String>,
}

impl Panel {
    pub fn new(title: Option<&str>) -> Panel {
        Panel { rect: Rect::EMPTY, title: title.map(String::from) }
    }

    /// Where the panel's children go.
    pub fn content(&self, fonts: &Fonts) -> Rect {
        let pad = 14;
        let top = if self.title.is_some() { fonts.line_height(FontKind::Bold) + 10 } else { 0 };
        let r = crate::layout::inset(self.rect, pad);
        Rect::new(r.x, r.y + top as i32, r.w, r.h.saturating_sub(top))
    }

    pub fn draw(&self, p: &mut Painter) {
        let t = *p.theme;
        p.card(self.rect, t.radius, t.surface, t.border);
        if let Some(title) = &self.title {
            let r = crate::layout::inset(self.rect, 14);
            let h = p.fonts.line_height(FontKind::Bold);
            p.text_in(FontKind::Bold, Rect::new(r.x, r.y, r.w, h), title, t.text_dim, Align::Left);
        }
    }
}

/// Breaks `text` into lines no wider than `width` pixels (at spaces where
/// possible; explicit newlines are kept).
pub fn wrap_text(fonts: &Fonts, font: FontKind, text: &str, width: u32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { String::from(word) } else { alloc::format!("{line} {word}") };
            if fonts.measure(font, &candidate) <= width || line.is_empty() {
                line = candidate;
                // A single word wider than the line is cut by characters.
                while fonts.measure(font, &line) > width && line.chars().count() > 1 {
                    let mut cut = String::new();
                    for ch in line.chars() {
                        let mut next = cut.clone();
                        next.push(ch);
                        if fonts.measure(font, &next) > width && !cut.is_empty() {
                            break;
                        }
                        cut = next;
                    }
                    let rest = String::from(&line[cut.len()..]);
                    lines.push(cut);
                    line = rest;
                }
            } else {
                lines.push(core::mem::take(&mut line));
                line = String::from(word);
            }
        }
        lines.push(line);
    }
    lines
}
