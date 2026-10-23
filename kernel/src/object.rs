//! Kernel objects behind process handles, and waiting on several of them.
//!
//! A handle names one of these; duplicating a handle (into a child's
//! standard streams, or through a channel message) clones the `Arc`, so the
//! object lives until the last handle to it is closed.

use crate::devices::DeviceFile;
use crate::ipc::{Endpoint, Listener};
use crate::net::SocketObj;
use crate::pipe::PipeEnd;
use crate::process::FileHandle;
use crate::shm::SharedMemory;
use crate::waitq::{block_on, WaitQueue};
use alloc::sync::Arc;
use alloc::vec::Vec;
use osyx_abi::ipc::{WAIT_READABLE, WAIT_WRITABLE};
use osyx_abi::Errno;

#[derive(Clone)]
pub enum Object {
    File(FileHandle),
    Pipe(Arc<PipeEnd>),
    Channel(Arc<Endpoint>),
    Listener(Arc<Listener>),
    Shm(Arc<SharedMemory>),
    Socket(Arc<SocketObj>),
}

impl Object {
    /// Objects that may travel in channel messages.
    pub fn transferable(&self) -> bool {
        matches!(self, Object::Pipe(_) | Object::Shm(_))
    }

    pub fn shm(&self) -> Result<&Arc<SharedMemory>, Errno> {
        match self {
            Object::Shm(s) => Ok(s),
            _ => Err(Errno::BadHandle),
        }
    }

    /// What waiting on this object means. Takes the file's sleeping lock,
    /// so it must be called before entering a wait.
    pub fn waitable(&self) -> Waitable {
        match self {
            Object::File(f) => match f.lock().device() {
                Some(d) => Waitable::Device(d),
                None => Waitable::Always,
            },
            Object::Pipe(p) => Waitable::Pipe(p.clone()),
            Object::Channel(c) => Waitable::Channel(c.clone()),
            Object::Listener(l) => Waitable::Listener(l.clone()),
            Object::Shm(_) => Waitable::Always,
            Object::Socket(s) => Waitable::Socket(s.clone()),
        }
    }
}

/// An object prepared for `wait_handles`: polling it only takes spinlocks.
pub enum Waitable {
    /// Regular files and memory objects never block.
    Always,
    Device(DeviceFile),
    Pipe(Arc<PipeEnd>),
    Channel(Arc<Endpoint>),
    Listener(Arc<Listener>),
    Socket(Arc<SocketObj>),
}

impl Waitable {
    pub fn poll(&self) -> u32 {
        match self {
            Waitable::Always => WAIT_READABLE | WAIT_WRITABLE,
            Waitable::Device(d) => d.poll(),
            Waitable::Pipe(p) => p.poll(),
            Waitable::Channel(c) => c.poll(),
            Waitable::Listener(l) => l.poll(),
            Waitable::Socket(s) => s.poll(),
        }
    }

    pub fn queue(&self) -> Option<&WaitQueue> {
        match self {
            Waitable::Always => None,
            Waitable::Device(d) => d.wait_queue(),
            Waitable::Pipe(p) => Some(p.queue()),
            Waitable::Channel(c) => Some(c.queue()),
            Waitable::Listener(l) => Some(l.queue()),
            Waitable::Socket(_) => Some(crate::net::socket_queue()),
        }
    }
}

/// Blocks until one of `items` (object, wanted events) is ready or the
/// deadline passes; returns the events of each item (hang-ups are always
/// reported).
pub fn wait_any(items: &[(Waitable, u32)], deadline: Option<u64>) -> Result<Vec<u32>, Errno> {
    let queues: Vec<&WaitQueue> = items.iter().filter_map(|(w, _)| w.queue()).collect();
    let check = || {
        let r: Vec<u32> = items.iter().map(|(w, want)| w.poll() & (*want | osyx_abi::ipc::WAIT_HANGUP)).collect();
        r.iter().any(|&e| e != 0).then_some(r)
    };
    match block_on(&queues, deadline, check) {
        Ok(r) => Ok(r),
        Err(Errno::TimedOut) => Ok(alloc::vec![0; items.len()]),
        Err(e) => Err(e),
    }
}
