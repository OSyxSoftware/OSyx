//! Research tool: decodes the API set schema (namespace version 6) stored in
//! the `.apiset` section of a schema DLL, and summarises export forwarding
//! of system DLLs. Read-only; nothing is executed. Prints facts about the
//! data format for the future OSyx PE loader (docs/research/).
//!
//! cargo run -p osyx-pe --example apiset-dump -- <apisetschema.dll> [dll...]
//!
//! Namespace v6 layout as used here (all little-endian, offsets relative to
//! the start of the namespace):
//!   header: version, size, flags, count, entry offset, hash offset, hash factor
//!   entry (24 bytes): flags, name offset, name length, hashed length,
//!                     value offset, value count
//!   value (20 bytes): flags, name offset, name length, value offset, value length
//!   hash  (8 bytes):  hash, entry index (sorted by hash)
//! Names are UTF-16LE without terminator; the API set name is stored without
//! the ".dll" suffix.

use osyx_pe::{ExportTarget, PeFile};
use std::collections::BTreeMap;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn utf16(b: &[u8], off: u32, len: u32) -> String {
    let s = &b[off as usize..(off + len) as usize];
    String::from_utf16_lossy(&s.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("read schema DLL");
    let pe = PeFile::parse(&data).expect("valid PE32+");
    let sec = pe.sections().find(|s| s.name() == ".apiset").expect(".apiset section");
    let start = sec.pointer_to_raw_data as usize;
    let ns = &data[start..start + (sec.size_of_raw_data as usize).min(data.len() - start)];
    let (version, size, flags, count, entry_off, hash_off, factor) =
        (le32(ns, 0), le32(ns, 4), le32(ns, 8), le32(ns, 12), le32(ns, 16), le32(ns, 20), le32(ns, 24));
    println!("schema namespace: version {version}, size {size} bytes, flags {flags:#x}, {count} API sets, hash factor {factor}");
    let mut hosts: BTreeMap<String, usize> = BTreeMap::new();
    let mut alternates = Vec::new();
    let mut empty = 0;
    let mut hash_ok = 0;
    let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples = Vec::new();
    for i in 0..count {
        let e = (entry_off + i * 24) as usize;
        let (eflags, name_off, name_len, hashed_len, val_off, val_count) =
            (le32(ns, e), le32(ns, e + 4), le32(ns, e + 8), le32(ns, e + 12), le32(ns, e + 16), le32(ns, e + 20));
        let name = utf16(ns, name_off, name_len);
        let prefix = name.split('-').take(2).collect::<Vec<_>>().join("-");
        *prefixes.entry(prefix).or_default() += 1;
        // Recompute the hash of the hashed part (lower-case) and find it.
        let hashed = utf16(ns, name_off, hashed_len).to_lowercase();
        let h = hashed.encode_utf16().fold(0u32, |h, c| h.wrapping_mul(factor).wrapping_add(c as u32));
        let found = (0..count).any(|k| {
            let o = (hash_off + k * 8) as usize;
            le32(ns, o) == h && le32(ns, o + 4) == i
        });
        hash_ok += found as usize;
        let mut values = Vec::new();
        for v in 0..val_count {
            let o = (val_off + v * 20) as usize;
            let importer = utf16(ns, le32(ns, o + 4), le32(ns, o + 8));
            let host = utf16(ns, le32(ns, o + 12), le32(ns, o + 16));
            values.push((importer, host));
        }
        match values.first() {
            None => empty += 1,
            Some((_, host)) if host.is_empty() => empty += 1,
            Some((_, host)) => *hosts.entry(host.to_lowercase()).or_default() += 1,
        }
        if values.len() > 1 {
            alternates.push((name.clone(), values.clone()));
        }
        if samples.len() < 12 && (name.starts_with("api-ms-win-core-file") || name.starts_with("api-ms-win-crt-runtime") || name.starts_with("api-ms-win-core-processthreads")) {
            samples.push((name.clone(), eflags, hashed_len / 2, values.clone()));
        }
    }
    println!("hash table: {hash_ok} of {count} entries found under the recomputed hash (name lower-cased up to the hashed length)");
    println!("entries without a host (no implementation on this edition): {empty}");
    println!("name prefixes: {:?}", prefixes.iter().filter(|(_, n)| **n > 20).collect::<Vec<_>>());
    let mut top: Vec<_> = hosts.iter().collect();
    top.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("most common default hosts ({} distinct):", hosts.len());
    for (h, n) in top.iter().take(12) {
        println!("  {n:>4}  {h}");
    }
    println!("sets with importer-specific hosts: {}", alternates.len());
    for (name, values) in alternates.iter().take(6) {
        println!("  {name}: {values:?}");
    }
    println!("samples (name, flags, hashed chars, [(importer, host)]):");
    for s in &samples {
        println!("  {s:?}");
    }
    for dll in &args[1..] {
        let data = std::fs::read(dll).expect("read DLL");
        let pe = PeFile::parse(&data).expect("valid PE32+");
        let exp = pe.exports().expect("export directory").expect("exports present");
        let mut total = 0;
        let mut by_target: BTreeMap<String, usize> = BTreeMap::new();
        let mut local = 0;
        for e in exp.iter() {
            let e = e.expect("valid export");
            total += 1;
            match e.target {
                ExportTarget::Rva(_) => local += 1,
                ExportTarget::Forwarder { dll, .. } => *by_target.entry(dll.to_lowercase()).or_default() += 1,
            }
        }
        let mut f: Vec<_> = by_target.into_iter().collect();
        f.sort_by_key(|x| std::cmp::Reverse(x.1));
        let forwarded: usize = f.iter().map(|x| x.1).sum();
        println!("{} ({}): {total} exports, {local} implemented here, {forwarded} forwarded", dll, exp.dll_name);
        for (t, n) in f.iter().take(8) {
            println!("  {n:>5} -> {t}");
        }
    }
}
