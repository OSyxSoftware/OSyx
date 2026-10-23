mod common;
use common::{Builder, RDATA_RVA};
use osyx_pe::{dir, ImportEntry};
fn image() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::DELAY_IMPORT, RDATA_RVA + 0xb00, 64);
    for (off, value) in [
        (0xb00, 1),
        (0xb04, RDATA_RVA + 0xb60),
        (0xb08, RDATA_RVA + 0xbc0),
        (0xb0c, RDATA_RVA + 0xba0),
        (0xb10, RDATA_RVA + 0xb80),
    ] {
        b.rd_u32(off, value);
    }
    b.rd_str(0xb60, "example.dll");
    b.rd_u64(0xb80, (RDATA_RVA + 0xbe0) as u64);
    b.rd_u64(0xb88, (1 << 63) | 7);
    b.rd_u16(0xbe0, 12);
    b.rd_str(0xbe2, "DelayedFunction");
    b
}
#[test]
fn reads_delayed_names_ordinals_and_handle_slot() {
    let b = image();
    let pe = b.parse();
    let ds = pe.delay_imports().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].import.dll, "example.dll");
    assert_eq!(ds[0].module_handle_rva, RDATA_RVA + 0xbc0);
    assert_eq!(
        ds[0].import.entries().collect::<Result<Vec<_>, _>>().unwrap(),
        vec![
            ImportEntry::ByName { hint: 12, name: "DelayedFunction", iat_rva: RDATA_RVA + 0xba0 },
            ImportEntry::ByOrdinal { ordinal: 7, iat_rva: RDATA_RVA + 0xba8 },
        ]
    );
}
#[test]
fn rejects_legacy_attributes_out_of_range_and_unterminated_tables() {
    for (field, value) in [(0xb00, 0), (0xb00, 3), (0xb04, u32::MAX), (0xb0c, u32::MAX), (0xb10, 0)] {
        let mut b = image();
        b.rd_u32(field, value);
        let pe = b.parse();
        let mut ds = pe.delay_imports().unwrap();
        assert!(ds.next().unwrap().is_err());
        assert!(ds.next().is_none());
    }
    let mut b = image();
    b.dir(dir::DELAY_IMPORT, RDATA_RVA + 0xb00, 32);
    let pe = b.parse();
    let mut ds = pe.delay_imports().unwrap();
    assert!(ds.next().unwrap().is_ok());
    assert!(ds.next().unwrap().is_err());
    assert!(ds.next().is_none());
}
#[test]
fn all_delay_descriptor_byte_mutations_are_bounded() {
    for i in 0xb00..0xbf0 {
        for v in [0, 0x7f, 0xff] {
            let mut b = image();
            b.bytes[Builder::rd(i)] = v;
            common::walk(&b.parse());
        }
    }
}
