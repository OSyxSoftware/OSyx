//! First OSyx user program. Runs as `\EFI\OSYX\INIT.ELF` in ring 3 and checks
//! OSyx Std plus the kernel's system call validation from the user side.
//!
//! Arguments (kernel command line `init.args=...`):
//! * none:          run the checks; exit status = number of failed checks
//! * `fault`:       dereference an unmapped address (the kernel must kill us)
//! * `panic`:       panic (OSyx Std must report it and exit with 101)
//! * `exit,<code>`: exit with `<code>`

#![no_std]
#![no_main]

use os_std::os::raw::{self, Errno, PROT_EXEC, PROT_READ, PROT_WRITE};
use os_std::prelude::*;
use os_std::time::Instant;
use os_std::{env, os, process};

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
            info.abi_version == 1 && info.page_size == 4096 && info.free_memory <= info.total_memory
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

    println!("user checks: {} passed, {} failed", t.passed, t.failed);
    t.failed as i32
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
