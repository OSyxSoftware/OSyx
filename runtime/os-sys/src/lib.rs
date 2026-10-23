//! Raw OSyx system calls (ABI in `osyx_abi`).
//!
//! These wrappers are only compiled for the OSyx target
//! (`x86_64-unknown-none` user programs); on any other target the crate only
//! re-exports the ABI definitions. Every wrapper is a safe function: the
//! kernel validates all pointers and lengths, so passing a bad address yields
//! `Err(Errno::Fault)` rather than undefined behaviour in the caller.

#![no_std]

pub use osyx_abi::{
    auxv, nr, startup, DirRecord, Errno, ExecArgs, FileStat, FsStat, MountArgs, SystemInfo, ABI_VERSION,
    CLOCK_MONOTONIC, CLOCK_REALTIME, KIND_BLOCK_DEVICE, KIND_CHAR_DEVICE, KIND_DIR, KIND_FILE, KIND_SYMLINK,
    MOUNT_READ_ONLY, OPEN_APPEND, OPEN_CASE_INSENSITIVE, OPEN_CREATE, OPEN_DIRECTORY, OPEN_EXCLUSIVE, OPEN_NOFOLLOW,
    OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, PROT_EXEC, PROT_READ, PROT_WRITE, SEEK_CUR, SEEK_END, SEEK_SET, STDERR,
    STDOUT,
};

pub use osyx_abi::{ipc, net, HANDLE_NONE, KILLED_STATUS, MAP_ALIGN_64K, MAP_FIXED, STDIN};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod imp {
    use core::arch::asm;
    use osyx_abi::ipc::{ChannelMsg, DisplayInfo, MountInfo, ProcInfo, WaitItem};
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
    fn syscall5(n: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
        let ret: u64;
        // SAFETY: as in `syscall1`.
        unsafe {
            asm!("syscall", inlateout("rax") n => ret, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5,
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

    fn exec_args(path: &str, argv: &[u8], envp: &[u8]) -> osyx_abi::ExecArgs {
        osyx_abi::ExecArgs {
            path: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            envp: envp.as_ptr() as u64,
            envp_len: envp.len() as u64,
        }
    }

    /// Starts program `path` (a name without `/` is searched in `/bin` and
    /// `/usr/bin`) as a child. `argv` (including `argv[0]`) and `envp` are
    /// blocks of NUL-terminated strings back to back.
    pub fn spawn(path: &str, argv: &[u8], envp: &[u8]) -> Result<u64, Errno> {
        let a = exec_args(path, argv, envp);
        decode(syscall1(nr::SPAWN, &a as *const osyx_abi::ExecArgs as u64))
    }

    /// Replaces the calling program (arguments as for [`spawn`]). Returns
    /// only if that failed, with the reason.
    pub fn execve(path: &str, argv: &[u8], envp: &[u8]) -> Errno {
        let a = exec_args(path, argv, envp);
        match decode(syscall1(nr::EXECVE, &a as *const osyx_abi::ExecArgs as u64)) {
            Err(e) => e,
            Ok(_) => Errno::NotSupported,
        }
    }

    /// `spawn` / `execve` with an arbitrary argument block address (for
    /// validation tests).
    pub fn exec_raw(n: u64, args_addr: usize) -> Result<u64, Errno> {
        decode(syscall1(n, args_addr as u64))
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

    /// Opens `path` with `OPEN_*` flags (`mode`: permissions of a created
    /// file); returns a handle.
    pub fn open(path: &str, flags: u64, mode: u32) -> Result<u64, Errno> {
        decode(syscall4(nr::OPEN, path.as_ptr() as u64, path.len() as u64, flags, mode as u64))
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

    pub fn mkdir(path: &str, mode: u32) -> Result<(), Errno> {
        decode(syscall3(nr::MKDIR, path.as_ptr() as u64, path.len() as u64, mode as u64)).map(|_| ())
    }

    pub fn lstat(path: &str) -> Result<FileStat, Errno> {
        let mut st = FileStat::default();
        decode(syscall3(nr::LSTAT, path.as_ptr() as u64, path.len() as u64, &mut st as *mut FileStat as u64))?;
        Ok(st)
    }

    pub fn chdir(path: &str) -> Result<(), Errno> {
        path_call(nr::CHDIR, path)
    }

    /// Writes the current directory into `buf`; returns its length.
    pub fn getcwd(buf: &mut [u8]) -> Result<usize, Errno> {
        decode(syscall2(nr::GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64)).map(|n| n as usize)
    }

    pub fn symlink(target: &str, path: &str) -> Result<(), Errno> {
        decode(syscall4(
            nr::SYMLINK,
            target.as_ptr() as u64,
            target.len() as u64,
            path.as_ptr() as u64,
            path.len() as u64,
        ))
        .map(|_| ())
    }

    /// Writes the link target into `buf`; returns its length (truncated to `buf`).
    pub fn readlink(path: &str, buf: &mut [u8]) -> Result<usize, Errno> {
        decode(syscall4(
            nr::READLINK,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ))
        .map(|n| n as usize)
    }

    pub fn chmod(path: &str, mode: u32) -> Result<(), Errno> {
        decode(syscall3(nr::CHMOD, path.as_ptr() as u64, path.len() as u64, mode as u64)).map(|_| ())
    }

    pub fn mount(source: &str, target: &str, fstype: &str, flags: u64, size_limit: u64) -> Result<(), Errno> {
        let a = osyx_abi::MountArgs {
            source: source.as_ptr() as u64,
            source_len: source.len() as u64,
            target: target.as_ptr() as u64,
            target_len: target.len() as u64,
            fstype: fstype.as_ptr() as u64,
            fstype_len: fstype.len() as u64,
            flags,
            size_limit,
        };
        decode(syscall1(nr::MOUNT, &a as *const osyx_abi::MountArgs as u64)).map(|_| ())
    }

    pub fn umount(path: &str) -> Result<(), Errno> {
        path_call(nr::UMOUNT, path)
    }

    /// (uid, gid) of the calling process.
    pub fn getuid() -> (u32, u32) {
        let v = syscall0(nr::GETUID);
        (v as u32, (v >> 32) as u32)
    }

    /// Changes identity (root only, permanent for this process and its children).
    pub fn setuid(uid: u32, gid: u32) -> Result<(), Errno> {
        decode(syscall2(nr::SETUID, uid as u64, gid as u64)).map(|_| ())
    }

    pub fn statfs(path: &str) -> Result<osyx_abi::FsStat, Errno> {
        let mut st = osyx_abi::FsStat::default();
        decode(syscall3(nr::STATFS, path.as_ptr() as u64, path.len() as u64, &mut st as *mut osyx_abi::FsStat as u64))?;
        Ok(st)
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

    // ---- ABI v7: objects, IPC, display, system ----------------------------------

    /// A pipe: (read end, write end).
    pub fn pipe() -> Result<(u64, u64), Errno> {
        let mut out = [0u64; 2];
        decode(syscall1(nr::PIPE, out.as_mut_ptr() as u64))?;
        Ok((out[0], out[1]))
    }

    /// `spawn` with the child's handles 0-2 taken from `stdio`
    /// (`HANDLE_NONE` keeps the console).
    pub fn spawn_stdio(path: &str, argv: &[u8], envp: &[u8], stdio: [u64; 3]) -> Result<u64, Errno> {
        let a = exec_args(path, argv, envp);
        decode(syscall2(nr::SPAWN_STDIO, &a as *const osyx_abi::ExecArgs as u64, stdio.as_ptr() as u64))
    }

    pub fn channel_listen(name: &str) -> Result<u64, Errno> {
        decode(syscall2(nr::CHANNEL_LISTEN, name.as_ptr() as u64, name.len() as u64))
    }

    pub fn channel_connect(name: &str) -> Result<u64, Errno> {
        decode(syscall2(nr::CHANNEL_CONNECT, name.as_ptr() as u64, name.len() as u64))
    }

    pub fn channel_accept(listener: u64) -> Result<u64, Errno> {
        decode(syscall1(nr::CHANNEL_ACCEPT, listener))
    }

    /// Sends `data` with `handles` duplicated to the receiver.
    pub fn channel_send(channel: u64, data: &[u8], handles: &[u64]) -> Result<(), Errno> {
        let m = ChannelMsg {
            data: data.as_ptr() as u64,
            len: data.len() as u64,
            handles: handles.as_ptr() as u64,
            handle_count: handles.len() as u64,
        };
        decode(syscall2(nr::CHANNEL_SEND, channel, &m as *const ChannelMsg as u64)).map(|_| ())
    }

    /// Receives one message into `buf`; received handles go to `handles`.
    /// Returns (bytes, handles received).
    pub fn channel_recv(channel: u64, buf: &mut [u8], handles: &mut [u64]) -> Result<(usize, usize), Errno> {
        let mut m = ChannelMsg {
            data: buf.as_mut_ptr() as u64,
            len: buf.len() as u64,
            handles: handles.as_mut_ptr() as u64,
            handle_count: handles.len() as u64,
        };
        let n = decode(syscall2(nr::CHANNEL_RECV, channel, &mut m as *mut ChannelMsg as u64))?;
        Ok((n as usize, m.handle_count as usize))
    }

    /// (pid, uid) of the channel's other end.
    pub fn channel_peer(channel: u64) -> Result<(u64, u32), Errno> {
        let v = decode(syscall1(nr::CHANNEL_PEER, channel))?;
        Ok((v & 0xFFFF_FFFF, (v >> 32) as u32))
    }

    pub fn shm_create(size: u64) -> Result<u64, Errno> {
        decode(syscall1(nr::SHM_CREATE, size))
    }

    /// Maps a shared memory object (unmap with [`unmap`] and its size).
    pub fn shm_map(handle: u64, writable: bool) -> Result<*mut u8, Errno> {
        let prot = osyx_abi::PROT_READ | if writable { osyx_abi::PROT_WRITE } else { 0 };
        decode(syscall2(nr::SHM_MAP, handle, prot)).map(|a| a as *mut u8)
    }

    pub fn shm_size(handle: u64) -> Result<u64, Errno> {
        decode(syscall1(nr::SHM_SIZE, handle))
    }

    /// Waits until an item is ready (`timeout_ns`: 0 polls, `u64::MAX`
    /// forever); returns how many are ready (their `revents` are set).
    pub fn wait_handles(items: &mut [WaitItem], timeout_ns: u64) -> Result<usize, Errno> {
        decode(syscall3(nr::WAIT_HANDLES, items.as_mut_ptr() as u64, items.len() as u64, timeout_ns))
            .map(|n| n as usize)
    }

    pub fn display_info() -> Result<DisplayInfo, Errno> {
        let mut d = DisplayInfo::default();
        decode(syscall1(nr::DISPLAY_INFO, &mut d as *mut DisplayInfo as u64))?;
        Ok(d)
    }

    /// Takes over the display; returns the mapped framebuffer.
    pub fn display_acquire() -> Result<*mut u32, Errno> {
        decode(syscall0(nr::DISPLAY_ACQUIRE)).map(|a| a as *mut u32)
    }

    pub fn display_release() -> Result<(), Errno> {
        decode(syscall0(nr::DISPLAY_RELEASE)).map(|_| ())
    }

    /// Presents a changed rectangle of the display buffer (owner only).
    pub fn display_flush(x: u32, y: u32, w: u32, h: u32) -> Result<(), Errno> {
        decode(syscall4(nr::DISPLAY_FLUSH, x as u64, y as u64, w as u64, h as u64)).map(|_| ())
    }

    /// Switches the display resolution (root only).
    pub fn display_set_mode(width: u32, height: u32) -> Result<(), Errno> {
        decode(syscall2(nr::DISPLAY_SET_MODE, width as u64, height as u64)).map(|_| ())
    }

    /// Sets the calling thread's GS base.
    pub fn set_gs_base(base: usize) -> Result<(), Errno> {
        decode(syscall1(nr::SET_GS_BASE, base as u64)).map(|_| ())
    }

    /// Changes the protection of whole pages of an anonymous mapping.
    ///
    /// # Safety
    /// Removing access from memory still in use makes later accesses fault.
    pub unsafe fn protect(addr: *mut u8, len: usize, prot: u64) -> Result<(), Errno> {
        decode(syscall3(nr::PROTECT, addr as u64, len as u64, prot)).map(|_| ())
    }

    /// Anonymous zeroed memory, optionally at a fixed address
    /// (`MAP_FIXED`) or 64 KiB aligned (`MAP_ALIGN_64K`).
    pub fn map_ex(addr: usize, len: usize, prot: u64, flags: u64) -> Result<*mut u8, Errno> {
        decode(syscall4(nr::MAP_EX, addr as u64, len as u64, prot, flags)).map(|a| a as *mut u8)
    }

    /// Fills `out` with process records; returns how many.
    pub fn proc_list(out: &mut [ProcInfo]) -> Result<usize, Errno> {
        let bytes = core::mem::size_of_val(out) as u64;
        decode(syscall2(nr::PROC_LIST, out.as_mut_ptr() as u64, bytes))
            .map(|n| n as usize / core::mem::size_of::<ProcInfo>())
    }

    /// Fills `out` with mount records; returns how many.
    pub fn mount_list(out: &mut [MountInfo]) -> Result<usize, Errno> {
        let bytes = core::mem::size_of_val(out) as u64;
        decode(syscall2(nr::MOUNT_LIST, out.as_mut_ptr() as u64, bytes))
            .map(|n| n as usize / core::mem::size_of::<MountInfo>())
    }

    pub fn device_list(out: &mut [osyx_abi::ipc::DeviceInfo]) -> Result<usize, Errno> {
        decode(syscall2(nr::DEVICE_LIST, out.as_mut_ptr() as u64, core::mem::size_of_val(out) as u64))
            .map(|n| n as usize / core::mem::size_of::<osyx_abi::ipc::DeviceInfo>())
    }

    /// `POWER_OFF` / `POWER_REBOOT`; returns only on failure.
    pub fn power(cmd: u64) -> Errno {
        match decode(syscall1(nr::POWER, cmd)) {
            Err(e) => e,
            Ok(_) => Errno::NotSupported,
        }
    }

    pub fn kill(pid: u64) -> Result<(), Errno> {
        decode(syscall1(nr::KILL, pid)).map(|_| ())
    }

    use osyx_abi::net::{NetConfig, SockAddr};

    pub fn socket(kind: u64) -> Result<u64, Errno> {
        decode(syscall1(nr::SOCKET, kind))
    }

    pub fn sock_bind(h: u64, addr: &SockAddr) -> Result<(), Errno> {
        decode(syscall2(nr::SOCK_BIND, h, addr as *const SockAddr as u64)).map(|_| ())
    }

    pub fn sock_connect(h: u64, addr: &SockAddr) -> Result<(), Errno> {
        decode(syscall2(nr::SOCK_CONNECT, h, addr as *const SockAddr as u64)).map(|_| ())
    }

    pub fn sock_listen(h: u64, backlog: u32) -> Result<(), Errno> {
        decode(syscall2(nr::SOCK_LISTEN, h, backlog as u64)).map(|_| ())
    }

    pub fn sock_accept(h: u64) -> Result<(u64, SockAddr), Errno> {
        let mut peer = SockAddr::default();
        let c = decode(syscall2(nr::SOCK_ACCEPT, h, &mut peer as *mut SockAddr as u64))?;
        Ok((c, peer))
    }

    pub fn sock_send(h: u64, buf: &[u8], to: Option<&SockAddr>, flags: u64) -> Result<usize, Errno> {
        let to = to.map_or(0, |a| a as *const SockAddr as u64);
        decode(syscall5(nr::SOCK_SEND, h, buf.as_ptr() as u64, buf.len() as u64, to, flags)).map(|n| n as usize)
    }

    pub fn sock_recv(h: u64, buf: &mut [u8], flags: u64) -> Result<(usize, SockAddr), Errno> {
        let mut from = SockAddr::default();
        let n = decode(syscall5(
            nr::SOCK_RECV,
            h,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            &mut from as *mut SockAddr as u64,
            flags,
        ))?;
        Ok((n as usize, from))
    }

    pub fn sock_ctl(h: u64, op: u64, arg: u64) -> Result<u64, Errno> {
        decode(syscall3(nr::SOCK_CTL, h, op, arg))
    }

    /// IPv4 addresses of `name`; returns how many were stored in `out`.
    pub fn resolve(name: &str, out: &mut [[u8; 4]]) -> Result<usize, Errno> {
        decode(syscall4(
            nr::RESOLVE,
            name.as_ptr() as u64,
            name.len() as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
        ))
        .map(|n| n as usize)
    }

    /// Fills `out` with `kind` records (`net::query`); returns how many.
    pub fn net_query<T: Copy>(kind: u64, out: &mut [T]) -> Result<usize, Errno> {
        let bytes = core::mem::size_of_val(out) as u64;
        decode(syscall3(nr::NET_QUERY, kind, out.as_mut_ptr() as u64, bytes))
            .map(|n| n as usize / core::mem::size_of::<T>())
    }

    pub fn net_config(c: &NetConfig) -> Result<(), Errno> {
        decode(syscall1(nr::NET_CONFIG, c as *const NetConfig as u64)).map(|_| ())
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use imp::*;
