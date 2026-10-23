//! Assembles a GPT disk image from partition images.
//!
//! cargo run -p osyx-part --example mkdisk -- <out.img> <disk MiB> <kind>:<image>[:<MiB>]...
//!
//! `kind` is `esp`, `root` or `data`. Partitions start on 1 MiB boundaries;
//! a partition is as large as its image unless a size is given. Only the
//! output file is written.

use osyx_part::{build, Guid, NewPartition, TYPE_ESP, TYPE_LINUX_DATA, TYPE_ROOT_X86_64};
use std::io::{Seek, SeekFrom, Write};

const MIB: u64 = 1 << 20;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: mkdisk <out.img> <disk MiB> <esp|root|data>:<image>[:<MiB>]...");
        std::process::exit(2);
    }
    let disk_bytes = args[1].parse::<u64>().expect("disk size in MiB") * MIB;
    let mut parts = Vec::new();
    let mut next = MIB;
    for (i, spec) in args[2..].iter().enumerate() {
        let mut it = spec.splitn(3, ':');
        let kind = it.next().unwrap();
        // Windows paths contain a drive colon: rejoin "C" + ":\..." pieces.
        let mut rest: Vec<&str> = spec[kind.len() + 1..].split(':').collect();
        let size_mib = match rest.last() {
            Some(s) if rest.len() > 1 && s.parse::<u64>().is_ok() => Some(rest.pop().unwrap().parse::<u64>().unwrap()),
            _ => None,
        };
        let image = rest.join(":");
        let len = std::fs::metadata(&image).unwrap_or_else(|e| panic!("{image}: {e}")).len();
        let size = size_mib.map_or(len, |m| m * MIB).max(len).div_ceil(MIB) * MIB;
        let (type_guid, name) = match kind {
            "esp" => (TYPE_ESP, "EFI system partition"),
            "root" => (TYPE_ROOT_X86_64, "OSyx root"),
            "data" => (TYPE_LINUX_DATA, "OSyx data"),
            _ => panic!("unknown partition kind {kind}"),
        };
        parts.push((type_guid, name, next, size, image, len, i));
        next += size;
    }
    if next + MIB > disk_bytes {
        eprintln!("partitions need {} MiB, disk has {}", (next + MIB) / MIB, disk_bytes / MIB);
        std::process::exit(1);
    }
    let unique = |i: usize| {
        let mut g = [0u8; 16];
        g[..4].copy_from_slice(b"OSyx");
        g[4] = i as u8 + 1;
        g[6] = 0x40; // version 4 layout bits
        g[8] = 0x80;
        Guid(g)
    };
    let new: Vec<NewPartition<'_>> = parts
        .iter()
        .map(|(t, name, start, size, _, _, i)| NewPartition {
            type_guid: *t,
            unique_guid: unique(*i),
            first_lba: start / 512,
            sectors: size / 512,
            name,
        })
        .collect();
    let gpt = build(disk_bytes / 512, unique(99), &new).expect("valid layout");
    let mut out = std::fs::File::create(&args[0]).expect("create output");
    out.set_len(disk_bytes).expect("size output");
    for (lba, data) in gpt {
        out.seek(SeekFrom::Start(lba * 512)).unwrap();
        out.write_all(&data).unwrap();
    }
    for (_, name, start, size, image, len, _) in &parts {
        let data = std::fs::read(image).unwrap();
        out.seek(SeekFrom::Start(*start)).unwrap();
        out.write_all(&data).unwrap();
        println!("  {name:<22} {} MiB at {} MiB ({len} bytes from {image})", size / MIB, start / MIB);
    }
    println!("{}: GPT, {} MiB, {} partitions", args[0], disk_bytes / MIB, parts.len());
}
