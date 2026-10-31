//! Pieces shared by the desktop programs: the event loop of a one-window
//! application, privilege dropping, application entries, formatting.
//! (Each program uses a different subset.)

#![allow(dead_code)]

use os_std::ipc::WaitItem;
use os_std::prelude::*;
use os_std::time::Duration;
use os_std::{env, fs, os};
use osyx_ui::{Event as Ui, Fonts, Icon, Painter, Theme};
use prism_client::ui::Adapter;
use prism_client::{Connection, Event, WindowSpec};

/// What an application tells the event loop.
pub struct Ctx {
    pub fonts: Fonts,
    pub theme: Theme,
    pub clipboard: osyx_ui::Clipboard,
    pub file_clipboard: Option<(Vec<String>, bool)>,
    pub clipboard_entries: Option<Vec<prism_client::proto::ClipEntry>>,
    /// The window has keyboard focus.
    pub focused: bool,
    /// Set to leave the event loop (the window is destroyed).
    pub quit: bool,
    /// A new window title.
    pub title: Option<String>,
    /// Notifications to post: (title, body).
    pub notes: Vec<(String, String)>,
    pub drag_file: Option<String>,
}

/// A program with one PRISM window driven by [`run_app`].
pub trait App {
    /// The window content size changed (also called before the first draw).
    fn layout(&mut self, w: u32, h: u32, cx: &mut Ctx);
    /// A toolkit event; returns true if the window must be redrawn.
    fn event(&mut self, ev: Ui, cx: &mut Ctx) -> bool;
    fn file_drop(&mut self, _path: &str, _x: i32, _y: i32, _cx: &mut Ctx) -> bool { false }
    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, cx: &Ctx);
    /// Extra handles to wait on (pipes, ...).
    fn wait_items(&self) -> Vec<WaitItem> {
        Vec::new()
    }
    /// Some of [`App::wait_items`] are ready; returns true to redraw.
    fn ready(&mut self, _items: &[WaitItem], _cx: &mut Ctx) -> bool {
        false
    }
    /// Periodic work every [`App::tick_ms`]; returns true to redraw.
    fn tick(&mut self, _cx: &mut Ctx) -> bool {
        false
    }
    fn tick_ms(&self) -> Option<u64> {
        None
    }
    /// The user asked to close the window; true to quit.
    fn close_requested(&mut self, _cx: &mut Ctx) -> bool {
        true
    }
}

/// Connects to PRISM, opens the window and runs `app` until it quits or
/// PRISM goes away. Returns the program's exit status.
pub fn run_app(name: &str, spec: WindowSpec, app: &mut dyn App) -> i32 {
    let mut conn = match Connection::connect(name, false) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{name}: cannot connect to PRISM: {e}");
            return 1;
        }
    };
    let mut win = match conn.create_window(spec) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{name}: cannot create a window: {e}");
            return 1;
        }
    };
    let mut cx = Ctx {
        fonts: Fonts::load(),
        theme: configured_theme(),
        clipboard: osyx_ui::Clipboard::default(),
        file_clipboard: None,
        clipboard_entries: None,
        focused: false,
        quit: false,
        title: None,
        notes: Vec::new(),
        drag_file: None,
    };
    let mut adapter = Adapter::default();
    let mut size = (0, 0);
    let mut last_tick = os_std::time::Instant::now();
    loop {
        let mut extra = app.wait_items();
        let timeout =
            app.tick_ms().map(|ms| Duration::from_millis(ms.saturating_sub(last_tick.elapsed().as_millis() as u64)));
        if let Err(e) = conn.wait(&mut extra, timeout) {
            eprintln!("{name}: lost PRISM: {e}");
            return 2;
        }
        if extra.iter().any(|i| i.revents != 0) && app.ready(&extra, &mut cx) {
            win.dirty = true;
        }
        loop {
            let ev = match conn.poll() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("{name}: lost PRISM: {e}");
                    return 2;
                }
            };
            win.on_event(&ev);
            match &ev {
                Event::ClipboardChanged { entries, .. } => {
                    cx.clipboard.receive(prism_client::proto::clipboard_text(entries));
                    cx.file_clipboard = prism_client::proto::clipboard_files(entries);
                }
                Event::FocusChanged { focused, .. } => {
                    cx.focused = *focused;
                    win.dirty = true;
                }
                Event::CloseRequest { .. } => {
                    if app.close_requested(&mut cx) {
                        cx.quit = true;
                    }
                }
                Event::FileDrop { window, path, x, y } if *window == win.id => {
                    if app.file_drop(path, *x, *y, &mut cx) { win.dirty = true; }
                }
                Event::Error { message, .. } => eprintln!("{name}: PRISM error: {message}"),
                _ => {}
            }
            if (win.width, win.height) != size && win.is_configured() {
                size = (win.width, win.height);
                app.layout(size.0, size.1, &mut cx);
                win.dirty = true;
            }
            if let Some((_, ui)) = adapter.translate(&ev) {
                if app.event(ui, &mut cx) {
                    win.dirty = true;
                }
                if let Some(text) = cx.clipboard.take_changed() {
                    let entry = prism_client::proto::clipboard_text_entry(text);
                    let _ = conn.send(&prism_client::Request::ClipboardSet { entries: vec![entry] }, &[]);
                }
                if let Some(entries) = cx.clipboard_entries.take() {
                    let _ = conn.send(&prism_client::Request::ClipboardSet { entries }, &[]);
                }
                if let Some(path) = cx.drag_file.take() {
                    let _ = conn.send(&prism_client::Request::FileDrag { window: win.id, path }, &[]);
                }
            }
        }
        if let Some(ms) = app.tick_ms() {
            if last_tick.elapsed() >= Duration::from_millis(ms) {
                last_tick = os_std::time::Instant::now();
                if app.tick(&mut cx) {
                    win.dirty = true;
                }
            }
        }
        if let Some(t) = cx.title.take() {
            let _ = win.set_title(&mut conn, &t);
        }
        for (t, b) in core::mem::take(&mut cx.notes) {
            let _ = conn.notify(&t, &b);
        }
        if cx.quit {
            let _ = win.destroy(&mut conn);
            return 0;
        }
        if win.dirty && win.can_draw() && size.0 > 0 {
            let (w, h) = size;
            match win.surface() {
                Ok(mut s) => {
                    let (theme, fonts) = (cx.theme, cx.fonts);
                    let mut p = Painter::new(&mut s, &theme, &fonts);
                    app.draw(&mut p, w, h, &cx);
                }
                Err(e) => {
                    eprintln!("{name}: no drawing buffer: {e}");
                    continue;
                }
            }
            if let Err(e) = win.present(&mut conn, None) {
                eprintln!("{name}: present failed: {e}");
            }
        }
    }
}

/// Applications run as the desktop user, not as root.
pub const USER_UID: u32 = 1000;
/// The administrators group ("sudo"), the desktop user's primary group: it
/// may change the system settings.
pub const ADMIN_GID: u32 = 27;
pub const USER_HOME: &str = "/home/user";

/// Drops root privileges (programs started by the shell run as root until
/// they call this) and sets the user's environment.
pub fn become_user() {
    if os::getuid().0 == 0 {
        if let Err(e) = os::setuid(USER_UID, ADMIN_GID) {
            eprintln!("cannot drop privileges: {e}");
        }
    }
    env::set_var("HOME", USER_HOME);
    env::set_var("USER", "user");
    let _ = env::set_current_dir(USER_HOME);
}

/// An installed application (`/system/share/applications/*.app`):
/// `key=value` lines with `Name`, `Exec`, `Icon` and `Comment`.
#[derive(Clone, Debug)]
pub struct AppEntry {
    pub source: String,
    pub name: String,
    pub exec: String,
    pub icon: Icon,
    pub comment: String,
    /// Arguments and working directory of a Windows shortcut.
    pub args: Vec<String>,
    pub workdir: String,
    /// The Start Menu folder of a shortcut ("" for the rest).
    pub group: String,
}

pub const APP_DIR: &str = "/system/share/applications";
pub const DESKTOP_DIR: &str = "/home/user/Desktop";

pub fn configured_theme() -> Theme {
    let mut theme = Theme::default();
    let name = fs::read_to_string("/etc/osyx/accent").unwrap_or_default();
    let (r, g, b) = match name.trim() {
        "Purple" => (160, 110, 240),
        "Pink" => (236, 92, 160),
        "Red" => (240, 84, 84),
        "Orange" => (245, 150, 60),
        "Yellow" => (240, 200, 60),
        "Green" => (80, 196, 120),
        "Graphite" => (140, 146, 160),
        _ => (76, 141, 255),
    };
    theme.accent = osyx_gfx::rgb(r, g, b);
    theme.accent_hover = osyx_gfx::rgb(r.saturating_add(20), g.saturating_add(20), b.saturating_add(20));
    theme.accent_pressed = osyx_gfx::rgb(r.saturating_sub(20), g.saturating_sub(20), b.saturating_sub(20));
    theme
}

pub fn icon_by_name(name: &str) -> Icon {
    match name {
        "terminal" => Icon::Terminal,
        "files" | "folder" => Icon::Folder,
        "info" | "sysinfo" => Icon::Info,
        "zen" | "browser" => Icon::Browser,
        _ => Icon::Program,
    }
}

/// Programs larger than this are not read for their icon.
const MAX_ICON_SOURCE: u64 = 64 << 20;

/// Icons found so far: program path -> (file size when read, its icon).
static EXE_ICONS: os_std::sync::Mutex<os_std::collections::BTreeMap<String, (u64, Option<Icon>)>> =
    os_std::sync::Mutex::new(os_std::collections::BTreeMap::new());

/// The icon of a Windows program: its first icon group, in the size that
/// suits the shell best; the generic program icon when there is none. Read
/// once per program (and again if its size changes).
pub fn exe_icon(path: &str) -> Icon {
    icon_from(path, 0).unwrap_or(Icon::Program)
}

/// The icon at `index` of `path`: an `.ico` file, or a program or library
/// (index 0 is the first icon group, a negative index names the group's
/// resource id). `None` when the file has no icon this shell can draw.
pub fn icon_from(path: &str, index: i32) -> Option<Icon> {
    let key = format!("{path}#{index}");
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Some((known, icon)) = EXE_ICONS.lock().ok().and_then(|c| c.get(&key).copied()) {
        if known == len {
            return icon;
        }
    }
    let icon = read_icon(path, len, index);
    if let Ok(mut cache) = EXE_ICONS.lock() {
        cache.insert(key, (len, icon));
    }
    icon
}

/// Registers the best-sized candidate of (rank, colours, data) that decodes.
fn register_best(mut candidates: Vec<(usize, u16, &[u8])>) -> Option<Icon> {
    candidates.sort_by_key(|c| (c.0, c.1));
    for (_, _, data) in candidates {
        if let Some(icon) = osyx_ui::icon::decode_image(data).and_then(osyx_ui::icon::register_bitmap) {
            return Some(icon);
        }
    }
    None
}

/// The shell draws icons at 16 to 40 pixels: 48 and 32 look best.
const PREFERRED_SIZES: [u32; 6] = [48, 32, 64, 24, 16, 256];

fn read_icon(path: &str, len: u64, index: i32) -> Option<Icon> {
    if len == 0 || len > MAX_ICON_SOURCE {
        return None;
    }
    let image = fs::read(path).ok()?;
    if path.to_ascii_lowercase().ends_with(".ico") {
        // ICONDIR: reserved, type (1), count; then 16-byte entries of width,
        // height, colours, reserved, planes, bit count, size and offset.
        let u16_at = |at: usize| image.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |at: usize| image.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let mut candidates = Vec::new();
        for i in 0..(u16_at(4)? as usize).min(64) {
            let at = 6 + i * 16;
            let width = match *image.get(at)? {
                0 => 256,
                w => w as u32,
            };
            let (bits, size, offset) = (u16_at(at + 6)?, u32_at(at + 8)?, u32_at(at + 12)?);
            if let Some(rank) = PREFERRED_SIZES.iter().position(|&w| w == width) {
                if let Some(data) = image.get(offset..offset.checked_add(size)?) {
                    candidates.push((rank, u16::MAX - bits, data));
                }
            }
        }
        return register_best(candidates);
    }
    let pe = osyx_pe::PeFile::parse_any(&image).ok()?;
    // GRPICONDIR: reserved, type (1), count; then 14-byte entries of width,
    // height, colours, reserved, planes, bit count, size and RT_ICON id.
    let group_id = if index < 0 { u16::try_from(-(index as i64)).ok() } else { None };
    let group = pe.resource(14, group_id, None).ok()??;
    let u16_at = |at: usize| group.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let count = u16_at(4)? as usize;
    let mut candidates: Vec<(usize, u16, &[u8])> = Vec::new();
    for i in 0..count.min(64) {
        let at = 6 + i * 14;
        let width = match *group.get(at)? {
            0 => 256,
            w => w as u32,
        };
        let (bits, id) = (u16_at(at + 6)?, u16_at(at + 12)?);
        if let Some(rank) = PREFERRED_SIZES.iter().position(|&w| w == width) {
            if let Some(data) = pe.resource(3, Some(id), None).ok().flatten() {
                candidates.push((rank, u16::MAX - bits, data));
            }
        }
    }
    register_best(candidates)
}

/// The launcher's applications: the installed `.app` entries, then Windows
/// programs (Start Menu shortcuts, and programs found in Program Files).
pub fn load_apps() -> Vec<AppEntry> {
    let mut apps = load_apps_from(APP_DIR);
    for app in windows_apps() {
        if !apps.iter().any(|a| a.exec == app.exec) {
            apps.push(app);
        }
    }
    apps
}

/// Windows programs: Start Menu shortcuts of the user and of all users, then
/// programs in `/Program Files` and `/Program Files (x86)` that no shortcut
/// names. A shortcut's own file name is the program's name.
pub fn windows_apps() -> Vec<AppEntry> {
    let mut apps: Vec<AppEntry> = Vec::new();
    let home = env::var("HOME").unwrap_or_else(|_| String::from("/home/user"));
    let menus = [
        format!("{}/AppData/Roaming/Microsoft/Windows/Start Menu/Programs", home.trim_end_matches('/')),
        String::from("/ProgramData/Microsoft/Windows/Start Menu/Programs"),
    ];
    for menu in &menus {
        collect_links(menu, 2, "", &mut apps);
    }
    for root in ["/Program Files", "/Program Files (x86)"] {
        let Ok(dir) = fs::read_dir(root) else { continue };
        let mut names: Vec<String> =
            dir.filter(|e| e.file_type().is_dir()).map(|e| String::from(e.file_name())).collect();
        names.sort();
        for name in names {
            let folder = format!("{root}/{name}");
            let Some(exe) = main_program(&folder, &name) else { continue };
            if apps.iter().any(|a| a.exec.eq_ignore_ascii_case(&exe)) {
                continue;
            }
            let icon = exe_icon(&exe);
            apps.push(AppEntry { source: exe.clone(), name, exec: exe, icon, comment: String::from("Windows program"), args: Vec::new(), workdir: String::new(), group: String::new() });
        }
    }
    apps.sort_by_key(|a| (a.group.to_ascii_lowercase(), a.name.to_ascii_lowercase()));
    apps
}

/// The program of an install folder: the `.exe` named like the folder, else
/// the only `.exe` that is not an uninstaller.
fn main_program(folder: &str, name: &str) -> Option<String> {
    let mut exes: Vec<String> = fs::read_dir(folder)
        .ok()?
        .map(|e| String::from(e.file_name()))
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".exe") && !l.contains("unins")
        })
        .collect();
    exes.sort();
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let wanted = squash(name);
    if let Some(n) = exes.iter().find(|n| squash(n.get(..n.len() - 4).unwrap_or(n)) == wanted) {
        return Some(format!("{folder}/{n}"));
    }
    (exes.len() == 1).then(|| format!("{folder}/{}", exes[0]))
}

/// Shortcuts (`.lnk`) of a Start Menu folder, `depth` levels down.
fn collect_links(dir: &str, depth: u32, group: &str, apps: &mut Vec<AppEntry>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut items: Vec<(String, bool)> =
        entries.map(|e| (String::from(e.file_name()), e.file_type().is_dir())).collect();
    items.sort();
    for (name, is_dir) in items {
        let path = format!("{dir}/{name}");
        if is_dir {
            if depth > 1 {
                // Only the first folder below the Start Menu names a group.
                collect_links(&path, depth - 1, if group.is_empty() { &name } else { group }, apps);
            }
        } else if name.to_ascii_lowercase().ends_with(".lnk") && !name.to_ascii_lowercase().contains("uninstall") {
            if let Some(mut app) = load_link(&path) {
                app.group = String::from(group);
                if !apps.iter().any(|a| a.exec == app.exec) {
                    apps.push(app);
                }
            }
        }
    }
}

/// An application entry for a Windows shortcut file: named like the file,
/// started through its target.
pub fn load_link(path: &str) -> Option<AppEntry> {
    let data = fs::read(path).ok()?;
    let link = read_link(&data)?;
    // Windows names are not case sensitive, the file system's are.
    let target = existing_path(&link.target)?;
    if !target.to_ascii_lowercase().ends_with(".exe") || !fs::metadata(&target).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = file.get(..file.len().saturating_sub(4)).unwrap_or(file);
    // The shortcut's own icon, else the program's.
    let icon = link.icon.as_ref().and_then(|(file, index)| existing_path(file).and_then(|f| icon_from(&f, *index))).unwrap_or_else(|| exe_icon(&target));
    Some(AppEntry {
        source: String::from(path),
        name: String::from(name),
        exec: target,
        icon,
        comment: String::from("Windows program"),
        args: split_arguments(&link.arguments),
        workdir: existing_path(&link.working_dir).unwrap_or_default(),
        group: String::new(),
    })
}

/// `path` with each component spelled as it is on disk (the first entry that
/// matches ignoring case), or `None` if some component does not exist.
fn existing_path(path: &str) -> Option<String> {
    let mut found = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let dir = if found.is_empty() { "/" } else { found.as_str() };
        let exact = format!("{found}/{part}");
        if fs::metadata(&exact).is_ok() {
            found = exact;
            continue;
        }
        let name = fs::read_dir(dir).ok()?.map(|e| String::from(e.file_name())).find(|n| n.eq_ignore_ascii_case(part))?;
        found = format!("{found}/{name}");
    }
    (!found.is_empty()).then_some(found)
}

/// What the shell uses of a Shell Link (MS-SHLLINK); paths are OSyx paths.
pub struct LinkData {
    pub target: String,
    pub arguments: String,
    pub working_dir: String,
    /// Icon file and index, if the link names one.
    pub icon: Option<(String, i32)>,
}

/// A Windows path (`C:\a\b`) as an OSyx path (`/a/b`); `None` for anything
/// that is not on drive C.
fn windows_to_osyx(win: &str) -> Option<String> {
    let rest = win.get(2..).filter(|_| win.as_bytes().get(1) == Some(&b':'))?;
    Some(rest.replace('\\', "/"))
}

/// Reads a Shell Link: the local base path of its LinkInfo (else its
/// relative path string when that is a full path), the working directory,
/// the arguments and the icon location. Network targets and shell items
/// without a path have no target here.
pub fn read_link(data: &[u8]) -> Option<LinkData> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if u32_at(0)? != 0x4C {
        return None;
    }
    let flags = u32_at(0x14)?;
    let icon_index = u32_at(0x38)? as u32 as i32;
    let unicode = flags & 0x80 != 0;
    let mut at = 0x4C;
    if flags & 0x01 != 0 {
        at += 2 + u16_at(at)?; // LinkTargetIDList
    }
    let mut target = None;
    if flags & 0x02 != 0 {
        let base = at;
        let size = u32_at(base)?;
        if u32_at(base + 8)? & 1 != 0 {
            let text = |from: usize| -> Option<String> {
                let bytes = data.get(from..)?;
                let end = bytes.iter().position(|&b| b == 0)?;
                Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
            };
            let mut path = text(base + u32_at(base + 0x10)?)?;
            let tail = text(base + u32_at(base + 0x18)?).unwrap_or_default();
            if !tail.is_empty() {
                if !path.ends_with('\\') {
                    path.push('\\');
                }
                path.push_str(&tail);
            }
            target = Some(path);
        }
        at = base + size;
    }
    // StringData in order: name, relative path, working directory, arguments, icon.
    let (mut relative, mut working_dir, mut arguments, mut icon) = (None, String::new(), String::new(), String::new());
    for (i, bit) in [0x04usize, 0x08, 0x10, 0x20, 0x40].into_iter().enumerate() {
        if flags & bit == 0 {
            continue;
        }
        let count = u16_at(at)?;
        at += 2;
        let bytes = if unicode { count * 2 } else { count };
        let raw = data.get(at..at + bytes)?;
        at += bytes;
        let text = if unicode {
            char::decode_utf16((0..raw.len() / 2).map(|i| u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]])))
                .map(|c| c.unwrap_or('\u{FFFD}'))
                .collect::<String>()
        } else {
            String::from_utf8_lossy(raw).into_owned()
        };
        match i {
            1 => relative = Some(text),
            2 => working_dir = text,
            3 => arguments = text,
            4 => icon = text,
            _ => {}
        }
    }
    let win = target.or(relative.filter(|r| r.contains(":\\")))?;
    Some(LinkData {
        target: windows_to_osyx(&win)?,
        arguments,
        working_dir: windows_to_osyx(&working_dir).unwrap_or_default(),
        // "C:\path\file.ico" or "C:\path\app.exe,3": the index may ride in the text.
        icon: if icon.is_empty() {
            None
        } else {
            let (file, index) = match icon.rsplit_once(',') {
                Some((f, i)) if i.trim().parse::<i32>().is_ok() => (f, i.trim().parse().unwrap_or(0)),
                _ => (icon.as_str(), icon_index),
            };
            windows_to_osyx(file.trim_matches('"')).map(|f| (f, index))
        },
    })
}

/// A command line split the way Windows programs read theirs: spaces
/// separate, double quotes group, `\"` is a quote.
pub fn split_arguments(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'"') => {
                chars.next();
                current.push('"');
                started = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(core::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

pub fn load_apps_from(path: &str) -> Vec<AppEntry> {
    let mut apps = Vec::new();
    let Ok(dir) = fs::read_dir(path) else { return apps };
    let mut names: Vec<String> = dir
        .map(|e| String::from(e.file_name()))
        .filter(|n| n.ends_with(".app") || n.to_ascii_lowercase().ends_with(".lnk"))
        .collect();
    names.sort();
    for n in names {
        let file = format!("{path}/{n}");
        let app = if n.ends_with(".app") { load_app(&file) } else { load_link(&file) };
        if let Some(app) = app {
            apps.push(app);
        }
    }
    apps
}

/// The text of every installed application's `.app` file (for readers of
/// keys `AppEntry` does not keep, such as `Handles=`).
pub fn load_app_texts() -> Vec<String> {
    let Ok(dir) = fs::read_dir(APP_DIR) else { return Vec::new() };
    let mut names: Vec<String> = dir.map(|e| String::from(e.file_name())).filter(|n| n.ends_with(".app")).collect();
    names.sort();
    names.iter().filter_map(|n| fs::read_to_string(format!("{APP_DIR}/{n}")).ok()).collect()
}

pub fn load_app(path: &str) -> Option<AppEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut e = AppEntry {
        source: String::from(path),
        name: String::new(),
        exec: String::new(),
        icon: Icon::Program,
        comment: String::new(),
        args: Vec::new(),
        workdir: String::new(),
        group: String::new(),
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => e.name = String::from(v.trim()),
            "Exec" => e.exec = String::from(v.trim()),
            "Icon" => e.icon = icon_by_name(v.trim()),
            "Comment" => e.comment = String::from(v.trim()),
            _ => {}
        }
    }
    if !e.name.is_empty() && e.exec.starts_with('/') {
        Some(e)
    } else {
        None
    }
}

/// `1.5 MiB`
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut u = 0;
    let mut div = 1u128;
    while bytes as u128 / div >= 1024 && u < UNITS.len() - 1 {
        div *= 1024;
        u += 1;
    }
    let tenths = ((bytes as u128 * 10 + div / 2) / div) as u64;
    if tenths >= 100 {
        format!("{} {}", tenths / 10, UNITS[u])
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[u])
    }
}

/// `3 h 07 min`, `42 s`
pub fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d} d {h} h {m:02} min")
    } else if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

/// The configured time zone (`/etc/TZ`, a POSIX TZ string), UTC otherwise.
pub fn system_tz() -> osyx_ui::time::Tz {
    fs::read_to_string("/etc/TZ")
        .ok()
        .and_then(|s| osyx_ui::time::Tz::parse(s.trim()))
        .unwrap_or_else(osyx_ui::time::Tz::utc)
}

/// Seconds since the Unix epoch from the real-time clock.
pub fn unix_now() -> i64 {
    os_std::time::SystemTime::now()
        .duration_since(os_std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The taskbar's pinned applications, one source file per line: an `.app`
/// entry, a Windows shortcut or a program.
pub const PINS_FILE: &str = "/home/user/.pinned";

/// The entry a pinned source stands for (`None` once it is gone).
pub fn load_pinned(source: &str) -> Option<AppEntry> {
    let lower = source.to_ascii_lowercase();
    if lower.ends_with(".app") {
        load_app(source)
    } else if lower.ends_with(".lnk") {
        load_link(source)
    } else if lower.ends_with(".exe") && fs::metadata(source).is_ok_and(|m| m.is_file()) {
        let file = source.rsplit('/').next().unwrap_or(source);
        let name = file.get(..file.len().saturating_sub(4)).unwrap_or(file);
        Some(AppEntry {
            source: String::from(source),
            name: String::from(name),
            exec: String::from(source),
            icon: exe_icon(source),
            comment: String::from("Windows program"),
            args: Vec::new(),
            workdir: String::new(),
            group: String::new(),
        })
    } else {
        None
    }
}

/// The pinned entries, in the order they were pinned.
pub fn load_pins() -> Vec<AppEntry> {
    fs::read_to_string(PINS_FILE)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| load_pinned(l.trim()))
        .collect()
}

/// Writes the pinned sources back.
pub fn save_pins(pins: &[AppEntry]) -> os_std::error::Result<()> {
    let mut text = String::new();
    for pin in pins {
        text.push_str(&pin.source);
        text.push('\n');
    }
    fs::write(PINS_FILE, text)
}
