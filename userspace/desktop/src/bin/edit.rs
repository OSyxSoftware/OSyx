//! Text Editor (`/system/bin/edit [file]`): a plain-text and source editor
//! on the OSyx toolkit.
//!
//! * Opens and saves UTF-8 files (line ends are kept as found: LF or CRLF);
//!   a file that is not UTF-8 is shown read-only.
//! * Editing is the toolkit's [`TextArea`]: selection, clipboard, undo and
//!   redo, line numbers.
//! * AXIS scripts (`.ax`, or a `#!` line naming axis) are coloured by the
//!   shell's own tokenizer; other source and configuration files get their
//!   comments, strings and numbers coloured.
//! * Ctrl+N new, Ctrl+O open, Ctrl+S save, Ctrl+Shift+S save as, Ctrl+F
//!   find, Ctrl+G or F3 next match (Shift: previous), Ctrl+Q quit.

#![no_std]
#![no_main]

#[path = "../shell_common.rs"]
mod common;

use common::{become_user, run_app, App, Ctx};
use os_std::prelude::*;
use os_std::{env, fs};
use osyx_gfx::Rect;
use osyx_input::key;
use osyx_shell::highlight::{self, Kind};
use osyx_ui::layout::{split_bottom, split_top};
use osyx_ui::widget::{Button, ButtonStyle, Dialog, Style, TextArea};
use osyx_ui::{Align, Event as Ui, FontKind, Painter, Response};
use prism_client::WindowSpec;

os_std::entry!(main);

/// Writes `data` as the file `path` so that a failure half way leaves the
/// file as it was: to a new file beside it first, renamed over it once
/// every byte is there.
///
/// - Through a symbolic link, the file the link names is saved and the
///   link stays; a link that names nothing is an error.
/// - The file beside it is made for this save only (never an existing
///   file or link), readable by its owner alone until it holds everything
///   and has the permissions of the file it replaces.
/// - Where that cannot be done (a folder the user may not write, or a file
///   of another user or of another group with rights of its own, which a
///   rename would make this user's and this group's), the file
///   can only be written in place, where a failure half way loses the old
///   contents. That is done only with `in_place`, which the user grants;
///   otherwise the answer is [`Save::NeedsInPlace`] and nothing is touched.
enum Save {
    Done,
    NeedsInPlace,
}

fn write_whole(path: &str, data: &[u8], in_place: bool) -> Result<Save, Error> {
    let direct = |target: &str| if in_place { fs::write(target, data).map(|()| Save::Done) } else { Ok(Save::NeedsInPlace) };
    let target = match fs::symlink_metadata(path) {
        Ok(_) => String::from(fs::canonicalize(path)?.as_str()),
        Err(e) if e.kind() == ErrorKind::NotFound => String::from(path),
        Err(e) => return Err(e),
    };
    let old = fs::metadata(&target).ok();
    // A new file is this user's, in this process's group, and nothing here
    // can give a file away. So a file of another user is not replaced, nor
    // one of another group whose group may do something others may not:
    // the new group would get that.
    let (uid, gid) = os_std::os::getuid();
    let foreign = |m: &fs::Metadata| {
        let mode = m.permissions().mode();
        m.uid() != uid || (m.gid() != gid && (mode >> 3) & 7 != mode & 7)
    };
    if old.as_ref().is_some_and(foreign) {
        return direct(&target);
    }
    let existed = old.is_some();
    let mode = old.map_or(0o644, |m| m.permissions().mode());
    // No two saves share a name, and a name someone made first is not
    // used: "create new" follows no link and replaces nothing.
    let pid = os_std::process::id().unwrap_or(0);
    let mut made = None;
    for attempt in 0..8u32 {
        let nanos = os_std::time::SystemTime::now()
            .duration_since(os_std::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let tmp = format!("{target}.{pid:x}-{:x}.osyx-saving", nanos.wrapping_add(u64::from(attempt)));
        match fs::File::options().write(true).create_new(true).mode(0o600).open(&tmp) {
            Ok(f) => {
                made = Some((f, tmp));
                break;
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            // A file that does not exist yet has nothing to lose.
            Err(e) if !existed => return Err(e),
            Err(_) => return direct(&target),
        }
    }
    let Some((mut file, tmp)) = made else {
        return Err(Error::new(ErrorKind::AlreadyExists, "no temporary file could be made beside the file"));
    };
    let written = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    let done = written.and_then(|()| fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))).and_then(|()| fs::rename(&tmp, &target));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done.map(|()| Save::Done)
}

/// Larger files are refused: the editor keeps the whole text in memory.
const MAX_FILE: u64 = 8 << 20;
const TOOLBAR: u32 = 44;
const STATUS: u32 = 26;

const D_OPEN: u32 = 1;
const D_SAVE_AS: u32 = 2;
const D_FIND: u32 = 3;
const D_DISCARD: u32 = 4;
const D_ERROR: u32 = 5;
const D_OVERWRITE: u32 = 6;
const D_IN_PLACE: u32 = 7;

/// What to do once unsaved changes are dealt with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    New,
    Open,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    Plain,
    Axis,
    /// Comments start with `#` (shell, TOML, Python, configuration).
    Hash,
    /// Comments start with `//` (Rust, C, JavaScript, JSON with comments).
    Slash,
}

impl Language {
    fn name(self) -> &'static str {
        match self {
            Language::Plain => "Plain text",
            Language::Axis => "AXIS script",
            Language::Hash | Language::Slash => "Source",
        }
    }

    fn of(path: &str, first_line: &str) -> Language {
        if first_line.starts_with("#!") {
            return if first_line.contains("axis") { Language::Axis } else { Language::Hash };
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
            Some("ax") => Language::Axis,
            Some("sh" | "toml" | "conf" | "cfg" | "ini" | "py" | "yml" | "yaml" | "app" | "reg") => Language::Hash,
            Some("rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "js" | "ts" | "json" | "css" | "go" | "java" | "ld") => Language::Slash,
            _ => Language::Plain,
        }
    }
}

fn axis_colors(line: &str) -> Vec<(usize, usize, Style)> {
    highlight::highlight(line)
        .into_iter()
        .map(|(a, b, k)| {
            let style = match k {
                Kind::Comment => Style::Comment,
                Kind::String => Style::String,
                Kind::Number => Style::Number,
                Kind::Keyword => Style::Keyword,
                Kind::Command => Style::Command,
                Kind::Variable => Style::Variable,
                Kind::Operator => Style::Operator,
            };
            (a, b, style)
        })
        .collect()
}

/// Comments (from `comment` to the end of the line), quoted strings and
/// numbers: enough for any source or configuration file.
fn source_colors(line: &str, comment: &str) -> Vec<(usize, usize, Style)> {
    let c: Vec<char> = line.chars().collect();
    let mark: Vec<char> = comment.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let start = i;
        if c[i..].starts_with(&mark) {
            out.push((i, c.len(), Style::Comment));
            break;
        } else if c[i] == '"' || c[i] == '\'' {
            let quote = c[i];
            i += 1;
            while i < c.len() && c[i] != quote {
                // An escaped quote does not end the string.
                i += if c[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(c.len());
            out.push((start, i, Style::String));
        } else if c[i].is_ascii_digit() && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_')) {
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.' || c[i] == '_') {
                i += 1;
            }
            out.push((start, i, Style::Number));
        } else {
            i += 1;
        }
    }
    out
}

fn hash_colors(line: &str) -> Vec<(usize, usize, Style)> {
    source_colors(line, "#")
}

fn slash_colors(line: &str) -> Vec<(usize, usize, Style)> {
    source_colors(line, "//")
}

struct Editor {
    area: TextArea,
    path: Option<String>,
    language: Language,
    /// The file used CRLF line ends; they are written back.
    crlf: bool,
    buttons: Vec<Button>,
    dialog: Option<Dialog>,
    then: Option<Then>,
    /// The Save as path waiting for the answer to "replace it?".
    replacing: Option<String>,
    /// A file the user allowed to be written in place, and one to ask about.
    in_place: Option<String>,
    ask_in_place: Option<String>,
    needle: String,
    message: String,
    size: (u32, u32),
    /// The revision and modified state the title was made for.
    titled: Option<(bool, Option<String>)>,
}

const B_NEW: usize = 0;
const B_OPEN: usize = 1;
const B_SAVE: usize = 2;
const B_SAVE_AS: usize = 3;
const B_UNDO: usize = 4;
const B_REDO: usize = 5;
const B_FIND: usize = 6;

impl Editor {
    fn new() -> Editor {
        let labels = ["New", "Open", "Save", "Save as", "Undo", "Redo", "Find"];
        Editor {
            area: TextArea::new(""),
            path: None,
            language: Language::Plain,
            crlf: false,
            buttons: labels.iter().enumerate().map(|(i, l)| Button::new(i, l, ButtonStyle::Flat)).collect(),
            dialog: None,
            then: None,
            replacing: None,
            in_place: None,
            ask_in_place: None,
            needle: String::new(),
            message: String::new(),
            size: (0, 0),
            titled: None,
        }
    }

    fn set_language(&mut self, language: Language) {
        self.language = language;
        self.area.highlighter = match language {
            Language::Plain => None,
            Language::Axis => Some(axis_colors),
            Language::Hash => Some(hash_colors),
            Language::Slash => Some(slash_colors),
        };
    }

    fn name(&self) -> &str {
        self.path.as_deref().map_or("Untitled", |p| p.rsplit('/').next().unwrap_or(p))
    }

    fn clear(&mut self) {
        self.area.set_text("");
        self.area.read_only = false;
        self.path = None;
        self.crlf = false;
        self.set_language(Language::Plain);
        self.message.clear();
    }

    /// Loads `path`; a path that does not exist yet starts an empty file
    /// with that name.
    fn open(&mut self, path: &str) -> Result<(), String> {
        let path = absolute(path);
        let bytes = match fs::metadata(&path) {
            Ok(m) if m.is_dir() => return Err(format!("{path} is a folder.")),
            Ok(m) if m.len() > MAX_FILE => return Err(format!("{path} is larger than 8 MiB.")),
            Ok(_) => fs::read(&path).map_err(|e| format!("{path} cannot be read: {e}."))?,
            Err(e) if e.kind() == os_std::error::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("{path} cannot be read: {e}.")),
        };
        let (text, valid) = match String::from_utf8(bytes) {
            Ok(t) => (t, true),
            Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), false),
        };
        // One kind of line end is written back. A file with both keeps the
        // kind most of its lines have, and says so before anything is saved.
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        self.crlf = crlf > lf;
        self.area.set_text(&text);
        self.area.read_only = !valid;
        self.set_language(Language::of(&path, self.area.line(0)));
        self.message = if !valid {
            String::from("Not UTF-8 text: opened read-only")
        } else if crlf > 0 && lf > 0 {
            format!("Mixed line ends ({crlf} CRLF, {lf} LF): saving writes {} everywhere", if self.crlf { "CRLF" } else { "LF" })
        } else {
            String::new()
        };
        println!("edit: opened {path} ({} lines, {})", self.area.line_count(), self.language.name());
        self.path = Some(path);
        Ok(())
    }

    fn save_to(&mut self, path: &str) -> Result<(), String> {
        // What is shown of a file that is not UTF-8 has lost bytes: writing
        // it back would destroy the file.
        if self.area.read_only {
            return Err(String::from("This file is not UTF-8 text and was opened read-only: it cannot be saved."));
        }
        let path = absolute(path);
        let mut text = self.area.text();
        if self.crlf {
            text = text.replace('\n', "\r\n");
        }
        let in_place = self.in_place.take().is_some_and(|granted| granted == path);
        match write_whole(&path, text.as_bytes(), in_place).map_err(|e| format!("{path} cannot be written: {e}."))? {
            Save::Done => {}
            Save::NeedsInPlace => {
                // `error` asks instead of reporting.
                self.ask_in_place = Some(path.clone());
                return Err(format!("{path} can only be written in place."));
            }
        }
        self.area.mark_saved();
        self.set_language(Language::of(&path, self.area.line(0)));
        self.message = format!("Saved {} bytes", text.len());
        println!("edit: saved {path} ({} bytes)", text.len());
        self.path = Some(path);
        Ok(())
    }

    /// Saves to `path`, then does what was waiting for the save.
    fn save_then(&mut self, path: &str, cx: &mut Ctx) {
        match self.save_to(path) {
            Ok(()) => {
                if let Some(then) = self.then.take() {
                    self.proceed(then, cx);
                }
            }
            Err(e) => self.error(&e),
        }
    }

    fn error(&mut self, text: &str) {
        if let Some(path) = self.ask_in_place.take() {
            self.dialog = Some(Dialog::new(
                D_IN_PLACE,
                "Save",
                &format!("{path} cannot be replaced by a new file (its folder is not writable, or the file is not yours or belongs to another group). It can be written in place: if that fails half way, its old contents are lost. Write it in place?"),
                &[("Cancel", ButtonStyle::Secondary), ("Write in place", ButtonStyle::Danger)],
            ));
            self.replacing = Some(path);
            return;
        }
        self.dialog = Some(Dialog::new(D_ERROR, "Text Editor", text, &[("Close", ButtonStyle::Primary)]));
    }

    fn ask_path(&mut self, tag: u32) {
        let (title, text, button) = if tag == D_OPEN {
            ("Open", "The path of the file to open. A file that does not exist yet is created when it is saved.", "Open")
        } else {
            ("Save as", "The path to save this text to.", "Save")
        };
        let current = self.path.clone().unwrap_or_else(|| format!("{}/", env::current_dir().map(|p| String::from(p.as_str())).unwrap_or_default()));
        self.dialog =
            Some(Dialog::new(tag, title, text, &[("Cancel", ButtonStyle::Secondary), (button, ButtonStyle::Primary)]).with_input(&current, "/home/user/notes.txt"));
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => {
                if let Err(e) = self.save_to(&p) {
                    self.error(&e);
                }
            }
            None => self.ask_path(D_SAVE_AS),
        }
    }

    /// Runs `then` now, or asks first when there are unsaved changes.
    fn guarded(&mut self, then: Then, cx: &mut Ctx) {
        if self.area.is_modified() {
            self.then = Some(then);
            let msg = format!("{} has changes that are not saved.", self.name());
            self.dialog = Some(Dialog::new(
                D_DISCARD,
                "Unsaved changes",
                &msg,
                &[("Cancel", ButtonStyle::Secondary), ("Discard", ButtonStyle::Danger), ("Save", ButtonStyle::Primary)],
            ));
        } else {
            self.proceed(then, cx);
        }
    }

    fn proceed(&mut self, then: Then, cx: &mut Ctx) {
        match then {
            Then::New => self.clear(),
            Then::Open => self.ask_path(D_OPEN),
            Then::Quit => cx.quit = true,
        }
    }

    fn find_next(&mut self, forward: bool) {
        if self.needle.is_empty() {
            return;
        }
        let needle = self.needle.clone();
        self.message = if self.area.find(&needle, forward) { String::new() } else { format!("\"{needle}\" was not found") };
    }

    fn command(&mut self, id: usize, cx: &mut Ctx) {
        match id {
            B_NEW => self.guarded(Then::New, cx),
            B_OPEN => self.guarded(Then::Open, cx),
            B_SAVE => self.save(),
            B_SAVE_AS => self.ask_path(D_SAVE_AS),
            B_UNDO => {
                self.area.undo();
            }
            B_REDO => {
                self.area.redo();
            }
            B_FIND => {
                let start = self.area.selected_text().filter(|s| !s.contains('\n')).unwrap_or_else(|| self.needle.clone());
                self.dialog = Some(
                    Dialog::new(D_FIND, "Find", "Lower-case text matches any case.", &[("Cancel", ButtonStyle::Secondary), ("Find", ButtonStyle::Primary)])
                        .with_input(&start, "text to find"),
                );
            }
            _ => {}
        }
        self.area.reveal_cursor(&cx.fonts);
    }

    /// The answer to a dialog: `button` is the index pressed.
    fn answer(&mut self, tag: u32, button: usize, input: String, cx: &mut Ctx) {
        match (tag, button) {
            (D_OPEN, 1) => {
                if let Err(e) = self.open(input.trim()) {
                    self.error(&e);
                }
            }
            (D_SAVE_AS, 1) => {
                // Another file under that name is replaced only on a yes.
                let to = absolute(input.trim());
                if self.path.as_deref() != Some(to.as_str()) && fs::metadata(&to).is_ok_and(|m| m.is_file()) {
                    self.dialog = Some(Dialog::new(
                        D_OVERWRITE,
                        "Save as",
                        &format!("{to} exists. Replace it?"),
                        &[("Cancel", ButtonStyle::Secondary), ("Replace", ButtonStyle::Danger)],
                    ));
                    self.replacing = Some(to);
                } else {
                    self.save_then(&to, cx);
                }
            }
            (D_OVERWRITE, 1) => {
                if let Some(to) = self.replacing.take() {
                    self.save_then(&to, cx);
                }
            }
            (D_IN_PLACE, 1) => {
                if let Some(to) = self.replacing.take() {
                    self.in_place = Some(to.clone());
                    self.save_then(&to, cx);
                }
            }
            (D_FIND, 1) => {
                self.needle = input;
                self.find_next(true);
            }
            (D_DISCARD, 1) => {
                if let Some(then) = self.then.take() {
                    // Open only asks for a path: the text stays unsaved
                    // until another file has taken its place.
                    if !matches!(then, Then::Open) {
                        self.area.mark_saved();
                    }
                    self.proceed(then, cx);
                }
            }
            (D_DISCARD, 2) => match self.path.clone() {
                Some(p) => match self.save_to(&p) {
                    Ok(()) => {
                        if let Some(then) = self.then.take() {
                            self.proceed(then, cx);
                        }
                    }
                    Err(e) => self.error(&e),
                },
                // `then` stays set: it runs once the name is given.
                None => self.ask_path(D_SAVE_AS),
            },
            _ => {
                // Cancelled: nothing waits for this dialog any more.
                self.then = None;
                self.replacing = None;
            }
        }
        self.area.reveal_cursor(&cx.fonts);
    }

    fn update_title(&mut self, cx: &mut Ctx) {
        let state = (self.area.is_modified(), self.path.clone());
        if self.titled.as_ref() != Some(&state) {
            let star = if state.0 { "*" } else { "" };
            cx.title = Some(format!("{}{star} - Text Editor", self.name()));
            self.titled = Some(state);
        }
    }
}

/// `path` from the current directory when it is relative.
fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        return String::from(path);
    }
    let cwd = env::current_dir().map(|p| String::from(p.as_str())).unwrap_or_default();
    format!("{}/{path}", cwd.trim_end_matches('/'))
}

impl App for Editor {
    fn layout(&mut self, w: u32, h: u32, cx: &mut Ctx) {
        self.size = (w, h);
        let full = Rect::new(0, 0, w, h);
        let (bar, rest) = split_top(full, TOOLBAR);
        let (body, _) = split_bottom(rest, STATUS);
        let mut x = bar.x + 8;
        for (i, b) in self.buttons.iter_mut().enumerate() {
            // A gap before Undo and before Find.
            if i == B_UNDO || i == B_FIND {
                x += 12;
            }
            let bw = b.preferred_width(&cx.fonts);
            b.rect = Rect::new(x, bar.y + 6, bw, TOOLBAR - 12);
            x += bw as i32 + 4;
        }
        self.area.rect = body;
        self.area.layout(&cx.fonts);
        if let Some(d) = &mut self.dialog {
            d.layout(full, &cx.fonts);
        }
        self.update_title(cx);
    }

    fn event(&mut self, ev: Ui, cx: &mut Ctx) -> bool {
        if let Some(d) = &mut self.dialog {
            let r = d.event_with_clipboard(&ev, &cx.fonts, &mut cx.clipboard);
            let (tag, input) = (d.tag, String::from(d.input_text()));
            match r {
                Response::Activated(i) => {
                    self.dialog = None;
                    self.answer(tag, i, input, cx);
                }
                Response::Cancelled => {
                    self.dialog = None;
                    self.answer(tag, 0, input, cx);
                }
                r if !r.needs_redraw() => return false,
                _ => {}
            }
            let (w, h) = self.size;
            self.layout(w, h, cx);
            return true;
        }
        if let Ui::Key(k) = ev {
            if k.down && k.ctrl() {
                let id = match k.key {
                    key::N => Some(B_NEW),
                    key::O => Some(B_OPEN),
                    key::S if k.shift() => Some(B_SAVE_AS),
                    key::S => Some(B_SAVE),
                    key::F => Some(B_FIND),
                    _ => None,
                };
                if let Some(id) = id {
                    self.command(id, cx);
                    let (w, h) = self.size;
                    self.layout(w, h, cx);
                    return true;
                }
                if k.key == key::Q {
                    self.guarded(Then::Quit, cx);
                    let (w, h) = self.size;
                    self.layout(w, h, cx);
                    return true;
                }
            }
            // F3 is the third function key; Ctrl+G does the same.
            if k.down && (k.key == key::F1 + 2 || (k.ctrl() && k.key == key::G)) {
                self.find_next(!k.shift());
                self.area.reveal_cursor(&cx.fonts);
                return true;
            }
            if k.pressed(key::ESCAPE) {
                self.area.clear_find();
                self.message.clear();
                return true;
            }
        }
        let mut redraw = false;
        let mut pressed = None;
        for b in &mut self.buttons {
            match b.event(&ev, false) {
                Response::Activated(id) => pressed = Some(id),
                r => redraw |= r.needs_redraw(),
            }
        }
        if let Some(id) = pressed {
            self.command(id, cx);
            let (w, h) = self.size;
            self.layout(w, h, cx);
            return true;
        }
        let r = self.area.event(&ev, &cx.fonts, &mut cx.clipboard);
        if r == Response::Changed {
            self.message.clear();
            self.area.layout(&cx.fonts);
            // A `#!` line typed into a new text decides its language.
            let language = Language::of(self.path.as_deref().unwrap_or(""), self.area.line(0));
            if language != self.language {
                self.set_language(language);
            }
        }
        self.update_title(cx);
        redraw || r.needs_redraw()
    }

    fn file_drop(&mut self, path: &str, _x: i32, _y: i32, cx: &mut Ctx) -> bool {
        if self.area.is_modified() {
            self.message = String::from("Save or discard the changes before opening another file");
        } else if let Err(e) = self.open(path) {
            self.error(&e);
        }
        let (w, h) = self.size;
        self.layout(w, h, cx);
        true
    }

    fn close_requested(&mut self, cx: &mut Ctx) -> bool {
        if !self.area.is_modified() {
            return true;
        }
        self.guarded(Then::Quit, cx);
        let (w, h) = self.size;
        self.layout(w, h, cx);
        false
    }

    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, cx: &Ctx) {
        let t = cx.theme;
        let full = Rect::new(0, 0, w, h);
        p.fill(full, t.window);
        let (bar, rest) = split_top(full, TOOLBAR);
        p.fill(bar, t.surface);
        p.hline(bar.x, bar.y + bar.h as i32 - 1, bar.w, t.border);
        self.buttons[B_SAVE].enabled = self.area.is_modified() || self.path.is_none();
        self.buttons[B_UNDO].enabled = self.area.can_undo();
        self.buttons[B_REDO].enabled = self.area.can_redo();
        for b in &self.buttons {
            b.draw(p, false);
        }
        self.area.draw(p, cx.focused && self.dialog.is_none());
        let (_, status) = split_bottom(rest, STATUS);
        p.fill(status, t.surface);
        p.hline(status.x, status.y, status.w, t.border);
        let c = self.area.cursor();
        let mut left = format!("Ln {}, Col {}", c.line + 1, c.col + 1);
        if let Some(s) = self.area.selected_text() {
            left += &format!("  ·  {} selected", s.chars().count());
        }
        if !self.message.is_empty() {
            left += &format!("  ·  {}", self.message);
        }
        let inner = Rect::new(status.x + 12, status.y, status.w.saturating_sub(24), status.h);
        p.text_in(FontKind::Ui, inner, &left, t.text_dim, Align::Left);
        let ends = if self.crlf { "CRLF" } else { "LF" };
        let state = if self.area.read_only {
            "read-only"
        } else if self.area.is_modified() {
            "modified"
        } else {
            "saved"
        };
        let right = format!("{}  ·  UTF-8  ·  {ends}  ·  {} lines  ·  {state}", self.language.name(), self.area.line_count());
        p.text_in(FontKind::Ui, inner, &right, t.text_faint, Align::Right);
        if let Some(d) = &self.dialog {
            p.blend(full, t.scrim, 140);
            d.draw(p);
        }
    }
}

fn main() -> i32 {
    become_user();
    let mut app = Editor::new();
    let mut title = String::from("Untitled - Text Editor");
    if let Some(path) = env::args().nth(1) {
        match app.open(&path) {
            Ok(()) => title = format!("{} - Text Editor", app.name()),
            Err(e) => app.error(&e),
        }
    }
    let mut spec = WindowSpec::normal(&title, 860, 600);
    spec.min_width = 420;
    spec.min_height = 260;
    run_app("edit", spec, &mut app)
}
