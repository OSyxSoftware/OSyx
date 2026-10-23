//! Processes: exit, starting and waiting for child programs, and replacing
//! the running program.
//!
//! Programs are static ELF64 files. A name without `/` is looked up by the
//! kernel in `/bin`, then `/usr/bin`; a path with `/` is used as it is
//! (relative to the current directory). The file needs execute permission.
//!
//! ```ignore
//! let status = Command::new("worker").arg("exit").arg("7").env("MODE", "test").status()?;
//! assert_eq!(status.code(), Some(7));
//! ```

use crate::error::{Error, ErrorKind, Result};
use crate::sys;
use alloc::collections::BTreeMap;
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

/// Builder for starting a child program, or for replacing the running one
/// ([`Command::exec`]). The child gets the current environment
/// ([`crate::env::vars`]) with the changes made through this builder.
#[derive(Clone, Debug)]
pub struct Command {
    program: String,
    arg0: Option<String>,
    args: Vec<String>,
    env_clear: bool,
    /// `Some(value)` sets, `None` removes.
    env_changes: BTreeMap<String, Option<String>>,
}

impl Command {
    pub fn new(program: &str) -> Command {
        Command { program: program.to_string(), arg0: None, args: Vec::new(), env_clear: false, env_changes: BTreeMap::new() }
    }

    pub fn arg(&mut self, arg: &str) -> &mut Command {
        self.args.push(arg.to_string());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: AsRef<str>>(&mut self, args: I) -> &mut Command {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_string()));
        self
    }

    /// `argv[0]` of the program (default: the program as given to [`Command::new`]).
    pub fn arg0(&mut self, arg0: &str) -> &mut Command {
        self.arg0 = Some(arg0.to_string());
        self
    }

    pub fn env(&mut self, key: &str, value: &str) -> &mut Command {
        self.env_changes.insert(key.to_string(), Some(value.to_string()));
        self
    }

    pub fn envs<I: IntoIterator<Item = (K, V)>, K: AsRef<str>, V: AsRef<str>>(&mut self, vars: I) -> &mut Command {
        for (k, v) in vars {
            self.env(k.as_ref(), v.as_ref());
        }
        self
    }

    pub fn env_remove(&mut self, key: &str) -> &mut Command {
        self.env_changes.insert(key.to_string(), None);
        self
    }

    /// Starts from an empty environment: drops the inherited variables and
    /// earlier [`Command::env`] calls (later ones still apply).
    pub fn env_clear(&mut self) -> &mut Command {
        self.env_clear = true;
        self.env_changes.clear();
        self
    }

    pub fn get_program(&self) -> &str {
        &self.program
    }

    pub fn get_args(&self) -> impl Iterator<Item = &str> {
        self.args.iter().map(String::as_str)
    }

    /// The environment the program will get, as `KEY=value` strings.
    pub fn get_envs(&self) -> Vec<String> {
        let mut env: Vec<(String, String)> = if self.env_clear { Vec::new() } else { crate::env::vars().collect() };
        for (k, change) in &self.env_changes {
            env.retain(|(key, _)| key != k);
            if let Some(v) = change {
                env.push((k.clone(), v.clone()));
            }
        }
        env.into_iter().map(|(k, v)| alloc::format!("{k}={v}")).collect()
    }

    /// The `argv` and `envp` blocks for the kernel.
    fn blocks(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        let arg0 = self.arg0.as_deref().unwrap_or(&self.program);
        let argv = encode_args(&core::iter::once(arg0).chain(self.get_args()).collect::<Vec<_>>())?;
        if self.env_changes.keys().any(|k| k.is_empty() || k.contains('=')) {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid environment variable name"));
        }
        let envp = encode_args(&self.get_envs())?;
        if argv.len() + envp.len() > osyx_abi::MAX_EXEC_ARGS {
            return Err(Error::new(ErrorKind::InvalidInput, "arguments and environment too long"));
        }
        Ok((argv, envp))
    }

    /// Starts the program; it runs concurrently with the caller.
    pub fn spawn(&self) -> Result<Child> {
        let (argv, envp) = self.blocks()?;
        Ok(Child { pid: sys::spawn(&self.program, &argv, &envp)? })
    }

    /// Starts the program and waits for it to finish.
    pub fn status(&self) -> Result<ExitStatus> {
        self.spawn()?.wait()
    }

    /// Replaces the running program with this one, keeping the process id,
    /// open files, credentials and current directory. Returns only if that
    /// failed (the current program then simply continues). The process must
    /// have only one thread (`ResourceBusy` otherwise).
    pub fn exec(&self) -> Error {
        match self.blocks() {
            Ok((argv, envp)) => sys::execve(&self.program, &argv, &envp),
            Err(e) => e,
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
impl Command {
    /// Starts the program with its standard input connected to a pipe the
    /// caller writes, and its standard output and error to one pipe the
    /// caller reads (end of data once the child and its descendants have
    /// closed it, normally by exiting).
    pub fn spawn_piped(&self) -> Result<(Child, crate::ipc::PipeWriter, crate::ipc::PipeReader)> {
        let (argv, envp) = self.blocks()?;
        let (in_r, in_w) = crate::ipc::pipe()?;
        let (out_r, out_w) = crate::ipc::pipe()?;
        let stdio = [in_r.handle(), out_w.handle(), out_w.handle()];
        let pid = os_sys::spawn_stdio(&self.program, &argv, &envp, stdio).map_err(Error::from)?;
        // The child holds its own references; ours would keep the pipes open.
        drop(in_r);
        drop(out_w);
        Ok((Child { pid }, in_w, out_r))
    }

    /// Starts the program with handles 0, 1 and 2 set to the given handles
    /// of this process (pipe ends or open files; `os::raw::HANDLE_NONE`
    /// leaves one unset, so the child's writes go to the kernel log). A shell
    /// passes its own redirected streams this way.
    pub fn spawn_stdio(&self, stdio: [u64; 3]) -> Result<Child> {
        let (argv, envp) = self.blocks()?;
        let pid = os_sys::spawn_stdio(&self.program, &argv, &envp, stdio).map_err(Error::from)?;
        Ok(Child { pid })
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

/// Encodes strings as a `spawn` / `execve` block (NUL-terminated strings
/// back to back).
pub fn encode_args<S: AsRef<str>>(args: &[S]) -> Result<Vec<u8>> {
    if args.len() > osyx_abi::MAX_EXEC_STRINGS {
        return Err(Error::new(ErrorKind::InvalidInput, "too many arguments"));
    }
    let mut block = Vec::new();
    for a in args {
        let a = a.as_ref();
        if a.as_bytes().contains(&0) {
            return Err(Error::new(ErrorKind::InvalidInput, "argument contains a NUL byte"));
        }
        block.extend_from_slice(a.as_bytes());
        block.push(0);
    }
    if block.len() > osyx_abi::MAX_EXEC_ARGS {
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
        let long = "x".repeat(osyx_abi::MAX_EXEC_ARGS);
        assert_eq!(encode_args(&[long]).unwrap_err().kind(), ErrorKind::InvalidInput);
        let many = alloc::vec![""; osyx_abi::MAX_EXEC_STRINGS + 1];
        assert_eq!(encode_args(&many).unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn command_environment_and_blocks() {
        crate::env::set_var("OSYX_CMD_INHERITED", "yes");
        crate::env::set_var("OSYX_CMD_REMOVED", "gone");
        let mut c = Command::new("/usr/bin/probe");
        c.arg("x").env("OSYX_CMD_SET", "a=b").env_remove("OSYX_CMD_REMOVED");
        let envs = c.get_envs();
        assert!(envs.contains(&"OSYX_CMD_INHERITED=yes".to_string()));
        assert!(envs.contains(&"OSYX_CMD_SET=a=b".to_string()));
        assert!(!envs.iter().any(|e| e.starts_with("OSYX_CMD_REMOVED=")));
        let (argv, envp) = c.blocks().unwrap();
        assert_eq!(argv, b"/usr/bin/probe\x00x\x00");
        assert!(envp.ends_with(b"\x00") && envp.windows(17).any(|w| w == b"OSYX_CMD_SET=a=b\x00"));
        c.arg0("probe").env_clear().env("ONLY", "1");
        assert_eq!(c.get_envs(), ["ONLY=1"]);
        assert_eq!(c.blocks().unwrap(), (b"probe\x00x\x00".to_vec(), b"ONLY=1\x00".to_vec()));
        assert_eq!(Command::new("p").env("A=B", "x").spawn().unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(Command::new("p").exec().kind(), ErrorKind::Unsupported);
        crate::env::remove_var("OSYX_CMD_INHERITED");
        crate::env::remove_var("OSYX_CMD_REMOVED");
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
