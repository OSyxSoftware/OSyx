//! Launch user-editable desktop shortcuts with the desktop user's rights.
#![no_std]
#![no_main]
use os_std::prelude::*;
use os_std::process::Command;
use os_std::{env, os};
os_std::entry!(main);

fn main() -> i32 {
    let Some(program) = env::args().nth(1) else { return 2 };
    // The desktop user (uid 1000) in the administrators group (gid 27,
    // "sudo"), which may change the system settings.
    if os::getuid().0 == 0 && os::setuid(1000, 27).is_err() {
        return 1;
    }
    env::set_var("HOME", "/home/user");
    env::set_var("USER", "user");
    let _ = env::set_current_dir("/home/user");
    let pe = program.to_ascii_lowercase().ends_with(".exe");
    let mut command = Command::new(if pe { "/bin/winrun" } else { &program });
    if pe {
        command.arg(&program);
    }
    command.args(env::args().skip(2));
    eprintln!("run-app: {}", command.exec());
    1
}
