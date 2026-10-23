//! Base relocation directory: blocks of `(page RVA: u32, block size: u32)`
//! followed by 16-bit entries `type << 12 | offset`.

use crate::{dir, PeError, PeFile, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationKind {
    /// Padding; nothing to do.
    Absolute,
    /// 64-bit absolute address (the only real type in x64 images).
    Dir64,
    /// 32-bit absolute address (PE32 images).
    HighLow,
    /// Any other type; loader policy decides (OSyx rejects them).
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// RVA of the field to patch.
    pub rva: u32,
    pub kind: RelocationKind,
}

impl RelocationKind {
    /// Number of bytes the relocation patches.
    pub fn width(&self) -> u32 {
        match self {
            RelocationKind::Absolute => 0,
            RelocationKind::Dir64 => 8,
            RelocationKind::HighLow => 4,
            RelocationKind::Other(_) => 0,
        }
    }
}

/// Iterator over all relocations. Yields `Err` once on malformed data, then stops.
pub struct BaseRelocations<'a> {
    pe: PeFile<'a>,
    /// Next block header.
    block: u32,
    end: u32,
    /// Current block: page RVA, next entry RVA, entries end RVA.
    page: u32,
    entry: u32,
    entry_end: u32,
    done: bool,
}

fn bad(reason: &'static str) -> PeError {
    PeError::BadRelocation { reason }
}

pub(crate) fn parse(pe: PeFile<'_>) -> Result<BaseRelocations<'_>> {
    let d = pe.directory(dir::BASERELOC);
    let (start, end) = if d.is_present() { (d.virtual_address, d.virtual_address + d.size) } else { (0, 0) };
    Ok(BaseRelocations { pe, block: start, end, page: 0, entry: 0, entry_end: 0, done: !d.is_present() })
}

impl BaseRelocations<'_> {
    fn read(&mut self) -> Result<Option<Relocation>> {
        loop {
            if self.entry < self.entry_end {
                let raw = self.pe.u16_at_rva(self.entry).map_err(|_| bad("entry not in file"))?;
                self.entry += 2;
                let kind = match raw >> 12 {
                    0 => RelocationKind::Absolute,
                    3 => RelocationKind::HighLow,
                    10 => RelocationKind::Dir64,
                    t => RelocationKind::Other(t as u8),
                };
                let rva = self.page + (raw & 0xFFF) as u32;
                if kind.width() > 0 && rva.checked_add(kind.width()).is_none_or(|e| e > self.pe.size_of_image()) {
                    return Err(bad("target outside the image"));
                }
                return Ok(Some(Relocation { rva, kind }));
            }
            if self.block >= self.end {
                return Ok(None);
            }
            if self.end - self.block < 8 {
                return Err(bad("truncated block header"));
            }
            let page = self.pe.u32_at_rva(self.block).map_err(|_| bad("block header not in file"))?;
            let size = self.pe.u32_at_rva(self.block + 4).map_err(|_| bad("block header not in file"))?;
            if size < 8 || !size.is_multiple_of(2) {
                return Err(bad("invalid block size"));
            }
            if size > self.end - self.block {
                return Err(bad("block extends past the directory"));
            }
            if !page.is_multiple_of(0x1000) || page >= self.pe.size_of_image() {
                return Err(bad("block page not page-aligned inside the image"));
            }
            self.page = page;
            self.entry = self.block + 8;
            self.entry_end = self.block + size;
            self.block += size;
        }
    }
}

impl Iterator for BaseRelocations<'_> {
    type Item = Result<Relocation>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.read() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
