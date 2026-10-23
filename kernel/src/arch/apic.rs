//! The local APIC: receives message-signalled interrupts (MSI and MSI-X)
//! from PCI devices and provides the tick timer (see `arch::timer`).
//!
//! Two register interfaces exist. In xAPIC mode the registers are a 4 KiB
//! MMIO block whose address comes from IA32_APIC_BASE; in x2APIC mode
//! (IA32_APIC_BASE bit 10) they are MSRs 0x800 + offset / 16 and the MMIO
//! block no longer responds. Firmware on current machines often hands over
//! with x2APIC already enabled, and it cannot be switched back without
//! disabling the APIC, so the kernel follows whatever mode it finds (or
//! enables x2APIC itself with `apic=x2apic`, which tests use).
//!
//! LINT0 is set to ExtINT so the 8259 PIC can deliver the PIT interrupt
//! ("virtual wire") when the PIT is the tick source; MSIs arrive as fixed
//! interrupts at vectors [`crate::irq`] hands out and are acknowledged here
//! with an EOI.

use super::cpu;
use crate::{kprintln, mem};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const APIC_X2APIC_ENABLE: u64 = 1 << 10;
const X2APIC_MSR_BASE: u32 = 0x800;
const REG_ID: u64 = 0x20;
const REG_VERSION: u64 = 0x30;
const REG_TPR: u64 = 0x80;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;
const REG_LVT_TIMER: u64 = 0x320;
const REG_LINT0: u64 = 0x350;
const REG_LINT1: u64 = 0x360;
const REG_TIMER_INITIAL: u64 = 0x380;
const REG_TIMER_CURRENT: u64 = 0x390;
const REG_TIMER_DIVIDE: u64 = 0x3E0;
const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
const DELIVERY_EXTINT: u32 = 0b111 << 8;
const DELIVERY_NMI: u32 = 0b100 << 8;
/// Divide configuration 0b0011: the timer counts at bus clock / 16.
const TIMER_DIVIDE_16: u32 = 0b0011;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

/// xAPIC register block (0 in x2APIC mode or without an APIC).
static BASE: AtomicU64 = AtomicU64::new(0);
static X2APIC: AtomicBool = AtomicBool::new(false);
static AVAILABLE: AtomicBool = AtomicBool::new(false);
static ID: AtomicU32 = AtomicU32::new(0);

fn read(off: u64) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: x2APIC mode is enabled, so MSRs 0x800-0x8FF are the APIC
        // registers; only architectural, readable offsets are passed.
        unsafe { cpu::rdmsr(X2APIC_MSR_BASE + (off >> 4) as u32) as u32 }
    } else {
        // SAFETY: BASE maps the 4 KiB local APIC register block (uncached).
        unsafe { read_volatile((BASE.load(Ordering::Relaxed) + off) as *const u32) }
    }
}

fn write(off: u64, v: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: as in `read`; only writable registers are passed.
        unsafe { cpu::wrmsr(X2APIC_MSR_BASE + (off >> 4) as u32, v as u64) }
    } else {
        // SAFETY: as in `read`.
        unsafe { write_volatile((BASE.load(Ordering::Relaxed) + off) as *mut u32, v) }
    }
}

/// Enables the local APIC. Returns false if the CPU has none or it cannot
/// be used (then devices fall back to polling and the PIT drives the tick).
/// `want_x2apic` switches to x2APIC mode if the CPU supports it.
pub fn init(cpu_has_apic: bool, cpu_has_x2apic: bool, want_x2apic: bool) -> bool {
    if !cpu_has_apic {
        kprintln!("apic: no local APIC");
        return false;
    }
    // SAFETY: IA32_APIC_BASE exists when CPUID reports an APIC.
    let mut msr = unsafe { cpu::rdmsr(IA32_APIC_BASE) };
    let phys = msr & 0x000F_FFFF_FFFF_F000;
    let was_enabled = msr & APIC_GLOBAL_ENABLE != 0;
    let firmware_x2apic = msr & APIC_X2APIC_ENABLE != 0 && was_enabled;
    if !was_enabled {
        msr |= APIC_GLOBAL_ENABLE;
        // SAFETY: setting the global enable bit (xAPIC mode first: going
        // from disabled straight to x2APIC is not a valid transition).
        unsafe { cpu::wrmsr(IA32_APIC_BASE, msr & !APIC_X2APIC_ENABLE) };
    }
    if !firmware_x2apic && want_x2apic && cpu_has_x2apic {
        msr |= APIC_X2APIC_ENABLE;
        // SAFETY: xAPIC -> x2APIC is a valid transition on a CPU that
        // reports x2APIC support.
        unsafe { cpu::wrmsr(IA32_APIC_BASE, msr) };
    }
    let x2 = firmware_x2apic || msr & APIC_X2APIC_ENABLE != 0;
    if x2 {
        X2APIC.store(true, Ordering::Relaxed);
    } else {
        let Ok(virt) = mem::map_mmio(phys, 4096) else {
            kprintln!("apic: cannot map the register block at {phys:#x}");
            return false;
        };
        BASE.store(virt, Ordering::Relaxed);
    }
    AVAILABLE.store(true, Ordering::Relaxed);
    let lint0_before = read(REG_LINT0);
    // Keep the PIC's interrupts flowing through LINT0 (ExtINT), NMIs on
    // LINT1, the timer masked until `arch::timer` starts it, accept every
    // priority and software-enable the APIC.
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LINT0, DELIVERY_EXTINT);
    write(REG_LINT1, DELIVERY_NMI);
    write(REG_TPR, 0);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);
    // The x2APIC ID register holds the full 32-bit ID; xAPIC keeps 8 bits
    // in the top byte.
    let id = if x2 { read(REG_ID) } else { read(REG_ID) >> 24 };
    ID.store(id, Ordering::Relaxed);
    kprintln!(
        "apic: local APIC {} in {} mode{}, base {:#x}, version {:#x}, LINT0 {:#x} -> ExtINT{}",
        id,
        if x2 { "x2APIC (MSR)" } else { "xAPIC (MMIO)" },
        if firmware_x2apic {
            " (enabled by the firmware)"
        } else if x2 {
            " (enabled by apic=x2apic)"
        } else {
            ""
        },
        phys,
        read(REG_VERSION) & 0xFF,
        lint0_before,
        if was_enabled { "" } else { " (was globally disabled)" }
    );
    if id > 0xFF {
        kprintln!("apic: WARNING: APIC ID {id} does not fit an MSI address; devices fall back to polling");
    }
    true
}

pub fn available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

pub fn x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

pub fn mode_name() -> &'static str {
    match (available(), x2apic()) {
        (false, _) => "no local APIC",
        (true, true) => "x2APIC",
        (true, false) => "xAPIC",
    }
}

pub fn id() -> u32 {
    ID.load(Ordering::Relaxed)
}

/// Whether MSIs can target this CPU (its ID fits the 8-bit MSI
/// destination field; larger IDs need interrupt remapping).
pub fn msi_capable() -> bool {
    available() && id() <= 0xFF
}

/// Acknowledges the interrupt being handled.
pub fn eoi() {
    if available() {
        write(REG_EOI, 0);
    }
}

/// Address and data of an MSI for `vector` delivered to this CPU (fixed
/// delivery, edge triggered, physical destination mode). The format is the
/// same in xAPIC and x2APIC mode for destinations up to 255.
pub fn msi_message(vector: u8) -> (u64, u32) {
    (0xFEE0_0000 | ((id() & 0xFF) as u64) << 12, vector as u32)
}

/// Starts the timer counting down from the maximum (masked, one-shot) for
/// calibration; [`timer_elapsed`] reads how far it got.
pub fn timer_start_calibration() {
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
    write(REG_TIMER_INITIAL, u32::MAX);
}

/// Timer counts since [`timer_start_calibration`].
pub fn timer_elapsed() -> u32 {
    u32::MAX - read(REG_TIMER_CURRENT)
}

/// Runs the timer periodically: an interrupt at `vector` every `count`
/// timer counts (bus clock / 16).
pub fn timer_start_periodic(vector: u8, count: u32) {
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
    write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | vector as u32);
    write(REG_TIMER_INITIAL, count.max(1));
}

pub fn timer_stop() {
    if available() {
        write(REG_LVT_TIMER, LVT_MASKED);
        write(REG_TIMER_INITIAL, 0);
    }
}
