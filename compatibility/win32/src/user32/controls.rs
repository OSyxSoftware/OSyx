//! The progress bar (`msctls_progress32`) and list view (`SysListView32`)
//! of the common controls: enough of their messages and drawing for the
//! pages of installers. A progress bar keeps its range, position, step and
//! colours; a report list view keeps one text per item (and per column) and
//! a scroll position, and draws the rows that are visible.

use super::*;

/// The state of a common control, kept with its window.
#[derive(Default, Clone)]
pub(super) struct ControlData {
    // progress bar
    pub range: (i32, i32),
    pub pos: i32,
    pub step: i32,
    pub bar_colour: Option<u32>,
    pub back_colour: Option<u32>,
    // list view
    pub rows: Vec<Vec<String>>,
    pub columns: Vec<i32>,
    /// First visible row.
    pub top: usize,
    pub text_colour: Option<u32>,
}

const PBM_SETRANGE: u32 = 0x0401;
const PBM_SETPOS: u32 = 0x0402;
const PBM_DELTAPOS: u32 = 0x0403;
const PBM_SETSTEP: u32 = 0x0404;
const PBM_STEPIT: u32 = 0x0405;
const PBM_SETRANGE32: u32 = 0x0406;
const PBM_GETRANGE: u32 = 0x0407;
const PBM_GETPOS: u32 = 0x0408;
const PBM_SETBARCOLOR: u32 = 0x0409;
const PBM_SETMARQUEE: u32 = 0x040A;
const CCM_SETBKCOLOR: u32 = 0x2001;

const LVIF_TEXT: u32 = 0x0001;
const LVM_SETBKCOLOR: u32 = 0x1001;
const LVM_GETITEMCOUNT: u32 = 0x1004;
const LVM_INSERTITEMA: u32 = 0x1007;
const LVM_DELETEITEM: u32 = 0x1008;
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_ENSUREVISIBLE: u32 = 0x1013;
const LVM_INSERTCOLUMNA: u32 = 0x101B;
const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
const LVM_SETTEXTCOLOR: u32 = 0x1024;
const LVM_SETITEMTEXTA: u32 = 0x102E;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_SETITEMTEXTW: u32 = 0x1074;

/// The class kinds this module serves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Progress,
    ListView,
}

pub(super) fn kind_of(class: &str) -> Option<Kind> {
    if class.eq_ignore_ascii_case("msctls_progress32") {
        Some(Kind::Progress)
    } else if class.eq_ignore_ascii_case("SysListView32") {
        Some(Kind::ListView)
    } else {
        None
    }
}

fn data_of<R>(hwnd: u64, f: impl FnOnce(&mut ControlData) -> R) -> Option<R> {
    with(|g| g.windows.get_mut(&hwnd).map(|w| f(&mut w.data)))
}

fn colour_ref(c: u32) -> u32 {
    c & 0x00FF_FFFF
}

/// Handles a message of a progress bar or list view; `None` when it is not
/// one this module knows (the caller then uses the default).
pub(super) fn message(hwnd: u64, kind: Kind, msg: u32, w: u64, l: i64) -> Option<i64> {
    match kind {
        Kind::Progress => progress_message(hwnd, msg, w, l),
        Kind::ListView => list_message(hwnd, msg, w, l),
    }
}

fn redraw(hwnd: u64) {
    InvalidateRect(hwnd, core::ptr::null(), FALSE);
}

fn progress_message(hwnd: u64, msg: u32, w: u64, l: i64) -> Option<i64> {
    let result = data_of(hwnd, |d| {
        if d.range == (0, 0) {
            d.range = (0, 100);
        }
        if d.step == 0 {
            d.step = 10;
        }
        let clamp = |d: &ControlData, v: i32| v.clamp(d.range.0, d.range.1.max(d.range.0));
        match msg {
            PBM_SETRANGE => {
                d.range = ((l & 0xFFFF) as i32, ((l >> 16) & 0xFFFF) as i32);
                d.pos = clamp(d, d.pos);
                Some(1)
            }
            PBM_SETRANGE32 => {
                d.range = (w as u32 as i32, l as i32);
                d.pos = clamp(d, d.pos);
                Some(1)
            }
            PBM_SETPOS => {
                let old = d.pos;
                d.pos = clamp(d, w as u32 as i32);
                Some(old as i64)
            }
            PBM_DELTAPOS => {
                let old = d.pos;
                d.pos = clamp(d, old.saturating_add(w as u32 as i32));
                Some(old as i64)
            }
            PBM_SETSTEP => {
                let old = d.step;
                d.step = w as u32 as i32;
                Some(old as i64)
            }
            PBM_STEPIT => {
                let old = d.pos;
                let next = old.saturating_add(d.step);
                d.pos = if next > d.range.1 { d.range.0 } else { clamp(d, next) };
                Some(old as i64)
            }
            PBM_GETRANGE => {
                if l != 0 {
                    // SAFETY: the caller's PBRANGE (iLow, iHigh).
                    unsafe {
                        teb::put(l as u64, 0, d.range.0);
                        teb::put(l as u64, 4, d.range.1);
                    }
                }
                Some(if w != 0 { d.range.0 } else { d.range.1 } as i64)
            }
            PBM_GETPOS => Some(d.pos as i64),
            PBM_SETBARCOLOR => {
                let old = d.bar_colour.unwrap_or(0xFF00_0000);
                d.bar_colour = (l as u32 != 0xFF00_0000).then_some(colour_ref(l as u32));
                Some(old as i64)
            }
            CCM_SETBKCOLOR => {
                let old = d.back_colour.unwrap_or(0xFF00_0000);
                d.back_colour = (l as u32 != 0xFF00_0000).then_some(colour_ref(l as u32));
                Some(old as i64)
            }
            PBM_SETMARQUEE => Some(1),
            _ => None,
        }
    })
    .flatten();
    if result.is_some() && !matches!(msg, PBM_GETRANGE | PBM_GETPOS) {
        redraw(hwnd);
    }
    result
}

/// The text of an LVITEM (`pszText`) in the caller's layout.
/// `mask_says_text`: an inserted item must state LVIF_TEXT; `LVM_SETITEMTEXT` has no mask.
fn item_text(item: i64, wide: bool, mask_says_text: bool) -> String {
    if item == 0 {
        return String::new();
    }
    // SAFETY: the caller's LVITEM: mask at 0.
    if mask_says_text && unsafe { teb::get::<u32>(item as u64, 0) } & LVIF_TEXT == 0 {
        return String::new(); // no text in this item
    }
    // LVITEM: mask, iItem, iSubItem, state, stateMask, then pszText: at 20
    // for a PE32 program, at 24 (after padding) for a 64-bit one.
    // SAFETY: the caller's LVITEM.
    let text = unsafe {
        if crate::wow64::is_active() {
            teb::get::<u32>(item as u64, 20) as u64
        } else {
            teb::get::<u64>(item as u64, 24)
        }
    };
    // LPSTR_TEXTCALLBACK (-1): the program supplies the text on demand, which
    // this list does not ask for; it is not a pointer.
    let callback = if crate::wow64::is_active() { text == u32::MAX as u64 } else { text == u64::MAX };
    if text == 0 || callback {
        return String::new();
    }
    // SAFETY: a NUL-terminated string of the caller.
    unsafe {
        if wide {
            path::wide_to_string(text as *const u16)
        } else {
            path::ansi_to_string(text as *const u8)
        }
    }
    .unwrap_or_default()
}

fn item_fields(item: i64) -> (i32, i32) {
    // SAFETY: the caller's LVITEM: iItem at 4, iSubItem at 8.
    unsafe { (teb::get::<i32>(item as u64, 4), teb::get::<i32>(item as u64, 8)) }
}

const ROW_HEIGHT: i32 = 16;

fn list_message(hwnd: u64, msg: u32, w: u64, l: i64) -> Option<i64> {
    let visible_rows = with(|g| g.windows.get(&hwnd).map_or(1, |win| (win.size.1 as i32 / ROW_HEIGHT).max(1) as usize));
    let result = match msg {
        LVM_GETITEMCOUNT => return data_of(hwnd, |d| d.rows.len() as i64),
        LVM_INSERTITEMW | LVM_INSERTITEMA => {
            let text = item_text(l, msg == LVM_INSERTITEMW, true);
            let (index, _) = item_fields(l);
            data_of(hwnd, |d| {
                let at = (index.max(0) as usize).min(d.rows.len());
                d.rows.insert(at, alloc::vec![text]);
                at as i64
            })
        }
        LVM_SETITEMTEXTW | LVM_SETITEMTEXTA => {
            let text = item_text(l, msg == LVM_SETITEMTEXTW, false);
            let (_, column) = item_fields(l);
            data_of(hwnd, |d| match d.rows.get_mut(w as usize) {
                Some(row) => {
                    if row.len() <= column.max(0) as usize {
                        row.resize(column.max(0) as usize + 1, String::new());
                    }
                    row[column.max(0) as usize] = text;
                    1
                }
                None => 0,
            })
        }
        LVM_DELETEITEM => data_of(hwnd, |d| {
            if (w as usize) < d.rows.len() {
                d.rows.remove(w as usize);
                // The first visible row stays a row (or 0 with none left).
                d.top = d.top.min(d.rows.len().saturating_sub(1));
                1
            } else {
                0
            }
        }),
        LVM_DELETEALLITEMS => data_of(hwnd, |d| {
            d.rows.clear();
            d.top = 0;
            1
        }),
        LVM_ENSUREVISIBLE => data_of(hwnd, |d| {
            let index = w as usize;
            if index >= d.rows.len() {
                return 0;
            }
            if index < d.top {
                d.top = index;
            } else if index >= d.top + visible_rows {
                d.top = index + 1 - visible_rows;
            }
            1
        }),
        LVM_INSERTCOLUMNW | LVM_INSERTCOLUMNA => data_of(hwnd, |d| {
            d.columns.push(0);
            (d.columns.len() - 1) as i64
        }),
        LVM_SETCOLUMNWIDTH => data_of(hwnd, |d| {
            if let Some(c) = d.columns.get_mut(w as usize) {
                *c = l as i32;
            }
            1
        }),
        LVM_SETTEXTCOLOR => data_of(hwnd, |d| {
            d.text_colour = Some(colour_ref(l as u32));
            1
        }),
        LVM_SETBKCOLOR => data_of(hwnd, |d| {
            d.back_colour = Some(colour_ref(l as u32));
            1
        }),
        _ => None,
    };
    if result.is_some() {
        redraw(hwnd);
    }
    result
}

/// Draws a progress bar or list view into its paint DC.
pub(super) fn paint(dc: u64, kind: Kind, hwnd: u64, width: i32, height: i32) {
    // Only the rows that show are copied out of the lock; `top` is then 0.
    let visible = (height / ROW_HEIGHT).max(1) as usize;
    let Some(data) = data_of(hwnd, |d| ControlData {
        range: d.range,
        pos: d.pos,
        step: d.step,
        bar_colour: d.bar_colour,
        back_colour: d.back_colour,
        rows: d.rows.iter().skip(d.top).take(visible).cloned().collect(),
        columns: Vec::new(),
        top: 0,
        text_colour: d.text_colour,
    }) else {
        return;
    };
    let whole = [0, 0, width, height];
    let solid = |colour: u32, rect: [i32; 4]| {
        let brush = CreateSolidBrush(colour);
        FillRect(dc, rect.as_ptr(), brush);
        DeleteObject(brush);
    };
    match kind {
        Kind::Progress => {
            solid(data.back_colour.unwrap_or(0x00E6_E6E6), whole);
            let (low, high) = if data.range == (0, 0) { (0, 100) } else { data.range };
            let (low, high) = (low as i64, high as i64);
            let span = (high - low).max(1);
            let inner = (width - 2).max(0) as i64;
            let filled = ((data.pos as i64 - low).clamp(0, (high - low).max(0)) * inner / span) as i32;
            if filled > 0 {
                // The bar's default is the Windows green of the classic theme.
                solid(data.bar_colour.unwrap_or(0x0025_B006), [1, 1, 1 + filled, (height - 1).max(2)]);
            }
            let border = CreateSolidBrush(0x00BC_BCBC);
            FrameRect(dc, whole.as_ptr(), border);
            DeleteObject(border);
        }
        Kind::ListView => {
            solid(data.back_colour.unwrap_or(0x00FF_FFFF), whole);
            SetBkMode(dc, 1);
            SetTextColor(dc, data.text_colour.unwrap_or(0));
            let rows = (height / ROW_HEIGHT).max(1) as usize;
            for (line, row) in data.rows.iter().skip(data.top).take(rows).enumerate() {
                let mut rect = [2, line as i32 * ROW_HEIGHT, width - 2, (line as i32 + 1) * ROW_HEIGHT];
                let text: Vec<u16> = row.first().map_or("", String::as_str).encode_utf16().chain(core::iter::once(0)).collect();
                DrawTextW(dc, text.as_ptr(), -1, rect.as_mut_ptr(), 0x20 | 0x800); // DT_SINGLELINE | DT_NOPREFIX
            }
            let border = CreateSolidBrush(0x00BC_BCBC);
            FrameRect(dc, whole.as_ptr(), border);
            DeleteObject(border);
        }
    }
}
