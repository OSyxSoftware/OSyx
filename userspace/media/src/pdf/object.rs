//! PDF objects and their syntax (ISO 32000-1, 7.2 and 7.3): the lexer, the
//! parser for direct objects, and indirect references.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    pub num: u32,
    pub gen: u16,
}

pub type Dict = BTreeMap<Vec<u8>, Object>;

#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// A string, already unescaped (and decrypted once read through a
    /// document).
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Rc<Stream>),
    Ref(Ref),
}

#[derive(Debug, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// The raw (still encoded) data.
    pub data: Vec<u8>,
}

impl Object {
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Object::Int(i) => Some(i as f64),
            Object::Real(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Object::Int(i) => Some(i),
            Object::Real(r) => Some(r as i64),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Object::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn is_white(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// A token of the PDF syntax.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    /// `{` and `}` (PostScript calculator functions).
    BraceStart,
    BraceEnd,
    /// A keyword or operator: `obj`, `R`, `true`, `re`, `Tj`, ...
    Word(Vec<u8>),
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Lexer<'a> {
        Lexer { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Lexer<'a> {
        Lexer { data, pos }
    }

    pub fn skip_white(&mut self) {
        while self.pos < self.data.len() {
            let c = self.data[self.pos];
            if is_white(c) {
                self.pos += 1;
            } else if c == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\r' | b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn next(&mut self) -> Option<Token> {
        let d = self.data;
        let c = loop {
            self.skip_white();
            let c = *d.get(self.pos)?;
            self.pos += 1;
            // A stray ')' or a single '>' is skipped.
            if c == b')' || (c == b'>' && d.get(self.pos) != Some(&b'>')) {
                continue;
            }
            break c;
        };
        Some(match c {
            b'[' => Token::ArrayStart,
            b']' => Token::ArrayEnd,
            b'{' => Token::BraceStart,
            b'}' => Token::BraceEnd,
            b'<' if d.get(self.pos) == Some(&b'<') => {
                self.pos += 1;
                Token::DictStart
            }
            b'>' if d.get(self.pos) == Some(&b'>') => {
                self.pos += 1;
                Token::DictEnd
            }
            b'<' => {
                let mut out = Vec::new();
                let mut hi: Option<u8> = None;
                while let Some(&c) = d.get(self.pos) {
                    self.pos += 1;
                    if c == b'>' {
                        break;
                    }
                    let v = match c {
                        b'0'..=b'9' => c - b'0',
                        b'a'..=b'f' => c - b'a' + 10,
                        b'A'..=b'F' => c - b'A' + 10,
                        _ => continue,
                    };
                    match hi.take() {
                        Some(h) => out.push(h << 4 | v),
                        None => hi = Some(v),
                    }
                }
                if let Some(h) = hi {
                    out.push(h << 4);
                }
                Token::Str(out)
            }
            b'(' => Token::Str(self.literal()),
            b'/' => {
                let mut out = Vec::new();
                while let Some(&c) = d.get(self.pos) {
                    if is_white(c) || is_delim(c) {
                        break;
                    }
                    self.pos += 1;
                    if c == b'#' && self.pos + 1 < d.len() {
                        let h = core::str::from_utf8(&d[self.pos..self.pos + 2])
                            .ok()
                            .and_then(|s| u8::from_str_radix(s, 16).ok());
                        if let Some(b) = h {
                            out.push(b);
                            self.pos += 2;
                            continue;
                        }
                    }
                    out.push(c);
                }
                Token::Name(out)
            }
            _ => {
                let start = self.pos - 1;
                while let Some(&c) = d.get(self.pos) {
                    if is_white(c) || is_delim(c) {
                        break;
                    }
                    self.pos += 1;
                }
                let w = &d[start..self.pos];
                match number(w) {
                    Some(t) => t,
                    None => Token::Word(w.to_vec()),
                }
            }
        })
    }

    /// The rest of a literal string after its `(`.
    fn literal(&mut self) -> Vec<u8> {
        let d = self.data;
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&c) = d.get(self.pos) {
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(&e) = d.get(self.pos) else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match d.get(self.pos) {
                                    Some(&o @ b'0'..=b'7') => {
                                        v = v * 8 + (o - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            if d.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'\r' => {
                    // An end of line in a string reads as \n.
                    if d.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
        out
    }
}

fn number(w: &[u8]) -> Option<Token> {
    if w.is_empty() || !w.iter().all(|&c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.')) {
        return None;
    }
    let s = core::str::from_utf8(w).ok()?;
    if !s.contains('.') {
        if let Ok(i) = s.parse::<i64>() {
            return Some(Token::Int(i));
        }
    }
    // Reals; tolerate the odd "--5" or "5-" producers write.
    let neg = s.starts_with('-');
    let digits: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = digits.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let frac: String = frac.chars().filter(|c| c.is_ascii_digit()).collect();
    if int.is_empty() && frac.is_empty() {
        return Some(Token::Real(0.0));
    }
    let mut v = 0.0f64;
    for c in int.bytes() {
        v = v * 10.0 + (c - b'0') as f64;
    }
    let mut scale = 0.1;
    for c in frac.bytes() {
        v += (c - b'0') as f64 * scale;
        scale *= 0.1;
    }
    Some(Token::Real(if neg { -v } else { v }))
}

/// Parses a direct object (references stay references). Streams are not
/// handled here; see `Document`.
pub fn parse_object(lx: &mut Lexer, depth: u32) -> Option<Object> {
    let t = lx.next()?;
    parse_from(lx, t, depth)
}

pub fn parse_from(lx: &mut Lexer, t: Token, depth: u32) -> Option<Object> {
    if depth > 64 {
        return None;
    }
    Some(match t {
        Token::Int(i) => {
            // "n g R" is a reference.
            let save = lx.pos;
            if i >= 0 {
                if let Some(Token::Int(g)) = lx.next() {
                    if let Some(Token::Word(w)) = lx.next() {
                        if w == b"R" && (0..=65535).contains(&g) {
                            return Some(Object::Ref(Ref { num: i as u32, gen: g as u16 }));
                        }
                    }
                }
            }
            lx.pos = save;
            Object::Int(i)
        }
        Token::Real(r) => Object::Real(r),
        Token::Str(s) => Object::Str(s),
        Token::Name(n) => Object::Name(n),
        Token::ArrayStart => {
            let mut a = Vec::new();
            loop {
                let t = lx.next()?;
                if t == Token::ArrayEnd {
                    break;
                }
                a.push(parse_from(lx, t, depth + 1)?);
            }
            Object::Array(a)
        }
        Token::DictStart => {
            let mut d = Dict::new();
            loop {
                let t = lx.next()?;
                match t {
                    Token::DictEnd => break,
                    Token::Name(k) => {
                        let save = lx.pos;
                        let v = lx.next()?;
                        if v == Token::DictEnd {
                            // A key without a value.
                            lx.pos = save;
                            continue;
                        }
                        let v = parse_from(lx, v, depth + 1)?;
                        d.insert(k, v);
                    }
                    _ => {}
                }
            }
            Object::Dict(d)
        }
        Token::Word(w) => match w.as_slice() {
            b"true" => Object::Bool(true),
            b"false" => Object::Bool(false),
            _ => Object::Null,
        },
        _ => Object::Null,
    })
}

/// Shorthand for building names in lookups.
pub fn key<'a>(d: &'a Dict, k: &str) -> Option<&'a Object> {
    d.get(k.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_objects() {
        let src = b"<< /Type /Page /Kids [1 0 R 2 0 R] /N -3.5 /S (a\\(b\\)\\101\nc) /H <48 65 6>  /E#20x 1 >>";
        let mut lx = Lexer::new(src);
        let o = parse_object(&mut lx, 0).unwrap();
        let d = o.as_dict().unwrap();
        assert_eq!(key(d, "Type").unwrap().as_name(), Some(&b"Page"[..]));
        assert_eq!(key(d, "Kids").unwrap().as_array().unwrap()[1], Object::Ref(Ref { num: 2, gen: 0 }));
        assert_eq!(key(d, "N").unwrap().as_f64(), Some(-3.5));
        assert_eq!(key(d, "S").unwrap().as_str(), Some(&b"a(b)A\nc"[..]));
        assert_eq!(key(d, "H").unwrap().as_str(), Some(&b"He`"[..]));
        assert_eq!(key(d, "E x").unwrap().as_i64(), Some(1));
    }

    #[test]
    fn stray_closers_do_not_recurse() {
        let mut src = alloc::vec![b')'; 300_000];
        src.extend_from_slice(b"> 7");
        assert_eq!(Lexer::new(&src).next(), Some(Token::Int(7)));
    }

    #[test]
    fn numbers() {
        assert_eq!(number(b".5"), Some(Token::Real(0.5)));
        assert_eq!(number(b"-.25"), Some(Token::Real(-0.25)));
        assert_eq!(number(b"+17"), Some(Token::Int(17)));
        assert_eq!(number(b"abc"), None);
    }
}
