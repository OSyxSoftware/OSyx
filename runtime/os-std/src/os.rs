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

/// Statistics of the OSyx Std heap (capacity obtained from the kernel, bytes
/// in use, number of free blocks).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn heap_stats() -> osyx_mm::heap::HeapStats {
    sys::heap_stats()
}

/// Low-level system call access for tests and runtime code.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod raw {
    pub use os_sys::{map, raw_syscall, sysinfo_raw, unmap, write_raw, Errno, PROT_EXEC, PROT_READ, PROT_WRITE};
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
