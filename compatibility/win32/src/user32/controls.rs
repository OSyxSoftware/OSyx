//! The progress bar (`msctls_progress32`) and list view (`SysListView32`)
//! of the common controls, and the rich edit control (`RichEdit20W` and
//! relatives): enough of their messages and drawing for the pages of
//! installers. A progress bar keeps its range, position, step and colours;
//! a report list view keeps one text per item (and per column) and a scroll
//! position, and draws the rows that are visible; a rich edit control shows
//! read-only text (plain or the text of RTF) streamed in with EM_STREAMIN
//! or set with WM_SETTEXT, word-wrapped and scrollable.

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
    // static control
    /// STM_SETIMAGE / STM_SETICON: the bitmap or icon a static shows.
    pub image: u64,
    // rich edit
    pub event_mask: u32,
    /// Wrapped lines for (text hash, width).
    pub wrapped: Option<(u64, i32, Vec<String>)>,
    // combo box (its strings are `rows`, one column each)
    pub sel: Option<usize>,
    pub item_data: Vec<u64>,
    // tree view
    pub nodes: Vec<TreeNode>,
    pub next_handle: u64,
    pub selected: u64,
    pub state_images: u64,
}

/// One tree view item; `nodes` keeps them in display order (each item
/// before its children).
#[derive(Default, Clone)]
pub(super) struct TreeNode {
    pub handle: u64,
    /// 0 for a root item.
    pub parent: u64,
    pub text: String,
    pub state: u32,
    pub image: i32,
    pub selected_image: i32,
    pub children: i32,
    pub lparam: u64,
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

const WM_KEYDOWN: u32 = 0x0100;
const WM_VSCROLL: u32 = 0x0115;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_GETDLGCODE: u32 = 0x0087;
const EM_SCROLL: u32 = 0x00B5;
const EM_LINESCROLL: u32 = 0x00B6;
const EM_GETLINECOUNT: u32 = 0x00BA;
const EM_GETFIRSTVISIBLELINE: u32 = 0x00CE;
const EM_GETEVENTMASK: u32 = 0x043B;
const EM_SETBKGNDCOLOR: u32 = 0x0443;
const EM_SETEVENTMASK: u32 = 0x0445;
const EM_STREAMIN: u32 = 0x0449;
const EM_GETTEXTLENGTHEX: u32 = 0x045F;

/// The class kinds this module serves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Progress,
    ListView,
    RichEdit,
    ComboBox,
    TreeView,
}

/// Window classes of the rich edit control: riched20.dll (2.0/3.0),
/// riched32.dll (1.0) and msftedit.dll (4.1+).
pub(super) const RICH_EDIT_CLASSES: [&str; 5] = ["RichEdit20W", "RichEdit20A", "RICHEDIT50W", "RICHEDIT60W", "RichEdit"];

pub(super) fn kind_of(class: &str) -> Option<Kind> {
    if class.eq_ignore_ascii_case("msctls_progress32") {
        Some(Kind::Progress)
    } else if class.eq_ignore_ascii_case("SysListView32") {
        Some(Kind::ListView)
    } else if RICH_EDIT_CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c)) {
        Some(Kind::RichEdit)
    } else if class.eq_ignore_ascii_case("ComboBox") {
        Some(Kind::ComboBox)
    } else if class.eq_ignore_ascii_case("SysTreeView32") {
        Some(Kind::TreeView)
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
        Kind::RichEdit => rich_message(hwnd, msg, w, l),
        Kind::ComboBox => combo_message(hwnd, msg, w, l),
        Kind::TreeView => tree_message(hwnd, msg, w, l),
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
                // A row removed above the view moves the rest up: keep the same
                // first row showing. Otherwise it stays a row (0 with none left).
                if (w as usize) < d.top {
                    d.top -= 1;
                } else {
                    d.top = d.top.min(d.rows.len().saturating_sub(1));
                }
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
    // The rich edit, combo box and tree view keep their own scroll state
    // and caches: they get a full copy.
    let Some(data) = data_of(hwnd, |d| match kind {
        Kind::Progress | Kind::ListView => ControlData {
            range: d.range,
            pos: d.pos,
            step: d.step,
            bar_colour: d.bar_colour,
            back_colour: d.back_colour,
            rows: d.rows.iter().skip(d.top).take(visible).cloned().collect(),
            columns: Vec::new(),
            top: 0,
            text_colour: d.text_colour,
            ..ControlData::default()
        },
        Kind::RichEdit | Kind::ComboBox | Kind::TreeView => d.clone(),
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
        Kind::RichEdit => paint_rich(dc, hwnd, data, width, height),
        Kind::ComboBox => paint_combo(dc, hwnd, &data, width, height),
        Kind::TreeView => paint_tree(dc, hwnd, &data, width, height),
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

// ---- rich edit ----------------------------------------------------------------

/// Lines one wheel notch (or arrow key) moves.
const WHEEL_LINES: i32 = 3;

fn rich_text(hwnd: u64) -> String {
    with(|g| g.windows.get(&hwnd).map(|w| w.title.clone()).unwrap_or_default())
}

/// Replaces the text (or appends it for SFF_SELECTION) and shows its start.
fn set_rich_text(hwnd: u64, text: String, append: bool) {
    with(|g| {
        if let Some(w) = g.windows.get_mut(&hwnd) {
            if append {
                w.title.push_str(&text);
            } else {
                w.title = text;
                w.data.top = 0;
            }
        }
    });
    redraw(hwnd);
}

/// Scrolls by `lines` (negative: up), clamped to the text.
fn scroll_rich(hwnd: u64, lines: i32) -> bool {
    let total = with(|g| g.windows.get(&hwnd).and_then(|w| w.data.wrapped.as_ref().map(|c| c.2.len())));
    let total = total.unwrap_or_else(|| rich_text(hwnd).lines().count());
    let moved = data_of(hwnd, |d| {
        let old = d.top;
        let next = (d.top as i64 + lines as i64).clamp(0, total.saturating_sub(1) as i64) as usize;
        d.top = next;
        old != next
    })
    .unwrap_or(false);
    if moved {
        redraw(hwnd);
    }
    moved
}

fn visible_lines(hwnd: u64) -> i32 {
    with(|g| g.windows.get(&hwnd).map_or(1, |w| ((w.size.1 as i32 - 4) / ROW_HEIGHT).max(1)))
}

fn rich_message(hwnd: u64, msg: u32, w: u64, l: i64) -> Option<i64> {
    const SB_LINEUP: u32 = 0;
    const SB_LINEDOWN: u32 = 1;
    const SB_PAGEUP: u32 = 2;
    const SB_PAGEDOWN: u32 = 3;
    const SB_THUMBTRACK: u32 = 5;
    const SB_TOP: u32 = 6;
    const SB_BOTTOM: u32 = 7;
    let page = visible_lines(hwnd);
    let scroll_code = |code: u32, pos: i32| -> i32 {
        match code {
            SB_LINEUP => -1,
            SB_LINEDOWN => 1,
            SB_PAGEUP => -page,
            SB_PAGEDOWN => page,
            SB_THUMBTRACK | 4 => pos - data_of(hwnd, |d| d.top as i32).unwrap_or(0),
            SB_TOP => i32::MIN / 2,
            SB_BOTTOM => i32::MAX / 2,
            _ => 0,
        }
    };
    match msg {
        EM_STREAMIN => Some(stream_in(hwnd, w as u32, l)),
        EM_SETBKGNDCOLOR => data_of(hwnd, |d| {
            let old = d.back_colour.unwrap_or(0x00FF_FFFF);
            // wParam != 0: the system's window colour.
            d.back_colour = if w != 0 { None } else { Some(colour_ref(l as u32)) };
            old as i64
        })
        .inspect(|_| redraw(hwnd)),
        EM_SETEVENTMASK => data_of(hwnd, |d| core::mem::replace(&mut d.event_mask, l as u32) as i64),
        EM_GETEVENTMASK => data_of(hwnd, |d| d.event_mask as i64),
        EM_GETLINECOUNT => Some(rich_text(hwnd).lines().count().max(1) as i64),
        EM_GETFIRSTVISIBLELINE => data_of(hwnd, |d| d.top as i64),
        EM_GETTEXTLENGTHEX => Some(rich_text(hwnd).encode_utf16().count() as i64),
        EM_LINESCROLL => {
            scroll_rich(hwnd, l as i32);
            Some(1)
        }
        EM_SCROLL => {
            let moved = scroll_code(w as u32 & 0xFFFF, 0);
            scroll_rich(hwnd, moved);
            Some(0x1_0000 | (moved.unsigned_abs() as i64 & 0xFFFF))
        }
        WM_VSCROLL => {
            scroll_rich(hwnd, scroll_code(w as u32 & 0xFFFF, (w >> 16) as u16 as i32));
            Some(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (w >> 16) as u16 as i16 as i32;
            scroll_rich(hwnd, -delta * WHEEL_LINES / 120);
            Some(0)
        }
        WM_KEYDOWN => {
            let lines = match w as u32 {
                0x26 => -1,          // VK_UP
                0x28 => 1,           // VK_DOWN
                0x21 => -page,       // VK_PRIOR
                0x22 => page,        // VK_NEXT
                0x24 => i32::MIN / 2, // VK_HOME
                0x23 => i32::MAX / 2, // VK_END
                _ => return None,
            };
            scroll_rich(hwnd, lines);
            Some(0)
        }
        WM_GETDLGCODE => Some(0x1 | 0x80), // DLGC_WANTARROWS | DLGC_HASSETSEL
        // Accepted without effect: formatting, limits, URL detection, options
        // and selection of a read-only display.
        0x00B1 | 0x00C5 | 0x00CF | 0x00D3 | 0x0435 | 0x0437 | 0x043F | 0x0444 | 0x0447 | 0x0448 | 0x044D | 0x0459 | 0x045B
        | 0x0478 | 0x04CC => Some(if msg == 0x00CF || msg == 0x0448 { 1 } else { 0 }),
        _ => None,
    }
}

/// EM_STREAMIN: reads the document from the caller's EDITSTREAM callback
/// (32-bit code for a WoW64 program) in 4 KiB pieces until it delivers
/// nothing or reports an error; returns the characters read.
fn stream_in(hwnd: u64, format: u32, es: i64) -> i64 {
    const SF_RTF: u32 = 0x2;
    const SF_UNICODE: u32 = 0x10;
    const SFF_SELECTION: u32 = 0x8000;
    const MAX_DOCUMENT: usize = 16 << 20;
    if es == 0 {
        return 0;
    }
    let wow = crate::wow64::is_active();
    // EDITSTREAM: dwCookie (DWORD_PTR), dwError (DWORD), pfnCallback.
    // SAFETY: the caller's EDITSTREAM in its own layout.
    let (cookie, callback) = unsafe {
        if wow {
            (teb::get::<u32>(es as u64, 0) as u64, teb::get::<u32>(es as u64, 8) as u64)
        } else {
            (teb::get::<u64>(es as u64, 0), teb::get::<u64>(es as u64, 16))
        }
    };
    if callback == 0 {
        return 0;
    }
    // Heap memory: below 2 GiB for a 32-bit program, which writes it.
    let mut chunk = alloc::vec![0u8; 4096];
    let mut got = alloc::boxed::Box::new(0i32);
    let mut bytes = Vec::new();
    let mut error = 0u32;
    loop {
        *got = 0;
        let got_ptr: *mut i32 = &mut *got;
        let r = if wow {
            crate::wow64::call32(callback as u32, &[cookie as u32, chunk.as_mut_ptr() as u32, chunk.len() as u32, got_ptr as u32])
        } else {
            // SAFETY: a caller-supplied EDITSTREAMCALLBACK of the Windows x64 ABI.
            let f: extern "win64" fn(u64, *mut u8, i32, *mut i32) -> u32 = unsafe { core::mem::transmute(callback as usize) };
            f(cookie, chunk.as_mut_ptr(), chunk.len() as i32, got_ptr)
        };
        if r != 0 {
            error = r;
            break;
        }
        let n = (*got).clamp(0, chunk.len() as i32) as usize;
        if n == 0 || bytes.len() + n > MAX_DOCUMENT {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    // SAFETY: dwError of the caller's EDITSTREAM.
    unsafe { teb::put(es as u64, if wow { 4 } else { 8 }, error) };
    let text = if format & SF_RTF != 0 {
        rtf_to_text(&bytes)
    } else if format & SF_UNICODE != 0 {
        char::decode_utf16(bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c))).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    } else {
        ansi_text(&bytes)
    };
    let count = text.chars().count() as i64;
    set_rich_text(hwnd, text.replace("\r\n", "\n").replace('\r', "\n"), format & SFF_SELECTION != 0);
    count
}

/// ANSI text as elsewhere in the runtime: UTF-8 when it is valid, else
/// Windows-1252.
fn ansi_text(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(s) => String::from(s),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

/// One Windows-1252 byte (the 0x80..0x9F block differs from Latin-1).
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&b) { HIGH[(b - 0x80) as usize] } else { b as char }
}

/// The text of an RTF document. Groups and control words are read;
/// destinations without body text (font, colour and style tables, document
/// information, pictures, objects, headers and `\*` groups) are skipped;
/// `\par` and `\line` become line breaks, `\tab` a tab, `\'hh` a
/// Windows-1252 byte and `\uN` a Unicode character (its ANSI fallback is
/// skipped as `\ucN` says).
pub(super) fn rtf_to_text(rtf: &[u8]) -> String {
    let mut out = String::new();
    let mut groups: Vec<(bool, usize)> = Vec::new();
    let (mut skip, mut uc, mut fallback) = (false, 1usize, 0usize);
    let mut i = 0;
    let emit = |out: &mut String, skip: bool, fallback: &mut usize, c: char| {
        if *fallback > 0 {
            *fallback -= 1;
        } else if !skip {
            out.push(c);
        }
    };
    while i < rtf.len() {
        match rtf[i] {
            b'{' => {
                groups.push((skip, uc));
                i += 1;
            }
            b'}' => {
                if let Some((s, u)) = groups.pop() {
                    skip = s;
                    uc = u;
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&d) = rtf.get(i) else { break };
                if d.is_ascii_alphabetic() {
                    let start = i;
                    while rtf.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
                        i += 1;
                    }
                    let word = &rtf[start..i];
                    let negative = rtf.get(i) == Some(&b'-');
                    if negative {
                        i += 1;
                    }
                    let digits = i;
                    while rtf.get(i).is_some_and(|c| c.is_ascii_digit()) {
                        i += 1;
                    }
                    let param = core::str::from_utf8(&rtf[digits..i]).ok().and_then(|t| t.parse::<i32>().ok()).map(|v| if negative { -v } else { v });
                    if rtf.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    let special = match word {
                        b"par" | b"line" | b"sect" | b"page" | b"row" => Some('\n'),
                        b"tab" | b"cell" => Some('\t'),
                        b"emdash" => Some('—'),
                        b"endash" => Some('–'),
                        b"bullet" => Some('•'),
                        b"lquote" => Some('‘'),
                        b"rquote" => Some('’'),
                        b"ldblquote" => Some('“'),
                        b"rdblquote" => Some('”'),
                        _ => None,
                    };
                    if let Some(c) = special {
                        if !skip {
                            out.push(c);
                        }
                    }
                    match word {
                        b"u" => {
                            if let Some(v) = param {
                                let code = if v < 0 { (v + 65536) as u32 } else { v as u32 };
                                if !skip {
                                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                                }
                                fallback = uc;
                            }
                        }
                        b"uc" => uc = param.unwrap_or(1).max(0) as usize,
                        b"fonttbl" | b"colortbl" | b"stylesheet" | b"info" | b"pict" | b"object" | b"header" | b"footer"
                        | b"headerl" | b"headerr" | b"footerl" | b"footerr" | b"listtable" | b"listoverridetable" | b"rsidtbl"
                        | b"generator" | b"xmlnstbl" | b"themedata" | b"colorschememapping" | b"datastore" | b"latentstyles"
                        | b"fldinst" | b"filetbl" | b"revtbl" => skip = true,
                        _ => {}
                    }
                } else {
                    i += 1;
                    match d {
                        b'\'' => {
                            let byte = rtf.get(i..i + 2).and_then(|h| core::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                            i += 2;
                            if let Some(b) = byte {
                                emit(&mut out, skip, &mut fallback, cp1252(b));
                            }
                        }
                        b'*' => skip = true,
                        b'\\' | b'{' | b'}' => emit(&mut out, skip, &mut fallback, d as char),
                        b'~' => emit(&mut out, skip, &mut fallback, '\u{A0}'),
                        b'\n' | b'\r' if !skip => out.push('\n'),
                        _ => {}
                    }
                }
            }
            // Line breaks in the source are not text.
            b'\r' | b'\n' => i += 1,
            c => {
                emit(&mut out, skip, &mut fallback, cp1252(c));
                i += 1;
            }
        }
    }
    out
}

/// FNV-1a of the text, to know when the wrapped lines are stale.
fn text_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Word-wraps `text` to `width` pixels in the DC's font; tabs become four
/// spaces.
fn wrap_text(dc: u64, text: &str, width: i32) -> Vec<String> {
    let Some(kind) = with(|g| g.dcs.get(&dc).map(|d| font_of(&g.gdi, d.font))) else { return Vec::new() };
    let width = width.max(8) as u32;
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let para = para.replace('\t', "    ");
        let mut current = String::new();
        for word in para.split(' ') {
            let candidate = if current.is_empty() { String::from(word) } else { alloc::format!("{current} {word}") };
            if measure(kind, &candidate) > width && !current.is_empty() {
                lines.push(core::mem::take(&mut current));
                current = String::from(word);
            } else {
                current = candidate;
            }
        }
        lines.push(current);
    }
    lines
}

/// Background, the visible lines from the first visible one, a scroll
/// thumb when the text is longer than the control, and the frame.
fn paint_rich(dc: u64, hwnd: u64, data: ControlData, width: i32, height: i32) {
    const BAR: i32 = 6;
    let text = rich_text(hwnd);
    let brush = CreateSolidBrush(data.back_colour.unwrap_or(0x00FF_FFFF));
    FillRect(dc, [0, 0, width, height].as_ptr(), brush);
    DeleteObject(brush);
    let hash = text_hash(&text);
    let inner = width - 8 - BAR;
    let lines = match data.wrapped {
        Some((h, w, lines)) if h == hash && w == inner => lines,
        _ => {
            let lines = wrap_text(dc, &text, inner);
            data_of(hwnd, |d| d.wrapped = Some((hash, inner, lines.clone())));
            lines
        }
    };
    SetBkMode(dc, 1);
    SetTextColor(dc, data.text_colour.unwrap_or(0));
    let rows = ((height - 4) / ROW_HEIGHT).max(1) as usize;
    let top = data.top.min(lines.len().saturating_sub(1));
    for (n, line) in lines.iter().skip(top).take(rows).enumerate() {
        let y = 2 + n as i32 * ROW_HEIGHT;
        let mut rect = [4, y, 4 + inner, y + ROW_HEIGHT];
        let wide: Vec<u16> = line.encode_utf16().chain(core::iter::once(0)).collect();
        DrawTextW(dc, wide.as_ptr(), -1, rect.as_mut_ptr(), 0x20 | 0x800); // DT_SINGLELINE | DT_NOPREFIX
    }
    if lines.len() > rows {
        let track = (height - 4).max(1);
        let thumb = (track as usize * rows / lines.len()).max(12) as i32;
        let y = 2 + ((track - thumb) as i64 * top as i64 / (lines.len() - rows).max(1) as i64) as i32;
        let thumb_brush = CreateSolidBrush(0x00A0_A0A0);
        FillRect(dc, [width - BAR - 2, y, width - 2, y + thumb].as_ptr(), thumb_brush);
        DeleteObject(thumb_brush);
    }
    let border = CreateSolidBrush(0x00BC_BCBC);
    FrameRect(dc, [0, 0, width, height].as_ptr(), border);
    DeleteObject(border);
}

// ---- shared helpers -----------------------------------------------------------

/// The pointer size of the program's structures: 4 for a 32-bit program.
fn ptr_size() -> usize {
    if crate::wow64::is_active() { 4 } else { 8 }
}

/// A pointer-sized field of a caller structure; a 32-bit value is
/// sign-extended, so special handles (TVI_ROOT, ...) keep their 64-bit value.
fn read_ptr(base: u64, at: usize) -> u64 {
    // SAFETY: the caller's structure holds a pointer-sized field at `at`.
    unsafe {
        if ptr_size() == 4 {
            teb::get::<u32>(base, at) as i32 as i64 as u64
        } else {
            teb::get::<u64>(base, at)
        }
    }
}

fn write_ptr(base: u64, at: usize, value: u64) {
    // SAFETY: as `read_ptr`.
    unsafe {
        if ptr_size() == 4 {
            teb::put(base, at, value as u32);
        } else {
            teb::put(base, at, value);
        }
    }
}

fn wide_text(p: u64) -> String {
    if p == 0 {
        return String::new();
    }
    // SAFETY: a NUL-terminated UTF-16 string of the caller.
    unsafe { path::wide_to_string(p as *const u16) }.unwrap_or_default()
}

/// Copies `text` into the caller's buffer of `max` units (NUL included),
/// UTF-16 or ANSI; returns the units copied without the NUL.
fn copy_text(text: &str, buf: u64, max: i64, wide: bool) -> i64 {
    if buf == 0 || max <= 0 {
        return 0;
    }
    if wide {
        let units: Vec<u16> = text.encode_utf16().take(max as usize - 1).collect();
        // SAFETY: the caller's buffer of `max` UTF-16 units.
        unsafe {
            core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
            *(buf as *mut u16).add(units.len()) = 0;
        }
        units.len() as i64
    } else {
        let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).take(max as usize - 1).collect();
        // SAFETY: the caller's buffer of `max` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
            *(buf as *mut u8).add(bytes.len()) = 0;
        }
        bytes.len() as i64
    }
}

fn parent_and_id(hwnd: u64) -> (u64, u64) {
    with(|g| g.windows.get(&hwnd).map_or((0, 0), |w| (w.owner, w.control_id)))
}

/// WM_COMMAND notification `code` to the parent.
fn command(hwnd: u64, code: u32) {
    let (parent, id) = parent_and_id(hwnd);
    if parent != 0 {
        SendMessageW(parent, 0x0111, (id & 0xFFFF) | ((code as u64) << 16), hwnd as i64);
    }
}

/// WM_NOTIFY `code` to the parent: an NMHDR in the program's layout,
/// followed by `extra` bytes (laid out by the caller for that pointer size).
fn notify(hwnd: u64, code: i32, extra: &[u8]) -> i64 {
    let (parent, id) = parent_and_id(hwnd);
    if parent == 0 {
        return 0;
    }
    let ptr = ptr_size();
    // NMHDR: hwndFrom, idFrom, code (padded to pointer alignment).
    let head = 3 * ptr;
    // Heap memory: below 2 GiB for a 32-bit program.
    let mut block = alloc::vec![0u8; head + extra.len()];
    let base = block.as_mut_ptr() as u64;
    write_ptr(base, 0, hwnd);
    write_ptr(base, ptr, id);
    // SAFETY: inside `block`.
    unsafe { teb::put(base, 2 * ptr, code) };
    block[head..].copy_from_slice(extra);
    SendMessageW(parent, 0x004E, id, base as i64)
}

// ---- combo box ------------------------------------------------------------------

const CB_ERR: i64 = -1;

/// A drop-down list: the selection changes with a click (to the next
/// string), the arrow keys and the wheel; there is no drop-down popup.
fn combo_message(hwnd: u64, msg: u32, w: u64, l: i64) -> Option<i64> {
    const CBN_SELCHANGE: u32 = 1;
    let select = |delta: i32| {
        let changed = data_of(hwnd, |d| {
            let n = d.rows.len() as i32;
            if n == 0 {
                return false;
            }
            let next = match d.sel {
                Some(s) => (s as i32 + delta).rem_euclid(n),
                None => 0,
            } as usize;
            let changed = d.sel != Some(next);
            d.sel = Some(next);
            changed
        })
        .unwrap_or(false);
        if changed {
            redraw(hwnd);
            command(hwnd, CBN_SELCHANGE);
        }
    };
    let result = match msg {
        0x0143 => data_of(hwnd, |d| { // CB_ADDSTRING
            d.rows.push(alloc::vec![wide_text(l as u64)]);
            d.item_data.push(0);
            d.rows.len() as i64 - 1
        }),
        0x014A => data_of(hwnd, |d| { // CB_INSERTSTRING
            let at = if (w as i64) < 0 { d.rows.len() } else { (w as usize).min(d.rows.len()) };
            d.rows.insert(at, alloc::vec![wide_text(l as u64)]);
            d.item_data.insert(at, 0);
            at as i64
        }),
        0x0144 => data_of(hwnd, |d| { // CB_DELETESTRING
            let i = w as usize;
            if i >= d.rows.len() {
                return CB_ERR;
            }
            d.rows.remove(i);
            d.item_data.remove(i);
            if d.sel == Some(i) {
                d.sel = None;
            }
            d.rows.len() as i64
        }),
        0x014B => data_of(hwnd, |d| { // CB_RESETCONTENT
            d.rows.clear();
            d.item_data.clear();
            d.sel = None;
            0
        }),
        0x0146 => data_of(hwnd, |d| d.rows.len() as i64), // CB_GETCOUNT
        0x0147 => data_of(hwnd, |d| d.sel.map_or(CB_ERR, |s| s as i64)), // CB_GETCURSEL
        0x014E => data_of(hwnd, |d| { // CB_SETCURSEL
            let i = w as i64;
            if i < 0 || i as usize >= d.rows.len() {
                d.sel = None;
                CB_ERR
            } else {
                d.sel = Some(i as usize);
                i
            }
        }),
        0x0148 => { // CB_GETLBTEXT
            let text = data_of(hwnd, |d| d.rows.get(w as usize).and_then(|r| r.first().cloned())).flatten();
            match text {
                Some(t) => Some(copy_text(&t, l as u64, i32::MAX as i64, true)),
                None => Some(CB_ERR),
            }
        }
        0x0149 => data_of(hwnd, |d| d.rows.get(w as usize).and_then(|r| r.first()).map_or(CB_ERR, |t| t.encode_utf16().count() as i64)),
        0x014C | 0x0158 | 0x014D => { // CB_FINDSTRING, CB_FINDSTRINGEXACT, CB_SELECTSTRING
            let want = wide_text(l as u64).to_lowercase();
            let exact = msg == 0x0158;
            let found = data_of(hwnd, |d| {
                let n = d.rows.len();
                let start = if (w as i64) < 0 { 0 } else { (w as usize + 1) % n.max(1) };
                (0..n).map(|k| (start + k) % n).find(|&i| {
                    let t = d.rows[i].first().map(|t| t.to_lowercase()).unwrap_or_default();
                    if exact { t == want } else { t.starts_with(&want) }
                })
            })
            .flatten();
            match found {
                Some(i) => {
                    if msg == 0x014D {
                        data_of(hwnd, |d| d.sel = Some(i));
                    }
                    Some(i as i64)
                }
                None => Some(CB_ERR),
            }
        }
        0x0150 => data_of(hwnd, |d| d.item_data.get(w as usize).map_or(CB_ERR, |&v| v as i64)), // CB_GETITEMDATA
        0x0151 => data_of(hwnd, |d| match d.item_data.get_mut(w as usize) { // CB_SETITEMDATA
            Some(v) => {
                *v = l as u64;
                1
            }
            None => CB_ERR,
        }),
        0x0154 => Some(ROW_HEIGHT as i64), // CB_GETITEMHEIGHT
        // CB_LIMITTEXT, CB_SHOWDROPDOWN, CB_SETITEMHEIGHT, CB_SETEXTENDEDUI,
        // CB_GETDROPPEDSTATE, CB_SETDROPPEDWIDTH, CB_INITSTORAGE.
        0x0141 | 0x014F | 0x0153 | 0x0155 | 0x0157 | 0x0160 | 0x0161 => Some(if msg == 0x0141 { 1 } else { 0 }),
        0x0201 | 0x0203 => { // WM_LBUTTONDOWN, WM_LBUTTONDBLCLK
            select(1);
            return Some(0);
        }
        WM_MOUSEWHEEL => {
            select(if ((w >> 16) as u16 as i16) < 0 { 1 } else { -1 });
            return Some(0);
        }
        WM_KEYDOWN => {
            match w as u32 {
                0x26 | 0x25 => select(-1), // VK_UP, VK_LEFT
                0x28 | 0x27 => select(1),  // VK_DOWN, VK_RIGHT
                _ => return None,
            }
            return Some(0);
        }
        WM_GETDLGCODE => return Some(0x1), // DLGC_WANTARROWS
        _ => None,
    };
    if result.is_some() && !matches!(msg, 0x0146 | 0x0147 | 0x0148 | 0x0149 | 0x0150 | 0x0154) {
        redraw(hwnd);
    }
    result
}

fn paint_combo(dc: u64, _hwnd: u64, data: &ControlData, width: i32, height: i32) {
    let white = CreateSolidBrush(0x00FF_FFFF);
    FillRect(dc, [0, 0, width, height].as_ptr(), white);
    DeleteObject(white);
    let text = data.sel.and_then(|s| data.rows.get(s)).and_then(|r| r.first()).cloned().unwrap_or_default();
    SetBkMode(dc, 1);
    SetTextColor(dc, 0);
    let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
    let mut rect = [4, 0, (width - 20).max(4), height];
    DrawTextW(dc, wide.as_ptr(), -1, rect.as_mut_ptr(), 0x20 | 0x4 | 0x800); // SINGLELINE | VCENTER | NOPREFIX
    // The drop-down arrow: a small triangle of shrinking rows.
    let arrow = CreateSolidBrush(0x0040_4040);
    let (cx, cy) = (width - 10, height / 2 - 2);
    for row in 0..4 {
        FillRect(dc, [cx - 4 + row, cy + row, cx + 4 - row, cy + row + 1].as_ptr(), arrow);
    }
    DeleteObject(arrow);
    let border = CreateSolidBrush(0x00A0_A0A0);
    FrameRect(dc, [0, 0, width, height].as_ptr(), border);
    DeleteObject(border);
}

// ---- tree view ------------------------------------------------------------------

const TVI_ROOT: u64 = (-0x10000i64) as u64;
const TVI_FIRST: u64 = (-0xFFFFi64) as u64;
const TVI_LAST: u64 = (-0xFFFEi64) as u64;
const TVI_SORT: u64 = (-0xFFFDi64) as u64;
const TVIS_SELECTED: u32 = 0x2;
const TVIS_EXPANDED: u32 = 0x20;
const TVS_HASBUTTONS: u32 = 0x1;
const TVS_LINESATROOT: u32 = 0x4;
const TREE_ROW: i32 = 18;
const TREE_INDENT: i32 = 19;

fn node_index(d: &ControlData, handle: u64) -> Option<usize> {
    d.nodes.iter().position(|n| n.handle == handle)
}

fn depth_of(d: &ControlData, i: usize) -> i32 {
    let mut depth = 0;
    let mut parent = d.nodes[i].parent;
    while parent != 0 && depth < 64 {
        depth += 1;
        parent = node_index(d, parent).map_or(0, |p| d.nodes[p].parent);
    }
    depth
}

/// The end (exclusive) of item `i`'s subtree in display order.
fn subtree_end(d: &ControlData, i: usize) -> usize {
    let mut end = i + 1;
    while end < d.nodes.len() && is_descendant(d, end, d.nodes[i].handle) {
        end += 1;
    }
    end
}

fn is_descendant(d: &ControlData, i: usize, ancestor: u64) -> bool {
    let mut parent = d.nodes[i].parent;
    for _ in 0..64 {
        if parent == 0 {
            return false;
        }
        if parent == ancestor {
            return true;
        }
        parent = node_index(d, parent).map_or(0, |p| d.nodes[p].parent);
    }
    false
}

fn has_children(d: &ControlData, i: usize) -> bool {
    d.nodes[i].children > 0 || d.nodes.get(i + 1).is_some_and(|n| n.parent == d.nodes[i].handle)
}

/// The rows on screen: items whose ancestors are all expanded.
fn visible_rows(d: &ControlData) -> Vec<usize> {
    (0..d.nodes.len())
        .filter(|&i| {
            let mut parent = d.nodes[i].parent;
            for _ in 0..64 {
                if parent == 0 {
                    return true;
                }
                match node_index(d, parent) {
                    Some(p) if d.nodes[p].state & TVIS_EXPANDED != 0 => parent = d.nodes[p].parent,
                    _ => return false,
                }
            }
            false
        })
        .collect()
}

/// Columns of a row: (button x, state icon x, label x); the button column
/// exists for children, or for roots with TVS_LINESATROOT.
fn row_layout(style: u32, depth: i32, state_icon: bool) -> (Option<i32>, Option<i32>, i32) {
    let mut x = 2 + depth * TREE_INDENT;
    let button = (style & TVS_HASBUTTONS != 0 && (depth > 0 || style & TVS_LINESATROOT != 0)).then(|| {
        let b = x;
        x += TREE_INDENT;
        b
    });
    let icon = state_icon.then(|| {
        let i = x;
        x += 16;
        i
    });
    (button, icon, x + 2)
}

fn tree_style(hwnd: u64) -> u32 {
    with(|g| g.windows.get(&hwnd).map_or(0, |w| w.style))
}

/// TVM_HITTEST: (item, TVHT_* flags) at (x, y) in client coordinates.
fn tree_hit(hwnd: u64, x: i32, y: i32) -> (u64, u32) {
    const TVHT_NOWHERE: u32 = 0x1;
    const TVHT_ONITEMLABEL: u32 = 0x4;
    const TVHT_ONITEMINDENT: u32 = 0x8;
    const TVHT_ONITEMBUTTON: u32 = 0x10;
    const TVHT_ONITEMSTATEICON: u32 = 0x40;
    let style = tree_style(hwnd);
    data_of(hwnd, |d| {
        let rows = visible_rows(d);
        if y < 2 {
            return (0, TVHT_NOWHERE);
        }
        let Some(&i) = rows.get(d.top + ((y - 2) / TREE_ROW) as usize) else { return (0, TVHT_NOWHERE) };
        let node = &d.nodes[i];
        let (button, icon, label) = row_layout(style, depth_of(d, i), node.state >> 12 != 0);
        let flags = if button.is_some_and(|b| x >= b && x < b + TREE_INDENT) {
            TVHT_ONITEMBUTTON
        } else if icon.is_some_and(|c| x >= c && x < c + 16) {
            TVHT_ONITEMSTATEICON
        } else if x >= label {
            TVHT_ONITEMLABEL
        } else {
            TVHT_ONITEMINDENT
        };
        (node.handle, flags)
    })
    .unwrap_or((0, TVHT_NOWHERE))
}

/// Reads the TVITEM at `item` (the program's layout) into `node`.
fn tree_set_fields(node: &mut TreeNode, item: u64, wide: bool) {
    let p = ptr_size();
    // SAFETY: the caller's TVITEM.
    let (mask, state, state_mask) = unsafe { (teb::get::<u32>(item, 0), teb::get::<u32>(item, 2 * p), teb::get::<u32>(item, 2 * p + 4)) };
    let text_at = 2 * p + 8;
    let after_text = text_at + p + 4;
    if mask & 0x1 != 0 {
        let t = read_ptr(item, text_at);
        // LPSTR_TEXTCALLBACK (-1) is not supported: no text.
        node.text = if t == u64::MAX || t == 0 {
            String::new()
        } else if wide {
            wide_text(t)
        } else {
            // SAFETY: a NUL-terminated ANSI string of the caller.
            unsafe { path::ansi_to_string(t as *const u8) }.unwrap_or_default()
        };
    }
    // SAFETY: the caller's TVITEM.
    unsafe {
        if mask & 0x2 != 0 {
            node.image = teb::get::<i32>(item, after_text);
        }
        if mask & 0x20 != 0 {
            node.selected_image = teb::get::<i32>(item, after_text + 4);
        }
        if mask & 0x40 != 0 {
            node.children = teb::get::<i32>(item, after_text + 8);
        }
    }
    if mask & 0x4 != 0 {
        node.lparam = read_ptr(item, lparam_offset());
    }
    if mask & 0x8 != 0 {
        node.state = (node.state & !state_mask) | (state & state_mask);
    }
}

/// The offset of TVITEM.lParam: 36 in a 32-bit program, 48 on x64.
fn lparam_offset() -> usize {
    if ptr_size() == 4 { 36 } else { 48 }
}

/// Fills the caller's TVITEM at `item` from `node` for the fields its mask asks.
fn tree_get_fields(node: &TreeNode, has_kids: bool, item: u64, wide: bool) {
    let p = ptr_size();
    // SAFETY: the caller's TVITEM.
    let (mask, state_mask) = unsafe { (teb::get::<u32>(item, 0), teb::get::<u32>(item, 2 * p + 4)) };
    let text_at = 2 * p + 8;
    let after_text = text_at + p + 4;
    if mask & 0x1 != 0 {
        let buf = read_ptr(item, text_at);
        // SAFETY: the caller's TVITEM.
        let max = unsafe { teb::get::<i32>(item, text_at + p) };
        copy_text(&node.text, buf, max as i64, wide);
    }
    // SAFETY: the caller's TVITEM.
    unsafe {
        if mask & 0x8 != 0 {
            teb::put(item, 2 * p, node.state & if state_mask == 0 { u32::MAX } else { state_mask });
        }
        if mask & 0x2 != 0 {
            teb::put(item, after_text, node.image);
        }
        if mask & 0x20 != 0 {
            teb::put(item, after_text + 4, node.selected_image);
        }
        if mask & 0x40 != 0 {
            teb::put(item, after_text + 8, if has_kids { 1i32 } else { node.children });
        }
    }
    if mask & 0x4 != 0 {
        write_ptr(item, lparam_offset(), node.lparam);
    }
}

/// TVN_SELCHANGEDW with NMTREEVIEW (action, itemOld, itemNew, ptDrag).
fn notify_selection(hwnd: u64, old: Option<TreeNode>, new: Option<TreeNode>) {
    const TVN_SELCHANGEDW: i32 = -451;
    let p = ptr_size();
    let item_size = if p == 4 { 40 } else { 56 };
    // action (padded), itemOld, itemNew, ptDrag.
    let action_size = p;
    let mut extra = alloc::vec![0u8; action_size + 2 * item_size + 8];
    let mut put_item = |at: usize, node: &Option<TreeNode>| {
        let Some(n) = node else { return };
        extra[at..at + 4].copy_from_slice(&(0x1Cu32).to_le_bytes()); // TVIF_HANDLE | TVIF_STATE | TVIF_PARAM
        extra[at + p..at + p + 8.min(p)].copy_from_slice(&n.handle.to_le_bytes()[..p]);
        extra[at + 2 * p..at + 2 * p + 4].copy_from_slice(&n.state.to_le_bytes());
        let lp = if p == 4 { 36 } else { 48 };
        extra[at + lp..at + lp + p].copy_from_slice(&n.lparam.to_le_bytes()[..p]);
    };
    put_item(action_size, &old);
    put_item(action_size + item_size, &new);
    notify(hwnd, TVN_SELCHANGEDW, &extra);
}

fn tree_select(hwnd: u64, handle: u64) {
    let change = data_of(hwnd, |d| {
        if d.selected == handle {
            return None;
        }
        let old = node_index(d, d.selected).map(|i| {
            d.nodes[i].state &= !TVIS_SELECTED;
            d.nodes[i].clone()
        });
        let new = node_index(d, handle).map(|i| {
            d.nodes[i].state |= TVIS_SELECTED;
            d.nodes[i].clone()
        });
        d.selected = if new.is_some() { handle } else { 0 };
        Some((old, new))
    })
    .flatten();
    if let Some((old, new)) = change {
        redraw(hwnd);
        notify_selection(hwnd, old, new);
    }
}

fn tree_message(hwnd: u64, msg: u32, w: u64, l: i64) -> Option<i64> {
    const TVM_INSERTITEMA: u32 = 0x1100;
    const TVM_DELETEITEM: u32 = 0x1101;
    const TVM_EXPAND: u32 = 0x1102;
    const TVM_GETITEMRECT: u32 = 0x1104;
    const TVM_GETCOUNT: u32 = 0x1105;
    const TVM_GETINDENT: u32 = 0x1106;
    const TVM_SETINDENT: u32 = 0x1107;
    const TVM_GETIMAGELIST: u32 = 0x1108;
    const TVM_SETIMAGELIST: u32 = 0x1109;
    const TVM_GETNEXTITEM: u32 = 0x110A;
    const TVM_SELECTITEM: u32 = 0x110B;
    const TVM_GETITEMA: u32 = 0x110C;
    const TVM_SETITEMA: u32 = 0x110D;
    const TVM_GETVISIBLECOUNT: u32 = 0x1110;
    const TVM_HITTEST: u32 = 0x1111;
    const TVM_ENSUREVISIBLE: u32 = 0x1114;
    const TVM_SETITEMHEIGHT: u32 = 0x111B;
    const TVM_GETITEMHEIGHT: u32 = 0x111C;
    const TVM_SETBKCOLOR: u32 = 0x111D;
    const TVM_SETTEXTCOLOR: u32 = 0x111E;
    const TVM_GETITEMSTATE: u32 = 0x1127;
    const TVM_INSERTITEMW: u32 = 0x1132;
    const TVM_GETITEMW: u32 = 0x113E;
    const TVM_SETITEMW: u32 = 0x113F;
    const NM_CLICK: i32 = -2;
    const NM_DBLCLK: i32 = -3;
    let p = ptr_size();
    let result = match msg {
        TVM_INSERTITEMA | TVM_INSERTITEMW => {
            let base = l as u64;
            if base == 0 {
                return Some(0);
            }
            let parent = match read_ptr(base, 0) {
                TVI_ROOT => 0,
                h => h,
            };
            let after = read_ptr(base, p);
            let mut node = TreeNode::default();
            tree_set_fields(&mut node, base + 2 * p as u64, msg == TVM_INSERTITEMW);
            data_of(hwnd, |d| {
                let parent_index = if parent == 0 { None } else { Some(node_index(d, parent)?) };
                d.next_handle = d.next_handle.max(0x1000) + 1;
                node.handle = d.next_handle;
                node.parent = parent;
                let first = parent_index.map_or(0, |i| i + 1);
                let end = parent_index.map_or(d.nodes.len(), |i| subtree_end(d, i));
                let at = match after {
                    TVI_FIRST => first,
                    TVI_SORT => {
                        let key = node.text.to_lowercase();
                        let mut at = first;
                        while at < end {
                            if d.nodes[at].parent == parent && d.nodes[at].text.to_lowercase() > key {
                                break;
                            }
                            at += 1;
                        }
                        at
                    }
                    TVI_LAST | 0 => end,
                    h => node_index(d, h).filter(|&i| d.nodes[i].parent == parent).map_or(end, |i| subtree_end(d, i)),
                };
                let handle = node.handle;
                d.nodes.insert(at, node);
                Some(handle as i64)
            })
            .flatten()
            .or(Some(0))
        }
        TVM_DELETEITEM => data_of(hwnd, |d| {
            let target = l as u64;
            if target == 0 || target == TVI_ROOT {
                d.nodes.clear();
                d.selected = 0;
                d.top = 0;
                return 1;
            }
            let Some(i) = node_index(d, target) else { return 0 };
            let end = subtree_end(d, i);
            if d.nodes[i..end].iter().any(|n| n.handle == d.selected) {
                d.selected = 0;
            }
            d.nodes.drain(i..end);
            1
        }),
        TVM_EXPAND => data_of(hwnd, |d| {
            let Some(i) = node_index(d, l as u64) else { return 0 };
            let expanded = d.nodes[i].state & TVIS_EXPANDED != 0;
            let next = match w as u32 & 0x3 {
                1 => false,
                2 => true,
                3 => !expanded,
                _ => expanded,
            };
            if next {
                d.nodes[i].state |= TVIS_EXPANDED;
            } else {
                d.nodes[i].state &= !TVIS_EXPANDED;
            }
            1
        }),
        TVM_GETITEMRECT => {
            let rect = l as u64;
            if rect == 0 {
                return Some(0);
            }
            let handle = read_ptr(rect, 0);
            let style = tree_style(hwnd);
            let width = with(|g| g.windows.get(&hwnd).map_or(0, |w| w.size.0 as i32));
            let found = data_of(hwnd, |d| {
                let i = node_index(d, handle)?;
                let row = visible_rows(d).iter().position(|&r| r == i)?;
                let y = 2 + (row as i32 - d.top as i32) * TREE_ROW;
                let (_, _, label) = row_layout(style, depth_of(d, i), d.nodes[i].state >> 12 != 0);
                Some((y, label))
            })
            .flatten();
            match found {
                Some((y, label)) => {
                    let x = if w != 0 { label } else { 0 };
                    // SAFETY: the caller's RECT.
                    unsafe {
                        teb::put(rect, 0, x);
                        teb::put(rect, 4, y);
                        teb::put(rect, 8, width);
                        teb::put(rect, 12, y + TREE_ROW);
                    }
                    Some(1)
                }
                None => Some(0),
            }
        }
        TVM_GETCOUNT => data_of(hwnd, |d| d.nodes.len() as i64),
        TVM_GETINDENT => Some(TREE_INDENT as i64),
        TVM_SETINDENT | TVM_SETITEMHEIGHT => Some(0),
        TVM_GETITEMHEIGHT => Some(TREE_ROW as i64),
        TVM_GETVISIBLECOUNT => Some(with(|g| g.windows.get(&hwnd).map_or(1, |w| (w.size.1 as i32 - 4) / TREE_ROW)) as i64),
        TVM_GETIMAGELIST => data_of(hwnd, |d| if w == 2 { d.state_images as i64 } else { 0 }),
        TVM_SETIMAGELIST => data_of(hwnd, |d| if w == 2 { core::mem::replace(&mut d.state_images, l as u64) as i64 } else { 0 }),
        TVM_GETNEXTITEM => data_of(hwnd, |d| {
            let item = l as u64;
            let index = node_index(d, item);
            let rows = visible_rows(d);
            let found = match w as u32 {
                0 => d.nodes.first().map(|n| n.handle), // TVGN_ROOT
                1 => index.and_then(|i| { // TVGN_NEXT: next sibling
                    let parent = d.nodes[i].parent;
                    d.nodes.get(subtree_end(d, i)).filter(|n| n.parent == parent).map(|n| n.handle)
                }),
                2 => index.and_then(|i| { // TVGN_PREVIOUS
                    let parent = d.nodes[i].parent;
                    d.nodes[..i].iter().rev().find(|n| n.parent == parent).map(|n| n.handle)
                }),
                3 => index.map(|i| d.nodes[i].parent).filter(|&h| h != 0), // TVGN_PARENT
                4 => { // TVGN_CHILD
                    let parent = if item == 0 || item == TVI_ROOT { 0 } else { item };
                    match parent {
                        0 => d.nodes.first().map(|n| n.handle),
                        _ => index.and_then(|i| d.nodes.get(i + 1)).filter(|n| n.parent == parent).map(|n| n.handle),
                    }
                }
                5 => rows.get(d.top).map(|&i| d.nodes[i].handle), // TVGN_FIRSTVISIBLE
                6 => index.and_then(|i| rows.iter().position(|&r| r == i)).and_then(|r| rows.get(r + 1)).map(|&i| d.nodes[i].handle),
                7 => index.and_then(|i| rows.iter().position(|&r| r == i)).filter(|&r| r > 0).map(|r| d.nodes[rows[r - 1]].handle),
                9 => (d.selected != 0).then_some(d.selected), // TVGN_CARET
                0xA => rows.last().map(|&i| d.nodes[i].handle), // TVGN_LASTVISIBLE
                _ => None,
            };
            found.unwrap_or(0) as i64
        }),
        TVM_SELECTITEM => {
            match w as u32 & 0xFF {
                9 => tree_select(hwnd, l as u64),
                5 => {
                    data_of(hwnd, |d| {
                        if let Some(i) = node_index(d, l as u64) {
                            if let Some(r) = visible_rows(d).iter().position(|&x| x == i) {
                                d.top = r;
                            }
                        }
                    });
                }
                _ => {}
            }
            Some(1)
        }
        TVM_GETITEMA | TVM_GETITEMW => {
            let item = l as u64;
            if item == 0 {
                return Some(0);
            }
            let handle = read_ptr(item, 4.max(p));
            let node = data_of(hwnd, |d| node_index(d, handle).map(|i| (d.nodes[i].clone(), has_children(d, i)))).flatten();
            match node {
                Some((n, kids)) => {
                    tree_get_fields(&n, kids, item, msg == TVM_GETITEMW);
                    Some(1)
                }
                None => Some(0),
            }
        }
        TVM_SETITEMA | TVM_SETITEMW => {
            let item = l as u64;
            if item == 0 {
                return Some(0);
            }
            let handle = read_ptr(item, 4.max(p));
            data_of(hwnd, |d| match node_index(d, handle) {
                Some(i) => {
                    tree_set_fields(&mut d.nodes[i], item, msg == TVM_SETITEMW);
                    1
                }
                None => 0,
            })
        }
        TVM_GETITEMSTATE => data_of(hwnd, |d| node_index(d, w).map_or(0, |i| (d.nodes[i].state & l as u32) as i64)),
        TVM_HITTEST => {
            let info = l as u64;
            if info == 0 {
                return Some(0);
            }
            // SAFETY: the caller's TVHITTESTINFO: pt, flags, hItem.
            let (x, y) = unsafe { (teb::get::<i32>(info, 0), teb::get::<i32>(info, 4)) };
            let (item, flags) = tree_hit(hwnd, x, y);
            // SAFETY: as above.
            unsafe { teb::put(info, 8, flags) };
            write_ptr(info, if p == 4 { 12 } else { 16 }, item);
            return Some(item as i64);
        }
        TVM_ENSUREVISIBLE => data_of(hwnd, |d| {
            let Some(i) = node_index(d, l as u64) else { return 0 };
            // Expand the ancestors, then scroll the row into view.
            let mut parent = d.nodes[i].parent;
            while let Some(pi) = node_index(d, parent) {
                d.nodes[pi].state |= TVIS_EXPANDED;
                parent = d.nodes[pi].parent;
            }
            if let Some(r) = visible_rows(d).iter().position(|&x| x == i) {
                if r < d.top {
                    d.top = r;
                }
            }
            1
        }),
        TVM_SETBKCOLOR => data_of(hwnd, |d| {
            let old = d.back_colour.map_or(-1, |c| c as i64);
            d.back_colour = (l as i32 != -1).then_some(colour_ref(l as u32));
            old
        }),
        TVM_SETTEXTCOLOR => data_of(hwnd, |d| {
            let old = d.text_colour.map_or(-1, |c| c as i64);
            d.text_colour = (l as i32 != -1).then_some(colour_ref(l as u32));
            old
        }),
        0x0201 | 0x0203 => { // WM_LBUTTONDOWN, WM_LBUTTONDBLCLK
            let (x, y) = ((l & 0xFFFF) as i16 as i32, ((l >> 16) & 0xFFFF) as i16 as i32);
            let (item, flags) = tree_hit(hwnd, x, y);
            if item != 0 {
                if flags & 0x10 != 0 || msg == 0x0203 {
                    data_of(hwnd, |d| {
                        if let Some(i) = node_index(d, item) {
                            d.nodes[i].state ^= TVIS_EXPANDED;
                        }
                    });
                    redraw(hwnd);
                }
                tree_select(hwnd, item);
            }
            notify(hwnd, if msg == 0x0203 { NM_DBLCLK } else { NM_CLICK }, &[]);
            return Some(0);
        }
        WM_KEYDOWN => {
            let target = data_of(hwnd, |d| {
                let rows = visible_rows(d);
                let row = node_index(d, d.selected).and_then(|i| rows.iter().position(|&r| r == i));
                let next = match (w as u32, row) {
                    (0x26, Some(r)) if r > 0 => Some(rows[r - 1]), // VK_UP
                    (0x28, Some(r)) => rows.get(r + 1).copied(),   // VK_DOWN
                    (0x26 | 0x28, None) => rows.first().copied(),
                    (0x27 | 0x25, Some(r)) => {
                        // VK_RIGHT expands, VK_LEFT collapses.
                        let i = rows[r];
                        if w as u32 == 0x27 {
                            d.nodes[i].state |= TVIS_EXPANDED;
                        } else {
                            d.nodes[i].state &= !TVIS_EXPANDED;
                        }
                        None
                    }
                    _ => None,
                };
                next.map(|i| d.nodes[i].handle)
            })
            .flatten();
            if let Some(h) = target {
                tree_select(hwnd, h);
            }
            redraw(hwnd);
            return Some(0);
        }
        WM_MOUSEWHEEL => {
            let delta = (w >> 16) as u16 as i16 as i32;
            data_of(hwnd, |d| {
                let total = visible_rows(d).len() as i64;
                d.top = (d.top as i64 - (delta * WHEEL_LINES / 120) as i64).clamp(0, (total - 1).max(0)) as usize;
            });
            redraw(hwnd);
            return Some(0);
        }
        WM_GETDLGCODE => return Some(0x1), // DLGC_WANTARROWS
        // TVM_SETEXTENDEDSTYLE, TVM_SETSCROLLTIME, CCM_SETUNICODEFORMAT, CCM_SETVERSION.
        0x112C | 0x1121 | 0x2005 | 0x2007 => Some(0),
        _ => None,
    };
    if result.is_some() && !matches!(msg, TVM_GETCOUNT | TVM_GETNEXTITEM | TVM_GETITEMA | TVM_GETITEMW | TVM_GETITEMSTATE | TVM_GETITEMRECT | TVM_GETINDENT | TVM_GETITEMHEIGHT | TVM_GETVISIBLECOUNT | TVM_GETIMAGELIST) {
        redraw(hwnd);
    }
    result
}

/// Rows with their expand box, the state image as a check box (index 1
/// clear, 2 checked, 3 partly; 4 to 6 the read-only ones, greyed), the
/// label, and the selection.
fn paint_tree(dc: u64, hwnd: u64, data: &ControlData, width: i32, height: i32) {
    let style = tree_style(hwnd);
    let background = CreateSolidBrush(data.back_colour.unwrap_or(0x00FF_FFFF));
    FillRect(dc, [0, 0, width, height].as_ptr(), background);
    DeleteObject(background);
    let dark = CreateSolidBrush(0x0040_4040);
    let grey = CreateSolidBrush(0x00A0_A0A0);
    let white = CreateSolidBrush(0x00FF_FFFF);
    let highlight = CreateSolidBrush(0x00D7_7800);
    SetBkMode(dc, 1);
    let rows = visible_rows(data);
    let fit = ((height - 4) / TREE_ROW).max(1) as usize;
    for (n, &i) in rows.iter().skip(data.top).take(fit).enumerate() {
        let node = &data.nodes[i];
        let y = 2 + n as i32 * TREE_ROW;
        let state_icon = node.state >> 12;
        let (button, icon, label) = row_layout(style, depth_of(data, i), state_icon != 0);
        if let (Some(b), true) = (button, has_children(data, i)) {
            let (bx, by) = (b + 4, y + 4);
            FillRect(dc, [bx, by, bx + 9, by + 9].as_ptr(), white);
            FrameRect(dc, [bx, by, bx + 9, by + 9].as_ptr(), grey);
            FillRect(dc, [bx + 2, by + 4, bx + 7, by + 5].as_ptr(), dark);
            if node.state & TVIS_EXPANDED == 0 {
                FillRect(dc, [bx + 4, by + 2, bx + 5, by + 7].as_ptr(), dark);
            }
        }
        if let Some(c) = icon {
            let (cx, cy) = (c + 1, y + 2);
            let read_only = state_icon > 3;
            FillRect(dc, [cx, cy, cx + 13, cy + 13].as_ptr(), if read_only { grey } else { white });
            FrameRect(dc, [cx, cy, cx + 13, cy + 13].as_ptr(), dark);
            match state_icon % 3 {
                2 => {
                    // A tick: two short strokes of 2-pixel squares.
                    for k in 0..3 {
                        FillRect(dc, [cx + 3 + k, cy + 6 + k, cx + 5 + k, cy + 8 + k].as_ptr(), dark);
                    }
                    for k in 0..4 {
                        FillRect(dc, [cx + 6 + k, cy + 7 - k, cx + 8 + k, cy + 9 - k].as_ptr(), dark);
                    }
                }
                0 => {
                    FillRect(dc, [cx + 3, cy + 3, cx + 10, cy + 10].as_ptr(), grey);
                }
                _ => {}
            }
        }
        let text: Vec<u16> = node.text.encode_utf16().chain(core::iter::once(0)).collect();
        let mut rect = [label, y, width - 2, y + TREE_ROW];
        if node.handle == data.selected {
            let mut measure_rect = rect;
            DrawTextW(dc, text.as_ptr(), -1, measure_rect.as_mut_ptr(), 0x20 | 0x800 | 0x400); // DT_CALCRECT
            FillRect(dc, [label - 2, y + 1, (measure_rect[2] + 2).min(width - 2), y + TREE_ROW - 1].as_ptr(), highlight);
            SetTextColor(dc, 0x00FF_FFFF);
        } else {
            SetTextColor(dc, data.text_colour.unwrap_or(0));
        }
        DrawTextW(dc, text.as_ptr(), -1, rect.as_mut_ptr(), 0x20 | 0x4 | 0x800);
    }
    for b in [dark, grey, white, highlight] {
        DeleteObject(b);
    }
    let border = CreateSolidBrush(0x00A0_A0A0);
    FrameRect(dc, [0, 0, width, height].as_ptr(), border);
    DeleteObject(border);
}
