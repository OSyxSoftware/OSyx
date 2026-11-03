//! user32 functions the Steam client's `vgui2_s.dll` imports: icon copies
//! and drawing, the current cursor, ANSI forms of window and image calls,
//! coalescable timers and shutdown block reasons.
use super::*;
use core::sync::atomic::{AtomicU64, Ordering};

const DI_MASK: u32 = 0x1;
const DI_IMAGE: u32 = 0x2;
const DI_DEFAULTSIZE: u32 = 0x8;
const LR_COPYDELETEORG: u32 = 0x8;
/// SM_CXICON / SM_CYICON.
const ICON_SIZE: u32 = 32;

/// Shared system icons and cursors (LoadIcon/LoadCursor): their handle is
/// the resource id and they have no image of their own.
fn is_system_icon(h: u64) -> bool {
    (0x1_0000..0x2_0000).contains(&h)
}

/// A copy of an icon or cursor, its bitmaps scaled to `w` x `h` when given
/// (a monochrome icon's mask holds the AND and XOR halves stacked).
fn copy_icon(g: &mut Gui, icon: u64, w: i32, h: i32) -> Option<u64> {
    let (is_icon, hotspot, mask, color) = g.icons.get(&icon).map(|i| (i.is_icon, i.hotspot, i.mask, i.color))?;
    let scale = |g: &mut Gui, b: u64, stacked: bool| {
        if w > 0 || h > 0 {
            gdi::scaled_bitmap(g, b, w, if stacked && h > 0 { h * 2 } else { h })
        } else {
            gdi::duplicate_bitmap(g, b)
        }
    };
    let m = scale(g, mask, color == 0)?;
    let c = if color == 0 { 0 } else { scale(g, color, false).unwrap_or(0) };
    let handle = g.handle();
    g.icons.insert(handle, Icon { is_icon, hotspot, mask: m, color: c });
    Some(handle)
}

extern "win64" fn CopyIcon(icon: u64) -> u64 {
    if is_system_icon(icon) {
        return icon;
    }
    with(|g| copy_icon(g, icon, 0, 0)).unwrap_or_else(|| fail(ERROR_INVALID_CURSOR_HANDLE, 0))
}

/// CopyImage: bitmaps, icons and cursors, scaled to `w` x `h` when given;
/// LR_COPYDELETEORG deletes the original.
extern "win64" fn CopyImage(image: u64, kind: u32, w: i32, h: i32, flags: u32) -> u64 {
    let copy = match kind {
        IMAGE_BITMAP => with(|g| if w > 0 || h > 0 { gdi::scaled_bitmap(g, image, w, h) } else { gdi::duplicate_bitmap(g, image) }),
        IMAGE_ICON | IMAGE_CURSOR if is_system_icon(image) => return image,
        IMAGE_ICON | IMAGE_CURSOR => with(|g| copy_icon(g, image, w, h)),
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    let Some(copy) = copy else { return fail(ERROR_INVALID_HANDLE, 0) };
    if flags & LR_COPYDELETEORG != 0 {
        if kind == IMAGE_BITMAP {
            DeleteObject(image);
        } else {
            DestroyIcon(image);
        }
    }
    copy
}

/// DrawIconEx: the icon's image through its mask (AND, then XOR), or
/// alpha-blended when its colour bitmap has an alpha channel, scaled to
/// `w` x `h` (0: the icon's own size, or SM_CXICON with DI_DEFAULTSIZE).
/// A flicker-free brush fills the rectangle first, as Windows' off-screen
/// copy does.
#[allow(clippy::too_many_arguments)]
extern "win64" fn DrawIconEx(dc: u64, x: i32, y: i32, icon: u64, w: i32, h: i32, _step: u32, brush: u64, flags: u32) -> Bool {
    if is_system_icon(icon) {
        // No image to draw: OSyx's pointer and stock icons are its own.
        return TRUE;
    }
    let images = with(|g| {
        let i = g.icons.get(&icon)?;
        let (mask, color) = (i.mask, i.color);
        let m = gdi::bitmap_pixels(g, mask)?;
        let c = if color == 0 { None } else { gdi::bitmap_pixels(g, color) };
        Some((m, c))
    });
    let Some(((mw, mh, mpx), color)) = images else { return fail(ERROR_INVALID_CURSOR_HANDLE, FALSE) };
    // A monochrome icon stacks its AND mask over its XOR image.
    let (iw, ih) = match &color {
        Some((cw, ch, _)) => (*cw, *ch),
        None => (mw, mh / 2),
    };
    if iw == 0 || ih == 0 {
        return TRUE;
    }
    let (dw, dh) = match (w, h, flags & DI_DEFAULTSIZE != 0) {
        (0, 0, true) => (ICON_SIZE, ICON_SIZE),
        (0, 0, false) => (iw, ih),
        (w, h, _) => (if w > 0 { w as u32 } else { iw }, if h > 0 { h as u32 } else { ih }),
    };
    if brush != 0 {
        // fill() works in device pixels: the icon's origin through the DC's
        // transform, as the drawing below uses it.
        let origin = with(|g| g.dcs.get(&dc).map(|d| d.transform.point(x, y)));
        let (ox, oy) = origin.unwrap_or((x, y));
        fill(dc, Rect::new(ox, oy, dw, dh), brush);
    }
    let alpha = color.as_ref().is_some_and(|(_, _, px)| px.iter().any(|p| p >> 24 != 0));
    let want_mask = flags & DI_MASK != 0;
    let want_image = flags & DI_IMAGE != 0;
    let ok = draw(dc, |s, d, _, clip| {
        let (ox, oy) = d.transform.point(x, y);
        let r = Rect::new(ox, oy, dw, dh).intersect(&clip);
        for py in r.y..r.y + r.h as i32 {
            for px in r.x..r.x + r.w as i32 {
                let sx = ((px - ox) as u64 * iw as u64 / dw as u64) as u32;
                let sy = ((py - oy) as u64 * ih as u64 / dh as u64) as u32;
                let and = mpx[(sy * mw + sx) as usize] & 0xFF_FFFF != 0;
                let xor = match &color {
                    Some((cw, _, cpx)) => cpx[(sy * cw + sx) as usize],
                    None => mpx[((sy + ih) * mw + sx) as usize] & 0xFF_FFFF,
                };
                let under = s.get(px, py).unwrap_or(0);
                let out = if alpha && want_mask && want_image {
                    // Per-pixel alpha (straight, as icon images store it).
                    let a = xor >> 24;
                    let mut c = 0u32;
                    for shift in [0, 8, 16] {
                        let (sc, dc) = ((xor >> shift) & 255, (under >> shift) & 255);
                        c |= ((sc * a + dc * (255 - a)) / 255) << shift;
                    }
                    c | (under & 0xFF00_0000)
                } else {
                    let mut c = under;
                    if want_mask {
                        c = if and { c } else { c & 0xFF00_0000 };
                    }
                    if want_image {
                        c = if want_mask { c ^ (xor & 0xFF_FFFF) } else { (c & 0xFF00_0000) | (xor & 0xFF_FFFF) };
                    }
                    c
                };
                s.put(px, py, out);
            }
        }
        (TRUE, r)
    });
    ok.unwrap_or_else(|| fail(ERROR_INVALID_HANDLE, FALSE))
}

extern "win64" fn DrawIcon(dc: u64, x: i32, y: i32, icon: u64) -> Bool {
    DrawIconEx(dc, x, y, icon, 0, 0, 0, 0, DI_MASK | DI_IMAGE | DI_DEFAULTSIZE)
}

/// The cursor SetCursor last set (the thread's shape on Windows; OSyx
/// draws one pointer for the process).
static CURSOR: AtomicU64 = AtomicU64::new(0x1_0000 + 32512); // IDC_ARROW

pub(super) fn set_cursor(c: u64) -> u64 {
    CURSOR.swap(c, Ordering::SeqCst)
}

extern "win64" fn GetCursor() -> u64 {
    CURSOR.load(Ordering::SeqCst)
}

/// An ANSI string argument, or the value itself when it is an atom or a
/// resource id (below 0x10000).
fn ansi_or_id(p: u64) -> Result<Option<Vec<u16>>, ()> {
    if p < 0x10000 {
        return Ok(None);
    }
    // SAFETY: a NUL-terminated ANSI string.
    match unsafe { path::ansi_to_wide(p as *const u8) } {
        Some(mut w) => {
            if w.last() != Some(&0) {
                w.push(0);
            }
            Ok(Some(w))
        }
        None => Err(()),
    }
}

fn as_arg(v: &Option<Vec<u16>>, raw: u64) -> u64 {
    v.as_ref().map_or(raw, |w| w.as_ptr() as u64)
}

extern "win64" fn FindWindowA(class: u64, title: u64) -> u64 {
    let (Ok(c), Ok(t)) = (ansi_or_id(class), ansi_or_id(title)) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    FindWindowW(as_arg(&c, class) as *const u16, as_arg(&t, title) as *const u16)
}

extern "win64" fn LoadImageA(inst: u64, name: u64, kind: u32, w: i32, h: i32, flags: u32) -> u64 {
    let Ok(n) = ansi_or_id(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    LoadImageW(inst, as_arg(&n, name), kind, w, h, flags)
}

/// FlashWindow: like FlashWindowEx, the result says whether the window was
/// the active one.
extern "win64" fn FlashWindow(wnd: u64, _invert: Bool) -> Bool {
    with(|g| {
        if !g.windows.contains_key(&wnd) {
            return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
        }
        (root_window(g, g.focus) == Some(wnd)) as Bool
    })
}

extern "win64" fn RegisterDeviceNotificationA(recipient: u64, filter: *const u8, flags: u32) -> u64 {
    RegisterDeviceNotificationW(recipient, filter, flags)
}

/// SetCoalescableTimer: SetTimer; the tolerance only lets Windows batch
/// expirations, which OSyx's timers do not need.
extern "win64" fn SetCoalescableTimer(wnd: u64, id: u64, ms: u32, proc_: u64, _tolerance: u32) -> u64 {
    SetTimer(wnd, id, ms, proc_)
}

/// Shutdown block reasons by window (shown to the user at shutdown on
/// Windows; OSyx keeps them for ShutdownBlockReasonQuery).
static BLOCK_REASONS: Mutex<BTreeMap<u64, Vec<u16>>> = Mutex::new(BTreeMap::new());

extern "win64" fn ShutdownBlockReasonCreate(wnd: u64, reason: *const u16) -> Bool {
    if !is_window(wnd) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
    }
    let Some(text) = wide(reason) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    BLOCK_REASONS.lock().unwrap().insert(wnd, text.encode_utf16().collect());
    TRUE
}

extern "win64" fn ShutdownBlockReasonDestroy(wnd: u64) -> Bool {
    if !is_window(wnd) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
    }
    BLOCK_REASONS.lock().unwrap().remove(&wnd);
    TRUE
}

/// The reason with its NUL; a NULL buffer asks for the size in characters.
extern "win64" fn ShutdownBlockReasonQuery(wnd: u64, buf: *mut u16, chars: *mut u32) -> Bool {
    if chars.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let Some(text) = BLOCK_REASONS.lock().unwrap().get(&wnd).cloned() else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    // SAFETY: the caller's count and buffer of *chars characters.
    unsafe {
        let need = text.len() as u32 + 1;
        if buf.is_null() {
            *chars = need;
            return TRUE;
        }
        if *chars < need {
            *chars = need;
            return fail(ERROR_INSUFFICIENT_BUFFER, FALSE);
        }
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
        *buf.add(text.len()) = 0;
    }
    TRUE
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CopyIcon,
        CopyImage,
        DrawIconEx,
        DrawIcon,
        GetCursor,
        FindWindowA,
        LoadImageA,
        FlashWindow,
        RegisterDeviceNotificationA,
        SetCoalescableTimer,
        ShutdownBlockReasonCreate,
        ShutdownBlockReasonDestroy,
        ShutdownBlockReasonQuery,
    ]
}
