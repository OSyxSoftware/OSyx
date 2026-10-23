//! The default thread pool (work, timer and wait objects, registered
//! waits) and I/O completion ports used as queues.
//!
//! Callbacks run on pool workers, which are Windows threads (TEB, TLS,
//! DLL notifications) started on demand while every worker is busy, up to
//! MAX_WORKERS. Idle workers sleep on a futex until a job is queued or
//! the nearest timer is due. Waits block in a helper thread on the object
//! and a private cancel event, then queue their callback. Custom callback
//! environments and pools, and associating files with completion ports
//! (files complete synchronously; pipes are the handles with overlapped
//! I/O and can be associated), fail with ERROR_NOT_SUPPORTED.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    objects::{self, EventObj, Object},
};
use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use os_std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_WORKERS: usize = 32;
const WAIT_TIMEOUT: u32 = 0x102;
const INVALID_HANDLE_VALUE: u64 = u64::MAX;
const ERROR_IO_PENDING: u32 = 997;

/// Outstanding (queued or running) callbacks of one pool object, with a
/// futex word to wait for them.
#[derive(Default)]
struct Pending(AtomicU32);

impl Pending {
    fn add(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn done(&self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        os_sys::futex_wake(self.0.as_ptr(), u32::MAX);
    }
    fn wait_idle(&self) {
        loop {
            let n = self.0.load(Ordering::SeqCst);
            if n == 0 {
                return;
            }
            let _ = os_sys::futex_wait(self.0.as_ptr(), n, 0);
        }
    }
}

struct Work {
    callback: u64,
    context: u64,
    pending: Pending,
}

struct Timer {
    callback: u64,
    context: u64,
    /// Next due time and period.
    due: Mutex<Option<(Instant, Duration)>>,
    pending: Pending,
}

struct Wait {
    callback: u64,
    context: u64,
    /// Cancel event of the current helper thread (0: none).
    cancel: AtomicU64,
    generation: AtomicU64,
    pending: Pending,
}

/// RegisterWaitForSingleObject.
struct Registered {
    callback: u64,
    context: u64,
    cancel: u64,
    stopped: AtomicBool,
    pending: Pending,
}

enum Job {
    Work(Arc<Work>),
    Timer(Arc<Timer>),
    Wait(Arc<Wait>, u32),
    Registered(Arc<Registered>, bool),
}

struct Pool {
    jobs: VecDeque<Job>,
    timers: Vec<Arc<Timer>>,
    idle: usize,
    workers: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { jobs: VecDeque::new(), timers: Vec::new(), idle: 0, workers: 0 });
static WAKE: AtomicU32 = AtomicU32::new(0);

fn wake() {
    WAKE.fetch_add(1, Ordering::SeqCst);
    os_sys::futex_wake(WAKE.as_ptr(), 1);
}

/// Makes sure a worker will pick up new work (or watch the timers).
fn ensure_worker(p: &mut Pool) {
    if p.idle == 0 && p.workers < MAX_WORKERS && kernel32::spawn_thread(worker as *const () as u64, 0, kernel32::stack_reserve(0, false), false).is_ok() {
        p.workers += 1;
    }
}

fn enqueue(job: Job) {
    let mut p = POOL.lock().unwrap();
    p.jobs.push_back(job);
    ensure_worker(&mut p);
    drop(p);
    wake();
}

/// PTP_CALLBACK_INSTANCE: what to do when the callback returns.
#[derive(Default)]
struct Instance {
    free_library: u64,
}

fn run(job: Job) {
    let mut instance = Instance::default();
    let inst = &mut instance as *mut Instance as u64;
    type Simple = extern "win64" fn(u64, u64, u64);
    type WaitCb = extern "win64" fn(u64, u64, u64, u32);
    type Legacy = extern "win64" fn(u64, u8);
    // SAFETY: callbacks registered with the matching Create* call.
    unsafe {
        match &job {
            Job::Work(w) => core::mem::transmute::<usize, Simple>(w.callback as usize)(inst, w.context, Arc::as_ptr(w) as u64),
            Job::Timer(t) => core::mem::transmute::<usize, Simple>(t.callback as usize)(inst, t.context, Arc::as_ptr(t) as u64),
            Job::Wait(w, r) => core::mem::transmute::<usize, WaitCb>(w.callback as usize)(inst, w.context, Arc::as_ptr(w) as u64, *r),
            Job::Registered(r, timed_out) => core::mem::transmute::<usize, Legacy>(r.callback as usize)(r.context, u8::from(*timed_out)),
        }
    }
    if instance.free_library != 0 {
        let _ = crate::loader::free_library(instance.free_library);
    }
    match job {
        Job::Work(w) => w.pending.done(),
        Job::Timer(t) => t.pending.done(),
        Job::Wait(w, _) => w.pending.done(),
        Job::Registered(r, _) => r.pending.done(),
    }
}

extern "win64" fn worker(_: u64) -> u32 {
    loop {
        let job = {
            let mut p = POOL.lock().unwrap();
            let now = Instant::now();
            let mut next: Option<Instant> = None;
            let timers = p.timers.clone();
            for t in timers {
                let mut due = t.due.lock().unwrap();
                if let Some((at, period)) = *due {
                    if at <= now {
                        t.pending.add();
                        p.jobs.push_back(Job::Timer(t.clone()));
                        *due = if period.is_zero() { None } else { Some((now + period, period)) };
                    }
                    if let Some((at, _)) = *due {
                        next = Some(next.map_or(at, |n| n.min(at)));
                    }
                }
            }
            p.timers.retain(|t| t.due.lock().unwrap().is_some());
            match p.jobs.pop_front() {
                Some(j) => {
                    if !p.jobs.is_empty() {
                        ensure_worker(&mut p);
                        wake();
                    }
                    Some(j)
                }
                None => {
                    p.idle += 1;
                    let seq = WAKE.load(Ordering::SeqCst);
                    drop(p);
                    let timeout = next.map_or(0, |n| n.duration_since(Instant::now()).as_nanos().max(1) as u64);
                    let _ = os_sys::futex_wait(WAKE.as_ptr(), seq, timeout);
                    POOL.lock().unwrap().idle -= 1;
                    None
                }
            }
        };
        if let Some(j) = job {
            run(j);
        }
    }
}

fn environment_ok(env: u64) -> bool {
    env == 0
}

// ---- work ------------------------------------------------------------------------

extern "win64" fn CreateThreadpoolWork(callback: u64, context: u64, env: u64) -> u64 {
    if callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !environment_ok(env) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    Arc::into_raw(Arc::new(Work { callback, context, pending: Pending::default() })) as u64
}

/// # Safety
/// `p` came from Arc::into_raw of a T and is still alive.
unsafe fn borrow<T>(p: u64) -> Arc<T> {
    // SAFETY: the caller's guarantee; the count is raised for the clone.
    unsafe {
        Arc::increment_strong_count(p as *const T);
        Arc::from_raw(p as *const T)
    }
}

extern "win64" fn SubmitThreadpoolWork(work: u64) {
    // SAFETY: a PTP_WORK from CreateThreadpoolWork.
    let w = unsafe { borrow::<Work>(work) };
    w.pending.add();
    enqueue(Job::Work(w));
}

extern "win64" fn WaitForThreadpoolWorkCallbacks(work: u64, cancel: i32) {
    // SAFETY: as above.
    let w = unsafe { borrow::<Work>(work) };
    if cancel != 0 {
        let mut p = POOL.lock().unwrap();
        let before = p.jobs.len();
        p.jobs.retain(|j| !matches!(j, Job::Work(x) if Arc::ptr_eq(x, &w)));
        for _ in 0..before - p.jobs.len() {
            w.pending.done();
        }
    }
    w.pending.wait_idle();
}

extern "win64" fn CloseThreadpoolWork(work: u64) {
    // SAFETY: releases the reference CreateThreadpoolWork returned;
    // queued jobs hold their own.
    drop(unsafe { Arc::from_raw(work as *const Work) });
}

// ---- timers ----------------------------------------------------------------------

extern "win64" fn CreateThreadpoolTimer(callback: u64, context: u64, env: u64) -> u64 {
    if callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !environment_ok(env) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    Arc::into_raw(Arc::new(Timer { callback, context, due: Mutex::new(None), pending: Pending::default() })) as u64
}

/// FILETIME due time: negative relative, positive absolute (100 ns).
fn deadline(ft: i64) -> Instant {
    let units = if ft < 0 {
        ft.unsigned_abs()
    } else {
        let now = kernel32::filetime(os_sys::clock(osyx_abi::CLOCK_REALTIME).unwrap_or(0) as i64);
        (ft as u64).saturating_sub(now)
    };
    Instant::now() + Duration::from_nanos(units.saturating_mul(100))
}

extern "win64" fn SetThreadpoolTimer(timer: u64, due: *const i64, period: u32, _window: u32) {
    // SAFETY: a PTP_TIMER from CreateThreadpoolTimer.
    let t = unsafe { borrow::<Timer>(timer) };
    // SAFETY: NULL or the caller's FILETIME.
    let when = (!due.is_null()).then(|| deadline(unsafe { core::ptr::read_unaligned(due) }));
    *t.due.lock().unwrap() = when.map(|w| (w, Duration::from_millis(period as u64)));
    if when.is_some() {
        let mut p = POOL.lock().unwrap();
        if !p.timers.iter().any(|x| Arc::ptr_eq(x, &t)) {
            p.timers.push(t.clone());
        }
        ensure_worker(&mut p);
        drop(p);
        wake();
    }
}

extern "win64" fn IsThreadpoolTimerSet(timer: u64) -> i32 {
    // SAFETY: as above.
    let t = unsafe { borrow::<Timer>(timer) };
    let set = t.due.lock().unwrap().is_some();
    i32::from(set)
}

extern "win64" fn WaitForThreadpoolTimerCallbacks(timer: u64, cancel: i32) {
    // SAFETY: as above.
    let t = unsafe { borrow::<Timer>(timer) };
    if cancel != 0 {
        *t.due.lock().unwrap() = None;
        let mut p = POOL.lock().unwrap();
        let before = p.jobs.len();
        p.jobs.retain(|j| !matches!(j, Job::Timer(x) if Arc::ptr_eq(x, &t)));
        for _ in 0..before - p.jobs.len() {
            t.pending.done();
        }
    }
    t.pending.wait_idle();
}

extern "win64" fn CloseThreadpoolTimer(timer: u64) {
    // SAFETY: as in CloseThreadpoolWork. A closed timer stops firing.
    let t = unsafe { Arc::from_raw(timer as *const Timer) };
    *t.due.lock().unwrap() = None;
}

// ---- waits -----------------------------------------------------------------------

fn new_cancel_event() -> u64 {
    objects::insert(Object::Event(Arc::new(EventObj { signaled: AtomicBool::new(false), manual: true, name: None::<String> })))
}

fn signal(h: u64) {
    if let Some(Object::Event(e)) = objects::get(h) {
        e.signaled.store(true, Ordering::SeqCst);
        objects::signal_all();
    }
}

extern "win64" fn CreateThreadpoolWait(callback: u64, context: u64, env: u64) -> u64 {
    if callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !environment_ok(env) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    Arc::into_raw(Arc::new(Wait { callback, context, cancel: AtomicU64::new(0), generation: AtomicU64::new(0), pending: Pending::default() }))
        as u64
}

fn stop_wait(w: &Wait) {
    w.generation.fetch_add(1, Ordering::SeqCst);
    let old = w.cancel.swap(0, Ordering::SeqCst);
    if old != 0 {
        signal(old);
    }
}

extern "win64" fn SetThreadpoolWait(wait: u64, handle: u64, timeout: *const i64) {
    // SAFETY: a PTP_WAIT from CreateThreadpoolWait.
    let w = unsafe { borrow::<Wait>(wait) };
    stop_wait(&w);
    if handle == 0 {
        return;
    }
    // SAFETY: NULL (infinite) or the caller's FILETIME.
    let ms = if timeout.is_null() {
        u32::MAX
    } else {
        let at = deadline(unsafe { core::ptr::read_unaligned(timeout) });
        at.duration_since(Instant::now()).as_millis().min(u32::MAX as u128 - 1) as u32
    };
    let cancel = new_cancel_event();
    w.cancel.store(cancel, Ordering::SeqCst);
    let generation = w.generation.load(Ordering::SeqCst);
    let _ = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
        let r = objects::wait(&[handle, cancel], false, ms);
        objects::close(cancel);
        let _ = w.cancel.compare_exchange(cancel, 0, Ordering::SeqCst, Ordering::SeqCst);
        let result = match r {
            Ok(0) => 0,
            Ok(WAIT_TIMEOUT) => WAIT_TIMEOUT,
            _ => return, // cancelled, or the handle went away
        };
        if w.generation.load(Ordering::SeqCst) == generation {
            w.pending.add();
            enqueue(Job::Wait(w, result));
        }
    });
}

extern "win64" fn WaitForThreadpoolWaitCallbacks(wait: u64, cancel: i32) {
    // SAFETY: as above.
    let w = unsafe { borrow::<Wait>(wait) };
    if cancel != 0 {
        stop_wait(&w);
        let mut p = POOL.lock().unwrap();
        let before = p.jobs.len();
        p.jobs.retain(|j| !matches!(j, Job::Wait(x, _) if Arc::ptr_eq(x, &w)));
        for _ in 0..before - p.jobs.len() {
            w.pending.done();
        }
    }
    w.pending.wait_idle();
}

extern "win64" fn CloseThreadpoolWait(wait: u64) {
    // SAFETY: as in CloseThreadpoolWork.
    let w = unsafe { Arc::from_raw(wait as *const Wait) };
    stop_wait(&w);
}

extern "win64" fn FreeLibraryWhenCallbackReturns(instance: u64, module: u64) {
    if instance != 0 {
        // SAFETY: the Instance passed to the running callback.
        unsafe { (*(instance as *mut Instance)).free_library = module };
    }
}

// ---- registered waits ------------------------------------------------------------

const WT_EXECUTEONLYONCE: u32 = 0x8;

extern "win64" fn RegisterWaitForSingleObject(out: *mut u64, handle: u64, callback: u64, context: u64, ms: u32, flags: u32) -> i32 {
    if out.is_null() || callback == 0 || objects::get(handle).is_none() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let r = Arc::new(Registered { callback, context, cancel: new_cancel_event(), stopped: AtomicBool::new(false), pending: Pending::default() });
    let me = r.clone();
    let once = flags & WT_EXECUTEONLYONCE != 0;
    let spawned = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || loop {
        let timed_out = match objects::wait(&[handle, me.cancel], false, ms) {
            Ok(0) => false,
            Ok(WAIT_TIMEOUT) => true,
            _ => return,
        };
        if me.stopped.load(Ordering::SeqCst) {
            return;
        }
        me.pending.add();
        enqueue(Job::Registered(me.clone(), timed_out));
        if once {
            return;
        }
        // A still-signaled manual-reset object would fire again at once;
        // Windows behaves the same way.
    });
    if spawned.is_err() {
        objects::close(r.cancel);
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = Arc::into_raw(r) as u64 };
    1
}

extern "win64" fn UnregisterWaitEx(wait: u64, completion: u64) -> i32 {
    if wait == 0 {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    // SAFETY: the handle RegisterWaitForSingleObject returned; consumed.
    let r = unsafe { Arc::from_raw(wait as *const Registered) };
    r.stopped.store(true, Ordering::SeqCst);
    signal(r.cancel);
    {
        let mut p = POOL.lock().unwrap();
        let before = p.jobs.len();
        p.jobs.retain(|j| !matches!(j, Job::Registered(x, _) if Arc::ptr_eq(x, &r)));
        for _ in 0..before - p.jobs.len() {
            r.pending.done();
        }
    }
    let cancel = r.cancel;
    match completion {
        INVALID_HANDLE_VALUE => {
            r.pending.wait_idle();
            objects::close(cancel);
            1
        }
        0 => {
            let busy = r.pending.0.load(Ordering::SeqCst) != 0;
            let _ = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
                r.pending.wait_idle();
                objects::close(cancel);
            });
            if busy { fail(ERROR_IO_PENDING, 0) } else { 1 }
        }
        event => {
            let _ = os_std::thread::Builder::new().stack_size(64 << 10).spawn(move || {
                r.pending.wait_idle();
                objects::close(cancel);
                signal(event);
            });
            1
        }
    }
}

extern "win64" fn UnregisterWait(wait: u64) -> i32 {
    UnregisterWaitEx(wait, 0)
}

// ---- I/O completion ports ---------------------------------------------------------

pub struct PortObj {
    /// (bytes, key, OVERLAPPED, Win32 error of the operation; 0 when it
    /// succeeded or the packet was posted).
    queue: Mutex<VecDeque<(u32, u64, u64, u32)>>,
    word: AtomicU32,
}

/// Pipes are the handles with overlapped I/O (pipes.rs): associating one
/// queues its completions to the port. Files complete synchronously and
/// cannot be associated.
extern "win64" fn CreateIoCompletionPort(file: u64, existing: u64, key: u64, _threads: u32) -> u64 {
    if file == INVALID_HANDLE_VALUE {
        if existing != 0 {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        return new_port();
    }
    let Some(Object::Pipe(p)) = objects::get(file) else {
        return fail(if objects::get(file).is_some() { ERROR_NOT_SUPPORTED } else { ERROR_INVALID_HANDLE }, 0);
    };
    let port = if existing == 0 {
        new_port()
    } else if matches!(objects::get(existing), Some(Object::Port(_))) {
        existing
    } else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    crate::pipes::associate(&p, port, key);
    port
}

fn new_port() -> u64 {
    objects::insert(Object::Port(Arc::new(PortObj { queue: Mutex::new(VecDeque::new()), word: AtomicU32::new(0) })))
}

/// Queues the completion of I/O on an associated handle, with its error.
pub fn post(port: u64, bytes: u32, key: u64, overlapped: u64, error: u32) {
    let Some(Object::Port(p)) = objects::get(port) else { return };
    p.queue.lock().unwrap().push_back((bytes, key, overlapped, error));
    p.word.fetch_add(1, Ordering::SeqCst);
    os_sys::futex_wake(p.word.as_ptr(), 1);
}

extern "win64" fn PostQueuedCompletionStatus(port: u64, bytes: u32, key: u64, overlapped: u64) -> i32 {
    let Some(Object::Port(p)) = objects::get(port) else { return fail(ERROR_INVALID_HANDLE, 0) };
    p.queue.lock().unwrap().push_back((bytes, key, overlapped, 0));
    p.word.fetch_add(1, Ordering::SeqCst);
    os_sys::futex_wake(p.word.as_ptr(), 1);
    1
}

extern "win64" fn GetQueuedCompletionStatus(port: u64, bytes: *mut u32, key: *mut u64, overlapped: *mut u64, ms: u32) -> i32 {
    let Some(Object::Port(p)) = objects::get(port) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if bytes.is_null() || key.is_null() || overlapped.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let end = (ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(ms as u64));
    loop {
        let seq = p.word.load(Ordering::SeqCst);
        if let Some((b, k, o, e)) = p.queue.lock().unwrap().pop_front() {
            // SAFETY: the caller's out pointers.
            unsafe {
                *bytes = b;
                *key = k;
                *overlapped = o;
            }
            // A failed operation dequeues with FALSE and its error.
            return if e == 0 { 1 } else { fail(e, 0) };
        }
        let timeout = match end {
            None => 0,
            Some(e) => {
                let left = e.duration_since(Instant::now());
                if left.is_zero() {
                    // SAFETY: as above.
                    unsafe { *overlapped = 0 };
                    return fail(WAIT_TIMEOUT, 0);
                }
                left.as_nanos() as u64
            }
        };
        let _ = os_sys::futex_wait(p.word.as_ptr(), seq, timeout);
    }
}

/// OVERLAPPED_ENTRY { lpCompletionKey, lpOverlapped, Internal,
/// dwNumberOfBytesTransferred }: waits for the first packet, then takes
/// what else is queued, up to `count`.
extern "win64" fn GetQueuedCompletionStatusEx(port: u64, entries: *mut u8, count: u32, removed: *mut u32, ms: u32, _alertable: i32) -> i32 {
    let Some(Object::Port(p)) = objects::get(port) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if entries.is_null() || count == 0 || removed.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let (mut b, mut k, mut o) = (0u32, 0u64, 0u64);
    if GetQueuedCompletionStatus(port, &mut b, &mut k, &mut o, ms) == 0 && o == 0 {
        return 0; // timed out (or the port went away)
    }
    let mut taken = alloc::vec![(b, k, o, 0u32)];
    {
        let mut q = p.queue.lock().unwrap();
        while taken.len() < count as usize {
            match q.pop_front() {
                Some(x) => taken.push(x),
                None => break,
            }
        }
    }
    for (i, (b, k, o, _)) in taken.iter().enumerate() {
        let e = entries as u64 + i as u64 * 32;
        // SAFETY: the caller's array of `count` entries.
        unsafe {
            core::ptr::write_unaligned(e as *mut u64, *k);
            core::ptr::write_unaligned((e + 8) as *mut u64, *o);
            core::ptr::write_unaligned((e + 16) as *mut u64, 0); // Internal: reserved
            core::ptr::write_unaligned((e + 24) as *mut u32, *b);
        }
    }
    // SAFETY: the caller's out count.
    unsafe { *removed = taken.len() as u32 };
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CreateThreadpoolWork,
        SubmitThreadpoolWork,
        WaitForThreadpoolWorkCallbacks,
        CloseThreadpoolWork,
        CreateThreadpoolTimer,
        SetThreadpoolTimer,
        IsThreadpoolTimerSet,
        WaitForThreadpoolTimerCallbacks,
        CloseThreadpoolTimer,
        CreateThreadpoolWait,
        SetThreadpoolWait,
        WaitForThreadpoolWaitCallbacks,
        CloseThreadpoolWait,
        FreeLibraryWhenCallbackReturns,
        RegisterWaitForSingleObject,
        UnregisterWaitEx,
        UnregisterWait,
        CreateIoCompletionPort,
        PostQueuedCompletionStatus,
        GetQueuedCompletionStatus,
        GetQueuedCompletionStatusEx,
    ]
}
