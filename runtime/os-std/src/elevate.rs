//! Asking for administrator rights: the client of the desktop's `elevate`
//! service (`userspace/desktop/src/elevate.rs`).
//!
//! A program that needs root sends one request naming the program to run.
//! The desktop asks the user ("User Account Control") and only on "Yes"
//! starts it as root. Windows programs (`.exe`) run under `/bin/winrun`,
//! anything else is executed directly.
//!
//! Request (one message, fields separated by newlines): `run`, `-v` or `-`,
//! the program's absolute path, the working directory, then the program's
//! arguments. Replies: `started` once the user agreed and the program runs,
//! then `exit <status>` when it ends; or `denied`, or `failed <reason>`.

use crate::ipc::{item, wait, Channel, MAX_MESSAGE, WAIT_READABLE};
use alloc::format;
use alloc::string::String;
use alloc::vec;

/// The service the desktop registers (root only, so a user cannot imitate it).
pub const SERVICE: &str = "elevate";

/// How an elevation request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The program ran as root and exited with this status.
    Exited(i32),
    /// The user said no (or closed the prompt).
    Denied,
    /// The request could not be made or the program could not start.
    Failed(String),
}

/// Asks for `program` (an absolute OSyx path) to run as root in `cwd` with
/// `args`, and waits until it ends. `started` is called once the user has
/// agreed and the program is running. `verbose` passes `-v` to winrun.
pub fn run(program: &str, cwd: &str, args: &[String], verbose: bool, mut started: impl FnMut()) -> Outcome {
    let mut request = format!("run\n{}\n{program}\n{cwd}", if verbose { "-v" } else { "-" });
    for arg in args {
        request.push('\n');
        request.push_str(arg);
    }
    if program.contains('\n') || cwd.contains('\n') || args.iter().any(|a| a.contains('\n')) || request.len() >= MAX_MESSAGE {
        return Outcome::Failed(String::from("the command line cannot be passed to the elevation service"));
    }
    let channel = match Channel::connect(SERVICE) {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("the elevation service is not running: {e}")),
    };
    if let Err(e) = channel.send(request.as_bytes(), &[]) {
        return Outcome::Failed(format!("cannot ask the elevation service: {e}"));
    }
    let mut reply = vec![0u8; MAX_MESSAGE];
    loop {
        // The user may take a while with the prompt: no time limit.
        let mut items = [item(channel.handle(), WAIT_READABLE)];
        let _ = wait(&mut items, None);
        match channel.recv(&mut reply) {
            Ok(Some((n, _))) => match parse(&String::from_utf8_lossy(&reply[..n])) {
                Some(outcome) => return outcome,
                None => started(),
            },
            Ok(None) => {}
            Err(_) => return Outcome::Failed(String::from("the elevation service closed the connection")),
        }
    }
}

/// The outcome a reply stands for; `None` for `started` (more follows).
fn parse(reply: &str) -> Option<Outcome> {
    let reply = reply.trim();
    if reply == "started" {
        return None;
    }
    if let Some(status) = reply.strip_prefix("exit ") {
        return Some(match status.trim().parse() {
            Ok(s) => Outcome::Exited(s),
            Err(_) => Outcome::Failed(format!("malformed reply: {reply}")),
        });
    }
    if reply == "denied" {
        return Some(Outcome::Denied);
    }
    Some(Outcome::Failed(String::from(reply.strip_prefix("failed ").unwrap_or(reply))))
}
