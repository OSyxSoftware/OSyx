//! Program loader checks run by `init`: `spawn` and `execve` of ELF files
//! from the VFS, the `/bin` + `/usr/bin` search, `argv`, the environment,
//! the auxiliary vector and the refusals. `/usr/bin/probe` reports what it
//! received (see `bin/probe.rs`).

use super::{worker, Tests};
use os_std::error::ErrorKind;
use os_std::fs::{self, Permissions};
use os_std::os::raw::{self, nr, Errno, ExecArgs};
use os_std::os::{self, auxv};
use os_std::prelude::*;
use os_std::process::Command;
use os_std::env;

const REPORT: &str = "/tmp/probe-report.txt";

/// A probe report: `key=value` lines in order.
struct Report(Vec<(String, String)>);

impl Report {
    fn read(path: &str) -> Option<Report> {
        let text = fs::read_to_string(path).ok()?;
        let lines = text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (String::from(k), String::from(v)));
        Some(Report(lines.collect()))
    }

    fn get(&self, key: &str) -> &str {
        self.0.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
    }

    fn all(&self, key: &str) -> Vec<&str> {
        self.0.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
    }
}

/// Runs `probe report REPORT extra...` built by `setup`; returns the report.
fn probe_report(setup: impl FnOnce(&mut Command)) -> Option<Report> {
    let _ = fs::remove_file(REPORT);
    let mut c = Command::new("probe");
    c.args(["report", REPORT]);
    setup(&mut c);
    match c.status() {
        Ok(s) if s.success() => Report::read(REPORT),
        other => {
            println!("probe failed: {other:?}");
            None
        }
    }
}

/// Same as `count_arg` in `bin/probe.rs`.
fn count_arg(i: usize) -> String {
    let mut s = format!("a{i}");
    while s.len() < 100 {
        s.push('.');
    }
    s
}

extern "C" {
    fn _start();
}

pub fn run(t: &mut Tests) {
    t.check("exec: init itself came from /bin/init with an environment", || {
        let exe = env::current_exe().map(|p| String::from(p.as_str()));
        let path = env::var("PATH");
        println!("init: exe {exe:?}, PATH {path:?}, {} variables", env::vars().count());
        exe.as_deref() == Ok("/bin/init") && path.as_deref() == Ok("/bin:/usr/bin") && env::var("HOME").as_deref() == Ok("/")
    });
    t.check("exec: auxiliary vector (page size, entry, headers, ids, random)", || {
        let entry = os::aux_value(auxv::AT_ENTRY) == Some(_start as *const () as u64);
        let page = os::aux_value(auxv::AT_PAGESZ) == Some(4096);
        let ids = [auxv::AT_UID, auxv::AT_EUID, auxv::AT_GID, auxv::AT_EGID].iter().all(|&k| os::aux_value(k) == Some(0));
        let secure = os::aux_value(auxv::AT_SECURE) == Some(0);
        let phdr = os::aux_value(auxv::AT_PHDR).is_some_and(|p| p == 0x40_0040) && os::aux_value(auxv::AT_PHNUM) >= Some(3);
        let random = os::aux_value(auxv::AT_RANDOM).is_some_and(|p| {
            // SAFETY: AT_RANDOM points to 16 bytes on the start-up stack.
            unsafe { core::slice::from_raw_parts(p as *const u8, 16) }.iter().any(|&b| b != 0)
        });
        println!("auxv: entry {entry} page {page} ids {ids} secure {secure} phdr {phdr} random {random}");
        entry && page && ids && secure && phdr && random
    });
    t.check("exec: spawn finds probe in /usr/bin; exact argv and envp", || {
        let Some(r) = probe_report(|c| {
            c.arg0("renamed-probe").args(["two words", "", "zażółć"]).env_clear().env("OSYX_A", "1").env("EMPTY", "");
        }) else {
            return false;
        };
        let args = r.all("arg");
        let envs = r.all("env");
        println!("probe argv {args:?} envp {envs:?}");
        args == ["renamed-probe", "report", REPORT, "two words", "", "zażółć"]
            && envs == ["EMPTY=", "OSYX_A=1"]
            && r.get("exe") == "/usr/bin/probe"
            && r.get("phdr") == "ok"
            && r.get("entry") == "ok"
            && r.get("uid") == "0"
            && r.get("handle3") == "none"
            && r.get("cwd") == "/"
    });
    t.check("exec: children inherit the environment and set_var changes", || {
        env::set_var("OSYX_INHERIT", "from-init");
        let r = probe_report(|c| {
            c.env_remove("HOME");
        });
        env::remove_var("OSYX_INHERIT");
        r.is_some_and(|r| {
            let envs = r.all("env");
            envs.contains(&"OSYX_INHERIT=from-init") && envs.contains(&"PATH=/bin:/usr/bin") && !envs.iter().any(|e| e.starts_with("HOME="))
        })
    });
    t.check("exec: AT_RANDOM differs between processes", || {
        let a = probe_report(|_| {}).map(|r| String::from(r.get("random")));
        let b = probe_report(|_| {}).map(|r| String::from(r.get("random")));
        matches!((a, b), (Some(a), Some(b)) if a.len() == 32 && a != b)
    });
    t.check("exec: absolute and relative program paths", || {
        let abs = matches!(Command::new("/usr/bin/probe").args(["count", "0"]).status(), Ok(s) if s.success());
        let rel = env::set_current_dir("/usr")
            .and_then(|_| Command::new("./bin/probe").args(["count", "0"]).status())
            .is_ok_and(|s| s.success());
        let back = env::set_current_dir("/").is_ok();
        abs && rel && back
    });
    t.check("exec: 1000 arguments of 100 bytes", || {
        let args: Vec<String> = (0..1000).map(count_arg).collect();
        let mut c = Command::new("probe");
        c.args(["count", "1000"]).args(&args);
        let ok = matches!(c.status(), Ok(s) if s.success());
        let too_long: Vec<String> = (0..1400).map(count_arg).collect();
        let refused = Command::new("probe").args(&too_long).spawn().map(|c| c.id()).map_err(|e| e.kind()) == Err(ErrorKind::InvalidInput);
        ok && refused
    });
    t.check("exec: execute permission, non-ELF files, directories", || {
        let Ok(image) = fs::read("/usr/bin/probe") else { return false };
        let copied = fs::write("/tmp/probe-copy", &image).is_ok();
        let kind = |p: &str| Command::new(p).args(["count", "0"]).status().map(|s| s.code()).map_err(|e| e.kind());
        let no_x = kind("/tmp/probe-copy") == Err(ErrorKind::PermissionDenied);
        let made_x = fs::set_permissions("/tmp/probe-copy", Permissions::from_mode(0o755)).is_ok();
        let runs_from_tmpfs = kind("/tmp/probe-copy") == Ok(Some(0));
        let script = fs::write("/tmp/exec-script", "#!/bin/sh\necho no\n").is_ok()
            && fs::set_permissions("/tmp/exec-script", Permissions::from_mode(0o755)).is_ok();
        let not_elf = kind("/tmp/exec-script") == Err(ErrorKind::InvalidExecutable);
        let dir = kind("/tmp") == Err(ErrorKind::PermissionDenied);
        let _ = fs::remove_file("/tmp/probe-copy");
        println!("exec perms: copied {copied} no-x {no_x} made-x {made_x} tmpfs {runs_from_tmpfs} not-elf {not_elf} dir {dir}");
        copied && no_x && made_x && runs_from_tmpfs && script && not_elf && dir
    });
    t.check("exec: kernel validates the argument block", || {
        let fault = raw::exec_raw(nr::SPAWN, 0xFFFF_8000_0000_0000) == Err(Errno::Fault)
            && raw::exec_raw(nr::EXECVE, 0) == Err(Errno::Fault);
        let path = "probe";
        let unterminated = b"probe\0count";
        let a = ExecArgs {
            path: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv: unterminated.as_ptr() as u64,
            argv_len: unterminated.len() as u64,
            envp: 0,
            envp_len: 0,
        };
        let open_block = raw::exec_raw(nr::SPAWN, &a as *const ExecArgs as usize) == Err(Errno::InvalidArgument);
        let huge = ExecArgs { argv_len: 1 << 30, ..a };
        let oversized = raw::exec_raw(nr::SPAWN, &huge as *const ExecArgs as usize) == Err(Errno::InvalidArgument);
        let bad_ptr = ExecArgs { argv: 0x10, argv_len: 4, ..a };
        let bad_argv = raw::exec_raw(nr::SPAWN, &bad_ptr as *const ExecArgs as usize) == Err(Errno::Fault);
        let empty = ExecArgs { path_len: 0, ..a };
        let no_path = raw::exec_raw(nr::SPAWN, &empty as *const ExecArgs as usize) == Err(Errno::InvalidArgument);
        fault && open_block && oversized && bad_argv && no_path
    });
    t.check("exec: execve keeps pid, handles and cwd; replaces program and env", || {
        let _ = fs::remove_file(REPORT);
        let Ok(child) = worker(&["exec-report", "/tmp/exec-handle.txt", REPORT]).spawn() else { return false };
        let pid = child.id();
        let status = child.wait();
        let Some(r) = Report::read(REPORT) else { return false };
        let handle_text = fs::read_to_string("/tmp/exec-handle.txt");
        println!("execve: status {status:?}, report pid {} cwd {} exe {}", r.get("pid"), r.get("cwd"), r.get("exe"));
        matches!(status, Ok(s) if s.success())
            && r.get("pid") == format!("{pid}")
            && r.get("cwd") == "/tmp"
            && r.get("exe") == "/usr/bin/probe"
            && r.all("arg") == ["probe", "report", REPORT, "after-exec"]
            && r.all("env").contains(&"FROM=worker")
            && r.get("handle3") == "ok"
            && handle_text.as_deref() == Ok("written by probe through an inherited handle\n")
    });
    t.check("exec: failed execve leaves the program running", || {
        matches!(worker(&["exec-fail"]).status(), Ok(s) if s.code() == Some(0))
    });
    t.check("exec: execve with other threads is refused", || {
        matches!(worker(&["exec-threads"]).status(), Ok(s) if s.code() == Some(0))
    });
    t.check("exec: a chain of 5 execve ends with the last program's status", || {
        matches!(Command::new("probe").args(["chain", "5"]).status(), Ok(s) if s.code() == Some(42))
    });
    let _ = fs::remove_file(REPORT);
    let _ = fs::remove_file("/tmp/exec-handle.txt");
    let _ = fs::remove_file("/tmp/exec-script");
}
