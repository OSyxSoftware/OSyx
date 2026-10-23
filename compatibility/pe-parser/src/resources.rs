//! Numeric resource trees and VS_VERSIONINFO blocks, from the public PE
//! format. Read-only, allocation-free, checked and bounded at every level.
use crate::{dir, PeError, PeFile, Result};
fn bad(reason: &'static str) -> PeError {
    PeError::BadResource { reason }
}
fn word(data: &[u8], at: usize) -> Result<u16> {
    let b = data.get(at..at.checked_add(2).ok_or(bad("word offset overflow"))?).ok_or(bad("truncated word"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn dword(data: &[u8], at: usize) -> Result<u32> {
    let b = data.get(at..at.checked_add(4).ok_or(bad("DWORD offset overflow"))?).ok_or(bad("truncated DWORD"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn entry(data: &[u8], at: usize, wanted: Option<u16>) -> Result<Option<u32>> {
    let named = word(data, at.checked_add(12).ok_or(bad("directory overflow"))?)? as usize;
    let count = named + word(data, at + 14)? as usize;
    let start = at.checked_add(16).ok_or(bad("directory overflow"))?;
    let end = start.checked_add(count * 8).ok_or(bad("entry array overflow"))?;
    if end > data.len() {
        return Err(bad("resource entries outside directory"));
    }
    for index in 0..count {
        let p = start + index * 8;
        let id = dword(data, p)?;
        if wanted.is_none() || wanted.is_some_and(|w| id == w as u32) {
            return Ok(Some(dword(data, p + 4)?));
        }
    }
    Ok(None)
}
pub(crate) fn resource<'a>(
    pe: PeFile<'a>,
    kind: u16,
    name: Option<u16>,
    language: Option<u16>,
) -> Result<Option<&'a [u8]>> {
    let d = pe.directory(dir::RESOURCE);
    if !d.is_present() {
        return Ok(None);
    }
    let data =
        pe.slice_at_rva(d.virtual_address, d.size as usize).map_err(|_| bad("resource directory not file-backed"))?;
    let mut at = 0usize;
    for id in [Some(kind), name] {
        let Some(next) = entry(data, at, id)? else { return Ok(None) };
        if next & 0x8000_0000 == 0 {
            return Err(bad("resource tree has a premature leaf"));
        }
        at = (next & 0x7fff_ffff) as usize;
    }
    let Some(leaf) = entry(data, at, language)? else { return Ok(None) };
    if leaf & 0x8000_0000 != 0 {
        return Err(bad("resource tree exceeds type/name/language depth"));
    }
    let at = leaf as usize;
    if at.checked_add(16).is_none_or(|end| end > data.len()) {
        return Err(bad("resource data entry outside directory"));
    }
    let rva = dword(data, at)?;
    let len = dword(data, at + 4)?;
    pe.slice_at_rva(rva, len as usize).map(Some).map_err(|_| bad("resource payload not file-backed"))
}

/// A borrowed version value. Text lengths count UTF-16 units; binary
/// lengths count bytes, matching VerQueryValueW's documented convention.
#[derive(Debug, PartialEq, Eq)]
pub struct VersionValue<'a> {
    pub data: &'a [u8],
    pub length: u32,
    pub text: bool,
}
struct Block<'a> {
    data: &'a [u8],
    key_end: usize,
    value_at: usize,
    value_bytes: usize,
    units: u16,
    text: bool,
}
fn align4(at: usize) -> Result<usize> {
    at.checked_add(3).map(|n| n & !3).ok_or(bad("alignment overflow"))
}
impl<'a> Block<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let len = word(data, 0)? as usize;
        let data = data.get(..len).filter(|d| d.len() >= 6).ok_or(bad("invalid version block length"))?;
        let units = word(data, 2)?;
        let kind = word(data, 4)?;
        if kind > 1 {
            return Err(bad("invalid version value type"));
        }
        let mut end = 6;
        while word(data, end)? != 0 {
            end += 2;
        }
        let value_at = align4(end + 2)?;
        let value_bytes = units as usize * if kind == 1 { 2 } else { 1 };
        if value_bytes != 0 && value_at.checked_add(value_bytes).is_none_or(|n| n > data.len()) {
            return Err(bad("version value outside block"));
        }
        Ok(Self { data, key_end: end, value_at, value_bytes, units, text: kind == 1 })
    }
    fn key_is(&self, key: &str) -> bool {
        let mut target = key.encode_utf16();
        let fold = |c: u16| if (65..=90).contains(&c) { c + 32 } else { c };
        for pair in self.data[6..self.key_end].as_chunks::<2>().0 {
            if target.next().is_none_or(|c| fold(c) != fold(u16::from_le_bytes([pair[0], pair[1]]))) {
                return false;
            }
        }
        target.next().is_none()
    }
    fn child(&self, key: &str) -> Result<Option<Self>> {
        let mut at = align4(self.value_at + self.value_bytes)?;
        for _ in 0..4096 {
            if at >= self.data.len() {
                return Ok(None);
            }
            let child = Self::parse(&self.data[at..])?;
            if child.key_is(key) {
                return Ok(Some(child));
            }
            at = align4(at + child.data.len())?;
        }
        Err(bad("too many version child blocks"))
    }
}
pub fn version_value<'a>(data: &'a [u8], subblock: &str) -> Result<Option<VersionValue<'a>>> {
    let mut block = Block::parse(data)?;
    if !block.key_is("VS_VERSION_INFO") {
        return Err(bad("invalid version root key"));
    }
    for (depth, key) in subblock.split('\\').filter(|s| !s.is_empty()).enumerate() {
        if depth >= 16 {
            return Err(bad("version query too deep"));
        }
        let Some(child) = block.child(key)? else { return Ok(None) };
        block = child;
    }
    let value = if block.value_bytes == 0 {
        &block.data[0..0]
    } else {
        &block.data[block.value_at..block.value_at + block.value_bytes]
    };
    Ok(Some(VersionValue { data: value, length: block.units as u32, text: block.text }))
}
