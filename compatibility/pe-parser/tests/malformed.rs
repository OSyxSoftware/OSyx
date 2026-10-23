//! Precisely malformed images built in memory: each test corrupts one field
//! of an otherwise valid PE32+ and checks the exact error.

mod common;

use common::*;
use osyx_pe::{dir, ExportTarget, ImportEntry, PeError, PeFile, RelocationKind};

/// A named corruption applied to a valid builder image.
type Case = (&'static str, fn(&mut Builder));

fn err(b: &Builder) -> PeError {
    PeFile::parse(&b.bytes).expect_err("image must be rejected")
}

#[test]
fn builder_image_is_valid() {
    let b = Builder::new();
    let pe = b.parse();
    assert_eq!(pe.section_count(), 3);
    assert_eq!(pe.entry_point(), TEXT_RVA);
    assert!(walk(&pe) > 0);
}

// ---- DOS / PE headers ---------------------------------------------------------

#[test]
fn rejects_short_and_non_pe_files() {
    assert_eq!(PeFile::parse(&[]).unwrap_err(), PeError::Truncated { what: "DOS header" });
    assert_eq!(PeFile::parse(b"MZ").unwrap_err(), PeError::Truncated { what: "DOS header" });
    let mut b = Builder::new();
    b.bytes[0] = b'Z';
    assert_eq!(err(&b), PeError::BadDosSignature);
    let elf = b"\x7fELF".iter().copied().chain(core::iter::repeat_n(0, 60)).collect::<Vec<u8>>();
    assert_eq!(PeFile::parse(&elf).unwrap_err(), PeError::BadDosSignature);
}

#[test]
fn rejects_bad_e_lfanew() {
    for v in [0u32, 2, 0x10_0000, u32::MAX, u32::MAX - 3] {
        let mut b = Builder::new();
        b.u32(0x3C, v);
        let e = err(&b);
        assert!(matches!(e, PeError::BadPeOffset(_) | PeError::BadPeSignature), "e_lfanew {v:#x}: {e:?}");
    }
    let mut b = Builder::new();
    let len = b.bytes.len() as u32;
    b.u32(0x3C, len - 10); // signature fits, COFF header does not
    assert_eq!(err(&b), PeError::BadPeOffset(len - 10));
}

#[test]
fn rejects_bad_signature_and_pe32() {
    let mut b = Builder::new();
    b.bytes[E_LFANEW + 2] = b'X';
    assert_eq!(err(&b), PeError::BadPeSignature);
    let mut b = Builder::new();
    b.u16(OPT, 0x10B);
    assert_eq!(err(&b), PeError::NotPe32Plus { magic: 0x10B });
    let mut b = Builder::new();
    b.u16(OPT, 0x107); // ROM image
    assert_eq!(err(&b), PeError::NotPe32Plus { magic: 0x107 });
}

#[test]
fn rejects_bad_optional_header_and_directory_count() {
    let mut b = Builder::new();
    b.u16(COFF + 16, 100);
    assert_eq!(err(&b), PeError::BadOptionalHeaderSize(100));
    let mut b = Builder::new();
    b.u32(OPT + 108, 17);
    assert_eq!(err(&b), PeError::TooManyDataDirectories(17));
    let mut b = Builder::new();
    b.u16(COFF + 16, 112 + 8 * 8); // room for 8 directories only
    assert_eq!(err(&b), PeError::BadOptionalHeaderSize(176));
    let mut b = Builder::new();
    b.u16(COFF + 16, 0xFFF0); // runs past the end of the file
    assert_eq!(err(&b), PeError::Truncated { what: "optional header" });
}

#[test]
fn fewer_data_directories_are_zero() {
    let mut b = Builder::new();
    b.dir(dir::TLS, RDATA_RVA, 40);
    b.u32(OPT + 108, 6); // only directories 0..5 exist
    let pe = b.parse();
    assert!(!pe.directory(dir::TLS).is_present());
    assert!(pe.tls().unwrap().is_none());
}

#[test]
fn rejects_bad_section_counts_and_table() {
    let mut b = Builder::new();
    b.u16(COFF + 2, 0);
    assert_eq!(err(&b), PeError::NoSections);
    let mut b = Builder::new();
    b.u16(COFF + 2, 97);
    assert_eq!(err(&b), PeError::TooManySections(97));
    let mut b = Builder::new();
    b.bytes.truncate(SECTIONS + 50); // table cut in the middle
    assert_eq!(err(&b), PeError::Truncated { what: "section table" });
}

#[test]
fn rejects_bad_alignment_and_sizes() {
    for (sa, fa) in [(0x1000, 0x300), (0x1000, 0x100), (0x800, 0x1000), (0x1800, 0x200), (0, 0)] {
        let mut b = Builder::new();
        b.u32(OPT + 32, sa);
        b.u32(OPT + 36, fa);
        assert!(matches!(err(&b), PeError::BadAlignment { .. }), "sa {sa:#x} fa {fa:#x}");
    }
    let mut b = Builder::new();
    b.u32(OPT + 56, IMAGE_SIZE + 0x10);
    assert_eq!(err(&b), PeError::BadSizeOfImage(IMAGE_SIZE + 0x10));
    let mut b = Builder::new();
    b.u32(OPT + 56, 0);
    assert_eq!(err(&b), PeError::BadSizeOfImage(0));
    let mut b = Builder::new();
    b.u32(OPT + 60, 0x100); // smaller than the end of the section table
    assert_eq!(err(&b), PeError::BadSizeOfHeaders(0x100));
    let mut b = Builder::new();
    b.u32(OPT + 60, 0x10_0000);
    assert_eq!(err(&b), PeError::BadSizeOfHeaders(0x10_0000));
}

#[test]
fn rejects_corrupted_sections() {
    let cases: [Case; 6] = [
        ("misaligned", |b| b.section(1, ".rdata", 0x1000, RDATA_RVA + 0x10, 0x1000, RDATA_FILE as u32, 0x4000_0040)),
        ("overlaps previous", |b| b.section(1, ".rdata", 0x1000, TEXT_RVA, 0x1000, RDATA_FILE as u32, 0x4000_0040)),
        ("overlaps headers", |b| b.section(0, ".text", 0x10, 0, 0x200, 0x400, 0x6000_0020)),
        ("past SizeOfImage", |b| b.section(2, ".data", 0x2000, DATA_RVA, 0x200, 0x1600, 0xC000_0040)),
        ("raw data past EOF", |b| b.section(2, ".data", 0x1000, DATA_RVA, 0x400, 0x1600, 0xC000_0040)),
        ("virtual overflow", |b| b.section(2, ".data", u32::MAX, DATA_RVA, 0x200, 0x1600, 0xC000_0040)),
    ];
    for (what, corrupt) in cases {
        let mut b = Builder::new();
        corrupt(&mut b);
        assert!(matches!(err(&b), PeError::BadSection { .. }), "{what}: {:?}", err(&b));
    }
}

#[test]
fn rejects_entry_point_and_directories_outside_image() {
    let mut b = Builder::new();
    b.u32(OPT + 16, IMAGE_SIZE);
    assert_eq!(err(&b), PeError::EntryPointOutsideImage(IMAGE_SIZE));
    let mut b = Builder::new();
    b.u32(OPT + 16, 0); // no entry point (legal for DLLs)
    assert_eq!(b.parse().entry_point(), 0);
    let mut b = Builder::new();
    b.dir(dir::IMPORT, RDATA_RVA, IMAGE_SIZE);
    assert_eq!(err(&b), PeError::BadDataDirectory { index: dir::IMPORT });
    let mut b = Builder::new();
    b.dir(dir::EXPORT, u32::MAX - 4, 0x10); // rva + size overflows
    assert_eq!(err(&b), PeError::BadDataDirectory { index: dir::EXPORT });
}

#[test]
fn certificate_table_is_a_file_offset() {
    let mut b = Builder::new();
    let len = b.bytes.len() as u32;
    b.dir(dir::SECURITY, len - 0x10, 0x10); // inside the file, past SizeOfImage? no: file < image here
    assert!(PeFile::parse(&b.bytes).is_ok());
    let mut b = Builder::new();
    b.dir(dir::SECURITY, len - 0x10, 0x20);
    assert_eq!(err(&b), PeError::BadDataDirectory { index: dir::SECURITY });
}

#[test]
fn rva_translation() {
    let b = Builder::new();
    let pe = b.parse();
    assert_eq!(pe.rva_to_offset(0x3C), Some(0x3C));
    assert_eq!(pe.rva_to_offset(RDATA_RVA + 0x123), Some(RDATA_FILE + 0x123));
    assert_eq!(pe.rva_to_offset(DATA_RVA + 0x1FF), Some(RDATA_FILE + RDATA_LEN + 0x1FF));
    assert_eq!(pe.rva_to_offset(DATA_RVA + 0x200), None, "zero-filled tail of .data");
    assert_eq!(pe.rva_to_offset(0x1800), None, "gap after .text");
    assert_eq!(pe.rva_to_offset(IMAGE_SIZE), None);
    assert!(pe.slice_at_rva(RDATA_RVA + 0xFF0, 0x10).is_ok());
    assert_eq!(
        pe.slice_at_rva(RDATA_RVA + 0xFF0, 0x11),
        Err(PeError::RvaNotInFile(RDATA_RVA + 0xFF0)),
        "crosses into .data"
    );
    assert!(pe.slice_at_rva(RDATA_RVA, usize::MAX).is_err());
}

#[test]
fn mapped_reads_see_zero_fill() {
    let mut b = Builder::new();
    let tail = RDATA_FILE + RDATA_LEN + 0x1FC; // last 4 file bytes of .data
    b.bytes[tail..tail + 4].copy_from_slice(&[1, 2, 3, 4]);
    let pe = b.parse();
    // Half from the file, half from the zero-filled part of the section.
    assert_eq!(pe.read_mapped::<8>(DATA_RVA + 0x1FC).unwrap(), [1, 2, 3, 4, 0, 0, 0, 0]);
    assert_eq!(pe.read_mapped::<4>(DATA_RVA + 0x800).unwrap(), [0; 4]);
    assert!(pe.read_mapped::<4>(TEXT_RVA + 0x800).is_err(), "unmapped gap");
    assert!(pe.read_mapped::<8>(TEXT_RVA + 0xC).is_err(), "crosses the end of .text");
    assert!(pe.read_mapped::<4>(u32::MAX - 1).is_err());
    assert_eq!(pe.read_mapped::<2>(0).unwrap(), *b"MZ");
}

#[test]
fn thunk_table_may_end_in_zero_filled_memory() {
    let mut b = with_import();
    // ILT entry in the last file-backed qword of .data; its terminator is in
    // the zero-filled tail, exactly as the mapped image would have it.
    b.rd_u32(0x100, DATA_RVA + 0x1F8);
    b.u64(RDATA_FILE + RDATA_LEN + 0x1F8, (RDATA_RVA + 0x400) as u64);
    let pe = b.parse();
    let imp = pe.imports().unwrap().next().unwrap().unwrap();
    let entries: Vec<_> = imp.entries().collect::<Result<_, _>>().unwrap();
    assert_eq!(entries.len(), 1);
}

// ---- imports ------------------------------------------------------------------

/// One descriptor at .rdata+0x100 for "test.dll" with ILT at +0x200, IAT at
/// +0x300 and a hint/name entry at +0x400.
fn with_import() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::IMPORT, RDATA_RVA + 0x100, 40);
    b.rd_u32(0x100, RDATA_RVA + 0x200); // ILT
    b.rd_u32(0x10C, RDATA_RVA + 0x180); // name
    b.rd_u32(0x110, RDATA_RVA + 0x300); // IAT
    b.rd_str(0x180, "test.dll");
    b.rd_u64(0x200, (RDATA_RVA + 0x400) as u64);
    b.rd_u64(0x208, (1 << 63) | 17);
    b.rd_u16(0x400, 5);
    b.rd_str(0x402, "DoThing");
    b
}

fn import_error(b: &Builder) -> PeError {
    let pe = b.parse();
    for imp in pe.imports().unwrap() {
        match imp {
            Ok(i) => {
                for e in i.entries() {
                    if let Err(e) = e {
                        return e;
                    }
                }
            }
            Err(e) => return e,
        }
    }
    panic!("expected an import error")
}

#[test]
fn imports_by_name_and_ordinal() {
    let b = with_import();
    let pe = b.parse();
    let imps: Vec<_> = pe.imports().unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(imps.len(), 1);
    assert_eq!(imps[0].dll, "test.dll");
    let e: Vec<_> = imps[0].entries().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        e,
        [
            ImportEntry::ByName { hint: 5, name: "DoThing", iat_rva: RDATA_RVA + 0x300 },
            ImportEntry::ByOrdinal { ordinal: 17, iat_rva: RDATA_RVA + 0x308 },
        ]
    );
}

#[test]
fn imports_fall_back_to_iat_without_ilt() {
    let mut b = with_import();
    b.rd_u32(0x100, 0);
    b.rd_u64(0x300, (RDATA_RVA + 0x400) as u64);
    let pe = b.parse();
    let imp = pe.imports().unwrap().next().unwrap().unwrap();
    assert_eq!(imp.entries().count(), 1);
}

#[test]
fn rejects_invalid_imports() {
    let cases: [Case; 10] = [
        ("name outside image", |b| b.rd_u32(0x10C, IMAGE_SIZE + 0x100)),
        ("name not terminated", |b| {
            b.rd_u32(0x10C, RDATA_RVA + 0xF00);
            let at = Builder::rd(0xF00);
            b.bytes[at..at + 0x100].fill(b'A');
        }),
        ("empty DLL name", |b| b.rd_str(0x180, "")),
        ("missing IAT", |b| b.rd_u32(0x110, 0)),
        ("ILT outside image", |b| b.rd_u32(0x100, IMAGE_SIZE + 8)),
        // Zero-filled memory: the hint reads as 0 but the name is not in the file.
        ("hint/name in zero-filled memory", |b| b.rd_u64(0x200, (DATA_RVA + 0x800) as u64)),
        ("hint/name in an unmapped gap", |b| b.rd_u64(0x200, (TEXT_RVA + 0x800) as u64)),
        ("ordinal thunk reserved bits", |b| b.rd_u64(0x208, (1 << 63) | (1 << 40) | 17)),
        ("hint/name RVA too large", |b| b.rd_u64(0x200, 1 << 40)),
        // .text maps only 0x10 bytes: the table runs into the unmapped gap
        // after it without a terminator.
        ("thunk table runs into a gap", |b| {
            b.rd_u32(0x100, TEXT_RVA + 8);
            b.u64(0x408, (RDATA_RVA + 0x400) as u64);
        }),
    ];
    for (what, corrupt) in cases {
        let mut b = with_import();
        corrupt(&mut b);
        assert!(matches!(import_error(&b), PeError::BadImport { .. }), "{what}");
    }
}

#[test]
fn rejects_unterminated_descriptor_array() {
    let mut b = with_import();
    // Fill the rest of .rdata with copies of the descriptor: the array never
    // ends inside the file-backed data.
    let first: Vec<u8> = b.bytes[Builder::rd(0x100)..Builder::rd(0x114)].to_vec();
    let mut off = 0x100;
    while off + 20 <= 0x1000 {
        let at = Builder::rd(off);
        b.bytes[at..at + 20].copy_from_slice(&first);
        off += 20;
    }
    b.rd_str(0x180, "test.dll"); // keep the name intact (overwritten above)
    let pe = b.parse();
    let results: Vec<_> = pe.imports().unwrap().collect();
    assert!(matches!(results.last(), Some(Err(PeError::BadImport { .. }))));
}

// ---- exports ------------------------------------------------------------------

/// Export directory at .rdata+0x100: base 1, 3 functions (one forwarder,
/// one unused slot), names "alpha" -> #1 and "beta" -> #3.
fn with_exports() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::EXPORT, RDATA_RVA + 0x100, 0x200);
    b.rd_u32(0x10C, RDATA_RVA + 0x1C0); // DLL name
    b.rd_u32(0x110, 1); // base
    b.rd_u32(0x114, 3); // functions
    b.rd_u32(0x118, 2); // names
    b.rd_u32(0x11C, RDATA_RVA + 0x140); // address table
    b.rd_u32(0x120, RDATA_RVA + 0x160); // name pointers
    b.rd_u32(0x124, RDATA_RVA + 0x170); // ordinals
    b.rd_u32(0x140, TEXT_RVA); // #1 -> code
    b.rd_u32(0x144, 0); // #2 unused
    b.rd_u32(0x148, RDATA_RVA + 0x1E0); // #3 -> forwarder (inside the directory)
    b.rd_u32(0x160, RDATA_RVA + 0x1D0);
    b.rd_u32(0x164, RDATA_RVA + 0x1D8);
    b.rd_u16(0x170, 0);
    b.rd_u16(0x172, 2);
    b.rd_str(0x1C0, "lib.dll");
    b.rd_str(0x1D0, "alpha");
    b.rd_str(0x1D8, "beta");
    b.rd_str(0x1E0, "other.Func");
    b
}

#[test]
fn exports_with_forwarder_and_unused_slot() {
    let b = with_exports();
    let pe = b.parse();
    let ex = pe.exports().unwrap().unwrap();
    assert_eq!(ex.dll_name, "lib.dll");
    ex.validate_names().unwrap();
    let all: Vec<_> = ex.iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(all.len(), 2, "unused slot skipped");
    assert_eq!((all[0].ordinal, all[0].name, all[0].target), (1, Some("alpha"), ExportTarget::Rva(TEXT_RVA)));
    assert_eq!(all[1].ordinal, 3);
    assert!(matches!(all[1].target, ExportTarget::Forwarder { dll: "other", name: Some("Func"), .. }));
    assert_eq!(ex.by_name("beta").unwrap().unwrap().ordinal, 3);
    assert_eq!(ex.by_ordinal(2).unwrap(), None);
}

fn export_error(b: &Builder) -> PeError {
    let pe = PeFile::parse(&b.bytes).unwrap();
    let ex = match pe.exports() {
        Err(e) => return e,
        Ok(ex) => ex.unwrap(),
    };
    if let Err(e) = ex.validate_names() {
        return e;
    }
    for e in ex.iter() {
        if let Err(e) = e {
            return e;
        }
    }
    panic!("expected an export error")
}

#[test]
fn rejects_invalid_exports() {
    let cases: [Case; 9] = [
        ("more names than functions", |b| b.rd_u32(0x118, 4)),
        ("too many functions", |b| b.rd_u32(0x114, 70_000)),
        ("ordinals exceed 16 bits", |b| b.rd_u32(0x110, 0xFFFF)),
        ("address table outside file", |b| b.rd_u32(0x11C, DATA_RVA + 0x1FC)),
        ("name pointers outside image", |b| b.rd_u32(0x120, IMAGE_SIZE)),
        ("ordinal index out of range", |b| b.rd_u16(0x172, 3)),
        ("names unsorted", |b| {
            b.rd_u32(0x160, RDATA_RVA + 0x1D8);
            b.rd_u32(0x164, RDATA_RVA + 0x1D0);
        }),
        ("forwarder without dot", |b| b.rd_str(0x1E0, "noDotHere")),
        ("export RVA outside image", |b| b.rd_u32(0x140, IMAGE_SIZE + 4)),
    ];
    for (what, corrupt) in cases {
        let mut b = with_exports();
        corrupt(&mut b);
        assert!(matches!(export_error(&b), PeError::BadExport { .. }), "{what}: {:?}", export_error(&b));
    }
}

// ---- base relocations ---------------------------------------------------------

/// Block for page 0x3000: DIR64 at +0x10, DIR64 at +0x20, padding.
fn with_relocs() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::BASERELOC, RDATA_RVA + 0x800, 14);
    b.rd_u32(0x800, DATA_RVA);
    b.rd_u32(0x804, 14);
    b.rd_u16(0x808, 0xA010);
    b.rd_u16(0x80A, 0xA020);
    b.rd_u16(0x80C, 0x0000);
    b
}

#[test]
fn relocations_parse() {
    let b = with_relocs();
    let pe = b.parse();
    let r: Vec<_> = pe.relocations().unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!((r[0].rva, r[0].kind), (DATA_RVA + 0x10, RelocationKind::Dir64));
    assert_eq!((r[1].rva, r[1].kind), (DATA_RVA + 0x20, RelocationKind::Dir64));
    assert_eq!(r[2].kind, RelocationKind::Absolute);
}

#[test]
fn rejects_invalid_relocations() {
    let cases: [Case; 6] = [
        ("block smaller than its header", |b| b.rd_u32(0x804, 4)),
        ("odd block size", |b| b.rd_u32(0x804, 13)),
        ("block past the directory", |b| b.rd_u32(0x804, 0x100)),
        ("page not aligned", |b| b.rd_u32(0x800, DATA_RVA + 8)),
        ("page outside image", |b| b.rd_u32(0x800, IMAGE_SIZE)),
        ("target past image end", |b| b.rd_u16(0x808, 0xAFFC)),
    ];
    for (what, corrupt) in cases {
        let mut b = with_relocs();
        corrupt(&mut b);
        let pe = b.parse();
        let last = pe.relocations().unwrap().last();
        assert!(matches!(last, Some(Err(PeError::BadRelocation { .. }))), "{what}: {last:?}");
    }
    let mut b = with_relocs();
    b.dir(dir::BASERELOC, RDATA_RVA + 0x800, 6); // shorter than one header
    assert!(matches!(b.parse().relocations().unwrap().last(), Some(Err(PeError::BadRelocation { .. }))));
    let mut b = with_relocs();
    b.rd_u16(0x80A, 0x1020); // HIGH: reported, policy is the loader's
    let kinds: Vec<_> = b.parse().relocations().unwrap().map(|r| r.unwrap().kind).collect();
    assert_eq!(kinds[1], RelocationKind::Other(1));
}

// ---- TLS ----------------------------------------------------------------------

/// TLS directory at .rdata+0x900, callbacks array at +0x940 (two entries).
fn with_tls() -> Builder {
    let mut b = Builder::new();
    let va = |rva: u32| IMAGE_BASE + rva as u64;
    b.dir(dir::TLS, RDATA_RVA + 0x900, 40);
    b.rd_u64(0x900, va(DATA_RVA));
    b.rd_u64(0x908, va(DATA_RVA + 0x20));
    b.rd_u64(0x910, va(DATA_RVA + 0x100));
    b.rd_u64(0x918, va(RDATA_RVA + 0x940));
    b.rd_u64(0x940, va(TEXT_RVA));
    b.rd_u64(0x948, va(TEXT_RVA + 4));
    b
}

#[test]
fn tls_directory_and_callbacks() {
    let b = with_tls();
    let pe = b.parse();
    let t = pe.tls().unwrap().unwrap();
    assert_eq!((t.raw_data_start_rva, t.raw_data_end_rva, t.index_rva), (DATA_RVA, DATA_RVA + 0x20, DATA_RVA + 0x100));
    let cbs: Vec<u32> = t.callbacks().collect::<Result<_, _>>().unwrap();
    assert_eq!(cbs, [TEXT_RVA, TEXT_RVA + 4]);
    assert_eq!(t.template().unwrap().len(), 0x20);
}

#[test]
fn rejects_invalid_tls() {
    let cases: [Case; 6] = [
        ("start below image base", |b| b.rd_u64(0x900, IMAGE_BASE - 1)),
        ("end before start", |b| b.rd_u64(0x908, IMAGE_BASE + DATA_RVA as u64 - 0x10)),
        ("index outside image", |b| b.rd_u64(0x910, IMAGE_BASE + IMAGE_SIZE as u64)),
        ("callback outside image", |b| b.rd_u64(0x948, 0xFFFF_FFFF_FFFF_0000)),
        ("callback array runs into a gap", |b| {
            b.rd_u64(0x918, IMAGE_BASE + (TEXT_RVA + 8) as u64);
            b.u64(0x408, IMAGE_BASE + TEXT_RVA as u64);
        }),
        ("directory too small", |b| b.dir(dir::TLS, RDATA_RVA + 0x900, 24)),
    ];
    for (what, corrupt) in cases {
        let mut b = with_tls();
        corrupt(&mut b);
        let pe = b.parse();
        let failed = match pe.tls() {
            Err(PeError::BadTls { .. }) => true,
            Ok(Some(t)) => t.callbacks().any(|c| matches!(c, Err(PeError::BadTls { .. }))),
            _ => false,
        };
        assert!(failed, "{what}");
    }
}

// ---- exception data -----------------------------------------------------------

/// Two RUNTIME_FUNCTIONs at .rdata+0xA00 with unwind info at +0xA80.
fn with_pdata() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::EXCEPTION, RDATA_RVA + 0xA00, 24);
    b.rd_u32(0xA00, TEXT_RVA);
    b.rd_u32(0xA04, TEXT_RVA + 4);
    b.rd_u32(0xA08, RDATA_RVA + 0xA80);
    b.rd_u32(0xA0C, TEXT_RVA + 8);
    b.rd_u32(0xA10, TEXT_RVA + 0x10);
    b.rd_u32(0xA14, RDATA_RVA + 0xA80);
    b.bytes[Builder::rd(0xA80)] = 0x01; // version 1, no flags
    b.bytes[Builder::rd(0xA81)] = 4; // prolog size
    b.bytes[Builder::rd(0xA82)] = 1; // one code
    b
}

#[test]
fn runtime_functions_and_lookup() {
    let b = with_pdata();
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t.lookup(TEXT_RVA + 2).unwrap().unwrap().begin, TEXT_RVA);
    assert_eq!(t.lookup(TEXT_RVA + 6).unwrap(), None, "gap between functions");
    assert_eq!(t.lookup(TEXT_RVA + 0xF).unwrap().unwrap().begin, TEXT_RVA + 8);
    let u = t.unwind_info(&t.get(0).unwrap()).unwrap();
    assert_eq!((u.version, u.size_of_prolog, u.count_of_codes), (1, 4, 1));
}

#[test]
fn rejects_invalid_exception_data() {
    let cases: [Case; 5] = [
        ("size not a multiple of 12", |b| b.dir(dir::EXCEPTION, RDATA_RVA + 0xA00, 20)),
        ("end before begin", |b| b.rd_u32(0xA04, TEXT_RVA - 1)),
        ("unsorted", |b| {
            b.rd_u32(0xA0C, 0x0F00);
            b.rd_u32(0xA10, 0x0F10);
        }),
        ("overlapping", |b| b.rd_u32(0xA0C, TEXT_RVA + 2)),
        ("end outside image", |b| b.rd_u32(0xA10, IMAGE_SIZE + 1)),
    ];
    for (what, corrupt) in cases {
        let mut b = with_pdata();
        corrupt(&mut b);
        assert!(matches!(b.parse().runtime_functions(), Err(PeError::BadExceptionData { .. })), "{what}");
    }
    let mut b = with_pdata();
    b.bytes[Builder::rd(0xA80)] = 0x03; // version 3 does not exist
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    assert!(t.unwind_info(&t.get(0).unwrap()).is_err());
    let mut b = with_pdata();
    b.rd_u32(0xA08, DATA_RVA + 0x400); // unwind info in the zero-filled tail
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    assert!(t.unwind_info(&t.get(0).unwrap()).is_err());
}

// ---- regressions found by the System32 sweep ----------------------------------

#[test]
fn long_decorated_export_names_are_accepted() {
    // Windows DLLs export MSVC-decorated C++ names of 500-700 bytes.
    let mut b = with_exports();
    let long = "?".to_string() + &"x".repeat(999);
    b.rd_u32(0x164, RDATA_RVA + 0x400);
    b.rd_str(0x400, &long);
    let pe = b.parse();
    let ex = pe.exports().unwrap().unwrap();
    let names: Vec<_> = ex.names().collect::<Result<_, _>>().unwrap();
    assert_eq!(names[1].0.len(), 1000);
}

#[test]
fn zero_length_runtime_function_is_accepted() {
    // symcryptk.dll ships an entry with begin == end.
    let mut b = with_pdata();
    b.rd_u32(0xA04, TEXT_RVA);
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    assert_eq!(t.lookup(TEXT_RVA).unwrap().map(|f| f.begin), None, "empty range contains nothing");
}

#[test]
fn unwind_info_without_codes_at_end_of_section_data() {
    // Kernel-mode Windows modules place code-less UNWIND_INFO records right
    // at the end of a section's data; the (empty) code array then starts at
    // the boundary.
    let mut b = with_pdata();
    b.rd_u32(0xA08, TEXT_RVA + 0xC);
    b.bytes[0x40C..0x410].copy_from_slice(&[0x01, 0, 0, 0]); // version 1, no codes
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    let u = t.unwind_info(&t.get(0).unwrap()).unwrap();
    assert_eq!((u.version, u.count_of_codes), (1, 0));
    // With one code the array would extend past the section: still an error.
    let mut b = with_pdata();
    b.rd_u32(0xA08, TEXT_RVA + 0xC);
    b.bytes[0x40C..0x410].copy_from_slice(&[0x01, 0, 1, 0]);
    let pe = b.parse();
    let t = pe.runtime_functions().unwrap();
    assert!(t.unwind_info(&t.get(0).unwrap()).is_err());
}
