//! Waitable timers participate in the same object waits as events/threads.
//! Deadlines use real OSyx clocks/futex timeouts; no background polling
//! thread or success stub. APC callbacks, wake-from-suspend and requested
//! high-resolution timers are not supported and fail explicitly.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    objects::{self, Object, TimerObj, TimerState},
    path, teb,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use os_std::{
    sync::Mutex,
    time::{Duration, Instant},
};
/// CREATE_WAITABLE_TIMER_MANUAL_RESET (1) and _HIGH_RESOLUTION (2): every
/// timer here has the kernel tick's resolution.
fn create(name: Option<String>, flags: u32) -> u64 {
    if flags & !3 != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    if let Some(old) = name.as_deref().and_then(objects::find_named) {
        return if matches!(old, Object::Timer(_)) {
            teb::set_last_error(ERROR_ALREADY_EXISTS);
            objects::insert(old)
        } else {
            fail(ERROR_INVALID_HANDLE, 0)
        };
    }
    teb::set_last_error(ERROR_SUCCESS);
    objects::insert(Object::Timer(Arc::new(TimerObj {
        manual: flags & 1 != 0,
        name,
        state: Mutex::new(TimerState { due: None, period: Duration::ZERO, signaled: false }),
    })))
}
extern "win64" fn CreateWaitableTimerExA(security: u64, name: *const u8, flags: u32, access: u32) -> u64 {
    if security != 0 || access != 0x1f0003 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    create(unsafe { path::ansi_to_string(name) }, flags)
}
extern "win64" fn CreateWaitableTimerExW(security: u64, name: *const u16, flags: u32, access: u32) -> u64 {
    if security != 0 || access != 0x1f0003 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    create(unsafe { path::wide_to_string(name) }, flags)
}
extern "win64" fn CreateWaitableTimerA(security: u64, manual: i32, name: *const u8) -> u64 {
    CreateWaitableTimerExA(security, name, u32::from(manual != 0), 0x1f0003)
}
extern "win64" fn CreateWaitableTimerW(security: u64, manual: i32, name: *const u16) -> u64 {
    CreateWaitableTimerExW(security, name, u32::from(manual != 0), 0x1f0003)
}
extern "win64" fn SetWaitableTimerEx(
    handle: u64,
    due: *const i64,
    period: i32,
    callback: u64,
    _arg: u64,
    wake_context: u64,
    _tolerance: u32,
) -> i32 {
    if due.is_null() || period < 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if callback != 0 || wake_context != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    let Some(Object::Timer(timer)) = objects::get(handle) else { return fail(ERROR_INVALID_HANDLE, 0) };
    let value = unsafe { core::ptr::read_unaligned(due) };
    let units = if value < 0 {
        value.unsigned_abs()
    } else {
        let Ok(now_ns) = os_sys::clock(osyx_abi::CLOCK_REALTIME) else {
            return fail(ERROR_GEN_FAILURE, 0);
        };
        let now = now_ns / 100 + 116_444_736_000_000_000;
        (value as u64).saturating_sub(now)
    };
    let Some(ns) = units.checked_mul(100) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let Some(deadline) = Instant::now().checked_add(Duration::from_nanos(ns)) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    {
        let mut state = timer.state.lock().unwrap();
        state.due = Some(deadline);
        state.period = Duration::from_millis(period as u64);
        state.signaled = false;
    }
    objects::signal_all();
    1
}
extern "win64" fn SetWaitableTimer(
    handle: u64,
    due: *const i64,
    period: i32,
    callback: u64,
    arg: u64,
    resume: i32,
) -> i32 {
    if resume != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    SetWaitableTimerEx(handle, due, period, callback, arg, 0, 0)
}
extern "win64" fn CancelWaitableTimer(handle: u64) -> i32 {
    let Some(Object::Timer(timer)) = objects::get(handle) else { return fail(ERROR_INVALID_HANDLE, 0) };
    // Cancellation preserves a signal whose deadline has already elapsed.
    timer.ready(false);
    timer.state.lock().unwrap().due = None;
    objects::signal_all();
    1
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CreateWaitableTimerExA,
        CreateWaitableTimerExW,
        CreateWaitableTimerA,
        CreateWaitableTimerW,
        SetWaitableTimerEx,
        SetWaitableTimer,
        CancelWaitableTimer
    ]
}
