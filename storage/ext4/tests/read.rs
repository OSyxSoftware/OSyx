//! Read tests against images made by e2fsprogs (tests/gen-fixtures.sh).

use osyx_ext4::{Error, Ext4, MountOptions, ROOT_INO};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(format!("{name}.img"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e} (run tests/gen-fixtures.sh)", p.display()))
}

fn ro(img: Vec<u8>) -> Result<Ext4<Vec<u8>>, Error> {
    Ext4::mount(img, MountOptions { read_only: true, clock: None })
}

fn pattern(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 7 + seed) % 251) as u8).collect()
}

fn read_all(fs: &mut Ext4<Vec<u8>>, ino: u32) -> Vec<u8> {
    let size = fs.stat(ino).unwrap().size as usize;
    let mut buf = vec![0xEE; size + 100];
    let mut done = 0;
    // Odd chunk sizes cross block boundaries.
    while done < size {
        let end = (done + 7777).min(buf.len());
        let n = fs.read(ino, done as u64, &mut buf[done..end]).unwrap();
        assert!(n > 0, "short read at {done}");
        done += n;
    }
    assert_eq!(fs.read(ino, size as u64, &mut [0u8; 8]).unwrap(), 0);
    buf.truncate(size);
    buf
}

const IMAGES: [&str; 6] = ["ext4-4k", "ext4-1k", "ext4-nocsum", "ext4-metabg", "ext3-1k", "ext2-4k"];

#[test]
fn every_image_reads_the_same_tree() {
    for name in IMAGES {
        let mut fs = ro(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(fs.label(), name);
        let mut names: Vec<String> = fs.read_dir(ROOT_INO).unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        for want in ["big.bin", "dangling", "dir", "empty-dir", "empty.txt", "fast-link", "hard-link.txt", "hello.txt", "lost+found", "many", "slow-link", "small.bin", "sparse.bin", "zażółć.txt"] {
            assert!(names.iter().any(|n| n == want), "{name}: {want} missing from {names:?}");
        }
        let hello = fs.lookup(ROOT_INO, "hello.txt").unwrap();
        assert_eq!(read_all(&mut fs, hello), b"Hello from mke2fs!\n", "{name}");
        let st = fs.stat(hello).unwrap();
        assert_eq!((st.uid, st.gid, st.links, st.is_file()), (1000, 1000, 2, true), "{name}");
        assert!(st.mtime_ns > 1_600_000_000_000_000_000, "{name}: mtime {}", st.mtime_ns);
        assert_eq!(fs.lookup(ROOT_INO, "hard-link.txt").unwrap(), hello, "{name}: hard link");
        let small = fs.resolve("/small.bin").unwrap();
        assert_eq!(fs.stat(small).unwrap().perm(), 0o600);
        assert_eq!(read_all(&mut fs, small), pattern(5000, 1), "{name}");
        let big = fs.resolve("/big.bin").unwrap();
        assert_eq!(read_all(&mut fs, big), pattern(3 << 20, 2), "{name}: big.bin");
        let empty = fs.resolve("/empty.txt").unwrap();
        assert_eq!(fs.stat(empty).unwrap().size, 0);
        let ed = fs.resolve("/empty-dir").unwrap();
        assert!(fs.read_dir(ed).unwrap().is_empty());
        let deep = fs.resolve("/dir/a/b/c/d/e/f/g/h/i/j/deep.txt").unwrap();
        assert_eq!(read_all(&mut fs, deep), b"deep\n");
        let unicode = fs.lookup(ROOT_INO, "zażółć.txt").unwrap();
        assert_eq!(String::from_utf8(read_all(&mut fs, unicode)).unwrap(), "zażółć gęślą jaźń\n");
        // 400 entries: hashed (dir_index) directories read linearly.
        let many = fs.resolve("/many").unwrap();
        let list = fs.read_dir(many).unwrap();
        assert_eq!(list.len(), 400, "{name}");
        for i in [0, 199, 399] {
            let f = fs.lookup(many, &format!("file-{i}.txt")).unwrap();
            assert_eq!(read_all(&mut fs, f), format!("file {i}\n").into_bytes());
        }
        // Links.
        let fast = fs.lookup(ROOT_INO, "fast-link").unwrap();
        assert!(fs.stat(fast).unwrap().is_symlink());
        assert_eq!(fs.readlink(fast).unwrap(), "hello.txt");
        let slow = fs.lookup(ROOT_INO, "slow-link").unwrap();
        let target = fs.readlink(slow).unwrap();
        assert!(target.len() > 60 && target.starts_with("dir/a/b/c/") && target.ends_with(&"x".repeat(80)), "{name}: {target}");
        let dangling = fs.lookup(ROOT_INO, "dangling").unwrap();
        assert_eq!(fs.readlink(dangling).unwrap(), "/does/not/exist");
        assert_eq!(fs.readlink(hello), Err(Error::InvalidInput("not a symbolic link")));
        // Errors.
        assert_eq!(fs.lookup(ROOT_INO, "nope"), Err(Error::NotFound));
        assert_eq!(fs.lookup(ROOT_INO, "HELLO.TXT"), Err(Error::NotFound), "case-sensitive");
        assert_eq!(fs.lookup(hello, "x"), Err(Error::NotADirectory));
        assert_eq!(fs.read(many, 0, &mut [0u8; 4]), Err(Error::IsADirectory));
    }
}

#[test]
fn sparse_and_large_files() {
    for name in ["ext4-4k", "ext4-1k", "ext3-1k"] {
        let mut fs = ro(fixture(name)).unwrap();
        let s = fs.resolve("/sparse.bin").unwrap();
        let st = fs.stat(s).unwrap();
        let six_gib = 6u64 << 30;
        assert_eq!(st.size, six_gib, "{name}: large_file size");
        assert!(st.blocks_512 < 200, "{name}: holes allocate nothing ({} sectors)", st.blocks_512);
        let mut head = [0u8; 8];
        fs.read(s, 0, &mut head).unwrap();
        assert_eq!(&head, b"start\0\0\0");
        let mut hole = vec![0xFFu8; 70_000];
        assert_eq!(fs.read(s, 12345, &mut hole).unwrap(), hole.len());
        assert!(hole.iter().all(|&b| b == 0), "{name}: hole reads zeros");
        let mut mid = vec![0u8; 4096];
        fs.read(s, 1 << 30, &mut mid).unwrap();
        assert_eq!(mid, pattern(4096, 3), "{name}: data at 1 GiB");
        let mut tail = vec![0u8; 5000];
        assert_eq!(fs.read(s, six_gib - 4096, &mut tail).unwrap(), 4096, "{name}: read stops at EOF");
        assert_eq!(&tail[..4096], &pattern(4096, 4)[..], "{name}: last block");
    }
}

#[test]
fn unsupported_features_are_refused() {
    for f in ["inline_data", "encrypt", "casefold"] {
        match ro(fixture(&format!("ext4-{f}"))) {
            Err(Error::Unsupported(w)) => assert_eq!(w, f),
            other => panic!("{f}: expected Unsupported, got {:?}", other.map(|_| ())),
        }
    }
    // Block-mapped (ext2/3) volumes are readable but never writable.
    for name in ["ext3-1k", "ext2-4k"] {
        let fs = ro(fixture(name)).unwrap();
        assert!(fs.write_refusal().is_some(), "{name}");
        assert!(matches!(Ext4::mount(fixture(name), MountOptions { read_only: false, clock: None }), Err(Error::Unsupported(_))));
    }
    assert!(matches!(ro(vec![0u8; 1 << 20]), Err(Error::NotExt)));
    assert!(matches!(ro(vec![0u8; 512]), Err(Error::NotExt)));
}

#[test]
fn read_only_mount_replays_the_journal_in_memory() {
    let img = fixture("ext4-journal");
    let before = img.clone();
    let mut fs = ro(img).unwrap();
    let hello = fs.resolve("/hello.txt").unwrap();
    let mut buf = vec![0u8; 19];
    fs.read(hello, 0, &mut buf).unwrap();
    assert_eq!(&buf[..19], b"Replayed by the jou", "journal replayed into the read view");
    let dev = fs.unmount().unwrap();
    assert!(dev == before, "a read-only mount must not write");
}

// ---- corrupted images ----------------------------------------------------------

fn sb_off() -> usize {
    1024
}

#[test]
fn corrupted_superblock_and_descriptors() {
    let mut img = fixture("ext4-4k");
    img[sb_off() + 56] = 0;
    assert!(matches!(ro(img), Err(Error::NotExt)));
    let mut img = fixture("ext4-4k");
    img[sb_off() + 0x78] ^= 1; // volume name byte: only the checksum notices
    assert_eq!(ro(img).err(), Some(Error::Corrupt("superblock checksum")));
    let mut img = fixture("ext4-4k");
    img[4096 + 3] ^= 0x40; // first group descriptor (block bitmap location)
    assert_eq!(ro(img).err(), Some(Error::Corrupt("group descriptor checksum")));
    // Without checksums the range check catches a descriptor pointing outside.
    let mut img = fixture("ext4-nocsum");
    // uninit_bg uses crc16: corrupt the checksum-protected field and fix nothing.
    img[4096 + 8..4096 + 12].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    assert!(matches!(ro(img), Err(Error::Corrupt(_))));
    let mut img = fixture("ext2-4k");
    img[4096 + 8..4096 + 12].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    assert_eq!(ro(img).err(), Some(Error::Corrupt("group metadata outside the file system")));
    let mut img = fixture("ext2-4k");
    img[sb_off() + 40..sb_off() + 44].copy_from_slice(&0u32.to_le_bytes()); // inodes per group
    assert!(matches!(ro(img), Err(Error::Corrupt(_))));
    let mut img = fixture("ext2-4k");
    img[sb_off() + 24] = 9; // 512 KiB blocks
    assert!(matches!(ro(img), Err(Error::Corrupt(_))));
}

/// Byte offset of inode `ino` in an image (via the driver's own layout).
fn inode_offset(fs: &mut Ext4<Vec<u8>>, ino: u32) -> usize {
    let sb = fs.superblock().clone();
    let g = (ino - 1) / sb.inodes_per_group;
    let idx = (ino - 1) % sb.inodes_per_group;
    (fs.groups()[g as usize].inode_table * sb.block_size as u64) as usize + idx as usize * sb.inode_size as usize
}

#[test]
fn corrupted_inodes_extents_and_directories() {
    let base = fixture("ext4-4k");
    let mut fs = ro(base.clone()).unwrap();
    let big = fs.resolve("/big.bin").unwrap();
    let many = fs.resolve("/many").unwrap();
    let big_off = inode_offset(&mut fs, big);
    let root_off = inode_offset(&mut fs, ROOT_INO);
    // Root inode checksum.
    let mut img = base.clone();
    img[root_off + 4] ^= 1;
    assert_eq!(ro(img).err(), Some(Error::Corrupt("inode checksum")));
    // Without checksums: an extent pointing past the end of the volume.
    let nocsum = fixture("ext4-nocsum");
    let mut fs2 = ro(nocsum.clone()).unwrap();
    let nbig = fs2.resolve("/big.bin").unwrap();
    let off = inode_offset(&mut fs2, nbig);
    let mut img = nocsum.clone();
    // i_block starts at 40: header (12), first extent at 52: start_lo at +8.
    img[off + 52 + 8..off + 52 + 12].copy_from_slice(&0x7FFF_FFF0u32.to_le_bytes());
    let mut fs3 = ro(img).unwrap();
    assert_eq!(fs3.read(nbig, 0, &mut [0u8; 16]), Err(Error::Corrupt("extent outside the file system")));
    let mut img = nocsum.clone();
    img[off + 40] = 0; // extent header magic
    let mut fs4 = ro(img).unwrap();
    assert_eq!(fs4.read(nbig, 0, &mut [0u8; 16]), Err(Error::Corrupt("bad extent header magic")));
    let mut img = nocsum.clone();
    img[off + 40 + 4..off + 40 + 6].copy_from_slice(&50u16.to_le_bytes()); // eh_max beyond i_block
    let mut fs5 = ro(img).unwrap();
    assert_eq!(fs5.read(nbig, 0, &mut [0u8; 16]), Err(Error::Corrupt("bad extent header")));
    // A directory entry with a zero record length (no checksums to hide behind).
    let mut fs6 = ro(nocsum.clone()).unwrap();
    let nmany = fs6.resolve("/many").unwrap();
    // Block 1 of the directory is a leaf in hashed and linear layouts alike.
    let leaf = fs6.physical_block(nmany, 1).unwrap().unwrap() as usize * 4096;
    let mut img = nocsum.clone();
    img[leaf + 4..leaf + 6].copy_from_slice(&0u16.to_le_bytes());
    let mut fs7 = ro(img).unwrap();
    assert_eq!(fs7.read_dir(nmany).err(), Some(Error::Corrupt("directory entry length")));
    // With checksums, the same damage is caught by the block checksum.
    let leaf = fs.physical_block(many, 1).unwrap().unwrap() as usize * 4096;
    let mut img = base.clone();
    img[leaf + 10] ^= 0x20;
    let mut fs8 = ro(img).unwrap();
    assert_eq!(fs8.read_dir(many).err(), Some(Error::Corrupt("directory block checksum")));
    let _ = big_off;
}

/// Walks the whole tree, reading every file and link.
fn walk(fs: &mut Ext4<Vec<u8>>, dir: u32, depth: u32) -> Result<usize, Error> {
    if depth > 20 {
        return Ok(0);
    }
    let mut n = 0;
    for e in fs.read_dir(dir)? {
        let st = fs.stat(e.ino)?;
        n += 1;
        if st.is_dir() {
            n += walk(fs, e.ino, depth + 1)?;
        } else if st.is_symlink() {
            fs.readlink(e.ino)?;
        } else {
            let mut buf = vec![0u8; 65536];
            let mut off = 0;
            while off < st.size.min(1 << 22) {
                let r = fs.read(e.ino, off, &mut buf)?;
                if r == 0 {
                    break;
                }
                off += r as u64;
            }
        }
    }
    Ok(n)
}

/// Random single-byte damage anywhere in the metadata-heavy start of the
/// image: every outcome must be an error or a successful walk, never a
/// panic, hang or out-of-range access.
#[test]
fn random_corruption_never_panics() {
    for name in ["ext4-1k", "ext4-nocsum", "ext3-1k"] {
        let base = fixture(name);
        let mut fs = ro(base.clone()).unwrap();
        let clean = walk(&mut fs, ROOT_INO, 0).unwrap();
        assert!(clean > 400);
        // Metadata byte ranges: superblock and descriptors, the first
        // inode-table blocks, and the blocks of a few directories.
        let bs = fs.block_size();
        let mut ranges: Vec<(usize, usize)> = vec![(1024, 4096 + 2 * bs)];
        let it = fs.groups()[0].inode_table as usize * bs;
        ranges.push((it, it + 8 * bs));
        for p in ["/", "/many", "/dir", "/dir/a"] {
            let ino = fs.resolve(p).unwrap();
            let blocks = fs.stat(ino).unwrap().size / bs as u64;
            for l in 0..blocks {
                if let Some(b) = fs.physical_block(ino, l).unwrap() {
                    ranges.push((b as usize * bs, (b as usize + 1) * bs));
                }
            }
        }
        let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ name.len() as u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut outcomes = [0u32; 2];
        for _ in 0..300 {
            let mut img = base.clone();
            for _ in 0..3 {
                let (a, b) = ranges[rnd(ranges.len() as u64) as usize];
                let at = a + rnd((b - a) as u64) as usize;
                img[at] ^= 1 << rnd(8);
            }
            let r = ro(img).and_then(|mut fs| walk(&mut fs, ROOT_INO, 0));
            outcomes[r.is_ok() as usize] += 1;
        }
        eprintln!("{name}: {} walks succeeded, {} reported corruption", outcomes[1], outcomes[0]);
    }
}
