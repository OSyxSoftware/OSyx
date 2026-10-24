//! File system checks run by `init`: OSyx Std `fs` through the system
//! calls into the kernel VFS and every mounted file system.
//!
//! Files left for the host to verify after QEMU exits:
//! * `/boot/osyx-out/*` on the FAT32 EFI partition (read with mtools);
//! * `/osyx-out/*` on the ext4 root, when the root is ext4 and writable
//!   (read with debugfs / e2fsck).

use super::{worker, Tests};
use os_std::error::ErrorKind;
use os_std::fs::{self, File, OpenOptions};
use os_std::io::SeekFrom;
use os_std::os::raw::{self, Errno};
use os_std::prelude::*;
use os_std::{env, os, thread};

pub const OUT_TEXT: &str = "Written by OSyx init through os_std::fs.\n";
pub const OUT_BIN_LEN: usize = 200_000;

pub fn out_byte(i: usize) -> u8 {
    (i * 31 + 5) as u8
}

fn kind_of<T>(r: os_std::error::Result<T>) -> Option<ErrorKind> {
    r.err().map(|e| e.kind())
}

fn fstype(path: &str) -> String {
    match os::statfs(path) {
        Ok(s) => {
            let end = s.fstype.iter().position(|&b| b == 0).unwrap_or(16);
            String::from_utf8_lossy(&s.fstype[..end]).into_owned()
        }
        Err(e) => format!("error: {e}"),
    }
}

fn fresh_dir(p: &str) -> bool {
    match fs::remove_dir_all(p) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => println!("removing {p}: {e}"),
    }
    fs::create_dir(p).is_ok()
}

pub fn run(t: &mut Tests) {
    t.check("fs: mounted tree (/, /boot, /tmp, /run, /dev)", || {
        let table: Vec<(&str, String)> =
            ["/", "/boot", "/tmp", "/run", "/dev"].iter().map(|p| (*p, fstype(p))).collect();
        println!("mounts: {table:?}");
        table[0].1 == "ext4" && table[1].1 == "fat32" && table[2].1 == "tmpfs" && table[3].1 == "tmpfs" && table[4].1 == "devfs"
    });
    t.check("fs: ext4 root: files from mke2fs, owners, links, sparse", || {
        let pattern: Vec<u8> = (0..300_000usize).map(|i| ((i * 11 + 3) % 253) as u8).collect();
        let hello = fs::read_to_string("/osyx-seed/hello.txt").as_deref() == Ok("Hello from the OSyx root file system!\n");
        let bin = fs::read("/osyx-seed/pattern.bin").is_ok_and(|b| b == pattern);
        let link = fs::read_link("/osyx-seed/link").is_ok_and(|p| p == "hello.txt")
            && fs::read_to_string("/osyx-seed/link").is_ok_and(|s| s.starts_with("Hello"));
        let sparse = File::open("/osyx-seed/sparse.bin").is_ok_and(|mut f| {
            let mut end = [0u8; 3];
            let mut hole = [1u8; 64];
            f.metadata().is_ok_and(|m| m.len() == (5 << 30) + 3 && m.blocks() < 64)
                && f.seek(SeekFrom::Start(3 << 30)).is_ok()
                && f.read_exact(&mut hole).is_ok()
                && hole == [0; 64]
                && f.seek(SeekFrom::End(-3)).is_ok()
                && f.read_exact(&mut end).is_ok()
                && &end == b"end"
        });
        let owners = fs::metadata("/osyx-seed/secret").is_ok_and(|m| m.uid() == 0 && m.permissions().mode() == 0o600)
            && fs::metadata("/home/user").is_ok_and(|m| m.uid() == 1000 && m.is_dir())
            && fs::metadata("/tmp").is_ok_and(|m| m.permissions().mode() == 0o1777);
        let programs = fs::metadata("/bin/init").is_ok_and(|m| m.is_file() && m.permissions().mode() == 0o755);
        hello && bin && link && sparse && owners && programs
    });
    t.check("fs: ext4 root: write, rename, links, chmod; files for the host", || {
        if !fresh_dir("/osyx-out") {
            return false;
        }
        let data: Vec<u8> = (0..OUT_BIN_LEN).map(out_byte).collect();
        let bin = File::create("/osyx-out/pattern.bin").is_ok_and(|mut f| {
            data.chunks(7_000).all(|c| f.write_all(c).is_ok()) && f.sync_all().is_ok()
        });
        let tree = fs::create_dir_all("/osyx-out/a/b/c").is_ok()
            && fs::write("/osyx-out/a/b/c/deep.txt", "deep\n").is_ok()
            && fs::rename("/osyx-out/a/b", "/osyx-out/moved").is_ok()
            && fs::symlink("moved/c/deep.txt", "/osyx-out/deep-link").is_ok()
            && fs::read_to_string("/osyx-out/deep-link").as_deref() == Ok("deep\n");
        let perm = fs::write("/osyx-out/private.txt", "640\n").is_ok()
            && fs::set_permissions("/osyx-out/private.txt", fs::Permissions::from_mode(0o640)).is_ok();
        let gone = fs::write("/osyx-out/temp.txt", "x").is_ok() && fs::remove_file("/osyx-out/temp.txt").is_ok();
        let child = matches!(worker(&["fs-write", "/osyx-out/from-worker.txt"]).status(), Ok(s) if s.success());
        let text = File::create("/osyx-out/hello-from-osyx.txt")
            .is_ok_and(|mut f| f.write_all(OUT_TEXT.as_bytes()).is_ok() && f.sync_all().is_ok());
        let back = fs::read("/osyx-out/pattern.bin").is_ok_and(|b| b == data);
        bin && tree && perm && gone && child && text && back
    });
    t.check("fs: the specification example on /tmp", || {
        fresh_dir("/tmp/example")
            && fs::write("/tmp/example/hello.txt", b"Hello OSyx!").is_ok()
            && fs::read("/tmp/example/hello.txt").is_ok_and(|d| d == b"Hello OSyx!")
            && fs::remove_dir_all("/tmp/example").is_ok()
    });
    t.check("fs: 200 KB file, seek and partial reads (tmpfs)", || {
        let data: Vec<u8> = (0..OUT_BIN_LEN).map(out_byte).collect();
        let written = File::create("/tmp/pattern.bin").is_ok_and(|mut f| {
            data.chunks(7_000).all(|c| f.write_all(c).is_ok()) && f.sync_all().is_ok()
        });
        let Ok(mut f) = File::open("/tmp/pattern.bin") else { return false };
        let mut mid = [0u8; 1000];
        let seeked = f.seek(SeekFrom::Start(123_457)).ok() == Some(123_457)
            && f.read_exact(&mut mid).is_ok()
            && mid[..] == data[123_457..124_457]
            && f.seek(SeekFrom::Current(-1000)).ok() == Some(123_457)
            && f.seek(SeekFrom::End(-10)).ok() == Some(OUT_BIN_LEN as u64 - 10);
        let mut tail = Vec::new();
        let tail_ok = f.read_to_end(&mut tail).ok() == Some(10) && tail[..] == data[OUT_BIN_LEN - 10..];
        drop(f);
        written && seeked && tail_ok && fs::remove_file("/tmp/pattern.bin").is_ok()
    });
    t.check("fs: append, set_len and create_new", || {
        let p = "/tmp/log.txt";
        let _ = fs::remove_file(p);
        let created = File::create_new(p).is_ok_and(|mut f| f.write_all(b"one\n").is_ok());
        let appended = OpenOptions::new().append(true).open(p).is_ok_and(|mut f| {
            f.write_all(b"two\n").is_ok() && f.seek(SeekFrom::Start(0)).is_ok() && f.write_all(b"three\n").is_ok()
        });
        let text_ok = fs::read_to_string(p).as_deref() == Ok("one\ntwo\nthree\n");
        let grown = OpenOptions::new().read(true).write(true).open(p).is_ok_and(|f| {
            f.set_len(3).is_ok() && f.set_len(5000).is_ok() && f.metadata().is_ok_and(|m| m.len() == 5000)
        });
        let back = fs::read(p).unwrap_or_default();
        let zero_filled = back.len() == 5000 && &back[..3] == b"one" && back[3..].iter().all(|&b| b == 0);
        let dup = kind_of(File::create_new(p)) == Some(ErrorKind::AlreadyExists);
        created && appended && text_ok && grown && zero_filled && dup && fs::remove_file(p).is_ok()
    });
    t.check("fs: directories, rename and recursive removal", || {
        let made = fs::create_dir_all("/tmp/tree/a/b/c").is_ok()
            && fs::write("/tmp/tree/a/one.txt", "1").is_ok()
            && fs::write("/tmp/tree/a/b/A Longer Name.txt", "22").is_ok();
        let renamed = fs::rename("/tmp/tree/a/one.txt", "/tmp/tree/a/b/c/uno.txt").is_ok()
            && fs::rename("/tmp/tree/a/b", "/tmp/tree/moved").is_ok();
        let layout = !fs::exists("/tmp/tree/a/b").unwrap_or(true)
            && fs::read_to_string("/tmp/tree/moved/c/uno.txt").as_deref() == Ok("1")
            && fs::metadata("/tmp/tree/moved/A Longer Name.txt").is_ok_and(|m| m.len() == 2);
        let not_empty = kind_of(fs::remove_dir("/tmp/tree")) == Some(ErrorKind::DirectoryNotEmpty);
        let into_itself = fs::rename("/tmp/tree", "/tmp/tree/moved/inside").is_err();
        let cross = kind_of(fs::rename("/tmp/tree/moved/c/uno.txt", "/run/uno.txt")) == Some(ErrorKind::CrossesDevices);
        let removed = fs::remove_dir_all("/tmp/tree").is_ok() && !fs::exists("/tmp/tree").unwrap_or(true);
        made && renamed && layout && not_empty && into_itself && cross && removed
    });
    t.check("fs: errors map to the right kinds", || {
        kind_of(File::open("/tmp/missing.txt")) == Some(ErrorKind::NotFound)
            && kind_of(File::open("/no-dir/x.txt")) == Some(ErrorKind::NotFound)
            && kind_of(File::create("/tmp")) == Some(ErrorKind::IsADirectory)
            && kind_of(fs::read_dir("/dev/null")) == Some(ErrorKind::NotADirectory)
            && kind_of(fs::create_dir("/tmp")) == Some(ErrorKind::AlreadyExists)
            && kind_of(fs::remove_dir("/tmp")) == Some(ErrorKind::ResourceBusy)
            && kind_of(fs::remove_file("/run")) == Some(ErrorKind::ResourceBusy)
            && kind_of(fs::read("")) == Some(ErrorKind::InvalidInput)
            && kind_of(fs::write(format!("/tmp/{}", "n".repeat(300)), "x")) == Some(ErrorKind::InvalidInput)
    });
    t.check("fs: symbolic links, metadata and times", || {
        if !fresh_dir("/tmp/links") {
            return false;
        }
        let made = fs::write("/tmp/links/target.txt", "T!").is_ok()
            && fs::symlink("target.txt", "/tmp/links/rel").is_ok()
            && fs::symlink("/dev/zero", "/tmp/links/zero").is_ok()
            && fs::symlink("loop", "/tmp/links/loop").is_ok();
        let follow = fs::metadata("/tmp/links/rel").is_ok_and(|m| m.is_file() && m.len() == 2);
        let lmeta = fs::symlink_metadata("/tmp/links/rel").is_ok_and(|m| m.is_symlink());
        let target = fs::read_link("/tmp/links/rel").is_ok_and(|p| p == "target.txt");
        let dev = fs::metadata("/tmp/links/zero").is_ok_and(|m| m.file_type().is_char_device());
        let looped = kind_of(fs::metadata("/tmp/links/loop")) == Some(ErrorKind::FilesystemLoop);
        let listed = fs::read_dir("/tmp/links").is_ok_and(|d| {
            d.filter(|e| e.file_type().is_symlink()).count() == 3
        });
        let m = fs::metadata("/tmp/links/target.txt");
        let times = m.is_ok_and(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(os_std::time::UNIX_EPOCH).ok())
                .is_some_and(|d| d.as_secs() > 1_577_836_800)
                && m.ino() != 0
                && m.permissions().mode() == 0o644
                && m.uid() == 0
        });
        let devs = fs::metadata("/tmp").ok().map(|m| m.dev()) != fs::metadata("/run").ok().map(|m| m.dev());
        let removed = fs::remove_dir_all("/tmp/links").is_ok();
        made && follow && lmeta && target && dev && looped && listed && times && devs && removed
    });
    t.check("fs: current directory, relative paths, inherited by children", || {
        let ok = fresh_dir("/tmp/cwd")
            && env::set_current_dir("/tmp/cwd").is_ok()
            && env::current_dir().is_ok_and(|p| p == "/tmp/cwd")
            && fs::create_dir("sub").is_ok()
            && fs::write("sub/rel.txt", "relative").is_ok()
            && fs::read_to_string("/tmp/cwd/sub/rel.txt").as_deref() == Ok("relative")
            && env::set_current_dir("sub/../sub").is_ok()
            && env::current_dir().is_ok_and(|p| p == "/tmp/cwd/sub")
            && matches!(worker(&["pwd", "/tmp/cwd/sub"]).status(), Ok(s) if s.success());
        let back = env::set_current_dir("/").is_ok();
        ok && back && fs::remove_dir_all("/tmp/cwd").is_ok()
    });
    t.check("fs: kernel validates handles and buffers", || {
        let Ok(h) = raw::open("/dev/zero", raw::OPEN_READ, 0) else { return false };
        let bad_path = raw::raw_syscall(16, 0xFFFF_8000_0000_0000, 4, raw::OPEN_READ) == Err(Errno::Fault);
        let bad_buf = raw::read_raw(h, 0xFFFF_8000_0000_0000, 16) == Err(Errno::Fault);
        let mut first = [0xFFu8; 5];
        let ok_read = raw::read(h, &mut first) == Ok(5) && first == [0; 5];
        let ro_write = raw::write(h, b"x") == Err(Errno::BadHandle);
        let closed = raw::close(h).is_ok() && raw::close(h) == Err(Errno::BadHandle);
        let stale = raw::read(h, &mut first) == Err(Errno::BadHandle);
        let console = raw::close(1) == Err(Errno::BadHandle);
        bad_path && bad_buf && ok_read && ro_write && closed && stale && console
    });
    t.check("fs: per-process handle limit", || {
        let mut open = Vec::new();
        let err = loop {
            match File::open("/dev/null") {
                Ok(f) => open.push(f),
                Err(e) => break e,
            }
            if open.len() > raw::MAX_HANDLES + 16 {
                return false;
            }
        };
        let n = open.len();
        open.clear();
        println!("opened {n} handles before {err}");
        n == raw::MAX_HANDLES && err.raw_os_error() == Some(Errno::TooManyHandles) && File::open("/dev/null").is_ok()
    });
    t.check("fs: 4 threads write their own files concurrently", || {
        let hs: Vec<_> = (0..4usize)
            .map(|k| {
                thread::spawn(move || {
                    let path = format!("/tmp/thread-{k}.bin");
                    let data: Vec<u8> = (0..40_000usize).map(|i| (i * (k + 3)) as u8).collect();
                    let w = File::create(&path).is_ok_and(|mut f| data.chunks(4096).all(|c| f.write_all(c).is_ok()));
                    w && fs::read(&path).is_ok_and(|b| b == data) && fs::remove_file(&path).is_ok()
                })
            })
            .collect();
        hs.into_iter().all(|h| h.join().unwrap_or(false))
    });
    t.check("fs: handles left open at exit are closed by the kernel", || {
        let ok = matches!(worker(&["fs-leak", "/tmp/leaked.txt"]).status(), Ok(s) if s.success());
        ok && fs::read_to_string("/tmp/leaked.txt").as_deref() == Ok("still here\n") && fs::remove_file("/tmp/leaked.txt").is_ok()
    });
    t.check("fs: /dev devices from user space", || {
        let null = File::options().read(true).write(true).open("/dev/null").is_ok_and(|mut f| {
            let mut b = [1u8; 4];
            f.read(&mut b).ok() == Some(0) && f.write(b"gone").ok() == Some(4)
        });
        let zero = File::open("/dev/zero").is_ok_and(|mut f| {
            let mut b = [1u8; 64];
            f.read_exact(&mut b).is_ok() && b == [0; 64]
        });
        let random = match File::open("/dev/random") {
            Ok(mut f) => {
                let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
                f.read_exact(&mut a).is_ok() && f.read_exact(&mut b).is_ok() && a != b && a != [0; 32]
            }
            Err(e) => {
                println!("/dev/random: {e}");
                false
            }
        };
        let console = File::options().write(true).open("/dev/console").is_ok_and(|mut f| f.write_all(b"init writes to /dev/console\n").is_ok());
        let disks = fs::read_dir("/dev/disks").is_ok_and(|d| {
            let v: Vec<_> = d.collect();
            !v.is_empty() && v.iter().all(|e| e.file_type().is_block_device())
        });
        // The input core's seat stream; per-device nodes only for attached devices.
        let input = fs::metadata("/dev/input/seat0").is_ok_and(|m| m.file_type().is_char_device())
            && fs::read_dir("/dev/input").is_ok_and(|mut d| d.all(|e| e.file_name() == "seat0" || e.file_name().starts_with("event")));
        null && zero && random && console && disks && input
    });
    t.check("fs: an unprivileged process is confined", || {
        let secret = File::options().write(true).create_new(true).mode(0o600).open("/tmp/root-only").is_ok_and(|mut f| f.write_all(b"secret").is_ok());
        let ok = matches!(worker(&["as-user", "1000"]).status(), Ok(s) if s.success());
        secret && ok && fs::remove_file("/tmp/root-only").is_ok() && fs::remove_file("/tmp/user-file").is_ok()
    });
    t.check("fs: mount and unmount a size-limited tmpfs", || {
        if !fresh_dir("/tmp/mnt") || os::mount("", "/tmp/mnt", "tmpfs", false, 128 * 1024).is_err() {
            return false;
        }
        let limited = os::statfs("/tmp/mnt").is_ok_and(|s| s.total_blocks * s.block_size == 128 * 1024);
        let mut f = match File::create("/tmp/mnt/fill") {
            Ok(f) => f,
            Err(_) => return false,
        };
        let chunk = vec![7u8; 32 * 1024];
        let mut written = 0;
        let full = loop {
            match f.write(&chunk) {
                Ok(n) => written += n,
                Err(e) => break e.kind() == ErrorKind::StorageFull,
            }
            if written > 256 * 1024 {
                break false;
            }
        };
        let busy = kind_of(os::unmount("/tmp/mnt")) == Some(ErrorKind::ResourceBusy);
        drop(f);
        let gone = os::unmount("/tmp/mnt").is_ok() && fs::read_dir("/tmp/mnt").is_ok_and(|d| d.count() == 0);
        limited && full && busy && gone && fs::remove_dir("/tmp/mnt").is_ok()
    });
    t.check("fs: Windows-style case-insensitive open", || {
        let made = fs::write("/run/MixedCase.Txt", "ci").is_ok();
        let exact = kind_of(File::open("/RUN/mixedcase.txt")) == Some(ErrorKind::NotFound);
        let ci = OpenOptions::new().read(true).case_insensitive(true).open("/RUN/mixedcase.txt").is_ok();
        made && exact && ci && fs::remove_file("/run/MixedCase.Txt").is_ok()
    });
    t.check("fs: FAT32 /boot: files for the host, case-insensitive names", || {
        if !fresh_dir("/boot/osyx-out") {
            return false;
        }
        let data: Vec<u8> = (0..OUT_BIN_LEN).map(out_byte).collect();
        let bin = File::create("/boot/osyx-out/pattern.bin").is_ok_and(|mut f| {
            data.chunks(7_000).all(|c| f.write_all(c).is_ok()) && f.sync_all().is_ok()
        });
        let back = fs::read("/boot/OSYX-OUT/PATTERN.BIN").is_ok_and(|b| b == data);
        let child = matches!(worker(&["fs-write", "/boot/osyx-out/from-worker.txt"]).status(), Ok(s) if s.success());
        let text = File::create("/boot/osyx-out/hello-from-osyx.txt")
            .is_ok_and(|mut f| f.write_all(OUT_TEXT.as_bytes()).is_ok() && f.sync_all().is_ok());
        let fat_meta = fs::metadata("/boot/osyx-out/pattern.bin").is_ok_and(|m| m.uid() == 0 && m.attributes() & 0x20 != 0);
        let no_links = kind_of(fs::symlink("x", "/boot/osyx-out/link")) == Some(ErrorKind::Unsupported);
        bin && back && child && text && fat_meta && no_links
    });
}
