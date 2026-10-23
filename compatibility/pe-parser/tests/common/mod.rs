//! Shared test helpers: an in-memory PE32+ builder for precise malformed
//! inputs, and `walk`, which exercises every accessor of the parser.
#![allow(dead_code)]

use osyx_pe::{dir, PeFile};

pub const IMAGE_BASE: u64 = 0x1_4000_0000;
pub const TEXT_RVA: u32 = 0x1000;
pub const RDATA_RVA: u32 = 0x2000;
pub const DATA_RVA: u32 = 0x3000;
pub const IMAGE_SIZE: u32 = 0x4000;
pub const RDATA_FILE: usize = 0x600;
pub const RDATA_LEN: usize = 0x1000;

pub const E_LFANEW: usize = 0x40;
pub const COFF: usize = E_LFANEW + 4;
pub const OPT: usize = COFF + 20;
pub const DIRS: usize = OPT + 112;
pub const SECTIONS: usize = OPT + 240;

/// A valid PE32+ image: `.text` (RX) at 0x1000, `.rdata` (R, 4 KiB of file
/// data tests can fill) at 0x2000, `.data` (RW, 0x200 bytes in the file, 0x1000
/// in memory) at 0x3000.
pub struct Builder {
    pub bytes: Vec<u8>,
}

impl Builder {
    pub fn new() -> Builder {
        let mut b = vec![0u8; RDATA_FILE + RDATA_LEN + 0x200];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
        b[E_LFANEW..E_LFANEW + 4].copy_from_slice(b"PE\0\0");
        let mut s = Builder { bytes: b };
        s.u16(COFF, 0x8664); // machine
        s.u16(COFF + 2, 3); // sections
        s.u16(COFF + 16, 240); // size of optional header
        s.u16(COFF + 18, 0x0022); // executable, large address aware
        s.u16(OPT, 0x20B);
        s.u32(OPT + 16, TEXT_RVA); // entry point
        s.u32(OPT + 20, TEXT_RVA); // base of code
        s.u64(OPT + 24, IMAGE_BASE);
        s.u32(OPT + 32, 0x1000); // section alignment
        s.u32(OPT + 36, 0x200); // file alignment
        s.u16(OPT + 48, 6); // subsystem version
        s.u32(OPT + 56, IMAGE_SIZE);
        s.u32(OPT + 60, 0x400); // size of headers
        s.u16(OPT + 68, 3); // console
        s.u16(OPT + 70, 0x0160); // high entropy VA, dynamic base, NX
        s.u64(OPT + 72, 0x10_0000);
        s.u64(OPT + 80, 0x1000);
        s.u64(OPT + 88, 0x10_0000);
        s.u64(OPT + 96, 0x1000);
        s.u32(OPT + 108, 16); // data directories
        s.section(0, ".text", 0x10, TEXT_RVA, 0x200, 0x400, 0x6000_0020);
        s.section(1, ".rdata", RDATA_LEN as u32, RDATA_RVA, RDATA_LEN as u32, RDATA_FILE as u32, 0x4000_0040);
        s.section(2, ".data", 0x1000, DATA_RVA, 0x200, (RDATA_FILE + RDATA_LEN) as u32, 0xC000_0040);
        s.bytes[0x400] = 0xC3; // ret
        s
    }

    #[allow(clippy::too_many_arguments)]
    pub fn section(&mut self, i: usize, name: &str, vsize: u32, va: u32, raw_size: u32, raw_ptr: u32, chars: u32) {
        let at = SECTIONS + i * 40;
        self.bytes[at..at + 8].fill(0);
        self.bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
        self.u32(at + 8, vsize);
        self.u32(at + 12, va);
        self.u32(at + 16, raw_size);
        self.u32(at + 20, raw_ptr);
        self.u32(at + 36, chars);
    }

    pub fn u16(&mut self, at: usize, v: u16) {
        self.bytes[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, at: usize, v: u32) {
        self.bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, at: usize, v: u64) {
        self.bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// File offset of `.rdata` byte `off` (RVA `RDATA_RVA + off`).
    pub fn rd(off: u32) -> usize {
        RDATA_FILE + off as usize
    }

    pub fn rd_u16(&mut self, off: u32, v: u16) {
        self.u16(Self::rd(off), v);
    }

    pub fn rd_u32(&mut self, off: u32, v: u32) {
        self.u32(Self::rd(off), v);
    }

    pub fn rd_u64(&mut self, off: u32, v: u64) {
        self.u64(Self::rd(off), v);
    }

    pub fn rd_str(&mut self, off: u32, s: &str) {
        let at = Self::rd(off);
        self.bytes[at..at + s.len()].copy_from_slice(s.as_bytes());
        self.bytes[at + s.len()] = 0;
    }

    pub fn dir(&mut self, index: usize, rva: u32, size: u32) {
        self.u32(DIRS + index * 8, rva);
        self.u32(DIRS + index * 8 + 4, size);
    }

    pub fn parse(&self) -> PeFile<'_> {
        PeFile::parse(&self.bytes).expect("builder image must parse")
    }
}

/// Calls every accessor and iterator; must never panic on any input.
pub fn walk(pe: &PeFile) -> usize {
    let mut n = 0;
    for s in pe.sections() {
        n += s.name().len();
        let _ = pe.rva_to_offset(s.virtual_address);
        let _ = pe.slice_at_rva(s.virtual_address, s.file_size() as usize);
    }
    for rva in [0, 1, 0x3C, 0x400, 0x1000, 0x1FFF, 0x2000, 0x7FFF_FFFF, u32::MAX] {
        let _ = pe.rva_to_offset(rva);
        let _ = pe.slice_at_rva(rva, 16);
        let _ = pe.cstr_at_rva(rva);
    }
    if let Ok(it) = pe.imports() {
        for imp in it.flatten() {
            n += imp.entries().count();
        }
    }
    if let Ok(Some(ex)) = pe.exports() {
        let _ = ex.validate_names();
        for (ordinal, _) in ex.functions().flatten() {
            let _ = ex.by_ordinal(ordinal);
            n += 1;
        }
        for (name, _) in ex.names().flatten() {
            let _ = ex.by_name(name);
        }
        n += ex.iter().take(64).count();
        let _ = ex.by_name("");
        let _ = ex.by_ordinal(0);
        let _ = ex.by_ordinal(u16::MAX);
    }
    if let Ok(r) = pe.relocations() {
        n += r.count();
    }
    if let Ok(Some(t)) = pe.tls() {
        let _ = t.template();
        n += t.callbacks().count();
    }
    if let Ok(f) = pe.runtime_functions() {
        for rf in f.iter() {
            let _ = f.unwind_info(&rf);
            let _ = f.lookup(rf.begin);
        }
        let _ = f.lookup(u32::MAX);
    }
    for i in 0..17 {
        let _ = pe.directory(i);
    }
    let _ = pe.directory(dir::RESERVED);
    n
}
