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
extern "win64" fn InitOnceInitialize(once: u64) {
    unsafe { &*(once as *const AtomicU64) }.store(0, Ordering::Relaxed);
}
extern "win64" fn InitOnceExecuteOnce(once: u64, callback: u64, parameter: u64, context: *mut u64) -> i32 {
    if once == 0 || !once.is_multiple_of(8) || callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let state = unsafe { &*(once as *const AtomicU64) };
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
extern "win64" fn InitializeConditionVariable(cv: u64) {
    word(cv).store(0, Ordering::Relaxed);
}
extern "win64" fn WakeConditionVariable(cv: u64) {
    word(cv).fetch_add(1, Ordering::Release);
    os_sys::futex_wake(word(cv).as_ptr(), 1);
}
extern "win64" fn WakeAllConditionVariable(cv: u64) {
    word(cv).fetch_add(1, Ordering::Release);
    os_sys::futex_wake(word(cv).as_ptr(), u32::MAX);
}
extern "win64" fn SleepConditionVariableSRW(cv: u64, lock: u64, ms: u32, flags: u32) -> i32 {
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
extern "win64" fn WaitOnAddress(address: u64, compare: *const u32, size: usize, ms: u32) -> i32 {
    if address == 0 || compare.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if size != 4 || !address.is_multiple_of(4) {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    let expected = unsafe { core::ptr::read_unaligned(compare) };
    if word(address).load(Ordering::Acquire) != expected {
        return 1;
    }
    wait(address, expected, ms)
}
extern "win64" fn WakeByAddressSingle(address: u64) {
    if address != 0 && address.is_multiple_of(4) {
        os_sys::futex_wake(word(address).as_ptr(), 1);
    }
}
extern "win64" fn WakeByAddressAll(address: u64) {
    if address != 0 && address.is_multiple_of(4) {
        os_sys::futex_wake(word(address).as_ptr(), u32::MAX);
    }
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        InitOnceInitialize,
        InitOnceExecuteOnce,
        InitializeConditionVariable,
        WakeConditionVariable,
        WakeAllConditionVariable,
        SleepConditionVariableSRW,
        WaitOnAddress,
        WakeByAddressSingle,
        WakeByAddressAll
    ]
}
