//! API set schema, namespace version 6 (the format of Windows 10/11).
//!
//! An import from `api-ms-win-core-file-l1-2-5.dll` names a *contract*,
//! not a file; the schema maps it to the DLL that implements it (its
//! *host*), optionally depending on which module imports it. Layout (all
//! little-endian, offsets relative to the start of the namespace; see
//! docs/research/windows11-apiset-and-forwarding.md):
//!
//! ```text
//! header (28 bytes): version, size, flags, count, entry offset, hash offset, hash factor
//! entry  (24 bytes): flags, name offset, name length, hashed length, value offset, value count
//! value  (20 bytes): flags, importer offset, importer length, host offset, host length
//! hash   ( 8 bytes): hash, entry index            (sorted by hash)
//! ```
//!
//! Names are UTF-16LE without terminator and without `.dll`. The hashed
//! length covers the name up to (not including) its last `-`, so every minor
//! revision of a contract finds the same entry: `h = h * factor + c` over
//! the lower-cased hashed part.
//!
//! The parser works on any v6 namespace (the contract count and the hosts
//! come from the data, never from constants); [`SchemaBuilder`] writes one,
//! which is how OSyx ships its own schema for the contracts it implements.

use alloc::string::String;
use alloc::vec::Vec;

pub const VERSION: u32 = 6;
const HEADER: usize = 28;
const ENTRY: usize = 24;
const VALUE: usize = 20;
const HASH: usize = 8;
/// Upper bounds that keep a hostile namespace from causing huge loops.
const MAX_ENTRIES: u32 = 65536;
const MAX_VALUES: u32 = 64;
const MAX_NAME_BYTES: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiSetError {
    Truncated,
    BadVersion(u32),
    /// A table or string lies outside the namespace.
    OutOfBounds(&'static str),
    BadName,
}

/// A validated namespace (borrowed bytes).
#[derive(Clone, Copy, Debug)]
pub struct Schema<'a> {
    ns: &'a [u8],
    count: u32,
    entry_off: u32,
    hash_off: u32,
    factor: u32,
}

fn le32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o.checked_add(4)?).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn ascii_lower(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) {
        c + 32
    } else {
        c
    }
}

/// True for names the loader must look up in the schema (`api-*` / `ext-*`,
/// case-insensitive), as opposed to DLL file names.
pub fn is_api_set_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 4 && (b[..4].eq_ignore_ascii_case(b"api-") || b[..4].eq_ignore_ascii_case(b"ext-"))
}

/// The part of a contract name that is hashed: lower-cased, `.dll` removed,
/// cut at the last `-` (`api-ms-win-core-file-l1-2-5.dll` →
/// `api-ms-win-core-file-l1-2`).
pub fn hashed_part(name: &str) -> Option<String> {
    let mut n = name.to_ascii_lowercase();
    if n.ends_with(".dll") {
        n.truncate(n.len() - 4);
    }
    let cut = n.rfind('-')?;
    if cut == 0 || !n.is_ascii() {
        return None;
    }
    n.truncate(cut);
    Some(n)
}

pub fn hash(part: &str, factor: u32) -> u32 {
    part.bytes().fold(0u32, |h, c| h.wrapping_mul(factor).wrapping_add(ascii_lower(c as u16) as u32))
}

impl<'a> Schema<'a> {
    /// Validates the header and that every table and string referenced by
    /// the entries lies inside `ns`.
    pub fn parse(ns: &'a [u8]) -> Result<Schema<'a>, ApiSetError> {
        if ns.len() < HEADER {
            return Err(ApiSetError::Truncated);
        }
        let h = |i: usize| le32(ns, i * 4).ok_or(ApiSetError::Truncated);
        let version = h(0)?;
        if version != VERSION {
            return Err(ApiSetError::BadVersion(version));
        }
        let (count, entry_off, hash_off, factor) = (h(3)?, h(4)?, h(5)?, h(6)?);
        if count > MAX_ENTRIES {
            return Err(ApiSetError::OutOfBounds("entry count"));
        }
        let fits = |off: u32, n: u32, size: usize| (off as usize).checked_add(n as usize * size).is_some_and(|end| end <= ns.len());
        if !fits(entry_off, count, ENTRY) {
            return Err(ApiSetError::OutOfBounds("entry table"));
        }
        if !fits(hash_off, count, HASH) {
            return Err(ApiSetError::OutOfBounds("hash table"));
        }
        let s = Schema { ns, count, entry_off, hash_off, factor };
        for i in 0..count {
            let e = s.entry(i)?;
            s.string(e.name_off, e.name_len)?;
            if e.hashed_len > e.name_len {
                return Err(ApiSetError::OutOfBounds("hashed length"));
            }
            if e.value_count > MAX_VALUES || !fits(e.value_off, e.value_count, VALUE) {
                return Err(ApiSetError::OutOfBounds("value table"));
            }
            for v in 0..e.value_count {
                let o = (e.value_off + v * VALUE as u32) as usize;
                let r = |k: usize| le32(ns, o + k).ok_or(ApiSetError::Truncated);
                s.string(r(4)?, r(8)?)?;
                s.string(r(12)?, r(16)?)?;
            }
            let ho = hash_off as usize + i as usize * HASH;
            if le32(ns, ho + 4).ok_or(ApiSetError::Truncated)? >= count {
                return Err(ApiSetError::OutOfBounds("hash entry index"));
            }
        }
        Ok(s)
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn entry(&self, i: u32) -> Result<Entry, ApiSetError> {
        let o = self.entry_off as usize + i as usize * ENTRY;
        let r = |k: usize| le32(self.ns, o + k).ok_or(ApiSetError::Truncated);
        Ok(Entry { name_off: r(4)?, name_len: r(8)?, hashed_len: r(12)?, value_off: r(16)?, value_count: r(20)? })
    }

    fn string(&self, off: u32, len: u32) -> Result<&'a [u8], ApiSetError> {
        if !len.is_multiple_of(2) || len > MAX_NAME_BYTES {
            return Err(ApiSetError::BadName);
        }
        let start = off as usize;
        self.ns.get(start..start + len as usize).ok_or(ApiSetError::OutOfBounds("string"))
    }

    /// Decodes a UTF-16LE name (lossy for unpaired surrogates).
    fn decode(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    }

    fn eq_ascii_ci(utf16: &[u8], s: &str) -> bool {
        utf16.len() == s.len() * 2 && utf16.as_chunks::<2>().0.iter().zip(s.bytes()).all(|(c, b)| ascii_lower(u16::from_le_bytes(*c)) == ascii_lower(b as u16))
    }

    /// Name of entry `i` (for listings).
    pub fn name(&self, i: u32) -> Result<String, ApiSetError> {
        let e = self.entry(i)?;
        Ok(Self::decode(self.string(e.name_off, e.name_len)?))
    }

    /// Finds the entry for a contract name (any minor revision).
    fn find(&self, name: &str) -> Option<u32> {
        let part = hashed_part(name)?;
        let h = hash(&part, self.factor);
        // Binary search for the first record with this hash, then check the
        // names of all records that share it.
        let (mut lo, mut hi) = (0u32, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let v = le32(self.ns, self.hash_off as usize + mid as usize * HASH)?;
            if v < h {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut k = lo;
        while k < self.count && le32(self.ns, self.hash_off as usize + k as usize * HASH)? == h {
            let idx = le32(self.ns, self.hash_off as usize + k as usize * HASH + 4)?;
            let e = self.entry(idx).ok()?;
            let hashed = self.string(e.name_off, e.hashed_len).ok()?;
            if Self::eq_ascii_ci(hashed, &part) {
                return Some(idx);
            }
            k += 1;
        }
        None
    }

    /// Resolves `name` to its host DLL as seen by `importer` (a DLL file
    /// name; importer-specific values win over the default). `Ok(None)`: no
    /// such contract, or it has no host on this system.
    pub fn resolve(&self, name: &str, importer: Option<&str>) -> Result<Option<String>, ApiSetError> {
        let Some(idx) = self.find(name) else { return Ok(None) };
        let e = self.entry(idx)?;
        let mut default = None;
        for v in 0..e.value_count {
            let o = (e.value_off + v * VALUE as u32) as usize;
            let r = |k: usize| le32(self.ns, o + k).ok_or(ApiSetError::Truncated);
            let imp = self.string(r(4)?, r(8)?)?;
            let host = self.string(r(12)?, r(16)?)?;
            if imp.is_empty() {
                default.get_or_insert(host);
            } else if importer.is_some_and(|i| Self::eq_ascii_ci(imp, i)) {
                return Ok(Some(Self::decode(host)).filter(|h| !h.is_empty()));
            }
        }
        Ok(default.map(Self::decode).filter(|h| !h.is_empty()))
    }
}

#[derive(Clone, Copy)]
struct Entry {
    name_off: u32,
    name_len: u32,
    hashed_len: u32,
    value_off: u32,
    value_count: u32,
}

/// Writes a version-6 namespace.
#[derive(Default)]
pub struct SchemaBuilder {
    /// (contract name without `.dll`, [(importer, host)]) — the default
    /// value has an empty importer.
    sets: Vec<(String, Vec<(String, String)>)>,
}

pub const HASH_FACTOR: u32 = 31;

impl SchemaBuilder {
    pub fn new() -> SchemaBuilder {
        SchemaBuilder::default()
    }

    /// Adds a contract (`api-ms-win-core-file-l1-2-5`, `.dll` optional)
    /// with its default host (empty: no host) and importer-specific hosts.
    pub fn add(&mut self, name: &str, host: &str, alternates: &[(&str, &str)]) -> Result<&mut Self, ApiSetError> {
        let mut n = name.to_ascii_lowercase();
        if n.ends_with(".dll") {
            n.truncate(n.len() - 4);
        }
        if !is_api_set_name(&n) || hashed_part(&n).is_none() {
            return Err(ApiSetError::BadName);
        }
        let mut values = alloc::vec![(String::new(), String::from(host))];
        values.extend(alternates.iter().map(|(i, h)| (String::from(*i), String::from(*h))));
        self.sets.push((n, values));
        Ok(self)
    }

    pub fn build(&self) -> Vec<u8> {
        let mut sets: Vec<&(String, Vec<(String, String)>)> = self.sets.iter().collect();
        sets.sort_by(|a, b| a.0.cmp(&b.0));
        let count = sets.len();
        let values: usize = sets.iter().map(|s| s.1.len()).sum();
        let entry_off = HEADER;
        let value_off = entry_off + count * ENTRY;
        let hash_off = value_off + values * VALUE;
        let strings_off = hash_off + count * HASH;
        let mut out = alloc::vec![0u8; strings_off];
        let mut strings: Vec<u8> = Vec::new();
        let add_str =|s: &str, strings: &mut Vec<u8>| -> (u32, u32) {
            if s.is_empty() {
                return (0, 0);
            }
            let off = (strings_off + strings.len()) as u32;
            for u in s.encode_utf16() {
                strings.extend_from_slice(&u.to_le_bytes());
            }
            (off, (s.len() * 2) as u32)
        };
        let put = |out: &mut Vec<u8>, o: usize, v: u32| out[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let mut hashes: Vec<(u32, u32)> = Vec::with_capacity(count);
        let mut vi = 0;
        for (i, (name, vals)) in sets.iter().enumerate() {
            let (no, nl) = add_str(name, &mut strings);
            let part = hashed_part(name).expect("checked in add");
            let e = entry_off + i * ENTRY;
            put(&mut out, e + 4, no);
            put(&mut out, e + 8, nl);
            put(&mut out, e + 12, (part.len() * 2) as u32);
            put(&mut out, e + 16, (value_off + vi * VALUE) as u32);
            put(&mut out, e + 20, vals.len() as u32);
            for (imp, host) in vals.iter() {
                let (io, il) = add_str(imp, &mut strings);
                let (ho, hl) = add_str(host, &mut strings);
                let v = value_off + vi * VALUE;
                put(&mut out, v + 4, io);
                put(&mut out, v + 8, il);
                put(&mut out, v + 12, ho);
                put(&mut out, v + 16, hl);
                vi += 1;
            }
            hashes.push((hash(&part, HASH_FACTOR), i as u32));
        }
        hashes.sort();
        for (k, (h, i)) in hashes.iter().enumerate() {
            put(&mut out, hash_off + k * HASH, *h);
            put(&mut out, hash_off + k * HASH + 4, *i);
        }
        out.extend_from_slice(&strings);
        let size = out.len() as u32;
        for (k, v) in [VERSION, size, 0, count as u32, entry_off as u32, hash_off as u32, HASH_FACTOR].into_iter().enumerate() {
            put(&mut out, k * 4, v);
        }
        out
    }
}
