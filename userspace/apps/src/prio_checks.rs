//! Scheduler priority checks, enabled by init.args=tests,smp: priority
//! inheritance through futex locks (alone and on every CPU at once) and the
//! dynamic boost after a wait. They read the scheduler's own view of a
//! thread with `priority::INFO`. The input boost needs pointer events; the
//! SMP test script sends them over QMP while the guest runs.
use crate::Tests;
use os_std::io::Read;
use os_std::os::raw;
use os_std::prelude::*;
use os_std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use os_std::sync::{Arc, Mutex};
use os_std::time::{Duration, Instant};
use os_std::{fs, system, thread};
use system::priority as p;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Info {
    base: u64,
    effective: u64,
    boost: u64,
    inherited: u64,
}

impl Info {
    fn unpack(v: u64) -> Info {
        Info { base: v & 0xff, effective: v >> 8 & 0xff, boost: v >> 16 & 0xff, inherited: v >> 24 & 0xff }
    }
}

/// Scheduler view of thread `tid` of this process (0: the caller).
fn info(tid: u64) -> Option<Info> {
    system::thread_priority(tid, p::INFO).ok().map(Info::unpack)
}

fn pin(cpu: u32) -> bool {
    system::thread_affinity(0, 1u64 << cpu).is_ok()
}

fn set_priority(level: u64) -> bool {
    system::thread_priority(0, level).is_ok()
}

fn spin_for(d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        core::hint::spin_loop();
    }
}

/// Polls `f` until it holds or `limit` passes; returns the time it took.
fn wait_for(limit: Duration, mut f: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    loop {
        if f() {
            return Some(start.elapsed());
        }
        if start.elapsed() > limit {
            return None;
        }
        thread::yield_now();
    }
}

fn ms(d: Duration) -> u128 {
    d.as_micros() / 1000
}

pub fn run(t: &mut Tests) {
    let (cpus, _) = system::cpu_info().unwrap_or((1, 0));
    if cpus < 2 {
        t.check("PRIO: the priority checks need two CPUs", || false);
        return;
    }
    // The checking thread observes from CPU 0; the contended work runs on
    // the last CPU.
    let all = (1u64 << cpus) - 1;
    let _ = system::thread_affinity(0, 1);
    let started = Instant::now();
    t.check("PRIO 1: priority inversion: LOW holder inherits HIGH, gives it back on unlock", || inversion(cpus - 1));
    println!("prio: test 1 took {} ms", ms(started.elapsed()));
    let started = Instant::now();
    t.check("PRIO 2: dynamic boost: +1 futex/IPC, +2 input, HIGH ceiling, decay per slice", || boost(cpus - 1));
    println!("prio: test 2 took {} ms", ms(started.elapsed()));
    let started = Instant::now();
    t.check("PRIO 3: SMP inheritance: contended locks on every CPU, no lost update, no deadlock", || smp_inheritance(cpus));
    println!("prio: test 3 took {} ms", ms(started.elapsed()));
    let _ = system::thread_affinity(0, all);
}

/// LOW takes a lock and keeps working; MEDIUM keeps the same CPU busy;
/// HIGH then waits for the lock. LOW must run at HIGH's priority until it
/// unlocks, and only then.
fn inversion(cpu: u32) -> bool {
    let lock = Arc::new(Mutex::new(0u64));
    let low_tid = Arc::new(AtomicU64::new(0));
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let low = {
        let (lock, low_tid, held, release, progress) =
            (lock.clone(), low_tid.clone(), held.clone(), release.clone(), progress.clone());
        thread::spawn(move || {
            if !set_priority(p::LOW) || !pin(cpu) {
                return None;
            }
            low_tid.store(raw::gettid(), Ordering::Release);
            let mut guard = lock.lock().ok()?;
            held.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                progress.fetch_add(1, Ordering::Relaxed);
                *guard += 1;
            }
            drop(guard);
            info(0)
        })
    };
    if wait_for(Duration::from_secs(5), || held.load(Ordering::Acquire)).is_none() {
        println!("prio1: LOW never took the lock");
        release.store(true, Ordering::Release);
        let _ = low.join();
        return false;
    }
    let low_tid = low_tid.load(Ordering::Acquire);
    let medium = {
        let stop = stop.clone();
        thread::spawn(move || {
            if !set_priority(p::NORMAL) || !pin(cpu) {
                return false;
            }
            while !stop.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            true
        })
    };
    thread::sleep(Duration::from_millis(50));
    let before_wait = info(low_tid);
    let p0 = progress.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(200));
    let starved = progress.load(Ordering::Relaxed) - p0;
    let base = Instant::now();
    let acquired_ns = Arc::new(AtomicU64::new(0));
    let high = {
        let (lock, acquired_ns) = (lock.clone(), acquired_ns.clone());
        thread::spawn(move || {
            if !set_priority(p::HIGH) || !pin(cpu) {
                return None;
            }
            let waiting = Instant::now();
            let guard = lock.lock().ok()?;
            acquired_ns.store(base.elapsed().as_nanos() as u64, Ordering::Release);
            let waited = waiting.elapsed();
            drop(guard);
            Some((waited, info(0)?))
        })
    };
    let lent = wait_for(Duration::from_secs(2), || info(low_tid).is_some_and(|i| i.inherited == p::HIGH));
    let during = info(low_tid);
    let p1 = progress.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(200));
    let boosted = progress.load(Ordering::Relaxed) - p1;
    let released_at = base.elapsed().as_nanos() as u64;
    release.store(true, Ordering::Release);
    let high = high.join().ok().flatten();
    let after = low.join().ok().flatten();
    stop.store(true, Ordering::Release);
    let medium_ok = medium.join().unwrap_or(false);
    let handoff_us = acquired_ns.load(Ordering::Acquire).saturating_sub(released_at) / 1000;
    println!(
        "prio1: before {:?}; lent after {:?}; during {:?}; LOW progress per 200 ms {} starved vs {} inheriting; \
         unlock to HIGH {} us; HIGH {:?}; LOW after unlock {:?}",
        before_wait, lent, during, starved, boosted, handoff_us, high, after
    );
    let Some((_, high_info)) = high else { return false };
    let before_ok = before_wait.is_some_and(|i| i.base == p::LOW && i.inherited == 0);
    let during_ok = lent.is_some() && during.is_some_and(|i| i.inherited == p::HIGH && i.effective == p::HIGH && i.base == p::LOW);
    // Inheriting, LOW outruns MEDIUM; without it, LOW only runs when aging
    // lifts it to MEDIUM's level.
    let ran_ahead = boosted > starved.saturating_mul(4);
    let restored = after.is_some_and(|i| i.inherited == 0 && i.base == p::LOW && i.effective <= p::LOW + 1);
    let high_ok = high_info.base == p::HIGH && high_info.inherited == 0;
    before_ok && during_ok && ran_ahead && restored && high_ok && medium_ok
}

static WORD: AtomicU32 = AtomicU32::new(0);
static WAITING: AtomicU32 = AtomicU32::new(0);

/// After its wait ends: (scheduler view right away, view after `spin`).
type Woken = Option<(Info, Info)>;

/// A thread of priority `level` on `cpu` blocks in `wait`, records its view
/// right after waking, burns `spin` of CPU time and records it again.
fn woken(cpu: u32, level: u64, spin: Duration, wait: impl FnOnce() -> bool + Send + 'static) -> Woken {
    let worker = thread::spawn(move || {
        if !set_priority(level) || !pin(cpu) || !wait() {
            return None;
        }
        let now = info(0)?;
        spin_for(spin);
        Some((now, info(0)?))
    });
    worker.join().ok().flatten()
}

fn futex_woken(cpu: u32, level: u64, spin: Duration) -> Woken {
    WORD.store(0, Ordering::Release);
    WAITING.store(0, Ordering::Release);
    let waker = thread::spawn(|| {
        if wait_for(Duration::from_secs(2), || WAITING.load(Ordering::Acquire) == 1).is_none() {
            return;
        }
        // Long enough for the waiter to be asleep in the kernel.
        thread::sleep(Duration::from_millis(30));
        WORD.store(1, Ordering::Release);
        raw::futex_wake(WORD.as_ptr(), 1);
    });
    let r = woken(cpu, level, spin, || {
        WAITING.store(1, Ordering::Release);
        while WORD.load(Ordering::Acquire) == 0 {
            if raw::futex_wait(WORD.as_ptr(), 0, 2_000_000_000) == Err(raw::Errno::TimedOut) {
                return false;
            }
        }
        true
    });
    let _ = waker.join();
    r
}

fn pipe_woken(cpu: u32, level: u64, spin: Duration) -> Woken {
    let Ok((rd, wr)) = os_sys::pipe() else { return None };
    let waker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        let _ = raw::write(wr, b"x");
    });
    let r = woken(cpu, level, spin, move || {
        let mut b = [0u8; 1];
        raw::read(rd, &mut b) == Ok(1)
    });
    let _ = waker.join();
    let _ = raw::close(rd);
    let _ = raw::close(wr);
    r
}

/// Waits for pointer input twice: the first read may find events already
/// queued, the second one sleeps until the next event arrives.
fn input_woken(cpu: u32, level: u64, spin: Duration) -> Woken {
    woken(cpu, level, spin, || {
        let Ok(mut seat) = fs::File::open("/dev/input/seat0") else {
            println!("prio2: /dev/input/seat0 cannot be opened");
            return false;
        };
        let mut buf = [0u8; 4096];
        let first = seat.read(&mut buf).is_ok_and(|n| n > 0);
        let start = Instant::now();
        let second = seat.read(&mut buf).is_ok_and(|n| n > 0);
        println!("prio2: input read slept {} ms", ms(start.elapsed()));
        first && second
    })
}

fn boost(cpu: u32) -> bool {
    // One time slice is 10 ticks of 1 ms: 60 ms of work uses several.
    let decay = Duration::from_millis(60);
    let futex = futex_woken(cpu, p::NORMAL, decay);
    let pipe = pipe_woken(cpu, p::NORMAL, decay);
    let input = input_woken(cpu, p::NORMAL, decay);
    let input_above = input_woken(cpu, p::ABOVE_NORMAL, Duration::ZERO);
    let futex_high = futex_woken(cpu, p::HIGH, Duration::ZERO);
    let futex_realtime = futex_woken(cpu, p::REALTIME, Duration::ZERO);
    println!(
        "prio2: futex {:?}; pipe {:?}; input {:?}; input ABOVE_NORMAL {:?}; futex HIGH {:?}; futex REALTIME {:?}",
        futex, pipe, input, input_above, futex_high, futex_realtime
    );
    let woke = |w: Woken, base: u64, boost: u64, effective: u64| {
        w.is_some_and(|(now, _)| now.base == base && now.boost == boost && now.effective == effective && now.inherited == 0)
    };
    let decayed = |w: Woken, base: u64| w.is_some_and(|(_, later)| later.boost == 0 && later.effective == base);
    let ipc = woke(futex, p::NORMAL, 1, p::NORMAL + 1) && woke(pipe, p::NORMAL, 1, p::NORMAL + 1);
    let input_ok = woke(input, p::NORMAL, 2, p::NORMAL + 2);
    let ceiling = woke(input_above, p::ABOVE_NORMAL, 2, p::HIGH)
        && woke(futex_high, p::HIGH, 1, p::HIGH)
        && woke(futex_realtime, p::REALTIME, 1, p::REALTIME);
    let decay_ok = decayed(futex, p::NORMAL) && decayed(pipe, p::NORMAL) && decayed(input, p::NORMAL);
    println!("prio2: IPC {ipc}, input {input_ok}, ceiling {ceiling}, decay {decay_ok}");
    ipc && input_ok && ceiling && decay_ok
}

const LOCKS: usize = 4;
const ROUNDS: u64 = 300;

/// On every CPU a LOW and a HIGH thread take the same few locks in turn
/// while a NORMAL thread keeps that CPU busy. Every update must survive,
/// all threads must finish, LOW holders must be seen inheriting, nobody may
/// inherit more than the highest waiter, and nothing stays lent at the end.
fn smp_inheritance(cpus: u32) -> bool {
    let locks: Arc<Vec<Mutex<u64>>> = Arc::new((0..LOCKS).map(|_| Mutex::new(0)).collect());
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let low_tids: Arc<Vec<AtomicU64>> = Arc::new((0..cpus).map(|_| AtomicU64::new(0)).collect());
    let workers_total = 2 * cpus as usize;
    let mut spinners = Vec::new();
    for cpu in 0..cpus {
        let stop = stop.clone();
        spinners.push(thread::spawn(move || {
            if !set_priority(p::NORMAL) || !pin(cpu) {
                return false;
            }
            while !stop.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            true
        }));
    }
    let mut workers = Vec::new();
    for cpu in 0..cpus {
        for (k, level) in [p::LOW, p::HIGH].into_iter().enumerate() {
            let (locks, finished, release, low_tids) = (locks.clone(), finished.clone(), release.clone(), low_tids.clone());
            workers.push(thread::spawn(move || {
                if !set_priority(level) || !pin(cpu) {
                    return None;
                }
                if level == p::LOW {
                    low_tids[cpu as usize].store(raw::gettid(), Ordering::Release);
                }
                let mut seed = (cpu as u64 + 1) * 0x9E37_79B9 + k as u64 * 7;
                let mut taken = [0u64; LOCKS];
                let hold = if level == p::LOW { Duration::from_micros(200) } else { Duration::from_micros(20) };
                for _ in 0..ROUNDS {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let i = (seed % LOCKS as u64) as usize;
                    let mut g = locks[i].lock().ok()?;
                    // A lost update shows if two threads ever hold it at once.
                    let v = *g;
                    spin_for(hold);
                    *g = v + 1;
                    drop(g);
                    taken[i] += 1;
                }
                finished.fetch_add(1, Ordering::AcqRel);
                // Stay alive until everyone is done so the final view is read
                // while no lock is held or waited for.
                while !release.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(2));
                }
                Some((taken, info(0)?))
            }));
        }
    }
    let start = Instant::now();
    let mut lent_seen = 0u64;
    let mut over = 0u64;
    let mut max_seen = 0u64;
    let done = wait_for(Duration::from_secs(60), || {
        for tid in low_tids.iter() {
            let tid = tid.load(Ordering::Acquire);
            if let Some(i) = info(tid).filter(|_| tid != 0) {
                if i.inherited > 0 {
                    lent_seen += 1;
                }
                if i.inherited > p::HIGH {
                    over += 1;
                }
                max_seen = max_seen.max(i.inherited);
            }
        }
        finished.load(Ordering::Acquire) == workers_total
    });
    let elapsed = start.elapsed();
    release.store(true, Ordering::Release);
    let results: Vec<_> = workers.into_iter().map(|w| w.join().ok().flatten()).collect();
    stop.store(true, Ordering::Release);
    let spinners_ok = spinners.into_iter().all(|s| s.join().unwrap_or(false));
    let mut expected = [0u64; LOCKS];
    let mut clean = true;
    for (taken, last) in results.iter().flatten() {
        for i in 0..LOCKS {
            expected[i] += taken[i];
        }
        clean &= last.inherited == 0 && last.effective <= last.base + 1;
    }
    let complete = results.iter().all(Option::is_some);
    let counts: Vec<u64> = locks.iter().map(|m| *m.lock().unwrap()).collect();
    let consistent = counts.iter().zip(expected.iter()).all(|(a, b)| a == b) && counts.iter().sum::<u64>() == ROUNDS * workers_total as u64;
    println!(
        "prio3: {} CPUs, {} workers x {} rounds in {} ms (finished: {}); counts {:?} expected {:?}; \
         inheritance seen {} samples, max {}, over HIGH {}; final views clean {}",
        cpus,
        workers_total,
        ROUNDS,
        ms(elapsed),
        done.is_some(),
        counts,
        expected,
        lent_seen,
        max_seen,
        over,
        clean
    );
    done.is_some() && complete && consistent && lent_seen > 0 && max_seen <= p::HIGH && over == 0 && clean && spinners_ok
}
