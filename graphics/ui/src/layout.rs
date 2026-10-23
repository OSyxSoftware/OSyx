//! Rectangle arithmetic for laying out widgets: insets, splitting off
//! strips, rows of equal or fixed-width cells, centring.

use osyx_gfx::Rect;

pub fn inset(r: Rect, d: u32) -> Rect {
    inset_xy(r, d, d)
}

pub fn inset_xy(r: Rect, dx: u32, dy: u32) -> Rect {
    Rect::new(r.x + dx as i32, r.y + dy as i32, r.w.saturating_sub(2 * dx), r.h.saturating_sub(2 * dy))
}

/// Splits `h` pixels off the top: (strip, rest).
pub fn split_top(r: Rect, h: u32) -> (Rect, Rect) {
    let h = h.min(r.h);
    (Rect::new(r.x, r.y, r.w, h), Rect::new(r.x, r.y + h as i32, r.w, r.h - h))
}

/// Splits `h` pixels off the bottom: (rest, strip).
pub fn split_bottom(r: Rect, h: u32) -> (Rect, Rect) {
    let h = h.min(r.h);
    (Rect::new(r.x, r.y, r.w, r.h - h), Rect::new(r.x, r.y + (r.h - h) as i32, r.w, h))
}

/// Splits `w` pixels off the left: (strip, rest).
pub fn split_left(r: Rect, w: u32) -> (Rect, Rect) {
    let w = w.min(r.w);
    (Rect::new(r.x, r.y, w, r.h), Rect::new(r.x + w as i32, r.y, r.w - w, r.h))
}

/// Splits `w` pixels off the right: (rest, strip).
pub fn split_right(r: Rect, w: u32) -> (Rect, Rect) {
    let w = w.min(r.w);
    (Rect::new(r.x, r.y, r.w - w, r.h), Rect::new(r.x + (r.w - w) as i32, r.y, w, r.h))
}

/// A `w` x `h` rectangle centred in `r`.
pub fn center(r: Rect, w: u32, h: u32) -> Rect {
    let w = w.min(r.w);
    let h = h.min(r.h);
    Rect::new(r.x + ((r.w - w) / 2) as i32, r.y + ((r.h - h) / 2) as i32, w, h)
}

/// Cells of the given widths laid out left to right with `gap` between them.
pub fn row<const N: usize>(r: Rect, widths: [u32; N], gap: u32) -> [Rect; N] {
    let mut out = [Rect::EMPTY; N];
    let mut x = r.x;
    for (i, w) in widths.iter().enumerate() {
        out[i] = Rect::new(x, r.y, *w, r.h).intersect(&r);
        x += *w as i32 + gap as i32;
    }
    out
}

/// The same, laid out from the right edge leftwards (first width rightmost).
pub fn row_right<const N: usize>(r: Rect, widths: [u32; N], gap: u32) -> [Rect; N] {
    let mut out = [Rect::EMPTY; N];
    let mut right = r.right() as i32;
    for (i, w) in widths.iter().enumerate() {
        out[i] = Rect::new(right - *w as i32, r.y, *w, r.h).intersect(&r);
        right -= *w as i32 + gap as i32;
    }
    out
}

/// `n` equal cells with `gap` between them.
pub fn columns(r: Rect, n: u32, gap: u32) -> impl Iterator<Item = Rect> {
    let n = n.max(1);
    let total_gap = gap * (n - 1);
    let cell = r.w.saturating_sub(total_gap) / n;
    (0..n).map(move |i| Rect::new(r.x + (i * (cell + gap)) as i32, r.y, cell, r.h))
}

pub fn contains(r: &Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && (x as i64) < r.right() && (y as i64) < r.bottom()
}
