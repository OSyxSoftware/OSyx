//! Native web/HTML opener used by AXIS and Files.
#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::process::Command;
use os_std::{env, fs};

os_std::entry!(main);

fn main() -> i32 {
    let targets: Vec<String> = env::args().skip(1).collect();
    if targets.is_empty() {
        eprintln!("usage: open URL|FILE.html ...");
        return 2;
    }
    let browser = fs::read_to_string("/etc/osyx/default-browser").unwrap_or_default();
    let browser = browser.trim();
    if !browser.starts_with('/') {
        eprintln!("open: no default browser installed");
        return 1;
    }
    let mut command = Command::new(browser);
    for target in targets {
        if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with("file://")
            || target.starts_with("about:")
        {
            command.arg(&target);
        } else {
            let path = if target.starts_with('/') {
                target
            } else {
                match env::current_dir() {
                    Ok(cwd) => format!("{}/{target}", cwd.display()),
                    Err(e) => {
                        eprintln!("open: {e}");
                        return 1;
                    }
                }
            };
            if fs::metadata(&path).is_err() {
                eprintln!("open: file not found: {path}");
                return 1;
            }
            command.arg(&path);
        }
    }
    eprintln!("open: {}", command.exec());
    1
}
