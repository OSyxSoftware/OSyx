//! mkfs: new volumes of several sizes pass e2fsck, mount writable, keep
//! files across a remount and pass e2fsck again with data on them.

use osyx_ext4::{format, Ext4, FormatOptions, MountOptions, NewKind, ROOT_INO};
use std::process::Command;

fn clock() -> (i64, u32) {
    (1_761_200_000, 0)
}

fn wsl_e2fsck(name: &str, img: &[u8]) {
    // OSYX_TEST_IMAGES=<dir> keeps the images for checking outside the test.
    if let Ok(dir) = std::env::var("OSYX_TEST_IMAGES") {
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.img")), img).unwrap();
    }
    let ok = Command::new("wsl.exe")
        .args(["-d", "Ubuntu-24.04", "--", "bash", "-lc", "command -v e2fsck"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIPPED e2fsck for {name}: WSL with e2fsprogs not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("osyx-mkfs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}.img"));
    std::fs::write(&p, img).unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    let (drive, rest) = s.split_once(':').unwrap();
    let wsl = format!("/mnt/{}{}", drive.to_lowercase(), rest);
    let o = Command::new("wsl.exe").args(["-d", "Ubuntu-24.04", "--", "e2fsck", "-fn", &wsl]).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let _ = std::fs::remove_file(&p);
    assert!(o.status.success(), "{name}: e2fsck found problems:\n{text}");
}

fn new_volume(mib: usize) -> Vec<u8> {
    let mut img = vec![0xA5u8; mib << 20];
    let opts = FormatOptions { label: "OSyx root", uuid: *b"0123456789abcdef", time: 1_761_200_000 };
    let f = format(&mut img, &opts).unwrap();
    assert!(f.journal_blocks >= 1024);
    img
}

#[test]
fn fresh_volumes_are_clean_and_writable() {
    // One group, two groups, and a short last group that is kept.
    for mib in [64usize, 200, 1100] {
        let img = new_volume(mib);
        wsl_e2fsck(&format!("fresh-{mib}"), &img);
        let mut fs = Ext4::mount(img, MountOptions { read_only: false, clock: Some(clock) }).unwrap();
        assert_eq!(fs.write_refusal(), None, "{mib} MiB volume must be writable");
        assert_eq!(fs.label(), "OSyx root");
        let etc = fs.create(ROOT_INO, "etc", NewKind::Dir, 0o755, 0, 0).unwrap();
        let host = fs.create(etc, "hostname", NewKind::File, 0o644, 0, 0).unwrap();
        fs.write(host, 0, b"osyx-test\n").unwrap();
        let big = fs.create(ROOT_INO, "big.bin", NewKind::File, 0o644, 1000, 1000).unwrap();
        let data: Vec<u8> = (0..3_000_000).map(|i| (i % 251) as u8).collect();
        for (i, c) in data.chunks(65536).enumerate() {
            fs.write(big, (i * 65536) as u64, c).unwrap();
        }
        fs.create(ROOT_INO, "link", NewKind::Symlink("etc/hostname"), 0, 0, 0).unwrap();
        let img = fs.unmount().unwrap();
        wsl_e2fsck(&format!("used-{mib}"), &img);
        let mut fs = Ext4::mount(img, MountOptions { read_only: true, clock: None }).unwrap();
        let mut buf = vec![0u8; data.len()];
        let mut done = 0;
        while done < buf.len() {
            let n = fs.read(big, done as u64, &mut buf[done..]).unwrap();
            assert!(n > 0);
            done += n;
        }
        assert_eq!(buf, data);
    }
}

#[test]
fn tiny_devices_are_refused() {
    let mut img = vec![0u8; 8 << 20];
    let opts = FormatOptions { label: "x", uuid: [1; 16], time: 0 };
    assert!(format(&mut img, &opts).is_err());
}
