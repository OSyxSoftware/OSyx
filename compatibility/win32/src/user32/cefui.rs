//! user32 and gdi32 functions that libcef.dll (Chromium) delay-loads. A
//! delay-loaded function that is missing is a crash through Chromium's
//! delay-load hook, the first time it is called.
use super::*;
use core::sync::atomic::{AtomicU64, Ordering};

const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
const ERROR_INVALID_MENU_HANDLE: u32 = 1401;
const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;
const ERROR_HOTKEY_NOT_REGISTERED: u32 = 1419;
const ERROR_MENU_ITEM_NOT_FOUND: u32 = 1456;

// ---- characters --------------------------------------------------------------------------

/// One UTF-16 unit upper-cased when its simple upper case is one unit.
fn upper_unit(c: u16) -> u16 {
    match char::from_u32(c as u32) {
        Some(ch) => {
            let mut up = ch.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if (u as u32) < 0x1_0000 => u as u16,
                _ => c,
            }
        }
        None => c,
    }
}

/// CharUpperW: a string upper-cased in place, or, when the high word of
/// the argument is 0, one character in its low word, returned the same way.
extern "win64" fn CharUpperW(s: *mut u16) -> *mut u16 {
    let v = s as u64;
    if v >> 16 == 0 {
        return upper_unit(v as u16) as u64 as *mut u16;
    }
    let mut i = 0;
    // SAFETY: a NUL-terminated string from the caller, modified in place.
    unsafe {
        while *s.add(i) != 0 {
            *s.add(i) = upper_unit(*s.add(i));
            i += 1;
        }
    }
    s
}

/// The OEM code page is UTF-8 (GetOEMCP reports 65001), as the ANSI one.
/// CharToOemBuffW converts `len` characters into at most `len` bytes;
/// a character whose UTF-8 form no longer fits ends the output.
extern "win64" fn CharToOemBuffW(src: *const u16, dst: *mut u8, len: u32) -> Bool {
    if src.is_null() || dst.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: the caller's `len` characters.
    let units = unsafe { core::slice::from_raw_parts(src, len as usize) };
    let mut out = Vec::with_capacity(len as usize);
    for c in char::decode_utf16(units.iter().copied()) {
        let mut buf = [0u8; 4];
        let bytes = c.unwrap_or('?').encode_utf8(&mut buf).as_bytes();
        if out.len() + bytes.len() > len as usize {
            break;
        }
        out.extend_from_slice(bytes);
    }
    out.resize(len as usize, 0);
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), dst, out.len()) };
    TRUE
}

/// OEM and ANSI text are both UTF-8: the conversion is a copy.
extern "win64" fn OemToCharA(src: *const u8, dst: *mut u8) -> Bool {
    if src.is_null() || dst.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: a NUL-terminated source; the destination is at least as long
    // (it may be the source itself).
    unsafe {
        let n = (0..).take_while(|&i| *src.add(i) != 0).count();
        core::ptr::copy(src, dst, n + 1);
    }
    TRUE
}

extern "win64" fn OemToCharBuffA(src: *const u8, dst: *mut u8, len: u32) -> Bool {
    if src.is_null() || dst.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    // SAFETY: `len` bytes in both buffers (they may be the same).
    unsafe { core::ptr::copy(src, dst, len as usize) };
    TRUE
}

// ---- deferred window positions -------------------------------------------------------------

/// SetWindowPos arguments: window, insert-after, x, y, cx, cy, flags.
type Placement = (u64, u64, i32, i32, i32, i32, u32);

static DEFERRED: Mutex<BTreeMap<u64, Vec<Placement>>> = Mutex::new(BTreeMap::new());
static NEXT_DEFER: AtomicU64 = AtomicU64::new(0x7d00_0004);

extern "win64" fn BeginDeferWindowPos(count: i32) -> u64 {
    if count < 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let h = NEXT_DEFER.fetch_add(4, Ordering::Relaxed);
    DEFERRED.lock().unwrap().insert(h, Vec::with_capacity(count as usize));
    h
}

/// Queues a move for EndDeferWindowPos. A failure abandons the whole set,
/// as on Windows, whose callers then do not call EndDeferWindowPos.
#[allow(clippy::too_many_arguments)]
extern "win64" fn DeferWindowPos(h: u64, wnd: u64, after: u64, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> u64 {
    let mut deferred = DEFERRED.lock().unwrap();
    if !deferred.contains_key(&h) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    if !with(|g| g.windows.contains_key(&wnd)) {
        deferred.remove(&h);
        return fail(ERROR_INVALID_WINDOW_HANDLE, 0);
    }
    deferred.get_mut(&h).expect("checked above").push((wnd, after, x, y, cx, cy, flags));
    h
}

extern "win64" fn EndDeferWindowPos(h: u64) -> Bool {
    let Some(list) = DEFERRED.lock().unwrap().remove(&h) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    let mut ok = TRUE;
    for (wnd, after, x, y, cx, cy, flags) in list {
        if SetWindowPos(wnd, after, x, y, cx, cy, flags) == FALSE {
            ok = FALSE;
        }
    }
    ok
}

/// The window LockWindowUpdate locked (0: none); only one at a time.
static LOCKED: AtomicU64 = AtomicU64::new(0);

/// LockWindowUpdate: records the one locked window. Drawing into it is not
/// held back: the compositor shows what is drawn.
extern "win64" fn LockWindowUpdate(wnd: u64) -> Bool {
    if wnd == 0 {
        LOCKED.store(0, Ordering::SeqCst);
        return TRUE;
    }
    if !with(|g| g.windows.contains_key(&wnd)) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
    }
    match LOCKED.compare_exchange(0, wnd, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => TRUE,
        Err(current) if current == wnd => TRUE,
        Err(_) => FALSE,
    }
}

// ---- menus --------------------------------------------------------------------------------

extern "win64" fn GetMenuItemCount(menu: u64) -> i32 {
    with(|g| match g.menus.get(&menu) {
        Some(m) => m.items.len() as i32,
        None => fail(ERROR_INVALID_MENU_HANDLE, -1),
    })
}

const MFS_DEFAULT: u32 = 0x1000;

/// SetMenuDefaultItem: the item gets MFS_DEFAULT and every other item of
/// the menu loses it; -1 leaves the menu without a default item.
extern "win64" fn SetMenuDefaultItem(menu: u64, item: u32, by_pos: u32) -> Bool {
    with(|g| {
        let Some(m) = g.menus.get_mut(&menu) else { return fail(ERROR_INVALID_MENU_HANDLE, FALSE) };
        let index = if item == u32::MAX {
            None
        } else if by_pos != 0 {
            match (item as usize) < m.items.len() {
                true => Some(item as usize),
                false => return fail(ERROR_MENU_ITEM_NOT_FOUND, FALSE),
            }
        } else {
            match m.items.iter().position(|i| i.flags & 0x10 == 0 && i.id == item as u64) {
                Some(i) => Some(i),
                None => return fail(ERROR_MENU_ITEM_NOT_FOUND, FALSE),
            }
        };
        for (i, it) in m.items.iter_mut().enumerate() {
            it.flags = if Some(i) == index { it.flags | MFS_DEFAULT } else { it.flags & !MFS_DEFAULT };
        }
        TRUE
    })
}

/// InsertMenuItemW: a new item described by a MENUITEMINFOW, before
/// `item` (a position or a command id); built like InsertMenuW, then given
/// the fields that call cannot set.
extern "win64" fn InsertMenuItemW(menu: u64, item: u32, by_pos: Bool, info: *const u8) -> Bool {
    const MIIM_STATE: u32 = 0x1;
    const MIIM_ID: u32 = 0x2;
    const MIIM_SUBMENU: u32 = 0x4;
    const MIIM_TYPE: u32 = 0x10;
    const MIIM_DATA: u32 = 0x20;
    const MIIM_STRING: u32 = 0x40;
    const MIIM_FTYPE: u32 = 0x100;
    const MF_SEPARATOR: u32 = 0x800;
    const MF_POPUP: u32 = 0x10;
    let (size, sub_at, data_at, text_at, _, _) = client::menuiteminfo();
    if info.is_null() || unsafe { teb::get::<u32>(info as u64, 0) } != size {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let b = info as u64;
    // SAFETY: a MENUITEMINFOW of the size just checked.
    let (mask, kind, state, id, sub, data, text) = unsafe {
        (teb::get::<u32>(b, 4), teb::get::<u32>(b, 8), teb::get::<u32>(b, 12), teb::get::<u32>(b, 16),
         client::get_ptr(b, sub_at), client::get_ptr(b, data_at), client::get_ptr(b, text_at))
    };
    let separator = mask & (MIIM_TYPE | MIIM_FTYPE) != 0 && kind & MF_SEPARATOR != 0;
    let mut flags = if separator { MF_SEPARATOR } else { 0 };
    if mask & MIIM_STATE != 0 {
        flags |= state & (0x1 | 0x2 | 0x8 | 0x80);
    }
    let mut command = if mask & MIIM_ID != 0 { id as u64 } else { 0 };
    if mask & MIIM_SUBMENU != 0 && sub != 0 {
        flags |= MF_POPUP;
        command = sub;
    }
    let label = if !separator && (mask & MIIM_STRING != 0 || mask & MIIM_TYPE != 0) { text as *const u16 } else { core::ptr::null() };
    let position_flag = if by_pos != 0 { 0x400 } else { 0 };
    if client::InsertMenuW(menu, item, flags | position_flag, command, label) == FALSE {
        return FALSE;
    }
    if mask & MIIM_DATA != 0 || mask & MIIM_STATE != 0 && state & MFS_DEFAULT != 0 {
        with(|g| {
            let found = if by_pos != 0 {
                g.menus.get(&menu).map(|m| (menu, (item as usize).min(m.items.len().saturating_sub(1))))
            } else {
                menu_item(g, menu, if flags & MF_POPUP != 0 { 0 } else { command as u32 }, false, 0)
            };
            if let Some((h, i)) = found {
                if let Some(it) = g.menus.get_mut(&h).and_then(|m| m.items.get_mut(i)) {
                    if mask & MIIM_DATA != 0 {
                        it.data = data;
                    }
                    if mask & MIIM_STATE != 0 {
                        it.flags |= state & MFS_DEFAULT;
                    }
                }
            }
        });
    }
    TRUE
}

/// MENUINFO fields that menus keep (style, maximum height, background
/// brush, help context, application data), by menu.
#[derive(Clone, Copy, Default)]
struct MenuInfo {
    style: u32,
    max_height: u32,
    background: u64,
    help: u32,
    data: u64,
}

static MENU_INFO: Mutex<BTreeMap<u64, MenuInfo>> = Mutex::new(BTreeMap::new());

const MIM_MAXHEIGHT: u32 = 0x1;
const MIM_BACKGROUND: u32 = 0x2;
const MIM_HELPID: u32 = 0x4;
const MIM_MENUDATA: u32 = 0x8;
const MIM_STYLE: u32 = 0x10;
const MIM_APPLYTOSUBMENUS: u32 = 0x8000_0000;

/// MENUINFO: cbSize, fMask, dwStyle, cyMax, hbrBack, dwContextHelpID,
/// dwMenuData; 40 bytes (28 for 32-bit callers).
fn menuinfo_layout() -> (u32, u64, u64, u64) {
    if crate::wow64::is_active() { (28, 16, 20, 24) } else { (40, 16, 24, 32) }
}

extern "win64" fn GetMenuInfo(menu: u64, info: *mut u8) -> Bool {
    let (size, back_at, help_at, data_at) = menuinfo_layout();
    if info.is_null() || unsafe { teb::get::<u32>(info as u64, 0) } != size {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if !with(|g| g.menus.contains_key(&menu)) {
        return fail(ERROR_INVALID_MENU_HANDLE, FALSE);
    }
    let m = MENU_INFO.lock().unwrap().get(&menu).copied().unwrap_or_default();
    let p = info as u64;
    // SAFETY: a MENUINFO of the size just checked.
    unsafe {
        let mask = teb::get::<u32>(p, 4);
        if mask & MIM_STYLE != 0 {
            teb::put(p, 8, m.style);
        }
        if mask & MIM_MAXHEIGHT != 0 {
            teb::put(p, 12, m.max_height);
        }
        if mask & MIM_BACKGROUND != 0 {
            client::put_ptr(p, back_at, m.background);
        }
        if mask & MIM_HELPID != 0 {
            teb::put(p, help_at as usize, m.help);
        }
        if mask & MIM_MENUDATA != 0 {
            client::put_ptr(p, data_at, m.data);
        }
    }
    TRUE
}

extern "win64" fn SetMenuInfo(menu: u64, info: *const u8) -> Bool {
    let (size, back_at, help_at, data_at) = menuinfo_layout();
    if info.is_null() || unsafe { teb::get::<u32>(info as u64, 0) } != size {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let p = info as u64;
    // SAFETY: a MENUINFO of the size just checked.
    let (mask, style, max_height, background, help, data) = unsafe {
        (teb::get::<u32>(p, 4), teb::get::<u32>(p, 8), teb::get::<u32>(p, 12), client::get_ptr(p, back_at), teb::get::<u32>(p, help_at as usize), client::get_ptr(p, data_at))
    };
    // The menu and, with MIM_APPLYTOSUBMENUS, the submenus it opens.
    let menus = with(|g| {
        if !g.menus.contains_key(&menu) {
            return None;
        }
        let mut all = alloc::vec![menu];
        let mut i = 0;
        while mask & MIM_APPLYTOSUBMENUS != 0 && i < all.len() && all.len() < 256 {
            if let Some(m) = g.menus.get(&all[i]) {
                for it in m.items.iter().filter(|it| it.flags & 0x10 != 0) {
                    if !all.contains(&it.id) {
                        all.push(it.id);
                    }
                }
            }
            i += 1;
        }
        Some(all)
    });
    let Some(menus) = menus else { return fail(ERROR_INVALID_MENU_HANDLE, FALSE) };
    let mut infos = MENU_INFO.lock().unwrap();
    for h in menus {
        let m = infos.entry(h).or_default();
        if mask & MIM_STYLE != 0 {
            m.style = style;
        }
        if mask & MIM_MAXHEIGHT != 0 {
            m.max_height = max_height;
        }
        if mask & MIM_BACKGROUND != 0 {
            m.background = background;
        }
        if mask & MIM_HELPID != 0 && h == menu {
            m.help = help;
        }
        if mask & MIM_MENUDATA != 0 && h == menu {
            m.data = data;
        }
    }
    TRUE
}

// ---- drawing ------------------------------------------------------------------------------

const BF_LEFT: u32 = 0x1;
const BF_TOP: u32 = 0x2;
const BF_RIGHT: u32 = 0x4;
const BF_BOTTOM: u32 = 0x8;
const BF_DIAGONAL: u32 = 0x10;
const BF_MIDDLE: u32 = 0x800;
const BF_ADJUST: u32 = 0x2000;
const BF_FLAT: u32 = 0x4000;
const BF_MONO: u32 = 0x8000;

const COLOR_WINDOW: u32 = 5;
const COLOR_WINDOWFRAME: u32 = 6;
const COLOR_BTNFACE: u32 = 15;
const COLOR_BTNSHADOW: u32 = 16;
const COLOR_BTNHIGHLIGHT: u32 = 20;
const COLOR_3DDKSHADOW: u32 = 21;
const COLOR_3DLIGHT: u32 = 22;

/// DrawEdge: up to two one-pixel rings, the outer then the inner, in the
/// system's 3D colours (BDR_RAISED/SUNKEN OUTER/INNER), on the sides
/// `flags` names; BF_MIDDLE fills the rest and BF_ADJUST returns it.
/// Diagonal edges are not drawn (ERROR_NOT_SUPPORTED).
extern "win64" fn DrawEdge(dc: u64, rect: *mut u8, edge: u32, flags: u32) -> Bool {
    if rect.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if flags & BF_DIAGONAL != 0 {
        return fail(ERROR_NOT_SUPPORTED, FALSE);
    }
    let (l, t, r, b) = rect_at(rect);
    // (top-left colour, bottom-right colour) of each ring present.
    let ring = |raised: bool, sunken: bool, outer: bool| -> Option<(u32, u32)> {
        if flags & BF_MONO != 0 {
            return (raised || sunken).then_some((if outer { COLOR_WINDOWFRAME } else { COLOR_WINDOW }, if outer { COLOR_WINDOWFRAME } else { COLOR_WINDOW }));
        }
        if flags & BF_FLAT != 0 {
            return (raised || sunken).then_some(if outer { (COLOR_BTNSHADOW, COLOR_BTNSHADOW) } else { (COLOR_BTNFACE, COLOR_BTNFACE) });
        }
        match (raised, sunken, outer) {
            (true, _, true) => Some((COLOR_3DLIGHT, COLOR_3DDKSHADOW)),
            (true, _, false) => Some((COLOR_BTNHIGHLIGHT, COLOR_BTNSHADOW)),
            (_, true, true) => Some((COLOR_BTNSHADOW, COLOR_BTNHIGHLIGHT)),
            (_, true, false) => Some((COLOR_3DDKSHADOW, COLOR_3DLIGHT)),
            _ => None,
        }
    };
    let rings = [ring(edge & 1 != 0, edge & 2 != 0, true), ring(edge & 4 != 0, edge & 8 != 0, false)];
    let (mut l2, mut t2, mut r2, mut b2) = (l, t, r, b);
    let ok = draw(dc, |s, _, _, clip| {
        let (mut x0, mut y0, mut x1, mut y1) = (l, t, r, b);
        for (tl, br) in rings.iter().flatten() {
            if x1 <= x0 || y1 <= y0 {
                break;
            }
            let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
            let mut put = |r: Rect, c: u32| s.fill_rect(r.intersect(&clip), sys_color(c));
            if flags & BF_TOP != 0 {
                put(Rect::new(x0, y0, w, 1), *tl);
            }
            if flags & BF_LEFT != 0 {
                put(Rect::new(x0, y0, 1, h), *tl);
            }
            if flags & BF_BOTTOM != 0 {
                put(Rect::new(x0, y1 - 1, w, 1), *br);
            }
            if flags & BF_RIGHT != 0 {
                put(Rect::new(x1 - 1, y0, 1, h), *br);
            }
            x0 += (flags & BF_LEFT != 0) as i32;
            y0 += (flags & BF_TOP != 0) as i32;
            x1 -= (flags & BF_RIGHT != 0) as i32;
            y1 -= (flags & BF_BOTTOM != 0) as i32;
        }
        if flags & BF_MIDDLE != 0 && x1 > x0 && y1 > y0 {
            let face = if flags & BF_MONO != 0 { COLOR_WINDOW } else { COLOR_BTNFACE };
            s.fill_rect(Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&clip), sys_color(face));
        }
        (l2, t2, r2, b2) = (x0, y0, x1, y1);
        (TRUE, Rect::new(l, t, (r - l).max(0) as u32, (b - t).max(0) as u32))
    })
    .unwrap_or(FALSE);
    if ok != FALSE && flags & BF_ADJUST != 0 {
        put_rect(rect, (l2, t2, r2, b2));
    }
    ok
}

/// InvertRect: every pixel of the rectangle becomes its colour's inverse.
extern "win64" fn InvertRect(dc: u64, rect: *const u8) -> Bool {
    if rect.is_null() {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let (l, t, r, b) = rect_at(rect);
    if r <= l || b <= t {
        return TRUE;
    }
    let area = Rect::new(l, t, (r - l) as u32, (b - t) as u32);
    draw(dc, |s, _, _, clip| {
        let a = area.intersect(&clip);
        for y in a.y..a.y + a.h as i32 {
            for x in a.x..a.x + a.w as i32 {
                if let Some(c) = s.get(x, y) {
                    s.put(x, y, c ^ 0x00FF_FFFF);
                }
            }
        }
        (TRUE, a)
    })
    .unwrap_or(FALSE)
}

// ---- hot keys, resources, raw input -------------------------------------------------------

/// Registered hot keys: (window, id) -> (modifiers, virtual key). WM_HOTKEY
/// is not delivered: the compositor sends keys to the focused window only.
static HOTKEYS: Mutex<BTreeMap<(u64, i32), (u32, u32)>> = Mutex::new(BTreeMap::new());

extern "win64" fn RegisterHotKey(wnd: u64, id: i32, modifiers: u32, vk: u32) -> Bool {
    if wnd != 0 && !with(|g| g.windows.contains_key(&wnd)) {
        return fail(ERROR_INVALID_WINDOW_HANDLE, FALSE);
    }
    // MOD_NOREPEAT does not make a different key combination.
    let combo = (modifiers & 0xF, vk);
    let mut keys = HOTKEYS.lock().unwrap();
    if keys.iter().any(|(k, v)| *v == combo && *k != (wnd, id)) {
        return fail(ERROR_HOTKEY_ALREADY_REGISTERED, FALSE);
    }
    keys.insert((wnd, id), combo);
    TRUE
}

extern "win64" fn UnregisterHotKey(wnd: u64, id: i32) -> Bool {
    match HOTKEYS.lock().unwrap().remove(&(wnd, id)) {
        Some(_) => TRUE,
        None => fail(ERROR_HOTKEY_NOT_REGISTERED, FALSE),
    }
}

/// Highest GDI and USER object counts seen, for the _PEAK flags.
static PEAK: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// GetGuiResources: this process's GDI objects (pens, brushes, fonts,
/// bitmaps, regions, DCs) or USER objects (windows, menus, icons and
/// cursors), or their peak counts. Other processes' are not known here.
extern "win64" fn GetGuiResources(process: u64, flags: u32) -> u32 {
    let own = process == crate::objects::CURRENT_PROCESS
        || matches!(crate::objects::get(process), Some(crate::objects::Object::Process(p)) if p.pid == crate::kernel32::pid());
    if !own {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    let (gdi, user) = with(|g| {
        let gdi = g.gdi.keys().filter(|&&h| h < STOCK_BASE || h >= STOCK_BASE + 0x100).count() + g.dcs.len() + g.bitmaps.len() + g.regions.len();
        (gdi as u64, (g.windows.len() + g.menus.len() + g.icons.len()) as u64)
    });
    let gdi_peak = PEAK[0].fetch_max(gdi, Ordering::Relaxed).max(gdi);
    let user_peak = PEAK[1].fetch_max(user, Ordering::Relaxed).max(user);
    match flags {
        0 => gdi as u32,
        1 => user as u32,
        2 => gdi_peak as u32,
        4 => user_peak as u32,
        _ => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

/// DefRawInputProc: raw input needs no default processing; S_OK when the
/// header size is that of RAWINPUTHEADER, -1 otherwise.
extern "win64" fn DefRawInputProc(_input: u64, _count: i32, header_size: u32) -> i64 {
    let expected = if crate::wow64::is_active() { 16 } else { 24 };
    if header_size == expected { 0 } else { -1 }
}

// ---- gdi32 --------------------------------------------------------------------------------

const OBJ_PEN: u32 = 1;
const OBJ_BRUSH: u32 = 2;
const OBJ_DC: u32 = 3;
const OBJ_FONT: u32 = 6;
const OBJ_BITMAP: u32 = 7;
const OBJ_REGION: u32 = 8;
const OBJ_MEMDC: u32 = 10;

extern "win64" fn GetObjectType(h: u64) -> u32 {
    let kind = with(|g| {
        if let Some(o) = g.gdi.get(&h) {
            return Some(match o {
                Gdi::Brush(_) | Gdi::NullBrush => OBJ_BRUSH,
                Gdi::Pen(..) | Gdi::NullPen => OBJ_PEN,
                Gdi::Font(_) => OBJ_FONT,
                Gdi::Bitmap => OBJ_BITMAP,
                Gdi::Region => OBJ_REGION,
            });
        }
        if let Some(d) = g.dcs.get(&h) {
            return Some(if d.memory { OBJ_MEMDC } else { OBJ_DC });
        }
        if g.bitmaps.contains_key(&h) {
            return Some(OBJ_BITMAP);
        }
        g.regions.contains_key(&h).then_some(OBJ_REGION)
    });
    kind.unwrap_or_else(|| fail(ERROR_INVALID_HANDLE, 0))
}

extern "win64" fn CreateRectRgnIndirect(rect: *const u8) -> u64 {
    if rect.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let (l, t, r, b) = rect_at(rect);
    gdi::CreateRectRgn(l, t, r, b)
}

/// EqualRgn: whether two regions cover the same pixels (their exclusive-or
/// is empty); ERROR (-1) when either handle is not a region.
extern "win64" fn EqualRgn(a: u64, b: u64) -> i32 {
    if !with(|g| g.regions.contains_key(&a) && g.regions.contains_key(&b)) {
        return fail(ERROR_INVALID_HANDLE, -1);
    }
    let tmp = gdi::CreateRectRgn(0, 0, 0, 0);
    let kind = gdi::CombineRgn(tmp, a, b, 3); // RGN_XOR
    DeleteObject(tmp);
    match kind {
        0 => -1,
        1 => TRUE,
        _ => FALSE,
    }
}

/// The drawing mode SetROP2 last set, by DC (R2_COPYPEN until then).
/// Lines and shapes are drawn as with R2_COPYPEN whatever the mode.
static ROP2: Mutex<BTreeMap<u64, i32>> = Mutex::new(BTreeMap::new());
const R2_COPYPEN: i32 = 13;

extern "win64" fn SetROP2(dc: u64, mode: i32) -> i32 {
    if !(1..=16).contains(&mode) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !with(|g| g.dcs.contains_key(&dc)) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    ROP2.lock().unwrap().insert(dc, mode).unwrap_or(R2_COPYPEN)
}

extern "win64" fn GetROP2(dc: u64) -> i32 {
    if !with(|g| g.dcs.contains_key(&dc)) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    ROP2.lock().unwrap().get(&dc).copied().unwrap_or(R2_COPYPEN)
}

/// GetCharWidthW: advance widths of the characters `first..=last` in the
/// DC's font (logical units).
extern "win64" fn GetCharWidthW(dc: u64, first: u32, last: u32, widths: *mut i32) -> Bool {
    if widths.is_null() || first > last || last > 0x10_FFFF {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let Some(k) = with(|g| g.dcs.get(&dc).map(|d| font_of(&g.gdi, d.font))) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    for (i, c) in (first..=last).enumerate() {
        let ch = char::from_u32(c).unwrap_or('?');
        let mut buf = [0; 4];
        // SAFETY: the caller's array of last - first + 1 widths.
        unsafe { *widths.add(i) = measure(k, ch.encode_utf8(&mut buf)) as i32 };
    }
    TRUE
}

/// CreateFontIndirectA: the LOGFONTA's face name (ANSI is UTF-8) widened
/// into a LOGFONTW.
extern "win64" fn CreateFontIndirectA(lf: *const u8) -> u64 {
    if lf.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut w = [0u8; 92];
    // SAFETY: the caller's LOGFONTA (60 bytes: 28 of fields, 32 of name).
    unsafe {
        core::ptr::copy_nonoverlapping(lf, w.as_mut_ptr(), 28);
        let name = core::slice::from_raw_parts(lf.add(28), 32);
        let n = name.iter().position(|&c| c == 0).unwrap_or(32);
        let units: Vec<u16> = String::from_utf8_lossy(&name[..n]).encode_utf16().take(31).collect();
        for (i, u) in units.iter().enumerate() {
            w[28 + 2 * i..30 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
    }
    CreateFontIndirectW(w.as_ptr())
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CharUpperW,
        CharToOemBuffW,
        OemToCharA,
        OemToCharBuffA,
        BeginDeferWindowPos,
        DeferWindowPos,
        EndDeferWindowPos,
        LockWindowUpdate,
        GetMenuItemCount,
        SetMenuDefaultItem,
        InsertMenuItemW,
        GetMenuInfo,
        SetMenuInfo,
        DrawEdge,
        InvertRect,
        RegisterHotKey,
        UnregisterHotKey,
        GetGuiResources,
        DefRawInputProc
    ]
}

pub(super) fn gdi_exports() -> Vec<(&'static str, u64)> {
    export_table![GetObjectType, CreateRectRgnIndirect, EqualRgn, SetROP2, GetROP2, GetCharWidthW, CreateFontIndirectA]
}
