//! OSyx system call ABI, version 5.
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

pub const ABI_VERSION: u32 = 5;

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
    /// `spawn(name: *const u8, name_len: usize, args: *const u8, args_len: usize) -> pid`
    ///
    /// Starts the boot module `name` as a child process. `args` holds the
    /// arguments after `argv[0]` as NUL-terminated strings back to back
    /// (`args_len` may be 0); `argv[0]` is `name`.
    pub const SPAWN: u64 = 6;
    /// `wait(pid: u64, status: *mut i32) -> pid`
    ///
    /// Blocks until the child `pid` (or any child if `pid == 0`) has exited,
    /// stores its exit status and releases it.
    pub const WAIT: u64 = 7;
    /// `yield() -> 0`: gives up the rest of the time slice.
    pub const YIELD: u64 = 8;
    /// `sleep(ns: u64) -> 0`: blocks for at least `ns` nanoseconds.
    pub const SLEEP: u64 = 9;
    /// `thread_spawn(entry: u64, stack_top: u64, arg: u64, fs_base: u64) -> tid`
    ///
    /// Starts a thread in the calling process at `entry` with `rsp =
    /// stack_top` (16-byte aligned), `rdi = arg` and FS base `fs_base`.
    pub const THREAD_SPAWN: u64 = 10;
    /// `thread_exit(code: i32, done_word: *mut u32) -> !`
    ///
    /// Ends the calling thread. If `done_word` is not null the kernel stores
    /// 1 there and wakes all futex waiters on it *after* the thread has left
    /// its user stack, so a joiner may free that stack. If it was the last
    /// thread, the process exits with `code`.
    pub const THREAD_EXIT: u64 = 11;
    /// `futex_wait(addr: *const u32, expected: u32, timeout_ns: u64) -> 0`
    ///
    /// Blocks while `*addr == expected` until a `futex_wake` on `addr`
    /// (`timeout_ns == 0`: no timeout). Fails with `WouldBlock` if the value
    /// differs, `TimedOut` on timeout. Spurious returns are allowed.
    pub const FUTEX_WAIT: u64 = 12;
    /// `futex_wake(addr: *const u32, count: u32) -> woken`
    pub const FUTEX_WAKE: u64 = 13;
    /// `set_fs_base(addr: u64) -> 0`: thread pointer for TLS (`fs:[0]`).
    pub const SET_FS_BASE: u64 = 14;
    /// `gettid() -> tid`
    pub const GETTID: u64 = 15;
    /// `open(path: *const u8, path_len: usize, flags: u64, mode: u32) -> handle`
    ///
    /// Opens a file, directory or device by path (`OPEN_*` flags). Relative
    /// paths start at the current directory. `mode` gives the permission
    /// bits of a file created by `OPEN_CREATE`. A directory can only be
    /// opened for reading and is then listed with `readdir`. Handles are per
    /// process, start at 3 and are closed when the process exits.
    pub const OPEN: u64 = 16;
    /// `close(handle) -> 0`
    pub const CLOSE: u64 = 17;
    /// `read(handle, buf: *mut u8, len: usize) -> bytes` (0 at end of file).
    /// `write` (number 1) also accepts file handles.
    pub const READ: u64 = 18;
    /// `seek(handle, offset: i64, whence: u64) -> new position` (`SEEK_*`).
    pub const SEEK: u64 = 19;
    /// `fstat(handle, out: *mut FileStat) -> 0`
    pub const FSTAT: u64 = 20;
    /// `stat(path, path_len, out: *mut FileStat) -> 0` (follows symbolic links)
    pub const STAT: u64 = 21;
    /// `readdir(handle, buf: *mut u8, len: usize) -> bytes`
    ///
    /// Fills `buf` with as many [`DirRecord`](super::DirRecord)s as fit and
    /// returns the number of bytes used; 0 means the listing is complete.
    /// `.` and `..` are not listed.
    pub const READDIR: u64 = 22;
    /// `mkdir(path, path_len, mode: u32) -> 0`
    pub const MKDIR: u64 = 23;
    /// `unlink(path, path_len) -> 0`: removes a file, symbolic link or device node.
    pub const UNLINK: u64 = 24;
    /// `rmdir(path, path_len) -> 0`: removes an empty directory.
    pub const RMDIR: u64 = 25;
    /// `rename(from, from_len, to, to_len) -> 0`: replaces an existing file
    /// at `to`; both paths must be on the same volume.
    pub const RENAME: u64 = 26;
    /// `ftruncate(handle, len: u64) -> 0`
    pub const FTRUNCATE: u64 = 27;
    /// `fsync(handle) -> 0`: writes cached data and metadata and flushes the disk.
    pub const FSYNC: u64 = 28;
    /// `chdir(path, path_len) -> 0`: sets the current directory.
    pub const CHDIR: u64 = 29;
    /// `getcwd(buf: *mut u8, len: usize) -> bytes`: the current directory
    /// (absolute, no trailing NUL); `InvalidArgument` if `buf` is too small.
    pub const GETCWD: u64 = 30;
    /// `symlink(target, target_len, path, path_len) -> 0`
    pub const SYMLINK: u64 = 31;
    /// `readlink(path, path_len, buf, buf_len) -> bytes` of the link target.
    pub const READLINK: u64 = 32;
    /// `lstat(path, path_len, out: *mut FileStat) -> 0` (does not follow a
    /// final symbolic link)
    pub const LSTAT: u64 = 33;
    /// `chmod(path, path_len, mode: u32) -> 0`: owner or root only.
    pub const CHMOD: u64 = 34;
    /// `mount(args: *const MountArgs) -> 0`: root only.
    pub const MOUNT: u64 = 35;
    /// `umount(path, path_len) -> 0`: root only; `Busy` while files on the
    /// volume are open or other volumes are mounted below it.
    pub const UMOUNT: u64 = 36;
    /// `getuid() -> uid | gid << 32`
    pub const GETUID: u64 = 37;
    /// `setuid(uid: u32, gid: u32) -> 0`: root only; children inherit it.
    pub const SETUID: u64 = 38;
    /// `statfs(path, path_len, out: *mut FsStat) -> 0`
    pub const STATFS: u64 = 39;
}

pub const OPEN_READ: u64 = 1 << 0;
pub const OPEN_WRITE: u64 = 1 << 1;
/// Create the file if it does not exist (requires `OPEN_WRITE`).
pub const OPEN_CREATE: u64 = 1 << 2;
/// Truncate to zero length (requires `OPEN_WRITE`).
pub const OPEN_TRUNCATE: u64 = 1 << 3;
/// Every write goes to the end of the file.
pub const OPEN_APPEND: u64 = 1 << 4;
/// With `OPEN_CREATE`: fail with `AlreadyExists` if the file exists.
pub const OPEN_EXCLUSIVE: u64 = 1 << 5;
/// Fail with `NotADirectory` unless the path names a directory.
pub const OPEN_DIRECTORY: u64 = 1 << 6;
/// Fail with `Loop` if the final component is a symbolic link.
pub const OPEN_NOFOLLOW: u64 = 1 << 7;
/// Match every path component case-insensitively (ASCII), as Windows does.
/// Exact matches win; otherwise the first case-insensitive match is used.
pub const OPEN_CASE_INSENSITIVE: u64 = 1 << 8;
pub const OPEN_ALL: u64 = (1 << 9) - 1;

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

/// Longest path accepted by the file system calls.
pub const MAX_PATH: usize = 1024;
/// Longest single path component.
pub const MAX_NAME_LEN: usize = 255;
/// Symbolic links followed while resolving one path.
pub const MAX_SYMLINKS: usize = 40;
/// Open handles per process.
pub const MAX_HANDLES: usize = 64;
/// Largest single `read`.
pub const MAX_READ: usize = 1 << 20;
/// First handle number used for files (0-2 are the console streams).
pub const FIRST_FILE_HANDLE: u64 = 3;

pub const KIND_FILE: u32 = 1;
pub const KIND_DIR: u32 = 2;
pub const KIND_SYMLINK: u32 = 3;
pub const KIND_CHAR_DEVICE: u32 = 4;
pub const KIND_BLOCK_DEVICE: u32 = 5;

/// Permission bits (`FileStat::mode`).
pub const MODE_SETUID: u32 = 0o4000;
pub const MODE_SETGID: u32 = 0o2000;
/// On a directory: only the owner of an entry (or of the directory, or
/// root) may remove or rename it.
pub const MODE_STICKY: u32 = 0o1000;
pub const MODE_PERMS: u32 = 0o7777;

/// FAT attribute bits (`FileStat::attributes`, 0 on other file systems).
pub const ATTR_READ_ONLY: u32 = 0x01;
pub const ATTR_HIDDEN: u32 = 0x02;
pub const ATTR_SYSTEM: u32 = 0x04;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_ARCHIVE: u32 = 0x20;

/// The super-user id: bypasses permission checks (not `setuid` limits).
pub const ROOT_UID: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    /// Inode number, unique within `dev`.
    pub ino: u64,
    /// Id of the mounted volume (stable while it stays mounted).
    pub dev: u64,
    /// Storage used, in 512-byte units.
    pub blocks: u64,
    /// Times in nanoseconds since 1970-01-01 UTC (0 = unknown).
    pub atime_ns: i64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    /// `KIND_*`.
    pub kind: u32,
    /// Permission bits (`MODE_*`, `0o777`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    /// FAT attribute bits (`ATTR_*`), 0 elsewhere.
    pub attributes: u32,
    /// Device number of a device node (0 otherwise).
    pub rdev: u32,
    pub reserved: u32,
}

/// One `readdir` record: this header, then `name_len` bytes of UTF-8 name,
/// padded so the next record starts 8-byte aligned (`record_len` bytes in all).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirRecord {
    pub ino: u64,
    /// `KIND_*` of the entry (without following symbolic links).
    pub kind: u32,
    pub reserved: u16,
    pub name_len: u16,
}

pub const DIR_RECORD_HEADER: usize = 16;

impl DirRecord {
    /// Total bytes of a record with a `name_len`-byte name.
    pub const fn record_len(name_len: usize) -> usize {
        (DIR_RECORD_HEADER + name_len + 7) & !7
    }

    pub fn encode(&self, name: &[u8], out: &mut [u8]) -> Option<usize> {
        let len = Self::record_len(name.len());
        if out.len() < len || name.len() != self.name_len as usize {
            return None;
        }
        out[0..8].copy_from_slice(&self.ino.to_le_bytes());
        out[8..12].copy_from_slice(&self.kind.to_le_bytes());
        out[12..14].copy_from_slice(&self.reserved.to_le_bytes());
        out[14..16].copy_from_slice(&self.name_len.to_le_bytes());
        out[16..16 + name.len()].copy_from_slice(name);
        out[16 + name.len()..len].fill(0);
        Some(len)
    }

    /// Parses the record at the start of `buf`: (record, name, record length).
    pub fn decode(buf: &[u8]) -> Option<(DirRecord, &[u8], usize)> {
        let h = buf.get(..DIR_RECORD_HEADER)?;
        let r = DirRecord {
            ino: u64::from_le_bytes(h[0..8].try_into().ok()?),
            kind: u32::from_le_bytes(h[8..12].try_into().ok()?),
            reserved: u16::from_le_bytes([h[12], h[13]]),
            name_len: u16::from_le_bytes([h[14], h[15]]),
        };
        let len = Self::record_len(r.name_len as usize);
        let name = buf.get(DIR_RECORD_HEADER..DIR_RECORD_HEADER + r.name_len as usize)?;
        (buf.len() >= len).then_some((r, name, len))
    }
}

/// Argument block of `mount`. Strings are UTF-8, not NUL-terminated.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MountArgs {
    /// Device path (e.g. `/dev/disks/vda2`); ignored by `tmpfs`.
    pub source: u64,
    pub source_len: u64,
    /// Existing directory to mount on.
    pub target: u64,
    pub target_len: u64,
    /// `"fat32"`, `"ext4"` or `"tmpfs"`.
    pub fstype: u64,
    pub fstype_len: u64,
    /// `MOUNT_*` flags.
    pub flags: u64,
    /// tmpfs: maximum bytes of memory (0 = default).
    pub size_limit: u64,
}

pub const MOUNT_READ_ONLY: u64 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsStat {
    /// Bytes per allocation unit.
    pub block_size: u64,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
    /// Volume id (`FileStat::dev`).
    pub dev: u64,
    /// `MOUNT_*` flags of the mount.
    pub flags: u64,
    /// NUL-padded file system type name.
    pub fstype: [u8; 16],
}

/// Maximum number of threads per process.
pub const MAX_THREADS: usize = 64;

/// Maximum length of a program name passed to `spawn`.
pub const MAX_NAME: usize = 31;
/// Maximum size of the `spawn` argument block.
pub const MAX_SPAWN_ARGS: usize = 4096;

pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

/// Monotonic clock since boot.
pub const CLOCK_MONOTONIC: u64 = 0;
/// Wall-clock time: nanoseconds since 1970-01-01 UTC (from the RTC).
pub const CLOCK_REALTIME: u64 = 1;

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
    /// The named object (e.g. a program for `spawn`) does not exist.
    NotFound = 7,
    /// `wait` was called but the caller has no (matching) child.
    NoChild = 8,
    /// `futex_wait`: the value no longer matched; retry.
    WouldBlock = 9,
    /// A blocking call reached its timeout.
    TimedOut = 10,
    /// A blocking call was interrupted because the process is exiting.
    Interrupted = 11,
    AlreadyExists = 12,
    NotADirectory = 13,
    IsADirectory = 14,
    DirectoryNotEmpty = 15,
    /// The volume is full.
    NoSpace = 16,
    /// The device reported an error or the volume is damaged.
    Io = 17,
    /// The file would exceed the file system's size limit.
    FileTooLarge = 18,
    /// The volume or handle does not allow writing.
    ReadOnly = 19,
    /// The process already has `MAX_HANDLES` open handles.
    TooManyHandles = 20,
    /// `rename` between different volumes.
    CrossDevice = 21,
    /// The object is in use (`umount` of a volume with open files, raw
    /// writes to a mounted disk, removing an open file on FAT).
    Busy = 22,
    /// Too many symbolic links while resolving a path, or `OPEN_NOFOLLOW`
    /// on a link.
    Loop = 23,
    /// The volume uses a feature this driver does not implement (it is
    /// refused rather than risked).
    Unsupported = 24,
}

const ALL_ERRNOS: [Errno; 24] = [
    Errno::InvalidArgument,
    Errno::Fault,
    Errno::NoMemory,
    Errno::BadHandle,
    Errno::NotSupported,
    Errno::PermissionDenied,
    Errno::NotFound,
    Errno::NoChild,
    Errno::WouldBlock,
    Errno::TimedOut,
    Errno::Interrupted,
    Errno::AlreadyExists,
    Errno::NotADirectory,
    Errno::IsADirectory,
    Errno::DirectoryNotEmpty,
    Errno::NoSpace,
    Errno::Io,
    Errno::FileTooLarge,
    Errno::ReadOnly,
    Errno::TooManyHandles,
    Errno::CrossDevice,
    Errno::Busy,
    Errno::Loop,
    Errno::Unsupported,
];

impl Errno {
    pub fn from_raw(v: u64) -> Option<Errno> {
        ALL_ERRNOS.iter().copied().find(|&e| e as u64 == v)
    }

    pub fn name(self) -> &'static str {
        match self {
            Errno::InvalidArgument => "invalid argument",
            Errno::Fault => "bad address",
            Errno::NoMemory => "out of memory",
            Errno::BadHandle => "bad handle",
            Errno::NotSupported => "not supported",
            Errno::PermissionDenied => "permission denied",
            Errno::NotFound => "not found",
            Errno::NoChild => "no child process",
            Errno::WouldBlock => "operation would block",
            Errno::TimedOut => "timed out",
            Errno::Interrupted => "interrupted",
            Errno::AlreadyExists => "already exists",
            Errno::NotADirectory => "not a directory",
            Errno::IsADirectory => "is a directory",
            Errno::DirectoryNotEmpty => "directory not empty",
            Errno::NoSpace => "no space left on volume",
            Errno::Io => "I/O error",
            Errno::FileTooLarge => "file too large",
            Errno::ReadOnly => "read-only",
            Errno::TooManyHandles => "too many open handles",
            Errno::CrossDevice => "cross-volume operation",
            Errno::Busy => "resource busy",
            Errno::Loop => "too many symbolic links",
            Errno::Unsupported => "unsupported file system feature",
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
        for (i, e) in ALL_ERRNOS.into_iter().enumerate() {
            assert_eq!(e as usize, i + 1, "errno values are dense");
            assert_eq!(decode(encode(Err(e))), Err(e));
            assert_eq!(Errno::from_raw(e as u64), Some(e));
        }
        assert_eq!(Errno::from_raw(0), None);
        assert_eq!(Errno::from_raw(ALL_ERRNOS.len() as u64 + 1), None);
        // Large unsigned values outside the error window are successes.
        assert_eq!(decode(u64::MAX - 5000), Ok(u64::MAX - 5000));
        assert_eq!(decode((-99i64) as u64), Err(Errno::NotSupported));
    }

    #[test]
    fn sysinfo_layout_is_stable() {
        assert_eq!(core::mem::size_of::<SystemInfo>(), 56);
        assert_eq!(core::mem::size_of::<FileStat>(), 88);
        assert_eq!(core::mem::size_of::<DirRecord>(), DIR_RECORD_HEADER);
        assert_eq!(core::mem::size_of::<MountArgs>(), 64);
        assert_eq!(core::mem::size_of::<FsStat>(), 72);
    }

    #[test]
    fn dir_records_round_trip_and_reject_truncation() {
        let mut buf = [0xAAu8; 64];
        let rec = DirRecord { ino: 1234, kind: KIND_FILE, reserved: 0, name_len: 9 };
        let n = rec.encode(b"hello.txt", &mut buf).unwrap();
        assert_eq!(n, 32);
        let (r, name, len) = DirRecord::decode(&buf).unwrap();
        assert_eq!((r, name, len), (rec, &b"hello.txt"[..], 32));
        assert!(DirRecord::decode(&buf[..31]).is_none());
        assert!(rec.encode(b"hello.txt", &mut buf[..31]).is_none());
        assert!(rec.encode(b"wrong", &mut buf).is_none());
        assert_eq!(DirRecord::record_len(0), 16);
        assert_eq!(DirRecord::record_len(1), 24);
    }
}
