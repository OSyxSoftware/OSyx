//! NT keyed events: one wait pairs with one release of the same key.
//! Both operations block until paired; a release is not a stored event signal.
#![allow(non_snake_case)]
use crate::error::*;
use crate::objects::{self, Object};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};
use os_std::{sync::Mutex, time::{Duration, Instant}};

struct Pending {
    key: u64,
    release: bool,
    done: Arc<AtomicU32>,
}
pub struct KeyedEvent {
    pending: Mutex<Vec<Pending>>,
}

impl KeyedEvent {
    fn rendezvous(&self, key: u64, release: bool, milliseconds: u32) -> NtStatus {
        let deadline = (milliseconds != objects::INFINITE)
            .then(|| Instant::now() + Duration::from_millis(milliseconds as u64));
        let done;
        {
            let mut pending = self.pending.lock().unwrap();
            if let Some(index) = pending.iter().position(|p| p.key == key && p.release != release) {
                let partner = pending.remove(index);
                partner.done.store(1, Ordering::Release);
                os_sys::futex_wake(partner.done.as_ptr(), 1);
                return STATUS_SUCCESS;
            }
            if milliseconds == 0 { return STATUS_TIMEOUT; }
            done = Arc::new(AtomicU32::new(0));
            pending.push(Pending { key, release, done: done.clone() });
        }
        loop {
            if done.load(Ordering::Acquire) != 0 { return STATUS_SUCCESS; }
            let remaining = deadline.map(|d| d.duration_since(Instant::now()));
            if remaining.is_some_and(|r| r.is_zero()) {
                // Match/removal share the same mutex, so a timeout cannot
                // strand the partner in a simultaneous successful release.
                let mut pending = self.pending.lock().unwrap();
                if done.load(Ordering::Acquire) != 0 { return STATUS_SUCCESS; }
                pending.retain(|p| !Arc::ptr_eq(&p.done, &done));
                return STATUS_TIMEOUT;
            }
            let ns = remaining.map_or(0, |r| r.as_nanos().min(u64::MAX as u128) as u64);
            let _ = os_sys::futex_wait(done.as_ptr(), 0, ns);
        }
    }
}

extern "win64" fn NtCreateKeyedEvent(out: *mut u64, _access: u32, attributes: u64, flags: u32) -> NtStatus {
    if out.is_null() || flags != 0 { return STATUS_INVALID_PARAMETER; }
    let inherit = if attributes == 0 { false } else {
        // OBJECT_ATTRIBUTES is 48 bytes on Windows x64. Named keyed events
        // and security descriptors are not supported by this local backend.
        unsafe {
            if crate::teb::get::<u32>(attributes, 0) != 48 { return STATUS_INVALID_PARAMETER; }
            if crate::teb::get::<u64>(attributes, 8) != 0 || crate::teb::get::<u64>(attributes, 16) != 0
                || crate::teb::get::<u64>(attributes, 32) != 0 || crate::teb::get::<u64>(attributes, 40) != 0 {
                return STATUS_NOT_SUPPORTED;
            }
            crate::teb::get::<u32>(attributes, 24) & 2 != 0
        }
    };
    let handle = objects::insert(Object::KeyedEvent(Arc::new(KeyedEvent { pending: Mutex::new(Vec::new()) })));
    objects::set_inheritable(handle, inherit);
    unsafe { *out = handle; }
    STATUS_SUCCESS
}

fn operate(handle: u64, key: u64, timeout: *const i64, release: bool) -> NtStatus {
    if key & 1 != 0 { return STATUS_INVALID_PARAMETER; }
    let event = match objects::get(handle) {
        Some(Object::KeyedEvent(event)) => event,
        _ => return STATUS_INVALID_HANDLE,
    };
    let milliseconds = match crate::ntdll::timeout_ms(timeout) {
        Ok(ms) => ms,
        Err(status) => return status,
    };
    event.rendezvous(key, release, milliseconds)
}
extern "win64" fn NtWaitForKeyedEvent(handle: u64, key: u64, _alertable: u8, timeout: *const i64) -> NtStatus {
    operate(handle, key, timeout, false)
}
extern "win64" fn NtReleaseKeyedEvent(handle: u64, key: u64, _alertable: u8, timeout: *const i64) -> NtStatus {
    operate(handle, key, timeout, true)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![NtCreateKeyedEvent, NtWaitForKeyedEvent, NtReleaseKeyedEvent]
}
