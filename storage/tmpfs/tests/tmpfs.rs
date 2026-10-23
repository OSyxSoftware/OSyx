use osyx_abi::{Errno, KIND_DIR, KIND_FILE, KIND_SYMLINK};
use osyx_tmpfs::{TmpFs, NODE_COST, PAGE, ROOT};
use osyx_vfs::{Cred, NewNode};
use std::collections::BTreeMap;

const ME: Cred = Cred { uid: 1000, gid: 100 };

fn clock() -> i64 {
    1_700_000_000_000_000_000
}

fn fs(limit: u64) -> TmpFs {
    TmpFs::new(limit, 0o1777, clock)
}

fn read_all(t: &mut TmpFs, ino: u64) -> Vec<u8> {
    let size = t.stat(ino).unwrap().size as usize;
    let mut buf = vec![0xEE; size + 10];
    let n = t.read(ino, 0, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

#[test]
fn files_directories_and_metadata() {
    let mut t = fs(1 << 20);
    let d = t.create(ROOT, "docs", NewNode::Dir, 0o750, &ME).unwrap();
    let f = t.create(d, "a.txt", NewNode::File, 0o640, &ME).unwrap();
    assert_eq!(t.write(f, 0, b"hello").unwrap(), 5);
    assert_eq!(t.write(f, 5, b", world").unwrap(), 7);
    assert_eq!(read_all(&mut t, f), b"hello, world");
    let st = t.stat(f).unwrap();
    assert_eq!((st.kind, st.size, st.mode, st.uid, st.gid, st.nlink), (KIND_FILE, 12, 0o640, 1000, 100, 1));
    assert_eq!(st.blocks, (PAGE / 512) as u64);
    assert_eq!(st.mtime_ns, clock());
    let ds = t.stat(d).unwrap();
    assert_eq!((ds.kind, ds.nlink), (KIND_DIR, 2));
    assert_eq!(t.stat(ROOT).unwrap().nlink, 3);
    assert_eq!(t.lookup(d, "a.txt"), Ok(f));
    assert_eq!(t.lookup(d, ".."), Ok(ROOT));
    assert_eq!(t.lookup(ROOT, ".."), Ok(ROOT));
    assert_eq!(t.lookup(d, "A.TXT"), Err(Errno::NotFound), "tmpfs is case-sensitive");
    assert_eq!(t.lookup(f, "x"), Err(Errno::NotADirectory));
    assert_eq!(t.create(d, "a.txt", NewNode::File, 0o644, &ME), Err(Errno::AlreadyExists));
    assert_eq!(t.create(d, "bad/name", NewNode::File, 0o644, &ME), Err(Errno::InvalidArgument));
    let names: Vec<_> = t.read_dir(ROOT).unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
    assert_eq!(names, [("docs".to_string(), KIND_DIR)]);
    let l = t.create(d, "link", NewNode::Symlink("../docs/a.txt"), 0, &ME).unwrap();
    assert_eq!(t.readlink(l).unwrap(), "../docs/a.txt");
    assert_eq!(t.stat(l).unwrap().kind, KIND_SYMLINK);
    assert_eq!(t.readlink(f), Err(Errno::InvalidArgument));
    t.set_mode(f, 0o4755).unwrap();
    assert_eq!(t.stat(f).unwrap().mode, 0o4755);
    t.check().unwrap();
}

#[test]
fn empty_sparse_and_large_files() {
    let mut t = fs(1 << 20);
    let e = t.create(ROOT, "empty", NewNode::File, 0o644, &ME).unwrap();
    assert_eq!(read_all(&mut t, e), b"");
    assert_eq!(t.read(e, 100, &mut [0u8; 4]).unwrap(), 0);
    // 10 GiB sparse file with data only at the end: one page charged.
    let s = t.create(ROOT, "sparse", NewNode::File, 0o644, &ME).unwrap();
    let before = t.used_bytes();
    let off = 10u64 << 30;
    t.write(s, off, b"tail").unwrap();
    assert_eq!(t.used_bytes() - before, PAGE as u64);
    assert_eq!(t.stat(s).unwrap().size, off + 4);
    let mut buf = [0xFFu8; 8];
    assert_eq!(t.read(s, off - 4, &mut buf).unwrap(), 8);
    assert_eq!(&buf, b"\0\0\0\0tail");
    assert_eq!(t.read(s, 12345, &mut buf).unwrap(), 8);
    assert_eq!(buf, [0; 8]);
    assert_eq!(t.write(s, osyx_tmpfs::MAX_FILE_SIZE, b"x"), Err(Errno::FileTooLarge));
    assert_eq!(t.write(s, u64::MAX, b"x"), Err(Errno::FileTooLarge));
    t.check().unwrap();
}

#[test]
fn truncate_frees_pages_and_zeroes_tail() {
    let mut t = fs(1 << 20);
    let f = t.create(ROOT, "f", NewNode::File, 0o644, &ME).unwrap();
    let data: Vec<u8> = (0..3 * PAGE + 100).map(|i| (i % 251) as u8 + 1).collect();
    t.write(f, 0, &data).unwrap();
    let full = t.used_bytes();
    t.truncate(f, 10).unwrap();
    assert_eq!(full - t.used_bytes(), 3 * PAGE as u64);
    t.truncate(f, 2 * PAGE as u64).unwrap();
    let back = read_all(&mut t, f);
    assert_eq!(&back[..10], &data[..10]);
    assert!(back[10..].iter().all(|&b| b == 0), "grown region must read as zeros");
    assert_eq!(t.truncate(ROOT, 0), Err(Errno::IsADirectory));
    t.check().unwrap();
}

#[test]
fn memory_limit_is_enforced_and_reclaimed() {
    let limit = NODE_COST * 4 + 3 * PAGE as u64;
    let mut t = fs(limit);
    let base = t.used_bytes();
    let f = t.create(ROOT, "big", NewNode::File, 0o644, &ME).unwrap();
    let data = vec![7u8; 10 * PAGE];
    // Only whole pages that fit are written: a short write, then NoSpace.
    let n = t.write(f, 0, &data).unwrap();
    assert!(n > 0 && n < data.len() && n.is_multiple_of(PAGE), "short write of {n}");
    assert_eq!(t.write(f, n as u64, &data), Err(Errno::NoSpace));
    assert!(t.used_bytes() <= limit);
    // Overwriting existing pages needs no new memory.
    assert_eq!(t.write(f, 0, &data[..n]).unwrap(), n);
    let u = t.usage();
    assert!(u.free_blocks < 1);
    t.unlink(ROOT, "big").unwrap();
    assert_eq!(t.used_bytes(), base, "all memory returned");
    // Many small nodes hit the limit too.
    let mut made = 0;
    loop {
        match t.create(ROOT, &format!("n{made}"), NewNode::File, 0o644, &ME) {
            Ok(_) => made += 1,
            Err(e) => {
                assert_eq!(e, Errno::NoSpace);
                break;
            }
        }
    }
    assert!(made >= 3);
    for i in 0..made {
        t.unlink(ROOT, &format!("n{i}")).unwrap();
    }
    assert_eq!(t.used_bytes(), base);
    assert_eq!(t.node_count(), 1);
    t.check().unwrap();
}

#[test]
fn unlinked_open_files_live_until_release() {
    let mut t = fs(1 << 20);
    let base = t.used_bytes();
    let f = t.create(ROOT, "f", NewNode::File, 0o644, &ME).unwrap();
    t.write(f, 0, b"still readable").unwrap();
    t.open(f).unwrap();
    t.open(f).unwrap();
    t.unlink(ROOT, "f").unwrap();
    assert_eq!(t.lookup(ROOT, "f"), Err(Errno::NotFound));
    assert_eq!(read_all(&mut t, f), b"still readable");
    assert_eq!(t.stat(f).unwrap().nlink, 0);
    t.check().unwrap();
    t.release(f);
    assert!(t.stat(f).is_ok());
    t.release(f);
    assert_eq!(t.stat(f), Err(Errno::NotFound));
    assert_eq!(t.used_bytes(), base);
    t.check().unwrap();
}

#[test]
fn rename_semantics() {
    let mut t = fs(1 << 20);
    let a = t.create(ROOT, "a", NewNode::Dir, 0o755, &ME).unwrap();
    let b = t.create(ROOT, "b", NewNode::Dir, 0o755, &ME).unwrap();
    let f = t.create(a, "f", NewNode::File, 0o644, &ME).unwrap();
    let g = t.create(b, "g", NewNode::File, 0o644, &ME).unwrap();
    t.write(g, 0, b"old").unwrap();
    // File over file replaces it (and frees it).
    t.rename(a, "f", b, "g").unwrap();
    assert_eq!(t.lookup(b, "g"), Ok(f));
    assert_eq!(t.stat(g), Err(Errno::NotFound));
    // Directory moves update parent links and `..`.
    let c = t.create(a, "c", NewNode::Dir, 0o755, &ME).unwrap();
    t.rename(a, "c", b, "c2").unwrap();
    assert_eq!(t.lookup(c, ".."), Ok(b));
    assert_eq!((t.stat(a).unwrap().nlink, t.stat(b).unwrap().nlink), (2, 3));
    // Refusals.
    assert_eq!(t.rename(ROOT, "b", c, "inside"), Err(Errno::InvalidArgument), "dir into its own subtree");
    assert_eq!(t.rename(ROOT, "b", ROOT, "b"), Ok(()));
    assert_eq!(t.rename(b, "g", ROOT, "a"), Err(Errno::IsADirectory));
    assert_eq!(t.rename(ROOT, "a", b, "g"), Err(Errno::NotADirectory));
    assert_eq!(t.rename(ROOT, "b", ROOT, "a").map(|_| ()), Ok(()), "dir over empty dir");
    let x = t.create(ROOT, "x", NewNode::Dir, 0o755, &ME).unwrap();
    t.create(x, "y", NewNode::File, 0o644, &ME).unwrap();
    let z = t.create(ROOT, "z", NewNode::Dir, 0o755, &ME).unwrap();
    assert_eq!(t.rename(ROOT, "z", ROOT, "x"), Err(Errno::DirectoryNotEmpty));
    assert_eq!(t.rename(ROOT, "missing", ROOT, "q"), Err(Errno::NotFound));
    assert_eq!(t.rmdir(ROOT, "x"), Err(Errno::DirectoryNotEmpty));
    assert_eq!(t.rmdir(ROOT, "z").map(|_| z), Ok(z));
    assert_eq!(t.unlink(ROOT, "x"), Err(Errno::IsADirectory));
    t.check().unwrap();
}

#[test]
fn deep_nesting_and_full_teardown() {
    let mut t = fs(4 << 20);
    let base = t.used_bytes();
    let mut dir = ROOT;
    let mut chain = vec![ROOT];
    for i in 0..100 {
        dir = t.create(dir, &format!("level{i}"), NewNode::Dir, 0o755, &ME).unwrap();
        chain.push(dir);
        let f = t.create(dir, "data", NewNode::File, 0o644, &ME).unwrap();
        t.write(f, 0, &vec![i as u8; 100 + i]).unwrap();
    }
    t.check().unwrap();
    for i in (0..100).rev() {
        let d = chain[i + 1];
        t.unlink(d, "data").unwrap();
        t.rmdir(chain[i], &format!("level{i}")).unwrap();
    }
    assert_eq!(t.used_bytes(), base);
    assert_eq!(t.node_count(), 1);
    t.check().unwrap();
}

/// Random operations against a simple model of path -> contents.
#[test]
fn randomised_operations_match_a_model() {
    let mut t = fs(64 << 20);
    let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut seed = 0x1234_5678_9ABC_DEF0u64;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for _ in 0..3000 {
        let name = format!("f{}", rnd(20));
        match rnd(5) {
            0 => {
                let r = t.create(ROOT, &name, NewNode::File, 0o644, &ME);
                assert_eq!(r.is_ok(), !model.contains_key(&name));
                model.entry(name).or_default();
            }
            1 => {
                if let Ok(ino) = t.lookup(ROOT, &name) {
                    let off = rnd(3 * PAGE as u64) as usize;
                    let data: Vec<u8> = (0..rnd(2 * PAGE as u64) as usize).map(|i| (i + off) as u8).collect();
                    t.write(ino, off as u64, &data).unwrap();
                    let m = model.get_mut(&name).unwrap();
                    if m.len() < off + data.len() {
                        m.resize(off + data.len(), 0);
                    }
                    m[off..off + data.len()].copy_from_slice(&data);
                }
            }
            2 => {
                if let Ok(ino) = t.lookup(ROOT, &name) {
                    let len = rnd(4 * PAGE as u64) as usize;
                    t.truncate(ino, len as u64).unwrap();
                    model.get_mut(&name).unwrap().resize(len, 0);
                }
            }
            3 => {
                let r = t.unlink(ROOT, &name);
                assert_eq!(r.is_ok(), model.remove(&name).is_some());
            }
            _ => {
                let to = format!("f{}", rnd(20));
                let r = t.rename(ROOT, &name, ROOT, &to);
                if let Some(v) = model.remove(&name) {
                    r.unwrap();
                    model.insert(to, v);
                } else {
                    assert_eq!(r, Err(Errno::NotFound));
                }
            }
        }
    }
    for (name, want) in &model {
        let ino = t.lookup(ROOT, name).unwrap();
        assert_eq!(&read_all(&mut t, ino), want, "{name}");
    }
    assert_eq!(t.read_dir(ROOT).unwrap().len(), model.len());
    t.check().unwrap();
}
