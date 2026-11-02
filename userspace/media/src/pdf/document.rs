//! A PDF file: cross-reference tables and streams (with incremental
//! updates and hybrid files), object streams, a rebuilt table when the
//! file's own is damaged, decryption, and the page tree with inherited
//! attributes.

use super::crypt::Decryptor;
use super::filter;
use super::object::{key, parse_from, parse_object, Dict, Lexer, Object, Ref, Stream, Token};
#[allow(unused_imports)]
use super::Float;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

#[derive(Clone, Copy, Debug)]
enum Entry {
    /// Byte offset of `n g obj`.
    Offset(usize),
    /// Inside object stream `stream` at `index`.
    InStream { stream: u32, index: usize },
}

pub struct Document {
    pub data: Vec<u8>,
    xref: BTreeMap<u32, Entry>,
    pub trailer: Dict,
    cache: RefCell<BTreeMap<u32, Object>>,
    /// Decoded object streams: (offsets in the stream, decoded data).
    objstms: RefCell<BTreeMap<u32, Rc<(Vec<(u32, usize)>, Vec<u8>)>>>,
    crypt: Option<Decryptor>,
    pub pages: Vec<Page>,
}

/// One page with its inherited attributes resolved.
#[derive(Clone, Debug)]
pub struct Page {
    pub dict: Dict,
    pub resources: Dict,
    /// The visible area (CropBox within MediaBox), in default user space.
    pub bbox: [f64; 4],
    /// Clockwise rotation in degrees (0, 90, 180, 270).
    pub rotate: i32,
}

fn find_last(data: &[u8], what: &[u8]) -> Option<usize> {
    data.windows(what.len()).rposition(|w| w == what)
}

impl Document {
    pub fn open(data: Vec<u8>) -> Result<Document, String> {
        if !data.windows(5).take(1024).any(|w| w == b"%PDF-") {
            return Err(String::from("not a PDF file"));
        }
        let mut doc = Document {
            data,
            xref: BTreeMap::new(),
            trailer: Dict::new(),
            cache: RefCell::new(BTreeMap::new()),
            objstms: RefCell::new(BTreeMap::new()),
            crypt: None,
            pages: Vec::new(),
        };
        let ok = doc.read_xref_chain();
        if !ok || doc.trailer.get(&b"Root"[..]).is_none() || doc.catalog().is_none() {
            doc.rebuild_xref();
        }
        if let Some(enc) = doc.trailer.get(&b"Encrypt"[..]).cloned() {
            let enc = doc.resolve(&enc);
            let id = doc
                .trailer
                .get(&b"ID"[..])
                .and_then(Object::as_array)
                .and_then(|a| a.first())
                .and_then(Object::as_str)
                .map(|s| s.to_vec())
                .unwrap_or_default();
            let d = enc.as_dict().ok_or("broken encryption dictionary")?;
            doc.crypt = Some(Decryptor::new(d, &id)?);
            // Objects read so far were not decrypted.
            doc.cache.borrow_mut().clear();
            doc.objstms.borrow_mut().clear();
        }
        doc.collect_pages();
        if doc.pages.is_empty() {
            return Err(String::from("the PDF file has no pages"));
        }
        Ok(doc)
    }

    fn catalog(&self) -> Option<Dict> {
        let root = self.trailer.get(&b"Root"[..])?.clone();
        self.resolve(&root).as_dict().cloned()
    }

    /// Reads the xref section at `startxref` and those it points back to.
    fn read_xref_chain(&mut self) -> bool {
        let Some(sx) = find_last(&self.data, b"startxref") else { return false };
        let mut lx = Lexer::at(&self.data, sx + 9);
        let Some(Token::Int(mut off)) = lx.next() else { return false };
        let mut seen = Vec::new();
        let mut first = true;
        while off > 0 && (off as usize) < self.data.len() && !seen.contains(&off) {
            seen.push(off);
            let Some(trailer) = self.read_xref_at(off as usize) else { return !first };
            if first {
                self.trailer = trailer.clone();
                first = false;
            }
            // Hybrid files: an xref stream for the same revision.
            if let Some(x) = key(&trailer, "XRefStm").and_then(Object::as_i64) {
                let _ = self.read_xref_at(x as usize);
            }
            off = key(&trailer, "Prev").and_then(Object::as_i64).unwrap_or(0);
        }
        !first
    }

    /// One xref section (table or stream); entries already known (newer)
    /// are kept. Returns its trailer dictionary.
    fn read_xref_at(&mut self, off: usize) -> Option<Dict> {
        let mut lx = Lexer::at(&self.data, off);
        lx.skip_white();
        if self.data[lx.pos..].starts_with(b"xref") {
            lx.pos += 4;
            loop {
                match lx.next()? {
                    Token::Int(start) => {
                        let Token::Int(count) = lx.next()? else { return None };
                        lx.skip_white();
                        for i in 0..count.max(0) as u32 {
                            // "oooooooooo ggggg n" lines, 20 bytes, often
                            // with sloppy whitespace: read tokens.
                            let Token::Int(o) = lx.next()? else { return None };
                            let Token::Int(_g) = lx.next()? else { return None };
                            let Token::Word(w) = lx.next()? else { return None };
                            let num = start as u32 + i;
                            if w == b"n" && o > 0 {
                                self.xref.entry(num).or_insert(Entry::Offset(o as usize));
                            }
                        }
                    }
                    Token::Word(w) if w == b"trailer" => {
                        return parse_object(&mut lx, 0).and_then(|o| o.as_dict().cloned());
                    }
                    _ => return None,
                }
            }
        }
        // An xref stream: "n g obj << /Type /XRef ... >> stream".
        let (obj, _) = self.parse_indirect_at(off)?;
        let Object::Stream(s) = obj else { return None };
        if key(&s.dict, "Type").and_then(Object::as_name) != Some(b"XRef") {
            return None;
        }
        let (data, _) = filter::decode(&s.data, &s.dict);
        let w: Vec<usize> =
            key(&s.dict, "W")?.as_array()?.iter().map(|o| o.as_i64().unwrap_or(0).max(0) as usize).collect();
        if w.len() < 3 {
            return None;
        }
        let size = key(&s.dict, "Size").and_then(Object::as_i64).unwrap_or(0);
        let index: Vec<i64> = match key(&s.dict, "Index").and_then(Object::as_array) {
            Some(a) => a.iter().filter_map(Object::as_i64).collect(),
            None => alloc::vec![0, size],
        };
        let row = w[0] + w[1] + w[2];
        let field = |r: &[u8], from: usize, n: usize, def: u64| -> u64 {
            if n == 0 {
                def
            } else {
                r[from..from + n].iter().fold(0u64, |a, &b| a << 8 | b as u64)
            }
        };
        let mut p = 0;
        for pair in index.chunks(2) {
            let (start, count) = (pair[0], *pair.get(1).unwrap_or(&0));
            for i in 0..count {
                if p + row > data.len() {
                    break;
                }
                let r = &data[p..p + row];
                p += row;
                let t = field(r, 0, w[0], 1);
                let a = field(r, w[0], w[1], 0);
                let b = field(r, w[0] + w[1], w[2], 0);
                let num = (start + i) as u32;
                let e = match t {
                    1 => Entry::Offset(a as usize),
                    2 => Entry::InStream { stream: a as u32, index: b as usize },
                    _ => continue,
                };
                self.xref.entry(num).or_insert(e);
            }
        }
        Some(s.dict.clone())
    }

    /// Scans the whole file for "n g obj" when the cross-reference data is
    /// unusable.
    fn rebuild_xref(&mut self) {
        self.xref.clear();
        let d = &self.data;
        let mut i = 0;
        let mut trailer = Dict::new();
        while i + 3 < d.len() {
            if &d[i..i + 3] == b"obj" && (i == 0 || !d[i - 1].is_ascii_alphanumeric()) {
                // Walk back over "num gen ".
                let mut j = i;
                while j > 0 && d[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                let gen_end = j;
                while j > 0 && d[j - 1].is_ascii_digit() {
                    j -= 1;
                }
                let gen_start = j;
                while j > 0 && d[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                let num_end = j;
                while j > 0 && d[j - 1].is_ascii_digit() {
                    j -= 1;
                }
                if gen_start < gen_end && j < num_end {
                    if let Ok(n) = core::str::from_utf8(&d[j..num_end]).unwrap_or("x").parse::<u32>() {
                        self.xref.insert(n, Entry::Offset(j));
                    }
                }
            }
            if &d[i..i + 3] == b"tra" && d[i..].starts_with(b"trailer") {
                let mut lx = Lexer::at(d, i + 7);
                if let Some(Object::Dict(t)) = parse_object(&mut lx, 0) {
                    for (k, v) in t {
                        trailer.insert(k, v);
                    }
                }
            }
            i += 1;
        }
        // Object streams and the catalog when no trailer survived.
        let nums: Vec<u32> = self.xref.keys().copied().collect();
        for n in nums {
            if let Some((Object::Stream(s), _)) = self.parse_indirect(n) {
                if key(&s.dict, "Type").and_then(Object::as_name) == Some(b"ObjStm") {
                    let (data, _) = filter::decode(&s.data, &s.dict);
                    let count = key(&s.dict, "N").and_then(Object::as_i64).unwrap_or(0) as usize;
                    let mut lx = Lexer::new(&data);
                    for idx in 0..count {
                        let Some(Token::Int(on)) = lx.next() else { break };
                        let _ = lx.next();
                        self.xref.entry(on as u32).or_insert(Entry::InStream { stream: n, index: idx });
                    }
                }
                if key(&s.dict, "Type").and_then(Object::as_name) == Some(b"XRef") {
                    for (k, v) in s.dict.iter() {
                        if matches!(k.as_slice(), b"Root" | b"Info" | b"ID" | b"Encrypt") {
                            trailer.entry(k.clone()).or_insert(v.clone());
                        }
                    }
                }
            }
        }
        if trailer.get(&b"Root"[..]).is_none() {
            let nums: Vec<u32> = self.xref.keys().copied().collect();
            for n in nums {
                if let Some(o) = self.get(n) {
                    if o.as_dict().and_then(|d| key(d, "Type")).and_then(Object::as_name) == Some(b"Catalog") {
                        trailer.insert(b"Root".to_vec(), Object::Ref(Ref { num: n, gen: 0 }));
                        break;
                    }
                }
            }
        }
        self.trailer = trailer;
        self.cache.borrow_mut().clear();
    }

    /// The object at `off` ("n g obj ... endobj"), streams included.
    fn parse_indirect_at(&self, off: usize) -> Option<(Object, Ref)> {
        let mut lx = Lexer::at(&self.data, off);
        let Token::Int(num) = lx.next()? else { return None };
        let Token::Int(gen) = lx.next()? else { return None };
        let Token::Word(w) = lx.next()? else { return None };
        if w != b"obj" {
            return None;
        }
        let r = Ref { num: num as u32, gen: gen as u16 };
        let t = lx.next()?;
        let obj = parse_from(&mut lx, t, 0)?;
        // A stream follows its dictionary.
        if let (Object::Dict(d), Some(Token::Word(kw))) = (&obj, lx.next()) {
            if kw == b"stream" {
                let mut p = lx.pos;
                if self.data.get(p) == Some(&b'\r') {
                    p += 1;
                }
                if self.data.get(p) == Some(&b'\n') {
                    p += 1;
                }
                let declared = match key(d, "Length") {
                    Some(Object::Ref(lr)) if lr.num != r.num => self.get(lr.num).and_then(|o| o.as_i64()),
                    Some(o) => o.as_i64(),
                    None => None,
                };
                let end = match declared {
                    Some(l) if l >= 0 && p + l as usize <= self.data.len() && self.endstream_near(p + l as usize) => {
                        p + l as usize
                    }
                    // Wrong or missing length: up to "endstream".
                    _ => {
                        let e = self.data[p..]
                            .windows(9)
                            .position(|w| w == b"endstream")
                            .map_or(self.data.len(), |e| p + e);
                        let mut e2 = e;
                        while e2 > p && matches!(self.data[e2 - 1], b'\r' | b'\n') {
                            e2 -= 1;
                        }
                        e2
                    }
                };
                let mut data = self.data[p..end].to_vec();
                if let Some(c) = &self.crypt {
                    if key(d, "Type").and_then(Object::as_name) != Some(b"XRef") {
                        data = c.decrypt_stream(r, d, &data);
                    }
                }
                let mut dict = d.clone();
                if let Some(c) = &self.crypt {
                    decrypt_strings_in_dict(&mut dict, c, r);
                }
                return Some((Object::Stream(Rc::new(Stream { dict, data })), r));
            }
        }
        let mut obj = obj;
        if let Some(c) = &self.crypt {
            decrypt_strings(&mut obj, c, r);
        }
        Some((obj, r))
    }

    fn endstream_near(&self, p: usize) -> bool {
        let tail = &self.data[p..(p + 20).min(self.data.len())];
        tail.windows(9).any(|w| w == b"endstream")
    }

    fn parse_indirect(&self, num: u32) -> Option<(Object, Ref)> {
        match *self.xref.get(&num)? {
            Entry::Offset(o) => self.parse_indirect_at(o),
            Entry::InStream { stream, index } => {
                let st = self.objstm(stream)?;
                let &(n, off) =
                    st.0.get(index).filter(|(n, _)| *n == num).or_else(|| st.0.iter().find(|(n, _)| *n == num))?;
                let mut lx = Lexer::at(&st.1, off);
                let o = parse_object(&mut lx, 0)?;
                Some((o, Ref { num: n, gen: 0 }))
            }
        }
    }

    fn objstm(&self, num: u32) -> Option<Rc<(Vec<(u32, usize)>, Vec<u8>)>> {
        if let Some(s) = self.objstms.borrow().get(&num) {
            return Some(s.clone());
        }
        let Entry::Offset(o) = *self.xref.get(&num)? else { return None };
        let (Object::Stream(s), _) = self.parse_indirect_at(o)? else { return None };
        let (data, _) = filter::decode(&s.data, &s.dict);
        let n = key(&s.dict, "N").and_then(Object::as_i64).unwrap_or(0).max(0) as usize;
        let first = key(&s.dict, "First").and_then(Object::as_i64).unwrap_or(0).max(0) as usize;
        let mut lx = Lexer::new(&data);
        let mut offs = Vec::with_capacity(n);
        for _ in 0..n {
            let (Some(Token::Int(on)), Some(Token::Int(off))) = (lx.next(), lx.next()) else { break };
            offs.push((on as u32, first + off as usize));
        }
        let rc = Rc::new((offs, data));
        self.objstms.borrow_mut().insert(num, rc.clone());
        Some(rc)
    }

    /// Indirect object `num`.
    pub fn get(&self, num: u32) -> Option<Object> {
        if let Some(o) = self.cache.borrow().get(&num) {
            return Some(o.clone());
        }
        let (o, _) = self.parse_indirect(num)?;
        self.cache.borrow_mut().insert(num, o.clone());
        Some(o)
    }

    /// Follows references (a few levels).
    pub fn resolve(&self, o: &Object) -> Object {
        let mut cur = o.clone();
        for _ in 0..8 {
            match cur {
                Object::Ref(r) => cur = self.get(r.num).unwrap_or(Object::Null),
                _ => return cur,
            }
        }
        Object::Null
    }

    pub fn get_key(&self, d: &Dict, k: &str) -> Object {
        d.get(k.as_bytes()).map_or(Object::Null, |o| self.resolve(o))
    }

    /// A stream's decoded bytes (image codecs not applied).
    pub fn stream_data(&self, s: &Stream) -> Vec<u8> {
        filter::decode(&s.data, &s.dict).0
    }

    fn collect_pages(&mut self) {
        let Some(cat) = self.catalog() else { return };
        let root = self.get_key(&cat, "Pages");
        let mut out = Vec::new();
        let mut seen = Vec::new();
        self.walk(&root, &Inherited::default(), &mut out, &mut seen, 0);
        self.pages = out;
    }

    fn walk(&self, node: &Object, inh: &Inherited, out: &mut Vec<Page>, seen: &mut Vec<u32>, depth: u32) {
        if depth > 64 {
            return;
        }
        let Some(d) = node.as_dict() else { return };
        let mut inh = inh.clone();
        if let Some(r) = d.get(&b"Resources"[..]) {
            if let Some(rd) = self.resolve(r).as_dict() {
                inh.resources = Some(rd.clone());
            }
        }
        for (k, slot) in [("MediaBox", &mut inh.media), ("CropBox", &mut inh.crop)] {
            if let Some(b) = self.rect(d, k) {
                *slot = Some(b);
            }
        }
        if let Some(r) = self.get_key(d, "Rotate").as_i64() {
            inh.rotate = Some(r as i32);
        }
        let is_pages = self.get_key(d, "Type").as_name() == Some(b"Pages") || d.contains_key(&b"Kids"[..]);
        if is_pages {
            let kids = self.get_key(d, "Kids");
            for k in kids.as_array().unwrap_or(&[]) {
                if let Object::Ref(r) = k {
                    if seen.contains(&r.num) {
                        continue;
                    }
                    seen.push(r.num);
                }
                let kid = self.resolve(k);
                self.walk(&kid, &inh, out, seen, depth + 1);
            }
        } else {
            let media = inh.media.unwrap_or([0.0, 0.0, 612.0, 792.0]);
            let crop = inh
                .crop
                .map_or(media, |c| [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])]);
            let bbox = if crop[2] > crop[0] && crop[3] > crop[1] { crop } else { media };
            out.push(Page {
                dict: d.clone(),
                resources: inh.resources.clone().unwrap_or_default(),
                bbox,
                rotate: inh.rotate.unwrap_or(0).rem_euclid(360) / 90 * 90,
            });
        }
    }

    /// A rectangle `[x0 y0 x1 y1]`, normalised.
    pub fn rect(&self, d: &Dict, k: &str) -> Option<[f64; 4]> {
        let a = self.get_key(d, k);
        let a = a.as_array()?;
        if a.len() < 4 {
            return None;
        }
        let v: Vec<f64> = a.iter().map(|o| self.resolve(o).as_f64().unwrap_or(0.0)).collect();
        Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
    }

    /// The document title from the Info dictionary, if any.
    pub fn title(&self) -> Option<String> {
        let info = self.trailer.get(&b"Info"[..])?;
        let info = self.resolve(info);
        let t = self.get_key(info.as_dict()?, "Title");
        let s = t.as_str()?;
        let text = text_string(s);
        (!text.trim().is_empty()).then_some(text)
    }
}

#[derive(Clone, Default)]
struct Inherited {
    resources: Option<Dict>,
    media: Option<[f64; 4]>,
    crop: Option<[f64; 4]>,
    rotate: Option<i32>,
}

fn decrypt_strings(o: &mut Object, c: &Decryptor, r: Ref) {
    match o {
        Object::Str(s) => *s = c.decrypt_string(r, s),
        Object::Array(a) => a.iter_mut().for_each(|x| decrypt_strings(x, c, r)),
        Object::Dict(d) => decrypt_strings_in_dict(d, c, r),
        _ => {}
    }
}

fn decrypt_strings_in_dict(d: &mut Dict, c: &Decryptor, r: Ref) {
    for v in d.values_mut() {
        decrypt_strings(v, c, r);
    }
}

/// A PDF text string (UTF-16BE with a byte order mark, else
/// PDFDocEncoding, which matches Latin-1 for the printable range).
pub fn text_string(s: &[u8]) -> String {
    if s.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = s[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    }
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&s[3..]).into_owned();
    }
    s.iter().map(|&b| b as char).collect()
}
