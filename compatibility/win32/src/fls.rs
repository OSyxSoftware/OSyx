//! Fiber local storage. OSyx runs no fibers (ConvertThreadToFiber is not
//! implemented), so every thread is one fiber and FLS is per thread, as
//! on Windows for code that never switches fibers. Callbacks run when a
//! thread ends (before DLL_THREAD_DETACH), for the exiting thread at
//! ExitProcess, and for every thread's value when the index is freed.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail};
use alloc::{collections::BTreeMap, vec::Vec};
use os_std::sync::Mutex;

/// FLS_MAXIMUM_AVAILABLE on Windows 10.
const MAXIMUM: usize = 4080;
const FLS_OUT_OF_INDEXES: u32 = u32::MAX;

struct Fls {
    /// Per index: allocated, and its callback (0: none).
    slots: Vec<Option<u64>>,
    /// Per thread id: values by index.
    values: BTreeMap<u64, Vec<u64>>,
}

static FLS: Mutex<Fls> = Mutex::new(Fls { slots: Vec::new(), values: BTreeMap::new() });

type Callback = extern "win64" fn(u64);

extern "win64" fn FlsAlloc(callback: u64) -> u32 {
    let mut f = FLS.lock().unwrap();
    let index = match f.slots.iter().position(Option::is_none) {
        Some(i) => i,
        None if f.slots.len() < MAXIMUM => {
            f.slots.push(None);
            f.slots.len() - 1
        }
        None => return fail(ERROR_NO_MORE_ITEMS, FLS_OUT_OF_INDEXES),
    };
    f.slots[index] = Some(callback);
    // A reused index starts out NULL in every thread.
    for v in f.values.values_mut() {
        if let Some(x) = v.get_mut(index) {
            *x = 0;
        }
    }
    index as u32
}

fn valid(f: &Fls, index: u32) -> bool {
    f.slots.get(index as usize).is_some_and(Option::is_some)
}

/// Like Windows, Get/Set only check the range: a freed index is plain
/// storage without a callback until it is allocated again.
fn in_range(index: u32) -> bool {
    (index as usize) < MAXIMUM
}

extern "win64" fn FlsGetValue(index: u32) -> u64 {
    let f = FLS.lock().unwrap();
    if !in_range(index) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    crate::teb::set_last_error(ERROR_SUCCESS);
    f.values.get(&crate::teb::current_tid()).and_then(|v| v.get(index as usize)).copied().unwrap_or(0)
}

extern "win64" fn FlsSetValue(index: u32, value: u64) -> i32 {
    let mut f = FLS.lock().unwrap();
    if !in_range(index) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let v = f.values.entry(crate::teb::current_tid()).or_default();
    if v.len() <= index as usize {
        v.resize(index as usize + 1, 0);
    }
    v[index as usize] = value;
    1
}

extern "win64" fn FlsFree(index: u32) -> i32 {
    let (callback, pending) = {
        let mut f = FLS.lock().unwrap();
        if !valid(&f, index) {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        let callback = f.slots[index as usize].take().unwrap_or(0);
        let mut pending = Vec::new();
        for v in f.values.values_mut() {
            if let Some(x) = v.get_mut(index as usize) {
                if *x != 0 {
                    pending.push(*x);
                }
                *x = 0;
            }
        }
        (callback, pending)
    };
    if callback != 0 {
        // SAFETY: a PFLS_CALLBACK_FUNCTION registered with FlsAlloc.
        let f: Callback = unsafe { core::mem::transmute(callback as usize) };
        for value in pending {
            f(value);
        }
    }
    1
}

/// Runs the calling thread's callbacks and forgets its values.
pub fn thread_exit() {
    let work: Vec<(u64, u64)> = {
        let mut f = FLS.lock().unwrap();
        let Some(values) = f.values.remove(&crate::teb::current_tid()) else { return };
        values
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v != 0)
            .filter_map(|(i, &v)| f.slots.get(i).copied().flatten().filter(|&c| c != 0).map(|c| (c, v)))
            .collect()
    };
    for (callback, value) in work {
        // SAFETY: as in FlsFree.
        let f: Callback = unsafe { core::mem::transmute(callback as usize) };
        f(value);
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![FlsAlloc, FlsGetValue, FlsSetValue, FlsFree]
}
