//! System information and control: processes, mounted volumes, the
//! display, power.

use crate::error::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;
pub use os_sys::ipc::{
    field_str, pixel, DisplayInfo, MountInfo, ProcInfo, BACKEND_LINEAR, BACKEND_VIRTIO_GPU, POWER_OFF, POWER_REBOOT,
    PROC_RUNNING, PROC_ZOMBIE,
};

/// Every process (running and not yet collected).
pub fn processes() -> Result<Vec<ProcInfo>> {
    let mut buf = alloc::vec![ProcInfo::default(); 128];
    let n = os_sys::proc_list(&mut buf).map_err(Error::from)?;
    buf.truncate(n);
    Ok(buf)
}

/// Every mounted volume, outermost first.
pub fn mounts() -> Result<Vec<MountInfo>> {
    let mut buf = alloc::vec![MountInfo::default(); 64];
    let n = os_sys::mount_list(&mut buf).map_err(Error::from)?;
    buf.truncate(n);
    Ok(buf)
}

pub fn devices() -> Result<Vec<os_sys::ipc::DeviceInfo>> {
    let mut buf = alloc::vec![os_sys::ipc::DeviceInfo::default(); 512];
    let n = os_sys::device_list(&mut buf).map_err(Error::from)?;
    buf.truncate(n);
    Ok(buf)
}

/// A process's name.
pub fn process_name(p: &ProcInfo) -> String {
    String::from(field_str(&p.name))
}

/// Ends process `pid` (root, or a process of the same user).
pub fn kill(pid: u64) -> Result<()> {
    os_sys::kill(pid).map_err(Error::from)
}

/// Unmounts everything and switches the machine off (root only). Returns
/// only if that is not possible.
pub fn power_off() -> Error {
    Error::from(os_sys::power(POWER_OFF))
}

/// Unmounts everything and restarts the machine (root only).
pub fn reboot() -> Error {
    Error::from(os_sys::power(POWER_REBOOT))
}

/// Geometry of the framebuffer.
pub fn display_info() -> Result<DisplayInfo> {
    os_sys::display_info().map_err(Error::from)
}

/// Becomes the display owner (root only); returns the mapped framebuffer
/// (`display_info().size` bytes, `stride` pixels per row).
pub fn display_acquire() -> Result<*mut u32> {
    os_sys::display_acquire().map_err(Error::from)
}

/// Gives the display back to the kernel.
pub fn display_release() -> Result<()> {
    os_sys::display_release().map_err(Error::from)
}

/// Presents a changed rectangle of the acquired buffer. Required when
/// `display_info().backend` is `BACKEND_VIRTIO_GPU`, a no-op otherwise.
pub fn display_flush(x: u32, y: u32, w: u32, h: u32) -> Result<()> {
    os_sys::display_flush(x, y, w, h).map_err(Error::from)
}

/// Switches the display resolution (root only); acquire again afterwards.
pub fn display_set_mode(width: u32, height: u32) -> Result<()> {
    os_sys::display_set_mode(width, height).map_err(Error::from)
}
