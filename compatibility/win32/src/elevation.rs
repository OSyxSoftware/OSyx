//! Elevation of programs that demand administrator rights (UAC).
//!
//! The desktop user runs without root, so a program whose manifest says
//! `requestedExecutionLevel level="requireAdministrator"` (an installer
//! writing to `C:\Program Files`, for one) cannot work as that user. As on
//! Windows the user is asked first: `winrun` hands the launch to the
//! desktop's `elevate` service (through `os_std::elevate`), which shows a
//! consent prompt naming the program and, on "Yes", starts it as root. The
//! service reports the exit status back, so the caller sees the program's
//! own result.
//!
//! ShellExecute's "runas" verb starts such a winrun as a proxy and names a
//! service in `OSYX_ELEVATE_NOTIFY`; the proxy tells it `started` once the
//! user agreed, or `denied`, so ShellExecute can return the user's answer as
//! Windows does (see shell32.rs).

use alloc::string::String;
use os_std::elevate::Outcome;
use os_std::eprintln;
use os_std::ipc::Channel;
use osyx_pe::{execution_level, PeFile};

/// The variable naming the service a "runas" caller listens on.
pub(crate) const NOTIFY_ENV: &str = "OSYX_ELEVATE_NOTIFY";

/// ERROR_ELEVATION_REQUIRED: the program needs rights it was not given.
const ELEVATION_REQUIRED: i32 = 740;
/// ERROR_CANCELLED: the user declined the prompt.
pub(crate) const CANCELLED: i32 = 1223;

/// RT_MANIFEST, resource id 1: the manifest of an executable.
const RT_MANIFEST: u16 = 24;

/// Whether the program's manifest asks for administrator rights.
pub(crate) fn wanted(image: &[u8]) -> bool {
    let Ok(pe) = PeFile::parse_any(image) else { return false };
    let Ok(Some(manifest)) = pe.resource(RT_MANIFEST, Some(1), None) else { return false };
    execution_level(manifest).needs_elevation()
}

/// Tells a waiting "runas" caller how the request went (if one waits).
fn notify(what: &str) {
    let Ok(service) = os_std::env::var(NOTIFY_ENV) else { return };
    if let Ok(c) = Channel::connect(&service) {
        let _ = c.send(what.as_bytes(), &[]);
    }
}

/// Runs `exe` elevated through the desktop's service and returns its exit
/// status (or the Windows error that stopped the request).
pub(crate) fn run(exe: &str, cwd: &str, verbose: bool, args: &[String]) -> i32 {
    match os_std::elevate::run(exe, cwd, args, verbose, || notify("started")) {
        Outcome::Exited(status) => status,
        Outcome::Denied => {
            eprintln!("winrun: elevation of {exe} was declined");
            notify("denied");
            CANCELLED
        }
        Outcome::Failed(why) => {
            eprintln!("winrun: {exe} needs administrator rights and cannot get them: {why}");
            notify("failed");
            ELEVATION_REQUIRED
        }
    }
}
