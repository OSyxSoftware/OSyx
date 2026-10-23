//! Mounts a copy of a FAT32 disk image and runs the driver's consistency
//! check (FAT chains, sizes, lost clusters, FSInfo free count).
//!
//! cargo run -p osyx-fat --example fatcheck -- <image>[@@<byte offset>[K|M]]
//!
//! The optional offset (mtools syntax) selects a partition inside a disk image.
//!
//! Exit status 0 if the volume is consistent and FSInfo is accurate.

use osyx_fat::FileSystem;

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: fatcheck <image>[@@offset]");
        std::process::exit(2);
    };
    let (path, offset) = match arg.split_once("@@") {
        Some((p, o)) => {
            let (num, mul) = match o.chars().last() {
                Some('K') => (&o[..o.len() - 1], 1024),
                Some('M') => (&o[..o.len() - 1], 1 << 20),
                _ => (o, 1),
            };
            (p.to_string(), num.parse::<usize>().expect("offset") * mul)
        }
        None => (arg.clone(), 0),
    };
    let mut img = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(2)
    });
    if offset > 0 {
        img.drain(..offset.min(img.len()));
    }
    // A FAT32 volume ends where its BPB says; the rest of a disk image is
    // not part of it.
    if img.len() >= 512 {
        let total = u32::from_le_bytes([img[32], img[33], img[34], img[35]]) as usize * 512;
        if total > 0 && total < img.len() {
            img.truncate(total);
        }
    }
    // The image is checked in memory; the file itself is never written.
    let mut fs = FileSystem::mount(img).unwrap_or_else(|e| {
        eprintln!("{path}: mount failed: {e}");
        std::process::exit(1)
    });
    match fs.check() {
        Ok(r) => {
            println!(
                "{path}: label {}, {} files, {} directories, {} clusters used, {} free, FSInfo {}",
                fs.label(),
                r.files,
                r.directories,
                r.used_clusters,
                r.free_clusters,
                if r.fsinfo_accurate { "accurate" } else { "STALE" }
            );
            std::process::exit(if r.fsinfo_accurate { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("{path}: INCONSISTENT: {e}");
            std::process::exit(1)
        }
    }
}
