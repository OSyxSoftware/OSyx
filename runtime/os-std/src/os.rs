//! OSyx-specific functionality.

use crate::error::Result;
use crate::sys;
pub use osyx_abi::SystemInfo;

/// Kernel and memory information (`sysinfo` system call).
pub fn sysinfo() -> Result<SystemInfo> {
    sys::sysinfo()
}

/// Kernel version string from a [`SystemInfo`].
pub fn kernel_version(info: &SystemInfo) -> &str {
    let end = info.kernel_version.iter().position(|&b| b == 0).unwrap_or(info.kernel_version.len());
    core::str::from_utf8(&info.kernel_version[..end]).unwrap_or("?")
}

/// The (user id, group id) of the calling process.
pub fn getuid() -> (u32, u32) {
    sys::fs::getuid()
}

/// Changes the process's identity; only root (uid 0) may, and the change is
/// permanent for this process and inherited by its children.
pub fn setuid(uid: u32, gid: u32) -> Result<()> {
    sys::fs::setuid(uid, gid)
}

/// Capacity and type of the volume holding `path`.
pub fn statfs(path: &str) -> Result<osyx_abi::FsStat> {
    sys::fs::statfs(path)
}

/// Mounts a file system (root only): `fstype` is "tmpfs" (with an optional
/// memory limit in bytes), "fat32" or "ext4" (with a block device node such
/// as `/dev/disks/vda2` as `source`).
pub fn mount(source: &str, target: &str, fstype: &str, read_only: bool, size_limit: u64) -> Result<()> {
    let flags = if read_only { osyx_abi::MOUNT_READ_ONLY } else { 0 };
    sys::fs::mount(source, target, fstype, flags, size_limit)
}

/// Unmounts the volume mounted at `target` (root only).
pub fn unmount(target: &str) -> Result<()> {
    sys::fs::umount(target)
}

/// Statistics of the OSyx Std heap (capacity obtained from the kernel, bytes
/// in use, number of free blocks).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn heap_stats() -> osyx_mm::heap::HeapStats {
    sys::heap_stats()
}

/// Value of entry `key` of the auxiliary vector the kernel passed at start-up
/// (`osyx_abi::auxv::AT_*`), e.g. `AT_ENTRY` or `AT_PAGESZ`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn aux_value(key: u64) -> Option<u64> {
    sys::aux_value(key)
}

pub use osyx_abi::auxv;

/// Low-level system call access for tests and runtime code.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod raw {
    pub use os_sys::{fault_handler,fault_return};
    pub use osyx_abi::fault;
    pub use os_sys::{
        close, discard, exec_raw, execve, futex_wait, futex_wake, gettid, map, map_ex, nr, open, protect, raw_syscall, read, read_raw,
        set_gs_base, spawn, sysinfo_raw, thread_exit, unmap, wait_raw, write, write_raw, Errno, ExecArgs, ABI_VERSION,
        HANDLE_NONE, KILLED_STATUS, MAP_ALIGN_64K, MAP_FIXED, OPEN_CREATE, OPEN_DIRECTORY, OPEN_READ, OPEN_WRITE, PROT_EXEC,
        PROT_READ, PROT_WRITE,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_version_stops_at_nul() {
        let mut info = SystemInfo::default();
        info.kernel_version[..5].copy_from_slice(b"0.1.0");
        assert_eq!(kernel_version(&info), "0.1.0");
        info.kernel_version = [b'9'; 16];
        assert_eq!(kernel_version(&info).len(), 16);
    }

    #[test]
    fn host_backend_reports_sysinfo_unsupported() {
        assert_eq!(sysinfo().unwrap_err().kind(), crate::error::ErrorKind::Unsupported);
    }
}
