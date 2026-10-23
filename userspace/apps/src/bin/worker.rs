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

#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::process::{self, Command};
use os_std::time::{Duration, Instant};
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
        other => {
            eprintln!("worker: unknown mode {other:?}");
            2
        }
    }
}
