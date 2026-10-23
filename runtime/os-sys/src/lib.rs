//! Raw OSyx system calls (ABI in `osyx_abi`).
//!
//! These wrappers are only compiled for the OSyx target
//! (`x86_64-unknown-none` user programs); on any other target the crate only
//! re-exports the ABI definitions. Every wrapper is a safe function: the
//! kernel validates all pointers and lengths, so passing a bad address yields
//! `Err(Errno::Fault)` rather than undefined behaviour in the caller.

#![no_std]

pub use osyx_abi::{
    DirRecord, Errno, FileStat, SystemInfo, ABI_VERSION, CLOCK_MONOTONIC, KIND_DIR, KIND_FILE, OPEN_APPEND, OPEN_CREATE,
    OPEN_EXCLUSIVE, OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, PROT_EXEC, PROT_READ, PROT_WRITE, SEEK_CUR, SEEK_END, SEEK_SET,
    STDERR, STDOUT,
};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod imp {
    use core::arch::asm;
    use osyx_abi::{decode, nr, Errno, FileStat, SystemInfo};

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

    #[inline]
    fn syscall4(n: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
        let ret: u64;
        // SAFETY: as in `syscall1`.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4,
                 lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    #[inline]
    fn syscall0(n: u64) -> u64 {
        let ret: u64;
        // SAFETY: as in `syscall1`.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        ret
    }

    /// Starts boot module `name` as a child. `args` is the argument block:
    /// NUL-terminated strings back to back (argv[0] is `name`).
    pub fn spawn(name: &str, args: &[u8]) -> Result<u64, Errno> {
        decode(syscall4(nr::SPAWN, name.as_ptr() as u64, name.len() as u64, args.as_ptr() as u64, args.len() as u64))
    }

    /// Waits for child `pid` (0 = any) and returns (pid, exit status).
    pub fn wait(pid: u64) -> Result<(u64, i32), Errno> {
        let mut status = 0i32;
        let child = decode(syscall2(nr::WAIT, pid, &mut status as *mut i32 as u64))?;
        Ok((child, status))
    }

    /// `wait` with an arbitrary status address (for validation tests).
    pub fn wait_raw(pid: u64, status_addr: usize) -> Result<u64, Errno> {
        decode(syscall2(nr::WAIT, pid, status_addr as u64))
    }

    pub fn yield_now() {
        syscall0(nr::YIELD);
    }

    /// Blocks for at least `ns` nanoseconds (timer resolution: 10 ms).
    pub fn sleep(ns: u64) {
        syscall1(nr::SLEEP, ns);
    }

    /// Starts a thread in this process at `entry` with `rsp = stack_top`,
    /// `rdi = arg` and FS base `fs_base`. Returns its thread id.
    pub fn thread_spawn(entry: usize, stack_top: usize, arg: usize, fs_base: usize) -> Result<u64, Errno> {
        decode(syscall4(nr::THREAD_SPAWN, entry as u64, stack_top as u64, arg as u64, fs_base as u64))
    }

    /// Ends the calling thread; the kernel then stores 1 to `done_word` (if
    /// not null) and wakes its futex waiters.
    pub fn thread_exit(code: i32, done_word: *const u32) -> ! {
        syscall2(nr::THREAD_EXIT, code as u32 as u64, done_word as u64);
        loop {
            core::hint::spin_loop();
        }
    }

    /// Blocks while `*word == expected` (0 = no timeout).
    pub fn futex_wait(word: *const u32, expected: u32, timeout_ns: u64) -> Result<(), Errno> {
        decode(syscall3(nr::FUTEX_WAIT, word as u64, expected as u64, timeout_ns)).map(|_| ())
    }

    /// Wakes up to `count` waiters on `word`; returns how many were woken.
    pub fn futex_wake(word: *const u32, count: u32) -> u64 {
        decode(syscall2(nr::FUTEX_WAKE, word as u64, count as u64)).unwrap_or(0)
    }

    pub fn set_fs_base(base: usize) -> Result<(), Errno> {
        decode(syscall1(nr::SET_FS_BASE, base as u64)).map(|_| ())
    }

    pub fn gettid() -> u64 {
        syscall0(nr::GETTID)
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

    // ---- files ----------------------------------------------------------

    /// Opens `path` (absolute) with `OPEN_*` flags; returns a handle.
    pub fn open(path: &str, flags: u64) -> Result<u64, Errno> {
        decode(syscall3(nr::OPEN, path.as_ptr() as u64, path.len() as u64, flags))
    }

    pub fn close(handle: u64) -> Result<(), Errno> {
        decode(syscall1(nr::CLOSE, handle)).map(|_| ())
    }

    /// Reads up to `buf.len()` bytes; 0 at end of file.
    pub fn read(handle: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        decode(syscall3(nr::READ, handle, buf.as_mut_ptr() as u64, buf.len() as u64)).map(|n| n as usize)
    }

    /// `read` into an arbitrary address (for validation tests).
    pub fn read_raw(handle: u64, addr: usize, len: usize) -> Result<usize, Errno> {
        decode(syscall3(nr::READ, handle, addr as u64, len as u64)).map(|n| n as usize)
    }

    /// Moves the file position (`SEEK_*`); returns the new position.
    pub fn seek(handle: u64, offset: i64, whence: u64) -> Result<u64, Errno> {
        decode(syscall3(nr::SEEK, handle, offset as u64, whence))
    }

    pub fn fstat(handle: u64) -> Result<FileStat, Errno> {
        let mut st = FileStat::default();
        decode(syscall2(nr::FSTAT, handle, &mut st as *mut FileStat as u64))?;
        Ok(st)
    }

    pub fn stat(path: &str) -> Result<FileStat, Errno> {
        let mut st = FileStat::default();
        decode(syscall3(nr::STAT, path.as_ptr() as u64, path.len() as u64, &mut st as *mut FileStat as u64))?;
        Ok(st)
    }

    /// Fills `buf` with directory records (see `osyx_abi::DirRecord`);
    /// returns the bytes used, 0 when the listing is complete.
    pub fn readdir(handle: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        decode(syscall3(nr::READDIR, handle, buf.as_mut_ptr() as u64, buf.len() as u64)).map(|n| n as usize)
    }

    fn path_call(n: u64, path: &str) -> Result<(), Errno> {
        decode(syscall2(n, path.as_ptr() as u64, path.len() as u64)).map(|_| ())
    }

    pub fn mkdir(path: &str) -> Result<(), Errno> {
        path_call(nr::MKDIR, path)
    }

    pub fn unlink(path: &str) -> Result<(), Errno> {
        path_call(nr::UNLINK, path)
    }

    pub fn rmdir(path: &str) -> Result<(), Errno> {
        path_call(nr::RMDIR, path)
    }

    pub fn rename(from: &str, to: &str) -> Result<(), Errno> {
        decode(syscall4(nr::RENAME, from.as_ptr() as u64, from.len() as u64, to.as_ptr() as u64, to.len() as u64))
            .map(|_| ())
    }

    pub fn ftruncate(handle: u64, len: u64) -> Result<(), Errno> {
        decode(syscall2(nr::FTRUNCATE, handle, len)).map(|_| ())
    }

    pub fn fsync(handle: u64) -> Result<(), Errno> {
        decode(syscall1(nr::FSYNC, handle)).map(|_| ())
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use imp::*;
