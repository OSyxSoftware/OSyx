//! OSyx system call ABI, version 1.
//!
//! Calling convention (x86-64 `syscall` instruction):
//! * `rax` = system call number, arguments in `rdi, rsi, rdx, r10, r8, r9`.
//! * `rcx` and `r11` are clobbered by the CPU; all other registers are preserved.
//! * Return value in `rax`: a value in `0..=MAX_RESULT` is success; values
//!   `-1..=-4095` (as `i64`) are `-(Errno)`.
//!
//! The kernel never trusts user pointers: every buffer is range checked
//! against the user half of the address space and accessed with fault
//! recovery, so a bad pointer yields [`Errno::Fault`] instead of a crash.
//!
//! Process start-up state (System V style):
//! `rsp` -> `argc`, `argv[0..argc]`, `NULL`, `envp[..]`, `NULL`; `rsp` is
//! 16-byte aligned; all other general purpose registers are zero.

#![no_std]

pub const ABI_VERSION: u32 = 1;

/// Highest user virtual address + 1 (the canonical lower half).
pub const USER_END: u64 = 0x0000_8000_0000_0000;

pub mod nr {
    /// `exit(code: i32) -> !`
    pub const EXIT: u64 = 0;
    /// `write(fd: u64, buf: *const u8, len: usize) -> usize`
    pub const WRITE: u64 = 1;
    /// `map(len: usize, prot: u64) -> *mut u8`: zeroed anonymous memory.
    pub const MAP: u64 = 2;
    /// `unmap(addr: *mut u8, len: usize) -> 0`: must match one whole `map`.
    pub const UNMAP: u64 = 3;
    /// `sysinfo(out: *mut SystemInfo) -> 0`
    pub const SYSINFO: u64 = 4;
    /// `clock(id: u64) -> u64` nanoseconds.
    pub const CLOCK: u64 = 5;
}

pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

/// Monotonic clock since boot.
pub const CLOCK_MONOTONIC: u64 = 0;

pub const MAX_WRITE: usize = 1 << 20;

#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Errno {
    /// An argument is malformed or out of range.
    InvalidArgument = 1,
    /// A user pointer is outside user space or not mapped.
    Fault = 2,
    /// Not enough memory.
    NoMemory = 3,
    /// The handle / file descriptor does not exist.
    BadHandle = 4,
    /// The system call number is unknown or the operation is unsupported.
    NotSupported = 5,
    /// The operation violates a security policy (e.g. writable + executable).
    PermissionDenied = 6,
}

impl Errno {
    pub fn from_raw(v: u64) -> Option<Errno> {
        Some(match v {
            1 => Errno::InvalidArgument,
            2 => Errno::Fault,
            3 => Errno::NoMemory,
            4 => Errno::BadHandle,
            5 => Errno::NotSupported,
            6 => Errno::PermissionDenied,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Errno::InvalidArgument => "invalid argument",
            Errno::Fault => "bad address",
            Errno::NoMemory => "out of memory",
            Errno::BadHandle => "bad handle",
            Errno::NotSupported => "not supported",
            Errno::PermissionDenied => "permission denied",
        }
    }
}

/// Encodes a kernel result into the `rax` return value.
pub fn encode(r: Result<u64, Errno>) -> u64 {
    match r {
        Ok(v) => v,
        Err(e) => (-(e as i64)) as u64,
    }
}

/// Decodes a raw `rax` return value.
pub fn decode(raw: u64) -> Result<u64, Errno> {
    let signed = raw as i64;
    if (-4095..0).contains(&signed) {
        Err(Errno::from_raw((-signed) as u64).unwrap_or(Errno::NotSupported))
    } else {
        Ok(raw)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemInfo {
    pub abi_version: u32,
    pub page_size: u32,
    pub total_memory: u64,
    pub free_memory: u64,
    pub uptime_ns: u64,
    pub pid: u64,
    /// NUL-padded UTF-8 kernel version.
    pub kernel_version: [u8; 16],
}

/// Exit status reported when the kernel terminates a process because of a
/// CPU exception: `EXCEPTION_EXIT_BASE | vector`.
pub const EXCEPTION_EXIT_BASE: i32 = 0x4000_0000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_encoding_round_trips() {
        for v in [0u64, 1, 4096, u64::MAX >> 1] {
            assert_eq!(decode(encode(Ok(v))), Ok(v));
        }
        for e in [
            Errno::InvalidArgument,
            Errno::Fault,
            Errno::NoMemory,
            Errno::BadHandle,
            Errno::NotSupported,
            Errno::PermissionDenied,
        ] {
            assert_eq!(decode(encode(Err(e))), Err(e));
            assert_eq!(Errno::from_raw(e as u64), Some(e));
        }
        // Large unsigned values outside the error window are successes.
        assert_eq!(decode(u64::MAX - 5000), Ok(u64::MAX - 5000));
        assert_eq!(decode((-9i64) as u64), Err(Errno::NotSupported));
    }

    #[test]
    fn sysinfo_layout_is_stable() {
        assert_eq!(core::mem::size_of::<SystemInfo>(), 56);
    }
}
