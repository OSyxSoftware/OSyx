//! msi.dll: a read-only Windows Installer database reader (the seven
//! entry points Zen imports by ordinal). MsiOpenDatabaseW parses a real
//! compound-file (CFB) installer package, MsiDatabaseOpenViewW parses a
//! small SELECT subset, MsiViewExecute evaluates it and MsiViewFetch hands
//! out rows as records. MsiGetProductInfoW answers from the uninstall
//! registry keys. Nothing here writes a database or installs a product.
#![allow(non_snake_case)]

use crate::{error::*, kernel32::fail, path, registry, teb};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::{format, string::{String, ToString}, vec::Vec};
use core::cmp::Ordering as Cmp;
use core::sync::atomic::{AtomicU32, Ordering};
use os_std::sync::Mutex;

const ERROR_OPEN_FAILED: u32 = 110;
const ERROR_UNKNOWN_PRODUCT: u32 = 1605;
const ERROR_UNKNOWN_PROPERTY: u32 = 1608;
const ERROR_INVALID_FIELD: u32 = 1610;
const ERROR_BAD_QUERY_SYNTAX: u32 = 1615;

pub fn exports() -> Vec<(&'static str, u64)> {
    alloc::vec![]
}

pub fn ordinals() -> Vec<(u16, u64)> {
    alloc::vec![
        (8, MsiCloseHandle as *const () as u64),
        (32, MsiDatabaseOpenViewW as *const () as u64),
        (70, MsiGetProductInfoW as *const () as u64),
        (92, MsiOpenDatabaseW as *const () as u64),
        (118, MsiRecordGetStringW as *const () as u64),
        (159, MsiViewExecute as *const () as u64),
        (160, MsiViewFetch as *const () as u64),
    ]
}

// ---- handles ----------------------------------------------------------------------

static HANDLES: Mutex<BTreeMap<u32, Handle>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU32 = AtomicU32::new(0x1000);

enum Handle {
    Db(Database),
    View(View),
    Record(Record),
}

fn new_handle(h: Handle) -> u32 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    HANDLES.lock().unwrap().insert(id, h);
    id
}

fn is_db(h: u32) -> bool {
    matches!(HANDLES.lock().unwrap().get(&h), Some(Handle::Db(_)))
}

// ---- strings and tables -----------------------------------------------------------

#[derive(Clone)]
struct StringTable {
    data: Vec<u8>,
    /// (offset, length) in `data` for references 1, 2, ...
    entries: Vec<(u32, u32)>,
    codepage: u16,
    /// Table streams use 4-byte string references.
    wide: bool,
}

impl StringTable {
    fn parse(pool: &[u8], data: &[u8]) -> Option<StringTable> {
        if pool.len() < 2 {
            return None;
        }
        let codepage = u16::from_le_bytes([pool[0], pool[1]]);
        let mut entries = Vec::new();
        let mut wide = false;
        let mut at = 2usize;
        let mut offset = 0u32;
        loop {
            let word = pool.get(at..at + 4)?;
            let mut len = u16::from_le_bytes([word[0], word[1]]) as u32;
            let count = u16::from_le_bytes([word[2], word[3]]);
            if at == 2 {
                wide = len & 0x8000 != 0;
                len &= 0x7fff;
            }
            if len == 0 && count == 0 {
                break;
            }
            entries.push((offset, len));
            offset = offset.saturating_add(len);
            at += 4;
        }
        Some(StringTable { data: data.to_vec(), entries, codepage, wide })
    }

    fn string(&self, r: u32) -> String {
        if r == 0 {
            return String::new();
        }
        let Some(&(off, len)) = self.entries.get(r as usize - 1) else {
            return String::new();
        };
        let start = off as usize;
        if start >= self.data.len() {
            return String::new();
        }
        let end = start.saturating_add(len as usize).min(self.data.len());
        self.decode(&self.data[start..end])
    }

    fn decode(&self, bytes: &[u8]) -> String {
        match self.codepage {
            65001 => String::from_utf8_lossy(bytes).into_owned(),
            1200 => {
                let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&units)
            }
            _ => bytes.iter().map(|&b| cp1252(b)).collect(),
        }
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}',
        '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

fn read_ref(bytes: &[u8], at: usize, wide: bool) -> Option<u32> {
    if wide {
        let s = bytes.get(at..at.checked_add(4)?)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    } else {
        let s = bytes.get(at..at.checked_add(2)?)?;
        Some(u16::from_le_bytes([s[0], s[1]]) as u32)
    }
}

#[derive(Clone)]
struct Column {
    number: u16,
    name: String,
    kind: u8,
}

#[derive(Clone)]
struct Table {
    #[allow(dead_code)]
    name: String,
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
}

#[derive(Clone)]
enum Value {
    Null,
    Int(i32),
    Str(String),
}

#[derive(Clone)]
struct Database {
    #[allow(dead_code)]
    strings: StringTable,
    tables: BTreeMap<String, Table>,
}

fn decode_row(strings: &StringTable, columns: &[Column], row: &[u8], wide: bool) -> Option<Vec<Value>> {
    let mut out = Vec::with_capacity(columns.len());
    let mut at = 0usize;
    for c in columns {
        match c.kind {
            1 => {
                out.push(Value::Str(strings.string(read_ref(row, at, wide)?)));
                at += if wide { 4 } else { 2 };
            }
            3 => {
                let s = row.get(at..at + 4)?;
                out.push(Value::Int(i32::from_le_bytes([s[0], s[1], s[2], s[3]])));
                at += 4;
            }
            kind => {
                let s = row.get(at..at + 2)?;
                let v = i16::from_le_bytes([s[0], s[1]]);
                out.push(if kind == 2 { Value::Int(v as i32) } else { Value::Str(String::new()) });
                at += 2;
            }
        }
    }
    Some(out)
}

fn parse_database(bytes: &[u8]) -> Option<Database> {
    let cfb = Cfb::parse(bytes)?;
    let pool = cfb.stream("_StringPool")?;
    let data = cfb.stream("_StringData")?;
    let strings = StringTable::parse(&pool, &data)?;
    let ref_width = if strings.wide { 4usize } else { 2 };
    let raw = cfb.stream("_Columns")?;
    let width = 2 * ref_width + 4;
    if raw.len() % width != 0 {
        return None;
    }
    let mut grouped: BTreeMap<String, (String, Vec<Column>)> = BTreeMap::new();
    for row in raw.chunks_exact(width) {
        let table = strings.string(read_ref(row, 0, strings.wide)?);
        let num = u16::from_le_bytes([*row.get(ref_width)?, *row.get(ref_width + 1)?]);
        let name = strings.string(read_ref(row, ref_width + 2, strings.wide)?);
        let ty = u16::from_le_bytes([*row.get(2 * ref_width + 2)?, *row.get(2 * ref_width + 3)?]);
        if table.is_empty() {
            continue;
        }
        let number = num.wrapping_sub(0x8000);
        let ty = ty.wrapping_sub(0x8000) as i16;
        grouped
            .entry(table.to_lowercase())
            .or_insert_with(|| (table, Vec::new()))
            .1
            .push(Column { number, name, kind: (ty & 0xFF) as u8 });
    }
    let mut tables = BTreeMap::new();
    for (key, (name, mut columns)) in grouped {
        columns.sort_by_key(|c| c.number);
        let raw = cfb.stream(&name)?;
        let row_width: usize = columns.iter().map(|c| match c.kind { 1 => ref_width, 3 => 4, _ => 2 }).sum();
        if row_width == 0 || raw.len() % row_width != 0 {
            return None;
        }
        let rows = raw
            .chunks_exact(row_width)
            .map(|r| decode_row(&strings, &columns, r, strings.wide))
            .collect::<Option<Vec<_>>>()?;
        tables.insert(key, Table { name, columns, rows });
    }
    Some(Database { strings, tables })
}

// ---- compound file binary ---------------------------------------------------------

const END: u32 = 0xFFFF_FFFE;
const FREE: u32 = 0xFFFF_FFFF;

struct Dir {
    name: String,
    kind: u8,
    start: u32,
    size: u64,
}

struct Cfb<'a> {
    data: &'a [u8],
    sector: usize,
    mini_sector: usize,
    mini_cutoff: u32,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    entries: Vec<Dir>,
    mini_stream: Vec<u8>,
}

fn rd16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn rd32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn sector_offset(s: u32, sector: usize) -> usize {
    (s as usize + 1).saturating_mul(sector)
}

fn chain(fat: &[u32], start: u32, cap: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut s = start;
    while s != END && s != FREE && (s as usize) < fat.len() && out.len() <= cap {
        out.push(s);
        match fat.get(s as usize) {
            Some(&next) => s = next,
            None => break,
        }
    }
    out
}

fn chain_bytes(data: &[u8], fat: &[u32], start: u32, sector: usize, limit: usize) -> Option<Vec<u8>> {
    let cap = data.len() / sector + 2;
    let mut out = Vec::new();
    for s in chain(fat, start, cap) {
        if out.len() >= limit {
            break;
        }
        let off = sector_offset(s, sector);
        let end = off.checked_add(sector)?;
        if end > data.len() {
            return None;
        }
        let take = sector.min(limit - out.len());
        out.extend_from_slice(&data[off..off + take]);
    }
    Some(out)
}

impl<'a> Cfb<'a> {
    fn parse(data: &'a [u8]) -> Option<Cfb<'a>> {
        if data.len() < 512 || !data.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
            return None;
        }
        if rd16(data, 28)? != 0xFFFE {
            return None;
        }
        let major = rd16(data, 26)?;
        let shift = rd16(data, 30)?;
        if !matches!((major, shift), (3, 9) | (4, 12)) {
            return None;
        }
        let mini_shift = rd16(data, 32)?;
        if mini_shift != 6 {
            return None;
        }
        let sector = 1usize << shift;
        let mini_sector = 1usize << mini_shift;
        if data.len() < sector {
            return None;
        }
        let n_fat = rd32(data, 44)? as usize;
        let dir_start = rd32(data, 48)?;
        let cutoff = rd32(data, 56)?;
        let mini_fat_start = rd32(data, 60)?;
        let _mini_fat_count = rd32(data, 64)?;
        let difat_start = rd32(data, 68)?;
        let _difat_count = rd32(data, 72)?;
        if n_fat == 0 || n_fat > data.len() / sector + 1 {
            return None;
        }
        let mut fat_sectors = Vec::new();
        for i in 0..109 {
            if fat_sectors.len() >= n_fat {
                break;
            }
            let s = rd32(data, 76 + i * 4)?;
            if s != FREE {
                fat_sectors.push(s);
            }
        }
        let per_difat = sector / 4 - 1;
        let mut difat = difat_start;
        let mut guard = 0;
        while fat_sectors.len() < n_fat && difat != END && difat != FREE && guard < n_fat + 1 {
            let off = sector_offset(difat, sector);
            if off + sector > data.len() {
                return None;
            }
            for i in 0..per_difat {
                if fat_sectors.len() >= n_fat {
                    break;
                }
                let s = rd32(data, off + i * 4)?;
                if s != FREE {
                    fat_sectors.push(s);
                }
            }
            difat = rd32(data, off + per_difat * 4)?;
            guard += 1;
        }
        if fat_sectors.len() < n_fat {
            return None;
        }
        let mut fat = Vec::with_capacity(fat_sectors.len() * (sector / 4));
        for s in fat_sectors {
            let off = sector_offset(s, sector);
            let end = off.checked_add(sector)?;
            if end > data.len() {
                return None;
            }
            for i in 0..sector / 4 {
                fat.push(rd32(data, off + i * 4)?);
            }
        }
        let dir_bytes = chain_bytes(data, &fat, dir_start, sector, usize::MAX)?;
        let mut entries = Vec::new();
        for chunk in dir_bytes.chunks_exact(128) {
            let kind = *chunk.get(66)?;
            if kind != 2 && kind != 5 {
                continue;
            }
            let nlen = rd16(chunk, 64)? as usize;
            if nlen < 2 {
                continue;
            }
            let units = nlen.min(64) / 2 - 1;
            let mut name = Vec::with_capacity(units);
            for i in 0..units {
                name.push(rd16(chunk, i * 2)?);
            }
            let start = rd32(chunk, 116)?;
            let size = if major == 3 {
                rd32(chunk, 120)? as u64
            } else {
                u64::from_le_bytes(chunk.get(120..128)?.try_into().ok()?)
            };
            entries.push(Dir { name: String::from_utf16_lossy(&name), kind, start, size });
        }
        let root = entries.iter().find(|e| e.kind == 5)?;
        let mini_size = root.size.min(data.len() as u64) as usize;
        let mini_stream = chain_bytes(data, &fat, root.start, sector, mini_size)?;
        if mini_stream.len() < mini_size {
            return None;
        }
        let mut mini_fat = Vec::new();
        if mini_fat_start != END && mini_fat_start != FREE {
            for s in chain(&fat, mini_fat_start, data.len() / sector + 2) {
                let off = sector_offset(s, sector);
                if off + sector > data.len() {
                    return None;
                }
                for i in 0..sector / 4 {
                    mini_fat.push(rd32(data, off + i * 4)?);
                }
            }
        }
        Some(Cfb { data, sector, mini_sector, mini_cutoff: cutoff, fat, mini_fat, entries, mini_stream })
    }

    fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.entries.iter().find(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(name))?;
        let size = e.size.min(self.data.len() as u64) as usize;
        if size == 0 {
            return Some(Vec::new());
        }
        let out = if e.size < self.mini_cutoff as u64 {
            let mut out = Vec::with_capacity(size);
            let mut s = e.start;
            let cap = self.data.len() / self.mini_sector + 2;
            let mut n = 0;
            while out.len() < size && s != END && s != FREE && n <= cap {
                let off = s as usize * self.mini_sector;
                let end = (off + self.mini_sector).min(self.mini_stream.len());
                if off >= self.mini_stream.len() {
                    break;
                }
                out.extend_from_slice(&self.mini_stream[off..end]);
                s = *self.mini_fat.get(s as usize)?;
                n += 1;
            }
            out
        } else {
            chain_bytes(self.data, &self.fat, e.start, self.sector, size)?
        };
        if out.len() < size {
            return None;
        }
        let mut out = out;
        out.truncate(size);
        Some(out)
    }
}

// ---- SQL subset -------------------------------------------------------------------

#[derive(Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(i64),
    Str(String),
    Star,
    Comma,
    Param,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LParen,
    RParen,
}

fn tokenize(q: &str) -> Result<Vec<Tok>, ()> {
    let b = q.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b'[' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b']' {
                    j += 1;
                }
                if j >= b.len() {
                    return Err(());
                }
                let name = core::str::from_utf8(&b[start..j]).map_err(|_| ())?;
                out.push(Tok::Ident(String::from(name)));
                i = j + 1;
            }
            b'\'' => {
                let mut bytes = Vec::new();
                let mut j = i + 1;
                loop {
                    let c = *b.get(j).ok_or(())?;
                    if c == b'\'' {
                        if b.get(j + 1) == Some(&b'\'') {
                            bytes.push(b'\'');
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    bytes.push(c);
                    j += 1;
                }
                out.push(Tok::Str(String::from_utf8(bytes).map_err(|_| ())?));
                i = j + 1;
            }
            b'?' => {
                out.push(Tok::Param);
                i += 1;
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'=' => {
                out.push(Tok::Eq);
                i += 1;
            }
            b'<' => {
                match b.get(i + 1) {
                    Some(b'=') => {
                        out.push(Tok::Le);
                        i += 2;
                    }
                    Some(b'>') => {
                        out.push(Tok::Ne);
                        i += 2;
                    }
                    _ => {
                        out.push(Tok::Lt);
                        i += 1;
                    }
                }
            }
            b'>' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            b'!' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ne);
                    i += 2;
                } else {
                    return Err(());
                }
            }
            b'-' if b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) => {
                let (n, next) = number(b, i + 1, true)?;
                out.push(Tok::Num(n));
                i = next;
            }
            c if c.is_ascii_digit() => {
                let (n, next) = number(b, i, false)?;
                out.push(Tok::Num(n));
                i = next;
            }
            c if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'#' | b'.') || c >= 0x80 => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'#' | b'.') || b[i] >= 0x80) {
                    i += 1;
                }
                out.push(Tok::Ident(String::from(core::str::from_utf8(&b[start..i]).map_err(|_| ())?)));
            }
            _ => return Err(()),
        }
    }
    Ok(out)
}

fn number(b: &[u8], from: usize, negative: bool) -> Result<(i64, usize), ()> {
    let mut j = from;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    let text = core::str::from_utf8(&b[from..j]).map_err(|_| ())?;
    let mut n: i64 = text.parse().map_err(|_| ())?;
    if negative {
        n = n.checked_neg().ok_or(())?;
    }
    Ok((n, j))
}

#[derive(Clone, Copy)]
enum Op {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone)]
enum Operand {
    Val(Value),
    Param(u32),
}

enum Cond {
    Cmp { col: usize, op: Op, rhs: Operand },
    IsNull { col: usize, not: bool },
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

struct View {
    table: Table,
    cols: Vec<usize>,
    cond: Option<Cond>,
    order: Option<(usize, bool)>,
    rows: Vec<Vec<Value>>,
    next: usize,
    params: u32,
}

fn find_col(t: &Table, name: &str) -> Option<usize> {
    t.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
}

struct Parser<'a> {
    db: &'a Database,
    toks: Vec<Tok>,
    at: usize,
    params: u32,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn take_keyword(&mut self, k: &str) -> bool {
        match self.peek() {
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(k) => {
                self.at += 1;
                true
            }
            _ => false,
        }
    }

    fn ident(&mut self) -> Result<String, ()> {
        match self.toks.get(self.at) {
            Some(Tok::Ident(s)) => {
                self.at += 1;
                Ok(s.clone())
            }
            _ => Err(()),
        }
    }

    fn parse(mut self) -> Result<View, ()> {
        if !self.take_keyword("SELECT") {
            return Err(());
        }
        let selected = if matches!(self.peek(), Some(Tok::Star)) {
            self.at += 1;
            None
        } else {
            let mut names = alloc::vec![self.ident()?];
            while matches!(self.peek(), Some(Tok::Comma)) {
                self.at += 1;
                names.push(self.ident()?);
            }
            Some(names)
        };
        if !self.take_keyword("FROM") {
            return Err(());
        }
        let table_name = self.ident()?;
        let table = self.db.tables.get(&table_name.to_lowercase()).cloned().ok_or(())?;
        let cols = match selected {
            None => (0..table.columns.len()).collect(),
            Some(names) => names.iter().map(|n| find_col(&table, n)).collect::<Option<Vec<_>>>().ok_or(())?,
        };
        let mut cond = None;
        if self.take_keyword("WHERE") {
            cond = Some(self.cond(&table)?);
        }
        let mut order = None;
        if self.take_keyword("ORDER") {
            if !self.take_keyword("BY") {
                return Err(());
            }
            let name = self.ident()?;
            let idx = find_col(&table, &name).ok_or(())?;
            let desc = if self.take_keyword("DESC") {
                true
            } else {
                self.take_keyword("ASC");
                false
            };
            order = Some((idx, desc));
        }
        if self.at != self.toks.len() {
            return Err(());
        }
        Ok(View { table, cols, cond, order, rows: Vec::new(), next: 0, params: self.params })
    }

    fn cond(&mut self, t: &Table) -> Result<Cond, ()> {
        let mut left = self.term(t)?;
        loop {
            if self.take_keyword("AND") {
                let right = self.term(t)?;
                left = Cond::And(Box::new(left), Box::new(right));
            } else if self.take_keyword("OR") {
                let right = self.term(t)?;
                left = Cond::Or(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn term(&mut self, t: &Table) -> Result<Cond, ()> {
        let name = self.ident()?;
        let col = find_col(t, &name).ok_or(())?;
        if self.take_keyword("IS") {
            let not = self.take_keyword("NOT");
            if !self.take_keyword("NULL") {
                return Err(());
            }
            return Ok(Cond::IsNull { col, not });
        }
        let op = match self.toks.get(self.at) {
            Some(Tok::Eq) => Op::Eq,
            Some(Tok::Ne) => Op::Ne,
            Some(Tok::Lt) => Op::Lt,
            Some(Tok::Gt) => Op::Gt,
            Some(Tok::Le) => Op::Le,
            Some(Tok::Ge) => Op::Ge,
            _ => return Err(()),
        };
        self.at += 1;
        let rhs = match self.toks.get(self.at) {
            Some(Tok::Str(s)) => Operand::Val(Value::Str(s.clone())),
            Some(Tok::Num(n)) => Operand::Val(Value::Int(i32::try_from(*n).map_err(|_| ())?)),
            Some(Tok::Param) => {
                self.params += 1;
                Operand::Param(self.params)
            }
            _ => return Err(()),
        };
        self.at += 1;
        Ok(Cond::Cmp { col, op, rhs })
    }
}

fn cmp_str(a: &str, b: &str) -> Cmp {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

fn cmp_value(a: &Value, b: &Value) -> Option<Cmp> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => Some(cmp_str(x, y)),
        (Value::Int(x), Value::Str(y)) => Some(cmp_str(&x.to_string(), y)),
        (Value::Str(x), Value::Int(y)) => Some(cmp_str(x, &y.to_string())),
        _ => None,
    }
}

fn apply(op: Op, ord: Cmp) -> bool {
    match op {
        Op::Eq => ord == Cmp::Equal,
        Op::Ne => ord != Cmp::Equal,
        Op::Lt => ord == Cmp::Less,
        Op::Gt => ord == Cmp::Greater,
        Op::Le => ord != Cmp::Greater,
        Op::Ge => ord != Cmp::Less,
    }
}

fn eval(c: &Cond, row: &[Value], params: &[Value]) -> bool {
    match c {
        Cond::And(a, b) => eval(a, row, params) && eval(b, row, params),
        Cond::Or(a, b) => eval(a, row, params) || eval(b, row, params),
        Cond::IsNull { col, not } => {
            let null = !matches!(row.get(*col), Some(Value::Int(_)) | Some(Value::Str(_)));
            if *not { !null } else { null }
        }
        Cond::Cmp { col, op, rhs } => {
            let a = match row.get(*col) {
                Some(v @ (Value::Int(_) | Value::Str(_))) => v,
                _ => return false,
            };
            let b = match rhs {
                Operand::Val(v) => v,
                Operand::Param(n) => match params.get(*n as usize - 1) {
                    Some(v) => v,
                    None => return false,
                },
            };
            match cmp_value(a, b) {
                Some(o) => apply(*op, o),
                None => false,
            }
        }
    }
}

fn order_cmp(a: &Value, b: &Value, desc: bool) -> Cmp {
    let base = match (a, b) {
        (Value::Null, Value::Null) => Cmp::Equal,
        (Value::Null, _) => Cmp::Less,
        (_, Value::Null) => Cmp::Greater,
        _ => cmp_value(a, b).unwrap_or(Cmp::Equal),
    };
    if desc && !matches!(a, Value::Null) && !matches!(b, Value::Null) {
        base.reverse()
    } else {
        base
    }
}

impl View {
    fn run(&mut self, params: &[Value]) {
        let mut matched: Vec<(Value, Vec<Value>)> = Vec::new();
        for row in &self.table.rows {
            let keep = match &self.cond {
                Some(c) => eval(c, row, params),
                None => true,
            };
            if !keep {
                continue;
            }
            let key = self.order.and_then(|(i, _)| row.get(i)).cloned().unwrap_or(Value::Null);
            let out = self.cols.iter().map(|&i| row.get(i).cloned().unwrap_or(Value::Null)).collect();
            matched.push((key, out));
        }
        if let Some((_, desc)) = self.order {
            matched.sort_by(|a, b| order_cmp(&a.0, &b.0, desc));
        }
        self.rows = matched.into_iter().map(|(_, row)| row).collect();
        self.next = 0;
    }
}

fn parse_query(db: &Database, q: &str) -> Result<View, ()> {
    let toks = tokenize(q)?;
    Parser { db, toks, at: 0, params: 0 }.parse()
}

// ---- buffers and records ----------------------------------------------------------

fn render(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Int(i) => i.to_string(),
        Value::Str(s) => s.clone(),
    }
}

fn put_string(s: &str, buf: *mut u16, pcch: *mut u32) -> u32 {
    if pcch.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    let need = units.len() as u32;
    // SAFETY: `pcch` is the caller's readable in/out length.
    let cap = unsafe { core::ptr::read_unaligned(pcch) };
    if buf.is_null() || cap == 0 || cap <= need {
        // SAFETY: as above, and writable per the API contract.
        unsafe { core::ptr::write_unaligned(pcch, need) };
        return fail(ERROR_MORE_DATA, 0);
    }
    for (i, unit) in units.iter().enumerate() {
        // SAFETY: the caller's buffer holds `cap` units and cap > need.
        unsafe { teb::put(buf as u64, i * 2, *unit) };
    }
    // SAFETY: one more unit for the terminator fits in the same buffer.
    unsafe {
        teb::put(buf as u64, units.len() * 2, 0u16);
        core::ptr::write_unaligned(pcch, need);
    }
    ERROR_SUCCESS
}

fn property_names(attribute: &str) -> &'static [&'static str] {
    const PRODUCT_NAME: &[&str] = &["DisplayName", "ProductName"];
    const VERSION: &[&str] = &["DisplayVersion", "VersionString", "Version"];
    const PUBLISHER: &[&str] = &["Publisher"];
    const LOCATION: &[&str] = &["InstallLocation"];
    const SOURCE: &[&str] = &["InstallSource"];
    const PRODUCT_CODE: &[&str] = &["ProductCode"];
    const PACKAGE: &[&str] = &["PackageName"];
    for (names, value) in [
        (&["ProductName", "DisplayName"][..], PRODUCT_NAME),
        (&["VersionString", "Version", "DisplayVersion"][..], VERSION),
        (&["Publisher"][..], PUBLISHER),
        (&["InstallLocation"][..], LOCATION),
        (&["InstallSource"][..], SOURCE),
        (&["ProductID", "ProductCode"][..], PRODUCT_CODE),
        (&["PackageCode", "PackageName"][..], PACKAGE),
    ] {
        if names.iter().any(|n| attribute.eq_ignore_ascii_case(n)) {
            return value;
        }
    }
    &[]
}

// ---- entry points -----------------------------------------------------------------

extern "win64" fn MsiCloseHandle(h: u32) -> u32 {
    if h <= 1 {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    match HANDLES.lock().unwrap().remove(&h) {
        Some(_) => ERROR_SUCCESS,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn MsiOpenDatabaseW(path_w: *const u16, persist: *const u16, ph: *mut u32) -> u32 {
    if ph.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's output handle; MSI leaves it 0 on failure.
    unsafe { core::ptr::write_unaligned(ph, 0) };
    // SAFETY: a NUL-terminated wide string from the caller.
    let Some(name) = (unsafe { path::wide_to_string(path_w) }) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    let mode = persist as usize;
    let base = mode & !0x20usize;
    if base == 3 || base == 4 {
        return fail(ERROR_OPEN_FAILED, 0);
    }
    if base > 2 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let osyx = match crate::kernel32::locate(&name) {
        Ok((p, true)) => p,
        Ok(_) => return fail(ERROR_FILE_NOT_FOUND, 0),
        Err(e) => return fail(e, 0),
    };
    let bytes = match os_std::fs::read(&osyx) {
        Ok(b) => b,
        Err(e) => {
            let err = match e.kind() {
                os_std::error::ErrorKind::NotFound => ERROR_FILE_NOT_FOUND,
                os_std::error::ErrorKind::PermissionDenied => ERROR_ACCESS_DENIED,
                _ => ERROR_OPEN_FAILED,
            };
            return fail(err, 0);
        }
    };
    if bytes.len() > 64 << 20 {
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    let Some(db) = parse_database(&bytes) else {
        return fail(ERROR_OPEN_FAILED, 0);
    };
    let h = new_handle(Handle::Db(db));
    // SAFETY: as above.
    unsafe { core::ptr::write_unaligned(ph, h) };
    ERROR_SUCCESS
}

extern "win64" fn MsiDatabaseOpenViewW(db: u32, query: *const u16, ph: *mut u32) -> u32 {
    if ph.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's output handle; cleared on failure.
    unsafe { core::ptr::write_unaligned(ph, 0) };
    if !is_db(db) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    let database = {
        let m = HANDLES.lock().unwrap();
        match m.get(&db) {
            Some(Handle::Db(d)) => d.clone(),
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        }
    };
    // SAFETY: a NUL-terminated wide string from the caller.
    let Some(q) = (unsafe { path::wide_to_string(query) }) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    let view = match parse_query(&database, &q) {
        Ok(v) => v,
        Err(()) => return fail(ERROR_BAD_QUERY_SYNTAX, 0),
    };
    let h = new_handle(Handle::View(view));
    // SAFETY: as above.
    unsafe { core::ptr::write_unaligned(ph, h) };
    ERROR_SUCCESS
}

extern "win64" fn MsiRecordGetStringW(h: u32, field: u32, buf: *mut u16, pcch: *mut u32) -> u32 {
    let text = {
        let m = HANDLES.lock().unwrap();
        let rec = match m.get(&h) {
            Some(Handle::Record(r)) => r,
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        };
        if field == 0 || field as usize >= rec.fields.len() {
            return fail(ERROR_INVALID_FIELD, 0);
        }
        match rec.fields.get(field as usize) {
            Some(v) => render(v),
            None => return fail(ERROR_INVALID_FIELD, 0),
        }
    };
    put_string(&text, buf, pcch)
}

extern "win64" fn MsiGetProductInfoW(product: *const u16, attribute: *const u16, buf: *mut u16, pcch: *mut u32) -> u32 {
    // SAFETY: NUL-terminated wide strings from the caller.
    let (product, attribute) = unsafe { (path::wide_to_string(product), path::wide_to_string(attribute)) };
    let (Some(product), Some(attribute)) = (product, attribute) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    let mut values = Vec::new();
    let mut found = false;
    for key in [
        format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{product}"),
        format!("Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{product}"),
    ] {
        let list = registry::machine_strings(&key);
        if !list.is_empty() {
            found = true;
        }
        values.extend(list);
    }
    if !found {
        return fail(ERROR_UNKNOWN_PRODUCT, 0);
    }
    let names = property_names(&attribute);
    if names.is_empty() {
        return fail(ERROR_UNKNOWN_PROPERTY, 0);
    }
    let value = names
        .iter()
        .find_map(|n| values.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()));
    match value {
        Some(v) => put_string(&v, buf, pcch),
        None => fail(ERROR_UNKNOWN_PROPERTY, 0),
    }
}

extern "win64" fn MsiViewExecute(hView: u32, hRecord: u32) -> u32 {
    let params_count = {
        let m = HANDLES.lock().unwrap();
        match m.get(&hView) {
            Some(Handle::View(v)) => v.params,
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        }
    };
    let mut params = Vec::new();
    if params_count > 0 {
        if hRecord == 0 {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        let m = HANDLES.lock().unwrap();
        let rec = match m.get(&hRecord) {
            Some(Handle::Record(r)) => r,
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        };
        if params_count as usize >= rec.fields.len() {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        params = (1..=params_count).map(|i| rec.fields.get(i as usize).cloned().unwrap_or(Value::Null)).collect();
    }
    let mut m = HANDLES.lock().unwrap();
    match m.get_mut(&hView) {
        Some(Handle::View(v)) => {
            v.run(&params);
            ERROR_SUCCESS
        }
        _ => fail(ERROR_INVALID_HANDLE, 0),
    }
}

extern "win64" fn MsiViewFetch(hView: u32, phRecord: *mut u32) -> u32 {
    if phRecord.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let row = {
        let mut m = HANDLES.lock().unwrap();
        let view = match m.get_mut(&hView) {
            Some(Handle::View(v)) => v,
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        };
        if view.next >= view.rows.len() {
            // SAFETY: the caller's output handle.
            unsafe { core::ptr::write_unaligned(phRecord, 0) };
            return fail(ERROR_NO_MORE_ITEMS, 0);
        }
        let row = view.rows[view.next].clone();
        view.next += 1;
        row
    };
    let mut fields = Vec::with_capacity(row.len() + 1);
    fields.push(Value::Null);
    fields.extend(row);
    let h = new_handle(Handle::Record(Record { fields }));
    // SAFETY: the caller's output handle.
    unsafe { core::ptr::write_unaligned(phRecord, h) };
    ERROR_SUCCESS
}

struct Record {
    /// Fields are 1-based; index 0 is reserved and always Null.
    fields: Vec<Value>,
}
