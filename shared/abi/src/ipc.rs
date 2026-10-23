//! Structures of the ABI v7 system calls: channels, waiting, the display,
//! process and mount listings. All are `repr(C)` without padding.

/// Largest channel message (bytes).
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Handles one message may carry.
pub const MAX_MESSAGE_HANDLES: usize = 4;
/// Messages queued per direction before `channel_send` says `WouldBlock`.
pub const MAX_QUEUED_MESSAGES: usize = 256;
/// Bytes queued per direction before `channel_send` says `WouldBlock`.
pub const MAX_QUEUED_BYTES: usize = 1024 * 1024;
/// Connections waiting to be accepted per listener.
pub const MAX_BACKLOG: usize = 32;
/// Longest service name.
pub const MAX_SERVICE_NAME: usize = 64;
/// Items one `wait_handles` call may watch.
pub const MAX_WAIT_ITEMS: usize = 64;
/// Largest shared memory object.
pub const MAX_SHM: u64 = 64 * 1024 * 1024;
/// Bytes a pipe buffers.
pub const PIPE_CAPACITY: usize = 64 * 1024;

/// Argument block of `channel_send` / `channel_recv`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelMsg {
    /// Message bytes (send) or the receive buffer.
    pub data: u64,
    /// Message length (send) or buffer capacity (receive).
    pub len: u64,
    /// `[u64; MAX_MESSAGE_HANDLES]` of handles to pass / received handles.
    pub handles: u64,
    /// Handles to pass (send); capacity in, count received out (receive).
    pub handle_count: u64,
}

/// Readiness events for `wait_handles`.
pub const WAIT_READABLE: u32 = 1 << 0;
pub const WAIT_WRITABLE: u32 = 1 << 1;
/// Output only: the other end is gone (a read will return 0 or `BrokenPipe`).
pub const WAIT_HANGUP: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitItem {
    pub handle: u64,
    /// `WAIT_READABLE` / `WAIT_WRITABLE` wanted.
    pub events: u32,
    /// Filled in by the kernel.
    pub revents: u32,
}

/// Pixel layouts of the framebuffer: 32 bits per pixel.
pub mod pixel {
    /// Blue in the lowest byte (`0x00RRGGBB` as a little-endian `u32`).
    pub const BGRX: u32 = 1;
    /// Red in the lowest byte.
    pub const RGBX: u32 = 2;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    /// Pixels per row.
    pub stride: u32,
    /// [`pixel`] layout.
    pub format: u32,
    /// Bytes of the mapping `display_acquire` returns.
    pub size: u64,
    /// pid of the owner, 0 if the kernel draws the display.
    pub owner: u64,
    /// `BACKEND_*`: how pixels reach the screen.
    pub backend: u32,
    /// Outputs the device reports (scanouts).
    pub outputs: u32,
    /// The output's preferred (native) mode, 0 if unknown.
    pub native_width: u32,
    pub native_height: u32,
}

/// `DisplayInfo::backend`: a linear framebuffer the hardware scans out
/// (UEFI GOP); writes are visible at once.
pub const BACKEND_LINEAR: u32 = 1;
/// A VirtIO-GPU scanout: writes go to guest memory and reach the screen
/// with `display_flush`.
pub const BACKEND_VIRTIO_GPU: u32 = 2;

/// `ProcInfo::state`
pub const PROC_RUNNING: u32 = 0;
pub const PROC_ZOMBIE: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u64,
    pub parent: u64,
    pub uid: u32,
    pub gid: u32,
    pub threads: u32,
    pub state: u32,
    /// Physical frames the process owns (its memory, page tables included).
    pub frames: u64,
    /// NUL-padded UTF-8.
    pub name: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MountInfo {
    pub dev: u64,
    /// `MOUNT_*` flags.
    pub flags: u64,
    /// NUL-padded UTF-8 (truncated if longer).
    pub path: [u8; 64],
    pub source: [u8; 48],
    pub fstype: [u8; 16],
}

impl Default for MountInfo {
    fn default() -> Self {
        MountInfo { dev: 0, flags: 0, path: [0; 64], source: [0; 48], fstype: [0; 16] }
    }
}

/// `power` commands.
pub const POWER_OFF: u64 = 1;
pub const POWER_REBOOT: u64 = 2;

/// The text of a NUL-padded field.
pub fn field_str(b: &[u8]) -> &str {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    // Fields are cut at a byte limit; drop a partial character at the end.
    match core::str::from_utf8(&b[..end]) {
        Ok(s) => s,
        Err(e) => core::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or(""),
    }
}

/// Copies `s` into a NUL-padded field, cutting at a character boundary.
pub fn fill_field(dst: &mut [u8], s: &str) {
    dst.fill(0);
    let mut n = s.len().min(dst.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_fields_round_trip_and_cut_cleanly() {
        let mut f = [0xFFu8; 8];
        fill_field(&mut f, "init");
        assert_eq!(field_str(&f), "init");
        fill_field(&mut f, "żółwżółw"); // 14 bytes of UTF-8
        assert_eq!(field_str(&f), "żółw");
        let mut g = [0u8; 3];
        fill_field(&mut g, "ąą");
        assert_eq!(field_str(&g), "ą");
        assert_eq!(field_str(&[b'a', 0xC5]), "a", "a partial character is dropped");
    }
}
