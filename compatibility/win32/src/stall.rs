//! Reports of threads that stop making progress. A blocking call notes
//! what the calling thread waits for (an object wait, a lock, a condition
//! variable, a completion port, a socket); a watchdog thread reports a
//! wait that has lasted longer than `REPORT_AFTER_NS`, once, together
//! with what every other waiting thread is blocked on and the code
//! addresses on its stack. A program that gives up on a stalled thread
//! (Steam's "fatal stalled cross-thread pipe") then shows where it stood.
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use os_std::sync::Mutex;

/// A minute and a half: longer than any wait a responsive program makes
/// on its own message or I/O threads, shorter than Steam's 180 s limit.
const REPORT_AFTER_NS: u64 = 90_000_000_000;
const SCAN_EVERY_MS: u64 = 10_000;

struct Wait {
    api: &'static str,
    /// The call's subject: a handle, an address.
    what: u64,
    /// A second detail (a handle count, a lock owner).
    extra: u64,
    since: u64,
    /// The thread's TEB and stack pointer when it started waiting.
    teb: u64,
    rsp: u64,
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
    let rsp: u64;
    // SAFETY: reads the stack pointer only.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    // Helper threads of winrun's own (thread-pool timers and waits) have no
    // TEB: only registered Windows threads do, and only theirs is read.
    let teb = if crate::kernel32::thread_object(tid as u32).is_some() { crate::teb::current() } else { 0 };
    let wait = Wait { api, what, extra, since: now_ns(), teb, rsp, reported: false };
    WAITS.lock().unwrap().entry(tid).or_default().push(wait);
    Guard(tid)
}

/// What a wait is on, in words.
fn describe(w: &Wait) -> String {
    match w.api {
        "WaitForObjects" => {
            let kind = crate::objects::get(w.what).map_or("closed handle", |o| crate::objects::kind_name(&o));
            format!("{} handle(s), first {:#x} ({kind})", w.extra, w.what)
        }
        "EnterCriticalSection" => format!("critical section {:#x}, owner thread {}", w.what, w.extra),
        _ => format!("{:#x}", w.what),
    }
}

fn watchdog() {
    loop {
        os_std::thread::sleep(core::time::Duration::from_millis(SCAN_EVERY_MS));
        let now = now_ns();
        // Copied out, so the report below runs without the lock. The stack
        // of a newly stalled thread is copied while the lock is held: the
        // thread cannot leave its wait (and exit, freeing its stack) before
        // the lock is released.
        let mut stacks: Vec<(u64, u64, u64, Vec<u64>)> = Vec::new();
        let snapshot: Vec<(u64, &'static str, String, u64, u64, u64, bool)> = {
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
                        if w.teb != 0 {
                            // SAFETY: the waiting thread's TEB and stack, alive while it
                            // waits (it cannot drop its guard while WAITS is held).
                            unsafe {
                                let (limit, base) = (crate::teb::get::<u64>(w.teb, crate::teb::TEB_STACK_LIMIT), crate::teb::get::<u64>(w.teb, crate::teb::TEB_STACK_BASE));
                                let start = w.rsp.max(limit).min(base) & !7;
                                let end = start.saturating_add(32 << 10).min(base);
                                let words = core::slice::from_raw_parts(start as *const u64, ((end - start) / 8) as usize).to_vec();
                                stacks.push((tid, start, end, words));
                            }
                        }
                    }
                    out.push((tid, w.api, describe(w), now.saturating_sub(w.since) / 1_000_000_000, w.teb, w.rsp, fresh));
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
        for (i, &(tid, _)) in threads.iter().enumerate() {
            let busy = after[i].saturating_sub(before[i]);
            let waiting = snapshot.iter().any(|w| w.0 == tid as u64);
            let tail = match last.get(&(tid as u64)) {
                Some((api, what, at)) => format!("last {api} on {what:#x} {} ms ago", now.saturating_sub(*at) / 1_000_000),
                None => String::from("no tracked wait yet"),
            };
            os_std::eprintln!("winrun:   thread {tid}: {busy} ticks{} — {tail}", if waiting { ", waiting" } else { "" });
        }
        os_std::eprintln!("winrun: stalled waits ({} thread waits under way):", snapshot.len());
        for (tid, api, what, secs, teb, rsp, fresh) in snapshot {
            os_std::eprintln!("winrun:   thread {tid}{} {api} for {secs} s on {what}", if fresh { " STALLED" } else { "" });
            let _ = (teb, rsp);
            if fresh {
                if let Some((_, _, _, words)) = stacks.iter().find(|s| s.0 == tid) {
                    if let Some(chain) = crate::exceptions::chain_of(words) {
                        os_std::eprintln!("winrun:     called from: {chain}");
                    }
                }
            }
        }
    }
}
