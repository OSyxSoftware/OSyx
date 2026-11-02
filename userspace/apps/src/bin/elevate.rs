//! `elevate PROGRAM [ARGS...]`: runs a program with administrator rights
//! after the user agrees in the desktop's "User Account Control" prompt
//! (see `os_std::elevate`). Exits with the program's status, 1223
//! (ERROR_CANCELLED) when the user says no, 740 (ERROR_ELEVATION_REQUIRED)
//! when the request cannot be made.
#![no_std]
#![no_main]

use os_std::elevate::Outcome;
use os_std::prelude::*;
use os_std::{env, fs};

os_std::entry!(main);

/// Where a bare program name is looked up.
const SEARCH: &[&str] = &["/bin", "/usr/bin", "/system/bin"];

fn main() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(program) = args.first() else {
        eprintln!("usage: elevate PROGRAM [ARGS...]");
        return 2;
    };
    let cwd = match env::current_dir() {
        Ok(c) => c.display().to_string(),
        Err(e) => {
            eprintln!("elevate: {e}");
            return 1;
        }
    };
    let path = if program.starts_with('/') {
        program.clone()
    } else if program.contains('/') {
        format!("{}/{program}", cwd.trim_end_matches('/'))
    } else {
        match SEARCH.iter().map(|d| format!("{d}/{program}")).find(|p| fs::metadata(p).is_ok_and(|m| m.is_file())) {
            Some(p) => p,
            None => {
                eprintln!("elevate: {program}: not found");
                return 2;
            }
        }
    };
    if os_std::os::getuid().0 == 0 {
        eprintln!("elevate: already running as root");
        return 1;
    }
    match os_std::elevate::run(&path, &cwd, &args[1..], false, || println!("elevate: {path} started")) {
        Outcome::Exited(status) => {
            println!("elevate: {path} exited with status {status}");
            status
        }
        Outcome::Denied => {
            eprintln!("elevate: the request was declined");
            1223
        }
        Outcome::Failed(why) => {
            eprintln!("elevate: {why}");
            740
        }
    }
}
