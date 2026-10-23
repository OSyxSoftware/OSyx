//! Processes: exit, and starting and waiting for child programs.
//!
//! Programs are the boot modules in `\EFI\OSYX\BIN\` (there is no filesystem
//! yet), started by file name:
//!
//! ```ignore
//! let status = Command::new("worker").arg("exit").arg("7").status()?;
//! assert_eq!(status.code(), Some(7));
//! ```

use crate::error::{Error, ErrorKind, Result};
use crate::sys;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// Terminates the process with `code`. Destructors of live values do not run.
pub fn exit(code: i32) -> ! {
    sys::exit(code)
}

/// Terminates the process abnormally (status 134, like SIGABRT on Unix).
pub fn abort() -> ! {
    sys::exit(134)
}

/// The kernel's identifier of the current process.
pub fn id() -> Result<u64> {
    Ok(sys::sysinfo()?.pid)
}

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    raw: i32,
}

impl ExitStatus {
    pub const fn from_raw(raw: i32) -> ExitStatus {
        ExitStatus { raw }
    }

    /// True if the process exited normally with status 0.
    pub fn success(&self) -> bool {
        self.raw == 0
    }

    /// The exit code, or `None` if the kernel terminated the process because
    /// of a CPU exception (see [`ExitStatus::exception`]).
    pub fn code(&self) -> Option<i32> {
        if self.exception().is_some() {
            None
        } else {
            Some(self.raw)
        }
    }

    /// The CPU exception vector that terminated the process, if any.
    pub fn exception(&self) -> Option<u8> {
        let base = osyx_abi::EXCEPTION_EXIT_BASE;
        if self.raw & !0xFF == base {
            Some((self.raw & 0xFF) as u8)
        } else {
            None
        }
    }

    pub fn raw(&self) -> i32 {
        self.raw
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.exception() {
            Some(v) => write!(f, "terminated by CPU exception {v}"),
            None => write!(f, "exit status {}", self.raw),
        }
    }
}

/// Builder for starting a child program.
#[derive(Clone, Debug)]
pub struct Command {
    program: String,
    args: Vec<String>,
}

impl Command {
    pub fn new(program: &str) -> Command {
        Command { program: program.to_string(), args: Vec::new() }
    }

    pub fn arg(&mut self, arg: &str) -> &mut Command {
        self.args.push(arg.to_string());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: AsRef<str>>(&mut self, args: I) -> &mut Command {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_string()));
        self
    }

    /// Starts the program; it runs concurrently with the caller.
    pub fn spawn(&self) -> Result<Child> {
        let block = encode_args(&self.args)?;
        Ok(Child { pid: sys::spawn(&self.program, &block)? })
    }

    /// Starts the program and waits for it to finish.
    pub fn status(&self) -> Result<ExitStatus> {
        self.spawn()?.wait()
    }
}

/// A running (or finished but not yet waited for) child process.
#[derive(Debug)]
pub struct Child {
    pid: u64,
}

impl Child {
    pub fn id(&self) -> u64 {
        self.pid
    }

    /// Blocks until the child exits and releases it.
    pub fn wait(self) -> Result<ExitStatus> {
        let (_, raw) = sys::wait(self.pid)?;
        Ok(ExitStatus::from_raw(raw))
    }
}

/// Waits for any child; returns its pid and status.
pub fn wait_any() -> Result<(u64, ExitStatus)> {
    let (pid, raw) = sys::wait(0)?;
    Ok((pid, ExitStatus::from_raw(raw)))
}

/// Encodes arguments as the kernel's `spawn` argument block.
pub fn encode_args<S: AsRef<str>>(args: &[S]) -> Result<Vec<u8>> {
    let mut block = Vec::new();
    for a in args {
        let a = a.as_ref();
        if a.as_bytes().contains(&0) {
            return Err(Error::new(ErrorKind::InvalidInput, "argument contains a NUL byte"));
        }
        block.extend_from_slice(a.as_bytes());
        block.push(0);
    }
    if block.len() > osyx_abi::MAX_SPAWN_ARGS {
        return Err(Error::new(ErrorKind::InvalidInput, "arguments too long"));
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn argument_block_encoding() {
        assert_eq!(encode_args::<&str>(&[]).unwrap(), b"");
        assert_eq!(encode_args(&["spin", "30"]).unwrap(), b"spin\x0030\x00");
        assert_eq!(encode_args(&["a\0b"]).unwrap_err().kind(), ErrorKind::InvalidInput);
        let long = "x".repeat(osyx_abi::MAX_SPAWN_ARGS);
        assert_eq!(encode_args(&[long]).unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn exit_status_classification() {
        assert!(ExitStatus::from_raw(0).success());
        assert_eq!(ExitStatus::from_raw(7).code(), Some(7));
        let fault = ExitStatus::from_raw(osyx_abi::EXCEPTION_EXIT_BASE | 14);
        assert_eq!(fault.code(), None);
        assert_eq!(fault.exception(), Some(14));
        assert!(!fault.success());
        assert_eq!(fault.to_string(), "terminated by CPU exception 14");
        assert_eq!(ExitStatus::from_raw(-1).exception(), None);
    }

    #[test]
    fn command_builder_and_host_backend() {
        let mut c = Command::new("worker");
        c.arg("exit").args(["7", "x"]);
        assert_eq!(c.args, ["exit", "7", "x"]);
        // The host back end cannot start OSyx programs and says so.
        assert_eq!(c.spawn().unwrap_err().kind(), ErrorKind::Unsupported);
        assert_eq!(wait_any().unwrap_err().kind(), ErrorKind::Unsupported);
    }
}
