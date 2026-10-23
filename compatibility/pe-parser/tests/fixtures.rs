//! Real PE32+ files compiled locally (tests/fixtures/src, built by
//! scripts/gen-pe-fixtures.ps1 with clang + lld). Every expected value below
//! was taken from GNU objdump 2.x (`objdump -p/-h/-s`), not from this parser.

mod common;

use osyx_pe::{dir, ExportTarget, ImportEntry, PeFile, RelocationKind, DLLCHAR_DYNAMIC_BASE, DLLCHAR_NX_COMPAT};

static HELLO: &[u8] = include_bytes!("fixtures/hello.exe");
static SAMPLE: &[u8] = include_bytes!("fixtures/sample.dll");

#[test]
fn hello_exe_headers_and_sections() {
    let pe = PeFile::parse(HELLO).unwrap();
    assert!(!pe.is_dll());
    assert_eq!(pe.machine(), 0x8664);
    assert_eq!(pe.image_base(), 0x1_4000_0000);
    assert_eq!(pe.entry_point(), 0x1000);
    assert_eq!(pe.size_of_image(), 0x5000);
    let o = pe.optional();
    assert_eq!((o.section_alignment, o.file_alignment, o.size_of_headers), (0x1000, 0x200, 0x400));
    assert_eq!(o.subsystem, 3, "console");
    assert_eq!(o.dll_characteristics, 0x8160);
    assert_ne!(o.dll_characteristics & DLLCHAR_DYNAMIC_BASE, 0);
    assert_ne!(o.dll_characteristics & DLLCHAR_NX_COMPAT, 0);
    let names: Vec<(&str, u32, u32, u32)> =
        pe.sections().map(|s| (s.name(), s.virtual_address, s.virtual_size, s.pointer_to_raw_data)).collect();
    assert_eq!(
        names,
        [
            (".text", 0x1000, 0x70, 0x400),
            (".rdata", 0x2000, 0xf8, 0x600),
            (".buildid", 0x3000, 0x35, 0x800),
            (".pdata", 0x4000, 0xc, 0xa00)
        ]
    );
    let text = pe.section(0).unwrap();
    assert!(text.is_executable() && !text.is_writable());
    assert_eq!(pe.directory(dir::IMPORT).virtual_address, 0x2040);
    assert_eq!(pe.directory(dir::IAT).virtual_address, 0x2088);
    assert!(!pe.directory(dir::BASERELOC).is_present());
}

#[test]
fn hello_exe_imports() {
    let pe = PeFile::parse(HELLO).unwrap();
    let imports: Vec<_> = pe.imports().unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].dll, "KERNEL32.dll");
    assert_eq!(imports[0].address_table_rva, 0x2088);
    let entries: Vec<ImportEntry> = imports[0].entries().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        entries,
        [
            ImportEntry::ByName { hint: 0x181, name: "ExitProcess", iat_rva: 0x2088 },
            ImportEntry::ByName { hint: 0x305, name: "GetStdHandle", iat_rva: 0x2090 },
            ImportEntry::ByName { hint: 0x64f, name: "WriteFile", iat_rva: 0x2098 },
        ]
    );
    assert!(pe.exports().unwrap().is_none());
    assert_eq!(pe.relocations().unwrap().count(), 0);
    assert!(pe.tls().unwrap().is_none());
}

#[test]
fn hello_exe_rva_translation() {
    let pe = PeFile::parse(HELLO).unwrap();
    assert_eq!(pe.rva_to_offset(0x10), Some(0x10), "headers map 1:1");
    assert_eq!(pe.rva_to_offset(0x1000), Some(0x400));
    assert_eq!(pe.rva_to_offset(0x2088), Some(0x688));
    assert_eq!(pe.rva_to_offset(0x5000), None, "past SizeOfImage");
    assert_eq!(pe.rva_to_offset(0x1500), None, "gap between .text (0x70 bytes) and .rdata");
    // The program's message lives in .rdata.
    let text = pe.slice_at_rva(0x2000, 0xf8).unwrap();
    assert!(text.windows(26).any(|w| w == b"Hello from a PE32+ fixture"));
}

#[test]
fn sample_dll_exports() {
    let pe = PeFile::parse(SAMPLE).unwrap();
    assert!(pe.is_dll());
    assert_eq!(pe.image_base(), 0x1_8000_0000);
    assert_eq!(pe.entry_point(), 0x10b0);
    assert_eq!(pe.size_of_image(), 0x8000);
    assert_eq!(pe.section_count(), 7);
    let ex = pe.exports().unwrap().unwrap();
    assert_eq!(ex.dll_name, "sample.dll");
    assert_eq!((ex.ordinal_base, ex.function_count(), ex.name_count()), (7, 6, 5));
    ex.validate_names().unwrap();
    let all: Vec<_> = ex.iter().collect::<Result<_, _>>().unwrap();
    let summary: Vec<(u16, Option<&str>, ExportTarget)> = all.iter().map(|e| (e.ordinal, e.name, e.target)).collect();
    assert_eq!(summary[0], (7, None, ExportTarget::Rva(0x1030)), "ordinal_only exported NONAME @7");
    assert_eq!(summary[1], (8, Some("add_numbers"), ExportTarget::Rva(0x1010)));
    assert_eq!(summary[2], (9, Some("bump_counter"), ExportTarget::Rva(0x1020)));
    assert_eq!(summary[3], (10, Some("checksum"), ExportTarget::Rva(0x1040)));
    assert_eq!(
        summary[4],
        (
            11,
            Some("forwarded_by_ordinal"),
            ExportTarget::Forwarder { dll: "kernel32", name: None, ordinal: Some(42), raw: "kernel32.#42" }
        )
    );
    assert_eq!(
        summary[5],
        (
            12,
            Some("forwarded_tick"),
            ExportTarget::Forwarder {
                dll: "kernel32",
                name: Some("GetTickCount"),
                ordinal: None,
                raw: "kernel32.GetTickCount"
            }
        )
    );
    assert_eq!(ex.by_name("checksum").unwrap().unwrap().ordinal, 10);
    assert_eq!(ex.by_name("missing").unwrap(), None);
    assert_eq!(ex.by_ordinal(7).unwrap().unwrap().target, ExportTarget::Rva(0x1030));
    assert_eq!(ex.by_ordinal(6).unwrap(), None);
    assert_eq!(ex.by_ordinal(13).unwrap(), None);
    let names: Vec<(&str, u16)> = ex.names().collect::<Result<_, _>>().unwrap();
    assert_eq!(names.len(), 5);
    assert_eq!(ex.functions().count(), 6);
}

#[test]
fn sample_dll_relocations_tls_and_unwind() {
    let pe = PeFile::parse(SAMPLE).unwrap();
    let relocs: Vec<_> = pe.relocations().unwrap().collect::<Result<_, _>>().unwrap();
    let dir64: Vec<u32> = relocs.iter().filter(|r| r.kind == RelocationKind::Dir64).map(|r| r.rva).collect();
    assert_eq!(dir64, [0x2000, 0x2008, 0x2010, 0x2018, 0x2028, 0x4000]);
    assert_eq!(relocs.iter().filter(|r| r.kind == RelocationKind::Absolute).count(), 2);

    let tls = pe.tls().unwrap().unwrap();
    assert_eq!((tls.raw_data_start_rva, tls.raw_data_end_rva), (0x6000, 0x6001));
    assert_eq!(tls.index_rva, 0x4008);
    assert_eq!(tls.callbacks_rva, 0x2028);
    assert_eq!(tls.characteristics, 0x0010_0000);
    let callbacks: Vec<u32> = tls.callbacks().collect::<Result<_, _>>().unwrap();
    assert_eq!(callbacks, [0x1000], "tls_callback is at .text+0");
    assert_eq!(tls.template().unwrap(), &[0u8]);

    let funcs = pe.runtime_functions().unwrap();
    assert_eq!(funcs.len(), 1);
    let f = funcs.get(0).unwrap();
    assert_eq!((f.begin, f.end, f.unwind_info), (0x1040, 0x10ab, 0x212c));
    let u = funcs.unwind_info(&f).unwrap();
    assert_eq!((u.version, u.flags, u.size_of_prolog, u.count_of_codes), (1, 0, 4, 1));
    assert_eq!(funcs.lookup(0x1050).unwrap(), Some(f));
    assert_eq!(funcs.lookup(0x10ab).unwrap(), None, "end is exclusive");
    assert_eq!(funcs.lookup(0x1000).unwrap(), None);
}

#[test]
fn walk_accepts_both_fixtures() {
    for data in [HELLO, SAMPLE] {
        let pe = PeFile::parse(data).unwrap();
        assert!(common::walk(&pe) > 0);
    }
}
