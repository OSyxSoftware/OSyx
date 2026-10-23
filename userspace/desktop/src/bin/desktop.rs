//! The OSyx desktop shell (`/system/bin/desktop`), started by PRISM and
//! granted the shell role. A separate process from the compositor: if it
//! crashes, PRISM keeps every application window and starts it again.
//!
//! It provides the desktop background, the task bar (launcher, running
//! windows with the active one marked, clock, power button), the
//! application launcher (applications installed on the root file system
//! under `/system/share/applications`), the desktop context menu,
//! notification bubbles and the shut down / restart dialog.

#![no_std]
#![no_main]

#[path = "../shell_common.rs"]
mod common;

use common::{load_apps, system_tz, unix_now, AppEntry};
use os_std::prelude::*;
use os_std::process::{self, Command};
use os_std::time::{Duration, Instant};
use os_std::{system, thread};
use osyx_gfx::{rgb, Color, Rect, Surface};
use osyx_input::{button, key};
use osyx_ui::layout::{contains, inset};
use osyx_ui::time::Tz;
use osyx_ui::widget::{Button, ButtonStyle, Column, Dialog, ListItem, ListView, Menu, MenuItem, TextField};
use osyx_ui::{icon, Align, Event as Ui, FontKind, Fonts, Icon, Painter, Response, Theme};
use prism_client::ui::Adapter;
use prism_client::{proto, Action, Connection, Event, Role, Window, WindowSpec};
use prism_proto::{state, WindowInfo};

os_std::entry!(main);

const PANEL_H: u32 = 52;
const NOTE_W: u32 = 360;
const NOTE_H: u32 = 84;
const NOTE_MS: u64 = 6000;
const MAX_NOTES: usize = 4;
const LAUNCHER_W: u32 = 380;
const LAUNCHER_H: u32 = 470;
const CLOCK_W: u32 = 132;
/// Procedural backgrounds after the default image.
const WALLPAPERS: usize = 4;
/// The default wallpaper on the root file system (OSXI, converted from PNG at build time).
const DEFAULT_WALLPAPER: &str = "/system/share/wallpapers/default.osxi";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuPurpose {
    Desktop,
    Power,
}

const CONFIRM_OFF: u32 = 1;
const CONFIRM_REBOOT: u32 = 2;

enum PopupKind {
    Launcher { search: TextField, list: ListView, shown: Vec<usize>, list_focus: bool },
    Menu { menu: Menu, purpose: MenuPurpose },
    Confirm { dialog: Dialog },
}

struct Popup {
    win: Window,
    kind: PopupKind,
}

impl Popup {
    fn tag(&self) -> PopupTag {
        match &self.kind {
            PopupKind::Launcher { .. } => PopupTag::Launcher,
            PopupKind::Menu { purpose: MenuPurpose::Power, .. } => PopupTag::PowerMenu,
            _ => PopupTag::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PopupTag {
    Launcher,
    PowerMenu,
    Other,
}

struct Note {
    win: Window,
    title: String,
    body: String,
    born: u64,
}

enum PanelAction {
    Launcher,
    Power,
    Task(usize),
    Clock,
}

/// Exits of the shell's children, collected by the reaper thread.
static EXITS: os_std::sync::Mutex<Vec<(u64, i32)>> = os_std::sync::Mutex::new(Vec::new());

struct Shell {
    conn: Connection,
    fonts: Fonts,
    theme: Theme,
    adapter: Adapter,
    start: Instant,
    w: u32,
    h: u32,
    desktop: Window,
    wallpaper: Vec<u32>,
    /// The default wallpaper image file, if installed.
    image: Option<Vec<u8>>,
    style: usize,
    panel: Window,
    launcher_btn: Button,
    power_btn: Button,
    tasks: Vec<Button>,
    windows: Vec<WindowInfo>,
    clock: (String, String),
    clock_hover: bool,
    tz: Tz,
    popup: Option<Popup>,
    /// When PRISM last closed a popup (a click on the popup's own toggle
    /// button must not reopen it at once).
    popup_closed: Option<(u64, PopupTag)>,
    notes: Vec<Note>,
    apps: Vec<AppEntry>,
    /// (pid, application name) of programs started from the shell.
    children: Vec<(u64, String)>,
}

fn main() -> i32 {
    let mut conn = match Connection::connect("desktop", true) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("desktop: cannot connect to PRISM: {e}");
            return 1;
        }
    };
    if !conn.shell {
        eprintln!("desktop: PRISM did not grant the shell role");
        return 1;
    }
    let (w, h) = (conn.screen_width, conn.screen_height);
    let windows = conn.create_window(WindowSpec::with_role(Role::Desktop, 0, 0, w, h)).and_then(|d| {
        conn.create_window(WindowSpec::with_role(Role::Panel, 0, (h - PANEL_H) as i32, w, PANEL_H)).map(|p| (d, p))
    });
    let (desktop, panel) = match windows {
        Ok(v) => v,
        Err(e) => {
            eprintln!("desktop: cannot create the desktop windows: {e}");
            return 1;
        }
    };
    thread::spawn(reaper);
    let fonts = Fonts::load();
    let mut shell = Shell {
        conn,
        fonts,
        theme: common::configured_theme(),
        adapter: Adapter::default(),
        start: Instant::now(),
        w,
        h,
        desktop,
        wallpaper: vec![0; (w * h) as usize],
        image: os_std::fs::read(DEFAULT_WALLPAPER).ok().filter(|d| osyx_gfx::Image::parse(d).is_ok()),
        style: os_std::fs::read_to_string("/etc/osyx/wallpaper")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0)
            % WALLPAPERS,
        panel,
        launcher_btn: Button::new(0, "", ButtonStyle::Flat).with_icon(Icon::Apps),
        power_btn: Button::new(0, "", ButtonStyle::Flat).with_icon(Icon::Power),
        tasks: Vec::new(),
        windows: Vec::new(),
        clock: (String::new(), String::new()),
        clock_hover: false,
        tz: system_tz(),
        popup: None,
        popup_closed: None,
        notes: Vec::new(),
        apps: load_apps(),
        children: Vec::new(),
    };
    let t0 = Instant::now();
    if shell.image.is_none() {
        eprintln!("desktop: {DEFAULT_WALLPAPER} missing or invalid; using a drawn background");
        shell.style = 1;
    }
    paint_wallpaper(&mut shell.wallpaper, w, h, shell.style, shell.image.as_deref(), &shell.fonts);
    println!("desktop: wallpaper {} drawn in {} ms", shell.style, t0.elapsed().as_millis());
    shell.layout_panel();
    shell.update_clock();
    println!(
        "desktop: shell ready on a {}x{} screen, {} application(s) in {}, time zone {}",
        w,
        h,
        shell.apps.len(),
        common::APP_DIR,
        shell.tz.std_name
    );
    shell.run()
}

/// Collects the exit status of every application the shell started.
fn reaper() {
    loop {
        match process::wait_any() {
            Ok((pid, status)) => {
                if let Ok(mut q) = EXITS.lock() {
                    q.push((pid, status.raw()));
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

impl Shell {
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn run(&mut self) -> i32 {
        loop {
            let timeout = if self.notes.is_empty() { 1000 } else { 250 };
            if let Err(e) = self.conn.wait(&mut [], Some(Duration::from_millis(timeout))) {
                eprintln!("desktop: lost PRISM: {e}");
                return 2;
            }
            loop {
                match self.conn.poll() {
                    Ok(Some(ev)) => self.event(ev),
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("desktop: lost PRISM: {e}");
                        return 2;
                    }
                }
            }
            self.update_clock();
            self.expire_notes();
            self.collect_exits();
            if let Err(e) = self.redraw() {
                eprintln!("desktop: drawing failed: {e}");
            }
        }
    }

    // ---- events ------------------------------------------------------------

    fn event(&mut self, ev: Event) {
        match &ev {
            Event::WindowList { windows } => {
                self.windows = windows.clone();
                self.layout_panel();
                return;
            }
            Event::Notification { from_pid, title, body } => {
                println!("desktop: notification from process {from_pid}: {title}");
                let (t, b) = (title.clone(), body.clone());
                self.show_note(&t, &b);
                return;
            }
            Event::Error { code, message, .. } => {
                eprintln!("desktop: PRISM error {code}: {message}");
                return;
            }
            _ => {}
        }
        self.desktop.on_event(&ev);
        self.panel.on_event(&ev);
        if let Some(p) = &mut self.popup {
            p.win.on_event(&ev);
        }
        for n in &mut self.notes {
            n.win.on_event(&ev);
        }
        if let Event::CloseRequest { window } = ev {
            if self.popup.as_ref().is_some_and(|p| p.win.id == window) {
                let tag = self.popup.as_ref().map_or(PopupTag::Other, Popup::tag);
                self.popup_closed = Some((self.now(), tag));
                self.close_popup();
            }
            return;
        }
        if let Event::Key { window, usage, state: s, .. } = ev {
            // The Super key reaches the panel wherever the focus is.
            if window == self.panel.id {
                if (usage == key::LEFT_GUI || usage == key::RIGHT_GUI) && s == proto::key_state::DOWN {
                    self.toggle_launcher();
                }
                return;
            }
        }
        let Some((win, ui)) = self.adapter.translate(&ev) else { return };
        if win == self.desktop.id {
            self.desktop_input(ui);
        } else if win == self.panel.id {
            self.panel_input(ui);
        } else if self.popup.as_ref().is_some_and(|p| p.win.id == win) {
            self.popup_input(ui);
        } else if let Some(i) = self.notes.iter().position(|n| n.win.id == win) {
            if matches!(ui, Ui::PointerDown { .. }) {
                let n = self.notes.remove(i);
                let _ = n.win.destroy(&mut self.conn);
                self.restack_notes();
            }
        }
    }

    fn desktop_input(&mut self, ui: Ui) {
        if let Ui::PointerDown { x, y, button: b, .. } = ui {
            if b == button::RIGHT {
                let items = vec![
                    MenuItem::new("Open Terminal", Some(Icon::Terminal)),
                    MenuItem::new("Open Files", Some(Icon::Folder)),
                    MenuItem::new("System Information", Some(Icon::Info)),
                    MenuItem::new("Next wallpaper", Some(Icon::Refresh)).separated(),
                    MenuItem::new("About OSyx", Some(Icon::Bell)),
                ];
                self.open_menu(items, MenuPurpose::Desktop, x, y);
            }
        }
    }

    fn panel_input(&mut self, ui: Ui) {
        let mut action = None;
        let mut redraw = false;
        match self.launcher_btn.event(&ui, false) {
            Response::Activated(_) => action = Some(PanelAction::Launcher),
            r => redraw |= r.needs_redraw(),
        }
        match self.power_btn.event(&ui, false) {
            Response::Activated(_) => action = Some(PanelAction::Power),
            r => redraw |= r.needs_redraw(),
        }
        for b in &mut self.tasks {
            match b.event(&ui, false) {
                Response::Activated(i) => action = Some(PanelAction::Task(i)),
                r => redraw |= r.needs_redraw(),
            }
        }
        let clock = self.clock_rect();
        match ui {
            Ui::PointerMove { x, y } => {
                let h = contains(&clock, x, y);
                redraw |= h != self.clock_hover;
                self.clock_hover = h;
            }
            Ui::PointerLeave => {
                redraw |= self.clock_hover;
                self.clock_hover = false;
            }
            Ui::PointerDown { x, y, button: b, .. } if b == button::LEFT && contains(&clock, x, y) => {
                action = Some(PanelAction::Clock)
            }
            _ => {}
        }
        self.panel.dirty |= redraw;
        match action {
            Some(PanelAction::Launcher) => self.toggle_launcher(),
            Some(PanelAction::Power) => {
                if self.recently_closed(PopupTag::PowerMenu) {
                    return;
                }
                let items = vec![
                    MenuItem::new("Restart", Some(Icon::Restart)),
                    MenuItem::new("Shut down", Some(Icon::Power)).danger(),
                ];
                let (mw, mh) = Menu::new(items.clone()).size(&self.fonts);
                let r = self.power_btn.rect;
                let x = r.right() as i32 - mw as i32;
                let y = (self.h - PANEL_H) as i32 - mh as i32 - 8;
                self.open_menu(items, MenuPurpose::Power, x, y);
            }
            Some(PanelAction::Task(i)) => {
                if let Some(info) = self.windows.get(i) {
                    let active = info.flags & state::ACTIVATED != 0 && info.flags & state::MINIMIZED == 0;
                    let a = if active { Action::Minimize } else { Action::Activate };
                    println!("desktop: task bar: {:?} window {} \"{}\"", a, info.id, info.title);
                    let _ = self.conn.window_action(info.id, a, true);
                }
            }
            Some(PanelAction::Clock) => {
                let l = self.tz.local(unix_now());
                let off = l.offset.abs();
                let sign = if l.offset < 0 { '-' } else { '+' };
                let body = format!("{} {} (UTC{}{:02}:{:02})", l.iso(), l.zone, sign, off / 3600, off / 60 % 60);
                let title = l.long_date();
                self.show_note(&title, &body);
            }
            None => {}
        }
    }

    fn popup_input(&mut self, ui: Ui) {
        let fonts = self.fonts;
        let Some(p) = &mut self.popup else { return };
        let mut launch = None;
        let mut pick = None;
        let mut confirm = None;
        let mut close = false;
        match &mut p.kind {
            PopupKind::Launcher { search, list, shown, list_focus } => {
                // Arrow keys and Enter in the search field drive the list.
                if let Ui::Key(k) = ui {
                    if k.down && matches!(k.key, key::UP | key::DOWN | key::PAGE_UP | key::PAGE_DOWN) {
                        if list.selected().is_none() && !shown.is_empty() {
                            list.select(Some(0));
                        } else {
                            list.event(&ui, true);
                        }
                        p.win.dirty = true;
                        return;
                    }
                    if k.down && k.key == key::TAB {
                        *list_focus = !*list_focus;
                        p.win.dirty = true;
                        return;
                    }
                }
                let before = String::from(search.text());
                match search.event(&ui, !*list_focus, &fonts) {
                    Response::Submitted => {
                        launch = list
                            .selected()
                            .or(if shown.is_empty() { None } else { Some(0) })
                            .and_then(|i| shown.get(i).copied());
                    }
                    Response::Cancelled => close = true,
                    r if r.needs_redraw() => p.win.dirty = true,
                    _ => {}
                }
                if search.text() != before {
                    *shown = filter_apps(&self.apps, search.text());
                    list.set_items(app_items(&self.apps, shown));
                    list.select(if shown.is_empty() { None } else { Some(0) });
                }
                match list.event(&ui, *list_focus) {
                    Response::Activated(i) => launch = shown.get(i).copied(),
                    r if r.needs_redraw() => p.win.dirty = true,
                    _ => {}
                }
                if let Ui::PointerDown { x, y, .. } = ui {
                    if contains(&list.rect, x, y) {
                        *list_focus = true;
                    } else if contains(&search.rect, x, y) {
                        *list_focus = false;
                    }
                }
                if let Ui::Key(k) = ui {
                    if k.down && k.key == key::ESCAPE {
                        close = true;
                    }
                }
            }
            PopupKind::Menu { menu, purpose } => match menu.event(&ui) {
                Response::Activated(i) => pick = Some((*purpose, i)),
                Response::Cancelled => close = true,
                r if r.needs_redraw() => p.win.dirty = true,
                _ => {}
            },
            PopupKind::Confirm { dialog } => match dialog.event(&ui, &fonts) {
                Response::Activated(1) => confirm = Some(dialog.tag),
                Response::Activated(_) | Response::Cancelled => close = true,
                r if r.needs_redraw() => p.win.dirty = true,
                _ => {}
            },
        }
        if close || launch.is_some() || pick.is_some() || confirm.is_some() {
            self.close_popup();
        }
        if let Some(i) = launch {
            let app = self.apps[i].clone();
            self.launch(&app.name, &app.exec);
        }
        if let Some((purpose, i)) = pick {
            self.menu_pick(purpose, i);
        }
        if let Some(tag) = confirm {
            self.power(tag);
        }
    }

    fn menu_pick(&mut self, purpose: MenuPurpose, i: usize) {
        match (purpose, i) {
            (MenuPurpose::Desktop, 0) => self.launch("Terminal", "/system/bin/terminal"),
            (MenuPurpose::Desktop, 1) => self.launch("Files", "/system/bin/files"),
            (MenuPurpose::Desktop, 2) => self.launch("System Information", "/system/bin/sysinfo"),
            (MenuPurpose::Desktop, 3) => {
                self.style = (self.style + 1) % WALLPAPERS;
                if self.style == 0 && self.image.is_none() {
                    self.style = 1;
                }
                paint_wallpaper(&mut self.wallpaper, self.w, self.h, self.style, self.image.as_deref(), &self.fonts);
                self.desktop.dirty = true;
                println!("desktop: wallpaper {}", self.style);
                let _ = os_std::fs::write("/etc/osyx/wallpaper", format!("{}\n", self.style));
            }
            (MenuPurpose::Desktop, 4) => {
                let info = os_std::os::sysinfo().ok();
                let version = info.as_ref().map_or("?", os_std::os::kernel_version);
                let body = format!("Kernel {version}, PRISM compositor, AXIS shell. Many surfaces. One vision.");
                self.show_note("OSyx", &body);
            }
            (MenuPurpose::Power, 0) => self.confirm(
                CONFIRM_REBOOT,
                "Restart OSyx?",
                "All applications will be closed and the computer restarts.",
                "Restart",
            ),
            (MenuPurpose::Power, 1) => self.confirm(
                CONFIRM_OFF,
                "Shut down OSyx?",
                "All applications will be closed, file systems unmounted and the computer switched off.",
                "Shut down",
            ),
            _ => {}
        }
    }

    fn power(&mut self, tag: u32) {
        println!("desktop: {} requested", if tag == CONFIRM_OFF { "power off" } else { "restart" });
        let e = if tag == CONFIRM_OFF { system::power_off() } else { system::reboot() };
        // Only reached if the kernel refused.
        eprintln!("desktop: power request failed: {e}");
        self.show_note("Power", &format!("The system refused: {e}"));
    }

    fn launch(&mut self, name: &str, exec: &str) {
        let mut command = Command::new(if exec.ends_with(".exe") { "/bin/winrun" } else { exec });
        if exec.ends_with(".exe") {
            command.arg(exec);
        }
        match command.spawn() {
            Ok(child) => {
                println!("desktop: launched {} ({}) as process {}", name, exec, child.id());
                self.children.push((child.id(), String::from(name)));
                // The reaper thread collects it (dropping a Child does not wait).
            }
            Err(e) => {
                eprintln!("desktop: cannot start {exec}: {e}");
                self.show_note(&format!("Cannot start {name}"), &format!("{exec}: {e}"));
            }
        }
    }

    fn collect_exits(&mut self) {
        let exits: Vec<(u64, i32)> = match EXITS.lock() {
            Ok(mut q) => core::mem::take(&mut *q),
            Err(_) => return,
        };
        for (pid, raw) in exits {
            let name = self.children.iter().position(|(p, _)| *p == pid).map(|i| self.children.remove(i).1);
            let name = name.unwrap_or_else(|| format!("process {pid}"));
            let st = os_std::process::ExitStatus::from_raw(raw);
            if st.success() {
                println!("desktop: {name} (process {pid}) exited");
            } else if let Some(v) = st.exception() {
                println!("desktop: {name} (process {pid}) crashed with CPU exception {v}");
                self.show_note(&format!("{name} crashed"), &format!("Process {pid} was stopped by CPU exception {v}. Its windows are gone; the desktop keeps running."));
            } else {
                println!("desktop: {name} (process {pid}) exited with status {raw}");
                if raw == 101 {
                    self.show_note(&format!("{name} stopped"), &format!("Process {pid} panicked (exit status 101)."));
                }
            }
        }
    }

    // ---- popups ------------------------------------------------------------

    fn recently_closed(&self, tag: PopupTag) -> bool {
        self.popup_closed.is_some_and(|(t, g)| g == tag && self.now().saturating_sub(t) < 400)
    }

    fn close_popup(&mut self) {
        if let Some(p) = self.popup.take() {
            let _ = p.win.destroy(&mut self.conn);
        }
        self.launcher_btn.active = false;
        self.panel.dirty = true;
    }

    fn open_popup(&mut self, x: i32, y: i32, w: u32, h: u32, kind: PopupKind) {
        self.close_popup();
        let mut spec = WindowSpec::with_role(Role::Popup, x, y, w, h);
        spec.title = String::from("popup");
        match self.conn.create_window(spec) {
            Ok(win) => self.popup = Some(Popup { win, kind }),
            Err(e) => eprintln!("desktop: cannot open a popup: {e}"),
        }
    }

    fn open_menu(&mut self, items: Vec<MenuItem>, purpose: MenuPurpose, x: i32, y: i32) {
        let mut menu = Menu::new(items);
        let (w, h) = menu.size(&self.fonts);
        menu.rect = Rect::new(0, 0, w, h);
        println!("desktop: {:?} menu opened at ({}, {})", purpose, x, y);
        self.open_popup(x, y, w, h, PopupKind::Menu { menu, purpose });
    }

    fn toggle_launcher(&mut self) {
        if self.popup.as_ref().is_some_and(|p| p.tag() == PopupTag::Launcher) {
            self.close_popup();
            return;
        }
        if self.recently_closed(PopupTag::Launcher) {
            return;
        }
        let mut search = TextField::new("");
        search.placeholder = String::from("Search applications");
        search.rect = Rect::new(16, 60, LAUNCHER_W - 32, 36);
        let mut list = ListView::new(vec![Column::new("Application", 0, Align::Left)]);
        list.row_height = 44;
        list.rect = Rect::new(8, 108, LAUNCHER_W - 16, LAUNCHER_H - 108 - 48);
        let shown = filter_apps(&self.apps, "");
        list.set_items(app_items(&self.apps, &shown));
        list.select(if shown.is_empty() { None } else { Some(0) });
        let y = (self.h - PANEL_H) as i32 - LAUNCHER_H as i32 - 8;
        println!("desktop: launcher opened ({} applications)", shown.len());
        self.open_popup(8, y, LAUNCHER_W, LAUNCHER_H, PopupKind::Launcher { search, list, shown, list_focus: false });
        self.launcher_btn.active = true;
        self.panel.dirty = true;
    }

    fn confirm(&mut self, tag: u32, title: &str, message: &str, action: &str) {
        let mut dialog =
            Dialog::new(tag, title, message, &[("Cancel", ButtonStyle::Secondary), (action, ButtonStyle::Danger)]);
        let w = 440;
        let h = dialog.height_for(w, &self.fonts);
        dialog.layout_card(Rect::new(0, 0, w, h), &self.fonts);
        let x = (self.w as i32 - w as i32) / 2;
        let y = (self.h as i32 - h as i32) / 2;
        self.open_popup(x, y, w, h, PopupKind::Confirm { dialog });
    }

    // ---- notifications -----------------------------------------------------

    fn show_note(&mut self, title: &str, body: &str) {
        if self.notes.len() >= MAX_NOTES {
            let old = self.notes.remove(0);
            let _ = old.win.destroy(&mut self.conn);
        }
        let i = self.notes.len() as i32;
        let x = (self.w - NOTE_W - 16) as i32;
        let y = 16 + i * (NOTE_H as i32 + 10);
        match self.conn.create_window(WindowSpec::with_role(Role::Notification, x, y, NOTE_W, NOTE_H)) {
            Ok(win) => {
                let born = self.now();
                self.notes.push(Note { win, title: String::from(title), body: String::from(body), born });
            }
            Err(e) => eprintln!("desktop: cannot show a notification: {e}"),
        }
    }

    fn expire_notes(&mut self) {
        let now = self.now();
        let before = self.notes.len();
        let mut i = 0;
        while i < self.notes.len() {
            if now.saturating_sub(self.notes[i].born) >= NOTE_MS {
                let n = self.notes.remove(i);
                let _ = n.win.destroy(&mut self.conn);
            } else {
                i += 1;
            }
        }
        if self.notes.len() != before {
            self.restack_notes();
        }
    }

    fn restack_notes(&mut self) {
        let x = (self.w - NOTE_W - 16) as i32;
        for (i, n) in self.notes.iter_mut().enumerate() {
            let y = 16 + i as i32 * (NOTE_H as i32 + 10);
            let _ = n.win.set_geometry(&mut self.conn, x, y, NOTE_W, NOTE_H);
        }
    }

    // ---- panel ---------------------------------------------------------------

    fn clock_rect(&self) -> Rect {
        let r = self.power_btn.rect;
        Rect::new(r.x - 12 - CLOCK_W as i32, 6, CLOCK_W, PANEL_H - 12)
    }

    fn update_clock(&mut self) {
        self.tz = system_tz();
        if let Some(style) = os_std::fs::read_to_string("/etc/osyx/wallpaper")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|&s| s < WALLPAPERS)
        {
            if style != self.style && (style != 0 || self.image.is_some()) {
                self.style = style;
                paint_wallpaper(&mut self.wallpaper, self.w, self.h, style, self.image.as_deref(), &self.fonts);
                self.desktop.dirty = true;
                println!("desktop: wallpaper {style}");
            }
        }
        let l = self.tz.local(unix_now());
        let c = (l.hm(), l.short_date());
        if c != self.clock {
            self.clock = c;
            self.panel.dirty = true;
        }
    }

    fn layout_panel(&mut self) {
        let h = PANEL_H;
        self.launcher_btn.rect = Rect::new(8, 8, 44, h as i32 as u32 - 16);
        self.power_btn.rect = Rect::new(self.w as i32 - 8 - 40, 8, 40, h - 16);
        let left = 64;
        let right = self.clock_rect().x - 12;
        let n = self.windows.len().max(1) as i32;
        let avail = (right - left).max(0);
        let each = (avail / n).clamp(40, 220);
        self.tasks = self
            .windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let mut b = Button::new(i, &w.title, ButtonStyle::Flat).with_icon(task_icon(&w.title));
                b.rect = Rect::new(left + i as i32 * (each + 4), 8, (each - 4).max(36) as u32, h - 16);
                b.active = w.flags & state::ACTIVATED != 0 && w.flags & state::MINIMIZED == 0;
                b
            })
            .collect();
        self.panel.dirty = true;
    }

    // ---- drawing ---------------------------------------------------------------

    fn redraw(&mut self) -> os_std::error::Result<()> {
        let (theme, fonts) = (self.theme, self.fonts);
        if self.desktop.dirty && self.desktop.can_draw() {
            let (w, h) = (self.desktop.width, self.desktop.height);
            if (w, h) == (self.w, self.h) {
                let mut s = self.desktop.surface()?;
                for y in 0..h {
                    let row = &self.wallpaper[(y * w) as usize..][..w as usize];
                    s.row_mut(y).copy_from_slice(row);
                }
            }
            self.desktop.present(&mut self.conn, None)?;
        }
        if self.panel.dirty && self.panel.can_draw() {
            let clock = self.clock_rect();
            let mut s = self.panel.surface()?;
            let mut p = Painter::new(&mut s, &theme, &fonts);
            draw_panel(
                &mut p,
                &self.launcher_btn,
                &self.power_btn,
                &self.tasks,
                &self.clock,
                clock,
                self.clock_hover,
                self.windows.is_empty(),
            );
            self.panel.present(&mut self.conn, None)?;
        }
        if let Some(pop) = &mut self.popup {
            if pop.win.dirty && pop.win.can_draw() {
                let mut s = pop.win.surface()?;
                let mut p = Painter::new(&mut s, &theme, &fonts);
                draw_popup(&mut p, &pop.kind, &self.apps);
                pop.win.present(&mut self.conn, None)?;
            }
        }
        for n in &mut self.notes {
            if n.win.dirty && n.win.can_draw() {
                let mut s = n.win.surface()?;
                let mut p = Painter::new(&mut s, &theme, &fonts);
                draw_note(&mut p, &n.title, &n.body);
                n.win.present(&mut self.conn, None)?;
            }
        }
        Ok(())
    }
}

fn task_icon(title: &str) -> Icon {
    if title.contains("Terminal") {
        Icon::Terminal
    } else if title.contains("Files") {
        Icon::Folder
    } else if title.contains("System") {
        Icon::Info
    } else {
        Icon::Program
    }
}

fn filter_apps(apps: &[AppEntry], q: &str) -> Vec<usize> {
    let q = q.to_lowercase();
    (0..apps.len())
        .filter(|&i| {
            q.is_empty() || apps[i].name.to_lowercase().contains(&q) || apps[i].comment.to_lowercase().contains(&q)
        })
        .collect()
}

fn app_items(apps: &[AppEntry], shown: &[usize]) -> Vec<ListItem> {
    shown.iter().map(|&i| ListItem::new(Some(apps[i].icon), &[&apps[i].name])).collect()
}

const PANEL_BG: Color = rgb(11, 14, 21);
const PANEL_LINE: Color = rgb(34, 41, 56);

#[allow(clippy::too_many_arguments)]
fn draw_panel(
    p: &mut Painter,
    launcher: &Button,
    power: &Button,
    tasks: &[Button],
    clock: &(String, String),
    clock_rect: Rect,
    clock_hover: bool,
    empty: bool,
) {
    let t = *p.theme;
    let full = p.s.bounds();
    p.fill(full, PANEL_BG);
    p.hline(0, 0, full.w, PANEL_LINE);
    launcher.draw(p, false);
    for b in tasks {
        b.draw(p, false);
    }
    if empty {
        let r = Rect::new(72, 0, 460, full.h);
        p.text_in(
            FontKind::Ui,
            r,
            "Press Super or click the launcher to start an application",
            t.text_faint,
            Align::Left,
        );
    }
    if clock_hover {
        p.rounded(clock_rect, t.radius_small, t.raised, 255);
    }
    let lh = p.fonts.line_height(FontKind::Bold);
    let top = Rect::new(clock_rect.x, clock_rect.y + 2, clock_rect.w - 10, lh);
    p.text_in(FontKind::Bold, top, &clock.0, t.text, Align::Right);
    let bottom = Rect::new(clock_rect.x, clock_rect.y + 2 + lh as i32, clock_rect.w - 10, lh);
    p.text_in(FontKind::Ui, bottom, &clock.1, t.text_dim, Align::Right);
    p.vline(clock_rect.right() as i32 + 5, 12, full.h - 24, PANEL_LINE);
    power.draw(p, false);
}

fn draw_popup(p: &mut Painter, kind: &PopupKind, apps: &[AppEntry]) {
    let t = *p.theme;
    let full = p.s.bounds();
    p.fill(full, t.surface);
    match kind {
        PopupKind::Launcher { search, list, shown, list_focus } => {
            p.text(FontKind::Large, 20, 18, "Applications", t.text);
            search.draw(p, !*list_focus);
            if shown.is_empty() {
                let r = list.rect;
                p.text_in(
                    FontKind::Ui,
                    r,
                    if apps.is_empty() { "No applications installed" } else { "Nothing matches" },
                    t.text_faint,
                    Align::Center,
                );
            } else {
                list.draw(p, true);
            }
            let footer = Rect::new(20, (LAUNCHER_H - 44) as i32, LAUNCHER_W - 40, 40);
            p.hline(12, footer.y, LAUNCHER_W - 24, t.border);
            let comment = list.selected().and_then(|i| shown.get(i)).map_or("", |&i| apps[i].comment.as_str());
            p.text_in(FontKind::Ui, footer, comment, t.text_dim, Align::Left);
        }
        PopupKind::Menu { menu, .. } => menu.draw(p),
        PopupKind::Confirm { dialog } => dialog.draw(p),
    }
    // A one pixel frame (popups are square; PRISM adds the shadow).
    p.fill(Rect::new(0, 0, full.w, 1), t.border);
    p.fill(Rect::new(0, full.h as i32 - 1, full.w, 1), t.border);
    p.fill(Rect::new(0, 0, 1, full.h), t.border);
    p.fill(Rect::new(full.w as i32 - 1, 0, 1, full.h), t.border);
}

fn draw_note(p: &mut Painter, title: &str, body: &str) {
    let t = *p.theme;
    let full = p.s.bounds();
    p.fill(full, t.surface);
    p.fill(Rect::new(0, 0, 3, full.h), t.accent);
    icon::draw(p, Icon::Bell, Rect::new(16, 16, 20, 20), t.accent);
    let lh = p.fonts.line_height(FontKind::Bold);
    p.text_in(FontKind::Bold, Rect::new(48, 12, full.w - 60, lh), title, t.text, Align::Left);
    let lines = osyx_ui::widget::wrap_text(p.fonts, FontKind::Ui, body, full.w - 60);
    let ul = p.fonts.line_height(FontKind::Ui);
    for (i, l) in lines.iter().take(2).enumerate() {
        p.text(FontKind::Ui, 48, 14 + lh as i32 + 4 + i as i32 * ul as i32, l, t.text_dim);
    }
    let r = inset(full, 0);
    p.fill(Rect::new(r.x, r.bottom() as i32 - 1, r.w, 1), t.border);
}

/// Draws one of the desktop backgrounds. Style 0 is the OSyx wallpaper
/// image, scaled to cover the screen; styles 1..=3 are drawn: a deep
/// midnight gradient lit by soft glows and a prism refracting a beam of
/// light into restrained blues and violets.
fn paint_wallpaper(buf: &mut [u32], w: u32, h: u32, style: usize, image: Option<&[u8]>, fonts: &Fonts) {
    let theme = Theme::default();
    let Ok(mut s) = Surface::new(buf, w, h, w) else { return };
    if style == 0 {
        if let Some(img) = image.and_then(|d| osyx_gfx::Image::parse(d).ok()) {
            // Cover: fill the screen keeping the aspect ratio, centred.
            let (iw, ih) = (img.width() as u64, img.height() as u64);
            let (dw, dh) = if w as u64 * ih > h as u64 * iw {
                (w as u64, ih * w as u64 / iw)
            } else {
                (iw * h as u64 / ih, h as u64)
            };
            let dst = Rect::new((w as i32 - dw as i32) / 2, (h as i32 - dh as i32) / 2, dw as u32, dh as u32);
            s.fill(rgb(4, 8, 20));
            img.draw_scaled(&mut s, dst, 255);
            return;
        }
    }
    let (top, bottom, glow_a, glow_b) = match style {
        2 => (rgb(22, 16, 34), rgb(8, 7, 14), rgb(92, 52, 150), rgb(30, 70, 140)),
        3 => (rgb(9, 16, 22), rgb(4, 7, 10), rgb(20, 90, 110), rgb(26, 50, 110)),
        _ => (rgb(14, 19, 33), rgb(6, 8, 14), rgb(34, 64, 150), rgb(20, 88, 122)),
    };
    s.gradient_v(Rect::new(0, 0, w, h), top, bottom);
    let (fw, fh) = (w as i32, h as i32);
    s.radial_glow(fw * 18 / 100, fh * 12 / 100, w * 55 / 100, glow_a, 80);
    s.radial_glow(fw * 88 / 100, fh * 92 / 100, w * 45 / 100, glow_b, 55);
    let mut p = Painter::new(&mut s, &theme, fonts);
    if style != 3 {
        // The prism: an equilateral-ish triangle right of centre.
        let cx = fw * 60 / 100;
        let cy = fh * 46 / 100;
        let size = fh * 30 / 100;
        let apex = (cx, cy - size / 2);
        let left = (cx - size * 58 / 100, cy + size / 2);
        let right = (cx + size * 58 / 100, cy + size / 2);
        let fx = |(x, y): (i32, i32)| (x << 8, y << 8);
        // Incoming white beam hitting the left face.
        let hit = ((apex.0 + left.0) / 2, (apex.1 + left.1) / 2);
        let beam_from = (0, cy + size / 6);
        p.segment(fx(beam_from), fx(hit), 8 << 8, rgb(190, 205, 235), 18);
        p.segment(fx(beam_from), fx(hit), 2 << 8, rgb(220, 228, 245), 90);
        // Refracted fan leaving the right face.
        let exit = ((apex.0 + right.0) / 2 + 4, (apex.1 + right.1) / 2 + 6);
        let colors = [rgb(120, 140, 255), rgb(96, 170, 245), rgb(88, 200, 210), rgb(150, 130, 240), rgb(196, 128, 220)];
        for (i, c) in colors.iter().enumerate() {
            let spread = (i as i32 - 2) * fh * 7 / 100;
            let to = (fw + 40, cy + size / 3 + spread + fh * 8 / 100);
            p.segment(fx(exit), fx(to), 14 << 8, *c, 14);
            p.segment(fx(exit), fx(to), 3 << 8, *c, if style == 2 { 70 } else { 55 });
        }
        // Glass body: faint fill by horizontal spans, bright edges.
        for y in apex.1..left.1 {
            let t = (y - apex.1) * 256 / (left.1 - apex.1).max(1);
            let x0 = apex.0 + (left.0 - apex.0) * t / 256;
            let x1 = apex.0 + (right.0 - apex.0) * t / 256;
            p.blend(Rect::new(x0, y, (x1 - x0).max(0) as u32, 1), rgb(120, 150, 220), 14 + (t as u32 * 10 / 256));
        }
        for (a, b) in [(apex, left), (left, right), (right, apex)] {
            p.segment(fx(a), fx(b), 6 << 8, rgb(150, 178, 240), 22);
            p.segment(fx(a), fx(b), 3 << 7, rgb(200, 215, 248), 150);
        }
    }
    let lh = fonts.line_height(FontKind::Ui);
    let label = "OSyx  ·  PRISM";
    let tw = fonts.measure(FontKind::Ui, label);
    p.text_alpha(FontKind::Ui, fw - tw as i32 - 24, fh - PANEL_H as i32 - lh as i32 - 16, label, theme.text_dim, 110);
}
