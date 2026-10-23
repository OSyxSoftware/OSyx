//! Truncation and mutation of real and synthetic images: the parser and all
//! table walkers must return errors, never panic (tests run with overflow
//! checks, so arithmetic overflow would panic too).

mod common;

use osyx_pe::PeFile;

static HELLO: &[u8] = include_bytes!("fixtures/hello.exe");
static SAMPLE: &[u8] = include_bytes!("fixtures/sample.dll");

fn exercise(data: &[u8]) {
    if let Ok(pe) = PeFile::parse(data) {
        common::walk(&pe);
    }
}

fn synthetic() -> Vec<u8> {
    // The builder image with every directory populated (see malformed.rs).
    let mut b = common::Builder::new();
    b.dir(osyx_pe::dir::BASERELOC, common::RDATA_RVA + 0x800, 14);
    b.rd_u32(0x800, common::DATA_RVA);
    b.rd_u32(0x804, 14);
    b.rd_u16(0x808, 0xA010);
    b.dir(osyx_pe::dir::EXCEPTION, common::RDATA_RVA + 0xA00, 12);
    b.rd_u32(0xA00, common::TEXT_RVA);
    b.rd_u32(0xA04, common::TEXT_RVA + 4);
    b.rd_u32(0xA08, common::RDATA_RVA + 0xA80);
    b.bytes[common::Builder::rd(0xA80)] = 1;
    b.bytes
}

#[test]
fn every_truncation_is_handled() {
    for data in [HELLO, SAMPLE] {
        for len in 0..data.len() {
            exercise(&data[..len]);
        }
    }
}

#[test]
fn every_header_byte_mutation_is_handled() {
    for data in [HELLO.to_vec(), SAMPLE.to_vec(), synthetic()] {
        for i in 0..0x400.min(data.len()) {
            for v in [0x00, 0xFF, 0x7F, 0x80, data[i].wrapping_add(1), data[i] ^ 0x10] {
                let mut m = data.clone();
                m[i] = v;
                exercise(&m);
            }
        }
    }
}

#[test]
fn random_mutations_are_handled() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for data in [HELLO.to_vec(), SAMPLE.to_vec(), synthetic()] {
        for _ in 0..3000 {
            let mut m = data.clone();
            for _ in 0..1 + next() % 8 {
                let i = (next() % m.len() as u64) as usize;
                m[i] = next() as u8;
            }
            exercise(&m);
        }
    }
}

#[test]
fn table_fields_set_to_extremes_are_handled() {
    // Overwrite each aligned 4-byte field of the table-carrying sections with
    // boundary values, which reaches deeper than random byte flips.
    for data in [SAMPLE.to_vec(), HELLO.to_vec()] {
        let pe = PeFile::parse(&data).unwrap();
        let ranges: Vec<(usize, usize)> = pe
            .sections()
            .filter(|s| s.name() != ".text")
            .map(|s| (s.pointer_to_raw_data as usize, s.file_size() as usize))
            .collect();
        for (start, len) in ranges {
            for at in (start..start + len).step_by(4) {
                for v in [0u32, 1, 0x7FFF_FFFF, 0x8000_0000, u32::MAX, 0x1000, 0x2000] {
                    let mut m = data.clone();
                    m[at..at + 4].copy_from_slice(&v.to_le_bytes());
                    exercise(&m);
                }
            }
        }
    }
}
