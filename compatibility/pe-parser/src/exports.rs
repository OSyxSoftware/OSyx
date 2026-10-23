//! Export directory: `IMAGE_EXPORT_DIRECTORY` with the export address table
//! (EAT), the name pointer table and the ordinal table. An EAT entry that
//! points inside the export directory is a forwarder string
//! (`"DLL.Function"` or `"DLL.#ordinal"`).

use crate::{dir, PeError, PeFile, Result};

const DIRECTORY_SIZE: usize = 40;
const MAX_FUNCTIONS: u32 = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportTarget<'a> {
    /// Code or data inside this image.
    Rva(u32),
    /// Resolved through another DLL: `dll` and either a `name` or an ordinal.
    Forwarder { dll: &'a str, name: Option<&'a str>, ordinal: Option<u16>, raw: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Export<'a> {
    pub ordinal: u16,
    pub name: Option<&'a str>,
    pub target: ExportTarget<'a>,
}

/// A validated export directory.
#[derive(Clone, Copy, Debug)]
pub struct Exports<'a> {
    pe: PeFile<'a>,
    pub dll_name: &'a str,
    pub ordinal_base: u32,
    pub time_date_stamp: u32,
    number_of_functions: u32,
    number_of_names: u32,
    functions_rva: u32,
    names_rva: u32,
    ordinals_rva: u32,
    dir_start: u32,
    dir_end: u32,
}

fn bad(reason: &'static str) -> PeError {
    PeError::BadExport { reason }
}

pub(crate) fn parse(pe: PeFile<'_>) -> Result<Option<Exports<'_>>> {
    let d = pe.directory(dir::EXPORT);
    if !d.is_present() {
        return Ok(None);
    }
    let raw = pe.slice_at_rva(d.virtual_address, DIRECTORY_SIZE).map_err(|_| bad("directory not in file"))?;
    let f = |i: usize| u32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
    let e = Exports {
        pe,
        dll_name: "",
        time_date_stamp: f(4),
        ordinal_base: f(16),
        number_of_functions: f(20),
        number_of_names: f(24),
        functions_rva: f(28),
        names_rva: f(32),
        ordinals_rva: f(36),
        dir_start: d.virtual_address,
        dir_end: d.virtual_address + d.size,
    };
    if e.number_of_functions > MAX_FUNCTIONS {
        return Err(bad("too many functions"));
    }
    if e.number_of_names > e.number_of_functions {
        return Err(bad("more names than functions"));
    }
    if e.ordinal_base.checked_add(e.number_of_functions).is_none_or(|end| end > 0x1_0000) {
        return Err(bad("ordinals exceed 16 bits"));
    }
    // The three tables must be fully file-backed.
    let table = |rva: u32, count: u32, width: usize, what| {
        if count == 0 {
            return Ok(());
        }
        pe.slice_at_rva(rva, count as usize * width).map(|_| ()).map_err(|_| bad(what))
    };
    table(e.functions_rva, e.number_of_functions, 4, "address table not in file")?;
    table(e.names_rva, e.number_of_names, 4, "name pointer table not in file")?;
    table(e.ordinals_rva, e.number_of_names, 2, "ordinal table not in file")?;
    let name_rva = f(12);
    let dll_name = if name_rva == 0 { "" } else { pe.cstr_at_rva(name_rva).map_err(|_| bad("DLL name invalid"))? };
    Ok(Some(Exports { dll_name, ..e }))
}

impl<'a> Exports<'a> {
    pub fn function_count(&self) -> u32 {
        self.number_of_functions
    }

    pub fn name_count(&self) -> u32 {
        self.number_of_names
    }

    /// Address table entry `index` (ordinal - base); `None` for unused slots.
    fn target(&self, index: u32) -> Result<Option<ExportTarget<'a>>> {
        let rva = self.pe.u32_at_rva(self.functions_rva + index * 4)?;
        if rva == 0 {
            return Ok(None);
        }
        if rva >= self.dir_start && rva < self.dir_end {
            let raw = self.pe.cstr_at_rva(rva).map_err(|_| bad("forwarder string invalid"))?;
            let (dll, sym) = raw.rsplit_once('.').ok_or(bad("forwarder without '.'"))?;
            if dll.is_empty() || sym.is_empty() {
                return Err(bad("forwarder has an empty part"));
            }
            return Ok(Some(match sym.strip_prefix('#') {
                Some(n) => {
                    let ordinal = n.parse::<u16>().map_err(|_| bad("forwarder ordinal invalid"))?;
                    ExportTarget::Forwarder { dll, name: None, ordinal: Some(ordinal), raw }
                }
                None => ExportTarget::Forwarder { dll, name: Some(sym), ordinal: None, raw },
            }));
        }
        if rva >= self.pe.size_of_image() {
            return Err(bad("export RVA outside the image"));
        }
        Ok(Some(ExportTarget::Rva(rva)))
    }

    /// Address table index that name table entry `i` refers to.
    fn name_index(&self, i: u32) -> Result<u32> {
        let index = self.pe.u16_at_rva(self.ordinals_rva + i * 2)? as u32;
        if index >= self.number_of_functions {
            return Err(bad("name refers to a non-existent function"));
        }
        Ok(index)
    }

    /// Name table entry `i`: (name, address table index).
    fn name_entry(&self, i: u32) -> Result<(&'a str, u32)> {
        let index = self.name_index(i)?;
        let name_rva = self.pe.u32_at_rva(self.names_rva + i * 4)?;
        let name = self.pe.cstr_at_rva(name_rva).map_err(|_| bad("export name invalid"))?;
        Ok((name, index))
    }

    /// Name of address table entry `index`, if any (linear scan of the
    /// ordinal table; only the matching name string is read).
    fn name_of(&self, index: u32) -> Result<Option<&'a str>> {
        for i in 0..self.number_of_names {
            if self.name_index(i)? == index {
                return Ok(Some(self.name_entry(i)?.0));
            }
        }
        Ok(None)
    }

    /// Looks up an export by name (binary search; names are sorted).
    pub fn by_name(&self, name: &str) -> Result<Option<Export<'a>>> {
        let (mut lo, mut hi) = (0u32, self.number_of_names);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (n, index) = self.name_entry(mid)?;
            match n.as_bytes().cmp(name.as_bytes()) {
                core::cmp::Ordering::Equal => {
                    let target = self.target(index)?.ok_or(bad("named export has no address"))?;
                    let ordinal = (self.ordinal_base + index) as u16;
                    return Ok(Some(Export { ordinal, name: Some(n), target }));
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Looks up an export by ordinal.
    pub fn by_ordinal(&self, ordinal: u16) -> Result<Option<Export<'a>>> {
        let Some(index) = (ordinal as u32).checked_sub(self.ordinal_base) else { return Ok(None) };
        if index >= self.number_of_functions {
            return Ok(None);
        }
        let Some(target) = self.target(index)? else { return Ok(None) };
        Ok(Some(Export { ordinal, name: self.name_of(index)?, target }))
    }

    /// Checks that the name table is sorted (required for binary search by
    /// the loader) and that every entry is valid.
    pub fn validate_names(&self) -> Result<()> {
        let mut prev: Option<&str> = None;
        for i in 0..self.number_of_names {
            let (n, _) = self.name_entry(i)?;
            if prev.is_some_and(|p| p.as_bytes() >= n.as_bytes()) {
                return Err(bad("name table not strictly sorted"));
            }
            prev = Some(n);
        }
        Ok(())
    }

    /// Every used slot of the address table in ordinal order, without names
    /// (linear time). Stops after the first error.
    pub fn functions(&self) -> impl Iterator<Item = Result<(u16, ExportTarget<'a>)>> + 'a {
        let e = *self;
        let mut failed = false;
        (0..e.number_of_functions).filter_map(move |index| {
            if failed {
                return None;
            }
            match e.target(index) {
                Ok(Some(t)) => Some(Ok(((e.ordinal_base + index) as u16, t))),
                Ok(None) => None,
                Err(err) => {
                    failed = true;
                    Some(Err(err))
                }
            }
        })
    }

    /// The name table in order: (name, ordinal) (linear time). Stops after
    /// the first error.
    pub fn names(&self) -> impl Iterator<Item = Result<(&'a str, u16)>> + 'a {
        let e = *self;
        let mut failed = false;
        (0..e.number_of_names).filter_map(move |i| {
            if failed {
                return None;
            }
            match e.name_entry(i) {
                Ok((n, index)) => Some(Ok((n, (e.ordinal_base + index) as u16))),
                Err(err) => {
                    failed = true;
                    Some(Err(err))
                }
            }
        })
    }

    /// Every used slot of the address table, in ordinal order, with its name
    /// if it has one. Finding each name scans the ordinal table, so this is
    /// quadratic; use [`Exports::functions`] and [`Exports::names`] for large
    /// tables. Stops after the first error.
    pub fn iter(&self) -> impl Iterator<Item = Result<Export<'a>>> + 'a {
        let e = *self;
        let mut failed = false;
        (0..e.number_of_functions).filter_map(move |index| {
            if failed {
                return None;
            }
            let r = (|| {
                let Some(target) = e.target(index)? else { return Ok(None) };
                Ok(Some(Export { ordinal: (e.ordinal_base + index) as u16, name: e.name_of(index)?, target }))
            })();
            match r {
                Ok(Some(x)) => Some(Ok(x)),
                Ok(None) => None,
                Err(err) => {
                    failed = true;
                    Some(Err(err))
                }
            }
        })
    }
}
