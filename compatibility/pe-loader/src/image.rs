//! Turns a parsed PE32+ file into its in-memory image: headers and sections
//! laid out at their RVAs (zero-filled tails), base relocations applied for
//! the address it is loaded at, and the page protections each part needs.
//! Nothing here touches real memory; the caller copies the image into the
//! address space and applies the protections.

use alloc::vec::Vec;
use osyx_pe::{PeFile, RelocationKind};

pub const PAGE: u32 = 4096;

pub const SUBSYSTEM_GUI: u16 = 2;
pub const SUBSYSTEM_CONSOLE: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    Parse(osyx_pe::PeError),
    /// Not an x86-64 image.
    Machine(u16),
    /// Executables must be console or GUI programs.
    Subsystem(u16),
    /// The image has no relocations and cannot go where it was linked.
    NotRelocatable,
    /// A relocation type OSyx does not apply.
    Relocation(u8),
    /// A 32-bit relocation would truncate the new address.
    RelocationOverflow(u32),
    TooLarge,
    /// A section is both writable and executable (OSyx enforces W^X).
    WritableCode(u32),
}

impl From<osyx_pe::PeError> for ImageError {
    fn from(e: osyx_pe::PeError) -> Self {
        ImageError::Parse(e)
    }
}

/// Page protection of one part of the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protection {
    pub rva: u32,
    pub size: u32,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

/// Largest image accepted (the user address space and memory are finite).
pub const MAX_IMAGE: u32 = 512 << 20;

/// Checks that the file is something OSyx can run or load.
pub fn check(pe: &PeFile<'_>, want_dll: bool) -> Result<(), ImageError> {
    if pe.machine() != osyx_pe::MACHINE_AMD64 {
        return Err(ImageError::Machine(pe.machine()));
    }
    if !want_dll {
        let s = pe.optional().subsystem;
        if s != SUBSYSTEM_CONSOLE && s != SUBSYSTEM_GUI {
            return Err(ImageError::Subsystem(s));
        }
    }
    if pe.size_of_image() > MAX_IMAGE {
        return Err(ImageError::TooLarge);
    }
    for s in pe.sections() {
        if s.is_writable() && s.is_executable() {
            return Err(ImageError::WritableCode(s.virtual_address));
        }
    }
    Ok(())
}

/// Lays the file out as it appears in memory (`size_of_image` bytes).
pub fn layout(pe: &PeFile<'_>) -> Result<Vec<u8>, ImageError> {
    let mut img = alloc::vec![0u8; pe.size_of_image() as usize];
    let hdr = (pe.optional().size_of_headers as usize).min(pe.data().len()).min(img.len());
    img[..hdr].copy_from_slice(&pe.data()[..hdr]);
    for s in pe.sections() {
        let n = s.file_size() as usize;
        if n == 0 {
            continue;
        }
        let src = pe.slice_at_rva(s.virtual_address, n)?;
        let dst = s.virtual_address as usize;
        img.get_mut(dst..dst + n).ok_or(ImageError::TooLarge)?.copy_from_slice(src);
    }
    Ok(img)
}

const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

/// Whether the image may be loaded away from its linked base. An image
/// without a relocation directory qualifies when it is marked
/// dynamic-base and its relocations were not stripped: it simply has no
/// absolute addresses (typical for small RIP-relative x64 programs).
pub fn relocatable(pe: &PeFile<'_>) -> bool {
    if pe.directory(osyx_pe::dir::BASERELOC).is_present() {
        return true;
    }
    pe.coff().characteristics & IMAGE_FILE_RELOCS_STRIPPED == 0 && pe.optional().dll_characteristics & osyx_pe::DLLCHAR_DYNAMIC_BASE != 0
}

/// Applies base relocations for loading at `base` instead of the linked
/// image base. Returns how many fields were patched.
pub fn relocate(pe: &PeFile<'_>, img: &mut [u8], base: u64) -> Result<usize, ImageError> {
    let delta = base.wrapping_sub(pe.image_base());
    if delta == 0 {
        return Ok(0);
    }
    if !relocatable(pe) {
        return Err(ImageError::NotRelocatable);
    }
    if !pe.directory(osyx_pe::dir::BASERELOC).is_present() {
        return Ok(0);
    }
    let mut n = 0;
    for r in pe.relocations()? {
        let r = r?;
        let at = r.rva as usize;
        match r.kind {
            RelocationKind::Absolute => {}
            RelocationKind::Dir64 => {
                let f = img.get_mut(at..at + 8).ok_or(ImageError::TooLarge)?;
                let v = u64::from_le_bytes(f.try_into().expect("8 bytes")).wrapping_add(delta);
                f.copy_from_slice(&v.to_le_bytes());
                n += 1;
            }
            RelocationKind::HighLow => {
                let f = img.get_mut(at..at + 4).ok_or(ImageError::TooLarge)?;
                let v = u32::from_le_bytes(f.try_into().expect("4 bytes")) as u64;
                let nv = v.wrapping_add(delta);
                if nv > u32::MAX as u64 {
                    return Err(ImageError::RelocationOverflow(r.rva));
                }
                f.copy_from_slice(&(nv as u32).to_le_bytes());
                n += 1;
            }
            RelocationKind::Other(t) => return Err(ImageError::Relocation(t)),
        }
    }
    Ok(n)
}

/// Protections in RVA order: the headers read-only, each section by its
/// flags (pages shared by two sections get the union, W^X kept: a page
/// that would be both writable and executable is rejected).
pub fn protections(pe: &PeFile<'_>) -> Result<Vec<Protection>, ImageError> {
    let pages = pe.size_of_image().div_ceil(PAGE) as usize;
    // Per page: bit 0 read, 1 write, 2 execute.
    let mut p = alloc::vec![0u8; pages];
    let hdr_pages = pe.optional().size_of_headers.div_ceil(PAGE).max(1) as usize;
    for b in p.iter_mut().take(hdr_pages) {
        *b = 1;
    }
    for s in pe.sections() {
        let size = s.mapped_size();
        if size == 0 {
            continue;
        }
        let bits = (s.is_readable() as u8) | (s.is_writable() as u8) << 1 | (s.is_executable() as u8) << 2;
        let first = (s.virtual_address / PAGE) as usize;
        let last = ((s.virtual_address as u64 + size as u64).div_ceil(PAGE as u64) as usize).min(pages);
        for b in &mut p[first..last] {
            *b |= bits;
        }
    }
    let mut out: Vec<Protection> = Vec::new();
    for (i, &b) in p.iter().enumerate() {
        if b & 6 == 6 {
            return Err(ImageError::WritableCode(i as u32 * PAGE));
        }
        let prot = Protection { rva: i as u32 * PAGE, size: PAGE, read: b != 0, write: b & 2 != 0, execute: b & 4 != 0 };
        match out.last_mut() {
            Some(l) if (l.read, l.write, l.execute) == (prot.read, prot.write, prot.execute) => l.size += PAGE,
            _ => out.push(prot),
        }
    }
    Ok(out)
}

pub fn read_u64(img: &[u8], rva: u32) -> Option<u64> {
    img.get(rva as usize..rva as usize + 8).map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes")))
}

pub fn write_u64(img: &mut [u8], rva: u32, v: u64) -> Option<()> {
    img.get_mut(rva as usize..rva as usize + 8)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}
