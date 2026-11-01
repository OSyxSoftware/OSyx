//! Synchronous once initialization and condition variables backed by real
//! atomic state and kernel futexes. WaitOnAddress currently supports only
//! aligned 32-bit words; other sizes fail explicitly.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use osyx_abi::Errno;
fn word(p: u64) -> &'static AtomicU32 {
    unsafe { &*(p as *const AtomicU32) }
}
fn wait(p: u64, expected: u32, ms: u32) -> i32 {
    if ms == 0 {
        return fail(ERROR_TIMEOUT, 0);
    }
    let ns = if ms == u32::MAX { 0 } else { ms as u64 * 1_000_000 };
    match os_sys::futex_wait(word(p).as_ptr(), expected, ns) {
        Ok(()) | Err(Errno::WouldBlock) => 1,
        Err(e) => fail(errno_to_win32(e), 0),
    }
}
// INIT_ONCE: 0 not started, 1 synchronous initialization in progress,
// 3 asynchronous initialization(s) in progress, context | 2 done.
const ONCE_SYNC: u64 = 1;
const ONCE_DONE: u64 = 2;
const ONCE_ASYNC: u64 = 3;
const INIT_ONCE_CHECK_ONLY: u32 = 1;
const INIT_ONCE_ASYNC: u32 = 2;
const INIT_ONCE_INIT_FAILED: u32 = 4;
/// INIT_ONCE is pointer-sized: 8 bytes for a 64-bit caller, 4 for a
/// 32-bit (WoW64) one, whose context values are 32-bit too.
enum OnceState {
    Wide(&'static AtomicU64),
    Narrow(&'static AtomicU32),
}

impl OnceState {
    fn load(&self, order: Ordering) -> u64 {
        match self {
            OnceState::Wide(a) => a.load(order),
            OnceState::Narrow(a) => a.load(order) as u64,
        }
    }
    fn store(&self, v: u64, order: Ordering) {
        match self {
            OnceState::Wide(a) => a.store(v, order),
            OnceState::Narrow(a) => a.store(v as u32, order),
        }
    }
    fn compare_exchange(&self, old: u64, new: u64, success: Ordering, failure: Ordering) -> Result<u64, u64> {
        match self {
            OnceState::Wide(a) => a.compare_exchange(old, new, success, failure),
            OnceState::Narrow(a) => {
                a.compare_exchange(old as u32, new as u32, success, failure).map(u64::from).map_err(u64::from)
            }
        }
    }
}

fn once_state(once: u64) -> OnceState {
    // SAFETY: the caller's INIT_ONCE, aligned as `once_valid` checked.
    unsafe {
        if crate::wow64::is_active() {
            OnceState::Narrow(&*(once as *const AtomicU32))
        } else {
            OnceState::Wide(&*(once as *const AtomicU64))
        }
    }
}

/// A non-null INIT_ONCE aligned to its size.
fn once_valid(once: u64) -> bool {
    once != 0 && once.is_multiple_of(if crate::wow64::is_active() { 4 } else { 8 })
}
pub(crate) extern "win64" fn InitOnceInitialize(once: u64) {
    if once_valid(once) {
        once_state(once).store(0, Ordering::Relaxed);
    }
}
pub(crate) extern "win64" fn InitOnceExecuteOnce(once: u64, callback: u64, parameter: u64, context: *mut u64) -> i32 {
    if !once_valid(once) || callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let state = once_state(once);
    loop {
        let old = state.load(Ordering::Acquire);
        if old & 3 == 2 {
            if !context.is_null() {
                unsafe {
                    *context = old & !3;
                }
            }
            return 1;
        }
        if old == ONCE_ASYNC {
            return fail(ERROR_INVALID_PARAMETER, 0); // mixed with asynchronous initialization
        }
        if old == 0 && state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            let cb: extern "win64" fn(u64, u64, *mut u64) -> i32 = unsafe { core::mem::transmute(callback) };
            let mut value = 0u64;
            let ok = cb(once, parameter, &mut value);
            let valid = value & 3 == 0;
            state.store(if ok != 0 && valid { value | 2 } else { 0 }, Ordering::Release);
            os_sys::futex_wake(word(once).as_ptr(), u32::MAX);
            if ok == 0 {
                return 0;
            }
            if !valid {
                return fail(ERROR_INVALID_PARAMETER, 0);
            }
            if !context.is_null() {
                unsafe {
                    *context = value;
                }
            }
            return 1;
        }
        let _ = os_sys::futex_wait(word(once).as_ptr(), 1, 0);
    }
}
/// The caller initializes: a synchronous caller owns the object until
/// InitOnceComplete (others wait); asynchronous callers race and the first
/// InitOnceComplete wins. Error codes follow Windows.
extern "win64" fn InitOnceBeginInitialize(once: u64, flags: u32, pending: *mut i32, context: *mut u64) -> i32 {
    if !once_valid(once) || pending.is_null() || flags & !(INIT_ONCE_CHECK_ONLY | INIT_ONCE_ASYNC) != 0
        || flags == INIT_ONCE_CHECK_ONLY | INIT_ONCE_ASYNC
    {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let state = once_state(once);
    loop {
        let old = state.load(Ordering::Acquire);
        if old & 3 == ONCE_DONE {
            // SAFETY: the caller's out pointers.
            unsafe {
                *pending = 0;
                if !context.is_null() {
                    *context = old & !3;
                }
            }
            return 1;
        }
        if flags & INIT_ONCE_CHECK_ONLY != 0 {
            return fail(ERROR_GEN_FAILURE, 0);
        }
        let want = if flags & INIT_ONCE_ASYNC != 0 { ONCE_ASYNC } else { ONCE_SYNC };
        let start = old == 0 && state.compare_exchange(0, want, Ordering::Acquire, Ordering::Relaxed).is_ok();
        if start || (old == ONCE_ASYNC && want == ONCE_ASYNC) {
            // SAFETY: as above.
            unsafe { *pending = 1 };
            return 1;
        }
        if old == 0 {
            continue; // lost the race to start: look again
        }
        if want == ONCE_ASYNC || old == ONCE_ASYNC {
            return fail(ERROR_INVALID_PARAMETER, 0); // synchronous and asynchronous mixed
        }
        let _ = os_sys::futex_wait(word(once).as_ptr(), ONCE_SYNC as u32, 0);
    }
}
extern "win64" fn InitOnceComplete(once: u64, flags: u32, context: u64) -> i32 {
    let failed = flags & INIT_ONCE_INIT_FAILED != 0;
    let asynchronous = flags & INIT_ONCE_ASYNC != 0;
    if !once_valid(once) || flags & !(INIT_ONCE_ASYNC | INIT_ONCE_INIT_FAILED) != 0
        || (failed && (asynchronous || context != 0)) || context & 3 != 0
    {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let state = once_state(once);
    let new = if failed { 0 } else { context | ONCE_DONE };
    loop {
        let old = state.load(Ordering::Acquire);
        if old & 3 == ONCE_DONE {
            return fail(ERROR_GEN_FAILURE, 0); // already complete (an async race was lost)
        }
        let allowed = if asynchronous { old == 0 || old == ONCE_ASYNC } else { old == ONCE_SYNC };
        if !allowed {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        if state.compare_exchange(old, new, Ordering::Release, Ordering::Relaxed).is_ok() {
            os_sys::futex_wake(word(once).as_ptr(), u32::MAX);
            return 1;
        }
    }
}
extern "win64" fn InitializeConditionVariable(cv: u64) {
    word(cv).store(0, Ordering::Relaxed);
}
pub(crate) extern "win64" fn WakeConditionVariable(cv: u64) {
    word(cv).fetch_add(1, Ordering::Release);
    os_sys::futex_wake(word(cv).as_ptr(), 1);
}
pub(crate) extern "win64" fn WakeAllConditionVariable(cv: u64) {
    word(cv).fetch_add(1, Ordering::Release);
    os_sys::futex_wake(word(cv).as_ptr(), u32::MAX);
}
pub(crate) extern "win64" fn SleepConditionVariableSRW(cv: u64, lock: u64, ms: u32, flags: u32) -> i32 {
    if cv == 0 || lock == 0 || flags > 1 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let sequence = word(cv).load(Ordering::Acquire);
    if flags == 1 {
        kernel32::ReleaseSRWLockShared(lock);
    } else {
        kernel32::ReleaseSRWLockExclusive(lock);
    }
    let result = wait(cv, sequence, ms);
    if flags == 1 {
        kernel32::AcquireSRWLockShared(lock);
    } else {
        kernel32::AcquireSRWLockExclusive(lock);
    }
    result
}
pub(crate) extern "win64" fn SleepConditionVariableCS(cv: u64, cs: u64, ms: u32) -> i32 {
    if cv == 0 || cs == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let sequence = word(cv).load(Ordering::Acquire);
    kernel32::LeaveCriticalSection(cs);
    let result = wait(cv, sequence, ms);
    kernel32::EnterCriticalSection(cs);
    result
}
extern "win64" fn WaitOnAddress(address: u64, compare: *const u32, size: usize, ms: u32) -> i32 {
    if address == 0 || compare.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !matches!(size, 1 | 2 | 4 | 8) || !address.is_multiple_of(size as u64) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // Waiters sleep on the aligned 32-bit word holding the value (the low
    // one for 8 bytes); WakeByAddress* wakes the same word.
    let key = address & !3;
    let differs = || {
        // SAFETY: the caller's value and comparand of `size` bytes.
        unsafe {
            match size {
                1 => core::ptr::read_volatile(address as *const u8) != *(compare as *const u8),
                2 => core::ptr::read_volatile(address as *const u16) != core::ptr::read_unaligned(compare as *const u16),
                4 => word(address).load(Ordering::Acquire) != core::ptr::read_unaligned(compare),
                _ => core::ptr::read_volatile(address as *const u64) != core::ptr::read_unaligned(compare as *const u64),
            }
        }
    };
    let observed = word(key).load(Ordering::Acquire);
    if differs() {
        return 1;
    }
    let expected = observed;
    wait(key, expected, ms)
}
extern "win64" fn WakeByAddressSingle(address: u64) {
    if address != 0 {
        os_sys::futex_wake(word(address & !3).as_ptr(), 1);
    }
}
extern "win64" fn WakeByAddressAll(address: u64) {
    if address != 0 {
        os_sys::futex_wake(word(address & !3).as_ptr(), u32::MAX);
    }
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        InitOnceInitialize,
        InitOnceExecuteOnce,
        InitOnceBeginInitialize,
        InitOnceComplete,
        InitializeConditionVariable,
        WakeConditionVariable,
        WakeAllConditionVariable,
        SleepConditionVariableSRW,
        SleepConditionVariableCS,
        WaitOnAddress,
        WakeByAddressSingle,
        WakeByAddressAll
    ]
}
