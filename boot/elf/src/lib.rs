//! Validation of the OSyx kernel ELF64 image.
//!
//! The parser never indexes the input without a bounds check and uses checked
//! arithmetic for every offset/size computation, so arbitrary (malicious or
//! truncated) input yields an [`ElfError`] instead of a panic.
//!
//! Policy enforced for kernel images (beyond plain ELF validity):
//! * x86-64, little endian, `ET_EXEC`.
//! * Every `PT_LOAD` segment lies in the top 2 GiB (`kernel` code model).
//! * Segments are 4 KiB aligned, do not overlap, do not share pages, and are
//!   never both writable and executable (W^X).
//! * The entry point lies inside an executable segment.

#![no_std]

pub const PAGE_SIZE: u64 = 4096;
pub const KERNEL_SPACE_START: u64 = 0xFFFF_FFFF_8000_0000;
pub const MAX_SEGMENTS: usize = 16;

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_DYNAMIC: u32 = 2;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    BadVersion,
    WrongMachine(u16),
    NotExecutable(u16),
    BadProgramHeaderSize(u16),
    ProgramHeadersOutOfBounds,
    DynamicNotSupported,
    NoLoadableSegments,
    TooManySegments,
    SegmentFileRangeInvalid { index: usize },
    SegmentSizeInvalid { index: usize },
    SegmentNotPageAligned { index: usize },
    SegmentOutsideKernelSpace { index: usize },
    SegmentOverlap { index: usize },
    WritableAndExecutable { index: usize },
    EntryNotExecutable(u64),
}

impl ElfError {
    pub fn describe(&self) -> &'static str {
        match self {
            ElfError::TooSmall => "file is smaller than an ELF64 header",
            ElfError::BadMagic => "not an ELF file (bad magic)",
            ElfError::Not64Bit => "not an ELF64 file",
            ElfError::NotLittleEndian => "not little endian",
            ElfError::BadVersion => "unsupported ELF version",
            ElfError::WrongMachine(_) => "not an x86-64 image",
            ElfError::NotExecutable(_) => "not a static executable (ET_EXEC)",
            ElfError::BadProgramHeaderSize(_) => "unexpected program header size",
            ElfError::ProgramHeadersOutOfBounds => "program headers lie outside the file",
            ElfError::DynamicNotSupported => "dynamic linking is not supported",
            ElfError::NoLoadableSegments => "no loadable segments",
            ElfError::TooManySegments => "too many loadable segments",
            ElfError::SegmentFileRangeInvalid { .. } => "segment data lies outside the file",
            ElfError::SegmentSizeInvalid { .. } => "segment file size exceeds memory size",
            ElfError::SegmentNotPageAligned { .. } => "segment is not page aligned",
            ElfError::SegmentOutsideKernelSpace { .. } => "segment outside the kernel address range",
            ElfError::SegmentOverlap { .. } => "segments overlap or share a page",
            ElfError::WritableAndExecutable { .. } => "segment is writable and executable",
            ElfError::EntryNotExecutable(_) => "entry point is not in an executable segment",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Segment {
    pub vaddr: u64,
    pub mem_size: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub flags: u32,
}

impl Segment {
    pub fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }

    pub fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }

    /// Exclusive end of the segment rounded up to a page boundary.
    pub fn page_end(&self) -> u64 {
        // Cannot overflow: validated to stay below 2^64 - PAGE_SIZE.
        (self.vaddr + self.mem_size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KernelImage<'a> {
    data: &'a [u8],
    entry: u64,
    segments: [Segment; MAX_SEGMENTS],
    count: usize,
}

fn u16_at(d: &[u8], off: usize) -> Result<u16, ElfError> {
    let b = d.get(off..off.checked_add(2).ok_or(ElfError::TooSmall)?).ok_or(ElfError::TooSmall)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Result<u32, ElfError> {
    let b = d.get(off..off.checked_add(4).ok_or(ElfError::TooSmall)?).ok_or(ElfError::TooSmall)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(d: &[u8], off: usize) -> Result<u64, ElfError> {
    let b = d.get(off..off.checked_add(8).ok_or(ElfError::TooSmall)?).ok_or(ElfError::TooSmall)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Ok(u64::from_le_bytes(a))
}

impl<'a> KernelImage<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, ElfError> {
        if data.len() < 64 {
            return Err(ElfError::TooSmall);
        }
        if &data[0..4] != b"\x7fELF" {
            return Err(ElfError::BadMagic);
        }
        if data[4] != 2 {
            return Err(ElfError::Not64Bit);
        }
        if data[5] != 1 {
            return Err(ElfError::NotLittleEndian);
        }
        if data[6] != 1 || u32_at(data, 20)? != 1 {
            return Err(ElfError::BadVersion);
        }
        let e_type = u16_at(data, 16)?;
        let machine = u16_at(data, 18)?;
        if machine != 0x3E {
            return Err(ElfError::WrongMachine(machine));
        }
        if e_type != 2 {
            return Err(ElfError::NotExecutable(e_type));
        }
        let entry = u64_at(data, 24)?;
        let phoff = u64_at(data, 32)?;
        let phentsize = u16_at(data, 54)?;
        let phnum = u16_at(data, 56)? as u64;
        if phentsize != 56 {
            return Err(ElfError::BadProgramHeaderSize(phentsize));
        }
        let ph_end =
            phnum.checked_mul(56).and_then(|n| n.checked_add(phoff)).ok_or(ElfError::ProgramHeadersOutOfBounds)?;
        if ph_end > data.len() as u64 {
            return Err(ElfError::ProgramHeadersOutOfBounds);
        }

        let mut segments = [Segment::default(); MAX_SEGMENTS];
        let mut count = 0;
        for i in 0..phnum as usize {
            let base = phoff as usize + i * 56;
            let p_type = u32_at(data, base)?;
            if p_type == PT_DYNAMIC || p_type == PT_INTERP {
                return Err(ElfError::DynamicNotSupported);
            }
            if p_type != PT_LOAD {
                continue;
            }
            let seg = Segment {
                flags: u32_at(data, base + 4)?,
                file_offset: u64_at(data, base + 8)?,
                vaddr: u64_at(data, base + 16)?,
                file_size: u64_at(data, base + 32)?,
                mem_size: u64_at(data, base + 40)?,
            };
            if seg.mem_size == 0 {
                continue;
            }
            if count == MAX_SEGMENTS {
                return Err(ElfError::TooManySegments);
            }
            Self::check_segment(data, &seg, count)?;
            segments[count] = seg;
            count += 1;
        }
        if count == 0 {
            return Err(ElfError::NoLoadableSegments);
        }

        // Sort by address (insertion sort, at most MAX_SEGMENTS entries).
        for i in 1..count {
            let mut j = i;
            while j > 0 && segments[j - 1].vaddr > segments[j].vaddr {
                segments.swap(j - 1, j);
                j -= 1;
            }
        }
        for i in 1..count {
            // Segments start page aligned, so "shares a page" == "starts before
            // the previous segment's last page ends".
            if segments[i].vaddr < segments[i - 1].page_end() {
                return Err(ElfError::SegmentOverlap { index: i });
            }
        }
        let entry_ok =
            segments[..count].iter().any(|s| s.executable() && entry >= s.vaddr && entry < s.vaddr + s.mem_size);
        if !entry_ok {
            return Err(ElfError::EntryNotExecutable(entry));
        }
        Ok(KernelImage { data, entry, segments, count })
    }

    fn check_segment(data: &[u8], s: &Segment, index: usize) -> Result<(), ElfError> {
        let file_end = s.file_offset.checked_add(s.file_size);
        if file_end.is_none_or(|e| e > data.len() as u64) {
            return Err(ElfError::SegmentFileRangeInvalid { index });
        }
        if s.file_size > s.mem_size {
            return Err(ElfError::SegmentSizeInvalid { index });
        }
        if !s.vaddr.is_multiple_of(PAGE_SIZE) {
            return Err(ElfError::SegmentNotPageAligned { index });
        }
        let end = s.vaddr.checked_add(s.mem_size).and_then(|e| e.checked_add(PAGE_SIZE));
        if s.vaddr < KERNEL_SPACE_START || end.is_none() {
            return Err(ElfError::SegmentOutsideKernelSpace { index });
        }
        if s.writable() && s.executable() {
            return Err(ElfError::WritableAndExecutable { index });
        }
        Ok(())
    }

    pub fn entry(&self) -> u64 {
        self.entry
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.count]
    }

    /// Lowest virtual address of all segments (page aligned).
    pub fn virt_base(&self) -> u64 {
        self.segments[0].vaddr
    }

    /// Bytes from `virt_base` to the page-aligned end of the last segment.
    pub fn virt_span(&self) -> u64 {
        self.segments[self.count - 1].page_end() - self.virt_base()
    }

    /// File bytes backing `seg` (validated during parsing).
    pub fn file_bytes(&self, seg: &Segment) -> &'a [u8] {
        &self.data[seg.file_offset as usize..(seg.file_offset + seg.file_size) as usize]
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const BASE: u64 = KERNEL_SPACE_START;

    struct Ph {
        p_type: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        filesz: u64,
        memsz: u64,
    }

    fn load(flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64) -> Ph {
        Ph { p_type: PT_LOAD, flags, offset, vaddr, filesz, memsz }
    }

    fn build(entry: u64, phs: &[Ph], total: usize) -> Vec<u8> {
        let mut d = std::vec![0u8; total.max(64 + 56 * phs.len())];
        d[0..4].copy_from_slice(b"\x7fELF");
        d[4] = 2;
        d[5] = 1;
        d[6] = 1;
        d[16..18].copy_from_slice(&2u16.to_le_bytes());
        d[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        d[20..24].copy_from_slice(&1u32.to_le_bytes());
        d[24..32].copy_from_slice(&entry.to_le_bytes());
        d[32..40].copy_from_slice(&64u64.to_le_bytes());
        d[52..54].copy_from_slice(&64u16.to_le_bytes());
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
        for (i, p) in phs.iter().enumerate() {
            let b = 64 + i * 56;
            d[b..b + 4].copy_from_slice(&p.p_type.to_le_bytes());
            d[b + 4..b + 8].copy_from_slice(&p.flags.to_le_bytes());
            d[b + 8..b + 16].copy_from_slice(&p.offset.to_le_bytes());
            d[b + 16..b + 24].copy_from_slice(&p.vaddr.to_le_bytes());
            d[b + 32..b + 40].copy_from_slice(&p.filesz.to_le_bytes());
            d[b + 40..b + 48].copy_from_slice(&p.memsz.to_le_bytes());
        }
        d
    }

    fn good() -> Vec<u8> {
        build(
            BASE + 0x10,
            &[
                load(PF_R | PF_X, 0x1000, BASE, 0x800, 0x800),
                load(PF_R, 0x2000, BASE + 0x1000, 0x100, 0x100),
                load(PF_R | PF_W, 0x3000, BASE + 0x2000, 0x10, 0x5000),
            ],
            0x4000,
        )
    }

    #[test]
    fn accepts_valid_kernel() {
        let d = good();
        let k = KernelImage::parse(&d).unwrap();
        assert_eq!(k.entry(), BASE + 0x10);
        assert_eq!(k.segments().len(), 3);
        assert_eq!(k.virt_base(), BASE);
        assert_eq!(k.virt_span(), 0x7000);
        assert_eq!(k.file_bytes(&k.segments()[1]).len(), 0x100);
    }

    #[test]
    fn sorts_segments_by_address() {
        let d = build(
            BASE + 0x2000,
            &[load(PF_R | PF_W, 0x1000, BASE, 0x10, 0x10), load(PF_R | PF_X, 0x2000, BASE + 0x2000, 0x10, 0x10)],
            0x3000,
        );
        let d2 = build(
            BASE + 0x2000,
            &[load(PF_R | PF_X, 0x2000, BASE + 0x2000, 0x10, 0x10), load(PF_R | PF_W, 0x1000, BASE, 0x10, 0x10)],
            0x3000,
        );
        let a = KernelImage::parse(&d).unwrap();
        let b = KernelImage::parse(&d2).unwrap();
        assert_eq!(a.segments(), b.segments());
        assert_eq!(b.segments()[0].vaddr, BASE);
    }

    #[test]
    fn rejects_header_problems() {
        assert_eq!(KernelImage::parse(&[0; 10]).err(), Some(ElfError::TooSmall));
        let mut d = good();
        d[0] = b'M';
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::BadMagic));
        let mut d = good();
        d[4] = 1;
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::Not64Bit));
        let mut d = good();
        d[18] = 0xB7;
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::WrongMachine(0xB7)));
        let mut d = good();
        d[16] = 3;
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::NotExecutable(3)));
        let mut d = good();
        d[54] = 32;
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::BadProgramHeaderSize(32)));
    }

    #[test]
    fn rejects_program_headers_outside_file() {
        let mut d = good();
        d[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::ProgramHeadersOutOfBounds));
        let mut d = good();
        d[56..58].copy_from_slice(&0xFFFFu16.to_le_bytes());
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::ProgramHeadersOutOfBounds));
    }

    #[test]
    fn rejects_bad_segments() {
        let cases: &[(Ph, ElfError)] = &[
            (load(PF_R, 0x3F00, BASE, 0x200, 0x200), ElfError::SegmentFileRangeInvalid { index: 0 }),
            (load(PF_R, u64::MAX, BASE, 2, 2), ElfError::SegmentFileRangeInvalid { index: 0 }),
            (load(PF_R, 0, BASE, 0x20, 0x10), ElfError::SegmentSizeInvalid { index: 0 }),
            (load(PF_R, 0, BASE + 4, 0x10, 0x10), ElfError::SegmentNotPageAligned { index: 0 }),
            (load(PF_R, 0, 0x40_0000, 0x10, 0x10), ElfError::SegmentOutsideKernelSpace { index: 0 }),
            (load(PF_R, 0, BASE, 0, u64::MAX - BASE), ElfError::SegmentOutsideKernelSpace { index: 0 }),
            (load(PF_R | PF_W | PF_X, 0, BASE, 0x10, 0x10), ElfError::WritableAndExecutable { index: 0 }),
        ];
        for (ph, err) in cases {
            let d = build(BASE, core::slice::from_ref(ph), 0x4000);
            assert_eq!(KernelImage::parse(&d).err(), Some(*err));
        }
    }

    #[test]
    fn rejects_overlapping_and_page_sharing_segments() {
        let d = build(
            BASE,
            &[load(PF_R | PF_X, 0, BASE, 0x10, 0x1001), load(PF_R | PF_W, 0, BASE + 0x1000, 0x10, 0x10)],
            0x4000,
        );
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::SegmentOverlap { index: 1 }));
    }

    #[test]
    fn rejects_entry_outside_code_and_dynamic_images() {
        let d = build(BASE + 0x1000, &[load(PF_R | PF_X, 0, BASE, 0x10, 0x10)], 0x4000);
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::EntryNotExecutable(BASE + 0x1000)));
        let d = build(BASE, &[load(PF_R | PF_W, 0, BASE, 0x10, 0x10)], 0x4000);
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::EntryNotExecutable(BASE)));
        let mut dynamic = load(PF_R, 0, BASE, 0, 0);
        dynamic.p_type = PT_DYNAMIC;
        let d = build(BASE, &[dynamic], 0x4000);
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::DynamicNotSupported));
        let d = build(BASE, &[], 0x4000);
        assert_eq!(KernelImage::parse(&d).err(), Some(ElfError::NoLoadableSegments));
    }

    #[test]
    fn never_panics_on_truncation() {
        let d = good();
        for len in 0..d.len() {
            let _ = KernelImage::parse(&d[..len]);
        }
        // Flip every byte of the headers to a few values; must not panic.
        for i in 0..64 + 3 * 56 {
            for v in [0u8, 0x7F, 0xFF] {
                let mut m = d.clone();
                m[i] = v;
                let _ = KernelImage::parse(&m);
            }
        }
    }
}
