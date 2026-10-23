//! Child program used by `init` to exercise processes and the scheduler.
//!
//! Modes (first argument):
//! * `exit N`      exit with status N
//! * `spin MS`     busy-loop for MS milliseconds without ever yielding
//! * `fpu SEED MS` keep a pattern in xmm0-xmm15 while spinning MS ms; exit 0
//!   if the registers survived preemption, 1 otherwise
//! * `sleep MS`    sleep; exit 0 if at least MS ms passed
//! * `fault`       read an unmapped address (the kernel terminates us)
//! * `child N`     spawn `worker exit N`, wait, exit with its status
//! * `orphan`      spawn `worker sleep 100` and exit without waiting
//! * `threads-exit` return 42 from main while 4 threads are blocked/spinning
//! * `thread-last` main thread exits first (5); the last thread exits with 9
//! * `thread-fault` / `thread-panic`: a spawned thread faults / panics
//! * `fs-write PATH` write "written by worker <pid>" to PATH
//! * `fs-leak PATH`  write PATH, then exit with 11 handles still open
//! * `pwd DIR`       exit 0 if the inherited current directory is DIR
//! * `hold-orphan P` create P (1 MiB), remove it, keep it open, sleep
//! * `as-user UID`   drop root, then check that permissions confine us;
//!   exit status = number of failed checks

#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::process::{self, Command};
use os_std::sync::atomic::AtomicU32;
use os_std::time::{Duration, Instant};
use os_std::fs::{self, File};
use os_std::{env, thread};

#[path = "../fpu.rs"]
mod fpu;

os_std::entry!(main);

fn arg_num(args: &[String], i: usize) -> u64 {
    args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn main() -> i32 {
    let args: Vec<String> = env::args().collect();
    let pid = process::id().unwrap_or(0);
    match args.get(1).map(String::as_str) {
        Some("exit") => arg_num(&args, 2) as i32,
        Some("spin") => {
            let ms = arg_num(&args, 2);
            let start = Instant::now();
            println!("spin {pid}: started at {} ms", start.as_nanos_since_boot() / 1_000_000);
            let mut rounds = 0u64;
            while start.elapsed() < Duration::from_millis(ms) {
                rounds = rounds.wrapping_add(1);
            }
            println!(
                "spin {pid}: finished at {} ms after {rounds} rounds",
                Instant::now().as_nanos_since_boot() / 1_000_000
            );
            0
        }
        Some("fpu") => {
            let seed = arg_num(&args, 2) << 32;
            let ms = arg_num(&args, 3);
            let spin = || {
                let start = Instant::now();
                while start.elapsed() < Duration::from_millis(ms) {
                    core::hint::spin_loop();
                }
            };
            if fpu::preserved_across(seed, spin) {
                0
            } else {
                eprintln!("fpu {pid}: XMM registers were corrupted");
                1
            }
        }
        Some("sleep") => {
            let ms = arg_num(&args, 2);
            let start = Instant::now();
            thread::sleep(Duration::from_millis(ms));
            (start.elapsed() < Duration::from_millis(ms)) as i32
        }
        Some("fault") => {
            // SAFETY: deliberately invalid; the kernel terminates the process.
            let v = unsafe { core::ptr::read_volatile(0x1000 as *const u64) };
            v as i32
        }
        Some("child") => match Command::new("worker").arg("exit").arg(&args[2]).status() {
            Ok(s) => s.raw(),
            Err(e) => {
                eprintln!("child {pid}: {e}");
                100
            }
        },
        Some("orphan") => match Command::new("worker").args(["sleep", "100"]).spawn() {
            Ok(c) => {
                println!("orphan {pid}: leaving child {} behind", c.id());
                0
            }
            Err(_) => 1,
        },
        Some("threads-exit") => {
            // Leave threads blocked in every way possible, then return from
            // main: the process must end with 42 instead of hanging.
            static NEVER: AtomicU32 = AtomicU32::new(0);
            thread::spawn(|| os_std::os::raw::futex_wait(NEVER.as_ptr(), 0, 0));
            thread::spawn(|| thread::sleep(Duration::from_secs(3600)));
            thread::spawn(|| loop {
                core::hint::spin_loop();
            });
            let pair = Arc::new((Mutex::new(false), Condvar::new()));
            let p2 = pair.clone();
            thread::spawn(move || {
                let (m, cv) = &*p2;
                drop(cv.wait_while(m.lock().unwrap(), |go| !*go));
            });
            thread::sleep(Duration::from_millis(50));
            println!("threads-exit {pid}: returning with 4 threads still blocked");
            42
        }
        Some("thread-last") => {
            // The main thread ends first; the process lives on until its last
            // thread exits, whose code becomes the exit status.
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(50));
                println!("thread-last: second thread finishing");
                os_std::os::raw::thread_exit(9, core::ptr::null())
            });
            os_std::os::raw::thread_exit(5, core::ptr::null())
        }
        Some("thread-fault") => {
            let h = thread::spawn(|| {
                // SAFETY: deliberately invalid; the kernel ends the process.
                unsafe { core::ptr::read_volatile(0x1000 as *const u64) }
            });
            let _ = h.join();
            3
        }
        Some("thread-panic") => {
            let h = thread::spawn(|| -> u32 { panic!("panic in a spawned thread") });
            let _ = h.join();
            3
        }
        Some("fs-write") => {
            let text = format!("written by worker {pid}\n");
            match args.get(2).map(|p| fs::write(p, &text)) {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    eprintln!("fs-write {pid}: {e}");
                    1
                }
                None => 2,
            }
        }
        Some("fs-leak") => {
            // Write a file, then exit without running destructors: the open
            // handles must be closed by the kernel, and the data kept.
            let Some(path) = args.get(2) else { return 2 };
            let mut files = Vec::new();
            for _ in 0..10 {
                match File::open("/dev/null") {
                    Ok(f) => files.push(f),
                    Err(_) => return 1,
                }
            }
            let Ok(mut f) = File::create(path) else { return 1 };
            if f.write_all(b"still here\n").is_err() {
                return 1;
            }
            files.push(f);
            core::mem::forget(files);
            process::exit(0)
        }
        Some("pwd") => {
            let cwd = env::current_dir();
            println!("pwd {pid}: {cwd:?}");
            (cwd.ok().as_deref().map(|p| p.as_str()) != args.get(2).map(String::as_str)) as i32
        }
        Some("as-user") => as_user(arg_num(&args, 2) as u32),
        Some("hold-orphan") => {
            // Keeps a removed file open until the machine stops.
            let Some(path) = args.get(2) else { return 2 };
            let Ok(mut f) = File::create(path) else { return 1 };
            if f.write_all(&vec![0x0Fu8; 1 << 20]).is_err() || f.sync_all().is_err() || fs::remove_file(path).is_err() {
                return 1;
            }
            println!("hold-orphan {pid}: holding removed {path} open");
            thread::sleep(Duration::from_secs(3600));
            drop(f);
            0
        }
        other => {
            eprintln!("worker: unknown mode {other:?}");
            2
        }
    }
}

/// Permission checks as an ordinary user (`init` prepares `/tmp/root-only`,
/// mode 0600, owned by root).
fn as_user(uid: u32) -> i32 {
    use os_std::error::ErrorKind;
    use os_std::os;
    if os::setuid(uid, uid).is_err() || os::getuid() != (uid, uid) {
        eprintln!("as-user: setuid failed");
        return 100;
    }
    let kind = |r: os_std::error::Result<()>| r.err().map(|e| e.kind());
    let checks: [(&str, bool); 12] = [
        ("ext4: 0600 root file unreadable", kind(File::open("/osyx-seed/secret").map(|_| ())) == Some(ErrorKind::PermissionDenied)),
        ("ext4: /etc is read-only for users", kind(fs::write("/etc/user.txt", "x")) == Some(ErrorKind::PermissionDenied)),
        ("ext4: own home directory is writable", fs::write("/home/user/note.txt", "mine").is_ok()
            && fs::metadata("/home/user/note.txt").is_ok_and(|m| m.uid() == uid && m.gid() == uid)
            && fs::remove_file("/home/user/note.txt").is_ok()),
        ("cannot become root again", kind(os::setuid(0, 0)) == Some(ErrorKind::PermissionDenied)),
        ("raw disks are root-only", fs::read_dir("/dev/disks").is_ok_and(|d| {
            d.into_iter().all(|e| File::open(e.path()).err().map(|e| e.kind()) == Some(ErrorKind::PermissionDenied))
        })),
        ("0600 root file unreadable", kind(File::open("/tmp/root-only").map(|_| ())) == Some(ErrorKind::PermissionDenied)),
        ("/boot is read-only for users", kind(fs::write("/boot/user.txt", "x")) == Some(ErrorKind::PermissionDenied)),
        ("/run is root's", kind(fs::write("/run/user.txt", "x")) == Some(ErrorKind::PermissionDenied)),
        ("/tmp is writable (sticky)", fs::write("/tmp/user-file", "mine").is_ok()
            && fs::metadata("/tmp/user-file").is_ok_and(|m| m.uid() == uid)),
        ("others' files in /tmp are protected", kind(fs::remove_file("/tmp/root-only")) == Some(ErrorKind::PermissionDenied)),
        ("mount is root-only", kind(os::mount("", "/tmp", "tmpfs", false, 0)) == Some(ErrorKind::PermissionDenied)),
        ("chmod of root's file refused", kind(fs::set_permissions("/tmp/root-only", fs::Permissions::from_mode(0o666))) == Some(ErrorKind::PermissionDenied)),
    ];
    let mut failed = 0;
    for (name, ok) in checks {
        if !ok {
            eprintln!("as-user {uid}: {name} ... FAILED");
            failed += 1;
        }
    }
    println!("as-user {uid}: {} of {} permission checks passed", 12 - failed, 12);
    failed
}