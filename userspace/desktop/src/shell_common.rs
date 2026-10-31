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
static EXE_ICONS: os_std::sync::Mutex<os_std::collections::BTreeMap<String, (u64, Icon)>> =
    os_std::sync::Mutex::new(os_std::collections::BTreeMap::new());

/// The icon of a Windows program: its first icon group, in the size that
/// suits the shell best; the generic program icon when there is none. Read
/// once per program (and again if its size changes).
pub fn exe_icon(path: &str) -> Icon {
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Some(&(known, icon)) = EXE_ICONS.lock().ok().and_then(|c| c.get(path).copied()).as_ref() {
        if known == len {
            return icon;
        }
    }
    let icon = read_exe_icon(path, len).unwrap_or(Icon::Program);
    if let Ok(mut cache) = EXE_ICONS.lock() {
        cache.insert(String::from(path), (len, icon));
    }
    icon
}

fn read_exe_icon(path: &str, len: u64) -> Option<Icon> {
    if len == 0 || len > MAX_ICON_SOURCE {
        return None;
    }
    let image = fs::read(path).ok()?;
    let pe = osyx_pe::PeFile::parse_any(&image).ok()?;
    // GRPICONDIR: reserved, type (1), count; then 14-byte entries of width,
    // height, colours, reserved, planes, bit count, size and RT_ICON id.
    let group = pe.resource(14, None, None).ok()??;
    let u16_at = |at: usize| group.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let count = u16_at(4)? as usize;
    // The shell draws icons at 16 to 40 pixels: 48 and 32 look best.
    const PREFERRED: [u32; 5] = [48, 32, 64, 24, 16];
    let mut candidates: Vec<(usize, u16, u16)> = Vec::new();
    for i in 0..count.min(64) {
        let at = 6 + i * 14;
        let width = match *group.get(at)? {
            0 => 256,
            w => w as u32,
        };
        let (bits, id) = (u16_at(at + 6)?, u16_at(at + 12)?);
        if let Some(rank) = PREFERRED.iter().position(|&w| w == width) {
            candidates.push((rank, u16::MAX - bits, id)); // best size first, then most colours
        }
    }
    candidates.sort();
    for (_, _, id) in candidates {
        let Some(data) = pe.resource(3, Some(id), None).ok().flatten() else { continue };
        if let Some(bitmap) = osyx_ui::icon::decode_dib(data) {
            if let Some(icon) = osyx_ui::icon::register_bitmap(bitmap) {
                return Some(icon);
            }
        }
    }
    None
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
        collect_links(menu, 2, &mut apps);
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
            apps.push(AppEntry { source: exe.clone(), name, exec: exe, icon, comment: String::from("Windows program") });
        }
    }
    apps.sort_by_key(|a| a.name.to_ascii_lowercase());
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
fn collect_links(dir: &str, depth: u32, apps: &mut Vec<AppEntry>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut items: Vec<(String, bool)> =
        entries.map(|e| (String::from(e.file_name()), e.file_type().is_dir())).collect();
    items.sort();
    for (name, is_dir) in items {
        let path = format!("{dir}/{name}");
        if is_dir {
            if depth > 1 {
                collect_links(&path, depth - 1, apps);
            }
        } else if name.to_ascii_lowercase().ends_with(".lnk") && !name.to_ascii_lowercase().contains("uninstall") {
            if let Some(app) = load_link(&path) {
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
    // Windows names are not case sensitive, the file system's are.
    let target = existing_path(&link_target(&data)?)?;
    if !target.to_ascii_lowercase().ends_with(".exe") || !fs::metadata(&target).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = file.get(..file.len().saturating_sub(4)).unwrap_or(file);
    let icon = exe_icon(&target);
    Some(AppEntry {
        source: String::from(path),
        name: String::from(name),
        exec: target,
        icon,
        comment: String::from("Windows program"),
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

/// The target of a Shell Link (MS-SHLLINK) as an OSyx path: the local base
/// path of its LinkInfo, else its relative path string when that is a full
/// path. Anything else (network targets, shell items without a path) has no
/// target here.
pub fn link_target(data: &[u8]) -> Option<String> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if u32_at(0)? != 0x4C {
        return None;
    }
    let flags = u32_at(0x14)?;
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
    let mut relative = None;
    for (i, bit) in [0x04usize, 0x08, 0x10, 0x20, 0x40].into_iter().enumerate() {
        if flags & bit == 0 {
            continue;
        }
        let count = u16_at(at)?;
        at += 2;
        let bytes = if unicode { count * 2 } else { count };
        let raw = data.get(at..at + bytes)?;
        at += bytes;
        if i == 1 {
            relative = Some(if unicode {
                char::decode_utf16((0..raw.len() / 2).map(|i| u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]])))
                    .map(|c| c.unwrap_or('\u{FFFD}'))
                    .collect::<String>()
            } else {
                String::from_utf8_lossy(raw).into_owned()
            });
        }
    }
    let win = target.or(relative.filter(|r| r.contains(":\\")))?;
    let rest = win.get(2..).filter(|_| win.as_bytes().get(1) == Some(&b':'))?;
    Some(rest.replace('\\', "/"))
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
