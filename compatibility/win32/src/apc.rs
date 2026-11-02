//! User-mode asynchronous procedure calls.
//!
//! QueueUserAPC and the completion routines of ReadFileEx/WriteFileEx
//! queue a call to a thread; the thread runs its queue when it enters an
//! alertable wait (SleepEx, WaitFor*ObjectsEx, MsgWaitForMultipleObjectsEx
//! with MWMO_ALERTABLE, NtDelayExecution, ...), and the wait then returns
//! WAIT_IO_COMPLETION. A call queued while the thread already waits
//! alertably ends the wait too: alertable waits sleep in short slices and
//! look at the queue between them.
#![allow(non_snake_case)]

use crate::{
    error::*,
    kernel32::fail,
    objects::{self, Object},
};
use alloc::collections::{BTreeMap, VecDeque};
use os_std::sync::Mutex;

pub const WAIT_IO_COMPLETION: u32 = 0xC0;
/// How long an alertable wait sleeps before it looks at its queue again.
pub const SLICE_MS: u32 = 20;

enum Apc {
    /// PAPCFUNC(ULONG_PTR)
    User { func: u64, arg: u64 },
    /// LPOVERLAPPED_COMPLETION_ROUTINE(dwErrorCode, dwNumberOfBytesTransfered, lpOverlapped)
    Completion { func: u64, error: u32, bytes: u32, ov: u64 },
}

/// Pending calls by thread id.
static QUEUES: Mutex<BTreeMap<u32, VecDeque<Apc>>> = Mutex::new(BTreeMap::new());
/// Completion routines of pending ReadFileEx/WriteFileEx operations, by
/// OVERLAPPED: (thread id, routine).
static ROUTINES: Mutex<BTreeMap<u64, (u32, u64)>> = Mutex::new(BTreeMap::new());

fn current() -> u32 {
    os_sys::gettid() as u32
}

fn queue(tid: u32, apc: Apc) {
    QUEUES.lock().unwrap().entry(tid).or_default().push_back(apc);
}

/// Runs the current thread's queued calls in order; whether any ran.
pub fn drain() -> bool {
    let tid = current();
    let mut ran = false;
    loop {
        // Taken one at a time: a call may queue more.
        let Some(apc) = QUEUES.lock().unwrap().get_mut(&tid).and_then(|q| q.pop_front()) else { break };
        ran = true;
        match apc {
            Apc::User { func, arg } => {
                if crate::wow64::is_active() {
                    crate::wow64::call32(func as u32, &[arg as u32]);
                } else {
                    // SAFETY: the PAPCFUNC the program queued.
                    let f: extern "win64" fn(u64) = unsafe { core::mem::transmute(func) };
                    f(arg);
                }
            }
            Apc::Completion { func, error, bytes, ov } => {
                if crate::wow64::is_active() {
                    crate::wow64::call32(func as u32, &[error, bytes, ov as u32]);
                } else {
                    // SAFETY: the completion routine the program passed.
                    let f: extern "win64" fn(u32, u32, u64) = unsafe { core::mem::transmute(func) };
                    f(error, bytes, ov);
                }
            }
        }
    }
    ran
}

/// Runs queued calls before an alertable wait: Some(WAIT_IO_COMPLETION)
/// when any ran, so the wait returns at once.
pub fn before_wait(alertable: bool) -> Option<u32> {
    (alertable && drain()).then_some(WAIT_IO_COMPLETION)
}

/// An alertable wait: `wait(ms)` in slices of at most SLICE_MS, running
/// calls queued meanwhile. `timeout` is the result `wait` gives when its
/// time is up.
pub fn alertable_wait(ms: u32, timeout: u32, mut wait: impl FnMut(u32) -> u32) -> u32 {
    if let Some(r) = before_wait(true) {
        return r;
    }
    let deadline = (ms != objects::INFINITE).then(|| os_std::time::Instant::now() + core::time::Duration::from_millis(ms as u64));
    loop {
        let left = match deadline {
            None => SLICE_MS,
            Some(d) => d.duration_since(os_std::time::Instant::now()).as_millis().min(SLICE_MS as u128) as u32,
        };
        let r = wait(left);
        if r != timeout {
            return r;
        }
        if drain() {
            return WAIT_IO_COMPLETION;
        }
        if deadline.is_some_and(|d| os_std::time::Instant::now() >= d) {
            return timeout;
        }
    }
}

/// Remembers the completion routine of an operation on `ov`.
pub fn register(ov: u64, routine: u64) {
    ROUTINES.lock().unwrap().insert(ov, (current(), routine));
}

/// An operation on `ov` finished: queues its completion routine, if it
/// has one, to the thread that started it.
pub fn completed(ov: u64, error: u32, bytes: u32) {
    if let Some((tid, func)) = ROUTINES.lock().unwrap().remove(&ov) {
        queue(tid, Apc::Completion { func, error, bytes, ov });
    }
}

fn thread_id(h: u64) -> Option<u32> {
    if h == objects::CURRENT_THREAD {
        return Some(current());
    }
    match objects::get(h) {
        Some(Object::Thread(t)) => Some(t.tid.load(core::sync::atomic::Ordering::SeqCst)).filter(|&t| t != 0),
        _ => None,
    }
}

extern "win64" fn QueueUserAPC(func: u64, thread: u64, arg: u64) -> u32 {
    if func == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(tid) = thread_id(thread) else { return fail(ERROR_INVALID_HANDLE, 0) };
    queue(tid, Apc::User { func, arg });
    1
}

const ERROR_HANDLE_EOF: u32 = 38;

/// ReadFileEx/WriteFileEx: the transfer runs as an overlapped one; its
/// completion routine is queued to this thread when it finishes.
fn file_ex(op: impl FnOnce() -> i32, ov: u64, routine: u64) -> i32 {
    if ov == 0 || routine == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    register(ov, routine);
    if op() != 0 {
        // Finished at once (a disk file): the routine still runs only in
        // an alertable wait. A pipe that completed here queued it already.
        // SAFETY: the caller's OVERLAPPED.
        let (status, bytes) = unsafe { (crate::kernel32::overlapped::status(ov), crate::kernel32::overlapped::bytes(ov)) };
        completed(ov, if status == 0 { 0 } else { crate::pipes::error_of(status) }, bytes as u32);
        return 1;
    }
    match crate::teb::last_error() {
        // Queued; the pipe's helper reports the completion.
        997 => {
            crate::teb::set_last_error(0);
            1
        }
        // A message longer than the buffer: the pipe queued the routine
        // with ERROR_MORE_DATA.
        234 => 1,
        // Reading at end of file completes with ERROR_HANDLE_EOF.
        ERROR_HANDLE_EOF => {
            completed(ov, ERROR_HANDLE_EOF, 0);
            1
        }
        e => {
            ROUTINES.lock().unwrap().remove(&ov);
            fail(e, 0)
        }
    }
}

extern "win64" fn ReadFileEx(h: u64, buf: *mut u8, n: u32, ov: *mut u8, routine: u64) -> i32 {
    file_ex(|| crate::kernel32::ReadFile(h, buf, n, core::ptr::null_mut(), ov), ov as u64, routine)
}

extern "win64" fn WriteFileEx(h: u64, buf: *const u8, n: u32, ov: *mut u8, routine: u64) -> i32 {
    file_ex(|| crate::kernel32::WriteFile(h, buf, n, core::ptr::null_mut(), ov), ov as u64, routine)
}

pub fn exports() -> alloc::vec::Vec<(&'static str, u64)> {
    export_table![QueueUserAPC, ReadFileEx, WriteFileEx]
}
