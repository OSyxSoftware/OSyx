//! Creates a FAT32 disk image and copies a host directory tree into it.
//!
//! cargo run -p osyx-fat --example mkfat32 -- <image> <size MiB> <label> [source dir]

use osyx_fat::{format, FileSystem, OpenOptions};
use std::path::Path;

fn copy_tree(fs: &mut FileSystem<Vec<u8>>, src: &Path, dst: &str) -> std::io::Result<usize> {
    let mut count = 0;
    let mut entries: Vec<_> = std::fs::read_dir(src)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let target = format!("{dst}/{name}");
        if e.file_type()?.is_dir() {
            fs.create_dir(&target).map_err(|err| std::io::Error::other(format!("{target}: {err}")))?;
            count += copy_tree(fs, &e.path(), &target)?;
        } else {
            let data = std::fs::read(e.path())?;
            let mut f = fs
                .open(&target, OpenOptions { create: true, truncate: true, ..Default::default() })
                .map_err(|err| std::io::Error::other(format!("{target}: {err}")))?;
            fs.write(&mut f, 0, &data).map_err(|err| std::io::Error::other(format!("{target}: {err}")))?;
            count += 1;
        }
    }
    Ok(count)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: mkfat32 <image> <size MiB> <label> [source dir]");
        std::process::exit(2);
    }
    let size: usize = args[1].parse().expect("size in MiB");
    let mut img = vec![0u8; size * 1024 * 1024];
    format(&mut img, &args[2], 0x0515_2026).unwrap_or_else(|e| {
        eprintln!("format: {e}");
        std::process::exit(1)
    });
    let mut fs = FileSystem::mount(img).expect("mount fresh volume");
    let files = match args.get(3) {
        Some(src) => copy_tree(&mut fs, Path::new(src), "").unwrap_or_else(|e| {
            eprintln!("copy: {e}");
            std::process::exit(1)
        }),
        None => 0,
    };
    let report = {
        fs.flush().unwrap();
        fs.check().expect("consistency check")
    };
    let img = fs.unmount().unwrap();
    std::fs::write(&args[0], &img).expect("write image");
    println!(
        "{}: FAT32 {} MiB, label {}, {} files copied, {} clusters used, {} free",
        args[0], size, args[2], files, report.used_clusters, report.free_clusters
    );
}
