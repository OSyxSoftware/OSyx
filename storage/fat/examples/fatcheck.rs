//! Mounts a copy of a FAT32 disk image and runs the driver's consistency
//! check (FAT chains, sizes, lost clusters, FSInfo free count).
//!
//! cargo run -p osyx-fat --example fatcheck -- <image>
//!
//! Exit status 0 if the volume is consistent and FSInfo is accurate.

use osyx_fat::FileSystem;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: fatcheck <image>");
        std::process::exit(2);
    };
    let img = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(2)
    });
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
