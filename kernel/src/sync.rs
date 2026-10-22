//! Spinlocks.
//!
//! Locking rules (single CPU for now, but written to stay correct on SMP):
//! * Data that is ever touched from an interrupt or exception handler must be
//!   protected by [`IrqSpinLock`], which disables interrupts while held. A
//!   plain [`SpinLock`] taken in thread context could otherwise be re-taken by
//!   an interrupt handler on the same CPU and deadlock.
//! * Lock order (outer to inner):
//!   CURRENT (process) -> OUTPUT_TAIL -> HEAP -> PAGING -> FRAMES -> SERIAL.
//!   Never acquire an outer lock while holding an inner one.
//! * The panic path may forcibly unlock SERIAL / DISPLAY (see `force_unlock`),
//!   because the panicking context might be the holder.

use crate::arch::cpu;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `locked`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> SpinLock<T> {
    pub const fn new(v: T) -> Self {
        SpinLock { locked: AtomicBool::new(false), data: UnsafeCell::new(v) }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinGuard { lock: self }
    }

    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SpinGuard { lock: self })
    }

    /// # Safety
    /// Only for the panic path: the previous holder must never run again.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard proves exclusive ownership.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves exclusive ownership.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

/// A spinlock that keeps local interrupts disabled while held.
pub struct IrqSpinLock<T> {
    inner: SpinLock<T>,
}

pub struct IrqGuard<'a, T> {
    guard: Option<SpinGuard<'a, T>>,
    were_enabled: bool,
}

impl<T> IrqSpinLock<T> {
    pub const fn new(v: T) -> Self {
        IrqSpinLock { inner: SpinLock::new(v) }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let were_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        IrqGuard { guard: Some(self.inner.lock()), were_enabled }
    }

    pub fn try_lock(&self) -> Option<IrqGuard<'_, T>> {
        let were_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        match self.inner.try_lock() {
            Some(g) => Some(IrqGuard { guard: Some(g), were_enabled }),
            None => {
                if were_enabled {
                    cpu::enable_interrupts();
                }
                None
            }
        }
    }

    /// # Safety
    /// See [`SpinLock::force_unlock`].
    pub unsafe fn force_unlock(&self) {
        self.inner.force_unlock();
    }
}

impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().expect("guard alive")
    }
}

impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().expect("guard alive")
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        // Release the lock before re-enabling interrupts.
        self.guard.take();
        if self.were_enabled {
            cpu::enable_interrupts();
        }
    }
}

/// A value written once during single-threaded early boot and read-only after.
pub struct BootCell<T> {
    value: UnsafeCell<Option<T>>,
    set: AtomicBool,
}

// SAFETY: written once before any concurrency exists, immutable afterwards.
unsafe impl<T: Sync> Sync for BootCell<T> {}

impl<T> BootCell<T> {
    pub const fn new() -> Self {
        BootCell { value: UnsafeCell::new(None), set: AtomicBool::new(false) }
    }

    /// # Safety
    /// Must be called at most once, before any reader can run concurrently.
    pub unsafe fn init(&self, v: T) -> &T {
        assert!(!self.set.swap(true, Ordering::AcqRel), "BootCell initialised twice");
        *self.value.get() = Some(v);
        self.get().expect("just set")
    }

    pub fn get(&self) -> Option<&T> {
        if self.set.load(Ordering::Acquire) {
            // SAFETY: never mutated after `init`.
            unsafe { (*self.value.get()).as_ref() }
        } else {
            None
        }
    }
}
