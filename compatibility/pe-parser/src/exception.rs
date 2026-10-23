//! Exception directory (`.pdata`): an array of x64 `RUNTIME_FUNCTION`
//! entries `(begin RVA, end RVA, unwind info RVA)`, which must be sorted and
//! non-overlapping so the unwinder can binary-search it, plus the header of
//! each `UNWIND_INFO` record.

use crate::{dir, PeError, PeFile, Result};

const ENTRY_SIZE: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub begin: u32,
    pub end: u32,
    pub unwind_info: u32,
}

/// Header of an `UNWIND_INFO` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnwindInfo {
    pub version: u8,
    /// UNW_FLAG_EHANDLER = 1, UNW_FLAG_UHANDLER = 2, UNW_FLAG_CHAININFO = 4.
    pub flags: u8,
    pub size_of_prolog: u8,
    pub count_of_codes: u8,
    pub frame_register: u8,
    pub frame_offset: u8,
}

fn bad(reason: &'static str) -> PeError {
    PeError::BadExceptionData { reason }
}

/// The validated function table.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFunctions<'a> {
    pe: PeFile<'a>,
    start: u32,
    count: u32,
}

pub(crate) fn parse(pe: PeFile<'_>) -> Result<RuntimeFunctions<'_>> {
    let d = pe.directory(dir::EXCEPTION);
    if !d.is_present() {
        return Ok(RuntimeFunctions { pe, start: 0, count: 0 });
    }
    if !d.size.is_multiple_of(ENTRY_SIZE) {
        return Err(bad("directory size is not a multiple of 12"));
    }
    pe.slice_at_rva(d.virtual_address, d.size as usize).map_err(|_| bad("table not in file"))?;
    let t = RuntimeFunctions { pe, start: d.virtual_address, count: d.size / ENTRY_SIZE };
    let mut prev_end = 0;
    for i in 0..t.count {
        let f = t.get(i)?;
        // Zero-length entries (begin == end) occur in shipped Windows
        // binaries (e.g. symcryptk.dll) and are harmless for lookup.
        if f.begin > f.end {
            return Err(bad("function ends before it begins"));
        }
        if f.end > pe.size_of_image() || f.unwind_info >= pe.size_of_image() {
            return Err(bad("entry outside the image"));
        }
        if f.begin < prev_end {
            return Err(bad("entries unsorted or overlapping"));
        }
        prev_end = f.end;
    }
    Ok(t)
}

impl<'a> RuntimeFunctions<'a> {
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn get(&self, i: u32) -> Result<RuntimeFunction> {
        if i >= self.count {
            return Err(bad("index out of range"));
        }
        let at = self.start + i * ENTRY_SIZE;
        Ok(RuntimeFunction {
            begin: self.pe.u32_at_rva(at)?,
            end: self.pe.u32_at_rva(at + 4)?,
            unwind_info: self.pe.u32_at_rva(at + 8)?,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = RuntimeFunction> + 'a {
        let t = *self;
        (0..t.count).filter_map(move |i| t.get(i).ok())
    }

    /// The function containing `rva` (binary search, like the unwinder).
    pub fn lookup(&self, rva: u32) -> Result<Option<RuntimeFunction>> {
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let f = self.get(mid)?;
            if rva < f.begin {
                hi = mid;
            } else if rva >= f.end {
                lo = mid + 1;
            } else {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    /// Parses the `UNWIND_INFO` header of `f` and checks that its unwind
    /// codes are file-backed.
    pub fn unwind_info(&self, f: &RuntimeFunction) -> Result<UnwindInfo> {
        // Bit 0 set marks a chained entry pointing at another RUNTIME_FUNCTION
        // (used by some compilers); the header is then not an UNWIND_INFO.
        if f.unwind_info & 1 != 0 {
            return Err(bad("indirect unwind entry"));
        }
        let h = self.pe.slice_at_rva(f.unwind_info, 4).map_err(|_| bad("unwind info not in file"))?;
        let info = UnwindInfo {
            version: h[0] & 7,
            flags: h[0] >> 3,
            size_of_prolog: h[1],
            count_of_codes: h[2],
            frame_register: h[3] & 0xF,
            frame_offset: h[3] >> 4,
        };
        if info.version != 1 && info.version != 2 {
            return Err(bad("unknown unwind info version"));
        }
        // Codes are 2 bytes each, padded to an even count.
        let codes = (info.count_of_codes as usize).next_multiple_of(2) * 2;
        let codes_rva = f.unwind_info.checked_add(4).ok_or(bad("unwind info overflows"))?;
        self.pe.slice_at_rva(codes_rva, codes).map_err(|_| bad("unwind codes not in file"))?;
        Ok(info)
    }
}
