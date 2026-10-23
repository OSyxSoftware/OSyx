use crate::event::{Event, Response};
use crate::icon::{self, Icon};
use crate::layout::contains;
use crate::paint::{Align, FontKind, Fonts, Painter};
use alloc::string::String;
use alloc::vec::Vec;
use osyx_gfx::Rect;
use osyx_input::{button, key};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub icon: Option<Icon>,
    /// Right-aligned hint (a shortcut).
    pub hint: String,
    pub enabled: bool,
    /// Draw a separator line above this item.
    pub separator: bool,
    pub danger: bool,
}

impl MenuItem {
    pub fn new(label: &str, icon: Option<Icon>) -> MenuItem {
        MenuItem { label: String::from(label), icon, hint: String::new(), enabled: true, separator: false, danger: false }
    }

    pub fn hint(mut self, h: &str) -> MenuItem {
        self.hint = String::from(h);
        self
    }

    pub fn separated(mut self) -> MenuItem {
        self.separator = true;
        self
    }

    pub fn danger(mut self) -> MenuItem {
        self.danger = true;
        self
    }
}

/// A vertical menu (context menus, the launcher's power menu). Usually the
/// only content of a popup window: `rect` is then the whole surface.
#[derive(Clone, Debug)]
pub struct Menu {
    pub rect: Rect,
    pub items: Vec<MenuItem>,
    hovered: Option<usize>,
    pressed: bool,
}

const ITEM_H: u32 = 32;
const SEP_H: u32 = 9;
const PAD: u32 = 6;

impl Menu {
    pub fn new(items: Vec<MenuItem>) -> Menu {
        Menu { rect: Rect::EMPTY, items, hovered: None, pressed: false }
    }

    /// Size that fits every item.
    pub fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let w = self
            .items
            .iter()
            .map(|i| {
                let hint = if i.hint.is_empty() { 0 } else { fonts.measure(FontKind::Ui, &i.hint) + 24 };
                fonts.measure(FontKind::Ui, &i.label) + hint + 64
            })
            .max()
            .unwrap_or(120)
            .max(180);
        let h = self.items.iter().map(|i| ITEM_H + if i.separator { SEP_H } else { 0 }).sum::<u32>() + 2 * PAD;
        (w, h)
    }

    fn item_rects(&self) -> Vec<Rect> {
        let mut y = self.rect.y + PAD as i32;
        self.items
            .iter()
            .map(|i| {
                if i.separator {
                    y += SEP_H as i32;
                }
                let r = Rect::new(self.rect.x + PAD as i32, y, self.rect.w.saturating_sub(2 * PAD), ITEM_H);
                y += ITEM_H as i32;
                r
            })
            .collect()
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        self.item_rects().iter().position(|r| contains(r, x, y)).filter(|&i| self.items[i].enabled)
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    pub fn event(&mut self, ev: &Event) -> Response {
        match *ev {
            Event::PointerMove { x, y } => {
                let h = self.item_at(x, y);
                if h != self.hovered {
                    self.hovered = h;
                    return Response::Redraw;
                }
                Response::Ignored
            }
            Event::PointerLeave if self.hovered.is_some() => {
                self.hovered = None;
                Response::Redraw
            }
            Event::PointerDown { x, y, .. } => {
                if !contains(&self.rect, x, y) {
                    return Response::Cancelled;
                }
                self.pressed = true;
                Response::Consumed
            }
            Event::PointerUp { x, y, button: b } => {
                let was = core::mem::take(&mut self.pressed);
                match self.item_at(x, y) {
                    // A press that started on the item (or the release of the
                    // right button that opened a context menu) picks it.
                    Some(i) if was || b == button::RIGHT => Response::Activated(i),
                    _ => Response::Ignored,
                }
            }
            Event::Key(k) if k.down => {
                let enabled: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].enabled).collect();
                if enabled.is_empty() {
                    return if k.key == key::ESCAPE { Response::Cancelled } else { Response::Ignored };
                }
                let pos = self.hovered.and_then(|h| enabled.iter().position(|&e| e == h));
                match k.key {
                    key::DOWN => {
                        self.hovered = Some(enabled[pos.map_or(0, |p| (p + 1) % enabled.len())]);
                        Response::Redraw
                    }
                    key::UP => {
                        self.hovered = Some(enabled[pos.map_or(enabled.len() - 1, |p| (p + enabled.len() - 1) % enabled.len())]);
                        Response::Redraw
                    }
                    key::ENTER | key::KP_ENTER | key::SPACE => self.hovered.map_or(Response::Ignored, Response::Activated),
                    key::ESCAPE => Response::Cancelled,
                    _ => Response::Ignored,
                }
            }
            _ => Response::Ignored,
        }
    }

    pub fn draw(&self, p: &mut Painter) {
        let t = *p.theme;
        p.card(self.rect, t.radius, t.surface, t.border);
        for (i, (item, r)) in self.items.iter().zip(self.item_rects()).enumerate() {
            if item.separator {
                p.hline(r.x + 4, r.y - (SEP_H as i32) / 2 - 1, r.w.saturating_sub(8), t.border);
            }
            let hovered = self.hovered == Some(i);
            if hovered {
                p.rounded(r, t.radius_small, if item.danger { t.danger } else { t.selection }, 255);
            }
            let fg = if !item.enabled {
                t.text_faint
            } else if hovered && item.danger {
                t.on_accent
            } else if item.danger {
                t.danger_hover
            } else {
                t.text
            };
            let mut text = Rect::new(r.x + 12, r.y, r.w.saturating_sub(24), r.h);
            if let Some(ic) = item.icon {
                icon::draw(p, ic, Rect::new(r.x + 10, r.y + 7, 18, 18), fg);
                text = Rect::new(r.x + 38, r.y, r.w.saturating_sub(50), r.h);
            }
            p.text_in(FontKind::Ui, text, &item.label, fg, Align::Left);
            if !item.hint.is_empty() {
                p.text_in(FontKind::Ui, text, &item.hint, t.text_faint, Align::Right);
            }
        }
    }
}
