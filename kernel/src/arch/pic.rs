//! Legacy 8259A PIC pair and 8254 PIT, used as the initial timer source.
//! (The local APIC timer will replace this once ACPI/MADT parsing exists.)

use super::cpu::{inb, outb};

pub const IRQ_BASE: u8 = 32;
pub const TIMER_VECTOR: u8 = IRQ_BASE;
pub const TIMER_HZ: u32 = 100;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIT_CH0: u16 = 0x40;
const PIT_CMD: u16 = 0x43;
const PIT_FREQ: u32 = 1_193_182;

/// Remaps IRQ 0-15 to vectors 32-47, masks everything except the timer and
/// programs the PIT to `TIMER_HZ`.
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
    outb(PIC1_DATA, 0xFE); // only IRQ0 (timer)
    outb(PIC2_DATA, 0xFF);

    let divisor = PIT_FREQ / TIMER_HZ;
    outb(PIT_CMD, 0x34); // channel 0, lobyte/hibyte, mode 2 (rate generator)
    outb(PIT_CH0, divisor as u8);
    outb(PIT_CH0, (divisor >> 8) as u8);
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
