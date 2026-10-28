//! Real CPU and shared-address-space checks, enabled by init.args=tests,smp.
use crate::Tests;
use os_std::os::raw;
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use os_std::time::{Duration, Instant};
use os_std::{system, thread};

static READY: AtomicU32 = AtomicU32::new(0);
static COUNT: AtomicU64 = AtomicU64::new(0);
static REQUEST: AtomicU32 = AtomicU32::new(0);
static ACK: AtomicU32 = AtomicU32::new(0);

fn wait(value: &AtomicU32, want: u32) -> bool {
    let deadline = Instant::now();
    while value.load(Ordering::Acquire) < want {
        if deadline.elapsed() > Duration::from_secs(10) {
            return false;
        }
        thread::yield_now();
    }
    true
}

pub fn run(t: &mut Tests) {
    let (cpus, _) = system::cpu_info().unwrap_or((1, 0));
    t.check("SMP: more than one logical CPU is online", || cpus >= 2);
    let mut workers = Vec::new();
    for cpu in 0..cpus {
        workers.push(thread::spawn(move || {
            if system::thread_affinity(0, 1u64 << cpu).is_err() {
                return None;
            }
            READY.fetch_add(1, Ordering::AcqRel);
            if !wait(&READY, cpus) {
                return None;
            }
            let info = system::cpu_info().ok()?;
            let apic = core::arch::x86_64::__cpuid(1).ebx >> 24;
            for _ in 0..100_000 {
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            println!("SMP_CPU logical={} apic={} online={}", info.1, apic, info.0);
            Some((cpu, info.1, apic))
        }));
    }
    let mut apics = Vec::new();
    let mut pinned = true;
    for worker in workers {
        match worker.join().ok().flatten() {
            Some((wanted, actual, apic)) => {
                pinned &= wanted == actual;
                apics.push(apic);
            }
            None => pinned = false,
        }
    }
    apics.sort_unstable();
    apics.dedup();
    t.check("SMP: pinned userspace threads execute on every physical APIC", || pinned && apics.len() == cpus as usize);
    t.check("SMP: contended userspace atomics preserve every update", || {
        COUNT.load(Ordering::Relaxed) == cpus as u64 * 100_000
    });
    t.check("SMP: concurrent syscall entry survives preemption and migration", || {
        let mut callers = Vec::new();
        for _ in 0..cpus {
            callers.push(thread::spawn(move || {
                for n in 0..2000 {
                    match system::cpu_info() {
                        Ok((online, current)) if online == cpus && current < cpus => {}
                        _ => return false,
                    }
                    if n % 8 == 0 { thread::yield_now(); }
                }
                true
            }));
        }
        callers.into_iter().all(|worker| worker.join().unwrap_or(false))
    });
    if cpus < 2 {
        return;
    }
    t.check("SMP: cross-CPU TLB invalidation before physical-frame reuse", || {
        let Ok(page) = raw::map(4096, raw::PROT_READ | raw::PROT_WRITE) else {
            return false;
        };
        unsafe {
            (page as *mut u64).write_volatile(0xA5);
        }
        let address = page as usize;
        let reader = thread::spawn(move || {
            if system::thread_affinity(0, 2).is_err() {
                return false;
            }
            for n in 1..=50 {
                if !wait(&REQUEST, n) {
                    return false;
                }
                if unsafe { (address as *const u64).read_volatile() } != 0 {
                    return false;
                }
                ACK.store(n, Ordering::Release);
            }
            true
        });
        let mut ok = true;
        for n in 1..=50 {
            unsafe {
                (page as *mut u64).write_volatile(0xA5);
            }
            if unsafe { raw::discard(page, 4096) }.is_err() {
                ok = false;
                break;
            }
            let Ok(other) = raw::map(4096, raw::PROT_READ | raw::PROT_WRITE) else {
                ok = false;
                break;
            };
            unsafe {
                (other as *mut u64).write_volatile(0x5A);
            }
            REQUEST.store(n, Ordering::Release);
            ok &= wait(&ACK, n);
            let _ = unsafe { raw::unmap(other, 4096) };
            if !ok {
                break;
            }
        }
        ok &= reader.join().unwrap_or(false);
        let _ = unsafe { raw::unmap(page, 4096) };
        ok
    });
}
