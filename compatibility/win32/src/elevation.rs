//! Elevation of programs that demand administrator rights (UAC).
//!
//! The desktop user runs without root, so a program whose manifest says
//! `requestedExecutionLevel level="requireAdministrator"` (an installer
//! writing to `C:\Program Files`, for one) cannot work as that user. As on
//! Windows the user is asked first: `winrun` hands the launch to the
//! `elevate` service of the desktop, which shows a consent prompt naming the
//! program and, on "Yes", starts it as root. The service reports the exit
//! status back, so the caller sees the program's own result.
//!
//! Request (one message, fields separated by newlines):
//! `run`, `-v` or `-`, the program's OSyx path, the working directory, then
//! the program's arguments. Replies: `exit <status>`, `denied` or
//! `failed <reason>`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use os_std::eprintln;
use os_std::ipc::{item, wait, Channel, MAX_MESSAGE, WAIT_READABLE};
use osyx_pe::{execution_level, PeFile};

/// The service the desktop registers (root only, so a user cannot imitate it).
pub(crate) const SERVICE: &str = "elevate";

/// ERROR_ELEVATION_REQUIRED: the program needs rights it was not given.
const ELEVATION_REQUIRED: i32 = 740;
/// ERROR_CANCELLED: the user declined the prompt.
const CANCELLED: i32 = 1223;

/// RT_MANIFEST, resource id 1: the manifest of an executable.
const RT_MANIFEST: u16 = 24;

/// Whether the program's manifest asks for administrator rights.
pub(crate) fn wanted(image: &[u8]) -> bool {
    let Ok(pe) = PeFile::parse_any(image) else { return false };
    let Ok(Some(manifest)) = pe.resource(RT_MANIFEST, Some(1), None) else { return false };
    execution_level(manifest).needs_elevation()
}

/// Runs `exe` elevated through the desktop's service and returns its exit
/// status (or the Windows error that stopped the request).
pub(crate) fn run(exe: &str, cwd: &str, verbose: bool, args: &[String]) -> i32 {
    let mut request = format!("run\n{}\n{exe}\n{cwd}", if verbose { "-v" } else { "-" });
    for arg in args {
        request.push('\n');
        request.push_str(arg);
    }
    if [exe, cwd].iter().chain(args.iter().map(String::as_str).collect::<alloc::vec::Vec<_>>().iter()).any(|f| f.contains('\n'))
        || request.len() >= MAX_MESSAGE
    {
        eprintln!("winrun: {exe} needs administrator rights, and its command line cannot be passed to the elevation service");
        return ELEVATION_REQUIRED;
    }
    let channel = match Channel::connect(SERVICE) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("winrun: {exe} needs administrator rights, but the elevation service is not running: {e}");
            return ELEVATION_REQUIRED;
        }
    };
    if let Err(e) = channel.send(request.as_bytes(), &[]) {
        eprintln!("winrun: cannot ask the elevation service: {e}");
        return ELEVATION_REQUIRED;
    }
    let mut reply = vec![0u8; MAX_MESSAGE];
    loop {
        // The user may take a while with the prompt: no time limit.
        let mut items = [item(channel.handle(), WAIT_READABLE)];
        let _ = wait(&mut items, None);
        match channel.recv(&mut reply) {
            Ok(Some((n, _))) => return interpret(exe, &String::from_utf8_lossy(&reply[..n])),
            Ok(None) => {}
            Err(_) => {
                eprintln!("winrun: the elevation service closed the connection");
                return ELEVATION_REQUIRED;
            }
        }
    }
}

/// The exit status a service reply stands for.
fn interpret(exe: &str, reply: &str) -> i32 {
    let reply = reply.trim();
    if let Some(status) = reply.strip_prefix("exit ") {
        return status.trim().parse().unwrap_or(ELEVATION_REQUIRED);
    }
    if reply == "denied" {
        eprintln!("winrun: elevation of {exe} was declined");
        return CANCELLED;
    }
    eprintln!("winrun: cannot elevate {exe}: {}", reply.strip_prefix("failed ").unwrap_or(reply));
    ELEVATION_REQUIRED
}
