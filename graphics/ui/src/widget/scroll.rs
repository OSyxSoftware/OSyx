use crate::event::{Event, Response};
use crate::layout::{contains, split_right};
use crate::paint::Painter;
use osyx_gfx::Rect;
use osyx_input::button;

/// Width of a vertical scroll bar.
pub const BAR: u32 = 10;
/// Pixels one wheel detent scrolls.
pub const WHEEL_STEP: u32 = 48;

/// Vertical scroll position of `content` pixels shown through a
/// `viewport` pixels high, with a draggable scroll bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scroll {
    pub offset: u32,
    pub content: u32,
    pub viewport: u32,
    /// (pointer y at grab, offset at grab) while the thumb is dragged.
    drag: Option<(i32, u32)>,
    hovered: bool,
}

impl Scroll {
    pub fn max_offset(&self) -> u32 {
        self.content.saturating_sub(self.viewport)
    }

    pub fn needed(&self) -> bool {
        self.content > self.viewport
    }

    /// Sets the sizes, keeping the offset in range.
    pub fn set_sizes(&mut self, content: u32, viewport: u32) {
        self.content = content;
        self.viewport = viewport;
        self.offset = self.offset.min(self.max_offset());
    }

    pub fn scroll_to(&mut self, offset: i64) -> bool {
        let new = offset.clamp(0, self.max_offset() as i64) as u32;
        let changed = new != self.offset;
        self.offset = new;
        changed
    }

    pub fn scroll_by(&mut self, d: i64) -> bool {
        self.scroll_to(self.offset as i64 + d)
    }

    /// Scrolls the least amount that shows `top..top + h`.
    pub fn ensure_visible(&mut self, top: u32, h: u32) -> bool {
        if top < self.offset {
            self.scroll_to(top as i64)
        } else if top + h > self.offset + self.viewport {
            self.scroll_to((top + h) as i64 - self.viewport as i64)
        } else {
            false
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The thumb inside `track`, if the content does not fit.
    pub fn thumb(&self, track: Rect) -> Option<Rect> {
        if !self.needed() || track.h < 8 {
            return None;
        }
        let h = ((track.h as u64 * self.viewport as u64) / self.content as u64).max(24).min(track.h as u64) as u32;
        let room = track.h - h;
        let y = if self.max_offset() == 0 { 0 } else { (room as u64 * self.offset as u64 / self.max_offset() as u64) as u32 };
        Some(Rect::new(track.x, track.y + y as i32, track.w, h))
    }

    /// Scroll bar interaction in `track`; wheel events anywhere in `area`.
    pub fn event(&mut self, ev: &Event, track: Rect, area: Rect) -> Response {
        match *ev {
            Event::Wheel { x, y, dy } if contains(&area, x, y) => {
                if self.scroll_by(-(dy as i64) * WHEEL_STEP as i64) {
                    Response::Redraw
                } else {
                    Response::Consumed
                }
            }
            Event::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&track, x, y) && self.needed() => {
                let Some(th) = self.thumb(track) else { return Response::Ignored };
                if contains(&th, x, y) {
                    self.drag = Some((y, self.offset));
                    Response::Redraw
                } else {
                    // Page towards the click.
                    let page = self.viewport.saturating_sub(WHEEL_STEP) as i64;
                    self.scroll_by(if y < th.y { -page } else { page });
                    Response::Redraw
                }
            }
            Event::PointerMove { x, y } => {
                if let Some((y0, off0)) = self.drag {
                    let th = self.thumb(track).unwrap_or(track);
                    let room = track.h.saturating_sub(th.h).max(1) as i64;
                    let delta = (y - y0) as i64 * self.max_offset() as i64 / room;
                    self.scroll_to(off0 as i64 + delta);
                    return Response::Redraw;
                }
                let h = contains(&track, x, y);
                if h != self.hovered {
                    self.hovered = h;
                    return Response::Redraw;
                }
                Response::Ignored
            }
            Event::PointerUp { .. } if self.drag.is_some() => {
                self.drag = None;
                Response::Redraw
            }
            Event::PointerLeave if self.hovered => {
                self.hovered = false;
                Response::Redraw
            }
            _ => Response::Ignored,
        }
    }

    pub fn draw(&self, p: &mut Painter, track: Rect) {
        let Some(th) = self.thumb(track) else { return };
        let t = *p.theme;
        let c = if self.hovered || self.drag.is_some() { t.scrollbar_hover } else { t.scrollbar };
        let th = Rect::new(th.x + 2, th.y + 2, th.w.saturating_sub(4), th.h.saturating_sub(4));
        p.rounded(th, th.w / 2, c, 255);
    }
}

/// A scrolled area: the application draws its content shifted up by
/// `scroll.offset` inside [`ScrollView::content`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollView {
    pub rect: Rect,
    pub scroll: Scroll,
}

impl ScrollView {
    pub fn track(&self) -> Rect {
        split_right(self.rect, BAR).1
    }

    /// The content area (minus the scroll bar when one is shown).
    pub fn content(&self) -> Rect {
        if self.scroll.needed() {
            split_right(self.rect, BAR + 2).0
        } else {
            self.rect
        }
    }

    pub fn set_content_height(&mut self, h: u32) {
        self.scroll.set_sizes(h, self.rect.h);
    }

    pub fn event(&mut self, ev: &Event) -> Response {
        let track = self.track();
        self.scroll.event(ev, track, self.rect)
    }

    pub fn draw_bar(&self, p: &mut Painter) {
        self.scroll.draw(p, self.track());
    }
}
