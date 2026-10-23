//! Inter-process communication: pipes, channels to named services, shared
//! memory, and waiting on several handles at once.
//!
//! ```ignore
//! let server = Listener::listen("demo")?;
//! let client = Channel::connect("demo")?;
//! let conn = server.accept()?.expect("pending connection");
//! client.send(b"hello", &[])?;
//! let mut buf = [0u8; 64];
//! let (n, _handles) = conn.recv(&mut buf)?.expect("a message");
//! ```

use crate::error::{Error, ErrorKind, Result};
use crate::io::{Read, Write};
use alloc::vec::Vec;
use core::time::Duration;
pub use os_sys::ipc::{WaitItem, MAX_MESSAGE, MAX_MESSAGE_HANDLES, WAIT_HANGUP, WAIT_READABLE, WAIT_WRITABLE};

/// A kernel handle, closed when dropped.
#[derive(Debug)]
pub struct OwnedHandle(u64);

impl OwnedHandle {
    /// Takes ownership of a raw handle number.
    ///
    /// # Safety
    /// `h` must be an open handle nothing else will close.
    pub unsafe fn from_raw(h: u64) -> OwnedHandle {
        OwnedHandle(h)
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    /// Gives up ownership (the handle stays open).
    pub fn into_raw(self) -> u64 {
        let h = self.0;
        core::mem::forget(self);
        h
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = os_sys::close(self.0);
    }
}

/// The read end of a pipe.
#[derive(Debug)]
pub struct PipeReader(OwnedHandle);
/// The write end of a pipe.
#[derive(Debug)]
pub struct PipeWriter(OwnedHandle);

/// A new pipe.
pub fn pipe() -> Result<(PipeReader, PipeWriter)> {
    let (r, w) = os_sys::pipe().map_err(Error::from)?;
    // SAFETY: fresh handles from the kernel, owned by nobody else.
    unsafe { Ok((PipeReader(OwnedHandle::from_raw(r)), PipeWriter(OwnedHandle::from_raw(w)))) }
}

impl PipeReader {
    pub fn handle(&self) -> u64 {
        self.0.raw()
    }

    pub fn into_handle(self) -> OwnedHandle {
        self.0
    }
}

impl PipeWriter {
    pub fn handle(&self) -> u64 {
        self.0.raw()
    }

    pub fn into_handle(self) -> OwnedHandle {
        self.0
    }
}

impl Read for PipeReader {
    /// Blocks until data arrives; 0 once every writer is gone.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        os_sys::read(self.0.raw(), buf).map_err(Error::from)
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        os_sys::write(self.0.raw(), buf).map_err(Error::from)
    }
}

/// A registered service accepting [`Channel`] connections.
#[derive(Debug)]
pub struct Listener(OwnedHandle);

impl Listener {
    /// Registers `name` (root only; fails while someone else has it).
    pub fn listen(name: &str) -> Result<Listener> {
        let h = os_sys::channel_listen(name).map_err(Error::from)?;
        // SAFETY: fresh handle.
        Ok(Listener(unsafe { OwnedHandle::from_raw(h) }))
    }

    /// The next pending connection, if any.
    pub fn accept(&self) -> Result<Option<Channel>> {
        match os_sys::channel_accept(self.0.raw()) {
            // SAFETY: fresh handle.
            Ok(h) => Ok(Some(Channel(unsafe { OwnedHandle::from_raw(h) }))),
            Err(os_sys::Errno::WouldBlock) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    pub fn handle(&self) -> u64 {
        self.0.raw()
    }
}

/// One end of a message channel.
#[derive(Debug)]
pub struct Channel(OwnedHandle);

impl Channel {
    pub fn connect(service: &str) -> Result<Channel> {
        let h = os_sys::channel_connect(service).map_err(Error::from)?;
        // SAFETY: fresh handle.
        Ok(Channel(unsafe { OwnedHandle::from_raw(h) }))
    }

    /// Queues a message; `handles` (shared memory, pipe ends) are
    /// duplicated to the receiver. `WouldBlock` if the peer's queue is full.
    pub fn send(&self, data: &[u8], handles: &[u64]) -> Result<()> {
        os_sys::channel_send(self.0.raw(), data, handles).map_err(Error::from)
    }

    /// The next message: its length and the handles it carried (`None` if
    /// none is queued). `BrokenPipe` once the peer is gone and the queue empty.
    pub fn recv(&self, buf: &mut [u8]) -> Result<Option<(usize, Vec<OwnedHandle>)>> {
        let mut hs = [0u64; MAX_MESSAGE_HANDLES];
        match os_sys::channel_recv(self.0.raw(), buf, &mut hs) {
            Ok((n, count)) => {
                // SAFETY: the kernel just installed these handles for us.
                let handles = hs[..count].iter().map(|&h| unsafe { OwnedHandle::from_raw(h) }).collect();
                Ok(Some((n, handles)))
            }
            Err(os_sys::Errno::WouldBlock) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// (pid, uid) of the process at the other end.
    pub fn peer(&self) -> Result<(u64, u32)> {
        os_sys::channel_peer(self.0.raw()).map_err(Error::from)
    }

    pub fn handle(&self) -> u64 {
        self.0.raw()
    }
}

/// Memory shared between processes.
#[derive(Debug)]
pub struct SharedMemory {
    handle: OwnedHandle,
    size: u64,
}

impl SharedMemory {
    /// `size` bytes of zeroed memory.
    pub fn create(size: u64) -> Result<SharedMemory> {
        let h = os_sys::shm_create(size).map_err(Error::from)?;
        // SAFETY: fresh handle.
        let handle = unsafe { OwnedHandle::from_raw(h) };
        Ok(SharedMemory { handle, size: size.next_multiple_of(4096) })
    }

    /// Wraps a handle received from another process.
    pub fn from_handle(handle: OwnedHandle) -> Result<SharedMemory> {
        let size = os_sys::shm_size(handle.raw()).map_err(Error::from)?;
        Ok(SharedMemory { handle, size })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn handle(&self) -> u64 {
        self.handle.raw()
    }

    /// Maps the whole object.
    pub fn map(&self, writable: bool) -> Result<Mapping> {
        let ptr = os_sys::shm_map(self.handle.raw(), writable).map_err(Error::from)?;
        Ok(Mapping { ptr, len: self.size as usize })
    }
}

/// A mapping of shared memory, unmapped when dropped. Other processes may
/// change the contents at any time: treat what is read as untrusted.
#[derive(Debug)]
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this process's address space.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Takes ownership of a mapping made by the kernel (e.g. the display).
    ///
    /// # Safety
    /// `ptr` must start a mapping of `len` bytes that only this value unmaps.
    pub unsafe fn from_raw(ptr: *mut u8, len: usize) -> Mapping {
        Mapping { ptr, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// The mapping as 32-bit words.
    pub fn words(&self) -> &[u32] {
        // SAFETY: page-aligned mapping of `len` bytes that lives as long as
        // `self`. Concurrent writers in other processes can only change the
        // values, which callers treat as untrusted data.
        unsafe { core::slice::from_raw_parts(self.ptr as *const u32, self.len / 4) }
    }

    /// The mapping as 32-bit words, for writing (only meaningful for a
    /// writable mapping).
    pub fn words_mut(&mut self) -> &mut [u32] {
        // SAFETY: as in `words`; `&mut self` keeps this process's view unique.
        unsafe { core::slice::from_raw_parts_mut(self.ptr as *mut u32, self.len / 4) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `words_mut`.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: nothing borrows the mapping any more (`&mut self`).
        let _ = unsafe { os_sys::unmap(self.ptr, self.len) };
    }
}

/// Waits until one of `items` is ready or `timeout` passes (`None`: no
/// limit, `Some(ZERO)`: just poll). Returns how many items are ready; their
/// `revents` say what happened.
pub fn wait(items: &mut [WaitItem], timeout: Option<Duration>) -> Result<usize> {
    let ns = match timeout {
        None => u64::MAX,
        Some(d) => u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1).min(u64::MAX - 1),
    };
    os_sys::wait_handles(items, ns).map_err(Error::from)
}

/// A wait item for `handle` and `events`.
pub fn item(handle: u64, events: u32) -> WaitItem {
    WaitItem { handle, events, revents: 0 }
}

/// Errors meaning "the other side went away".
pub fn is_disconnect(e: &Error) -> bool {
    e.kind() == ErrorKind::BrokenPipe
}
