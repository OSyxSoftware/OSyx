//! Pipes: a bounded byte stream between a read end and a write end.
//!
//! Ends are reference counted (an end shared by several handles or
//! processes stays open until the last of them closes it). Readers see end
//! of data (0) once every write end is closed and the buffer is empty;
//! writers get `BrokenPipe` once every read end is closed.

use crate::sync::IrqSpinLock;
use crate::waitq::{block_on, WaitQueue};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use osyx_abi::ipc::{PIPE_CAPACITY, WAIT_HANGUP, WAIT_READABLE, WAIT_WRITABLE};
use osyx_abi::Errno;

struct State {
    buf: VecDeque<u8>,
    readers: u32,
    writers: u32,
}

pub struct Pipe {
    state: IrqSpinLock<State>,
    /// Signalled when data arrives or the last writer leaves.
    readable: WaitQueue,
    /// Signalled when space frees up or the last reader leaves.
    writable: WaitQueue,
}

/// One end of a pipe.
pub struct PipeEnd {
    pipe: Arc<Pipe>,
    write: bool,
}

/// A new pipe: (read end, write end).
pub fn new() -> (PipeEnd, PipeEnd) {
    let pipe = Arc::new(Pipe {
        state: IrqSpinLock::new(State { buf: VecDeque::new(), readers: 1, writers: 1 }),
        readable: WaitQueue::new(),
        writable: WaitQueue::new(),
    });
    (PipeEnd { pipe: pipe.clone(), write: false }, PipeEnd { pipe, write: true })
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        {
            let mut s = self.pipe.state.lock();
            if self.write {
                s.writers -= 1;
            } else {
                s.readers -= 1;
            }
        }
        self.pipe.readable.wake_all();
        self.pipe.writable.wake_all();
    }
}

impl PipeEnd {
    /// Reads at least one byte (blocking), or returns 0 at end of data.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.write {
            return Err(Errno::BadHandle);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let p = &self.pipe;
        let n = block_on(&[&p.readable], None, || {
            let mut s = p.state.lock();
            if !s.buf.is_empty() {
                let n = buf.len().min(s.buf.len());
                for (i, b) in s.buf.drain(..n).enumerate() {
                    buf[i] = b;
                }
                Some(n)
            } else if s.writers == 0 {
                Some(0)
            } else {
                None
            }
        })?;
        if n > 0 {
            p.writable.wake_all();
        }
        Ok(n)
    }

    /// Writes all of `data`, blocking while the pipe is full. A write cut
    /// short (readers gone, process killed) reports the bytes written so
    /// far, or the error if there were none.
    pub fn write(&self, data: &[u8]) -> Result<usize, Errno> {
        if !self.write {
            return Err(Errno::BadHandle);
        }
        let p = &self.pipe;
        let mut done = 0;
        while done < data.len() {
            let r = block_on(&[&p.writable], None, || {
                let mut s = p.state.lock();
                if s.readers == 0 {
                    return Some(Err(Errno::BrokenPipe));
                }
                let room = PIPE_CAPACITY - s.buf.len();
                if room == 0 {
                    return None;
                }
                let n = room.min(data.len() - done);
                s.buf.extend(&data[done..done + n]);
                Some(Ok(n))
            });
            match r.and_then(|r| r) {
                Ok(n) => {
                    done += n;
                    p.readable.wake_all();
                }
                Err(e) if done == 0 => return Err(e),
                Err(_) => break,
            }
        }
        Ok(done)
    }

    /// `wait_handles` readiness.
    pub fn poll(&self) -> u32 {
        let s = self.pipe.state.lock();
        if self.write {
            if s.readers == 0 {
                WAIT_WRITABLE | WAIT_HANGUP
            } else if s.buf.len() < PIPE_CAPACITY {
                WAIT_WRITABLE
            } else {
                0
            }
        } else if !s.buf.is_empty() {
            WAIT_READABLE | if s.writers == 0 { WAIT_HANGUP } else { 0 }
        } else if s.writers == 0 {
            WAIT_READABLE | WAIT_HANGUP
        } else {
            0
        }
    }

    pub fn queue(&self) -> &WaitQueue {
        if self.write {
            &self.pipe.writable
        } else {
            &self.pipe.readable
        }
    }
}
