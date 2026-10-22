//! Polling 16550 UART driver on COM1, used for boot diagnostics both before
//! and after ExitBootServices (it never calls firmware).

use core::arch::asm;
use core::fmt;

const COM1: u16 = 0x3F8;

/// # Safety
/// Port I/O; the caller must own the port (true for COM1 in the loader).
pub unsafe fn outb(port: u16, v: u8) {
    asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// See [`outb`].
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

pub fn init() {
    // SAFETY: standard 16550 programming sequence on the legacy COM1 ports.
    unsafe {
        outb(COM1 + 1, 0x00); // no interrupts
        outb(COM1 + 3, 0x80); // DLAB on
        outb(COM1, 0x01); // divisor 1 = 115200 baud
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03); // 8N1, DLAB off
        outb(COM1 + 2, 0xC7); // FIFO on, cleared, 14-byte threshold
        outb(COM1 + 4, 0x03); // DTR + RTS
    }
}

fn write_byte(b: u8) {
    // SAFETY: COM1 line-status / data registers. The bounded wait keeps a
    // missing UART from hanging the boot.
    unsafe {
        for _ in 0..100_000 {
            if inb(COM1 + 5) & 0x20 != 0 {
                break;
            }
        }
        outb(COM1, b);
    }
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                write_byte(b'\r');
            }
            write_byte(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::serial::Serial, "[osyx-boot] ");
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}

/// Fixed-capacity string for formatting without an allocator.
pub struct StrBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StrBuf<N> {
    pub const fn new() -> Self {
        StrBuf { buf: [0; N], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        // Only whole UTF-8 sequences are ever appended (see write_str).
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StrBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            let mut tmp = [0u8; 4];
            let enc = ch.encode_utf8(&mut tmp).as_bytes();
            if self.len + enc.len() > N {
                return Err(fmt::Error);
            }
            self.buf[self.len..self.len + enc.len()].copy_from_slice(enc);
            self.len += enc.len();
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! fmt_buf {
    ($n:expr, $($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut b = $crate::serial::StrBuf::<$n>::new();
        let _ = write!(b, $($arg)*);
        b
    }};
}
