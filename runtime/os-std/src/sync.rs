//! Synchronisation: futex-based `Mutex` and `Condvar`, plus re-exports of
//! `Arc`, `Weak` and the atomics.
//!
//! The API mirrors `std::sync`. Because a panic ends the whole process,
//! locks are never poisoned: `lock()` returns `LockResult` for source
//! compatibility, but it is always `Ok`.

use crate::sys;
use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

pub use alloc::sync::{Arc, Weak};
pub use core::sync::atomic;

/// Never constructed (see the module docs); kept for `std` compatibility.
pub struct PoisonError<G> {
    guard: G,
}

impl<G> PoisonError<G> {
    pub fn into_inner(self) -> G {
        self.guard
    }
}

impl<G> fmt::Debug for PoisonError<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PoisonError")
    }
}

pub type LockResult<G> = Result<G, PoisonError<G>>;

#[derive(Debug)]
pub enum TryLockError<G> {
    Poisoned(PoisonError<G>),
    WouldBlock,
}

pub type TryLockResult<G> = Result<G, TryLockError<G>>;

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
/// Locked, and at least one thread may be sleeping on the futex.
const CONTENDED: u32 = 2;

/// A mutual exclusion lock. Uncontended lock/unlock are a single atomic
/// operation; waiters sleep in the kernel (`futex_wait`) instead of spinning.
pub struct Mutex<T: ?Sized> {
    futex: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by the lock.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

/// RAII guard; the lock is released when it is dropped.
pub struct MutexGuard<'a, T: ?Sized + 'a> {
    lock: &'a Mutex<T>,
    _not_send: PhantomData<*const ()>,
}

// SAFETY: sharing the guard only shares `&T`.
unsafe impl<T: ?Sized + Sync> Sync for MutexGuard<'_, T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex { futex: AtomicU32::new(UNLOCKED), data: UnsafeCell::new(value) }
    }

    pub fn into_inner(self) -> LockResult<T> {
        Ok(self.data.into_inner())
    }
}

impl<T: ?Sized> Mutex<T> {
    fn raw_lock(&self) {
        if self.futex.compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            return;
        }
        // Brief spin: the holder is often about to release the lock.
        for _ in 0..100 {
            core::hint::spin_loop();
            if self.futex.load(Ordering::Relaxed) == UNLOCKED
                && self.futex.compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed).is_ok()
            {
                return;
            }
        }
        // Mark the lock contended so the holder wakes someone on unlock.
        while self.futex.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
            sys::futex_wait(&self.futex, CONTENDED, None);
        }
    }

    fn raw_unlock(&self) {
        if self.futex.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            sys::futex_wake(&self.futex, 1);
        }
    }

    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        self.raw_lock();
        Ok(MutexGuard { lock: self, _not_send: PhantomData })
    }

    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
        match self.futex.compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => Ok(MutexGuard { lock: self, _not_send: PhantomData }),
            Err(_) => Err(TryLockError::WouldBlock),
        }
    }

    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        Ok(self.data.get_mut())
    }

    pub fn is_poisoned(&self) -> bool {
        false
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Mutex<T> {
        Mutex::new(T::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Ok(g) => f.debug_struct("Mutex").field("data", &&*g).finish(),
            Err(_) => f.write_str("Mutex { <locked> }"),
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.raw_unlock();
    }
}

/// Result of [`Condvar::wait_timeout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    pub fn timed_out(&self) -> bool {
        self.0
    }
}

/// A condition variable. `notify_*` bump a sequence number and wake futex
/// waiters; a waiter that saw an older number returns at once, so
/// notifications sent between unlocking the mutex and sleeping are not lost.
/// Spurious wake-ups are possible, as with `std`: use `wait_while`.
pub struct Condvar {
    seq: AtomicU32,
}

impl Default for Condvar {
    fn default() -> Condvar {
        Condvar::new()
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Condvar")
    }
}

impl Condvar {
    pub const fn new() -> Condvar {
        Condvar { seq: AtomicU32::new(0) }
    }

    fn wait_inner<'a, T: ?Sized>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Option<Duration>,
    ) -> (MutexGuard<'a, T>, bool) {
        let lock = guard.lock;
        let seen = self.seq.load(Ordering::Relaxed);
        drop(guard);
        let woken = sys::futex_wait(&self.seq, seen, timeout);
        lock.raw_lock();
        (MutexGuard { lock, _not_send: PhantomData }, !woken)
    }

    pub fn wait<'a, T: ?Sized>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        Ok(self.wait_inner(guard, None).0)
    }

    pub fn wait_while<'a, T: ?Sized, F: FnMut(&mut T) -> bool>(
        &self,
        mut guard: MutexGuard<'a, T>,
        mut condition: F,
    ) -> LockResult<MutexGuard<'a, T>> {
        while condition(&mut *guard) {
            guard = self.wait_inner(guard, None).0;
        }
        Ok(guard)
    }

    pub fn wait_timeout<'a, T: ?Sized>(
        &self,
        guard: MutexGuard<'a, T>,
        dur: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        let (g, timed_out) = self.wait_inner(guard, Some(dur));
        Ok((g, WaitTimeoutResult(timed_out)))
    }

    pub fn notify_one(&self) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        sys::futex_wake(&self.seq, 1);
    }

    pub fn notify_all(&self) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        sys::futex_wake(&self.seq, u32::MAX);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::thread;
    use crate::time::Instant;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    #[test]
    fn mutex_counts_correctly_under_contention() {
        let m = Arc::new(Mutex::new(0u64));
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let m = m.clone();
                thread::spawn(move || {
                    for _ in 0..20_000 {
                        *m.lock().unwrap() += 1;
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(*m.lock().unwrap(), 160_000);
    }

    #[test]
    fn try_lock_and_into_inner() {
        let m = Mutex::new(5);
        let g = m.lock().unwrap();
        assert!(matches!(m.try_lock(), Err(TryLockError::WouldBlock)));
        drop(g);
        assert_eq!(*m.try_lock().ok().unwrap(), 5);
        assert_eq!(m.into_inner().ok(), Some(5));
    }

    #[test]
    fn condvar_producer_consumer() {
        let q = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let consumers: Vec<_> = (0..3)
            .map(|_| {
                let q = q.clone();
                thread::spawn(move || {
                    let mut sum = 0u64;
                    loop {
                        let (m, cv) = &*q;
                        let mut g = cv.wait_while(m.lock().unwrap(), |q: &mut VecDeque<u64>| q.is_empty()).unwrap();
                        match g.pop_front() {
                            Some(0) => return sum,
                            Some(v) => sum += v,
                            None => unreachable!(),
                        }
                    }
                })
            })
            .collect();
        {
            let (m, cv) = &*q;
            for v in 1..=1000u64 {
                m.lock().unwrap().push_back(v);
                cv.notify_one();
            }
            for _ in 0..3 {
                m.lock().unwrap().push_back(0);
            }
            cv.notify_all();
        }
        let total: u64 = consumers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 500_500);
    }

    #[test]
    fn condvar_wait_timeout_expires() {
        let m = Mutex::new(());
        let cv = Condvar::new();
        let start = Instant::now();
        let (_g, r) = cv.wait_timeout(m.lock().unwrap(), Duration::from_millis(30)).unwrap();
        assert!(r.timed_out());
        assert!(start.elapsed() >= Duration::from_millis(30));
    }
}
