//! QEMU `isa-debug-exit` device (iobase 0xf4) used by automated tests.
//! QEMU exits with status `(code << 1) | 1`. Without the device the write is
//! ignored and the caller falls through to its halt loop.

use crate::arch::cpu;

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum ExitCode {
    /// QEMU exit status 33.
    Success = 0x10,
    /// QEMU exit status 35.
    TestFailure = 0x11,
    /// QEMU exit status 37.
    Panic = 0x12,
}

pub fn exit(code: ExitCode) {
    // SAFETY: port 0xf4 is only the debug-exit device in our QEMU setup; on
    // real hardware nothing is decoded there.
    unsafe { cpu::outl(0xF4, code as u32) };
}
