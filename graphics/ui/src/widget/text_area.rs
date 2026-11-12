//! A multi-line text editor widget in the monospace font: cursor and
//! selection, keyboard and pointer editing, the clipboard, undo and redo,
//! line numbers, find, and colours from a per-line highlighter.
//!
//! The text is a list of lines; positions are (line, column) with the
//! column counted in characters, so every cell of the monospace font is one
//! column. A tab is drawn as spaces up to the next multiple of
//! [`TAB_WIDTH`]; the Tab key inserts spaces.

use crate::clipboard::Clipboard;
use crate::event::{Event, KeyEvent, Response};
use crate::layout::contains;
use crate::paint::{FontKind, Fonts, Painter};
use crate::theme::Theme;
use crate::widget::Scroll;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use osyx_gfx::{Color, Rect};
use osyx_input::{button, key};

pub const TAB_WIDTH: usize = 4;
/// Edits kept for undo.
const MAX_UNDO: usize = 1000;
const PAD: i32 = 6;
const SCROLLBAR: u32 = 12;

/// A position: line, and column in characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

/// What a highlighter says about a stretch of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Command,
    Variable,
    Operator,
}

impl Style {
    pub fn color(self, t: &Theme) -> Color {
        match self {
            Style::Plain => t.text,
            Style::Comment => t.text_faint,
            Style::String => t.success,
            Style::Number => t.warning,
            Style::Keyword => t.accent_hover,
            Style::Command => t.accent,
            Style::Variable => t.danger_hover,
            Style::Operator => t.text_dim,
        }
    }
}

/// Colours for one line: `(first column, end column, style)` in order,
/// not overlapping; columns it leaves out are plain.
pub type Highlighter = fn(&str) -> Vec<(usize, usize, Style)>;

/// One change, as undo needs it: at `at`, `removed` was replaced by
/// `inserted`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
    /// Cursor before the edit.
    before: Pos,
    /// Typed characters join the previous edit when this is set.
    typing: bool,
}

#[derive(Clone, Debug)]
pub struct TextArea {
    pub rect: Rect,
    lines: Vec<String>,
    cursor: Pos,
    /// The other end of the selection.
    anchor: Option<Pos>,
    /// The column vertical movement aims for.
    wanted_col: usize,
    /// For vertical moves: where the cursor was after the last one and
    /// the screen cell it aims for.
    wanted_cell: Option<(Pos, usize)>,
    /// First line shown, as a pixel offset, and columns scrolled out.
    pub scroll: Scroll,
    scroll_x: usize,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Changes since [`TextArea::mark_saved`], counted through undo.
    saved_at: Option<usize>,
    dragging: bool,
    pub read_only: bool,
    pub line_numbers: bool,
    pub highlighter: Option<Highlighter>,
    /// Matches of the last [`TextArea::find`] are shown in every line.
    needle: String,
    /// Counts every change of the text.
    revision: u64,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Byte index of column `col` (the end of the line if it is shorter).
fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Screen cells before column `col` of `line` (tabs take several).
/// The column whose cell is `cell`, or the last one before it (a tab
/// covers several cells); the line's length past its end.
fn col_covering(line: &str, cell: usize) -> usize {
    let mut at = 0;
    for (col, c) in line.chars().enumerate() {
        let next = if c == '\t' { (at / TAB_WIDTH + 1) * TAB_WIDTH } else { at + 1 };
        if next > cell {
            return col;
        }
        at = next;
    }
    char_len(line)
}

/// Columns where `needle` starts in `line`. With `fold`, letters match
/// whatever their case; the comparison is character by character, so a
/// letter whose other case is longer or shorter in UTF-8 moves nothing.
fn match_cols(line: &str, needle: &str, fold: bool) -> Vec<usize> {
    let lower = |c: char| if fold { c.to_lowercase().next().unwrap_or(c) } else { c };
    let hay: Vec<char> = line.chars().map(lower).collect();
    let needle: Vec<char> = needle.chars().map(lower).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - needle.len()).filter(|&i| hay[i..i + needle.len()] == needle[..]).collect()
}

fn cells_before(line: &str, col: usize) -> usize {
    let mut cells = 0;
    for c in line.chars().take(col) {
        cells += if c == '\t' { TAB_WIDTH - cells % TAB_WIDTH } else { 1 };
    }
    cells
}

/// The column of `line` at screen cell `cell` (the nearer side of a tab).
fn col_at_cell(line: &str, cell: usize) -> usize {
    let mut cells = 0;
    for (col, c) in line.chars().enumerate() {
        let w = if c == '\t' { TAB_WIDTH - cells % TAB_WIDTH } else { 1 };
        if cell < cells + w.div_ceil(2) {
            return col;
        }
        cells += w;
    }
    char_len(line)
}

impl Default for TextArea {
    fn default() -> Self {
        TextArea::new("")
    }
}

impl TextArea {
    pub fn new(text: &str) -> TextArea {
        let mut t = TextArea {
            rect: Rect::EMPTY,
            lines: vec![String::new()],
            cursor: Pos::default(),
            anchor: None,
            wanted_col: 0,
            wanted_cell: None,
            scroll: Scroll::default(),
            scroll_x: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_at: Some(0),
            dragging: false,
            read_only: false,
            line_numbers: true,
            highlighter: None,
            needle: String::new(),
            revision: 0,
        };
        t.set_text(text);
        t
    }

    /// Replaces everything (a file was loaded); the text counts as saved.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(|l| String::from(l.strip_suffix('\r').unwrap_or(l))).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor = Pos::default();
        self.anchor = None;
        self.wanted_col = 0;
        self.scroll_x = 0;
        self.scroll.scroll_to(0);
        self.undo.clear();
        self.redo.clear();
        self.saved_at = Some(0);
    }

    /// The whole text, lines joined by `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", |l| l.as_str())
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    /// A number that changes whenever the text does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_modified(&self) -> bool {
        self.saved_at != Some(self.undo.len())
    }

    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.undo.len());
        // Typing after a save starts a new undo step.
        if let Some(e) = self.undo.last_mut() {
            e.typing = false;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The selection, start before end; `None` when it is empty.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.range_text(a, b))
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(char_len(&self.lines[line])))
    }

    fn end(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos::new(line, char_len(&self.lines[line]))
    }

    fn range_text(&self, a: Pos, b: Pos) -> String {
        if a.line == b.line {
            let l = &self.lines[a.line];
            return String::from(&l[byte_at(l, a.col)..byte_at(l, b.col)]);
        }
        let first = &self.lines[a.line];
        let mut out = String::from(&first[byte_at(first, a.col)..]);
        for l in &self.lines[a.line + 1..b.line] {
            out.push('\n');
            out.push_str(l);
        }
        out.push('\n');
        let last = &self.lines[b.line];
        out.push_str(&last[..byte_at(last, b.col)]);
        out
    }

    /// Where text of `inserted` put at `at` ends.
    fn end_of(at: Pos, inserted: &str) -> Pos {
        match inserted.rsplit_once('\n') {
            Some((head, tail)) => Pos::new(at.line + head.matches('\n').count() + 1, char_len(tail)),
            None => Pos::new(at.line, at.col + char_len(inserted)),
        }
    }

    /// The one primitive every change goes through: the text from `a` to
    /// `b` becomes `text`. Returns what was there.
    fn splice(&mut self, a: Pos, b: Pos, text: &str) -> String {
        self.revision += 1;
        let removed = self.range_text(a, b);
        let first = &self.lines[a.line];
        let head = String::from(&first[..byte_at(first, a.col)]);
        let last = &self.lines[b.line];
        let tail = String::from(&last[byte_at(last, b.col)..]);
        let mut new: Vec<String> = text.split('\n').map(String::from).collect();
        new[0].insert_str(0, &head);
        let n = new.len();
        new[n - 1].push_str(&tail);
        self.lines.splice(a.line..=b.line, new);
        removed
    }

    /// Replaces `a..b` by `text` as one undo step (joined to the previous
    /// one while single characters are typed in a row).
    fn replace(&mut self, a: Pos, b: Pos, text: &str, typing: bool) {
        if self.read_only || (a == b && text.is_empty()) {
            return;
        }
        let before = self.cursor;
        let removed = self.splice(a, b, text);
        let after = Self::end_of(a, text);
        let at_saved = self.saved_at == Some(self.undo.len());
        let joined = match self.undo.last_mut() {
            Some(last)
                if typing
                    && last.typing
                    && removed.is_empty()
                    && !at_saved
                    && Self::end_of(last.at, &last.inserted) == a =>
            {
                last.inserted.push_str(text);
                true
            }
            _ => false,
        };
        if !joined {
            // A saved state that undo can no longer reach.
            if self.saved_at.is_some_and(|s| s > self.undo.len()) {
                self.saved_at = None;
            }
            self.undo.push(Edit { at: a, removed, inserted: String::from(text), before, typing });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
                self.saved_at = self.saved_at.and_then(|s| s.checked_sub(1));
            }
        }
        self.redo.clear();
        self.cursor = after;
        self.anchor = None;
        self.wanted_col = after.col;
    }

    pub fn undo(&mut self) -> bool {
        // Text that may not be changed is not changed back either.
        if self.read_only {
            return false;
        }
        let Some(e) = self.undo.pop() else { return false };
        let end = Self::end_of(e.at, &e.inserted);
        self.splice(e.at, end, &e.removed);
        self.cursor = e.before;
        self.anchor = None;
        self.wanted_col = self.cursor.col;
        self.redo.push(e);
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let Some(mut e) = self.redo.pop() else { return false };
        let end = Self::end_of(e.at, &e.removed);
        self.splice(e.at, end, &e.inserted);
        self.cursor = Self::end_of(e.at, &e.inserted);
        self.anchor = None;
        self.wanted_col = self.cursor.col;
        e.typing = false;
        self.undo.push(e);
        true
    }

    /// Inserts `text` over the selection (or at the cursor).
    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        let typing = a == b && char_len(&text) == 1 && text != "\n";
        self.replace(a, b, &text, typing);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        self.cursor = self.end();
    }

    /// Moves the cursor; with `select` the selection follows it.
    pub fn move_to(&mut self, p: Pos, select: bool) {
        let p = self.clamp(p);
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = p;
        self.wanted_col = p.col;
    }

    fn left_of(&self, p: Pos) -> Pos {
        if p.col > 0 {
            Pos::new(p.line, p.col - 1)
        } else if p.line > 0 {
            Pos::new(p.line - 1, char_len(&self.lines[p.line - 1]))
        } else {
            p
        }
    }

    fn right_of(&self, p: Pos) -> Pos {
        if p.col < char_len(&self.lines[p.line]) {
            Pos::new(p.line, p.col + 1)
        } else if p.line + 1 < self.lines.len() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        }
    }

    /// The start of the word before `p`: over blanks and punctuation,
    /// then over the word. Each line is read once.
    fn word_left(&self, mut p: Pos) -> Pos {
        loop {
            let line: Vec<char> = self.lines[p.line].chars().take(p.col).collect();
            let mut col = line.len();
            while col > 0 && !is_word(line[col - 1]) {
                col -= 1;
            }
            if col == 0 && p.line > 0 {
                // Nothing but blanks back to the line's start: the word
                // is on a line above.
                p = Pos::new(p.line - 1, char_len(&self.lines[p.line - 1]));
                continue;
            }
            while col > 0 && is_word(line[col - 1]) {
                col -= 1;
            }
            return Pos::new(p.line, col);
        }
    }

    /// The start of the word after `p`: over the word, then over blanks
    /// and punctuation. Each line is read once.
    fn word_right(&self, mut p: Pos) -> Pos {
        let mut in_word = true;
        loop {
            let mut col = p.col;
            for c in self.lines[p.line].chars().skip(p.col) {
                if in_word && !is_word(c) {
                    in_word = false;
                }
                if !in_word && is_word(c) {
                    return Pos::new(p.line, col);
                }
                col += 1;
            }
            if p.line + 1 >= self.lines.len() {
                return Pos::new(p.line, col);
            }
            // A line end is a blank.
            in_word = false;
            p = Pos::new(p.line + 1, 0);
        }
    }

    /// The word around `p` (a single character where there is none).
    fn word_around(&self, p: Pos) -> (Pos, Pos) {
        let line: Vec<char> = self.lines[p.line].chars().collect();
        if line.is_empty() {
            return (p, p);
        }
        let col = p.col.min(line.len() - 1);
        if !is_word(line[col]) {
            return (Pos::new(p.line, col), Pos::new(p.line, col + 1));
        }
        let start = (0..=col).rev().take_while(|&i| is_word(line[i])).last().unwrap_or(col);
        let end = (col..line.len()).take_while(|&i| is_word(line[i])).last().unwrap_or(col) + 1;
        (Pos::new(p.line, start), Pos::new(p.line, end))
    }

    /// Up or down by `by` lines, keeping the column aimed for.
    fn vertical(&mut self, by: i64, select: bool) {
        let line = (self.cursor.line as i64 + by).clamp(0, self.lines.len() as i64 - 1) as usize;
        // The same screen cell, so tabs do not shift the cursor. The cell
        // aimed for is where the cursor was when it last moved sideways;
        // short lines and tabs on the way do not change it.
        let cell = match self.wanted_cell {
            Some((at, cell)) if at == self.cursor => cell,
            _ => cells_before(&self.lines[self.cursor.line], self.cursor.col),
        };
        let col = col_at_cell(&self.lines[line], cell);
        self.move_to(Pos::new(line, col), select);
        self.wanted_cell = Some((self.cursor, cell));
    }

    // ----- find --------------------------------------------------------

    /// Selects the next (or previous) occurrence of `needle` after the
    /// selection, wrapping around; lower-case needles match any case.
    /// Returns false when there is none.
    pub fn find(&mut self, needle: &str, forward: bool) -> bool {
        self.needle = String::from(needle);
        if needle.is_empty() {
            return false;
        }
        let fold = !needle.chars().any(char::is_uppercase);
        let matches = |line: &str| match_cols(line, needle, fold);
        let n = self.lines.len();
        let len = char_len(needle);
        let from = if forward { self.selection().map_or(self.cursor, |s| s.1) } else { self.selection().map_or(self.cursor, |s| s.0) };
        for step in 0..=n {
            let line = if forward { (from.line + step) % n } else { (from.line + n - step % n) % n };
            let cols = matches(&self.lines[line]);
            let hit = if forward {
                cols.into_iter().find(|&c| step > 0 || c >= from.col)
            } else {
                cols.into_iter().rev().find(|&c| step > 0 || c + len <= from.col)
            };
            if let Some(col) = hit {
                self.anchor = Some(Pos::new(line, col));
                self.cursor = Pos::new(line, col + len);
                self.wanted_col = self.cursor.col;
                return true;
            }
        }
        false
    }

    /// Stops showing the matches of the last search.
    pub fn clear_find(&mut self) {
        self.needle.clear();
    }

    // ----- geometry ----------------------------------------------------

    fn gutter(&self, fonts: &Fonts) -> u32 {
        if !self.line_numbers {
            return 0;
        }
        // At least three digits, so the gutter does not jump at line 100.
        let mut digits = 3;
        let mut n = self.lines.len() / 1000;
        while n > 0 {
            digits += 1;
            n /= 10;
        }
        (digits + 2) * fonts.mono.cell_width()
    }

    /// The area the text is drawn in.
    fn text_rect(&self, fonts: &Fonts) -> Rect {
        let g = self.gutter(fonts) as i32;
        Rect::new(self.rect.x + g + PAD, self.rect.y, (self.rect.w as i32 - g - PAD - SCROLLBAR as i32).max(0) as u32, self.rect.h)
    }

    fn track(&self) -> Rect {
        Rect::new(self.rect.x + self.rect.w as i32 - SCROLLBAR as i32, self.rect.y, SCROLLBAR, self.rect.h)
    }

    fn visible_cols(&self, fonts: &Fonts) -> usize {
        (self.text_rect(fonts).w / fonts.mono.cell_width().max(1)) as usize
    }

    /// Call after the rectangle or the text changed size.
    pub fn layout(&mut self, fonts: &Fonts) {
        let lh = fonts.line_height(FontKind::Mono);
        self.scroll.set_sizes(self.lines.len() as u32 * lh + self.rect.h / 2, self.rect.h);
    }

    /// Scrolls so that the cursor is shown.
    pub fn reveal_cursor(&mut self, fonts: &Fonts) {
        self.layout(fonts);
        let lh = fonts.line_height(FontKind::Mono);
        self.scroll.ensure_visible(self.cursor.line as u32 * lh, lh);
        let cell = cells_before(&self.lines[self.cursor.line], self.cursor.col);
        let cols = self.visible_cols(fonts).max(1);
        if cell < self.scroll_x {
            self.scroll_x = cell.saturating_sub(4);
        } else if cell >= self.scroll_x + cols {
            self.scroll_x = cell + 4 - cols;
        }
    }

    fn pos_at(&self, x: i32, y: i32, fonts: &Fonts) -> Pos {
        let lh = fonts.line_height(FontKind::Mono) as i64;
        let r = self.text_rect(fonts);
        let line = ((y as i64 - r.y as i64 + self.scroll.offset as i64) / lh).clamp(0, self.lines.len() as i64 - 1) as usize;
        let cw = fonts.mono.cell_width() as i32;
        let cell = ((x - r.x + cw / 2).max(0) / cw) as usize + self.scroll_x;
        // `col_at_cell` rounds within a cell; the half cell is already added.
        let mut cells = 0;
        let mut col = char_len(&self.lines[line]);
        for (i, c) in self.lines[line].chars().enumerate() {
            let w = if c == '\t' { TAB_WIDTH - cells % TAB_WIDTH } else { 1 };
            if cell < cells + w {
                col = i;
                break;
            }
            cells += w;
        }
        Pos::new(line, col)
    }

    // ----- events ------------------------------------------------------

    fn key(&mut self, k: &KeyEvent, clipboard: &mut Clipboard, page: i64) -> Response {
        if !k.down {
            return Response::Ignored;
        }
        let (ctrl, shift) = (k.ctrl(), k.shift());
        let before = (self.cursor, self.anchor, self.revision);
        match k.key {
            key::LEFT if ctrl => self.move_to(self.word_left(self.cursor), shift),
            key::RIGHT if ctrl => self.move_to(self.word_right(self.cursor), shift),
            key::LEFT => match self.selection() {
                Some((a, _)) if !shift => self.move_to(a, false),
                _ => self.move_to(self.left_of(self.cursor), shift),
            },
            key::RIGHT => match self.selection() {
                Some((_, b)) if !shift => self.move_to(b, false),
                _ => self.move_to(self.right_of(self.cursor), shift),
            },
            key::UP => self.vertical(-1, shift),
            key::DOWN => self.vertical(1, shift),
            key::PAGE_UP => self.vertical(-page, shift),
            key::PAGE_DOWN => self.vertical(page, shift),
            key::HOME if ctrl => self.move_to(Pos::default(), shift),
            key::END if ctrl => self.move_to(self.end(), shift),
            key::HOME => {
                // First to the text, then to the margin.
                let indent = self.lines[self.cursor.line].chars().take_while(|c| c.is_whitespace()).count();
                let col = if self.cursor.col == indent { 0 } else { indent };
                self.move_to(Pos::new(self.cursor.line, col), shift);
            }
            key::END => self.move_to(Pos::new(self.cursor.line, usize::MAX), shift),
            key::BACKSPACE | key::DELETE => {
                let (a, b) = match (self.selection(), k.key == key::BACKSPACE, ctrl) {
                    (Some(s), _, _) => s,
                    (None, true, true) => (self.word_left(self.cursor), self.cursor),
                    (None, true, false) => {
                        // Inside the indentation: back to the previous tab stop.
                        let line = &self.lines[self.cursor.line];
                        let c = self.cursor.col;
                        let in_indent = c > 0 && line.chars().take(c).all(|ch| ch == ' ');
                        let n = if in_indent { (c - 1) % TAB_WIDTH + 1 } else { 1 };
                        (if in_indent { Pos::new(self.cursor.line, c - n) } else { self.left_of(self.cursor) }, self.cursor)
                    }
                    (None, false, true) => (self.cursor, self.word_right(self.cursor)),
                    (None, false, false) => (self.cursor, self.right_of(self.cursor)),
                };
                self.replace(a, b, "", false);
            }
            key::ENTER | key::KP_ENTER => {
                // The new line starts under the text of this one.
                let line = &self.lines[self.cursor.line];
                let indent: String = line.chars().take(self.cursor.col).take_while(|c| *c == ' ' || *c == '\t').collect();
                let mut text = String::from("\n");
                text.push_str(&indent);
                let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
                self.replace(a, b, &text, false);
            }
            key::TAB => {
                // To the next tab stop on screen (tabs before the cursor
                // take several cells).
                let (a, _) = self.selection().unwrap_or((self.cursor, self.cursor));
                let n = TAB_WIDTH - cells_before(&self.lines[a.line], a.col) % TAB_WIDTH;
                let spaces: String = core::iter::repeat_n(' ', n).collect();
                let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
                self.replace(a, b, &spaces, false);
            }
            key::A if ctrl => self.select_all(),
            key::C if ctrl => {
                if let Some(s) = self.selected_text() {
                    clipboard.set(&s);
                }
                return Response::Consumed;
            }
            key::X if ctrl => {
                if let (Some(s), Some((a, b))) = (self.selected_text(), self.selection()) {
                    clipboard.set(&s);
                    self.replace(a, b, "", false);
                }
            }
            key::V if ctrl => {
                if let Some(s) = clipboard.text().map(String::from) {
                    self.insert(&s);
                }
            }
            key::Z if ctrl && shift => {
                self.redo();
            }
            key::Z if ctrl => {
                self.undo();
            }
            key::Y if ctrl => {
                self.redo();
            }
            _ => match k.insertable() {
                Some(c) => {
                    let mut buf = [0u8; 4];
                    self.insert(c.encode_utf8(&mut buf));
                }
                None => return Response::Ignored,
            },
        }
        if self.revision != before.2 {
            Response::Changed
        } else if (self.cursor, self.anchor) != (before.0, before.1) {
            Response::Redraw
        } else {
            Response::Consumed
        }
    }

    /// Handles an event. `Changed`: the text changed; `Redraw`: the cursor,
    /// selection or scroll position did.
    pub fn event(&mut self, ev: &Event, fonts: &Fonts, clipboard: &mut Clipboard) -> Response {
        let lh = fonts.line_height(FontKind::Mono).max(1);
        let sb = self.scroll.event(ev, self.track(), self.rect);
        if sb.handled() {
            return sb;
        }
        match *ev {
            Event::Key(k) => {
                let page = (self.rect.h / lh).max(2) as i64 - 1;
                let r = self.key(&k, clipboard, page);
                if r.needs_redraw() {
                    self.reveal_cursor(fonts);
                }
                r
            }
            Event::PointerDown { x, y, button: button::LEFT, clicks } if contains(&self.rect, x, y) => {
                let p = self.pos_at(x, y, fonts);
                match clicks {
                    1 => {
                        self.cursor = p;
                        self.anchor = Some(p);
                        self.wanted_col = p.col;
                        self.dragging = true;
                    }
                    2 => {
                        let (a, b) = self.word_around(p);
                        self.anchor = Some(a);
                        self.cursor = b;
                    }
                    _ => {
                        self.anchor = Some(Pos::new(p.line, 0));
                        self.cursor = self.clamp(Pos::new(p.line, usize::MAX));
                    }
                }
                Response::Redraw
            }
            Event::PointerMove { x, y } if self.dragging => {
                let p = self.pos_at(x, y, fonts);
                if p == self.cursor {
                    return Response::Consumed;
                }
                self.cursor = p;
                self.wanted_col = p.col;
                self.reveal_cursor(fonts);
                Response::Redraw
            }
            Event::PointerUp { button: button::LEFT, .. } if self.dragging => {
                self.dragging = false;
                if self.anchor == Some(self.cursor) {
                    self.anchor = None;
                }
                Response::Redraw
            }
            _ => Response::Ignored,
        }
    }

    // ----- drawing -----------------------------------------------------

    /// Draws the lines in view. `focused`: show the cursor.
    pub fn draw(&self, p: &mut Painter, focused: bool) {
        let t = *p.theme;
        let fonts = *p.fonts;
        let (lh, cw) = (fonts.line_height(FontKind::Mono) as i32, fonts.mono.cell_width() as i32);
        let old = p.push_clip(self.rect);
        p.fill(self.rect, t.field);
        let gutter = self.gutter(&fonts);
        if gutter > 0 {
            p.fill(Rect::new(self.rect.x, self.rect.y, gutter, self.rect.h), t.surface);
            p.vline(self.rect.x + gutter as i32, self.rect.y, self.rect.h, t.border);
        }
        let area = self.text_rect(&fonts);
        let first = (self.scroll.offset / lh as u32) as usize;
        let last = (first + (self.rect.h / lh as u32) as usize + 2).min(self.lines.len());
        let selection = self.selection();
        let cols = self.visible_cols(&fonts) + 1;
        let fold = !self.needle.chars().any(char::is_uppercase);
        for i in first..last {
            let y = self.rect.y + (i as i32) * lh - self.scroll.offset as i32;
            let line = &self.lines[i];
            if gutter > 0 {
                let c = if i == self.cursor.line { t.text_dim } else { t.text_faint };
                let n = alloc::format!("{}", i + 1);
                let x = self.rect.x + gutter as i32 - cw - fonts.mono.measure(&n) as i32;
                p.text(FontKind::Mono, x, y, &n, c);
            }
            let text_clip = p.push_clip(area);
            if i == self.cursor.line && selection.is_none() {
                p.blend(Rect::new(area.x - PAD, y, area.w + PAD as u32, lh as u32), t.raised, 120);
            }
            // Screen cell of every column (tabs), relative to the scroll.
            let cell_x = |col: usize| area.x + (cells_before(line, col) as i32 - self.scroll_x as i32) * cw;
            let len = char_len(line);
            // Search matches, then the selection over them.
            if !self.needle.is_empty() {
                let n = char_len(&self.needle);
                // Only matches on screen get their place worked out.
                let on_screen = (col_covering(line, self.scroll_x), col_covering(line, self.scroll_x + cols + 1) + 1);
                for col in match_cols(line, &self.needle, fold).into_iter().filter(|c| c + n > on_screen.0 && *c < on_screen.1) {
                    p.blend(Rect::new(cell_x(col), y, (cell_x(col + n) - cell_x(col)).max(0) as u32, lh as u32), t.warning, 70);
                }
            }
            if let Some((a, b)) = selection {
                if a.line <= i && i <= b.line {
                    let from = if a.line == i { a.col } else { 0 };
                    let to = if b.line == i { b.col } else { len };
                    // A selected line end shows as one more cell.
                    let extra = if b.line > i { cw } else { 0 };
                    let x0 = cell_x(from);
                    p.fill(Rect::new(x0, y, (cell_x(to) + extra - x0).max(0) as u32, lh as u32), t.selection);
                }
            }
            // The columns on screen (a tab that starts left of it counts).
            let visible = (col_covering(line, self.scroll_x), col_covering(line, self.scroll_x + cols + 1) + 1);
            // The text, one run per style.
            let spans = self.highlighter.map(|h| h(line)).unwrap_or_default();
            let mut col = 0;
            let draw = |p: &mut Painter, from: usize, to: usize, style: Style| {
                if to <= from {
                    return;
                }
                // Only what is on screen: a line of a megabyte is drawn
                // as fast as a short one.
                let (from, to) = (from.max(visible.0), to.min(visible.1));
                if to <= from {
                    return;
                }
                let part: String = line.chars().skip(from).take(to - from).collect();
                if part.contains('\t') {
                    // Tabs are gaps: draw what lies between them.
                    let mut c = from;
                    for piece in part.split('\t') {
                        let n = char_len(piece);
                        p.text(FontKind::Mono, cell_x(c), y, piece, style.color(&t));
                        c += n + 1;
                    }
                } else {
                    p.text(FontKind::Mono, cell_x(from), y, &part, style.color(&t));
                }
            };
            for (from, to, style) in spans {
                let (from, to) = (from.min(len), to.min(len));
                if from < col {
                    continue;
                }
                draw(p, col, from, Style::Plain);
                draw(p, from, to, style);
                col = to;
            }
            draw(p, col, len, Style::Plain);
            if focused && i == self.cursor.line {
                p.fill(Rect::new(cell_x(self.cursor.col), y, 2, lh as u32), t.accent);
            }
            p.restore_clip(text_clip);
        }
        self.scroll.draw(p, self.track());
        p.restore_clip(old);
    }
}
