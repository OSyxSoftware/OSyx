//! Installed Zen browser: the unmodified Windows payload, a persistent
//! per-user profile and the software rendering path supported by OSyx.
#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::process::Command;
use os_std::{env, fs, os};

os_std::entry!(main);

const PROFILE: &str = "/home/user/.config/zen/profile";
const PREFS: &str = r#"user_pref("gfx.webrender.software", true);
user_pref("gfx.webrender.software.d3d11", false);
user_pref("gfx.webrender.compositor", false);
user_pref("layers.acceleration.disabled", true);
user_pref("layers.gpu-process.enabled", false);
user_pref("layers.gpu-process.force-enabled", false);
user_pref("zen.welcome-screen.seen", true);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("startup.homepage_welcome_url", "");
user_pref("startup.homepage_welcome_url.additional", "");
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("dom.ipc.processPrelaunch.enabled", false);
"#;

fn main() -> i32 {
    if fs::metadata("/opt/zen/zen.exe").is_err() {
        eprintln!("zen: browser payload is missing; build with -ZenDirectory and -RootMiB 1024");
        return 1;
    }
    if os::getuid().0 == 0 {
        if let Err(e) = os::setuid(1000, 1000) {
            eprintln!("zen: cannot switch to desktop user: {e}");
            return 1;
        }
    }
    env::set_var("HOME", "/home/user");
    env::set_var("USER", "user");
    if let Err(e) = fs::create_dir_all(PROFILE).and_then(|_| fs::write(format!("{PROFILE}/user.js"), PREFS)) {
        eprintln!("zen: cannot prepare profile: {e}");
        return 1;
    }
    let mut command = Command::new("/bin/winrun");
    command.args(["/opt/zen/zen.exe", "-profile", "C:\\home\\user\\.config\\zen\\profile"]);
    // Pass URLs as arguments, never through a shell. Native files are encoded
    // as file URLs so spaces, '#' and non-ASCII names reach Gecko intact.
    for target in env::args().skip(1) {
        if target.starts_with('/') {
            command.arg(&file_url(&target));
        } else if target.starts_with("https://")
            || target.starts_with("http://")
            || target.starts_with("file://")
            || target.starts_with("about:")
        {
            command.arg(&target);
        } else {
            eprintln!("zen: expected an absolute path or web address: {target}");
            return 2;
        }
    }
    // Gecko's launcher flag must follow all URLs (its command-line handler
    // skips the argument immediately after this bootstrap-only flag).
    command.arg("-no-deelevate");
    // The Windows broker's remote-memory patching is not implemented yet.
    for kind in ["CONTENT", "GPU", "RDD", "SOCKET_PROCESS", "GMP", "UTILITY", "VR"] {
        command.env(&format!("MOZ_DISABLE_{kind}_SANDBOX"), "1");
    }
    println!("zen: launching installed browser with persistent profile {PROFILE}");
    eprintln!("zen: {}", command.exec());
    1
}

fn file_url(path: &str) -> String {
    let mut url = String::from("file:///C:");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}
