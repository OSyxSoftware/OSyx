//! Benchmarks for the PE32+ parser over the real fixture images.

use divan::black_box;
use osyx_pe::PeFile;

fn main() {
    divan::main();
}

static HELLO: &[u8] = include_bytes!("../tests/fixtures/hello.exe");
static SAMPLE: &[u8] = include_bytes!("../tests/fixtures/sample.dll");

#[divan::bench(args = ["hello.exe", "sample.dll"])]
fn parse_headers(name: &str) -> usize {
    let data = if name == "hello.exe" { HELLO } else { SAMPLE };
    let pe = PeFile::parse(black_box(data)).unwrap();
    pe.sections().count()
}

#[divan::bench]
fn walk_imports() -> usize {
    let pe = PeFile::parse(black_box(HELLO)).unwrap();
    let mut n = 0;
    for desc in pe.imports().unwrap() {
        let desc = desc.unwrap();
        for e in desc.entries() {
            black_box(e.unwrap());
            n += 1;
        }
    }
    n
}

#[divan::bench]
fn walk_exports() -> usize {
    let pe = PeFile::parse(black_box(SAMPLE)).unwrap();
    let exports = pe.exports().unwrap().unwrap();
    exports.validate_names().unwrap();
    exports
        .iter()
        .inspect(|e| {
            black_box(e.as_ref().unwrap());
        })
        .count()
}

#[divan::bench]
fn lookup_export_by_name() -> bool {
    let pe = PeFile::parse(black_box(SAMPLE)).unwrap();
    let exports = pe.exports().unwrap().unwrap();
    let first = exports.names().next().unwrap().unwrap().0;
    exports.by_name(black_box(first)).unwrap().is_some()
}

#[divan::bench]
fn walk_relocations() -> usize {
    let pe = PeFile::parse(black_box(SAMPLE)).unwrap();
    pe.relocations()
        .unwrap()
        .inspect(|r| {
            black_box(r.as_ref().unwrap());
        })
        .count()
}

#[divan::bench]
fn rva_to_offset_all_sections() -> usize {
    let pe = PeFile::parse(black_box(SAMPLE)).unwrap();
    let mut acc = 0;
    for s in pe.sections() {
        let mut rva = s.virtual_address;
        while rva < s.virtual_address + s.virtual_size {
            acc += pe.rva_to_offset(black_box(rva)).unwrap_or(0);
            rva += 16;
        }
    }
    acc
}
