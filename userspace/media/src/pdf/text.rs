//! The text of a page with the place of every character, for searching
//! and for selecting with the pointer.
//!
//! Characters come in the order the page draws them, which is reading
//! order in nearly every file. A space is added where the page leaves a
//! gap between glyphs without drawing one, and a line break where the
//! text moves to another line.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

/// A glyph as the renderer met it: its text and the corners of its box
/// (origin, end of advance, both raised to the ascent) in page points, y
/// down.
pub struct RawGlyph {
    pub text: String,
    pub corners: [(f64, f64); 4],
    /// From a font written top to bottom.
    pub vertical: bool,
}

/// One character and its box in points from the page's top left corner,
/// as the page is displayed (rotation applied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextChar {
    pub ch: char,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl TextChar {
    fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    fn middle(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

#[derive(Default)]
pub struct PageText {
    pub chars: Vec<TextChar>,
}

/// Lower case for comparing, one character for one.
fn fold(c: char) -> char {
    if c == '\n' || c == '\t' || c == '\u{a0}' {
        return ' ';
    }
    c.to_lowercase().next().unwrap_or(c)
}

impl PageText {
    pub fn from_glyphs(glyphs: &[RawGlyph]) -> PageText {
        let mut chars: Vec<TextChar> = Vec::new();
        let mut prev_vertical = false;
        for g in glyphs {
            let xs = g.corners.map(|c| c.0 as f32);
            let ys = g.corners.map(|c| c.1 as f32);
            let min = |v: [f32; 4]| v.iter().copied().fold(f32::INFINITY, f32::min);
            let max = |v: [f32; 4]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let (x0, y0, x1, y1) = (min(xs), min(ys), max(xs), max(ys));
            if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
                continue;
            }
            let was_vertical = core::mem::replace(&mut prev_vertical, g.vertical);
            if let (true, true, Some(prev)) = (g.vertical, was_vertical, chars.last().copied()) {
                // Vertical writing: a line is a column. The next glyph is
                // below in the same column, or the column has ended.
                let w = (prev.x1 - prev.x0).max(x1 - x0).max(1.0);
                let same_column = ((prev.x0 + prev.x1) / 2.0 - (x0 + x1) / 2.0).abs() < w * 0.5 && y0 > prev.y0 - w;
                if !same_column && prev.ch != '\n' {
                    chars.push(TextChar { ch: '\n', x0: prev.x0, y0: prev.y1, x1: prev.x1, y1: prev.y1 });
                }
            } else if let Some(prev) = chars.last().copied() {
                let h = prev.height().max(y1 - y0).max(1.0);
                let same_line = (prev.middle() - (y0 + y1) / 2.0).abs() < h * 0.5;
                let space = |c: char| c == ' ' || c == '\n';
                let first = g.text.chars().next().unwrap_or(' ');
                if !same_line || x0 < prev.x0 - h {
                    // Another line (or back to the start of this one).
                    if prev.ch != '\n' {
                        chars.push(TextChar { ch: '\n', x0: prev.x1, y0: prev.y0, x1: prev.x1, y1: prev.y1 });
                    }
                } else if x0 - prev.x1 > h * 0.18 && !space(prev.ch) && !space(first) {
                    // A gap the page did not fill with a space glyph.
                    chars.push(TextChar { ch: ' ', x0: prev.x1, y0: prev.y0.min(y0), x1: x0, y1: prev.y1.max(y1) });
                }
            }
            // A ligature's characters share its box.
            for ch in g.text.chars() {
                chars.push(TextChar { ch, x0, y0, x1, y1 });
            }
        }
        PageText { chars }
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The text of characters `range`.
    pub fn string(&self, range: Range<usize>) -> String {
        self.chars[range.start.min(self.chars.len())..range.end.min(self.chars.len())].iter().map(|c| c.ch).collect()
    }

    /// Every place `needle` occurs, ignoring case; a space in the needle
    /// matches a space or a line break in the text.
    pub fn find(&self, needle: &str) -> Vec<Range<usize>> {
        let needle: Vec<char> = needle.trim().chars().map(fold).collect();
        let mut out = Vec::new();
        if needle.is_empty() || needle.len() > self.chars.len() {
            return out;
        }
        let text: Vec<char> = self.chars.iter().map(|c| fold(c.ch)).collect();
        let mut i = 0;
        while i + needle.len() <= text.len() {
            // Every place, also one that starts inside the match before.
            if text[i..i + needle.len()] == needle[..] {
                out.push(i..i + needle.len());
            }
            i += 1;
        }
        out
    }

    /// Boxes covering characters `range`, one per line, as
    /// `[x0, y0, x1, y1]`.
    pub fn rects(&self, range: Range<usize>) -> Vec<[f32; 4]> {
        let mut out: Vec<[f32; 4]> = Vec::new();
        let end = range.end.min(self.chars.len());
        let mut new_line = true;
        for c in &self.chars[range.start.min(end)..end] {
            if c.ch == '\n' {
                new_line = true;
                continue;
            }
            match out.last_mut() {
                Some(r) if !new_line && (c.middle() - (r[1] + r[3]) / 2.0).abs() < (r[3] - r[1]).max(1.0) * 0.5 => {
                    r[0] = r[0].min(c.x0);
                    r[1] = r[1].min(c.y0);
                    r[2] = r[2].max(c.x1);
                    r[3] = r[3].max(c.y1);
                }
                _ => out.push([c.x0, c.y0, c.x1, c.y1]),
            }
            new_line = false;
        }
        out
    }

    /// The character at (`x`, `y`), or the nearest one: on the nearest
    /// line first, then along it.
    pub fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (i, c) in self.chars.iter().enumerate() {
            if c.ch == '\n' {
                continue;
            }
            let dx = if x < c.x0 { c.x0 - x } else if x > c.x1 { x - c.x1 } else { 0.0 };
            let dy = if y < c.y0 { c.y0 - y } else if y > c.y1 { y - c.y1 } else { 0.0 };
            let d = dx + dy * 8.0;
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, i));
            }
        }
        best.map(|(_, i)| i)
    }

    /// The word around character `i`.
    pub fn word(&self, i: usize) -> Range<usize> {
        let part = |c: &TextChar| c.ch.is_alphanumeric() || c.ch == '_';
        if i >= self.chars.len() || !part(&self.chars[i]) {
            return i..(i + 1).min(self.chars.len());
        }
        let start = (0..i).rev().find(|&k| !part(&self.chars[k])).map_or(0, |k| k + 1);
        let end = (i..self.chars.len()).find(|&k| !part(&self.chars[k])).unwrap_or(self.chars.len());
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    /// Glyphs 6 wide and 10 high placed at (x, y of the top).
    fn glyphs(items: &[(&str, f64, f64)]) -> Vec<RawGlyph> {
        items
            .iter()
            .map(|&(t, x, y)| RawGlyph { text: t.to_string(), corners: [(x, y + 10.0), (x + 6.0, y + 10.0), (x + 6.0, y), (x, y)], vertical: false })
            .collect()
    }

    #[test]
    fn gaps_become_spaces_and_new_lines_line_breaks() {
        let t = PageText::from_glyphs(&glyphs(&[
            ("H", 0.0, 0.0),
            ("i", 6.0, 0.0),
            // A gap without a space glyph.
            ("y", 20.0, 0.0),
            ("o", 26.0, 0.0),
            ("u", 32.0, 0.0),
            // The next line.
            ("ﬁ", 0.0, 14.0),
            ("n", 6.0, 14.0),
            ("e", 12.0, 14.0),
        ]));
        // The ligature keeps its one character here; a ToUnicode map
        // would have given two.
        assert_eq!(t.string(0..t.chars.len()), "Hi you\nﬁne");
        assert_eq!(t.chars[2], TextChar { ch: ' ', x0: 12.0, y0: 0.0, x1: 20.0, y1: 10.0 });
    }

    #[test]
    fn vertical_writing_reads_down_its_columns() {
        // Two columns of three glyphs, the right one first.
        let mut g = Vec::new();
        for (col, word) in ["縦書き", "です。"].iter().enumerate() {
            for (row, c) in word.chars().enumerate() {
                let (x, y) = (100.0 - col as f64 * 14.0, row as f64 * 12.0);
                g.push(RawGlyph { text: c.to_string(), corners: [(x, y + 12.0), (x + 12.0, y + 12.0), (x + 12.0, y), (x, y)], vertical: true });
            }
        }
        let t = PageText::from_glyphs(&g);
        assert_eq!(t.string(0..t.chars.len()), "縦書き\nです。");
        assert_eq!(t.find("書き").len(), 1, "consecutive characters of a column are found");
    }

    #[test]
    fn finding_ignores_case_and_line_breaks() {
        let mut items = Vec::new();
        for (i, c) in "Light Theme".chars().enumerate() {
            let s = c.to_string();
            items.push((s, i as f64 * 6.0, 0.0));
        }
        for (i, c) in "and a light".chars().enumerate() {
            items.push((c.to_string(), i as f64 * 6.0, 14.0));
        }
        for (i, c) in "theme".chars().enumerate() {
            items.push((c.to_string(), i as f64 * 6.0, 28.0));
        }
        let refs: Vec<(&str, f64, f64)> = items.iter().map(|(s, x, y)| (s.as_str(), *x, *y)).collect();
        let t = PageText::from_glyphs(&glyphs(&refs));
        assert_eq!(t.string(0..t.chars.len()), "Light Theme\nand a light\ntheme");
        let found = t.find("light theme");
        assert_eq!(found, vec![0..11, 18..29]);
        // The second match is on two lines: two boxes.
        assert_eq!(t.rects(found[0].clone()), vec![[0.0, 0.0, 66.0, 10.0]]);
        assert_eq!(t.rects(found[1].clone()), vec![[36.0, 14.0, 66.0, 24.0], [0.0, 28.0, 30.0, 38.0]]);
        assert!(t.find("").is_empty());
        // Matches may overlap: "ana" is twice in "banana".
        let letters: Vec<String> = "banana".chars().map(|c| c.to_string()).collect();
        let items: Vec<(&str, f64, f64)> = letters.iter().enumerate().map(|(i, s)| (s.as_str(), i as f64 * 6.0, 0.0)).collect();
        assert_eq!(PageText::from_glyphs(&glyphs(&items)).find("ana"), vec![1..4, 3..6]);
        assert!(t.find("absent").is_empty());
        assert_eq!(t.find("THEME").len(), 2);
    }

    #[test]
    fn the_pointer_finds_characters_and_words() {
        let mut items = Vec::new();
        for (i, c) in "one two_2".chars().enumerate() {
            items.push((c.to_string(), i as f64 * 6.0, 0.0));
        }
        for (i, c) in "three".chars().enumerate() {
            items.push((c.to_string(), i as f64 * 6.0, 40.0));
        }
        let refs: Vec<(&str, f64, f64)> = items.iter().map(|(s, x, y)| (s.as_str(), *x, *y)).collect();
        let t = PageText::from_glyphs(&glyphs(&refs));
        assert_eq!(t.nearest(8.0, 5.0), Some(1));
        // Right of the first line: its last character, not the line below.
        assert_eq!(t.nearest(500.0, 12.0), Some(8));
        // Between the lines, nearer the second.
        assert_eq!(t.nearest(3.0, 35.0), Some(10));
        assert_eq!(t.string(t.word(5)), "two_2");
        assert_eq!(t.string(t.word(3)), " ");
        assert_eq!(t.string(t.word(11)), "three");
        assert!(PageText::default().nearest(0.0, 0.0).is_none());
    }
}
