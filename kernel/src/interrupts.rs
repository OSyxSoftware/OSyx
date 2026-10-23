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

pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Milliseconds since the timer was started.
pub fn uptime_ms() -> u64 {
    ticks() * 1000 / timer::TIMER_HZ as u64
}

// High-resolution time: the TSC between timer ticks. The TSC rate is
// measured against the PIT (first tick vs. the latest one), so no
// assumption about invariant-TSC CPUIDs is needed; the result is clamped
// to the tick interval and never runs backwards.
static FIRST_TSC: AtomicU64 = AtomicU64::new(0);
static FIRST_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_TSC: AtomicU64 = AtomicU64::new(0);
static LAST_TICK: AtomicU64 = AtomicU64::new(0);
/// TSC counts per tick (0 until calibrated).
static TSC_PER_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_NS: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    // SAFETY: RDTSC is available on every x86-64 CPU and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn record_tick(now: u64) {
    let tsc = rdtsc();
    if FIRST_TICK.load(Ordering::Relaxed) == 0 {
        FIRST_TSC.store(tsc, Ordering::Relaxed);
        FIRST_TICK.store(now, Ordering::Relaxed);
    } else {
        let ticks = now - FIRST_TICK.load(Ordering::Relaxed);
        if ticks >= 10 {
            let per = tsc.wrapping_sub(FIRST_TSC.load(Ordering::Relaxed)) / ticks;
            TSC_PER_TICK.store(per, Ordering::Relaxed);
        }
    }
    LAST_TSC.store(tsc, Ordering::Relaxed);
    LAST_TICK.store(now, Ordering::Relaxed);
}

/// Nanoseconds since the timer was started, interpolated with the TSC
/// between ticks (tick resolution until the TSC is calibrated).
pub fn monotonic_ns() -> u64 {
    let tick_ns = 1_000_000_000 / pic::TIMER_HZ as u64;
    let were = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    let (tick, base, per) = (LAST_TICK.load(Ordering::Relaxed), LAST_TSC.load(Ordering::Relaxed), TSC_PER_TICK.load(Ordering::Relaxed));
    let tsc = rdtsc();
    if were {
        cpu::enable_interrupts();
    }
    let mut ns = tick * tick_ns;
    if per > 0 {
        let extra = (tsc.wrapping_sub(base) as u128 * tick_ns as u128 / per as u128) as u64;
        ns += extra.min(tick_ns - 1);
    }
    // Monotonic across calls even if the rate estimate moves.
    let prev = LAST_NS.fetch_max(ns, Ordering::Relaxed);
    ns.max(prev)
}

#[no_mangle]
extern "C" fn osyx_interrupt_dispatch(f: &mut TrapFrame) {
    let from_user = f.cs & 3 == 3;
    match f.vector {
        0..=31 if from_user => kill_process(f),
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
        v if v == pic::TIMER_VECTOR as u64 => {
            let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
            record_tick(now);
            // Acknowledge first: the scheduler may switch to another thread
            // before this handler returns.
            pic::end_of_interrupt(0);
            crate::sched::tick(now);
        }
        v if v == timer::APIC_TIMER_VECTOR as u64 => {
            let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
            record_tick(now);
            apic::eoi();
            crate::sched::tick(now);
        }
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
        crate::process::finish_current_thread(0);
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
