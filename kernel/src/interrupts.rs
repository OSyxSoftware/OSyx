//! Rust side of interrupt handling: exception diagnostics, fixups, timer.

use crate::arch::idt::{TrapFrame, EXCEPTION_NAMES};
use crate::arch::{apic, cpu, pic, timer};
use crate::{fixup, kprintln};
use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);
static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);
static SPURIOUS: AtomicU64 = AtomicU64::new(0);

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// The tick the clock has really reached, for deadlines counted from now.
/// [`ticks`] can be behind: CPU 0 counts a tick only when its timer
/// interrupt runs, and the other CPUs raise the count only once it is more
/// than a tick late (`catch_up_ticks`). The count then jumps forward, so a
/// deadline of `ticks() + n` set while it was behind expires that much too
/// early. This adds what the TSC says has passed since the last counted
/// tick, by at most a second, rounded to the nearest tick as `record_tick`
/// counts: the count reaches `ticks_now() + n` no sooner than `n` ticks
/// minus the rounding of both ends, which the callers' extra tick covers.
pub fn ticks_now() -> u64 {
    let now = ticks();
    let per = per_tick_estimate();
    let (tick, tsc) = tick_anchor();
    if per == 0 || tick == 0 {
        return now;
    }
    // Another CPU's TSC can be slightly behind CPU 0's.
    let Some(elapsed) = rdtsc().checked_sub(tsc) else { return now };
    now.max((tick + (elapsed + per / 2) / per).min(now + timer::TIMER_HZ as u64))
}

pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Milliseconds since the timer was started.
pub fn uptime_ms() -> u64 {
    ticks() * 1000 / timer::TIMER_HZ as u64
}

// High-resolution time: the TSC between timer ticks. The rate is measured
// from the tick stream itself, and the result is clamped to the tick
// interval and never runs backwards.
static FIRST_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_TSC: AtomicU64 = AtomicU64::new(0);
static LAST_TICK: AtomicU64 = AtomicU64::new(0);
/// Odd while `record_tick` replaces the pair above: readers on other CPUs
/// wait, so they never pair the tick of one interrupt with the TSC of
/// another.
static ANCHOR_SEQ: AtomicU64 = AtomicU64::new(0);

/// The last counted tick and the TSC reading taken with it.
fn tick_anchor() -> (u64, u64) {
    loop {
        let seq = ANCHOR_SEQ.load(Ordering::Acquire);
        let pair = (LAST_TICK.load(Ordering::Relaxed), LAST_TSC.load(Ordering::Relaxed));
        if seq & 1 == 0 && ANCHOR_SEQ.load(Ordering::Acquire) == seq {
            return pair;
        }
        core::hint::spin_loop();
    }
}
/// TSC counts per tick (0 until calibrated).
static TSC_PER_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_NS: AtomicU64 = AtomicU64::new(0);
/// Windowed rate measurement. Long interrupt-disabled stretches (lock spins,
/// early boot) advance the TSC without ticks, so a cumulative average is
/// useless; each window is measured on its own and the slowest recent window
/// is kept. Contention can only inflate a window, never shrink it.
const PER_WINDOW_TICKS: u64 = 500;
const PER_SLOTS: usize = 16;
static WIN_TSC: AtomicU64 = AtomicU64::new(0);
static WIN_TICK: AtomicU64 = AtomicU64::new(0);
static PER_RING: [AtomicU64; PER_SLOTS] = [const { AtomicU64::new(0) }; PER_SLOTS];
static PER_NEXT: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    // SAFETY: RDTSC is available on every x86-64 CPU and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC counts per tick for catching up lost ticks. Deliberately independent
/// of the tick stream (boot calibration against the PIT only): deriving it
/// from counted ticks would feed the correction back into its own estimate.
fn per_tick_estimate() -> u64 {
    let khz = crate::arch::timer::measured_tsc_khz();
    if khz == 0 {
        return 0;
    }
    khz * 1000 / crate::arch::timer::TIMER_HZ as u64
}

/// Advances the tick count from the TSC when CPU 0 has stopped counting.
/// CPU 0 counts ticks only once it holds the kernel entry lock, so while
/// another CPU keeps that lock the clock would stand still, and with it
/// every deadline the holder waits for. Each CPU's timer interrupt calls
/// this before taking the lock. Ticks are raised (never counted twice) to
/// what the TSC says passed since CPU 0's last tick, and only when they
/// are more than a tick behind, so normal counting is left alone. Another
/// CPU's TSC can lag CPU 0's slightly: a reading before CPU 0's last tick
/// changes nothing, and one call advances at most a second.
fn catch_up_ticks() {
    let per = per_tick_estimate();
    let (tick, tsc) = tick_anchor();
    if per == 0 || tick == 0 {
        return;
    }
    let Some(elapsed) = rdtsc().checked_sub(tsc) else { return };
    let now = ticks();
    let target = (tick + elapsed / per).min(now + timer::TIMER_HZ as u64);
    if target > now + 1 {
        TICKS.fetch_max(target, Ordering::Relaxed);
    }
}

/// Counts this timer interrupt and returns the tick number, advanced by any
/// ticks the interval lost. The kernel entry lock spins with interrupts
/// disabled, so the local timer does not fire while a CPU waits for another
/// one: without this correction the guest clock (and every user timeout
/// scaled by it) runs at a fraction of wall time during contention.
///
/// The count is set from the time since the last counted tick, rounded to
/// whole ticks, instead of adding one per interrupt. A late interrupt is
/// followed at once by the one that became pending behind it, and another
/// CPU may already have raised the count (`catch_up_ticks`): adding one for
/// each on top of the lost ticks counted the same time twice, so the clock
/// ran ahead and sleeps ended early. An interrupt less than half a tick
/// after the last counted one adds nothing; its time is counted with the
/// next one.
fn record_tick() -> u64 {
    let tsc = rdtsc();
    let per = per_tick_estimate();
    let (prev_tick, prev_tsc) = tick_anchor();
    let now = match tsc.checked_sub(prev_tsc) {
        Some(elapsed) if per != 0 && prev_tick != 0 => {
            let steps = ((elapsed + per / 2) / per).min(1000);
            if steps == 0 {
                return ticks();
            }
            TICKS.fetch_max(prev_tick + steps, Ordering::Relaxed).max(prev_tick + steps)
        }
        // Not calibrated yet, the first tick, or a TSC behind CPU 0's.
        _ => TICKS.fetch_add(1, Ordering::Relaxed) + 1,
    };
    if FIRST_TICK.load(Ordering::Relaxed) == 0 {
        FIRST_TICK.store(now, Ordering::Relaxed);
    }
    let base = WIN_TICK.load(Ordering::Relaxed);
    if base == 0 {
        // Start measuring only once boot's interrupt-disabled stretches are
        // behind us; they inflate every window that contains one.
        if now >= 500 {
            WIN_TICK.store(now, Ordering::Relaxed);
            WIN_TSC.store(tsc, Ordering::Relaxed);
        }
    } else {
        let ticks = now.wrapping_sub(base);
        if ticks >= PER_WINDOW_TICKS {
            let per = tsc.wrapping_sub(WIN_TSC.load(Ordering::Relaxed)) / ticks;
            WIN_TICK.store(now, Ordering::Relaxed);
            WIN_TSC.store(tsc, Ordering::Relaxed);
            let slot = (PER_NEXT.fetch_add(1, Ordering::Relaxed) as usize) % PER_SLOTS;
            PER_RING[slot].store(per, Ordering::Relaxed);
            let mut best = u64::MAX;
            for p in PER_RING.iter() {
                let v = p.load(Ordering::Relaxed);
                if v != 0 && v < best {
                    best = v;
                }
            }
            if best != u64::MAX {
                TSC_PER_TICK.store(best, Ordering::Relaxed);
            }
        }
    }
    ANCHOR_SEQ.fetch_add(1, Ordering::AcqRel);
    LAST_TSC.store(tsc, Ordering::Relaxed);
    LAST_TICK.store(now, Ordering::Relaxed);
    ANCHOR_SEQ.fetch_add(1, Ordering::Release);
    now
}

/// TSC counts per timer tick (0 until calibrated). User space interpolates
/// its own clocks with this rate; it is constant on an invariant-TSC CPU.
pub fn tsc_per_tick() -> u64 {
    TSC_PER_TICK.load(Ordering::Relaxed)
}

/// Nanoseconds since the timer was started, interpolated with the TSC
/// from the last counted tick (tick resolution until the TSC is calibrated).
/// The interpolation is not limited to one tick: the tick count falls
/// behind when CPU 0's timer interrupt is late, and a clock that stood still
/// for that time made user space, which re-anchors its own TSC clock on this
/// one, measure intervals too short by the delay.
pub fn monotonic_ns() -> u64 {
    monotonic_pair().0
}

/// [`monotonic_ns`] and the TSC reading it was computed from. User space
/// anchors its clock on the pair (`TIME_CALIB`); read apart, a preemption
/// between the two would shift that clock by its length.
pub fn monotonic_pair() -> (u64, u64) {
    let tick_ns = 1_000_000_000 / pic::TIMER_HZ as u64;
    let were = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    let ((tick, base), per) = (tick_anchor(), TSC_PER_TICK.load(Ordering::Relaxed));
    let tsc = rdtsc();
    if were {
        cpu::enable_interrupts();
    }
    let mut ns = tick * tick_ns;
    if per > 0 {
        // Another CPU's TSC can be slightly behind CPU 0's.
        let extra = (tsc.saturating_sub(base) as u128 * tick_ns as u128 / per as u128) as u64;
        ns += extra.min(1_000_000_000);
    }
    // Monotonic across calls even if the rate estimate moves.
    let prev = LAST_NS.fetch_max(ns, Ordering::Relaxed);
    (ns.max(prev), tsc)
}

#[no_mangle]
extern "C" fn osyx_interrupt_dispatch(f: &mut TrapFrame) {
    if f.vector == 2 && crate::smp::handle_tlb_nmi() { return; }
    crate::smp::leave_tickless();
    if f.vector == crate::smp::RESCHED_VECTOR as u64 {
        // Work was queued for this idle CPU; nothing else to do here.
        apic::eoi();
        crate::sched::preempt_if_idle();
        return;
    }
    let from_user = f.cs & 3 == 3;
    // Sample before taking the entry lock: while it is contended the handler
    // spins with interrupts off, so samples after the acquire are rare.
    if from_user && (f.vector == pic::TIMER_VECTOR as u64 || f.vector == timer::APIC_TIMER_VECTOR as u64) {
        crate::smp::sample_user(f.rip, f.rsp);
    }
    if f.vector == pic::TIMER_VECTOR as u64 || f.vector == timer::APIC_TIMER_VECTOR as u64 {
        catch_up_ticks();
    }
    // A not-present page in an anonymous region: give it its frame (zeroed,
    // or restored from compressed memory) and retry, whether user code or a
    // kernel copy to/from user memory touched it. User code holds no kernel
    // lock, so its fault may wait for its memory lock instead of reporting
    // an access violation the process cannot fix. Both faults need only the
    // process memory, paging and frame locks, so they are resolved before the
    // entry lock: a browser starting up takes hundreds of thousands of them,
    // and a lock-free read or futex copy must not queue behind other CPUs'
    // system calls for one.
    if f.vector == 14 && f.error_code & 1 == 0 {
        let (write, fetch) = (f.error_code & 2 != 0, f.error_code & 0x10 != 0);
        let va = cpu::read_cr2();
        let mapped = if from_user {
            crate::process::demand_fault_user(va, write, fetch)
        } else {
            crate::process::demand_fault(va, write, fetch)
        };
        if mapped {
            return;
        }
    }
    // The kernel touched a present user page outside a user access window
    // (SMAP), or fetched an instruction from one (SMEP): a kernel bug, never
    // a page to repair. A routine with a fixup recovers below; anything else
    // stops the system with the faulting address.
    let supervisor_violation = f.vector == 14
        && !from_user
        && f.error_code & 1 != 0
        && cpu::read_cr2() < osyx_abi::USER_END
        && ((f.error_code & 0x10 != 0 && cpu::smep_enabled()) || (f.rflags & (1 << 18) == 0 && cpu::smap_enabled()));
    // A write to a read-only shared image page: copy it for this process.
    if f.vector == 14
        && f.error_code & 3 == 3
        && !supervisor_violation
        && crate::process::cow_fault(cpu::read_cr2(), from_user)
    {
        return;
    }
    // The timer only needs the tick counters (atomics) and the scheduler's
    // own lock. Taking the entry lock here made every CPU's tick wait for
    // other CPUs' system calls, and a thread preempted in user mode then had
    // to win the lock back (spinning, interrupts off) just to return to user
    // mode. A preempted kernel path that holds the lock keeps it across the
    // switch as before (`pick_next` records ownership per thread).
    if f.vector == pic::TIMER_VECTOR as u64 || f.vector == timer::APIC_TIMER_VECTOR as u64 {
        let now = if f.vector == pic::TIMER_VECTOR as u64 || crate::smp::index() == 0 {
            record_tick()
        } else {
            ticks()
        };
        // Acknowledge first: the scheduler may switch to another thread
        // before this handler returns.
        if f.vector == pic::TIMER_VECTOR as u64 {
            pic::end_of_interrupt(0);
        } else {
            apic::eoi();
        }
        crate::prof::sample(f, now);
        crate::sched::tick(now);
        if from_user && crate::sched::current_kill_pending() {
            let _entry = crate::smp::KernelEntry::new();
            crate::smp::set_hold_kind(crate::smp::HOLD_TIMER);
            crate::process::finish_current_thread(crate::sched::current_kill_code());
        }
        // A thread suspended while it ran user code stops at the first tick.
        if from_user {
            crate::sched::park_if_suspended();
        }
        return;
    }
    // Device MSIs: their handlers only take spinlocks and wake a driver
    // thread (see `irq`), so a packet interrupt does not wait for, or hold
    // up, other CPUs' system calls.
    if (crate::irq::FIRST as u64..=crate::irq::LAST as u64).contains(&f.vector) {
        crate::irq::dispatch(f.vector as u8);
        if from_user && crate::sched::current_kill_pending() {
            let _entry = crate::smp::KernelEntry::new();
            crate::smp::set_hold_kind(crate::smp::HOLD_IRQ);
            crate::process::finish_current_thread(crate::sched::current_kill_code());
        }
        return;
    }
    let _entry = crate::smp::KernelEntry::new();
    if _entry.acquired() {
        crate::smp::set_hold_kind(if f.vector < 32 {
            crate::smp::HOLD_EXCEPTION
        } else {
            crate::smp::HOLD_IRQ
        });
    }
    match f.vector {
        0..=31 if from_user => {
            if !crate::user_fault::deliver(f) {kill_process(f);}
        },
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
            kprintln!("breakpoint at {:#x}", f.rip);
        }
        13 | 14 => {
            if let Some(recover) = fixup::lookup(f.rip) {
                f.rip = recover;
                return;
            }
            fatal(f);
        }
        0..=31 => fatal(f),
        v @ 33..=47 => {
            let irq = (v - pic::IRQ_BASE as u64) as u8;
            if pic::is_spurious(irq) {
                SPURIOUS.fetch_add(1, Ordering::Relaxed);
            } else {
                kprintln!("unexpected IRQ {irq}");
                pic::end_of_interrupt(irq);
            }
        }
        v if (crate::irq::FIRST as u64..=crate::irq::LAST as u64).contains(&v) => crate::irq::dispatch(v as u8),
        v if v == apic::SPURIOUS_VECTOR as u64 => {
            // Local APIC spurious interrupt: no EOI.
            SPURIOUS.fetch_add(1, Ordering::Relaxed);
        }
        v => kprintln!("unexpected interrupt vector {v}"),
    }
    // Safe point: about to return to ring 3 in a thread whose process is
    // exiting (e.g. it was preempted in user mode by the timer).
    if from_user && crate::sched::current_kill_pending() {
        crate::process::finish_current_thread(crate::sched::current_kill_code());
    }
}

/// A CPU exception raised by user code terminates the process, not the kernel.
fn kill_process(f: &TrapFrame) -> ! {
    let name = EXCEPTION_NAMES[f.vector as usize];
    kprintln!(
        "process {} terminated: {} at rip {:#x}, error code {:#x}",
        crate::sched::current_pid(),
        name,
        f.rip,
        f.error_code
    );
    if f.vector == 14 {
        kprintln!("  faulting address {:#018x}", cpu::read_cr2());
    }
    // The exception arrived from ring 3, so we run on the thread's kernel
    // stack (TSS.RSP0) and can end the process from here.
    crate::process::exit_current(osyx_abi::EXCEPTION_EXIT_BASE | f.vector as i32)
}

fn fatal(f: &TrapFrame) -> ! {
    let name = EXCEPTION_NAMES[f.vector as usize];
    kprintln!("EXCEPTION: {} (vector {}, error code {:#x})", name, f.vector, f.error_code);
    if f.vector == 14 {
        let cr2 = cpu::read_cr2();
        let e = f.error_code;
        kprintln!(
            "  page fault address {:#018x}: {} {} access in {} mode{}{}{}",
            cr2,
            if e & 1 != 0 { "protection violation on" } else { "non-present page," },
            if e & (1 << 4) != 0 {
                "instruction fetch"
            } else if e & 2 != 0 {
                "write"
            } else {
                "read"
            },
            if e & 4 != 0 { "user" } else { "kernel" },
            if e & 8 != 0 { ", reserved bit set" } else { "" },
            if e & (1 << 5) != 0 { ", protection key" } else { "" },
            if e & (1 << 6) != 0 { ", shadow stack" } else { "" },
        );
    }
    if f.vector == 13 && f.error_code != 0 {
        kprintln!("  selector error code: index {:#x}, table {}", f.error_code >> 3, (f.error_code >> 1) & 3);
    }
    kprintln!("  rip {:#018x}  cs {:#06x}  rflags {:#010x}", f.rip, f.cs, f.rflags);
    kprintln!("  rsp {:#018x}  ss {:#06x}  cr3 {:#x}", f.rsp, f.ss, cpu::read_cr3());
    kprintln!("  rax {:#018x}  rbx {:#018x}  rcx {:#018x}", f.rax, f.rbx, f.rcx);
    kprintln!("  rdx {:#018x}  rsi {:#018x}  rdi {:#018x}", f.rdx, f.rsi, f.rdi);
    kprintln!("  rbp {:#018x}  r8  {:#018x}  r9  {:#018x}", f.rbp, f.r8, f.r9);
    kprintln!("  r10 {:#018x}  r11 {:#018x}  r12 {:#018x}", f.r10, f.r11, f.r12);
    kprintln!("  r13 {:#018x}  r14 {:#018x}  r15 {:#018x}", f.r13, f.r14, f.r15);
    panic!("unhandled CPU exception: {}", name);
}
