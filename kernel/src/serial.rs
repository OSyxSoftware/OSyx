//! COM1 serial console (polling 16550) and the kernel logging macros.

use crate::arch::cpu::{inb, outb};
use crate::sync::IrqSpinLock;
use core::fmt;

const COM1: u16 = 0x3F8;

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    fn write_byte(&mut self, b: u8) {
        // SAFETY: COM1 is owned by the kernel console. The bounded wait keeps a
        // missing UART from hanging the system.
        unsafe {
            for _ in 0..100_000 {
                if inb(self.base + 5) & 0x20 != 0 {
                    break;
                }
            }
            outb(self.base, b);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}

pub static SERIAL: IrqSpinLock<SerialPort> = IrqSpinLock::new(SerialPort { base: COM1 });

pub fn init() {
    // SAFETY: standard 16550 initialisation (115200 8N1, FIFO on, no IRQs).
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x01);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x03);
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    let ms = crate::interrupts::uptime_ms();
    let mut port = SERIAL.lock();
    let _ = write!(port, "[{:5}.{:03}] ", ms / 1000, ms % 1000);
    let _ = port.write_fmt(args);
    let _ = port.write_str("\n");
    // Under the serial lock, so both consoles show lines in the same order.
    crate::earlycon::print(ms, args);
}

/// Kernel log line on the serial console (and the early framebuffer log
/// while it is shown), prefixed with the uptime.
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => { $crate::serial::_print(format_args!($($arg)*)) };
}
