//! The desktop's `elevate` service: consent-based elevation for programs
//! that demand administrator rights (see `compatibility/win32/src/elevation.rs`
//! for the client and the message format).
//!
//! The desktop runs as root, a user process cannot. A user's `winrun` sends
//! one request naming the program; the shell asks the user ("User Account
//! Control") and only on "Yes" starts it as root. Declining, closing the
//! prompt or any failure ends the request with a reply, never with a
//! program running. The kernel names the requester (`Channel::peer`), so a
//! request cannot claim to come from someone else.

use os_std::ipc::{Channel, Listener, MAX_MESSAGE};
use os_std::prelude::*;
use os_std::process::Command;
use os_std::time::{Duration, Instant};
use os_std::collections::VecDeque;

/// The service name; only root may register it.
const SERVICE: &str = "elevate";
/// A connection that sends nothing is dropped after this long.
const REQUEST_WAIT: Duration = Duration::from_secs(10);
/// Requests waiting for their turn at the prompt; more are refused.
const MAX_QUEUE: usize = 4;

/// One validated request.
pub struct Request {
    pub channel: Channel,
    pub exe: String,
    pub cwd: String,
    pub verbose: bool,
    pub args: Vec<String>,
}

impl Request {
    /// The question put to the user.
    pub fn prompt(&self) -> String {
        format!("Do you want to allow this app to make changes to your computer?\n{}", self.exe)
    }

    /// Starts the program as root (this process's identity), working in the
    /// requester's directory and with the desktop user's profile.
    pub fn start(&self) -> Result<u64, String> {
        let mut command = Command::new("/bin/winrun");
        if self.verbose {
            command.arg("-v");
        }
        command.arg(&self.exe).args(&self.args);
        command.env("OSYX_WINRUN_CWD", &self.cwd).env("HOME", "/home/user").env("USER", "user");
        command.spawn().map(|child| child.id()).map_err(|e| format!("{e}"))
    }

    pub fn reply(&self, text: &str) {
        let _ = self.channel.send(text.as_bytes(), &[]);
    }
}

pub struct Service {
    listener: Option<Listener>,
    /// Connections that have not sent their request yet.
    incoming: Vec<(Channel, Instant)>,
    /// Requests waiting for the prompt.
    pub queue: VecDeque<Request>,
}

impl Service {
    pub fn start() -> Service {
        let listener = match Listener::listen(SERVICE) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("desktop: no elevation service: {e}");
                None
            }
        };
        Service { listener, incoming: Vec::new(), queue: VecDeque::new() }
    }

    /// Whether anything is in flight (the shell then polls more often).
    pub fn busy(&self) -> bool {
        !self.incoming.is_empty() || !self.queue.is_empty()
    }

    /// Takes new connections and the requests they carry.
    pub fn poll(&mut self) {
        let Some(listener) = &self.listener else { return };
        while let Ok(Some(channel)) = listener.accept() {
            self.incoming.push((channel, Instant::now()));
        }
        let mut waiting = Vec::new();
        for (channel, since) in core::mem::take(&mut self.incoming) {
            let mut buf = vec![0u8; MAX_MESSAGE];
            match channel.recv(&mut buf) {
                Ok(Some((n, _))) => self.accept(channel, &buf[..n]),
                Ok(None) if since.elapsed() < REQUEST_WAIT => waiting.push((channel, since)),
                _ => {} // silent, or gone: dropping the channel closes it
            }
        }
        self.incoming = waiting;
    }

    fn accept(&mut self, channel: Channel, message: &[u8]) {
        let refuse = |why: &str| {
            let _ = channel.send(format!("failed {why}").as_bytes(), &[]);
        };
        let Ok((_, uid)) = channel.peer() else { return refuse("unknown requester") };
        if uid == 0 {
            return refuse("root needs no elevation");
        }
        if self.queue.len() >= MAX_QUEUE {
            return refuse("too many elevation requests");
        }
        let Ok(text) = core::str::from_utf8(message) else { return refuse("malformed request") };
        match parse(text) {
            Ok((verbose, exe, cwd, args)) => {
                println!("desktop: elevation requested for {exe} by uid {uid}");
                self.queue.push_back(Request { channel, exe, cwd, verbose, args });
            }
            Err(why) => refuse(why),
        }
    }
}

/// `run`, verbosity, program, directory and arguments, one per line. The
/// program must be an absolute path of a `.exe` that exists.
fn parse(text: &str) -> Result<(bool, String, String, Vec<String>), &'static str> {
    let mut lines = text.split('\n');
    if lines.next() != Some("run") {
        return Err("unknown request");
    }
    let verbose = match lines.next() {
        Some("-v") => true,
        Some("-") => false,
        _ => return Err("malformed request"),
    };
    let (Some(exe), Some(cwd)) = (lines.next(), lines.next()) else { return Err("malformed request") };
    let lower = exe.to_ascii_lowercase();
    if !exe.starts_with('/') || !lower.ends_with(".exe") || exe.split('/').any(|part| part == "..") {
        return Err("not a program path");
    }
    if !cwd.starts_with('/') {
        return Err("not a directory path");
    }
    if !os_std::fs::metadata(exe).is_ok_and(|m| m.is_file()) {
        return Err("no such program");
    }
    Ok((verbose, String::from(exe), String::from(cwd), lines.map(String::from).collect()))
}
