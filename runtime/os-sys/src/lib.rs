//! Raw OSyx system calls (ABI in `osyx_abi`).
//!
//! These wrappers are only compiled for the OSyx target
//! (`x86_64-unknown-none` user programs); on any other target the crate only
//! re-exports the ABI definitions. Every wrapper is a safe function: the
//! kernel validates all pointers and lengths, so passing a bad address yields
//! `Err(Errno::Fault)` rather than undefined behaviour in the caller.

#![no_std]

pub use osyx_abi::{Errno, SystemInfo, ABI_VERSION, CLOCK_MONOTONIC, PROT_EXEC, PROT_READ, PROT_WRITE, STDERR, STDOUT};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod imp {
    use core::arch::asm;
    use osyx_abi::{decode, nr, Errno, SystemInfo};

    #[inline]
    fn syscall1(n: u64, a1: u64) -> u64 {
        let ret: u64;
        // SAFETY: `syscall` transfers to the kernel, which preserves all
        // registers except rax (result), rcx and r11, and never touches user
        // memory it has not validated.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, in("rdi") a1,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline]
    fn syscall2(n: u64, a1: u64, a2: u64) -> u64 {
        let ret: u64;
        // SAFETY: as in `syscall1`.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, in("rdi") a1, in("rsi") a2,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline]
    fn syscall3(n: u64, a1: u64, a2: u64, a3: u64) -> u64 {
        let ret: u64;
        // SAFETY: as in `syscall1`.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, in("rdi") a1, in("rsi") a2, in("rdx") a3,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    /// Terminates the calling process.
    pub fn exit(code: i32) -> ! {
        syscall1(nr::EXIT, code as u32 as u64);
        // The kernel never returns from exit.
        loop {
            core::hint::spin_loop();
        }
    }

    pub fn write(fd: u64, buf: &[u8]) -> Result<usize, Errno> {
        decode(syscall3(nr::WRITE, fd, buf.as_ptr() as u64, buf.len() as u64)).map(|n| n as usize)
    }

    /// `write` with an arbitrary address, e.g. to test the kernel's pointer
    /// validation. Safe: the kernel checks the range before reading it.
    pub fn write_raw(fd: u64, addr: usize, len: usize) -> Result<usize, Errno> {
        decode(syscall3(nr::WRITE, fd, addr as u64, len as u64)).map(|n| n as usize)
    }

    /// Maps `len` bytes (rounded up to pages) of zeroed memory.
    pub fn map(len: usize, prot: u64) -> Result<*mut u8, Errno> {
        decode(syscall2(nr::MAP, len as u64, prot)).map(|a| a as *mut u8)
    }

    /// Unmaps a region returned by [`map`] (the whole region, same length).
    ///
    /// # Safety
    /// No live reference may point into the region.
    pub unsafe fn unmap(addr: *mut u8, len: usize) -> Result<(), Errno> {
        decode(syscall2(nr::UNMAP, addr as u64, len as u64)).map(|_| ())
    }

    pub fn sysinfo() -> Result<SystemInfo, Errno> {
        let mut info = SystemInfo::default();
        decode(syscall1(nr::SYSINFO, &mut info as *mut SystemInfo as u64))?;
        Ok(info)
    }

    /// `sysinfo` with an arbitrary output address (for validation tests).
    pub fn sysinfo_raw(addr: usize) -> Result<(), Errno> {
        decode(syscall1(nr::SYSINFO, addr as u64)).map(|_| ())
    }

    pub fn clock(id: u64) -> Result<u64, Errno> {
        decode(syscall1(nr::CLOCK, id))
    }

    /// Issues an arbitrary system call number (for ABI tests).
    pub fn raw_syscall(n: u64, a1: u64, a2: u64, a3: u64) -> Result<u64, Errno> {
        decode(syscall3(n, a1, a2, a3))
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use imp::*;
