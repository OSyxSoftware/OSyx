//! Threads: `spawn`, `JoinHandle`, `current`, `sleep`, `yield_now` and
//! thread-local storage (`thread_local!` / [`LocalKey`]).
//!
//! A panic in any thread ends the whole process with status 101 (OSyx Std
//! uses `panic = abort`), so `join` never observes a panicked thread.
//! Returning from `main` ends the process, including all other threads.

use crate::error::{Error, ErrorKind, Result};
use crate::sync::Mutex;
use crate::{sys, tls};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

pub use crate::tls::LocalKey;

/// Blocks for at least `d` (OSyx timer resolution: 1 ms).
pub fn sleep(d: Duration) {
    let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    if ns > 0 {
        sys::sleep_ns(ns);
    }
}

/// Gives the rest of the current time slice to other threads.
pub fn yield_now() {
    sys::yield_now()
}

/// Kernel thread id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadId(u64);

impl ThreadId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// A handle to a thread (its id and name).
#[derive(Clone, Debug)]
pub struct Thread {
    id: ThreadId,
    name: Option<String>,
}

impl Thread {
    pub fn id(&self) -> ThreadId {
        self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// The calling thread.
pub fn current() -> Thread {
    let tcb = tls::current();
    // SAFETY: the TCB belongs to this thread.
    let name = unsafe { (*tcb).name.clone() };
    Thread { id: ThreadId(tls::current_tid()), name }
}

/// Shared between a thread and its `JoinHandle`. `done` is published by the
/// kernel (or host back end) only after the thread has left its stack.
struct Packet<T> {
    done: AtomicU32,
    result: UnsafeCell<Option<T>>,
}

// SAFETY: `result` is written by the thread before `done` becomes 1 and read
// by the joiner only after observing `done == 1` (Acquire).
unsafe impl<T: Send> Sync for Packet<T> {}

trait Finished: Send + Sync {
    fn finished(&self) -> bool;
}

impl<T: Send> Finished for Packet<T> {
    fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire) == 1
    }
}

/// Detached threads whose stacks are freed once they have finished.
static DETACHED: Mutex<Vec<(Arc<dyn Finished>, sys::RawThread)>> = Mutex::new(Vec::new());

fn reap_detached() {
    let mut list = DETACHED.lock().unwrap_or_else(|e| e.into_inner());
    let mut i = 0;
    while i < list.len() {
        if list[i].0.finished() {
            let (_, raw) = list.swap_remove(i);
            sys::thread_release(raw);
        } else {
            i += 1;
        }
    }
}

/// Number of detached threads whose resources are not yet reclaimed.
pub fn detached_pending() -> usize {
    reap_detached();
    DETACHED.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Owned permission to join a thread. Dropping it detaches the thread.
pub struct JoinHandle<T: Send + 'static> {
    packet: Arc<Packet<T>>,
    raw: Option<sys::RawThread>,
    thread: Thread,
}

impl<T: Send + 'static> JoinHandle<T> {
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    pub fn is_finished(&self) -> bool {
        self.packet.finished()
    }

    /// Waits for the thread to finish and returns its result.
    pub fn join(mut self) -> Result<T> {
        while self.packet.done.load(Ordering::Acquire) == 0 {
            sys::futex_wait(&self.packet.done, 0, None);
        }
        if let Some(raw) = self.raw.take() {
            sys::thread_release(raw);
        }
        // SAFETY: the thread finished (done == 1 with Acquire), so it no
        // longer touches the packet.
        unsafe { (*self.packet.result.get()).take() }
            .ok_or(Error::new(ErrorKind::Other, "thread finished without a result"))
    }
}

impl<T: Send + 'static> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            let packet: Arc<dyn Finished> = self.packet.clone();
            DETACHED.lock().unwrap_or_else(|e| e.into_inner()).push((packet, raw));
        }
    }
}

impl<T: Send + 'static> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle").field("thread", &self.thread).finish()
    }
}

/// Thread factory, for naming threads and choosing their stack size.
#[derive(Debug, Default)]
pub struct Builder {
    name: Option<String>,
    stack_size: Option<usize>,
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    pub fn name(mut self, name: String) -> Builder {
        self.name = Some(name);
        self
    }

    pub fn stack_size(mut self, size: usize) -> Builder {
        self.stack_size = Some(size);
        self
    }

    pub fn spawn<F, T>(self, f: F) -> Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        reap_detached();
        let packet = Arc::new(Packet { done: AtomicU32::new(0), result: UnsafeCell::new(None) });
        // The thread gets a raw pointer, not an Arc: the joiner (or the
        // detached list) keeps the packet alive until `done` is 1, and the
        // thread never touches it after storing the result.
        let out = SendPtr(Arc::as_ptr(&packet));
        let main: Box<dyn FnOnce() + Send> = Box::new(move || {
            let out = out;
            let r = f();
            // SAFETY: see above; written once, before `done` is published.
            unsafe { *(*out.0).result.get() = Some(r) };
        });
        let arg = Box::into_raw(Box::new(main)) as usize;
        let tcb = tls::new_tcb(self.name.clone());
        let stack = self.stack_size.unwrap_or(sys::DEFAULT_STACK);
        match sys::thread_spawn(thread_main, arg, tcb as usize, &packet.done, stack) {
            Ok(raw) => Ok(JoinHandle { packet, raw: Some(raw), thread: Thread { id: ThreadId(0), name: self.name } }),
            Err(e) => {
                // SAFETY: the thread was not created; we still own both.
                unsafe {
                    drop(Box::from_raw(arg as *mut Box<dyn FnOnce() + Send>));
                    tls::free_unused_tcb(tcb);
                }
                Err(e)
            }
        }
    }
}

struct SendPtr<T>(*const T);
// SAFETY: only used to hand the packet address to the new thread.
unsafe impl<T> Send for SendPtr<T> {}

/// Body of every spawned thread (runs on the new thread).
fn thread_main(arg: usize) {
    tls::init_current_tid();
    // SAFETY: `arg` was leaked by `Builder::spawn` for this thread.
    let f = unsafe { Box::from_raw(arg as *mut Box<dyn FnOnce() + Send>) };
    f();
    tls::destroy_current();
}

/// Spawns a thread running `f`. Panics if the thread cannot be created.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).expect("failed to spawn thread")
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::time::Instant;
    use core::cell::Cell;
    use core::sync::atomic::AtomicUsize;

    #[test]
    fn sleep_waits_at_least_the_duration() {
        let start = Instant::now();
        sleep(Duration::from_millis(20));
        assert!(start.elapsed() >= Duration::from_millis(20));
        yield_now();
    }

    #[test]
    fn spawn_and_join_returns_values() {
        let handles: Vec<JoinHandle<u64>> = (0..8u64).map(|i| spawn(move || (0..=i * 1000).sum::<u64>())).collect();
        let sums: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for (i, s) in sums.iter().enumerate() {
            let n = i as u64 * 1000;
            assert_eq!(*s, n * (n + 1) / 2);
        }
    }

    #[test]
    fn builder_name_and_current() {
        let h = Builder::new()
            .name(String::from("worker-7"))
            .spawn(|| (current().name().map(String::from), current().id()))
            .unwrap();
        let (name, id) = h.join().unwrap();
        assert_eq!(name.as_deref(), Some("worker-7"));
        assert_ne!(id, current().id());
    }

    crate::thread_local! {
        static LOCAL: Cell<u32> = Cell::new(7);
    }

    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct CountDrop;
    impl Drop for CountDrop {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    crate::thread_local! {
        static GUARD: CountDrop = CountDrop;
    }

    #[test]
    fn thread_locals_are_per_thread_and_destroyed() {
        LOCAL.with(|v| v.set(100));
        let before = DROPS.load(Ordering::SeqCst);
        let h = spawn(|| {
            GUARD.with(|_| ());
            LOCAL.with(|v| {
                let first = v.get();
                v.set(first + 1);
                (first, v.get())
            })
        });
        assert_eq!(h.join().unwrap(), (7, 8));
        assert_eq!(LOCAL.with(|v| v.get()), 100, "main thread's value unchanged");
        assert_eq!(DROPS.load(Ordering::SeqCst), before + 1, "TLS destructor ran at thread exit");
    }

    #[test]
    fn detached_threads_are_reaped() {
        let flag = Arc::new(AtomicU32::new(0));
        let f2 = flag.clone();
        drop(spawn(move || f2.store(1, Ordering::SeqCst)));
        let start = Instant::now();
        while detached_pending() > 0 && start.elapsed() < Duration::from_secs(5) {
            sleep(Duration::from_millis(5));
        }
        assert_eq!(flag.load(Ordering::SeqCst), 1);
    }
}
