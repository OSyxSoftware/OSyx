mod common;
use common::{Builder, RDATA_RVA};
use osyx_pe::{dir, version_value};

fn image() -> Builder {
    let mut b = Builder::new();
    b.dir(dir::RESOURCE, RDATA_RVA + 0xb00, 0x180);
    // Type 16 -> resource 1 -> language 0409 -> data entry.
    for (dir, id, target) in [(0xb00, 16, 0x80000020), (0xb20, 1, 0x80000040), (0xb40, 0x409, 0x60)] {
        b.rd_u16(dir + 14, 1);
        b.rd_u32(dir + 16, id);
        b.rd_u32(dir + 20, target);
    }
    b.rd_u32(0xb60, RDATA_RVA + 0xb80);
    b.rd_u32(0xb64, 92);
    b.rd_u16(0xb80, 92);
    b.rd_u16(0xb82, 52);
    for (i, c) in "VS_VERSION_INFO".encode_utf16().enumerate() {
        b.rd_u16(0xb86 + i as u32 * 2, c);
    }
    b.rd_u32(0xba8, 0xfeef04bd);
    b.rd_u32(0xbac, 0x10000);
    b.rd_u32(0xbb0, 0x00010002);
    b.rd_u32(0xbb4, 0x00030004);
    b
}
#[test]
fn looks_up_real_resource_tree_and_fixed_version_block() {
    let b = image();
    let pe = b.parse();
    let data = pe.resource(16, Some(1), Some(0x409)).unwrap().unwrap();
    let value = version_value(data, "\\").unwrap().unwrap();
    assert_eq!(value.length, 52);
    assert!(!value.text);
    assert_eq!(&value.data[8..16], &[2, 0, 1, 0, 4, 0, 3, 0]);
    assert!(pe.resource(16, Some(2), None).unwrap().is_none());
    assert!(version_value(data, "\\NoSuchKey").unwrap().is_none());
}
#[test]
fn rejects_cyclic_truncated_and_out_of_image_resources() {
    for (field, value) in [(0xb14, 0x80000000), (0xb54, 0xffffffff), (0xb60, u32::MAX), (0xb64, u32::MAX)] {
        let mut b = image();
        b.rd_u32(field, value);
        assert!(b.parse().resource(16, None, None).is_err());
    }
    let mut b = image();
    b.rd_u16(0xb0e, 0xffff);
    assert!(b.parse().resource(16, None, None).is_err());
}
#[test]
fn every_resource_and_version_byte_mutation_is_bounded() {
    for i in 0xb00..0xc80 {
        for value in [0, 0x7f, 0xff] {
            let mut b = image();
            b.bytes[Builder::rd(i)] = value;
            common::walk(&b.parse());
        }
    }
    let b = image();
    let pe = b.parse();
    let data = pe.resource(16, None, None).unwrap().unwrap();
    for end in 0..data.len() {
        let _ = version_value(&data[..end], "\\");
    }
}
