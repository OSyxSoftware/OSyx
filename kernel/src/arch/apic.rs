//! The local APIC, used to receive message-signalled interrupts (MSI and
//! MSI-X) from PCI devices.
//!
//! The 8259 PIC remains the timer source (through LINT0 in "virtual wire"
//! mode), so the local APIC is only switched on and told to pass ExtINT
//! from LINT0 through; MSIs then arrive as fixed interrupts at vectors
//! [`crate::irq`] hands out and are acknowledged here with an EOI. The
//! register block's address comes from IA32_APIC_BASE.

use super::cpu;
use crate::{kprintln, mem};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const REG_ID: u64 = 0x20;
const REG_VERSION: u64 = 0x30;
const REG_TPR: u64 = 0x80;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;
const REG_LINT0: u64 = 0x350;
const REG_LINT1: u64 = 0x360;
const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const DELIVERY_EXTINT: u32 = 0b111 << 8;
const DELIVERY_NMI: u32 = 0b100 << 8;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

static BASE: AtomicU64 = AtomicU64::new(0);
static ID: AtomicU32 = AtomicU32::new(0);

fn reg(off: u64) -> *mut u32 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u32
}

fn read(off: u64) -> u32 {
    // SAFETY: BASE maps the 4 KiB local APIC register block (uncached).
    unsafe { read_volatile(reg(off)) }
}

fn write(off: u64, v: u32) {
    // SAFETY: as in `read`.
    unsafe { write_volatile(reg(off), v) }
}

/// Enables the local APIC for MSI delivery. Returns false if the CPU has
/// none (then devices fall back to polling).
pub fn init(cpu_has_apic: bool) -> bool {
    if !cpu_has_apic {
        kprintln!("apic: no local APIC");
        return false;
    }
    // SAFETY: IA32_APIC_BASE exists when CPUID reports an APIC.
    let msr = unsafe { cpu::rdmsr(IA32_APIC_BASE) };
    let phys = msr & 0x000F_FFFF_FFFF_F000;
    let Ok(virt) = mem::map_mmio(phys, 4096) else {
        kprintln!("apic: cannot map the register block at {phys:#x}");
        return false;
    };
    BASE.store(virt, Ordering::Relaxed);
    let was_enabled = msr & APIC_GLOBAL_ENABLE != 0;
    if !was_enabled {
        // SAFETY: setting the global enable bit of the APIC we just mapped.
        unsafe { cpu::wrmsr(IA32_APIC_BASE, msr | APIC_GLOBAL_ENABLE) };
    }
    let lint0_before = read(REG_LINT0);
    // Keep the PIC's interrupts flowing through LINT0 (ExtINT), NMIs on
    // LINT1, accept every priority and software-enable the APIC.
    write(REG_LINT0, DELIVERY_EXTINT);
    write(REG_LINT1, DELIVERY_NMI);
    write(REG_TPR, 0);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);
    let id = read(REG_ID) >> 24;
    ID.store(id, Ordering::Relaxed);
    kprintln!(
        "apic: local APIC {} at {:#x}, version {:#x}, LINT0 {:#x} -> ExtINT{}",
        id,
        phys,
        read(REG_VERSION) & 0xFF,
        lint0_before,
        if was_enabled { "" } else { " (was globally disabled)" }
    );
    let _ = LVT_MASKED;
    true
}

pub fn available() -> bool {
    BASE.load(Ordering::Relaxed) != 0
}

pub fn id() -> u32 {
    ID.load(Ordering::Relaxed)
}

/// Acknowledges the interrupt being handled.
pub fn eoi() {
    if available() {
        write(REG_EOI, 0);
    }
}

/// Address and data of an MSI for `vector` delivered to this CPU (fixed
/// delivery, edge triggered).
pub fn msi_message(vector: u8) -> (u64, u32) {
    (0xFEE0_0000 | (id() as u64) << 12, vector as u32)
}
