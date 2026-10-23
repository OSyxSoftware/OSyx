//! The init program (`\EFI\OSYX\BIN\init`). Runs in ring 3 and checks OSyx
//! Std, the kernel's system call validation, processes (spawn / wait /
//! zombies / orphans) and the preemptive scheduler from the user side,
//! using `worker` as the child program. With a FAT32 data disk mounted at
//! `/` it also checks `os_std::fs` and leaves `/osyx-out/*` behind for the
//! host to verify with mtools.
//!
//! Arguments (kernel command line `init.args=...`):
//! * none:          run the checks; exit status = number of failed checks
//! * `fault`:       dereference an unmapped address (the kernel must kill us)
//! * `panic`:       panic (OSyx Std must report it and exit with 101)
//! * `exit,<code>`: exit with `<code>`

#![no_std]
#![no_main]

use os_std::error::ErrorKind;
use os_std::os::raw::{self, Errno, PROT_EXEC, PROT_READ, PROT_WRITE};
use os_std::prelude::*;
use os_std::process::{Child, Command};
use os_std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use os_std::time::{Duration, Instant};
use os_std::fs::{self, File, OpenOptions};
use os_std::io::SeekFrom;
use os_std::{env, os, process, thread};

#[path = "../fpu.rs"]
mod fpu;

os_std::entry!(main);

static RODATA: [u8; 64] = [0x5A; 64];

fn main() -> i32 {
    println!("Hello from OSyx userspace!");
    let args: Vec<String> = env::args().collect();
    println!("args: {:?}", args);

    match args.get(1).map(String::as_str) {
        Some("fault") => {
            println!("reading unmapped address 0x1000 ...");
            // SAFETY: deliberately invalid; the kernel terminates the process.
            let v = unsafe { core::ptr::read_volatile(0x1000 as *const u64) };
            println!("unreachable: read {v}");
            return 99;
        }
        Some("panic") => panic!("panic requested by command line"),
        Some("exit") => {
            let code = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            process::exit(code);
        }
        _ => {}
    }

    let mut t = Tests::default();
    t.check("std-style collections", || {
        let mut values = Vec::new();
        for v in [10, 20] {
            values.push(v);
        }
        println!("Values: {:?}", values);
        let mut words: BTreeMap<String, usize> = BTreeMap::new();
        for w in "the quick brown fox jumps over the lazy dog the end".split(' ') {
            *words.entry(w.to_string()).or_default() += 1;
        }
        values == [10, 20] && words["the"] == 3 && words.len() == 9
    });
    t.check("heap: 2 MiB vector and many small boxes", || {
        let big = vec![0xA5u8; 2 << 20];
        let boxes: Vec<Box<u64>> = (0..10_000).map(Box::new).collect();
        big.iter().all(|&b| b == 0xA5) && boxes.iter().map(|b| **b).sum::<u64>() == 49_995_000
    });
    t.check("string formatting", || {
        format!("{:>8.3}|{:#x}|{:?}", "OSyx Std", 255, Some("x")) == "     OSy|0xff|Some(\"x\")"
    });
    t.check("sysinfo", || match os::sysinfo() {
        Ok(info) => {
            println!(
                "kernel {} | ABI {} | pid {} | {} MiB free of {} MiB | page {} B",
                os::kernel_version(&info),
                info.abi_version,
                info.pid,
                info.free_memory >> 20,
                info.total_memory >> 20,
                info.page_size
            );
            info.abi_version == raw::ABI_VERSION && info.page_size == 4096 && info.free_memory <= info.total_memory
        }
        Err(e) => {
            println!("sysinfo failed: {e}");
            false
        }
    });
    t.check("monotonic clock", || {
        let a = Instant::now();
        let b = Instant::now();
        b >= a
    });
    t.check("kernel rejects kernel-space pointer", || raw::write_raw(1, 0xFFFF_8000_0000_0000, 8) == Err(Errno::Fault));
    t.check("kernel rejects null pointer", || raw::write_raw(1, 0, 4) == Err(Errno::Fault));
    t.check("kernel rejects unmapped user pointer", || raw::write_raw(1, 0x7000_0000_0000, 16) == Err(Errno::Fault));
    t.check("kernel rejects bad handle", || raw::write_raw(42, RODATA.as_ptr() as usize, 1) == Err(Errno::BadHandle));
    t.check("kernel refuses to write into read-only memory", || {
        raw::sysinfo_raw(RODATA.as_ptr() as usize) == Err(Errno::Fault) && RODATA[0] == 0x5A
    });
    t.check("unknown system call", || raw::raw_syscall(999, 0, 0, 0) == Err(Errno::NotSupported));
    t.check("W^X: writable+executable mapping refused", || {
        raw::map(4096, PROT_READ | PROT_WRITE | PROT_EXEC).err() == Some(Errno::PermissionDenied)
    });
    t.check("map / unmap round trip", || {
        let Ok(p) = raw::map(3 * 4096, PROT_READ | PROT_WRITE) else { return false };
        // SAFETY: freshly mapped, zeroed, writable, 3 pages long.
        let ok = unsafe {
            let s = core::slice::from_raw_parts_mut(p, 3 * 4096);
            let zeroed = s.iter().all(|&b| b == 0);
            s.fill(7);
            zeroed && s[3 * 4096 - 1] == 7
        };
        // SAFETY: no references into the region remain.
        ok && unsafe { raw::unmap(p, 3 * 4096) }.is_ok() && unsafe { raw::unmap(p, 3 * 4096) }.is_err()
    });

    process_checks(&mut t);
    thread_checks(&mut t);
    if fs::metadata("/").is_ok() {
        fs_checks(&mut t);
    } else {
        println!("[user] no volume mounted at /: file system checks skipped");
    }

    println!("user checks: {} passed, {} failed", t.passed, t.failed);
    t.failed as i32
}

fn worker(args: &[&str]) -> Command {
    let mut c = Command::new("worker");
    c.args(args);
    c
}

fn process_checks(t: &mut Tests) {
    t.check(
        "spawn + wait: child exit code",
        || matches!(worker(&["exit", "7"]).status(), Ok(s) if s.code() == Some(7)),
    );
    t.check("spawn: unknown program is NotFound", || {
        Command::new("no-such-program").spawn().map(|c| c.id()).map_err(|e| e.kind()) == Err(ErrorKind::NotFound)
    });
    t.check("wait: no such child", || raw::wait_raw(9999, 0) == Err(Errno::NoChild));
    t.check("wait: bad status pointer keeps the child waitable", || {
        let Ok(child) = worker(&["exit", "5"]).spawn() else { return false };
        let bad = raw::wait_raw(child.id(), 0xFFFF_8000_0000_0000) == Err(Errno::Fault);
        bad && matches!(child.wait(), Ok(s) if s.code() == Some(5))
    });
    t.check("wait_any collects every child once", || {
        let a = worker(&["exit", "11"]).spawn();
        let b = worker(&["exit", "12"]).spawn();
        if a.is_err() || b.is_err() {
            return false;
        }
        let mut codes = Vec::new();
        for _ in 0..2 {
            match process::wait_any() {
                Ok((_, s)) => codes.push(s.code().unwrap_or(-1)),
                Err(_) => return false,
            }
        }
        codes.sort();
        codes == [11, 12] && process::wait_any().map_err(|e| e.kind()).err() == Some(ErrorKind::NoChild)
    });
    t.check("nested spawn (grandchild)", || matches!(worker(&["child", "9"]).status(), Ok(s) if s.code() == Some(9)));
    t.check(
        "child CPU exception is reported",
        || matches!(worker(&["fault"]).status(), Ok(s) if s.exception() == Some(14) && s.code().is_none()),
    );
    t.check("sleep blocks for the requested time", || {
        let start = Instant::now();
        thread::sleep(Duration::from_millis(50));
        thread::yield_now();
        start.elapsed() >= Duration::from_millis(50)
    });
    t.check("preemption: 3 busy children run concurrently", || {
        // Each child spins 300 ms without yielding. Run one after another
        // they would need ~900 ms; preempted round robin, ~300 ms.
        let start = Instant::now();
        let kids: Vec<Child> = (0..3).filter_map(|_| worker(&["spin", "300"]).spawn().ok()).collect();
        let n = kids.len();
        let all_ok = kids.into_iter().all(|k| matches!(k.wait(), Ok(s) if s.success()));
        let took = start.elapsed();
        println!("3 x 300 ms spinners finished in {} ms", took.as_millis());
        n == 3 && all_ok && took >= Duration::from_millis(300) && took < Duration::from_millis(650)
    });
    t.check("FPU/SSE registers survive preemption", || {
        let mut kids_ok = false;
        // init keeps its own pattern in xmm0-15 while two children spin with
        // different patterns in theirs.
        let mine = fpu::preserved_across(0xAAAA_0000_0000_0000, || {
            let a = worker(&["fpu", "1", "200"]).spawn();
            let b = worker(&["fpu", "2", "200"]).spawn();
            if let (Ok(a), Ok(b)) = (a, b) {
                let sa = a.wait();
                let sb = b.wait();
                kids_ok = matches!(sa, Ok(s) if s.success()) && matches!(sb, Ok(s) if s.success());
            }
        });
        mine && kids_ok
    });
    t.check("orphaned grandchild is adopted by the kernel", || {
        let ok = matches!(worker(&["orphan"]).status(), Ok(s) if s.success());
        // Give the orphan (sleeping 100 ms) time to exit before init does.
        thread::sleep(Duration::from_millis(200));
        ok && process::wait_any().map_err(|e| e.kind()).err() == Some(ErrorKind::NoChild)
    });
    t.check("process id from sysinfo", || process::id().is_ok_and(|p| p >= 1));
}

#[derive(Default)]
struct Tests {
    passed: u32,
    failed: u32,
}

impl Tests {
    fn check(&mut self, name: &str, f: impl FnOnce() -> bool) {
        if f() {
            println!("[user] {name} ... ok");
            self.passed += 1;
        } else {
            eprintln!("[user] {name} ... FAILED");
            self.failed += 1;
        }
    }
}

os_std::thread_local! {
    static LOCAL_ID: core::cell::Cell<u64> = core::cell::Cell::new(0);
    static DROP_PROBE: DropProbe = DropProbe;
}

static TLS_DROPS: AtomicUsize = AtomicUsize::new(0);

struct DropProbe;

impl Drop for DropProbe {
    fn drop(&mut self) {
        TLS_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

fn thread_checks(t: &mut Tests) {
    t.check("threads: spawn and join return values", || {
        let hs: Vec<_> = (1..=4u64).map(|i| thread::spawn(move || (1..=i * 10_000).sum::<u64>())).collect();
        let sums: Vec<u64> = hs.into_iter().filter_map(|h| h.join().ok()).collect();
        sums == [50_005_000, 200_010_000, 450_015_000, 800_020_000]
    });
    t.check("threads: distinct ids and names", || {
        let main_id = thread::current().id();
        let h = thread::Builder::new()
            .name(String::from("named"))
            .spawn(|| (thread::current().id(), thread::current().name().map(String::from)))
            .unwrap();
        let (id, name) = h.join().unwrap();
        id != main_id && name.as_deref() == Some("named") && thread::current().name() == Some("main")
    });
    t.check("Mutex: 4 threads x 20000 slow increments under preemption", || {
        // Read, delay, write inside the lock: if the mutex did not exclude,
        // a preemption between read and write would lose increments.
        let m = Arc::new(Mutex::new(0u64));
        let contended = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                let contended = contended.clone();
                thread::spawn(move || {
                    for _ in 0..20_000 {
                        let mut g = match m.try_lock() {
                            Ok(g) => g,
                            Err(_) => {
                                contended.fetch_add(1, Ordering::Relaxed);
                                m.lock().unwrap()
                            }
                        };
                        let v = *g;
                        for _ in 0..40 {
                            core::hint::spin_loop();
                        }
                        *g = v + 1;
                    }
                })
            })
            .collect();
        let joined = hs.into_iter().all(|h| h.join().is_ok());
        let total = *m.lock().unwrap();
        let waits = contended.load(Ordering::Relaxed);
        println!("mutex counter: {total} in {} ms, {waits} contended acquisitions", start.elapsed().as_millis());
        joined && total == 80_000 && waits > 0
    });
    t.check("Condvar: producer / 3 consumers", || {
        let q = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let consumers: Vec<_> = (0..3)
            .map(|_| {
                let q = q.clone();
                thread::spawn(move || {
                    let (m, cv) = &*q;
                    let mut sum = 0u64;
                    loop {
                        let mut g = cv.wait_while(m.lock().unwrap(), |q: &mut VecDeque<u64>| q.is_empty()).unwrap();
                        match g.pop_front() {
                            Some(0) | None => return sum,
                            Some(v) => sum += v,
                        }
                    }
                })
            })
            .collect();
        let (m, cv) = &*q;
        for v in 1..=500u64 {
            m.lock().unwrap().push_back(v);
            cv.notify_one();
            if v % 50 == 0 {
                thread::yield_now();
            }
        }
        m.lock().unwrap().extend([0, 0, 0]);
        cv.notify_all();
        let total: u64 = consumers.into_iter().filter_map(|h| h.join().ok()).sum();
        total == 125_250
    });
    t.check("Condvar: wait_timeout expires", || {
        let m = Mutex::new(());
        let cv = Condvar::new();
        let start = Instant::now();
        let (_g, r) = cv.wait_timeout(m.lock().unwrap(), Duration::from_millis(30)).unwrap();
        r.timed_out() && start.elapsed() >= Duration::from_millis(30)
    });
    t.check("futex: value mismatch and timeout", || {
        let word = AtomicU32::new(0);
        let mismatch = raw::futex_wait(word.as_ptr(), 1, 0) == Err(Errno::WouldBlock);
        let start = Instant::now();
        let timed = raw::futex_wait(word.as_ptr(), 0, 30_000_000) == Err(Errno::TimedOut);
        let unaligned =
            raw::futex_wait((word.as_ptr() as usize + 1) as *const u32, 0, 1) == Err(Errno::InvalidArgument);
        mismatch
            && timed
            && unaligned
            && start.elapsed() >= Duration::from_millis(30)
            && raw::futex_wake(word.as_ptr(), 1) == 0
    });
    t.check("TLS: thread_local! per thread, destructors run", || {
        LOCAL_ID.with(|c| c.set(1));
        let before = TLS_DROPS.load(Ordering::SeqCst);
        let hs: Vec<_> = (2..=4u64)
            .map(|i| {
                thread::spawn(move || {
                    DROP_PROBE.with(|_| ());
                    LOCAL_ID.with(|c| {
                        let initial = c.get();
                        c.set(i);
                        thread::sleep(Duration::from_millis(20)); // let the others run
                        (initial, c.get())
                    })
                })
            })
            .collect();
        let results: Vec<(u64, u64)> = hs.into_iter().filter_map(|h| h.join().ok()).collect();
        results == [(0, 2), (0, 3), (0, 4)]
            && LOCAL_ID.with(|c| c.get()) == 1
            && TLS_DROPS.load(Ordering::SeqCst) == before + 3
    });
    t.check("detached threads are reaped", || {
        let flag = Arc::new(AtomicU32::new(0));
        let f = flag.clone();
        drop(thread::spawn(move || f.store(1, Ordering::SeqCst)));
        let start = Instant::now();
        while thread::detached_pending() > 0 && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        flag.load(Ordering::SeqCst) == 1 && thread::detached_pending() == 0
    });
    t.check(
        "exit with blocked threads ends the process",
        || matches!(worker(&["threads-exit"]).status(), Ok(s) if s.code() == Some(42)),
    );
    t.check(
        "process lives until its last thread exits",
        || matches!(worker(&["thread-last"]).status(), Ok(s) if s.code() == Some(9)),
    );
    t.check(
        "fault in a spawned thread ends the process",
        || matches!(worker(&["thread-fault"]).status(), Ok(s) if s.exception() == Some(14)),
    );
    t.check(
        "panic in a spawned thread exits with 101",
        || matches!(worker(&["thread-panic"]).status(), Ok(s) if s.code() == Some(101)),
    );
}

/// Contents of the files `scripts/build.ps1` puts on the data disk.
const SEED_HELLO: &str = "Hello from the build host!\n";
const SEED_LONG: &str = "Long file names work.\n";
const SEED_BIN_LEN: usize = 150_000;
/// Files left on the disk for the host to check after QEMU exits.
const OUT_TEXT: &str = "Written by OSyx init through os_std::fs.\n";
const OUT_BIN_LEN: usize = 200_000;

fn seed_byte(i: usize) -> u8 {
    (i * 13 + 7) as u8
}

fn out_byte(i: usize) -> u8 {
    (i * 31 + 5) as u8
}

fn kind_of<T>(r: os_std::error::Result<T>) -> Option<ErrorKind> {
    r.err().map(|e| e.kind())
}

fn fs_checks(t: &mut Tests) {
    match fs::remove_dir_all("/osyx-out") {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => println!("[user] removing an old /osyx-out failed: {e}"),
    }
    t.check("fs: seed files written by the host are readable", || {
        let bin = fs::read("/seed.bin").unwrap_or_default();
        fs::read_to_string("/hello.txt").as_deref() == Ok(SEED_HELLO)
            && fs::read_to_string("/DOCS/read me long name.TXT").as_deref() == Ok(SEED_LONG)
            && bin.len() == SEED_BIN_LEN
            && bin.iter().enumerate().all(|(i, &b)| b == seed_byte(i))
    });
    t.check("fs: read_dir lists the root with kinds and sizes", || {
        let Ok(dir) = fs::read_dir("/") else { return false };
        let entries: Vec<fs::DirEntry> = dir.collect();
        let find = |n: &str| entries.iter().find(|e| e.file_name() == n).map(|e| e.metadata());
        println!("root: {:?}", entries.iter().map(|e| e.file_name()).collect::<Vec<_>>());
        find("hello.txt").is_some_and(|m| m.is_file() && m.len() == SEED_HELLO.len() as u64)
            && find("Docs").is_some_and(|m| m.is_dir())
            && find("seed.bin").is_some_and(|m| m.len() == SEED_BIN_LEN as u64)
    });
    t.check("fs: 200 KB file written, re-read, seek and partial reads", || {
        let data: Vec<u8> = (0..OUT_BIN_LEN).map(out_byte).collect();
        let written = fs::create_dir("/osyx-out").is_ok()
            && File::create("/osyx-out/pattern.bin").is_ok_and(|mut f| {
                data.chunks(7_000).all(|c| f.write_all(c).is_ok()) && f.sync_all().is_ok()
            });
        let Ok(mut f) = File::open("/osyx-out/pattern.bin") else { return false };
        let mut mid = [0u8; 1000];
        let seeked = f.seek(SeekFrom::Start(123_457)).ok() == Some(123_457)
            && f.read_exact(&mut mid).is_ok()
            && mid[..] == data[123_457..124_457]
            && f.seek(SeekFrom::Current(-1000)).ok() == Some(123_457)
            && f.seek(SeekFrom::End(-10)).ok() == Some(OUT_BIN_LEN as u64 - 10);
        let mut tail = Vec::new();
        let tail_ok = f.read_to_end(&mut tail).ok() == Some(10) && tail[..] == data[OUT_BIN_LEN - 10..];
        written && seeked && tail_ok && fs::read("/osyx-out/pattern.bin").is_ok_and(|b| b == data)
    });
    t.check("fs: append, set_len and create_new", || {
        let p = "/osyx-out/log.txt";
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
        created && appended && text_ok && grown && zero_filled && kind_of(File::create_new(p)) == Some(ErrorKind::AlreadyExists)
    });
    t.check("fs: directories, rename and recursive removal", || {
        let made = fs::create_dir_all("/osyx-out/tree/a/b/c").is_ok()
            && fs::write("/osyx-out/tree/a/one.txt", "1").is_ok()
            && fs::write("/osyx-out/tree/a/b/A Longer Name.txt", "22").is_ok();
        let renamed = fs::rename("/osyx-out/tree/a/one.txt", "/osyx-out/tree/a/b/c/uno.txt").is_ok()
            && fs::rename("/osyx-out/tree/a/b", "/osyx-out/tree/moved").is_ok();
        let layout = !fs::exists("/osyx-out/tree/a/b").unwrap_or(true)
            && fs::read_to_string("/osyx-out/tree/moved/c/uno.txt").as_deref() == Ok("1")
            && fs::metadata("/osyx-out/tree/moved/a longer name.txt").is_ok_and(|m| m.len() == 2);
        let not_empty = kind_of(fs::remove_dir("/osyx-out/tree")) == Some(ErrorKind::DirectoryNotEmpty);
        let into_itself = fs::rename("/osyx-out/tree", "/osyx-out/tree/moved/inside").is_err();
        let removed = fs::remove_dir_all("/osyx-out/tree").is_ok() && !fs::exists("/osyx-out/tree").unwrap_or(true);
        made && renamed && layout && not_empty && into_itself && removed
    });
    t.check("fs: errors map to the right kinds", || {
        kind_of(File::open("/osyx-out/missing.txt")) == Some(ErrorKind::NotFound)
            && kind_of(File::open("/no-dir/x.txt")) == Some(ErrorKind::NotFound)
            && kind_of(File::create("/Docs")) == Some(ErrorKind::IsADirectory)
            && kind_of(fs::read_dir("/hello.txt")) == Some(ErrorKind::NotADirectory)
            && kind_of(fs::create_dir("/Docs")) == Some(ErrorKind::AlreadyExists)
            && kind_of(fs::remove_file("/Docs")) == Some(ErrorKind::IsADirectory)
            && kind_of(fs::read("hello.txt")) == Some(ErrorKind::InvalidInput)
            && kind_of(fs::write("/osyx-out/bad\u{1}name", "x")) == Some(ErrorKind::InvalidInput)
    });
    t.check("fs: kernel validates handles and buffers", || {
        let Ok(h) = raw::open("/hello.txt", raw::OPEN_READ) else { return false };
        let bad_path = raw::raw_syscall(16, 0xFFFF_8000_0000_0000, 4, raw::OPEN_READ) == Err(Errno::Fault);
        let bad_buf = raw::read_raw(h, 0xFFFF_8000_0000_0000, 16) == Err(Errno::Fault);
        // A failed copy-out must not consume data.
        let mut first = [0u8; 5];
        let intact = raw::read(h, &mut first) == Ok(5) && &first == b"Hello";
        let ro_write = raw::write(h, b"x") == Err(Errno::BadHandle);
        let closed = raw::close(h).is_ok() && raw::close(h) == Err(Errno::BadHandle);
        let stale = raw::read(h, &mut first) == Err(Errno::BadHandle);
        let console = raw::close(1) == Err(Errno::BadHandle);
        bad_path && bad_buf && intact && ro_write && closed && stale && console
    });
    t.check("fs: per-process handle limit", || {
        let mut open = Vec::new();
        let err = loop {
            match File::open("/hello.txt") {
                Ok(f) => open.push(f),
                Err(e) => break e,
            }
            if open.len() > 1000 {
                return false;
            }
        };
        let n = open.len();
        open.clear();
        println!("opened {n} handles before {err}");
        n == 64 && err.raw_os_error() == Some(Errno::TooManyHandles) && File::open("/hello.txt").is_ok()
    });
    t.check("fs: 4 threads write their own files concurrently", || {
        let hs: Vec<_> = (0..4usize)
            .map(|k| {
                thread::spawn(move || {
                    let path = format!("/osyx-out/thread-{k}.bin");
                    let data: Vec<u8> = (0..40_000usize).map(|i| (i * (k + 3)) as u8).collect();
                    let w = File::create(&path).is_ok_and(|mut f| data.chunks(4096).all(|c| f.write_all(c).is_ok()));
                    w && fs::read(&path).is_ok_and(|b| b == data) && fs::remove_file(&path).is_ok()
                })
            })
            .collect();
        hs.into_iter().all(|h| h.join().unwrap_or(false))
    });
    t.check("fs: a child process writes, init reads", || {
        let ok = matches!(worker(&["fs-write", "/osyx-out/from-worker.txt"]).status(), Ok(s) if s.success());
        ok && fs::read_to_string("/osyx-out/from-worker.txt").is_ok_and(|s| s.starts_with("written by worker "))
    });
    t.check("fs: handles left open at exit are closed by the kernel", || {
        let ok = matches!(worker(&["fs-leak", "/osyx-out/leaked.txt"]).status(), Ok(s) if s.success());
        ok && fs::read_to_string("/osyx-out/leaked.txt").as_deref() == Ok("still here\n")
    });
    t.check("fs: final output for the host", || {
        File::create("/osyx-out/hello-from-osyx.txt")
            .is_ok_and(|mut f| f.write_all(OUT_TEXT.as_bytes()).is_ok() && f.sync_all().is_ok())
    });
}
