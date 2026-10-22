//! Rust side of interrupt handling: exception diagnostics, fixups, timer.

use crate::arch::idt::{TrapFrame, EXCEPTION_NAMES};
use crate::arch::{cpu, pic};
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
    ticks() * 1000 / pic::TIMER_HZ as u64
}

#[no_mangle]
extern "C" fn osyx_interrupt_dispatch(f: &mut TrapFrame) {
    match f.vector {
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
            TICKS.fetch_add(1, Ordering::Relaxed);
            pic::end_of_interrupt(0);
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
        v => kprintln!("unexpected interrupt vector {v}"),
    }
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
