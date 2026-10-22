//! Byte-oriented I/O: the [`Write`] trait, standard output and error.

use crate::error::{Error, ErrorKind, Result};
use crate::sys;
use core::fmt;

pub trait Write {
    /// Writes some prefix of `buf`, returning how many bytes were written.
    fn write(&mut self, buf: &[u8]) -> Result<usize>;

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf)? {
                0 => return Err(ErrorKind::WriteZero.into()),
                n => buf = &buf[n..],
            }
        }
        Ok(())
    }

    /// Formats into this writer, reporting the first I/O error (not just
    /// `fmt::Error`).
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> Result<()> {
        struct Adapter<'a, W: ?Sized> {
            inner: &'a mut W,
            error: Option<Error>,
        }
        impl<W: Write + ?Sized> fmt::Write for Adapter<'_, W> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.inner.write_all(s.as_bytes()).map_err(|e| {
                    self.error = Some(e);
                    fmt::Error
                })
            }
        }
        let mut a = Adapter { inner: self, error: None };
        match fmt::write(&mut a, args) {
            Ok(()) => Ok(()),
            Err(_) => Err(a.error.unwrap_or_else(|| Error::new(ErrorKind::Other, "formatter error"))),
        }
    }
}

impl Write for alloc::vec::Vec<u8> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.extend_from_slice(buf);
        Ok(buf.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn fd(self) -> u64 {
        match self {
            Stream::Stdout => osyx_abi::STDOUT,
            Stream::Stderr => osyx_abi::STDERR,
        }
    }
}

/// Handle to the process's standard output (unbuffered; `print!` batches
/// each call into as few system calls as possible).
pub struct Stdout;
/// Handle to the process's standard error.
pub struct Stderr;

pub fn stdout() -> Stdout {
    Stdout
}

pub fn stderr() -> Stderr {
    Stderr
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        sys::write(Stream::Stdout.fd(), buf)
    }
}

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        sys::write(Stream::Stderr.fd(), buf)
    }
}

/// Collects formatted output in a fixed buffer and hands it to `sink` in
/// chunks of at most `N` bytes. Lets `print!` format without allocating and
/// with one system call per chunk instead of one per formatting fragment.
pub struct ChunkWriter<const N: usize, F: FnMut(&[u8]) -> Result<()>> {
    buf: [u8; N],
    len: usize,
    sink: F,
    error: Option<Error>,
}

impl<const N: usize, F: FnMut(&[u8]) -> Result<()>> ChunkWriter<N, F> {
    pub fn new(sink: F) -> Self {
        ChunkWriter { buf: [0; N], len: 0, sink, error: None }
    }

    fn drain(&mut self) {
        if self.len > 0 && self.error.is_none() {
            if let Err(e) = (self.sink)(&self.buf[..self.len]) {
                self.error = Some(e);
            }
        }
        self.len = 0;
    }

    /// Emits any buffered bytes and returns the first error encountered.
    pub fn finish(mut self) -> Result<()> {
        self.drain();
        self.error.map_or(Ok(()), Err)
    }
}

impl<const N: usize, F: FnMut(&[u8]) -> Result<()>> fmt::Write for ChunkWriter<N, F> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            if self.len == N {
                self.drain();
            }
            let n = (N - self.len).min(bytes.len());
            self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
            self.len += n;
            bytes = &bytes[n..];
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(stream: Stream, args: fmt::Arguments<'_>) {
    let fd = stream.fd();
    let mut w = ChunkWriter::<256, _>::new(|chunk| {
        let mut rest = chunk;
        while !rest.is_empty() {
            match sys::write(fd, rest)? {
                0 => return Err(ErrorKind::WriteZero.into()),
                n => rest = &rest[n..],
            }
        }
        Ok(())
    });
    let _ = fmt::write(&mut w, args);
    // Like std's print!, a failure to write stdout is fatal.
    if let Err(e) = w.finish() {
        panic!("failed printing to {:?}: {}", stream, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::fmt::Write as _;

    #[test]
    fn write_all_and_write_fmt_on_vec() {
        let mut v: Vec<u8> = Vec::new();
        v.write_all(b"OSyx ").unwrap();
        Write::write_fmt(&mut v, format_args!("{}-{:04}", "std", 7)).unwrap();
        assert_eq!(v, b"OSyx std-0007");
    }

    #[test]
    fn write_zero_is_an_error() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> Result<usize> {
                Ok(0)
            }
        }
        assert_eq!(Full.write_all(b"x").unwrap_err().kind(), ErrorKind::WriteZero);
        assert_eq!(Full.write_all(b"").map_err(|e| e.kind()), Ok(()));
    }

    #[test]
    fn chunk_writer_splits_and_preserves_bytes() {
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut w = ChunkWriter::<8, _>::new(|c: &[u8]| {
            chunks.push(c.to_vec());
            Ok(())
        });
        let (word, number, accent) = ("abcdefghij", 12345, "ż");
        write!(w, "{word}|{number}|{accent}").unwrap();
        w.finish().unwrap();
        assert!(chunks.iter().all(|c| c.len() <= 8 && !c.is_empty()));
        let joined: Vec<u8> = chunks.concat();
        assert_eq!(String::from_utf8(joined).unwrap(), "abcdefghij|12345|ż");
    }

    #[test]
    fn chunk_writer_reports_sink_errors() {
        let w = ChunkWriter::<4, _>::new(|_: &[u8]| Err(crate::error::Error::from(osyx_abi::Errno::Fault)));
        let mut w = w;
        write!(w, "hello").unwrap();
        assert_eq!(w.finish().unwrap_err().kind(), ErrorKind::BadAddress);
    }
}
