//! `winrun` — runs Windows x64 (PE32+) programs on OSyx.
//!
//! `winrun [-v] program.exe [arguments...]` loads the program and the DLLs
//! it needs through the OSyx PE loader (`osyx-pe-loader`), with OSyx's own
//! implementations of ntdll, kernel32/kernelbase, user32, gdi32 and ws2_32
//! (no Windows code), sets up the PEB, the main thread's TEB (GS), TLS and
//! a Windows-sized stack, runs DLL initialisation and calls the entry
//! point. The program's exit code is winrun's exit code. A fault in the
//! program ends only this process.

#![no_std]
#![no_main]

extern crate alloc;

/// Export table of a built-in DLL: `[(name, address)]` from function names.
macro_rules! export_table {
    ($($f:ident),* $(,)?) => {
        alloc::vec![$((stringify!($f), $f as *const () as usize as u64)),*]
    };
}

mod error;
mod iphlpapi;
mod kernel32;
mod loader;
mod mem;
mod ntdll;
mod objects;
mod path;
mod setupapi;
mod security;
mod teb;
mod user32;
mod volumes;
mod version;
mod synch;
mod psapi;
mod timers;
mod ws2_32;

use alloc::string::String;
use alloc::vec::Vec;
use os_std::prelude::*;

os_std::entry!(main);

/// Quotes an argument the way the Windows command-line parser expects.
fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return String::from(a);
    }
    let mut s = String::from("\"");
    s.push_str(&a.replace('"', "\\\""));
    s.push('"');
    s
}

fn main() -> i32 {
    let mut args: Vec<String> = os_std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--exports") {
        loader::print_capabilities();
        return 0;
    }
    let verbose = args.first().is_some_and(|a| a == "-v");
    if verbose {
        args.remove(0);
    }
    let Some(program) = args.first().cloned() else {
        eprintln!("usage: winrun [-v] program.exe [arguments...]");
        return 2;
    };
    // The program may be named with an OSyx path or a Windows path.
    let cwd = os_std::env::current_dir().map(|p| path::to_windows(p.as_str())).unwrap_or_else(|_| String::from("C:\\"));
    let osyx = if program.starts_with('/') {
        program.clone()
    } else {
        path::to_osyx(&program, &cwd).unwrap_or(program.clone())
    };
    let exe = match path::resolve(&osyx) {
        Ok((p, true)) => p,
        _ => {
            eprintln!("winrun: {program}: no such file");
            return 2;
        }
    };
    let win_exe = path::to_windows(&exe);
    let mut cmd = quote(&win_exe);
    for a in &args[1..] {
        cmd.push(' ');
        cmd.push_str(&quote(a));
    }
    let mut env: Vec<(String, String)> = os_std::env::vars().collect();
    for (k, v) in [
        ("OS", "Windows_NT"),
        ("OSYX", "1"),
        ("SystemDrive", "C:"),
        ("TEMP", "C:\\tmp"),
        ("TMP", "C:\\tmp"),
        ("PROCESSOR_ARCHITECTURE", "AMD64"),
    ] {
        if !env.iter().any(|(e, _)| e.eq_ignore_ascii_case(k)) {
            env.push((String::from(k), String::from(v)));
        }
    }
    let env_strings: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    kernel32::init_process(&exe, &cmd, env);
    let started = match loader::start(&exe, &cmd, &env_strings) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("winrun: {e}");
            return 126;
        }
    };
    if verbose {
        for (name, base, builtin, relocs, imports) in loader::summary() {
            println!(
                "winrun: {name:<16} {base:#014x} {} {relocs} relocations, {imports} imports bound",
                if builtin { "built-in" } else { "PE image" }
            );
        }
        println!("winrun: entry {:#x}, stack {:#x}..{:#x}", started.entry, started.stack_limit, started.stack_top);
    }
    if started.entry == 0 {
        eprintln!("winrun: {win_exe} has no entry point");
        return 126;
    }
    // The process entry gets the PEB in RCX, as from RtlUserThreadStart.
    let code = loader::run_on_stack(started.entry, started.peb, started.stack_top) as u32;
    kernel32::ExitProcess(code)
}
