//! Reports of threads that stop making progress. A blocking call notes
//! what the calling thread waits for (an object wait, a lock, a condition
//! variable, a completion port, a socket); a watchdog thread reports a
//! wait that has lasted longer than `REPORT_AFTER_NS`, once, together
//! with what every other thread is blocked on (a tracked wait, or the
//! system call the kernel last saw it make) and the code addresses on its
//! stack. A program that gives up on a stalled thread
//! (Steam's "fatal stalled cross-thread pipe") then shows where it stood.
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use os_std::sync::Mutex;

/// A minute and a half: longer than any wait a responsive program makes
/// on its own message or I/O threads, shorter than Steam's 180 s limit.
const REPORT_AFTER_NS: u64 = 90_000_000_000;
const SCAN_EVERY_MS: u64 = 10_000;
/// Entries read per thread: six state entries, then 32 KiB of stack.
const STATE_WORDS: usize = 6;
const STACK_WORDS: usize = STATE_WORDS + 4096;

/// A system call a blocked thread is in, in words.
fn syscall_text(nr: u64, arg0: u64, arg3: u64) -> String {
    use osyx_abi::nr;
    match nr {
        nr::FUTEX_WAIT_OWNED => {
            let what = if crate::loader::is_loader_lock(arg0) { " (the loader lock)" } else { "" };
            format!("waits for lock {arg0:#x}{what} held by thread {arg3}")
        }
        nr::FUTEX_WAIT => format!("futex_wait on {arg0:#x}"),
        nr::SLEEP => String::from("sleep"),
        nr::SOCK_ACCEPT => format!("sock_accept on handle {arg0:#x}"),
        nr::SOCK_RECV => format!("sock_recv on handle {arg0:#x}"),
        nr::SOCK_CONNECT => format!("sock_connect on handle {arg0:#x}"),
        _ => format!("system call {nr:#x} ({arg0:#x}, ..., {arg3:#x})"),
    }
}

struct Wait {
    api: &'static str,
    /// The call's subject: a handle, an address.
    what: u64,
    /// A second detail (a handle count, a lock owner).
    extra: u64,
    since: u64,
    reported: bool,
}

/// The waits under way, by thread id (innermost last).
static WAITS: Mutex<BTreeMap<u64, Vec<Wait>>> = Mutex::new(BTreeMap::new());
/// Each thread's last finished wait: (api, subject, when it ended).
static LAST: Mutex<BTreeMap<u64, (&'static str, u64, u64)>> = Mutex::new(BTreeMap::new());
static WATCHDOG: AtomicBool = AtomicBool::new(false);

fn now_ns() -> u64 {
    os_sys::clock(osyx_abi::CLOCK_MONOTONIC).unwrap_or(0)
}

/// Ends the wait it was made for when dropped.
pub struct Guard(u64);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut waits = WAITS.lock().unwrap();
        if let Some(list) = waits.get_mut(&self.0) {
            if let Some(w) = list.pop() {
                LAST.lock().unwrap().insert(self.0, (w.api, w.what, now_ns()));
            }
            if list.is_empty() {
                waits.remove(&self.0);
            }
        }
    }
}

/// Notes that the calling thread is about to block in `api` on `what`.
pub fn waiting(api: &'static str, what: u64, extra: u64) -> Guard {
    if !WATCHDOG.swap(true, Ordering::Relaxed) {
        // Diagnostics only: if the thread cannot start, waits go on
        // without reports.
        let _ = os_std::thread::Builder::new().stack_size(64 << 10).spawn(watchdog);
    }
    let tid = os_sys::gettid();
    let wait = Wait { api, what, extra, since: now_ns(), reported: false };
    WAITS.lock().unwrap().entry(tid).or_default().push(wait);
    Guard(tid)
}

/// What a wait is on, in words.
fn describe(w: &Wait) -> String {
    match w.api {
        "WaitForObjects" => {
            let object = crate::objects::get(w.what);
            let kind = object.as_ref().map_or("closed handle", crate::objects::kind_name);
            let detail = match &object {
                Some(crate::objects::Object::Event(e)) => {
                    let by = e.last_set_by.load(Ordering::Relaxed);
                    let set = if by == 0 {
                        String::from("never set here")
                    } else {
                        format!("last set by thread {by} {} ms ago", now_ns().saturating_sub(e.last_set_at.load(Ordering::Relaxed)) / 1_000_000)
                    };
                    format!(
                        " {}{}, {}, {set}",
                        if e.manual { "manual" } else { "auto" },
                        e.name.as_deref().map(|n| format!(" \"{n}\"")).unwrap_or_default(),
                        if e.is_set() { "SET" } else { "clear" },
                    )
                }
                _ => String::new(),
            };
            format!("{} handle(s), first {:#x} ({kind}{detail})", w.extra, w.what)
        }
        "EnterCriticalSection" => format!("critical section {:#x}, owner thread {}", w.what, w.extra),
        _ => format!("{:#x}", w.what),
    }
}

fn watchdog() {
    loop {
        os_std::thread::sleep(core::time::Duration::from_millis(SCAN_EVERY_MS));
        let now = now_ns();
        // Copied out, so the report below runs without the lock.
        let snapshot: Vec<(u64, &'static str, String, u64, bool)> = {
            let mut waits = WAITS.lock().unwrap();
            let stalled = waits.values().flatten().any(|w| !w.reported && now.saturating_sub(w.since) > REPORT_AFTER_NS);
            if !stalled {
                continue;
            }
            let mut out = Vec::new();
            for (&tid, list) in waits.iter_mut() {
                for w in list.iter_mut() {
                    let fresh = !w.reported && now.saturating_sub(w.since) > REPORT_AFTER_NS;
                    if fresh {
                        w.reported = true;
                    }
                    out.push((tid, w.api, describe(w), now.saturating_sub(w.since) / 1_000_000_000, fresh));
                }
            }
            out
        };
        // Every Windows thread, with its CPU use over two seconds: a
        // thread that is in no tracked wait is running or blocked
        // elsewhere, and a spinning one shows here.
        let threads = crate::kernel32::thread_list();
        let before: Vec<u64> = threads.iter().map(|&(t, _)| os_sys::thread_times(t as u64).unwrap_or(0)).collect();
        os_std::thread::sleep(core::time::Duration::from_secs(2));
        let after: Vec<u64> = threads.iter().map(|&(t, _)| os_sys::thread_times(t as u64).unwrap_or(0)).collect();
        let last = LAST.lock().unwrap().clone();
        let now = now_ns();
        os_std::eprintln!("winrun: threads ({}), CPU ticks in 2 s, and their last finished wait:", threads.len());
        let mut words: Vec<u64> = alloc::vec![0; STACK_WORDS];
        // The image list, if the loader is free: a stalled thread may hold
        // its lock, and the report must not wait for it.
        let images = crate::loader::try_images();
        if images.is_none() {
            os_std::eprintln!("winrun:   (the loader is busy: stacks shown as raw code addresses are not resolved)");
        }
        for (i, &(tid, _)) in threads.iter().enumerate() {
            let busy = after[i].saturating_sub(before[i]);
            let waiting = snapshot.iter().any(|w| w.0 == tid as u64);
            let tail = match last.get(&(tid as u64)) {
                Some((api, what, at)) => format!("last {api} on {what:#x} {} ms ago", now.saturating_sub(*at) / 1_000_000),
                None => String::from("no tracked wait yet"),
            };
            os_std::eprintln!("winrun:   thread {tid}: {busy} ticks{} — {tail}", if waiting { ", waiting" } else { "" });
            // Where the kernel last saw the thread: the system call it is in
            // (or made last) and the code addresses on its stack.
            if let Ok(n) = os_sys::thread_state(tid as u64, &mut words) {
                let state = ["ready", "running", "blocked", "dead"].get(words[0] as usize).copied().unwrap_or("?");
                os_std::eprintln!("winrun:     {state}, {} (from {:#x})", syscall_text(words[1], words[4], words[5]), words[2]);
                if let Some(chain) = images.as_ref().and_then(|i| crate::exceptions::chain_of(&words[STATE_WORDS.min(n)..n], i)) {
                    os_std::eprintln!("winrun:     stack: {chain}");
                }
            }
        }
        os_std::eprintln!("winrun: stalled waits ({} thread waits under way):", snapshot.len());
        for (tid, api, what, secs, fresh) in snapshot {
            os_std::eprintln!("winrun:   thread {tid}{} {api} for {secs} s on {what}", if fresh { " STALLED" } else { "" });
        }
    }
}
