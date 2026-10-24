//! System file picker, rendered with the native OSyx widgets on the same
//! PRISM connection as its owner. No extra process or taskbar button.
use super::*;
use alloc::{format, vec};
use os_std::fs;
use osyx_input::{button, key};
use osyx_ui::layout::contains;
use osyx_ui::widget::{Button, ButtonStyle, Column, Dialog, ListItem, ListView, TextField};
use osyx_ui::{Align, Event as Ui, FontKind as UiFont, Fonts, Icon as UiIcon, Painter, Response, Theme};

pub(crate) struct FileOptions {
    pub owner: u64,
    pub title: String,
    pub directory: String,
    pub filename: String,
    pub save: bool,
    pub folder: bool,
    pub must_exist: bool,
    pub overwrite: bool,
    pub extension: String,
    pub filters: Vec<(String, String)>,
    pub filter: usize,
}

pub(crate) struct FileSelection {
    pub path: String,
    pub filter: usize,
}

struct Entry {
    name: String,
    directory: bool,
    size: u64,
}

struct Picker {
    options: FileOptions,
    cwd: String,
    entries: Vec<Entry>,
    list: ListView,
    address: TextField,
    name: TextField,
    buttons: Vec<Button>,
    places: Vec<Button>,
    focus: usize,
    hidden: bool,
    status: String,
    confirm: Option<Dialog>,
    pending: String,
    result: Option<Option<FileSelection>>,
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

impl Picker {
    fn new(mut options: FileOptions) -> Self {
        if options.filters.is_empty() {
            options.filters.push((String::from("All files"), String::from("*")));
        }
        options.filter = options.filter.min(options.filters.len() - 1);
        let mut name = TextField::new(&options.filename);
        name.max_len = 4096;
        name.select_all();
        let accept = if options.folder {
            "Select folder"
        } else if options.save {
            "Save"
        } else {
            "Open"
        };
        let directory = options.directory.clone();
        let mut picker = Self {
            options,
            cwd: String::new(),
            entries: Vec::new(),
            list: ListView::new(vec![Column::new("Name", 0, Align::Left), Column::new("Size", 100, Align::Right)]),
            address: TextField::new(""),
            name,
            buttons: vec![
                Button::new(0, "", ButtonStyle::Secondary).with_icon(UiIcon::Up),
                Button::new(1, "New folder", ButtonStyle::Secondary).with_icon(UiIcon::NewFolder),
                Button::new(2, "Hidden", ButtonStyle::Flat),
                Button::new(3, "All files", ButtonStyle::Secondary),
                Button::new(4, "Cancel", ButtonStyle::Secondary),
                Button::new(5, accept, ButtonStyle::Primary),
            ],
            places: vec![
                Button::new(10, "Home", ButtonStyle::Flat).with_icon(UiIcon::Home),
                Button::new(11, "Documents", ButtonStyle::Flat).with_icon(UiIcon::Folder),
                Button::new(12, "Desktop", ButtonStyle::Flat).with_icon(UiIcon::Apps),
                Button::new(13, "System (C:)", ButtonStyle::Flat).with_icon(UiIcon::Drive),
            ],
            focus: 2,
            hidden: false,
            status: String::new(),
            confirm: None,
            pending: String::new(),
            result: None,
        };
        if !picker.navigate(&directory) {
            picker.navigate("/home/user");
        }
        picker
    }

    fn navigate(&mut self, directory: &str) -> bool {
        let directory = if directory.is_empty() { "/" } else { directory };
        let dir = match fs::read_dir(directory) {
            Ok(dir) => dir,
            Err(e) => {
                self.status = format!("Cannot open folder: {e}");
                return false;
            }
        };
        let pattern = &self.options.filters[self.options.filter].1;
        self.entries = dir
            .filter_map(|entry| {
                let name = String::from(entry.file_name());
                if !self.hidden && name.starts_with('.') {
                    return None;
                }
                let meta = entry.metadata().ok()?;
                if !meta.is_dir() && (self.options.folder || !matches_filter(&name, pattern)) {
                    return None;
                }
                Some(Entry { name, directory: meta.is_dir(), size: meta.len() })
            })
            .collect();
        self.entries.sort_by(|a, b| {
            b.directory.cmp(&a.directory).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.list.set_items(
            self.entries
                .iter()
                .map(|entry| {
                    let size = if entry.directory {
                        String::from("Folder")
                    } else if entry.size >= 1048576 {
                        format!("{} MiB", entry.size / 1048576)
                    } else {
                        format!("{} B", entry.size)
                    };
                    ListItem::new(
                        Some(if entry.directory { UiIcon::Folder } else { UiIcon::TextFile }),
                        &[&entry.name, &size],
                    )
                })
                .collect(),
        );
        self.list.select(None);
        self.list.scroll.offset = 0;
        self.cwd = String::from(directory);
        self.address.set_text(&path::to_windows(directory));
        self.status = format!("{} items", self.entries.len());
        self.buttons[3].label = self.options.filters[self.options.filter].0.clone();
        true
    }

    fn layout(&mut self, w: u32, h: u32, fonts: &Fonts) {
        self.buttons[0].rect = Rect::new(16, 64, 36, 36);
        self.address.rect = Rect::new(60, 64, w.saturating_sub(76), 36);
        for (i, place) in self.places.iter_mut().enumerate() {
            place.rect = Rect::new(12, 122 + i as i32 * 44, 142, 38);
        }
        self.buttons[1].rect = Rect::new(172, 112, 136, 32);
        self.buttons[2].rect = Rect::new(w as i32 - 104, 112, 88, 32);
        self.list.rect = Rect::new(166, 152, w.saturating_sub(182), h.saturating_sub(312));
        self.name.rect = Rect::new(166, h as i32 - 148, w.saturating_sub(182), 36);
        self.buttons[3].rect = Rect::new(166, h as i32 - 104, w.saturating_sub(182), 32);
        self.buttons[4].rect = Rect::new(w as i32 - 244, h as i32 - 48, 100, 34);
        self.buttons[5].rect = Rect::new(w as i32 - 134, h as i32 - 48, 118, 34);
        if let Some(dialog) = &mut self.confirm {
            dialog.layout(Rect::new(0, 0, w, h), fonts);
        }
    }

    fn activate_row(&mut self, i: usize) {
        let Some(entry) = self.entries.get(i) else { return };
        let target = join(&self.cwd, &entry.name);
        if entry.directory {
            self.navigate(&target);
        } else {
            self.name.set_text(&entry.name);
            self.accept();
        }
    }

    fn accept(&mut self) {
        let text = self.name.text().trim();
        let target = if self.options.folder && text.is_empty() {
            self.cwd.clone()
        } else {
            if text.is_empty() {
                self.status = String::from("Enter a file name or select a file.");
                self.focus = 2;
                return;
            }
            let win = if text.starts_with('/') { format!("C:{}", text.replace('/', "\\")) } else { String::from(text) };
            match path::to_osyx(&win, &path::to_windows(&self.cwd)).and_then(|p| path::resolve(&p).map(|(p, _)| p)) {
                Ok(p) => p,
                Err(_) => {
                    self.status = String::from("The folder or file name is invalid.");
                    return;
                }
            }
        };
        let mut target = target;
        if fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
            if self.options.folder {
                self.finish(target);
            } else {
                self.navigate(&target);
                self.name.set_text("");
            }
            return;
        }
        if self.options.folder {
            self.status = String::from("Choose an existing folder.");
            return;
        }
        let filename = target.rsplit('/').next().unwrap_or("");
        if !filename.contains('.') && !self.options.extension.is_empty() {
            target.push('.');
            target.push_str(self.options.extension.trim_start_matches('.'));
        }
        let parent = target.rsplit_once('/').map_or("/", |(dir, _)| if dir.is_empty() { "/" } else { dir });
        if !fs::metadata(parent).is_ok_and(|m| m.is_dir()) {
            self.status = String::from("The destination folder does not exist.");
            return;
        }
        let exists = fs::metadata(&target).is_ok();
        if self.options.must_exist && !exists {
            self.status = String::from("This file does not exist. Choose an existing file.");
            return;
        }
        if self.options.save && self.options.overwrite && exists {
            self.pending = target;
            self.confirm = Some(Dialog::new(
                1,
                "Replace existing file?",
                "A file with this name already exists. Replace its contents?",
                &[("Cancel", ButtonStyle::Secondary), ("Replace", ButtonStyle::Danger)],
            ));
        } else {
            self.finish(target);
        }
    }

    fn finish(&mut self, path: String) {
        os_std::println!("file-dialog: selected {}", path);
        self.result = Some(Some(FileSelection { path, filter: self.options.filter }));
    }

    fn event(&mut self, ui: Ui, fonts: &Fonts) {
        if let Some(dialog) = &mut self.confirm {
            match dialog.event(&ui, fonts) {
                Response::Activated(1) => {
                    let tag = dialog.tag;
                    let text = String::from(dialog.input_text());
                    self.confirm = None;
                    if tag == 1 {
                        self.finish(self.pending.clone());
                    } else if !text.trim().is_empty() && !text.contains(['/', '\\']) && text != "." && text != ".." {
                        match fs::create_dir(join(&self.cwd, text.trim())) {
                            Ok(()) => {
                                self.navigate(&self.cwd.clone());
                            }
                            Err(e) => self.status = format!("Cannot create folder: {e}"),
                        }
                    }
                }
                Response::Activated(_) | Response::Cancelled => self.confirm = None,
                _ => {}
            }
            return;
        }
        if let Ui::Key(k) = ui {
            if k.pressed(key::ESCAPE) {
                self.result = Some(None);
                return;
            }
            if k.pressed(key::TAB) {
                self.focus = if k.shift() { (self.focus + 4) % 5 } else { (self.focus + 1) % 5 };
                return;
            }
            if k.ctrl() && k.pressed(key::L) {
                self.focus = 0;
                self.address.select_all();
                return;
            }
            if k.alt() && k.pressed(key::UP) {
                self.up();
                return;
            }
            if k.pressed(key::ENTER) || k.pressed(key::KP_ENTER) {
                match self.focus {
                    0 => {
                        let address = self.address.text().replace('/', "\\");
                        if let Ok(p) = path::to_osyx(&address, &path::to_windows(&self.cwd))
                            .and_then(|p| path::resolve(&p).map(|(p, _)| p))
                        {
                            self.navigate(&p);
                        }
                    }
                    1 => {
                        if let Some(i) = self.list.selected() {
                            self.activate_row(i);
                        }
                    }
                    3 => self.result = Some(None),
                    _ => self.accept(),
                }
                return;
            }
        }
        if let Ui::PointerDown { x, y, button: button::LEFT, .. } = ui {
            if contains(&self.address.rect, x, y) {
                self.focus = 0;
            } else if contains(&self.list.rect, x, y) {
                self.focus = 1;
            } else if contains(&self.name.rect, x, y) {
                self.focus = 2;
            }
        }
        self.address.event(&ui, self.focus == 0, fonts);
        self.name.event(&ui, self.focus == 2, fonts);
        let previous = self.list.selected();
        let response = self.list.event(&ui, self.focus == 1);
        if previous != self.list.selected() {
            if let Some(entry) = self.list.selected().and_then(|i| self.entries.get(i)) {
                if !entry.directory || self.options.folder {
                    self.name.set_text(&entry.name);
                }
            }
        }
        if let Response::Activated(i) = response {
            self.activate_row(i);
        }
        let action = self.buttons.iter_mut().find_map(|button| {
            match button.event(&ui, (button.id == 4 && self.focus == 3) || (button.id == 5 && self.focus == 4)) {
                Response::Activated(i) => Some(i),
                _ => None,
            }
        });
        match action {
            Some(0) => self.up(),
            Some(1) => {
                self.confirm = Some(
                    Dialog::new(
                        2,
                        "New folder",
                        "Create a folder in the current location.",
                        &[("Cancel", ButtonStyle::Secondary), ("Create", ButtonStyle::Primary)],
                    )
                    .with_input("", "Folder name"),
                )
            }
            Some(2) => {
                self.hidden = !self.hidden;
                self.buttons[2].active = self.hidden;
                self.navigate(&self.cwd.clone());
            }
            Some(3) => {
                self.options.filter = (self.options.filter + 1) % self.options.filters.len();
                self.navigate(&self.cwd.clone());
            }
            Some(4) => self.result = Some(None),
            Some(5) => self.accept(),
            _ => {}
        }
        let place = self.places.iter_mut().find_map(|button| match button.event(&ui, false) {
            Response::Activated(i) => Some(i),
            _ => None,
        });
        if let Some(place) = place {
            self.navigate(match place {
                10 => "/home/user",
                11 => "/home/user/Documents",
                12 => "/home/user/Desktop",
                _ => "/",
            });
            if self.options.folder {
                self.name.set_text("");
            }
        }
    }

    fn up(&mut self) {
        let parent = self.cwd.rsplit_once('/').map_or("/", |(p, _)| if p.is_empty() { "/" } else { p }).to_string();
        self.navigate(&parent);
        self.name.set_text("");
    }

    fn draw(&self, p: &mut Painter, w: u32, h: u32) {
        let theme = *p.theme;
        p.fill(Rect::new(0, 0, w, h), theme.surface);
        p.text(UiFont::Large, 20, 20, &self.options.title, theme.text);
        p.fill(Rect::new(0, 112, 158, h.saturating_sub(174)), theme.raised);
        p.fill(self.list.rect, theme.window);
        for place in &self.places {
            place.draw(p, false);
        }
        self.address.draw(p, self.focus == 0);
        self.list.draw(p, self.focus == 1);
        p.text_in(
            UiFont::Ui,
            Rect::new(20, h as i32 - 148, 132, 36),
            if self.options.folder { "Folder" } else { "File name" },
            theme.text_dim,
            Align::Right,
        );
        self.name.draw(p, self.focus == 2);
        for button in &self.buttons {
            button.draw(p, (button.id == 4 && self.focus == 3) || (button.id == 5 && self.focus == 4));
        }
        p.text_in(
            UiFont::Ui,
            Rect::new(20, h as i32 - 48, w.saturating_sub(280), 34),
            &self.status,
            theme.text_dim,
            Align::Left,
        );
        if let Some(dialog) = &self.confirm {
            dialog.draw(p);
        }
    }
}

fn matches_filter(name: &str, patterns: &str) -> bool {
    patterns.split(';').any(|pattern| {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern == "*" || pattern == "*.*" {
            return true;
        }
        let text = name.to_ascii_lowercase();
        // Greedy wildcard match with backtracking only at the latest '*'.
        let (p, t) = (pattern.as_bytes(), text.as_bytes());
        let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
        while j < t.len() {
            if i < p.len() && (p[i] == b'?' || p[i] == t[j]) {
                i += 1;
                j += 1;
            } else if i < p.len() && p[i] == b'*' {
                star = Some(i);
                i += 1;
                retry = j;
            } else if let Some(s) = star {
                retry += 1;
                j = retry;
                i = s + 1;
            } else {
                return false;
            }
        }
        while i < p.len() && p[i] == b'*' {
            i += 1;
        }
        i == p.len()
    })
}

pub(crate) fn show(options: FileOptions) -> Result<Option<FileSelection>, u32> {
    let owner = options.owner;
    let enabled = IsWindowEnabled(owner);
    let mut win = with(|g| {
        let mut spec = WindowSpec::normal(&options.title, 760, 560);
        spec.min_width = 620;
        spec.min_height = 440;
        if let Some(w) = g.windows.get(&owner) {
            spec.parent = w.win.id;
            spec.flags |= proto::window_flags::SKIP_TASKBAR;
        }
        g.conn()?.create_window(spec).map_err(|_| ERROR_NOT_ENOUGH_MEMORY)
    })?;
    if owner != 0 {
        EnableWindow(owner, FALSE);
    }
    os_std::println!("file-dialog: opened {} (owner {owner:#x})", options.title);
    let mut picker = Picker::new(options);
    let fonts = Fonts::load();
    let theme = Theme::default();
    let mut adapter = prism_client::ui::Adapter::default();
    let outcome = loop {
        let events = with(|g| -> Result<Vec<Event>, u32> {
            let c = g.conn()?;
            c.wait(&mut [], Some(Duration::from_millis(100))).map_err(|_| ERROR_GEN_FAILURE)?;
            let mut events = Vec::new();
            while let Some(event) = c.poll().map_err(|_| ERROR_GEN_FAILURE)? {
                events.push(event);
            }
            Ok(events)
        });
        let events = match events {
            Ok(events) => events,
            Err(e) => break Err(e),
        };
        for event in events {
            if !win.on_event(&event) {
                with(|g| g.on_event(event));
                continue;
            }
            picker.layout(win.width, win.height, &fonts);
            if matches!(event, Event::CloseRequest { .. }) {
                picker.result = Some(None);
            }
            if let Some((_, event)) = adapter.translate(&event) {
                picker.event(event, &fonts);
                win.dirty = true;
            }
        }
        if let Some(result) = picker.result.take() {
            break Ok(result);
        }
        if win.dirty && win.can_draw() {
            picker.layout(win.width, win.height, &fonts);
            let (w, h) = (win.width, win.height);
            if let Ok(mut surface) = win.surface() {
                picker.draw(&mut Painter::new(&mut surface, &theme, &fonts), w, h);
            }
            with(|g| {
                if let Some(c) = g.conn.as_mut() {
                    let _ = win.present(c, None);
                }
            });
        }
    };
    with(|g| {
        if let Some(c) = g.conn.as_mut() {
            let _ = win.destroy(c);
        }
    });
    if owner != 0 {
        EnableWindow(owner, enabled);
    }
    outcome
}
