//! Thread control blocks and thread-local storage.
//!
//! Each thread's FS base points to its `Tcb`, whose first word holds its own
//! address (the x86-64 TLS convention, so `mov rax, fs:[0]` yields the TCB).
//! `thread_local!` keys get a process-wide slot index on first use; each
//! thread lazily allocates its value for a slot and registers a destructor
//! that runs when the thread ends.
//!
//! The compiler's own `#[thread_local]` / ELF TLS is not available to stable
//! `no_std` code, so OSyx Std implements TLS itself on top of the FS base.

use crate::sys;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

/// A thread-local value and the function that destroys it.
type Destructor = (*mut u8, unsafe fn(*mut u8));

#[repr(C)]
pub(crate) struct Tcb {
    /// Must stay the first field: `fs:[0]`.
    self_ptr: *mut Tcb,
    tid: u64,
    pub(crate) name: Option<String>,
    slots: Vec<*mut u8>,
    dtors: Vec<Destructor>,
}

/// Allocates a TCB for a thread that is about to be created.
pub(crate) fn new_tcb(name: Option<String>) -> *mut Tcb {
    let tcb = Box::into_raw(Box::new(Tcb {
        self_ptr: core::ptr::null_mut(),
        tid: 0,
        name,
        slots: Vec::new(),
        dtors: Vec::new(),
    }));
    // SAFETY: just allocated.
    unsafe { (*tcb).self_ptr = tcb };
    tcb
}

/// Frees a TCB whose thread was never started.
///
/// # Safety
/// `tcb` must come from [`new_tcb`] and not be installed in any thread.
pub(crate) unsafe fn free_unused_tcb(tcb: *mut Tcb) {
    drop(Box::from_raw(tcb));
}

/// Installs the main thread's TCB (called once by the OSyx start-up code).
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub(crate) fn init_main_thread() {
    let tcb = new_tcb(Some(String::from("main")));
    sys::set_tcb(tcb as usize);
    // Fills `fs:[8]` eagerly: `os_sys::gettid` reads the id from there. The
    // TCB must be installed first, because reading `fs:` faults without it.
    unsafe { (*tcb).tid = sys::current_tid() };
}

/// Fills the calling thread's cached id (a spawned thread does this once on
/// entry, so later `gettid` calls need no system call).
pub(crate) fn init_current_tid() {
    let tcb = current();
    // SAFETY: the current TCB is only accessed by its own thread.
    unsafe {
        if (*tcb).tid == 0 {
            (*tcb).tid = sys::current_tid();
        }
    }
}

/// The current thread's TCB. On the host back end a TCB is created on first
/// use for threads not started by OSyx Std (such as the test harness).
pub(crate) fn current() -> *mut Tcb {
    let p = sys::tcb();
    if p != 0 {
        return p as *mut Tcb;
    }
    let tcb = new_tcb(None);
    sys::set_tcb(tcb as usize);
    tcb
}

pub(crate) fn current_tid() -> u64 {
    let tcb = current();
    // SAFETY: the current TCB is only accessed by its own thread.
    unsafe {
        if (*tcb).tid == 0 {
            (*tcb).tid = sys::current_tid();
        }
        (*tcb).tid
    }
}

/// Runs the TLS destructors of the current thread (newest first) and frees
/// its TCB. Nothing may use TLS on this thread afterwards.
pub(crate) fn destroy_current() {
    let tcb = current();
    // SAFETY: owned by this thread; destructors may use other keys, which
    // can push new entries, so drain until empty.
    unsafe {
        while let Some((p, d)) = (*tcb).dtors.pop() {
            d(p);
        }
        sys::set_tcb(0);
        drop(Box::from_raw(tcb));
    }
}

static NEXT_KEY: AtomicUsize = AtomicUsize::new(1);

/// A thread-local value, created per thread on first access. Declare with
/// [`thread_local!`](crate::thread_local).
pub struct LocalKey<T: 'static> {
    /// Slot index + 1; 0 = not assigned yet.
    index: AtomicUsize,
    init: fn() -> T,
}

// SAFETY: the key itself only holds an atomic index and a function pointer;
// values are per thread and never shared through it.
unsafe impl<T: 'static> Sync for LocalKey<T> {}

unsafe fn drop_box<T>(p: *mut u8) {
    drop(Box::from_raw(p as *mut T));
}

impl<T: 'static> LocalKey<T> {
    #[doc(hidden)]
    pub const fn new(init: fn() -> T) -> LocalKey<T> {
        LocalKey { index: AtomicUsize::new(0), init }
    }

    fn slot(&self) -> usize {
        let i = self.index.load(Ordering::Acquire);
        if i != 0 {
            return i - 1;
        }
        let fresh = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        match self.index.compare_exchange(0, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh - 1,
            Err(existing) => existing - 1,
        }
    }

    /// Calls `f` with this thread's value, initialising it on first use.
    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        let slot = self.slot();
        let tcb = current();
        // SAFETY: the TCB belongs to this thread. The `&mut Tcb` borrows end
        // before `init` and `f` run (they may use other keys); values are
        // boxed, so their addresses stay valid even if `slots` reallocates.
        unsafe {
            let existing = {
                let t = &mut *tcb;
                if t.slots.len() <= slot {
                    t.slots.resize(slot + 1, core::ptr::null_mut());
                }
                t.slots[slot]
            };
            let p = if existing.is_null() {
                let p = Box::into_raw(Box::new((self.init)())) as *mut u8;
                let t = &mut *tcb;
                t.slots[slot] = p;
                t.dtors.push((p, drop_box::<T>));
                p
            } else {
                existing
            };
            f(&*(p as *const T))
        }
    }
}

/// Declares thread-local statics, like `std::thread_local!`:
///
/// ```ignore
/// os_std::thread_local! {
///     static COUNTER: core::cell::Cell<u32> = core::cell::Cell::new(0);
/// }
/// COUNTER.with(|c| c.set(c.get() + 1));
/// ```
#[macro_export]
macro_rules! thread_local {
    () => {};
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => {
        $(#[$attr])* $vis static $name: $crate::thread::LocalKey<$t> =
            $crate::thread::LocalKey::new({ fn __init() -> $t { $init } __init });
        $crate::thread_local!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => {
        $(#[$attr])* $vis static $name: $crate::thread::LocalKey<$t> =
            $crate::thread::LocalKey::new({ fn __init() -> $t { $init } __init });
    };
}
