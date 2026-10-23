//! Program loader probe (`/usr/bin/probe`), started by `init` and `worker`
//! to check what a program receives from `spawn` / `execve`.
//!
//! Modes (first argument):
//! * `report PATH ...` write `PATH`: one `key=value` line each for pid,
//!   current directory, program path, every argument, every environment
//!   variable, the `AT_RANDOM` bytes, `AT_UID`, whether the program headers
//!   (`AT_PHDR`) and entry point (`AT_ENTRY`) check out, and whether handle
//!   3 was open at start (it is written to if so); exit 0
//! * `count N`  exit 0 if exactly N more arguments follow, each `aI` with I
//!   its index padded by dots to 100 bytes; 1 otherwise
//! * `chain N`  `exec` itself with `chain N-1`; at 0 exit 42

#![no_std]
#![no_main]

use os_std::fs;
use os_std::os::{self, auxv, raw};
use os_std::prelude::*;
use os_std::process::{self, Command};
use os_std::env;

os_std::entry!(main);

extern "C" {
    /// The runtime's entry point (`os_std`).
    fn _start();
}

/// Checks `AT_PHDR` / `AT_PHNUM` against `AT_ENTRY`: some executable
/// `PT_LOAD` header must contain the entry point, which must be `_start`.
fn headers_ok() -> (&'static str, &'static str) {
    let entry = os::aux_value(auxv::AT_ENTRY);
    let entry_ok = entry == Some(_start as *const () as u64);
    let (Some(phdr), Some(phnum), Some(56)) =
        (os::aux_value(auxv::AT_PHDR), os::aux_value(auxv::AT_PHNUM), os::aux_value(auxv::AT_PHENT))
    else {
        return ("missing", if entry_ok { "ok" } else { "bad" });
    };
    let e = entry.unwrap_or(0);
    let covers = (0..phnum).any(|i| {
        // SAFETY: the kernel maps the program headers it reports in AT_PHDR.
        let h = unsafe { core::slice::from_raw_parts((phdr + i * 56) as *const u8, 56) };
        let word = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
        let (p_type, flags) = (u32::from_le_bytes(h[0..4].try_into().unwrap()), u32::from_le_bytes(h[4..8].try_into().unwrap()));
        p_type == 1 && flags & 1 != 0 && word(16) <= e && e < word(16) + word(40)
    });
    (if covers { "ok" } else { "bad" }, if entry_ok { "ok" } else { "bad" })
}

fn report(path: &str) -> i32 {
    // Before anything else opens a file: was handle 3 inherited (execve)?
    let handle3 = match raw::write(3, b"written by probe through an inherited handle\n") {
        Ok(_) => "ok",
        Err(_) => "none",
    };
    let mut out = String::new();
    let pid = process::id().unwrap_or(0);
    out += &format!("pid={pid}\n");
    out += &format!("cwd={}\n", env::current_dir().map(|p| String::from(p.as_str())).unwrap_or_default());
    out += &format!("exe={}\n", env::current_exe().map(|p| String::from(p.as_str())).unwrap_or_default());
    let args: Vec<String> = env::args().collect();
    out += &format!("argc={}\n", args.len());
    for a in &args {
        out += &format!("arg={a}\n");
    }
    for (k, v) in env::vars() {
        out += &format!("env={k}={v}\n");
    }
    let random = os::aux_value(auxv::AT_RANDOM).map(|p| {
        // SAFETY: AT_RANDOM points to 16 bytes on the start-up stack.
        let b = unsafe { core::slice::from_raw_parts(p as *const u8, 16) };
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    });
    out += &format!("random={}\n", random.unwrap_or_default());
    out += &format!("uid={}\n", os::aux_value(auxv::AT_UID).map_or(-1, |v| v as i64));
    let (phdr, entry) = headers_ok();
    out += &format!("phdr={phdr}\nentry={entry}\nhandle3={handle3}\n");
    println!("probe {pid}: argc {}, exe {:?}, phdr {phdr}, entry {entry}", args.len(), env::current_exe());
    match fs::write(path, &out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("probe {pid}: writing {path}: {e}");
            1
        }
    }
}

fn main() -> i32 {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("report") => match args.get(2) {
            Some(p) => report(p),
            None => 2,
        },
        Some("count") => {
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            let rest = &args[3.min(args.len())..];
            let ok = rest.len() == n && rest.iter().enumerate().all(|(i, a)| *a == count_arg(i));
            (!ok) as i32
        }
        Some("chain") => {
            let n: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            if n == 0 {
                println!("chain: done");
                return 42;
            }
            let e = Command::new("/usr/bin/probe").args(["chain", &format!("{}", n - 1)]).exec();
            eprintln!("chain {n}: exec failed: {e}");
            3
        }
        other => {
            eprintln!("probe: unknown mode {other:?}");
            2
        }
    }
}

/// Argument `i` of the `count` mode (`init` builds the same ones).
fn count_arg(i: usize) -> String {
    let mut s = format!("a{i}");
    while s.len() < 100 {
        s.push('.');
    }
    s
}
