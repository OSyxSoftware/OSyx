//! PDF fonts (ISO 32000-1, 9.5 to 9.10): simple fonts (Type 1, TrueType,
//! Type 3) with their encodings and widths, composite fonts (Type 0 with
//! CIDFontType0 and CIDFontType2, Identity and embedded CMaps), embedded
//! programs (TrueType and OpenType through ttf-parser, bare CFF, Type 1 by
//! our own parser), and system fonts for the standard 14 and other fonts
//! a file does not embed.

use super::document::Document;
use super::encoding::{GLYPH_LIST, MAC_ROMAN, STANDARD, WIN_ANSI};
use super::object::{key, Dict, Object, Stream};
use super::raster::{Path, Seg};
use super::type1::Type1;
#[allow(unused_imports)]
use super::Float;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use ttf_parser::{GlyphId, OutlineBuilder};

/// Loads a system font file by name (e.g. `Lato-Regular.ttf`).
pub type FontSource<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

/// The Unicode value of a glyph name (Adobe Glyph List, `uniXXXX`,
/// `uXXXX[XX]`).
pub fn glyph_unicode(name: &str) -> Option<char> {
    let base = name.split('.').next().unwrap_or(name);
    if let Ok(i) = GLYPH_LIST.binary_search_by(|(n, _)| (*n).cmp(base)) {
        return char::from_u32(GLYPH_LIST[i].1 as u32);
    }
    let hex = base
        .strip_prefix("uni")
        .filter(|h| h.len() >= 4)
        .map(|h| &h[..4])
        .or_else(|| base.strip_prefix('u').filter(|h| (4..=6).contains(&h.len())));
    if let Some(h) = hex {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            return char::from_u32(v);
        }
    }
    // TeX-style "gNN"/"cidNN" names carry no Unicode.
    None
}

enum Program {
    /// TrueType or OpenType (sfnt) data.
    Sfnt(Vec<u8>),
    /// A bare CFF font (FontFile3/Type1C or CIDFontType0C).
    Cff(Vec<u8>),
    Type1(Type1),
    None,
}

/// How codes become glyphs.
enum Lookup {
    /// Simple font: a glyph name per code (empty for none).
    Names(Vec<String>),
    /// Composite font: codes to CIDs, CIDs to glyph ids.
    Cid { cmap: CMap, cid_to_gid: Option<Vec<u16>> },
}

pub struct Font {
    program: Program,
    lookup: Lookup,
    /// Whether a simple TrueType font is flagged symbolic.
    symbolic: bool,
    /// Widths in thousandths of text space, by code (simple) or CID.
    widths: BTreeMap<u32, f64>,
    default_width: f64,
    /// Glyph space to text space.
    pub matrix: [f64; 6],
    pub vertical: bool,
    /// Type 3: glyph procedures and resources.
    pub type3: Option<(BTreeMap<String, Rc<Stream>>, Dict)>,
    cache: RefCell<BTreeMap<u32, Option<Rc<Path>>>>,
    /// CFF CID to glyph id, built on first use.
    cff_cids: RefCell<Option<BTreeMap<u16, u16>>>,
    /// Space between words (code 32 in one-byte encodings) gets Tw.
    pub single_byte: bool,
    pub name: String,
    /// A system font stands in for one the file does not embed: glyphs
    /// are scaled to the widths the file expects.
    substituted: bool,
}

/// Codespace ranges and code to CID mappings of a CMap.
#[derive(Default)]
pub struct CMap {
    /// (low, high, bytes).
    space: Vec<(u32, u32, usize)>,
    /// (low, high, first CID).
    ranges: Vec<(u32, u32, u32)>,
    identity: bool,
}

impl CMap {
    fn identity() -> CMap {
        CMap { space: alloc::vec![(0, 0xFFFF, 2)], ranges: Vec::new(), identity: true }
    }

    fn parse(data: &[u8]) -> CMap {
        let mut m = CMap::default();
        let text = data;
        let hexes = |s: &[u8]| -> Vec<(u32, usize)> {
            let mut out = Vec::new();
            let mut i = 0;
            while i < s.len() {
                if s[i] == b'<' {
                    let end = s[i..].iter().position(|&c| c == b'>').map_or(s.len(), |e| i + e);
                    let h: Vec<u8> = s[i + 1..end].iter().copied().filter(|c| c.is_ascii_hexdigit()).collect();
                    let v = u32::from_str_radix(core::str::from_utf8(&h).unwrap_or("0"), 16).unwrap_or(0);
                    out.push((v, h.len() / 2));
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            out
        };
        let ints = |s: &[u8]| -> Vec<u32> {
            core::str::from_utf8(s)
                .unwrap_or("")
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|t| t.parse().ok())
                .collect()
        };
        let sections = |begin: &[u8], end: &[u8], f: &mut dyn FnMut(&[u8])| {
            let mut p = 0;
            while let Some(b) = text[p..].windows(begin.len()).position(|w| w == begin) {
                let s = p + b + begin.len();
                let e = text[s..].windows(end.len()).position(|w| w == end).map_or(text.len(), |e| s + e);
                f(&text[s..e]);
                p = e;
            }
        };
        sections(b"begincodespacerange", b"endcodespacerange", &mut |s| {
            let h = hexes(s);
            for pair in h.chunks(2) {
                if pair.len() == 2 {
                    m.space.push((pair[0].0, pair[1].0, pair[0].1.max(1)));
                }
            }
        });
        // "<lo> <hi> cid" triples and "<code> cid" pairs, however the lines
        // are broken.
        let tokens = |s: &[u8]| -> Vec<Result<(u32, usize), u32>> {
            let mut out = Vec::new();
            let mut i = 0;
            while i < s.len() {
                if s[i] == b'<' {
                    let end = s[i..].iter().position(|&c| c == b'>').map_or(s.len(), |e| i + e);
                    out.extend(hexes(&s[i..(end + 1).min(s.len())]).into_iter().map(Ok));
                    i = end + 1;
                } else if s[i].is_ascii_digit() {
                    let st = i;
                    while i < s.len() && s[i].is_ascii_digit() {
                        i += 1;
                    }
                    out.extend(ints(&s[st..i]).into_iter().map(Err));
                } else {
                    i += 1;
                }
            }
            out
        };
        sections(b"begincidrange", b"endcidrange", &mut |s| {
            for t in tokens(s).chunks_exact(3) {
                if let (Ok(lo), Ok(hi), Err(cid)) = (t[0], t[1], t[2]) {
                    m.ranges.push((lo.0, hi.0, cid));
                }
            }
        });
        sections(b"begincidchar", b"endcidchar", &mut |s| {
            for t in tokens(s).chunks_exact(2) {
                if let (Ok(c), Err(cid)) = (t[0], t[1]) {
                    m.ranges.push((c.0, c.0, cid));
                }
            }
        });
        if m.space.is_empty() {
            m.space.push((0, 0xFFFF, 2));
        }
        m
    }

    /// Splits `s` into (code, CID) pairs using the codespace ranges.
    fn decode(&self, s: &[u8]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let mut matched = None;
            for n in 1..=4usize {
                if i + n > s.len() {
                    break;
                }
                let code = s[i..i + n].iter().fold(0u32, |a, &b| a << 8 | b as u32);
                if self.space.iter().any(|&(lo, hi, b)| b == n && code >= lo && code <= hi) {
                    matched = Some((code, n));
                    break;
                }
            }
            let (code, n) = matched.unwrap_or_else(|| {
                let n = self.space.first().map_or(1, |s| s.2).min(s.len() - i);
                (s[i..i + n].iter().fold(0u32, |a, &b| a << 8 | b as u32), n)
            });
            let cid = if self.identity {
                code
            } else {
                self.ranges.iter().find(|r| code >= r.0 && code <= r.1).map_or(0, |r| r.2 + (code - r.0))
            };
            out.push((code, cid));
            i += n;
        }
        out
    }
}

struct PathOut {
    path: Path,
    scale: f64,
    last: (f64, f64),
}

impl OutlineBuilder for PathOut {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = (x as f64 * self.scale, y as f64 * self.scale);
        self.path.segs.push(Seg::Move(p.0, p.1));
        self.last = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = (x as f64 * self.scale, y as f64 * self.scale);
        self.path.segs.push(Seg::Line(p.0, p.1));
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (q, p) = ((x1 as f64 * self.scale, y1 as f64 * self.scale), (x as f64 * self.scale, y as f64 * self.scale));
        let c1 = (self.last.0 + 2.0 / 3.0 * (q.0 - self.last.0), self.last.1 + 2.0 / 3.0 * (q.1 - self.last.1));
        let c2 = (p.0 + 2.0 / 3.0 * (q.0 - p.0), p.1 + 2.0 / 3.0 * (q.1 - p.1));
        self.path.segs.push(Seg::Cubic(c1.0, c1.1, c2.0, c2.1, p.0, p.1));
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.path.segs.push(Seg::Cubic(
            x1 as f64 * s,
            y1 as f64 * s,
            x2 as f64 * s,
            y2 as f64 * s,
            x as f64 * s,
            y as f64 * s,
        ));
        self.last = (x as f64 * s, y as f64 * s);
    }
    fn close(&mut self) {
        self.path.segs.push(Seg::Close);
    }
}

/// Widths by glyph name of a standard 14 font (or its common aliases),
/// for files that leave out /Widths. `None` for other fonts.
fn standard_widths(base: &str) -> Option<&'static [(&'static str, u16)]> {
    use super::metrics::*;
    let b = base.replace([' ', ','], "").to_ascii_lowercase();
    let bold = b.contains("bold");
    let italic = b.contains("italic") || b.contains("oblique");
    if b.starts_with("courier") {
        return Some(&[]);
    }
    if b.starts_with("helvetica") || b.starts_with("arial") {
        return Some(if bold { &HELVETICA_BOLD } else { &HELVETICA });
    }
    if b.starts_with("times") {
        return Some(match (bold, italic) {
            (true, true) => &TIMES_BOLDITALIC,
            (true, false) => &TIMES_BOLD,
            (false, true) => &TIMES_ITALIC,
            (false, false) => &TIMES_ROMAN,
        });
    }
    if b == "symbol" {
        return Some(&SYMBOL);
    }
    if b == "zapfdingbats" {
        return Some(&ZAPFDINGBATS);
    }
    None
}

/// The system font standing in for `base` (a PostScript font name).
fn substitute(base: &str, flags: i64) -> &'static str {
    let b = base.to_ascii_lowercase();
    let bold = b.contains("bold") || b.contains("black") || b.contains("heavy") || b.contains("semibold");
    let italic = b.contains("italic") || b.contains("oblique") || flags & 64 != 0;
    let mono = b.contains("courier") || b.contains("mono") || b.contains("consol") || flags & 1 != 0;
    let serif = b.contains("times")
        || b.contains("serif") && !b.contains("sans")
        || b.contains("georgia")
        || b.contains("garamond")
        || b.contains("cambria")
        || b.contains("roman")
        || b.contains("cmr")
        || b.contains("palatino")
        || b.contains("book")
        || (flags & 2 != 0 && !b.contains("arial") && !b.contains("helvetica"));
    if mono {
        return "KaTeX_Typewriter-Regular.ttf";
    }
    match (serif, bold, italic) {
        (true, true, true) => "KaTeX_Main-BoldItalic.ttf",
        (true, true, false) => "KaTeX_Main-Bold.ttf",
        (true, false, true) => "KaTeX_Main-Italic.ttf",
        (true, false, false) => "KaTeX_Main-Regular.ttf",
        (false, true, true) => "Lato-BoldItalic.ttf",
        (false, true, false) => "Lato-Bold.ttf",
        (false, false, true) => "Lato-Italic.ttf",
        (false, false, false) => "Lato-Regular.ttf",
    }
}

fn base_encoding(name: &[u8]) -> Option<&'static [&'static str; 256]> {
    match name {
        b"WinAnsiEncoding" => Some(&WIN_ANSI),
        b"MacRomanEncoding" | b"MacExpertEncoding" => Some(&MAC_ROMAN),
        b"StandardEncoding" => Some(&STANDARD),
        _ => None,
    }
}

fn numbers(doc: &Document, o: &Object) -> Vec<f64> {
    doc.resolve(o).as_array().unwrap_or(&[]).iter().map(|x| doc.resolve(x).as_f64().unwrap_or(0.0)).collect()
}

impl Font {
    pub fn load(doc: &Document, d: &Dict, fonts: FontSource) -> Font {
        let subtype = doc.get_key(d, "Subtype").as_name().map(|n| n.to_vec()).unwrap_or_default();
        let name =
            doc.get_key(d, "BaseFont").as_name().map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
        // Subset prefixes ("ABCDEF+Name") do not matter for substitution.
        let plain = name.split_once('+').map_or(name.as_str(), |(_, n)| n).to_string();
        if subtype == b"Type0" {
            return Font::load_type0(doc, d, &plain, fonts);
        }
        let desc = doc.get_key(d, "FontDescriptor");
        let desc = desc.as_dict();
        let flags = desc.map_or(0, |fd| doc.get_key(fd, "Flags").as_i64().unwrap_or(0));
        let symbolic = flags & 4 != 0;
        let program = desc.map_or(Program::None, |fd| Font::program(doc, fd));
        // Glyph names per code: the font's built-in encoding, then the
        // base encoding, then the differences.
        let enc = doc.get_key(d, "Encoding");
        let builtin: Option<Vec<String>> = match &program {
            Program::Type1(t) if !t.encoding.is_empty() => {
                Some((0..256).map(|c| t.encoding.get(&(c as u8)).cloned().unwrap_or_default()).collect())
            }
            _ => None,
        };
        let default_base: &[&str; 256] = if subtype == b"TrueType" || (matches!(program, Program::None) && !symbolic) {
            &WIN_ANSI
        } else {
            &STANDARD
        };
        let mut names: Vec<String> = match (&builtin, enc.as_name(), enc.as_dict()) {
            (_, Some(n), _) if base_encoding(n).is_some() => {
                base_encoding(n).unwrap().iter().map(|s| s.to_string()).collect()
            }
            (_, _, Some(ed)) if doc.get_key(ed, "BaseEncoding").as_name().and_then(base_encoding).is_some() => doc
                .get_key(ed, "BaseEncoding")
                .as_name()
                .and_then(base_encoding)
                .unwrap()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            (Some(b), _, _) => b.clone(),
            _ if matches!(program, Program::Cff(_)) || symbolic && !matches!(program, Program::None) => {
                alloc::vec![String::new(); 256]
            }
            _ => default_base.iter().map(|s| s.to_string()).collect(),
        };
        if let Some(ed) = enc.as_dict() {
            let diffs = doc.get_key(ed, "Differences");
            let mut code = 0usize;
            for o in diffs.as_array().unwrap_or(&[]) {
                match doc.resolve(o) {
                    Object::Int(c) => code = c.clamp(0, 255) as usize,
                    Object::Name(n) => {
                        if code < 256 {
                            names[code] = String::from_utf8_lossy(&n).into_owned();
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
        let mut widths = BTreeMap::new();
        let first = doc.get_key(d, "FirstChar").as_i64().unwrap_or(0).max(0) as u32;
        for (i, w) in numbers(doc, d.get(&b"Widths"[..]).unwrap_or(&Object::Null)).into_iter().enumerate() {
            widths.insert(first + i as u32, w);
        }
        let missing = desc.map_or(0.0, |fd| doc.get_key(fd, "MissingWidth").as_f64().unwrap_or(0.0));
        let mut matrix = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        let mut type3 = None;
        let mut program = program;
        let mut substituted = false;
        if subtype == b"Type3" {
            let m = numbers(doc, d.get(&b"FontMatrix"[..]).unwrap_or(&Object::Null));
            if m.len() == 6 {
                matrix.copy_from_slice(&m);
            }
            let mut procs = BTreeMap::new();
            if let Some(cp) = doc.get_key(d, "CharProcs").as_dict() {
                for (k, v) in cp {
                    if let Object::Stream(s) = doc.resolve(v) {
                        procs.insert(String::from_utf8_lossy(k).into_owned(), s);
                    }
                }
            }
            let res = doc.get_key(d, "Resources").as_dict().cloned().unwrap_or_default();
            type3 = Some((procs, res));
        } else if matches!(program, Program::None) {
            // Not embedded: a system font, with the standard metrics when the
            // file has no widths.
            if widths.is_empty() {
                if let Some(metrics) = standard_widths(&plain) {
                    for (code, n) in names.iter().enumerate() {
                        let w = if metrics.is_empty() {
                            600
                        } else {
                            match metrics.binary_search_by(|(m, _)| (*m).cmp(n.as_str())) {
                                Ok(i) => metrics[i].1,
                                Err(_) => continue,
                            }
                        };
                        if !n.is_empty() {
                            widths.insert(code as u32, w as f64);
                        }
                    }
                }
            }
            if let Some(data) = fonts(substitute(&plain, flags)) {
                program = Program::Sfnt(data);
                substituted = true;
            }
        }
        if let Program::Type1(t) = &program {
            matrix = t.matrix;
        }
        Font {
            program,
            lookup: Lookup::Names(names),
            symbolic,
            widths,
            default_width: missing,
            matrix,
            vertical: false,
            type3,
            cache: RefCell::new(BTreeMap::new()),
            cff_cids: RefCell::new(None),
            single_byte: true,
            name: plain,
            substituted,
        }
    }

    fn program(doc: &Document, fd: &Dict) -> Program {
        if let Object::Stream(s) = doc.get_key(fd, "FontFile2") {
            return Program::Sfnt(doc.stream_data(&s));
        }
        if let Object::Stream(s) = doc.get_key(fd, "FontFile3") {
            let data = doc.stream_data(&s);
            return match key(&s.dict, "Subtype").and_then(Object::as_name) {
                Some(b"OpenType") => Program::Sfnt(data),
                _ if data.starts_with(b"OTTO") || data.starts_with(&[0, 1, 0, 0]) => Program::Sfnt(data),
                _ => Program::Cff(data),
            };
        }
        if let Object::Stream(s) = doc.get_key(fd, "FontFile") {
            let data = doc.stream_data(&s);
            let len1 = key(&s.dict, "Length1").map(|o| doc.resolve(o)).and_then(|o| o.as_i64()).map(|l| l as usize);
            if let Some(t) = Type1::parse(&data, len1) {
                return Program::Type1(t);
            }
            // Some producers put CFF or TrueType data in FontFile.
            if data.starts_with(&[1, 0]) {
                return Program::Cff(data);
            }
        }
        Program::None
    }

    fn load_type0(doc: &Document, d: &Dict, name: &str, fonts: FontSource) -> Font {
        let enc = doc.get_key(d, "Encoding");
        let (cmap, vertical) = match &enc {
            Object::Name(n) => (CMap::identity(), n.ends_with(b"-V")),
            Object::Stream(s) => {
                let v = key(&s.dict, "WMode").and_then(Object::as_i64) == Some(1);
                (CMap::parse(&doc.stream_data(s)), v)
            }
            _ => (CMap::identity(), false),
        };
        let desc_font = doc.get_key(d, "DescendantFonts");
        let cid = desc_font.as_array().and_then(|a| a.first()).map(|o| doc.resolve(o)).unwrap_or(Object::Null);
        let cid = cid.as_dict().cloned().unwrap_or_default();
        let fd = doc.get_key(&cid, "FontDescriptor");
        let flags = fd.as_dict().map_or(0, |f| doc.get_key(f, "Flags").as_i64().unwrap_or(0));
        let mut program = fd.as_dict().map_or(Program::None, |f| Font::program(doc, f));
        let mut substituted = false;
        if matches!(program, Program::None) {
            if let Some(data) = fonts(substitute(name, flags)) {
                program = Program::Sfnt(data);
                substituted = true;
            }
        }
        let cid_to_gid = match doc.get_key(&cid, "CIDToGIDMap") {
            Object::Stream(s) => {
                Some(doc.stream_data(&s).chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
            }
            _ => None,
        };
        let mut widths = BTreeMap::new();
        let w = doc.get_key(&cid, "W");
        let w = w.as_array().unwrap_or(&[]);
        let mut i = 0;
        while i < w.len() {
            let first = doc.resolve(&w[i]).as_i64().unwrap_or(0).max(0) as u32;
            match w.get(i + 1).map(|o| doc.resolve(o)) {
                Some(Object::Array(list)) => {
                    for (k, v) in list.iter().enumerate() {
                        widths.insert(first + k as u32, doc.resolve(v).as_f64().unwrap_or(0.0));
                    }
                    i += 2;
                }
                Some(last) => {
                    let last = last.as_i64().unwrap_or(0).max(0) as u32;
                    let v = w.get(i + 2).map(|o| doc.resolve(o)).and_then(|o| o.as_f64()).unwrap_or(0.0);
                    for c in first..=last.min(first + 65535) {
                        widths.insert(c, v);
                    }
                    i += 3;
                }
                None => break,
            }
        }
        Font {
            program,
            lookup: Lookup::Cid { cmap, cid_to_gid },
            symbolic: false,
            widths,
            default_width: doc.get_key(&cid, "DW").as_f64().unwrap_or(1000.0),
            matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            vertical,
            type3: None,
            cache: RefCell::new(BTreeMap::new()),
            cff_cids: RefCell::new(None),
            single_byte: false,
            name: String::from(name),
            substituted,
        }
    }

    /// Splits a string operand into (code, glyph key) pairs; the key is the
    /// code for simple fonts and the CID for composite ones.
    pub fn decode(&self, s: &[u8]) -> Vec<(u32, u32)> {
        match &self.lookup {
            Lookup::Names(_) => s.iter().map(|&b| (b as u32, b as u32)).collect(),
            Lookup::Cid { cmap, .. } => cmap.decode(s),
        }
    }

    /// Advance of glyph `k` in thousandths of text space units.
    pub fn width(&self, k: u32) -> f64 {
        if let Some(&w) = self.widths.get(&k) {
            return w;
        }
        if self.widths.is_empty() {
            // No widths in the file (standard 14): the program's advance.
            if let Some(w) = self.program_advance(k) {
                return w;
            }
        }
        self.default_width
    }

    fn program_advance(&self, k: u32) -> Option<f64> {
        let Program::Sfnt(data) = &self.program else { return None };
        let face = ttf_parser::Face::parse(data, 0).ok()?;
        let gid = self.sfnt_gid(&face, k)?;
        Some(face.glyph_hor_advance(gid)? as f64 * 1000.0 / face.units_per_em() as f64)
    }

    fn name_of(&self, code: u32) -> &str {
        match &self.lookup {
            Lookup::Names(n) => n.get(code as usize).map_or("", |s| s.as_str()),
            _ => "",
        }
    }

    fn sfnt_gid(&self, face: &ttf_parser::Face, k: u32) -> Option<GlyphId> {
        match &self.lookup {
            Lookup::Cid { cid_to_gid, .. } => {
                let gid = match cid_to_gid {
                    Some(m) => *m.get(k as usize)?,
                    None if face.tables().cff.is_some() => {
                        return face.tables().cff.and_then(|c| self.cff_gid_for_cid(&c, k as u16)).map(GlyphId);
                    }
                    None => k as u16,
                };
                Some(GlyphId(gid))
            }
            Lookup::Names(_) => {
                let name = self.name_of(k);
                let cmap = face.tables().cmap?;
                let platform = |p: u16| match p {
                    0 => ttf_parser::PlatformId::Unicode,
                    1 => ttf_parser::PlatformId::Macintosh,
                    _ => ttf_parser::PlatformId::Windows,
                };
                let sub = |p: u16, e: u16| {
                    cmap.subtables.into_iter().find(|s| s.platform_id == platform(p) && s.encoding_id == e)
                };
                let ms_unicode = sub(3, 1).or_else(|| sub(0, 3)).or_else(|| sub(0, 4)).or_else(|| sub(3, 10));
                let ms_symbol = sub(3, 0);
                let mac = sub(1, 0);
                if !self.symbolic || ms_symbol.is_none() {
                    if let (Some(u), Some(c)) = (ms_unicode, glyph_unicode(name)) {
                        if let Some(g) = u.glyph_index(c as u32) {
                            return Some(g);
                        }
                    }
                }
                if let Some(s) = ms_symbol {
                    for base in [0xF000u32, 0, 0xF100, 0xF200] {
                        if let Some(g) = s.glyph_index(base + k) {
                            return Some(g);
                        }
                    }
                }
                if let Some(m) = mac {
                    let code = MAC_ROMAN.iter().position(|n| *n == name).map_or(k, |c| c as u32);
                    if let Some(g) = m.glyph_index(code) {
                        return Some(g);
                    }
                }
                if !name.is_empty() {
                    if let Some(g) = face.glyph_index_by_name(name) {
                        return Some(g);
                    }
                }
                if let (Some(u), None) = (ms_unicode, ms_symbol) {
                    if let Some(g) = u.glyph_index(k) {
                        return Some(g);
                    }
                }
                // Subset TrueType fonts without a usable cmap: codes are
                // glyph ids.
                (cmap.subtables.is_empty()).then_some(GlyphId(k as u16))
            }
        }
    }

    fn cff_gid_for_cid(&self, cff: &ttf_parser::cff::Table, cid: u16) -> Option<u16> {
        let mut cache = self.cff_cids.borrow_mut();
        let map = cache.get_or_insert_with(|| {
            let mut m = BTreeMap::new();
            for g in 0..cff.number_of_glyphs() {
                if let Some(c) = cff.glyph_cid(GlyphId(g)) {
                    m.insert(c, g);
                }
            }
            m
        });
        if map.is_empty() {
            return Some(cid);
        }
        map.get(&cid).copied()
    }

    /// The outline of glyph `k` in text space (FontMatrix applied).
    pub fn glyph(&self, k: u32) -> Option<Rc<Path>> {
        if let Some(p) = self.cache.borrow().get(&k) {
            return p.clone();
        }
        let mut p = self.load_glyph(k);
        if self.substituted {
            if let (Some(path), Some(&want), Some(have)) = (&mut p, self.widths.get(&k), self.program_advance(k)) {
                if have > 0.0 && want > 0.0 {
                    let f = (want / have).clamp(0.6, 1.6);
                    *path = path.transform(&super::raster::Matrix([f, 0.0, 0.0, 1.0, 0.0, 0.0]));
                }
            }
        }
        let p = p.map(Rc::new);
        self.cache.borrow_mut().insert(k, p.clone());
        p
    }

    fn load_glyph(&self, k: u32) -> Option<Path> {
        match &self.program {
            Program::Sfnt(data) => {
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let gid = self.sfnt_gid(&face, k)?;
                let mut out =
                    PathOut { path: Path::default(), scale: 1.0 / face.units_per_em() as f64, last: (0.0, 0.0) };
                face.outline_glyph(gid, &mut out)?;
                Some(out.path)
            }
            Program::Cff(data) => {
                let cff = ttf_parser::cff::Table::parse(data)?;
                let gid = match &self.lookup {
                    Lookup::Cid { .. } => GlyphId(self.cff_gid_for_cid(&cff, k as u16)?),
                    Lookup::Names(_) => {
                        let name = self.name_of(k);
                        if name.is_empty() {
                            cff.glyph_index(k as u8)?
                        } else {
                            cff.glyph_index_by_name(name).or_else(|| cff.glyph_index(k as u8))?
                        }
                    }
                };
                let m = cff.matrix();
                let mut out = PathOut { path: Path::default(), scale: 1.0, last: (0.0, 0.0) };
                cff.outline(gid, &mut out).ok()?;
                // CFF outlines are in the font matrix's units (usually 1/1000).
                let fm = super::raster::Matrix([
                    m.sx as f64,
                    m.ky as f64,
                    m.kx as f64,
                    m.sy as f64,
                    m.tx as f64,
                    m.ty as f64,
                ]);
                Some(out.path.transform(&fm))
            }
            Program::Type1(t) => {
                let mut name = self.name_of(k).to_string();
                if name.is_empty() || !t.has_glyph(&name) {
                    name = t.encoding.get(&(k as u8)).cloned().unwrap_or_default();
                }
                let p = t.outline(&name)?;
                let m = super::raster::Matrix(t.matrix);
                Some(p.transform(&m))
            }
            Program::None => None,
        }
    }

    /// The glyph name of a simple font's code (Type 3 procedures).
    pub fn code_name(&self, code: u32) -> String {
        self.name_of(code).to_string()
    }

    pub fn describe(&self) -> String {
        let kind = match &self.program {
            Program::Sfnt(_) => "sfnt",
            Program::Cff(_) => "cff",
            Program::Type1(_) => "type1",
            Program::None if self.type3.is_some() => "type3",
            Program::None => "none",
        };
        format!("{} ({kind})", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_unicode("A"), Some('A'));
        assert_eq!(glyph_unicode("Lslash"), Some('Ł'));
        assert_eq!(glyph_unicode("uni0105"), Some('ą'));
        assert_eq!(glyph_unicode("u1F600"), Some('😀'));
        assert_eq!(glyph_unicode("a.sc"), Some('a'));
        assert_eq!(glyph_unicode("fi"), Some('\u{FB01}'));
        assert_eq!(glyph_unicode("g123"), None);
    }

    #[test]
    fn cmaps() {
        let m = CMap::parse(b"1 begincodespacerange <00> <80> <8140> <FFFF> endcodespacerange 2 begincidrange <8140> <817E> 633 <20> <7E> 1 endcidrange");
        assert_eq!(m.decode(&[0x41, 0x81, 0x41]), [(0x41, 34), (0x8141, 634)]);
    }
}
