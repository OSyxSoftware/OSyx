use crate::event::{Event, Response};
use crate::icon::{self, Icon};
use crate::layout::contains;
use crate::paint::{Align, FontKind, Painter};
use alloc::string::String;
use osyx_gfx::Rect;
use osyx_input::{button, key};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Filled with the accent colour: the default action.
    Primary,
    /// Raised neutral button.
    Secondary,
    /// Destructive action.
    Danger,
    /// No background until hovered (tool bars, task bars).
    Flat,
}

#[derive(Clone, Debug)]
pub struct Button {
    pub rect: Rect,
    pub label: String,
    pub icon: Option<Icon>,
    pub style: ButtonStyle,
    pub enabled: bool,
    /// Reported in `Response::Activated`.
    pub id: usize,
    /// Drawn highlighted (e.g. the active window's task bar button).
    pub active: bool,
    hovered: bool,
    pressed: bool,
}

impl Button {
    pub fn new(id: usize, label: &str, style: ButtonStyle) -> Button {
        Button {
            rect: Rect::EMPTY,
            label: String::from(label),
            icon: None,
            style,
            enabled: true,
            id,
            active: false,
            hovered: false,
            pressed: false,
        }
    }

    pub fn with_icon(mut self, icon: Icon) -> Button {
        self.icon = Some(icon);
        self
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Width that fits the label and icon.
    pub fn preferred_width(&self, fonts: &crate::Fonts) -> u32 {
        let text = if self.label.is_empty() { 0 } else { fonts.measure(FontKind::Ui, &self.label) };
        let icon = if self.icon.is_some() { 18 + if text > 0 { 8 } else { 0 } } else { 0 };
        text + icon + 28
    }

    pub fn event(&mut self, ev: &Event, focused: bool) -> Response {
        if !self.enabled {
            let was = self.hovered || self.pressed;
            self.hovered = false;
            self.pressed = false;
            return if was { Response::Redraw } else { Response::Ignored };
        }
        match *ev {
            Event::PointerMove { x, y } => {
                let h = contains(&self.rect, x, y);
                if h != self.hovered {
                    self.hovered = h;
                    return Response::Redraw;
                }
                Response::Ignored
            }
            Event::PointerLeave => {
                let was = self.hovered || self.pressed;
                self.hovered = false;
                self.pressed = false;
                if was {
                    Response::Redraw
                } else {
                    Response::Ignored
                }
            }
            Event::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&self.rect, x, y) => {
                self.pressed = true;
                Response::Redraw
            }
            Event::PointerUp { x, y, button: b } if b == button::LEFT && self.pressed => {
                self.pressed = false;
                if contains(&self.rect, x, y) {
                    Response::Activated(self.id)
                } else {
                    Response::Redraw
                }
            }
            Event::Key(k) if focused && k.down && !k.repeat && (k.key == key::ENTER || k.key == key::SPACE || k.key == key::KP_ENTER) => {
                Response::Activated(self.id)
            }
            _ => Response::Ignored,
        }
    }

    pub fn draw(&self, p: &mut Painter, focused: bool) {
        let t = *p.theme;
        let r = self.rect;
        let (bg, fg, border) = match self.style {
            ButtonStyle::Primary => {
                let bg = if self.pressed {
                    t.accent_pressed
                } else if self.hovered {
                    t.accent_hover
                } else {
                    t.accent
                };
                (Some(bg), t.on_accent, None)
            }
            ButtonStyle::Danger => (Some(if self.hovered || self.pressed { t.danger_hover } else { t.danger }), t.on_accent, None),
            ButtonStyle::Secondary => {
                let bg = if self.pressed {
                    t.border
                } else if self.hovered {
                    osyx_gfx::lerp(t.raised, t.border, 128)
                } else {
                    t.raised
                };
                (Some(bg), t.text, Some(t.border))
            }
            ButtonStyle::Flat => {
                let bg = if self.pressed {
                    Some(t.border)
                } else if self.hovered {
                    Some(t.raised)
                } else if self.active {
                    Some(osyx_gfx::lerp(t.surface, t.raised, 160))
                } else {
                    None
                };
                (bg, t.text, None)
            }
        };
        let fg = if self.enabled { fg } else { t.text_faint };
        match (bg, border) {
            (Some(bg), Some(b)) => p.card(r, t.radius_small, bg, b),
            (Some(bg), None) => p.rounded(r, t.radius_small, bg, if self.enabled { 255 } else { 120 }),
            _ => {}
        }
        let text_w = if self.label.is_empty() { 0 } else { p.fonts.measure(FontKind::Ui, &self.label) };
        let icon_size = 18u32.min(r.h.saturating_sub(8));
        let gap = if self.icon.is_some() && text_w > 0 { 8 } else { 0 };
        let content_w = text_w + gap + if self.icon.is_some() { icon_size } else { 0 };
        let mut x = r.x + (r.w as i32 - content_w as i32).max(12) / 2;
        if content_w + 16 > r.w {
            x = r.x + 8;
        }
        if let Some(i) = self.icon {
            let ir = Rect::new(x, r.y + (r.h as i32 - icon_size as i32) / 2, icon_size, icon_size);
            icon::draw(p, i, ir, fg);
            x += (icon_size + gap) as i32;
        }
        if text_w > 0 {
            let avail = (r.right() as i32 - x - 6).max(0) as u32;
            p.text_in(FontKind::Ui, Rect::new(x, r.y, avail, r.h), &self.label, fg, Align::Left);
        }
        if self.active && self.style == ButtonStyle::Flat {
            // Active indicator: a short accent bar under the content.
            let w = (r.w / 3).max(12);
            p.rounded(Rect::new(r.x + (r.w - w) as i32 / 2, r.bottom() as i32 - 3, w, 3), 1, t.accent, 255);
        }
        if focused {
            p.focus_ring(r, t.radius_small);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Checkbox {
    pub rect: Rect,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
    hovered: bool,
    pressed: bool,
}

impl Checkbox {
    pub fn new(label: &str, checked: bool) -> Checkbox {
        Checkbox { rect: Rect::EMPTY, label: String::from(label), checked, enabled: true, hovered: false, pressed: false }
    }

    pub fn event(&mut self, ev: &Event, focused: bool) -> Response {
        if !self.enabled {
            return Response::Ignored;
        }
        match *ev {
            Event::PointerMove { x, y } => {
                let h = contains(&self.rect, x, y);
                if h != self.hovered {
                    self.hovered = h;
                    return Response::Redraw;
                }
                Response::Ignored
            }
            Event::PointerLeave if self.hovered || self.pressed => {
                self.hovered = false;
                self.pressed = false;
                Response::Redraw
            }
            Event::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&self.rect, x, y) => {
                self.pressed = true;
                Response::Consumed
            }
            Event::PointerUp { x, y, button: b } if b == button::LEFT && self.pressed => {
                self.pressed = false;
                if contains(&self.rect, x, y) {
                    self.checked = !self.checked;
                    Response::Changed
                } else {
                    Response::Ignored
                }
            }
            Event::Key(k) if focused && k.down && !k.repeat && k.key == key::SPACE => {
                self.checked = !self.checked;
                Response::Changed
            }
            _ => Response::Ignored,
        }
    }

    pub fn draw(&self, p: &mut Painter, focused: bool) {
        let t = *p.theme;
        let r = self.rect;
        let size = 18u32.min(r.h);
        let b = Rect::new(r.x, r.y + (r.h as i32 - size as i32) / 2, size, size);
        if self.checked {
            p.rounded(b, 5, if self.hovered { t.accent_hover } else { t.accent }, 255);
            let s = size as i32;
            let pt = |fx: i32, fy: i32| ((b.x << 8) + s * fx * 256 / 100, (b.y << 8) + s * fy * 256 / 100);
            p.segment(pt(26, 52), pt(44, 70), 520, t.on_accent, 255);
            p.segment(pt(44, 70), pt(76, 32), 520, t.on_accent, 255);
        } else {
            p.card(b, 5, t.field, if self.hovered { t.accent } else { t.field_border });
        }
        let c = if self.enabled { t.text } else { t.text_faint };
        let tr = Rect::new(b.right() as i32 + 10, r.y, r.w.saturating_sub(size + 10), r.h);
        p.text_in(FontKind::Ui, tr, &self.label, c, Align::Left);
        if focused {
            p.focus_ring(b, 5);
        }
    }
}
