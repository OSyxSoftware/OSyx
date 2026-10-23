//! TLS directory (`IMAGE_TLS_DIRECTORY64`). Its fields are virtual addresses
//! (image base included), converted here to RVAs inside the image.

use crate::{dir, PeError, PeFile, Result};

const DIRECTORY_SIZE: usize = 40;
const MAX_CALLBACKS: u32 = 1024;

#[derive(Clone, Copy, Debug)]
pub struct TlsInfo<'a> {
    pe: PeFile<'a>,
    /// Template data copied into each thread's TLS block: `[start, end)`.
    pub raw_data_start_rva: u32,
    pub raw_data_end_rva: u32,
    /// Where the loader stores the module's TLS index.
    pub index_rva: u32,
    /// Null-terminated array of callback VAs (0 = no callbacks).
    pub callbacks_rva: u32,
    pub size_of_zero_fill: u32,
    pub characteristics: u32,
}

fn bad(reason: &'static str) -> PeError {
    PeError::BadTls { reason }
}

pub(crate) fn parse(pe: PeFile<'_>) -> Result<Option<TlsInfo<'_>>> {
    let d = pe.directory(dir::TLS);
    if !d.is_present() {
        return Ok(None);
    }
    if (d.size as usize) < DIRECTORY_SIZE {
        return Err(bad("directory smaller than IMAGE_TLS_DIRECTORY64"));
    }
    let raw = pe.slice_at_rva(d.virtual_address, DIRECTORY_SIZE).map_err(|_| bad("directory not in file"))?;
    let q = |i: usize| {
        let mut a = [0u8; 8];
        a.copy_from_slice(&raw[i..i + 8]);
        u64::from_le_bytes(a)
    };
    let l = |i: usize| u32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
    let to_rva = |va: u64, what| if va == 0 { Ok(0) } else { pe.va_to_rva(va).ok_or(bad(what)) };
    let start = to_rva(q(0), "raw data start outside the image")?;
    let end = to_rva(q(8), "raw data end outside the image")?;
    if end < start {
        return Err(bad("raw data end before start"));
    }
    let index_rva = to_rva(q(16), "index address outside the image")?;
    if index_rva != 0 && index_rva.checked_add(4).is_none_or(|e| e > pe.size_of_image()) {
        return Err(bad("index slot outside the image"));
    }
    let callbacks_rva = to_rva(q(24), "callback array outside the image")?;
    Ok(Some(TlsInfo {
        pe,
        raw_data_start_rva: start,
        raw_data_end_rva: end,
        index_rva,
        callbacks_rva,
        size_of_zero_fill: l(32),
        characteristics: l(36),
    }))
}

impl<'a> TlsInfo<'a> {
    /// RVAs of the TLS callbacks, in call order.
    pub fn callbacks(&self) -> TlsCallbacks<'a> {
        TlsCallbacks { pe: self.pe, next: self.callbacks_rva, count: 0, done: self.callbacks_rva == 0 }
    }

    /// The initialised TLS template, if it is file-backed.
    pub fn template(&self) -> Result<&'a [u8]> {
        let len = (self.raw_data_end_rva - self.raw_data_start_rva) as usize;
        if len == 0 {
            return Ok(&[]);
        }
        self.pe.slice_at_rva(self.raw_data_start_rva, len).map_err(|_| bad("template not in file"))
    }
}

pub struct TlsCallbacks<'a> {
    pe: PeFile<'a>,
    next: u32,
    count: u32,
    done: bool,
}

impl Iterator for TlsCallbacks<'_> {
    type Item = Result<u32>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let r = (|| {
            if self.count == MAX_CALLBACKS {
                return Err(bad("callback array not terminated"));
            }
            let va = self.pe.u64_at_rva(self.next).map_err(|_| bad("callback array not in file"))?;
            if va == 0 {
                return Ok(None);
            }
            let rva = self.pe.va_to_rva(va).ok_or(bad("callback outside the image"))?;
            self.next = self.next.checked_add(8).ok_or(bad("callback array overflows"))?;
            self.count += 1;
            Ok(Some(rva))
        })();
        match r {
            Ok(Some(rva)) => Some(Ok(rva)),
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
