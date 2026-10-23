//! Legacy 8259A PIC pair and 8254 PIT: the fallback tick source when the
//! local APIC timer is unavailable (see `arch::timer`), and PIT channel 2 as
//! a calibration reference.

use super::cpu::{inb, outb};

pub const IRQ_BASE: u8 = 32;
pub const TIMER_VECTOR: u8 = IRQ_BASE;
pub const TIMER_HZ: u32 = 100;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIT_CH0: u16 = 0x40;
const PIT_CH2: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
/// NMI status and control: bit 0 gates PIT channel 2, bit 1 enables the
/// speaker, bit 5 reads channel 2's output.
const PORT_61: u16 = 0x61;
pub const PIT_FREQ: u32 = 1_193_182;

/// Remaps IRQ 0-15 to vectors 32-47, masks all of them and programs the PIT
/// to `TIMER_HZ`. [`enable_timer_irq`] lets the PIT tick through.
///
/// # Safety
/// Must run with interrupts disabled after the IDT is loaded.
pub unsafe fn init() {
    let io_wait = || outb(0x80, 0);
    outb(PIC1_CMD, 0x11); // ICW1: init, expect ICW4
    io_wait();
    outb(PIC2_CMD, 0x11);
    io_wait();
    outb(PIC1_DATA, IRQ_BASE); // ICW2: vector offsets
    io_wait();
    outb(PIC2_DATA, IRQ_BASE + 8);
    io_wait();
    outb(PIC1_DATA, 4); // ICW3: slave on IRQ2
    io_wait();
    outb(PIC2_DATA, 2);
    io_wait();
    outb(PIC1_DATA, 0x01); // ICW4: 8086 mode
    io_wait();
    outb(PIC2_DATA, 0x01);
    io_wait();
    outb(PIC1_DATA, 0xFF);
    outb(PIC2_DATA, 0xFF);

    let divisor = PIT_FREQ / TIMER_HZ;
    outb(PIT_CMD, 0x34); // channel 0, lobyte/hibyte, mode 2 (rate generator)
    outb(PIT_CH0, divisor as u8);
    outb(PIT_CH0, (divisor >> 8) as u8);
}

/// Unmasks IRQ0 (the PIT tick) and nothing else.
pub fn enable_timer_irq() {
    // SAFETY: OCW1 to the master PIC's mask register.
    unsafe { outb(PIC1_DATA, 0xFE) };
}

/// Masks every PIC interrupt.
pub fn disable_timer_irq() {
    // SAFETY: OCW1 to the master PIC's mask register.
    unsafe { outb(PIC1_DATA, 0xFF) };
}

/// Starts PIT channel 2 counting `count` (at most 65535) input clocks down
/// in mode 0 (output goes high at zero), with the speaker off.
pub fn channel2_start(count: u16) {
    // SAFETY: channel 2 and port 0x61 belong to the kernel; only the gate
    // and speaker bits are changed.
    unsafe {
        outb(PORT_61, (inb(PORT_61) & !0x02) | 0x01);
        outb(PIT_CMD, 0xB0); // channel 2, lobyte/hibyte, mode 0, binary
        outb(PIT_CH2, count as u8);
        outb(PIT_CH2, (count >> 8) as u8);
    }
}

/// Channel 2's output (high once its count reached zero).
pub fn channel2_done() -> bool {
    // SAFETY: reading port 0x61 has no side effects.
    unsafe { inb(PORT_61) & 0x20 != 0 }
}

/// Latches and reads channel 2's current count.
pub fn channel2_count() -> u16 {
    // SAFETY: counter latch command for channel 2, then its two bytes.
    unsafe {
        outb(PIT_CMD, 0x80);
        let lo = inb(PIT_CH2) as u16;
        let hi = inb(PIT_CH2) as u16;
        hi << 8 | lo
    }
}

/// Returns true if `irq` (0-15) is a spurious interrupt that must not be
/// acknowledged (IRQ7/IRQ15 without the in-service bit set).
pub fn is_spurious(irq: u8) -> bool {
    // SAFETY: OCW3 read of the in-service register.
    unsafe {
        match irq {
            7 => {
                outb(PIC1_CMD, 0x0B);
                inb(PIC1_CMD) & 0x80 == 0
            }
            15 => {
                outb(PIC2_CMD, 0x0B);
                if inb(PIC2_CMD) & 0x80 == 0 {
                    // The master did see an interrupt from the cascade line.
                    outb(PIC1_CMD, 0x20);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

pub fn end_of_interrupt(irq: u8) {
    // SAFETY: EOI command to the PIC(s) that delivered `irq`.
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, 0x20);
        }
        outb(PIC1_CMD, 0x20);
    }
}
