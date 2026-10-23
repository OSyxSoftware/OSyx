//! winmm's multimedia timer functions. timeGetTime is the monotonic clock
//! in milliseconds. The OSyx kernel ticks at a fixed 100 Hz
//! (kernel/src/arch/pic.rs, TIMER_HZ), so waits have 10 ms resolution:
//! timeGetDevCaps reports that, timeBeginPeriod accepts the periods the
//! kernel already provides and refuses finer ones with TIMERR_NOCANDO
//! instead of claiming a resolution OSyx does not have.
#![allow(non_snake_case)]
use alloc::{collections::BTreeMap, vec::Vec};
use os_std::sync::Mutex;

const TIMERR_NOERROR: u32 = 0;
const TIMERR_NOCANDO: u32 = 97;
/// The kernel tick, in milliseconds.
const PERIOD_MIN: u32 = 10;
const PERIOD_MAX: u32 = 1_000_000;

/// Outstanding timeBeginPeriod requests by period.
static PERIODS: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());

extern "win64" fn timeGetTime() -> u32 {
    (crate::kernel32::now_ns() / 1_000_000) as u32
}

extern "win64" fn timeBeginPeriod(period: u32) -> u32 {
    if !(PERIOD_MIN..=PERIOD_MAX).contains(&period) {
        return TIMERR_NOCANDO;
    }
    *PERIODS.lock().unwrap().entry(period).or_insert(0) += 1;
    TIMERR_NOERROR
}

extern "win64" fn timeEndPeriod(period: u32) -> u32 {
    let mut p = PERIODS.lock().unwrap();
    match p.get_mut(&period) {
        Some(n) => {
            *n -= 1;
            if *n == 0 {
                p.remove(&period);
            }
            TIMERR_NOERROR
        }
        None => TIMERR_NOCANDO, // no matching timeBeginPeriod
    }
}

/// TIMECAPS: wPeriodMin, wPeriodMax (two UINTs).
extern "win64" fn timeGetDevCaps(caps: *mut u32, size: u32) -> u32 {
    if caps.is_null() || size < 8 {
        return TIMERR_NOCANDO;
    }
    // SAFETY: the caller's TIMECAPS of `size` >= 8 bytes.
    unsafe {
        *caps = PERIOD_MIN;
        *caps.add(1) = PERIOD_MAX;
    }
    TIMERR_NOERROR
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![timeGetTime, timeBeginPeriod, timeEndPeriod, timeGetDevCaps]
}
