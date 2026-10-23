//! Cross-validation with GNU mtools, an independent FAT implementation:
//! volumes written by osyx-fat must read back correctly with mtools, and
//! volumes written by mtools must read back correctly (and pass the
//! consistency check) with osyx-fat. Skipped when mtools is not installed.

use osyx_fat::{format, FileSystem, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

const MIB: usize = 1024 * 1024;

fn mtool(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let candidates = [PathBuf::from(r"C:\msys64\ucrt64\bin").join(&exe), PathBuf::from("/usr/bin").join(&exe)];
    candidates.into_iter().find(|p| p.exists())
}

fn run(name: &str, args: &[&str]) -> std::process::Output {
    let out = Command::new(mtool(name).expect("mtools")).args(args).env("MTOOLS_SKIP_CHECK", "1").output().unwrap();
    assert!(out.status.success(), "{name} {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("osyx-fat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

#[test]
fn mtools_reads_what_osyx_writes() {
    if mtool("mdir").is_none() {
        eprintln!("mtools not installed; skipping");
        return;
    }
    let mut img = vec![0u8; 64 * MIB];
    format(&mut img, "OSYXTEST", 0xC0FFEE).unwrap();
    let mut fs = FileSystem::mount(img).unwrap();
    let put = |fs: &mut FileSystem<Vec<u8>>, path: &str, data: &[u8]| {
        let mut f = fs.open(path, OpenOptions { create: true, ..Default::default() }).unwrap();
        fs.write(&mut f, 0, data).unwrap();
    };
    put(&mut fs, "/hello.txt", b"Hello from osyx-fat\n");
    put(&mut fs, "/Long File Name With Spaces.txt", b"long name\n");
    fs.create_dir("/dir").unwrap();
    fs.create_dir("/dir/Sub Directory").unwrap();
    let blob = pattern(150_000, 11);
    put(&mut fs, "/dir/Sub Directory/blob.bin", &blob);
    fs.rename("/hello.txt", "/dir/moved.txt").unwrap();
    let img = fs.unmount().unwrap();
    let path = temp("osyx-written.img");
    std::fs::write(&path, &img).unwrap();
    let p = path.to_str().unwrap();

    let info = run("minfo", &["-i", p, "::"]);
    let info = String::from_utf8_lossy(&info.stdout);
    assert!(info.contains("OSYXTEST"), "label: {info}");

    let listing = String::from_utf8_lossy(&run("mdir", &["-i", p, "-/", "-b", "::"]).stdout).replace('\\', "/");
    for expected in ["::/Long File Name With Spaces.txt", "::/dir/moved.txt", "::/dir/Sub Directory/blob.bin"] {
        assert!(listing.lines().any(|l| l.trim() == expected), "{expected} missing from:\n{listing}");
    }
    assert!(!listing.contains("hello.txt"), "renamed file still listed");

    let text = run("mtype", &["-i", p, "::/dir/moved.txt"]).stdout;
    assert_eq!(text, b"Hello from osyx-fat\n");
    // mtools reads "C:" in a destination as one of its drive letters, so
    // extract with a relative path inside the temporary directory.
    let out = temp("blob-out.bin");
    let _ = std::fs::remove_file(&out);
    let status = Command::new(mtool("mcopy").unwrap())
        .current_dir(out.parent().unwrap())
        .args(["-i", p, "::/dir/Sub Directory/blob.bin", "blob-out.bin"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(&out).unwrap(), blob);
}

#[test]
fn osyx_reads_what_mtools_writes() {
    if mtool("mformat").is_none() {
        eprintln!("mtools not installed; skipping");
        return;
    }
    let path = temp("mtools-written.img");
    std::fs::write(&path, vec![0u8; 64 * MIB]).unwrap();
    let p = path.to_str().unwrap();
    run("mformat", &["-i", p, "-F", "-v", "MTOOLSVOL", "::"]);
    run("mmd", &["-i", p, "::/folder", "::/folder/Nested Folder"]);
    let small = temp("small.txt");
    std::fs::write(&small, b"written by mtools\n").unwrap();
    let big = temp("big.bin");
    let big_data = pattern(MIB + 777, 5);
    std::fs::write(&big, &big_data).unwrap();
    run("mcopy", &["-i", p, small.to_str().unwrap(), "::/folder/Mixed Case Name.txt"]);
    run("mcopy", &["-i", p, big.to_str().unwrap(), "::/folder/Nested Folder/big.bin"]);
    run("mcopy", &["-i", p, small.to_str().unwrap(), "::/UPPER.TXT"]);
    run("mcopy", &["-i", p, small.to_str().unwrap(), "::/lower.txt"]);

    let img = std::fs::read(&path).unwrap();
    let mut fs = FileSystem::mount(img).unwrap();
    assert_eq!(fs.label(), "MTOOLSVOL");
    let read = |fs: &mut FileSystem<Vec<u8>>, path: &str| {
        let mut f = fs.open(path, OpenOptions::default()).unwrap();
        let mut buf = vec![0u8; f.size() as usize];
        fs.read(&mut f, 0, &mut buf).unwrap();
        buf
    };
    assert_eq!(read(&mut fs, "/folder/Mixed Case Name.txt"), b"written by mtools\n");
    assert_eq!(read(&mut fs, "/folder/nested folder/BIG.BIN"), big_data);
    let mut root: Vec<String> = fs.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
    root.sort();
    assert_eq!(root, ["UPPER.TXT", "folder", "lower.txt"], "short names keep their case");
    let report = fs.check().unwrap();
    assert_eq!((report.files, report.directories), (4, 3));
    // Modify the mtools volume and let mtools read our changes.
    let mut f = fs.open("/folder/added by osyx.txt", OpenOptions { create: true, ..Default::default() }).unwrap();
    fs.write(&mut f, 0, b"osyx was here").unwrap();
    fs.remove_file("/UPPER.TXT").unwrap();
    let img = fs.unmount().unwrap();
    std::fs::write(&path, &img).unwrap();
    let listing = String::from_utf8_lossy(&run("mdir", &["-i", p, "-/", "-b", "::"]).stdout).replace('\\', "/");
    assert!(listing.lines().any(|l| l.trim() == "::/folder/added by osyx.txt"), "{listing}");
    assert!(!listing.contains("UPPER.TXT"));
    assert_eq!(run("mtype", &["-i", p, "::/folder/added by osyx.txt"]).stdout, b"osyx was here");
    let _ = std::fs::remove_dir_all(Path::new(&path).parent().unwrap());
}
