//! Windows compatibility checks (`init win,<tcp>,<udp>`): the PE32+ test
//! programs in /opt/wintests (built from compatibility/win32/tests/src by
//! scripts/build-wintests.ps1) run under /bin/winrun. Each program prints
//! its own checks to the log and exits with its failure count; the GUI
//! program runs inside a PRISM session started here.

use crate::Tests;
use os_std::prelude::*;
use os_std::process::Command;
use os_std::time::{Duration, Instant};
use os_std::{fs, thread};

const DIR: &str = "/opt/wintests";

fn run(exe: &str, args: &[&str]) -> Option<i32> {
    let t = Instant::now();
    let mut c = Command::new("/bin/winrun");
    c.arg(&format!("{DIR}/{exe}"));
    c.args(args);
    let r = c.status().ok().and_then(|s| s.code());
    println!("win: {exe} exited with {:?} after {} ms", r, t.elapsed().as_millis());
    r
}

pub fn run_all(t: &mut Tests, tcp: u16, udp: u16) {
    t.check("native CPU fault delivery and safe context resumption", || {
        Command::new("worker").arg("fault-checks").status().is_ok_and(|s|s.success())
    });
    if fs::metadata(DIR).is_err() {
        println!("win: {DIR} is missing (the Windows test programs were not built)");
        t.check("Windows test programs installed", || false);
        return;
    }
    t.check("PE32+ minimal program: relocated, exit code 42 (target A)", || run("minimal.exe", &[]) == Some(42));
    t.check("kernel32 console program: TEB/PEB, last error, modules (target B)", || {
        run("hello.exe", &["first arg", "second"]) == Some(0)
    });
    t.check("Windows file I/O on OSyx file systems (target C)", || run("fileio.exe", &[]) == Some(0));
    t.check("Windows volume, device and adapter inventory", || run("sysadmin.exe", &[]) == Some(0));
    t.check("Windows file version resources and queries", || run("version.exe", &[]) == Some(0));
    t.check("Windows vectored hardware exceptions and context resumption", || run("exceptions.exe", &[]) == Some(0));
    t.check("Windows file mappings: shared, read-only and copy-on-write views", || run("mapping.exe", &[]) == Some(0));
    t.check("NT runtime: Rtl strings and heap, SLists, unwinding, times, registry", || run("runtime.exe", &[]) == Some(0));
    t.check("structured exception handling, unwinding, consolidation and FLS", || run("seh.exe", &[]) == Some(0));
    t.check("newer Win32: files, NLS, thread pool, processes, jobs, shlwapi, WinRT", || run("platform.exe", &[]) == Some(0));
    t.check("Windows SID ACL descriptor and token data", || run("security.exe", &[]) == Some(0));
    t.check("Windows tokens follow native uid after dropping root", || {
        Command::new("worker").args(["exec-as", "1000", "/bin/winrun", "/opt/wintests/security.exe"])
            .env("WIN_EXPECT_UID", "1000").status().is_ok_and(|s|s.success())
    });
    t.check("Windows shared locks, once initialization, condition variables and modules", || run("synch.exe", &[]) == Some(0));
    t.check("threads, synchronisation, TLS, virtual memory, DLL loading (target D)", || {
        run("threads.exe", &[]) == Some(0)
    });
    let (tp, up) = (format!("{tcp}"), format!("{udp}"));
    t.check("Winsock over the OSyx network stack (target F)", || run("winsock.exe", &[&tp, &up]) == Some(0));
    t.check("a missing program is reported", || {
        Command::new("/bin/winrun").arg("/opt/wintests/none.exe").status().ok().and_then(|s| s.code()) == Some(2)
    });
    t.check("a non-PE file is rejected without crashing", || {
        Command::new("/bin/winrun").arg("/bin/init").status().ok().and_then(|s| s.code()) == Some(126)
    });
    // The GUI program needs a compositor: start PRISM (with the desktop
    // shell) in the background, then run it as a client.
    let prism = Command::new("/system/bin/prism").spawn();
    t.check("Win32 GUI window through PRISM (target E)", || {
        if prism.is_err() {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
        run("gui.exe", &[]) == Some(0)
    });
    if let Ok(p) = prism {
        let _ = os_std::system::kill(p.id());
        let _ = p.wait();
    }
}
