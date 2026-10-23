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
                match File::open("/hello.txt") {
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
        other => {
            eprintln!("worker: unknown mode {other:?}");
            2
        }
    }
}
