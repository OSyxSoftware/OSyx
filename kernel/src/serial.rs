//! COM1 serial console (polling 16550) and the kernel logging macros.

use crate::arch::cpu::{inb, outb};
use crate::sync::{IrqSpinLock, KMutex};
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
    let mut log = LOG.lock();
    let _ = write!(log, "[{:5}.{:03}] ", ms / 1000, ms % 1000);
    let _ = log.write_fmt(args);
    let _ = log.write_str("\n");
}

/// The last `LOG_MAX` bytes of the kernel log in a fixed ring (no heap:
/// logging must work before it exists and from the allocator itself),
/// copied to `/run/kernel.log` by [`start_log_file`] for AXIS `dmesg`.
const LOG_MAX: usize = 256 * 1024;
static LOG: IrqSpinLock<KernelLog> = IrqSpinLock::new(KernelLog { buf: [0; LOG_MAX], written: 0 });

struct KernelLog {
    buf: [u8; LOG_MAX],
    /// Bytes ever written; the ring holds the last `LOG_MAX` of them.
    written: u64,
}

impl fmt::Write for KernelLog {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            self.buf[(self.written % LOG_MAX as u64) as usize] = b;
            self.written += 1;
        }
        Ok(())
    }
}

/// Rewrites `/run/kernel.log` (and the live stick's copy) every two seconds
/// while the log grows.
pub fn start_log_file() {
    sync_log_file();
    crate::sched::spawn_kernel("klog", log_file_thread, 0, 0);
}

/// Copies the log ring into `text`; returns the byte count it ends at.
fn snapshot(text: &mut alloc::vec::Vec<u8>) -> u64 {
    let log = LOG.lock();
    text.clear();
    let start = log.written.saturating_sub(LOG_MAX as u64);
    for i in start..log.written {
        text.push(log.buf[(i % LOG_MAX as u64) as usize]);
    }
    log.written
}

/// Log bytes already on the stick; also serialises the writers of the copy.
static ON_STICK: KMutex<u64> = KMutex::new(u64::MAX);

/// Live boot: writes the log to the stick's EFI partition now, from the
/// calling thread, so the log of a real machine can be read on another
/// computer (`osyx-kernel.log` in the stick's root folder). Called before
/// steps that can hang a real machine (driver attach, firmware start), so
/// the copy ends just before them; the klog thread refreshes it as well.
pub fn sync_log_file() {
    use osyx_abi::{OPEN_CREATE, OPEN_TRUNCATE, OPEN_WRITE};
    let mut on_stick = ON_STICK.lock();
    let cx = crate::vfs::Lookup::kernel();
    if crate::vfs::stat(&cx, "/boot/EFI/OSYX/BIN/rootfs.z").is_err() {
        return;
    }
    // Full capacity up front: `snapshot` must not allocate under the log
    // lock (the allocator can log).
    let mut text = alloc::vec::Vec::with_capacity(LOG_MAX);
    let written = snapshot(&mut text);
    if written == *on_stick {
        return;
    }
    if let Ok(mut f) = crate::vfs::open(&cx, "/boot/osyx-kernel.log", OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE, 0o644) {
        if f.write(&text).is_ok() && f.sync().is_ok() {
            *on_stick = written;
        }
    }
}

fn log_file_thread(_: u64, _: u64) {
    use osyx_abi::{OPEN_CREATE, OPEN_TRUNCATE, OPEN_WRITE};
    let mut saved = u64::MAX;
    let mut text = alloc::vec::Vec::with_capacity(LOG_MAX);
    loop {
        let written = snapshot(&mut text);
        if written != saved {
            let cx = crate::vfs::Lookup::kernel();
            if let Ok(mut f) = crate::vfs::open(&cx, "/run/kernel.log", OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE, 0o644) {
                let _ = f.write(&text);
                saved = written;
            }
        }
        sync_log_file();
        crate::arch::timer::sleep_ms(2000);
    }
}

/// Kernel log line on the serial console (and the early framebuffer log
/// while it is shown), prefixed with the uptime.
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => { $crate::serial::_print(format_args!($($arg)*)) };
}
