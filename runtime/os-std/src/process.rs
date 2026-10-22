//! Process control.

use crate::sys;

/// Terminates the process with `code`. Destructors of live values do not run.
pub fn exit(code: i32) -> ! {
    sys::exit(code)
}

/// Terminates the process abnormally (status 134, like SIGABRT on Unix).
pub fn abort() -> ! {
    sys::exit(134)
}

/// The kernel's identifier of the current process.
pub fn id() -> crate::error::Result<u64> {
    Ok(sys::sysinfo()?.pid)
}
