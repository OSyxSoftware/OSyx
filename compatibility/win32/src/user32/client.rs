//! user32 functions the Steam client and SDL3 import: menu items
//! (InsertMenu, DeleteMenu, CheckMenuItem, GetMenuItemInfo), icons and
//! cursors made from resource data, display modes (ChangeDisplaySettingsEx
//! and the ANSI enumerations), window queries (GetWindowInfo,
//! IsWindowUnicode, GetClassLongA), activation, layered-window attributes,
//! desktops and keyboard helpers (VkKeyScanExA, keybd_event).
//!
//! Structures with pointer-sized members (MENUITEMINFOW, INPUT) have the
//! 32-bit layout when a 32-bit program runs (`wow64::is_active`).

use super::*;

const MF_BYPOSITION: u32 = 0x400;
const MF_POPUP: u32 = 0x10;
const MF_CHECKED: u32 = 0x8;
const MF_SEPARATOR: u32 = 0x800;
const ERROR_INVALID_MENU_HANDLE: u32 = 1401;
const ERROR_MENU_ITEM_NOT_FOUND: u32 = 1456;

/// Offsets of MENUITEMINFOW: (size, hSubMenu, dwItemData, dwTypeData, cch,
/// hbmpItem); the fields before hSubMenu are 4 bytes on both.
pub(super) fn menuiteminfo() -> (u32, u64, u64, u64, u64, u64) {
    if crate::wow64::is_active() { (48, 20, 32, 36, 40, 44) } else { (80, 24, 48, 56, 64, 72) }
}

/// Reads a pointer-sized field of a caller structure.
///
/// # Safety
/// `p + at` must be readable.
pub(super) unsafe fn get_ptr(p: u64, at: u64) -> u64 {
    // SAFETY: as documented.
    unsafe { if crate::wow64::is_active() { teb::get::<u32>(p, at as usize) as u64 } else { teb::get::<u64>(p, at as usize) } }
}

/// Writes a pointer-sized field of a caller structure.
///
/// # Safety
/// `p + at` must be writable.
pub(super) unsafe fn put_ptr(p: u64, at: u64, v: u64) {
    // SAFETY: as documented.
    unsafe {
        if crate::wow64::is_active() {
            teb::put(p, at as usize, v as u32)
        } else {
            teb::put(p, at as usize, v)
        }
    }
}

/// GetMenuItemInfoW: the fields `fMask` asks for. MIIM_STRING copies the
/// text into dwTypeData (cch characters at most, the NUL included) and
/// returns its length in cch; a NULL dwTypeData only returns the length.
extern "win64" fn GetMenuItemInfoW(menu: u64, item: u32, by_pos: Bool, info: *mut u8) -> Bool {
    const MIIM_STATE: u32 = 0x1;
    const MIIM_ID: u32 = 0x2;
    const MIIM_SUBMENU: u32 = 0x4;
    const MIIM_CHECKMARKS: u32 = 0x8;
    const MIIM_TYPE: u32 = 0x10;
    const MIIM_DATA: u32 = 0x20;
    const MIIM_STRING: u32 = 0x40;
    const MIIM_BITMAP: u32 = 0x80;
    const MIIM_FTYPE: u32 = 0x100;
    const MF_BITMAP: u32 = 0x4;
    const MF_OWNERDRAW: u32 = 0x100;
    let (size, sub_at, data_at, text_at, cch_at, bmp_at) = menuiteminfo();
    if info.is_null() || unsafe { teb::get::<u32>(info as u64, 0) } != size {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let p = info as u64;
    // SAFETY: a MENUITEMINFOW of the size just checked.
    let mask = unsafe { teb::get::<u32>(p, 4) };
    let found = with(|g| {
        let (handle, index) = menu_item(g, menu, item, by_pos != 0, 0)?;
        let it = g.menus.get(&handle)?.items.get(index)?.clone();
        Some(it)
    });
    let Some(it) = found else {
        return if with(|g| g.menus.contains_key(&menu)) { fail(ERROR_MENU_ITEM_NOT_FOUND, FALSE) } else { fail(ERROR_INVALID_MENU_HANDLE, FALSE) };
    };
    let popup = it.flags & MF_POPUP != 0;
    // The MFT_* type bits: separator, bitmap, owner-drawn, radio check,
    // break and right-justification.
    let kind = it.flags & (MF_SEPARATOR | MF_BITMAP | MF_OWNERDRAW | 0x200 | 0x20 | 0x40 | 0x4000);
    // SAFETY: as above.
    unsafe {
        if mask & MIIM_STATE != 0 {
            teb::put(p, 12, it.flags & (0x1 | 0x2 | 0x8 | 0x80 | 0x1000)); // grayed, disabled, checked, hilite, default
        }
        if mask & MIIM_ID != 0 {
            teb::put(p, 16, if popup { it.id as u32 } else { it.id as u32 });
        }
        if mask & MIIM_SUBMENU != 0 {
            put_ptr(p, sub_at, if popup { it.id } else { 0 });
        }
        if mask & MIIM_CHECKMARKS != 0 {
            let (checked, unchecked) = if crate::wow64::is_active() { (24, 28) } else { (32, 40) };
            put_ptr(p, checked, 0);
            put_ptr(p, unchecked, 0);
        }
        if mask & (MIIM_TYPE | MIIM_FTYPE) != 0 {
            teb::put(p, 8, kind);
        }
        if mask & MIIM_DATA != 0 {
            put_ptr(p, data_at, if it.flags & (MF_BITMAP | MF_OWNERDRAW) != 0 && it.text.is_none() { it.data } else { it.data });
        }
        if mask & MIIM_BITMAP != 0 {
            put_ptr(p, bmp_at, if it.flags & MF_BITMAP != 0 { it.data } else { 0 });
        }
        if mask & (MIIM_STRING | MIIM_TYPE) != 0 && it.flags & (MF_SEPARATOR | MF_BITMAP | MF_OWNERDRAW) == 0 {
            let units: Vec<u16> = it.text.as_deref().unwrap_or("").encode_utf16().collect();
            let buf = get_ptr(p, text_at) as *mut u16;
            let cap = teb::get::<u32>(p, cch_at as usize) as usize;
            if !buf.is_null() && cap > 0 {
                let n = units.len().min(cap - 1);
                core::ptr::copy_nonoverlapping(units.as_ptr(), buf, n);
                *buf.add(n) = 0;
                teb::put(p, cch_at as usize, n as u32);
            } else {
                teb::put(p, cch_at as usize, units.len() as u32);
            }
        }
    }
    TRUE
}

/// InsertMenuW: a new item before `position` (by position, or before the
/// item with that command id); position -1 by position appends.
extern "win64" fn InsertMenuW(menu: u64, position: u32, flags: u32, id: u64, item: *const u16) -> Bool {
    let at = with(|g| {
        let m = g.menus.get(&menu)?;
        if flags & MF_BYPOSITION != 0 {
            Some(if position == u32::MAX || position as usize > m.items.len() { m.items.len() } else { position as usize })
        } else {
            let (handle, index) = menu_item(g, menu, position, false, 0)?;
            // A command in a submenu is inserted there, as Windows does.
            Some(if handle == menu { index } else { usize::MAX - index })
        }
    });
    let Some(at) = at else {
        return if with(|g| g.menus.contains_key(&menu)) { fail(ERROR_MENU_ITEM_NOT_FOUND, FALSE) } else { fail(ERROR_INVALID_MENU_HANDLE, FALSE) };
    };
    // Built like AppendMenuW, then moved into place.
    let target = if at > usize::MAX / 2 {
        with(|g| menu_item(g, menu, position, false, 0).map(|(h, _)| h)).unwrap_or(menu)
    } else {
        menu
    };
    let index = if at > usize::MAX / 2 { usize::MAX - at } else { at };
    if AppendMenuW(target, flags & !MF_BYPOSITION, id, item) == FALSE {
        return FALSE;
    }
    with(|g| {
        let m = g.menus.get_mut(&target).expect("menu just appended to");
        let new = m.items.pop().expect("item just appended");
        m.items.insert(index.min(m.items.len()), new);
    });
    TRUE
}

/// DeleteMenu: removes an item; a submenu it opens is destroyed with it.
extern "win64" fn DeleteMenu(menu: u64, position: u32, flags: u32) -> Bool {
    with(|g| {
        if !g.menus.contains_key(&menu) {
            return fail(ERROR_INVALID_MENU_HANDLE, FALSE);
        }
        let Some((handle, index)) = menu_item(g, menu, position, flags & MF_BYPOSITION != 0, 0) else {
            return fail(ERROR_MENU_ITEM_NOT_FOUND, FALSE);
        };
        let removed = g.menus.get_mut(&handle).expect("located menu").items.remove(index);
        if removed.flags & MF_POPUP != 0 {
            destroy_menu(g, removed.id);
        }
        TRUE
    })
}

/// CheckMenuItem: sets or clears MF_CHECKED; returns the previous state
/// (MF_CHECKED or MF_UNCHECKED), or -1 when there is no such item.
extern "win64" fn CheckMenuItem(menu: u64, id: u32, check: u32) -> u32 {
    with(|g| {
        let Some((handle, index)) = menu_item(g, menu, id, check & MF_BYPOSITION != 0, 0) else { return u32::MAX };
        let it = &mut g.menus.get_mut(&handle).expect("located menu").items[index];
        let old = it.flags & MF_CHECKED;
        it.flags = (it.flags & !MF_CHECKED) | (check & MF_CHECKED);
        old
    })
}

// ---- icons and cursors -------------------------------------------------------------

/// CreateIconFromResourceEx: an icon or cursor from the data of one
/// RT_ICON / RT_CURSOR resource: a DIB (BITMAPINFOHEADER, colour bits of
/// 1-32 bpp and the AND mask) or a PNG image. A cursor resource starts
/// with its hotspot (two 16-bit values). Only version 0x00030000 exists.
extern "win64" fn CreateIconFromResourceEx(bits: *const u8, size: u32, icon: Bool, version: u32, _cx: i32, _cy: i32, _flags: u32) -> u64 {
    if bits.is_null() || size < 4 || !(0x0002_0000..=0x0003_0000).contains(&version) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's resource data of `size` bytes.
    let data = unsafe { core::slice::from_raw_parts(bits, size as usize) };
    let (hotspot, image) = if icon != 0 {
        ((0, 0), data)
    } else {
        ((u16::from_le_bytes([data[0], data[1]]) as u32, u16::from_le_bytes([data[2], data[3]]) as u32), &data[4..])
    };
    let Some(bitmap) = osyx_ui::icon::decode_image(image) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    with(|g| {
        let Some((mask, color)) = gdi::icon_bitmaps(g, bitmap.width, bitmap.height, &bitmap.pixels) else {
            return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        let h = g.handle();
        g.icons.insert(h, Icon { is_icon: icon != 0, hotspot, mask, color });
        h
    })
}

extern "win64" fn CreateIconFromResource(bits: *const u8, size: u32, icon: Bool, version: u32) -> u64 {
    CreateIconFromResourceEx(bits, size, icon, version, 0, 0, 0)
}

/// DestroyCursor: cursors and icons are the same objects.
extern "win64" fn DestroyCursor(cursor: u64) -> Bool {
    DestroyIcon(cursor)
}

// ---- display modes -------------------------------------------------------------------

const DISP_CHANGE_SUCCESSFUL: i32 = 0;
const DISP_CHANGE_BADMODE: i32 = -2;
const DISP_CHANGE_BADPARAM: i32 = -5;

/// ChangeDisplaySettingsExW: the display has one mode (the PRISM screen,
/// see EnumDisplaySettingsW). Asking for that mode, or for the registry
/// mode (NULL), succeeds without a change; any other is DISP_CHANGE_BADMODE,
/// as Windows answers for a mode the adapter does not have.
extern "win64" fn ChangeDisplaySettingsExW(device: *const u16, mode: *const u8, _hwnd: u64, flags: u32, _param: u64) -> i32 {
    const CDS_TEST: u32 = 0x2;
    const CDS_RESET: u32 = 0x4000_0000;
    // SAFETY: a NUL-terminated device name, or NULL.
    if let Some(d) = unsafe { path::wide_to_string(device) } {
        if !d.is_empty() && !d.eq_ignore_ascii_case(DISPLAY_NAME) {
            return DISP_CHANGE_BADPARAM;
        }
    }
    if mode.is_null() {
        return DISP_CHANGE_SUCCESSFUL;
    }
    let _ = flags & (CDS_TEST | CDS_RESET);
    // SAFETY: the caller's DEVMODEW: dmFields at 72, then the mode.
    let (fields, bpp, w, h, hz) = unsafe {
        let p = mode as u64;
        (teb::get::<u32>(p, 72), teb::get::<u32>(p, 168), teb::get::<u32>(p, 172), teb::get::<u32>(p, 176), teb::get::<u32>(p, 184))
    };
    let (_, _, sw, sh) = screen_rect();
    let ok = (fields & 0x4_0000 == 0 || bpp == 32)
        && (fields & 0x8_0000 == 0 || w == sw as u32)
        && (fields & 0x10_0000 == 0 || h == sh as u32)
        && (fields & 0x40_0000 == 0 || hz == 60 || hz <= 1);
    if ok { DISP_CHANGE_SUCCESSFUL } else { DISP_CHANGE_BADMODE }
}

/// DEVMODEA (156 bytes) <-> DEVMODEW (220): the device and form names
/// are 32 characters; everything between and after moves by their size.
fn devmode_w_to_a(w: &[u8; 220], a: *mut u8) {
    let mut out = [0u8; 156];
    let name = |at: usize| -> [u8; 32] {
        let mut n = [0u8; 32];
        for i in 0..31 {
            let c = u16::from_le_bytes([w[at + 2 * i], w[at + 2 * i + 1]]);
            if c == 0 {
                break;
            }
            n[i] = if c < 0x80 { c as u8 } else { b'?' };
        }
        n
    };
    out[..32].copy_from_slice(&name(0));
    out[32..70].copy_from_slice(&w[64..102]);
    out[70..102].copy_from_slice(&name(102));
    out[102..156].copy_from_slice(&w[166..220]);
    out[36..38].copy_from_slice(&156u16.to_le_bytes()); // dmSize
    // SAFETY: the caller's DEVMODEA of 156 bytes.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), a, 156) };
}

fn devmode_a_to_w(a: *const u8) -> [u8; 220] {
    // SAFETY: the caller's DEVMODEA of 156 bytes.
    let a = unsafe { core::slice::from_raw_parts(a, 156) };
    let mut w = [0u8; 220];
    for i in 0..32 {
        w[2 * i] = a[i];
        w[102 + 2 * i] = a[70 + i];
    }
    w[64..102].copy_from_slice(&a[32..70]);
    w[166..220].copy_from_slice(&a[102..156]);
    w[68..70].copy_from_slice(&220u16.to_le_bytes());
    w
}

extern "win64" fn ChangeDisplaySettingsExA(device: *const u8, mode: *const u8, hwnd: u64, flags: u32, param: u64) -> i32 {
    // SAFETY: NULL or a NUL-terminated name.
    let d = unsafe { path::ansi_to_wide(device) };
    let w = (!mode.is_null()).then(|| devmode_a_to_w(mode));
    ChangeDisplaySettingsExW(
        d.as_ref().map_or(core::ptr::null(), |d| d.as_ptr()),
        w.as_ref().map_or(core::ptr::null(), |w| w.as_ptr()),
        hwnd,
        flags,
        param,
    )
}

extern "win64" fn EnumDisplaySettingsA(device: *const u8, mode: u32, out: *mut u8) -> Bool {
    if out.is_null() {
        return FALSE;
    }
    // SAFETY: NULL or a NUL-terminated name.
    let d = unsafe { path::ansi_to_wide(device) };
    let mut w = [0u8; 220];
    if EnumDisplaySettingsW(d.as_ref().map_or(core::ptr::null(), |d| d.as_ptr()), mode, w.as_mut_ptr()) == FALSE {
        return FALSE;
    }
    devmode_w_to_a(&w, out);
    TRUE
}

/// EnumDisplayDevicesA: DISPLAY_DEVICEA (424 bytes): the strings of the
/// Unicode form in ANSI (32, 128, 128 and 128 characters).
extern "win64" fn EnumDisplayDevicesA(device: *const u8, index: u32, out: *mut u8, flags: u32) -> Bool {
    // SAFETY: the caller's DISPLAY_DEVICEA starts with cb.
    if out.is_null() || unsafe { teb::get::<u32>(out as u64, 0) } < 424 {
        return FALSE;
    }
    // SAFETY: NULL or a NUL-terminated name.
    let d = unsafe { path::ansi_to_wide(device) };
    let mut w = [0u8; 840];
    w[..4].copy_from_slice(&840u32.to_le_bytes());
    if EnumDisplayDevicesW(d.as_ref().map_or(core::ptr::null(), |d| d.as_ptr()), index, w.as_mut_ptr(), flags) == FALSE {
        return FALSE;
    }
    let mut a = [0u8; 424];
    a[..4].copy_from_slice(&424u32.to_le_bytes());
    // (W offset, A offset, characters)
    for (wo, ao, n) in [(4usize, 4usize, 32usize), (68, 36, 128), (328, 168, 128), (584, 296, 128)] {
        for i in 0..n - 1 {
            let c = u16::from_le_bytes([w[wo + 2 * i], w[wo + 2 * i + 1]]);
            if c == 0 {
                break;
            }
            a[ao + i] = if c < 0x80 { c as u8 } else { b'?' };
        }
    }
    a[164..168].copy_from_slice(&w[324..328]); // StateFlags
    // SAFETY: the caller's 424-byte DISPLAY_DEVICEA.
    unsafe { core::ptr::copy_nonoverlapping(a.as_ptr(), out, 424) };
    TRUE
}

// ---- windows -------------------------------------------------------------------------

/// GetWindowInfo: WINDOWINFO (60 bytes): the window and client rectangles
/// on the screen (there is no frame: they are the same), styles, whether
/// it is active, and no borders.
extern "win64" fn GetWindowInfo(wnd: u64, out: *mut u8) -> Bool {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let Some((style, ex, atom, active)) = with(|g| {
        let w = g.windows.get(&wnd)?;
        Some((w.style, w.ex_style, 0xC000 + w.class as u32, root_window(g, g.focus) == Some(wnd)))
    }) else {
        return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
    };
    let mut r = [0i32; 4];
    if GetWindowRect(wnd, r.as_mut_ptr()) == FALSE {
        return FALSE;
    }
    // SAFETY: the caller's WINDOWINFO.
    unsafe {
        let p = out as u64;
        teb::put(p, 0, 60u32);
        for (i, v) in r.iter().enumerate() {
            teb::put(p, 4 + 4 * i, *v);
            teb::put(p, 20 + 4 * i, *v);
        }
        teb::put(p, 36, style);
        teb::put(p, 40, ex);
        teb::put(p, 44, if active { 1u32 } else { 0 }); // WS_ACTIVECAPTION
        teb::put(p, 48, 0u32);
        teb::put(p, 52, 0u32);
        teb::put(p, 56, atom as u16);
        teb::put(p, 58, 0x0500u16); // wCreatorVersion
    }
    TRUE
}

/// IsWindowUnicode: windows of this runtime process messages the same way
/// for both character sets, so the answer follows the window procedure:
/// one this runtime implements (the controls) is Unicode, and so is a
/// program's class registered with RegisterClassW/ExW.
extern "win64" fn IsWindowUnicode(wnd: u64) -> Bool {
    with(|g| match g.windows.get(&wnd) {
        Some(w) => g.classes.get(w.class).is_none_or(|c| c.unicode) as Bool,
        None => FALSE,
    })
}

/// GetClassLongA: the 32-bit class values (GCL_*), as GetClassLongPtrW
/// reports them, plus the style (GCL_STYLE).
extern "win64" fn GetClassLongA(wnd: u64, index: i32) -> u32 {
    if index == -26 {
        return with(|g| match g.windows.get(&wnd) {
            Some(w) => g.classes[w.class].style,
            None => fail(ERROR_INVALID_WINDOW_HANDLE, 0),
        });
    }
    if index == -32 {
        // GCW_ATOM
        return with(|g| match g.windows.get(&wnd) {
            Some(w) => 0xC000 + w.class as u32,
            None => fail(ERROR_INVALID_WINDOW_HANDLE, 0),
        });
    }
    GetClassLongPtrW(wnd, index) as u32
}

/// SetActiveWindow: activates a top-level window of this thread; returns
/// the previously active one.
extern "win64" fn SetActiveWindow(wnd: u64) -> u64 {
    let previous = with(|g| g.focus);
    if wnd == 0 {
        return previous;
    }
    if !is_window(wnd) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
    }
    SetForegroundWindow(wnd);
    previous
}

/// SetLayeredWindowAttributes: the colour key and constant alpha of a
/// WS_EX_LAYERED window, kept for GetLayeredWindowAttributes. PRISM
/// composes every window opaque, so the window is shown as before.
extern "win64" fn SetLayeredWindowAttributes(wnd: u64, key: u32, alpha: u8, flags: u32) -> Bool {
    const WS_EX_LAYERED: u32 = 0x8_0000;
    with(|g| {
        let Some(w) = g.windows.get_mut(&wnd) else { return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE) };
        if w.ex_style & WS_EX_LAYERED == 0 || flags & !3 != 0 {
            return fail(ERROR_INVALID_PARAMETER, FALSE);
        }
        w.props.insert(String::from("#layered"), (key as u64) | (alpha as u64) << 32 | (flags as u64) << 40);
        TRUE
    })
}

extern "win64" fn GetLayeredWindowAttributes(wnd: u64, key: *mut u32, alpha: *mut u8, flags: *mut u32) -> Bool {
    with(|g| {
        let Some(w) = g.windows.get(&wnd) else { return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE) };
        let Some(&v) = w.props.get("#layered") else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
        // SAFETY: NULL or the caller's out values.
        unsafe {
            if !key.is_null() {
                *key = v as u32;
            }
            if !alpha.is_null() {
                *alpha = (v >> 32) as u8;
            }
            if !flags.is_null() {
                *flags = (v >> 40) as u32;
            }
        }
        TRUE
    })
}

/// GetShellWindow: the shell's desktop window belongs to a shell process
/// (Explorer); OSyx's desktop is not a Windows program, so there is none.
extern "win64" fn GetShellWindow() -> u64 {
    0
}

/// AttachThreadInput: this runtime keeps one input state (focus, capture,
/// key state) for all threads of the process, so threads are always
/// attached; the call checks its arguments as Windows does.
extern "win64" fn AttachThreadInput(from: u32, to: u32, _attach: Bool) -> Bool {
    if from == 0 || to == 0 || from == to {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    TRUE
}

/// EnumDesktopWindows: the top-level windows of the desktop; this process
/// has one desktop (NULL, OpenInputDesktop or OpenDesktop "Default").
extern "win64" fn EnumDesktopWindows(desktop: u64, proc_: u64, param: u64) -> Bool {
    if desktop != 0 && !matches!(crate::objects::get(desktop), Some(crate::objects::Object::Desktop(_))) {
        return fail(ERROR_INVALID_HANDLE, FALSE);
    }
    EnumWindows(proc_, param)
}

/// GetClipCursor: the pointer is never confined (ClipCursor): the screen.
extern "win64" fn GetClipCursor(rect: *mut i32) -> Bool {
    if rect.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (x, y, w, h) = screen_rect();
    write_rect(rect, Rect::new(x, y, w as u32, h as u32));
    TRUE
}

/// GetRawInputBuffer: no raw input device exists (GetRawInputDeviceList),
/// so there is never buffered input: 0, and the size needed is 0.
extern "win64" fn GetRawInputBuffer(data: *mut u8, size: *mut u32, header: u32) -> u32 {
    let expected = if crate::wow64::is_active() { 16 } else { 24 }; // RAWINPUTHEADER
    if header != expected || size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, u32::MAX);
    }
    if data.is_null() {
        // SAFETY: the caller's size.
        unsafe { *size = 0 };
    }
    0
}

extern "win64" fn UnionRect(dst: *mut i32, a: *const i32, b: *const i32) -> Bool {
    if dst.is_null() || a.is_null() || b.is_null() {
        return FALSE;
    }
    // SAFETY: the caller's RECTs.
    let (ra, rb) = unsafe { (core::slice::from_raw_parts(a, 4).to_vec(), core::slice::from_raw_parts(b, 4).to_vec()) };
    let empty = |r: &[i32]| r[0] >= r[2] || r[1] >= r[3];
    let out = match (empty(&ra), empty(&rb)) {
        (true, true) => [0; 4],
        (true, false) => [rb[0], rb[1], rb[2], rb[3]],
        (false, true) => [ra[0], ra[1], ra[2], ra[3]],
        (false, false) => [ra[0].min(rb[0]), ra[1].min(rb[1]), ra[2].max(rb[2]), ra[3].max(rb[3])],
    };
    // SAFETY: as above.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), dst, 4) };
    (out != [0; 4]) as Bool
}

// ---- keyboard ------------------------------------------------------------------------

/// VkKeyScanExW: the key and shift state that type `ch` on the US layout:
/// the low byte is the virtual key, the high byte 1 for Shift and 2 for
/// Ctrl; -1 when no key makes it.
extern "win64" fn VkKeyScanExW(ch: u16, hkl: u64) -> i16 {
    if hkl != 0 && hkl != HKL_US {
        return -1;
    }
    // KEYS rows: plain, Shift, Ctrl, Ctrl+Shift.
    for row in 0..4 {
        if let Some(k) = keyboard::KEYS.iter().find(|k| k.chars[row] == ch) {
            return ((row as i16) << 8) | k.vk as i16;
        }
    }
    -1
}

extern "win64" fn VkKeyScanExA(ch: u8, hkl: u64) -> i16 {
    VkKeyScanExW(ch as u16, hkl)
}

/// keybd_event: one keyboard INPUT through SendInput.
extern "win64" fn keybd_event(vk: u8, scan: u8, flags: u32, extra: u64) {
    let narrow = crate::wow64::is_active();
    let (size, ki) = if narrow { (28usize, 4usize) } else { (40, 8) };
    let mut input = [0u8; 40];
    input[..4].copy_from_slice(&1u32.to_le_bytes()); // INPUT_KEYBOARD
    input[ki..ki + 2].copy_from_slice(&(vk as u16).to_le_bytes());
    input[ki + 2..ki + 4].copy_from_slice(&(scan as u16).to_le_bytes());
    input[ki + 4..ki + 8].copy_from_slice(&flags.to_le_bytes());
    if narrow {
        input[ki + 12..ki + 16].copy_from_slice(&(extra as u32).to_le_bytes());
    } else {
        input[ki + 16..ki + 24].copy_from_slice(&extra.to_le_bytes());
    }
    SendInput(1, input.as_ptr(), size as i32);
}

// ---- dialogs -------------------------------------------------------------------------

/// DialogBoxIndirectParamW: a modal dialog from a DLGTEMPLATE(EX) in
/// memory, as DialogBoxParamW does for a resource.
extern "win64" fn DialogBoxIndirectParamW(module: u64, template: *const u8, owner: u64, proc_: u64, param: u64) -> i64 {
    if template.is_null() {
        return fail(ERROR_INVALID_PARAMETER, -1);
    }
    // The template's size is not passed: it may extend to the end of the
    // memory region holding it.
    let mut info = [0u8; 48];
    let len = if crate::kernel32::VirtualQuery(template as u64, info.as_mut_ptr(), 48) == 0 {
        4096
    } else {
        let base = u64::from_le_bytes(info[0..8].try_into().unwrap());
        let size = u64::from_le_bytes(info[24..32].try_into().unwrap());
        (base + size - template as u64).min(1 << 20) as usize
    };
    // SAFETY: `len` readable bytes of the caller's template.
    let raw = unsafe { core::slice::from_raw_parts(template, len) };
    match create_dialog_from(raw, module, owner, proc_, param, true) {
        Ok(hwnd) => run_modal(hwnd, owner),
        Err(e) => fail(e, -1),
    }
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetMenuItemInfoW,
        InsertMenuW,
        DeleteMenu,
        CheckMenuItem,
        CreateIconFromResourceEx,
        CreateIconFromResource,
        DestroyCursor,
        ChangeDisplaySettingsExW,
        ChangeDisplaySettingsExA,
        EnumDisplaySettingsA,
        EnumDisplayDevicesA,
        GetWindowInfo,
        IsWindowUnicode,
        GetClassLongA,
        SetActiveWindow,
        SetLayeredWindowAttributes,
        GetLayeredWindowAttributes,
        GetShellWindow,
        AttachThreadInput,
        EnumDesktopWindows,
        GetClipCursor,
        GetRawInputBuffer,
        UnionRect,
        VkKeyScanExW,
        VkKeyScanExA,
        keybd_event,
        DialogBoxIndirectParamW,
    ]
}
