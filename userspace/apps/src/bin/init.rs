//! The init program (`/bin/init` on the root file system). Runs in ring 3
//! and checks OSyx Std, the kernel's system call validation, processes
//! (spawn / wait / zombies / orphans) and the preemptive scheduler from the
//! user side, using `worker` as the child program, the file systems through
//! `os_std::fs` (see `fs_checks.rs`) and the program loader with `probe`
//! (see `exec_checks.rs`).
//!
//! Arguments (kernel command line `init.args=...`):
//! * none:          run the checks; exit status = number of failed checks
//! * `fault`:       dereference an unmapped address (the kernel must kill us)
//! * `panic`:       panic (OSyx Std must report it and exit with 101)
//! * `exit,<code>`: exit with `<code>`
//! * `crash-prep`:  leave an open, removed file on / for the power-cut test
//! * `net,<tcp>,<udp>`: network checks against the host's echo servers on
//!   10.0.2.2 (see `net_checks.rs`); exit status = failed checks
//! * `win,<tcp>,<udp>`: Windows PE32+ programs under /bin/winrun (see
//!   `win_checks.rs`); exit status = failed checks

#![no_std]
#![no_main]

use os_std::error::ErrorKind;
use os_std::os::raw::{self, Errno, PROT_EXEC, PROT_READ, PROT_WRITE};
use os_std::prelude::*;
use os_std::process::{Child, Command};
use os_std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use os_std::time::{Duration, Instant};
use os_std::{env, os, process, thread};

#[path = "../exec_checks.rs"]
mod exec_checks;
#[path = "../fpu.rs"]
mod fpu;
#[path = "../fs_checks.rs"]
mod fs_checks;
#[path = "../ipc_checks.rs"]
mod ipc_checks;
#[path = "../net_checks.rs"]
mod net_checks;
#[path = "../win_checks.rs"]
mod win_checks;

os_std::entry!(main);

static RODATA: [u8; 64] = [0x5A; 64];

fn main() -> i32 {
    println!("Hello from OSyx userspace!");
    let args: Vec<String> = env::args().collect();
    println!("args: {:?}", args);

    match args.get(1).map(String::as_str) {
        Some("fault") => {
            println!("reading unmapped address 0x1000 ...");
            // SAFETY: deliberately invalid; the kernel terminates the process.
            let v = unsafe { core::ptr::read_volatile(0x1000 as *const u64) };
            println!("unreachable: read {v}");
            return 99;
        }
        Some("panic") => panic!("panic requested by command line"),
        Some("exit") => {
            let code = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            process::exit(code);
        }
        Some("desktop") => return desktop_session(),
        Some("zen-probe") => return zen_probe(args.get(2).map(String::as_str), args.get(3).is_some_and(|s| s == "trace")),
        // `init winrun <program> [args...]`: one Windows program with no
        // sessions around it, for bring-up diagnostics.
        Some("winrun") => {
            let mut c = Command::new("/bin/winrun");
            c.arg("-v");
            for a in &args[2..] {
                c.arg(a);
            }
            return c.status().ok().and_then(|s| s.code()).unwrap_or(1);
        }
        Some("net") => {
            let port = |i: usize, d: u16| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let mut t = Tests::default();
            net_checks::run(&mut t, port(2, 7101), port(3, 7102));
            println!("net checks: {} passed, {} failed", t.passed, t.failed);
            return t.failed as i32;
        }
        Some("win") => {
            let port = |i: usize, d: u16| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let mut t = Tests::default();
            win_checks::run_all(&mut t, port(2, 7101), port(3, 7102));
            println!("win checks: {} passed, {} failed", t.passed, t.failed);
            return t.failed as i32;
        }
        Some("crash-prep") => {
            // Leave a removed-but-open file on the ext4 root, then exit
            // (the kernel's test.power-cut stops the machine right after).
            let _holder = worker(&["hold-orphan", "/crash-orphan.bin"]).spawn();
            thread::sleep(Duration::from_millis(500));
            println!("crash-prep: orphan held by a child; exiting");
            return 0;
        }
        _ => {}
    }

    let mut t = Tests::default();
    t.check("std-style collections", || {
        let mut values = Vec::new();
        for v in [10, 20] {
            values.push(v);
        }
        println!("Values: {:?}", values);
        let mut words: BTreeMap<String, usize> = BTreeMap::new();
        for w in "the quick brown fox jumps over the lazy dog the end".split(' ') {
            *words.entry(w.to_string()).or_default() += 1;
        }
        values == [10, 20] && words["the"] == 3 && words.len() == 9
    });
    t.check("heap: 2 MiB vector and many small boxes", || {
        let big = vec![0xA5u8; 2 << 20];
        let boxes: Vec<Box<u64>> = (0..10_000).map(Box::new).collect();
        big.iter().all(|&b| b == 0xA5) && boxes.iter().map(|b| **b).sum::<u64>() == 49_995_000
    });
    t.check("string formatting", || {
        format!("{:>8.3}|{:#x}|{:?}", "OSyx Std", 255, Some("x")) == "     OSy|0xff|Some(\"x\")"
    });
    t.check("sysinfo", || match os::sysinfo() {
        Ok(info) => {
            println!(
                "kernel {} | ABI {} | pid {} | {} MiB free of {} MiB | page {} B",
                os::kernel_version(&info),
                info.abi_version,
                info.pid,
                info.free_memory >> 20,
                info.total_memory >> 20,
                info.page_size
            );
            info.abi_version == raw::ABI_VERSION && info.page_size == 4096 && info.free_memory <= info.total_memory
        }
        Err(e) => {
            println!("sysinfo failed: {e}");
            false
        }
    });
    t.check("display: mode setting (VirtIO-GPU switches, GOP keeps its mode)", || {
        let Ok(d) = os_std::system::display_info() else { return true }; // headless
        let same = os_std::system::display_set_mode(d.width, d.height).is_ok();
        if d.backend == os_std::system::BACKEND_VIRTIO_GPU {
            let switched = os_std::system::display_set_mode(800, 600).is_ok()
                && os_std::system::display_info().is_ok_and(|n| (n.width, n.height, n.stride) == (800, 600, 800));
            let back = os_std::system::display_set_mode(d.width, d.height).is_ok()
                && os_std::system::display_info().is_ok_and(|n| (n.width, n.height) == (d.width, d.height));
            println!("display: virtio-gpu {}x{} -> 800x600 -> back: {}", d.width, d.height, switched && back);
            same && switched && back
        } else {
            let refused =
                os_std::system::display_set_mode(800, 600).err().map(|e| e.kind()) == Some(ErrorKind::Unsupported);
            same && refused
        }
    });
    t.check("memory: protect, fixed and 64 KiB aligned mappings", || {
        let Ok(p) = raw::map_ex(0, 3 * 4096, PROT_READ | PROT_WRITE, raw::MAP_ALIGN_64K) else { return false };
        let aligned = (p as usize).is_multiple_of(0x10000);
        // SAFETY: `p` maps three fresh pages.
        unsafe { p.add(4096).write(7) };
        // SAFETY: the middle page becomes read-only; it is not written again.
        let ro = unsafe { raw::protect(p.add(4096), 4096, PROT_READ) }.is_ok();
        // W^X: writable + executable is refused; outside a mapping is invalid.
        // SAFETY: the calls are expected to fail and change nothing.
        let wx = unsafe { raw::protect(p, 4096, PROT_READ | PROT_WRITE | PROT_EXEC) } == Err(Errno::PermissionDenied);
        // SAFETY: as above.
        let outside = unsafe { raw::protect(p.add(16 * 4096), 4096, PROT_READ) } == Err(Errno::InvalidArgument);
        // SAFETY: the page is still readable.
        let kept = unsafe { p.add(4096).read() } == 7;
        let fixed_at = p as usize + 0x100_0000;
        let fixed = raw::map_ex(fixed_at, 4096, PROT_READ | PROT_WRITE, raw::MAP_FIXED) == Ok(fixed_at as *mut u8);
        let taken = raw::map_ex(fixed_at, 4096, PROT_READ, raw::MAP_FIXED) == Err(Errno::AlreadyExists);
        // SAFETY: both regions were mapped above with these lengths.
        let freed = unsafe { raw::unmap(p, 3 * 4096).is_ok() && raw::unmap(fixed_at as *mut u8, 4096).is_ok() };
        aligned && ro && wx && outside && kept && fixed && taken && freed
    });
    t.check("GS base per thread (set_gs_base)", || {
        let read_gs = || {
            let v: u64;
            // SAFETY: gs:[0] is readable whenever the GS base points at `cell`.
            unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) v, options(nostack, readonly)) };
            v
        };
        let cell = Box::new(0x5EED_u64);
        if raw::set_gs_base(&*cell as *const u64 as usize).is_err() {
            return false;
        }
        let mine = read_gs() == 0x5EED;
        // Another thread has its own GS base (0 until it sets one).
        let other = thread::spawn(move || {
            let cell2 = Box::new(0xBEEF_u64);
            raw::set_gs_base(&*cell2 as *const u64 as usize).is_ok() && {
                let v: u64;
                // SAFETY: as above, for this thread's cell.
                unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) v, options(nostack, readonly)) };
                v == 0xBEEF
            }
        })
        .join()
        .unwrap_or(false);
        thread::yield_now();
        let still = read_gs() == 0x5EED;
        let _ = raw::set_gs_base(0);
        mine && other && still
    });
    t.check("monotonic clock", || {
        let a = Instant::now();
        let b = Instant::now();
        b >= a
    });
    t.check("kernel rejects kernel-space pointer", || raw::write_raw(1, 0xFFFF_8000_0000_0000, 8) == Err(Errno::Fault));
    t.check("kernel rejects null pointer", || raw::write_raw(1, 0, 4) == Err(Errno::Fault));
    t.check("kernel rejects unmapped user pointer", || raw::write_raw(1, 0x7000_0000_0000, 16) == Err(Errno::Fault));
    t.check("kernel rejects bad handle", || raw::write_raw(42, RODATA.as_ptr() as usize, 1) == Err(Errno::BadHandle));
    t.check("kernel refuses to write into read-only memory", || {
        raw::sysinfo_raw(RODATA.as_ptr() as usize) == Err(Errno::Fault) && RODATA[0] == 0x5A
    });
    t.check("unknown system call", || raw::raw_syscall(999, 0, 0, 0) == Err(Errno::NotSupported));
    t.check("W^X: writable+executable mapping refused", || {
        raw::map(4096, PROT_READ | PROT_WRITE | PROT_EXEC).err() == Some(Errno::PermissionDenied)
    });
    t.check("map / unmap round trip", || {
        let Ok(p) = raw::map(3 * 4096, PROT_READ | PROT_WRITE) else { return false };
        // SAFETY: freshly mapped, zeroed, writable, 3 pages long.
        let ok = unsafe {
            let s = core::slice::from_raw_parts_mut(p, 3 * 4096);
            let zeroed = s.iter().all(|&b| b == 0);
            s.fill(7);
            zeroed && s[3 * 4096 - 1] == 7
        };
        // SAFETY: no references into the region remain.
        ok && unsafe { raw::unmap(p, 3 * 4096) }.is_ok() && unsafe { raw::unmap(p, 3 * 4096) }.is_err()
    });
    t.check("memory: demand-zero pages, kernel access and discard", || {
        const GIB: usize = 1 << 30;
        let free = || os::sysinfo().map_or(0, |i| i.free_memory);
        let before = free();
        // A 1 GiB reservation costs no frames until its pages are touched.
        let Ok(p) = raw::map(GIB, PROT_READ | PROT_WRITE) else { return false };
        let cheap = before.saturating_sub(free()) < 4 << 20;
        // SAFETY: inside the fresh mapping.
        let touched = unsafe {
            let q = p.add(GIB / 2);
            let zero = q.read() == 0;
            q.write(9);
            zero && q.read() == 9
        };
        // The kernel fills an untouched page through a user copy.
        let kernel = raw::sysinfo_raw(p as usize + 700 * (1 << 20)).is_ok();
        // A fixed mapping may not land on untouched pages of the region.
        let taken = raw::map_ex(p as usize + GIB - 4096, 4096, PROT_READ, raw::MAP_FIXED) == Err(Errno::AlreadyExists);
        // SAFETY: the discarded page is only read again, as zeros.
        let discarded = unsafe { raw::discard(p.add(GIB / 2), 4096).is_ok() && p.add(GIB / 2).read() == 0 };
        // SAFETY: no references into the region remain.
        let freed = unsafe { raw::unmap(p, GIB) }.is_ok();
        cheap && touched && kernel && taken && discarded && freed
    });

    process_checks(&mut t);
    thread_checks(&mut t);
    fs_checks::run(&mut t);
    exec_checks::run(&mut t);
    ipc_checks::run(&mut t);

    println!("user checks: {} passed, {} failed", t.passed, t.failed);
    t.failed as i32
}

/// `init desktop`: runs the graphical session (PRISM from the root file
/// system, which starts the desktop shell) and restarts it if it fails. If
/// it cannot be brought up, init exits and the kernel's status screen
/// stays as the diagnostic fallback.
fn zen_probe(url: Option<&str>, network_trace: bool) -> i32 {
    println!("zen-probe: official /opt/zen/zen.exe via /bin/winrun; no substitute browser");
    let _ = Command::new("/bin/winrun").arg("--exports").status();
    let prism = Command::new("/system/bin/prism").spawn();
    if prism.is_err() { println!("ZEN_PROBE_RESULT infrastructure=prism_start_failed"); return 1; }
    thread::sleep(Duration::from_secs(2));
    // OSyx has no Direct3D driver. Use Gecko's software WebRender path
    // explicitly instead of repeatedly probing unsupported DXGI devices.
    // Keep repeated boots of a diagnostic image independent, including
    // window-placement preferences and incomplete startup caches.
    let stamp = os_std::time::SystemTime::now().duration_since(os_std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let profile = format!("/tmp/zen-probe-profile-{stamp}");
    println!("zen-probe: profile {profile}");
    if os_std::fs::create_dir_all(&profile).and_then(|_| os_std::fs::write(
        format!("{profile}/user.js"),
        r#"user_pref("gfx.webrender.software", true);
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
user_pref("devtools.console.stdout.chrome", true);
user_pref("devtools.console.stdout.content", true);
"#,
    )).and_then(|_| os_std::fs::write(
        "/tmp/zen-probe-page.html",
        "<!doctype html><meta charset=utf-8><title>OSyx Zen rendering test</title><style>body{margin:40px;background:#dbeafe;color:#172554;font:20px Lato,sans-serif}button{font:inherit;padding:16px}#box{width:240px;height:120px;background:#16a34a;margin:24px 0}</style><h1>Zen renders HTML</h1><p>This page is rendered by the unmodified browser.</p><div id=box></div><button onclick=\"document.title='OSyx click verified';this.textContent='Click verified';document.getElementById('box').style.background='#f97316'\">Verify click</button><p><a target=_blank href=https://example.com/>Open example.com over HTTPS</a></p>",
    )).is_err() {
        println!("ZEN_PROBE_RESULT infrastructure=profile_creation_failed");
        if let Ok(p) = prism { let _ = os_std::system::kill(p.id()); let _ = p.wait(); }
        return 1;
    }
    let mut zen = Command::new("/bin/winrun");
    // The session runs as root, so Gecko's launcher would start the browser
    // with a de-elevated token, which OSyx cannot run: keep its own. The
    // probe has its own temporary profile with software-rendering prefs.
    zen.args(["-v", "/opt/zen/zen.exe", "--no-remote", "-profile"]);
    let url = url.unwrap_or("file:///C:/tmp/zen-probe-page.html");
    println!("zen-probe: URL {url}");
    zen.arg(&format!("C:{}", profile.replace('/', "\\"))).arg(url);
    // This bootstrap flag also reaches Gecko's command-line handler, which
    // still sees it as unknown and skips the following argument. Keep it
    // last so that the requested URL is not silently discarded.
    zen.arg("-no-deelevate");
    // Gecko's Windows sandbox broker starts its children suspended and
    // patches their memory, which OSyx gives no process the right to do:
    // Mozilla's own switches start them unsandboxed instead.
    // Every failing Windows API call goes to the serial log (bring-up).
    zen.env("OSYX_WINRUN_TRACE", "1");
    let log_modules = if network_trace {
        "timestamp,sync,nsDocShell:5,DocLoader:5,nsStreamPump:5,nsSocketTransport:2,nsHostResolver:3,pipnss:4,nsHttp:5,nsChannelClassifier:5"
    } else {
        "timestamp,sync,DocLoader:5,pipnss:4"
    };
    zen.env("MOZ_LOG", log_modules);
    for kind in ["CONTENT", "GPU", "RDD", "SOCKET_PROCESS", "GMP", "UTILITY", "VR"] {
        zen.env(&format!("MOZ_DISABLE_{kind}_SANDBOX"), "1");
    }
    let result = zen.status();
    println!("ZEN_PROBE_RESULT exit={:?}", result.as_ref().ok().and_then(|s| s.code()));
    if let Ok(p) = prism { let _ = os_std::system::kill(p.id()); let _ = p.wait(); }
    // Boot completion is separate from browser success; the host reads
    // the actual winrun exit above and never treats this as a browser pass.
    0
}

fn desktop_session() -> i32 {
    const PRISM: &str = "/system/bin/prism";
    for attempt in 1..=3 {
        println!("starting the graphical session: {PRISM} (attempt {attempt})");
        match Command::new(PRISM).status() {
            Ok(s) if s.success() => return 0,
            Ok(s) => eprintln!("{PRISM} ended with status {:#x}", s.raw()),
            Err(e) => {
                eprintln!("cannot start {PRISM}: {e}");
                return 1;
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    eprintln!("the graphical session keeps failing; the kernel console stays");
    1
}

fn worker(args: &[&str]) -> Command {
    let mut c = Command::new("worker");
    c.args(args);
    c
}

fn process_checks(t: &mut Tests) {
    t.check(
        "spawn + wait: child exit code",
        || matches!(worker(&["exit", "7"]).status(), Ok(s) if s.code() == Some(7)),
    );
    t.check("spawn: unknown program is NotFound", || {
        Command::new("no-such-program").spawn().map(|c| c.id()).map_err(|e| e.kind()) == Err(ErrorKind::NotFound)
    });
    t.check("wait: no such child", || raw::wait_raw(9999, 0) == Err(Errno::NoChild));
    t.check("wait: bad status pointer keeps the child waitable", || {
        let Ok(child) = worker(&["exit", "5"]).spawn() else { return false };
        let bad = raw::wait_raw(child.id(), 0xFFFF_8000_0000_0000) == Err(Errno::Fault);
        bad && matches!(child.wait(), Ok(s) if s.code() == Some(5))
    });
    t.check("wait_any collects every child once", || {
        let a = worker(&["exit", "11"]).spawn();
        let b = worker(&["exit", "12"]).spawn();
        if a.is_err() || b.is_err() {
            return false;
        }
        let mut codes = Vec::new();
        for _ in 0..2 {
            match process::wait_any() {
                Ok((_, s)) => codes.push(s.code().unwrap_or(-1)),
                Err(_) => return false,
            }
        }
        codes.sort();
        codes == [11, 12] && process::wait_any().map_err(|e| e.kind()).err() == Some(ErrorKind::NoChild)
    });
    t.check("nested spawn (grandchild)", || matches!(worker(&["child", "9"]).status(), Ok(s) if s.code() == Some(9)));
    t.check(
        "child CPU exception is reported",
        || matches!(worker(&["fault"]).status(), Ok(s) if s.exception() == Some(14) && s.code().is_none()),
    );
    t.check("sleep blocks for the requested time", || {
        let start = Instant::now();
        thread::sleep(Duration::from_millis(50));
        thread::yield_now();
        start.elapsed() >= Duration::from_millis(50)
    });
    t.check("preemption: 3 busy children run concurrently", || {
        // Each child spins 300 ms without yielding. Run one after another
        // they would need ~900 ms; preempted round robin, ~300 ms.
        let start = Instant::now();
        let kids: Vec<Child> = (0..3).filter_map(|_| worker(&["spin", "300"]).spawn().ok()).collect();
        let n = kids.len();
        let all_ok = kids.into_iter().all(|k| matches!(k.wait(), Ok(s) if s.success()));
        let took = start.elapsed();
        println!("3 x 300 ms spinners finished in {} ms", took.as_millis());
        n == 3 && all_ok && took >= Duration::from_millis(300) && took < Duration::from_millis(650)
    });
    t.check("FPU/SSE registers survive preemption", || {
        let mut kids_ok = false;
        // init keeps its own pattern in xmm0-15 while two children spin with
        // different patterns in theirs.
        let mine = fpu::preserved_across(0xAAAA_0000_0000_0000, || {
            let a = worker(&["fpu", "1", "200"]).spawn();
            let b = worker(&["fpu", "2", "200"]).spawn();
            if let (Ok(a), Ok(b)) = (a, b) {
                let sa = a.wait();
                let sb = b.wait();
                kids_ok = matches!(sa, Ok(s) if s.success()) && matches!(sb, Ok(s) if s.success());
            }
        });
        mine && kids_ok
    });
    t.check("orphaned grandchild is adopted by the kernel", || {
        let ok = matches!(worker(&["orphan"]).status(), Ok(s) if s.success());
        // Give the orphan (sleeping 100 ms) time to exit before init does.
        thread::sleep(Duration::from_millis(200));
        ok && process::wait_any().map_err(|e| e.kind()).err() == Some(ErrorKind::NoChild)
    });
    t.check("process id from sysinfo", || process::id().is_ok_and(|p| p >= 1));
}

#[derive(Default)]
struct Tests {
    passed: u32,
    failed: u32,
}

impl Tests {
    fn check(&mut self, name: &str, f: impl FnOnce() -> bool) {
        if f() {
            println!("[user] {name} ... ok");
            self.passed += 1;
        } else {
            eprintln!("[user] {name} ... FAILED");
            self.failed += 1;
        }
    }
}

os_std::thread_local! {
    static LOCAL_ID: core::cell::Cell<u64> = core::cell::Cell::new(0);
    static DROP_PROBE: DropProbe = DropProbe;
}

static TLS_DROPS: AtomicUsize = AtomicUsize::new(0);

struct DropProbe;

impl Drop for DropProbe {
    fn drop(&mut self) {
        TLS_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

fn thread_checks(t: &mut Tests) {
    t.check("threads: spawn and join return values", || {
        let hs: Vec<_> = (1..=4u64).map(|i| thread::spawn(move || (1..=i * 10_000).sum::<u64>())).collect();
        let sums: Vec<u64> = hs.into_iter().filter_map(|h| h.join().ok()).collect();
        sums == [50_005_000, 200_010_000, 450_015_000, 800_020_000]
    });
    t.check("threads: distinct ids and names", || {
        let main_id = thread::current().id();
        let h = thread::Builder::new()
            .name(String::from("named"))
            .spawn(|| (thread::current().id(), thread::current().name().map(String::from)))
            .unwrap();
        let (id, name) = h.join().unwrap();
        id != main_id && name.as_deref() == Some("named") && thread::current().name() == Some("main")
    });
    t.check("Mutex: 4 threads x 20000 slow increments under preemption", || {
        // Read, delay, write inside the lock: if the mutex did not exclude,
        // a preemption between read and write would lose increments.
        let m = Arc::new(Mutex::new(0u64));
        let contended = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                let contended = contended.clone();
                thread::spawn(move || {
                    for _ in 0..20_000 {
                        let mut g = match m.try_lock() {
                            Ok(g) => g,
                            Err(_) => {
                                contended.fetch_add(1, Ordering::Relaxed);
                                m.lock().unwrap()
                            }
                        };
                        let v = *g;
                        for _ in 0..40 {
                            core::hint::spin_loop();
                        }
                        *g = v + 1;
                    }
                })
            })
            .collect();
        let joined = hs.into_iter().all(|h| h.join().is_ok());
        let total = *m.lock().unwrap();
        let waits = contended.load(Ordering::Relaxed);
        println!("mutex counter: {total} in {} ms, {waits} contended acquisitions", start.elapsed().as_millis());
        joined && total == 80_000 && waits > 0
    });
    t.check("Condvar: producer / 3 consumers", || {
        let q = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let consumers: Vec<_> = (0..3)
            .map(|_| {
                let q = q.clone();
                thread::spawn(move || {
                    let (m, cv) = &*q;
                    let mut sum = 0u64;
                    loop {
                        let mut g = cv.wait_while(m.lock().unwrap(), |q: &mut VecDeque<u64>| q.is_empty()).unwrap();
                        match g.pop_front() {
                            Some(0) | None => return sum,
                            Some(v) => sum += v,
                        }
                    }
                })
            })
            .collect();
        let (m, cv) = &*q;
        for v in 1..=500u64 {
            m.lock().unwrap().push_back(v);
            cv.notify_one();
            if v % 50 == 0 {
                thread::yield_now();
            }
        }
        m.lock().unwrap().extend([0, 0, 0]);
        cv.notify_all();
        let total: u64 = consumers.into_iter().filter_map(|h| h.join().ok()).sum();
        total == 125_250
    });
    t.check("Condvar: wait_timeout expires", || {
        let m = Mutex::new(());
        let cv = Condvar::new();
        let start = Instant::now();
        let (_g, r) = cv.wait_timeout(m.lock().unwrap(), Duration::from_millis(30)).unwrap();
        r.timed_out() && start.elapsed() >= Duration::from_millis(30)
    });
    t.check("futex: value mismatch and timeout", || {
        let word = AtomicU32::new(0);
        let mismatch = raw::futex_wait(word.as_ptr(), 1, 0) == Err(Errno::WouldBlock);
        let start = Instant::now();
        let timed = raw::futex_wait(word.as_ptr(), 0, 30_000_000) == Err(Errno::TimedOut);
        let unaligned =
            raw::futex_wait((word.as_ptr() as usize + 1) as *const u32, 0, 1) == Err(Errno::InvalidArgument);
        mismatch
            && timed
            && unaligned
            && start.elapsed() >= Duration::from_millis(30)
            && raw::futex_wake(word.as_ptr(), 1) == 0
    });
    t.check("TLS: thread_local! per thread, destructors run", || {
        LOCAL_ID.with(|c| c.set(1));
        let before = TLS_DROPS.load(Ordering::SeqCst);
        let hs: Vec<_> = (2..=4u64)
            .map(|i| {
                thread::spawn(move || {
                    DROP_PROBE.with(|_| ());
                    LOCAL_ID.with(|c| {
                        let initial = c.get();
                        c.set(i);
                        thread::sleep(Duration::from_millis(20)); // let the others run
                        (initial, c.get())
                    })
                })
            })
            .collect();
        let results: Vec<(u64, u64)> = hs.into_iter().filter_map(|h| h.join().ok()).collect();
        results == [(0, 2), (0, 3), (0, 4)]
            && LOCAL_ID.with(|c| c.get()) == 1
            && TLS_DROPS.load(Ordering::SeqCst) == before + 3
    });
    t.check("detached threads are reaped", || {
        let flag = Arc::new(AtomicU32::new(0));
        let f = flag.clone();
        drop(thread::spawn(move || f.store(1, Ordering::SeqCst)));
        let start = Instant::now();
        while thread::detached_pending() > 0 && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        flag.load(Ordering::SeqCst) == 1 && thread::detached_pending() == 0
    });
    t.check(
        "exit with blocked threads ends the process",
        || matches!(worker(&["threads-exit"]).status(), Ok(s) if s.code() == Some(42)),
    );
    t.check(
        "process lives until its last thread exits",
        || matches!(worker(&["thread-last"]).status(), Ok(s) if s.code() == Some(9)),
    );
    t.check(
        "fault in a spawned thread ends the process",
        || matches!(worker(&["thread-fault"]).status(), Ok(s) if s.exception() == Some(14)),
    );
    t.check(
        "panic in a spawned thread exits with 101",
        || matches!(worker(&["thread-panic"]).status(), Ok(s) if s.code() == Some(101)),
    );
}
