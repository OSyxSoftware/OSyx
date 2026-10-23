//! Error type shared by all OSyx Std APIs.

use core::fmt;
use osyx_abi::Errno;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    BadAddress,
    OutOfMemory,
    NotFound,
    Unsupported,
    PermissionDenied,
    WriteZero,
    NoChild,
    WouldBlock,
    TimedOut,
    Interrupted,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    StorageFull,
    FileTooLarge,
    ReadOnlyFilesystem,
    CrossesDevices,
    /// A read ended before the requested amount of data.
    UnexpectedEof,
    /// Data was not valid for the operation (e.g. not UTF-8).
    InvalidData,
    /// Storage hardware or volume error.
    Io,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::InvalidInput => "invalid input",
            ErrorKind::BadAddress => "bad address",
            ErrorKind::OutOfMemory => "out of memory",
            ErrorKind::NotFound => "not found",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::PermissionDenied => "permission denied",
            ErrorKind::WriteZero => "write returned zero bytes",
            ErrorKind::NoChild => "no child process",
            ErrorKind::WouldBlock => "operation would block",
            ErrorKind::TimedOut => "timed out",
            ErrorKind::Interrupted => "interrupted",
            ErrorKind::AlreadyExists => "entity already exists",
            ErrorKind::NotADirectory => "not a directory",
            ErrorKind::IsADirectory => "is a directory",
            ErrorKind::DirectoryNotEmpty => "directory not empty",
            ErrorKind::StorageFull => "no storage space",
            ErrorKind::FileTooLarge => "file too large",
            ErrorKind::ReadOnlyFilesystem => "read-only filesystem or storage medium",
            ErrorKind::CrossesDevices => "cross-device link or rename",
            ErrorKind::UnexpectedEof => "unexpected end of file",
            ErrorKind::InvalidData => "invalid data",
            ErrorKind::Io => "I/O error",
            ErrorKind::Other => "other error",
        }
    }
}

/// An error from the OSyx platform or from OSyx Std itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    errno: Option<Errno>,
    message: Option<&'static str>,
}

impl Error {
    pub const fn new(kind: ErrorKind, message: &'static str) -> Error {
        Error { kind, errno: None, message: Some(message) }
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The kernel error code, if the error came from a system call.
    pub const fn raw_os_error(&self) -> Option<Errno> {
        self.errno
    }

    pub(crate) const fn unsupported(what: &'static str) -> Error {
        Error::new(ErrorKind::Unsupported, what)
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Error {
        Error { kind, errno: None, message: None }
    }
}

impl From<Errno> for Error {
    fn from(e: Errno) -> Error {
        let kind = match e {
            Errno::InvalidArgument => ErrorKind::InvalidInput,
            Errno::Fault => ErrorKind::BadAddress,
            Errno::NoMemory => ErrorKind::OutOfMemory,
            Errno::BadHandle => ErrorKind::NotFound,
            Errno::NotSupported => ErrorKind::Unsupported,
            Errno::PermissionDenied => ErrorKind::PermissionDenied,
            Errno::NotFound => ErrorKind::NotFound,
            Errno::NoChild => ErrorKind::NoChild,
            Errno::WouldBlock => ErrorKind::WouldBlock,
            Errno::TimedOut => ErrorKind::TimedOut,
            Errno::Interrupted => ErrorKind::Interrupted,
            Errno::AlreadyExists => ErrorKind::AlreadyExists,
            Errno::NotADirectory => ErrorKind::NotADirectory,
            Errno::IsADirectory => ErrorKind::IsADirectory,
            Errno::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
            Errno::NoSpace => ErrorKind::StorageFull,
            Errno::Io => ErrorKind::Io,
            Errno::FileTooLarge => ErrorKind::FileTooLarge,
            Errno::ReadOnly => ErrorKind::ReadOnlyFilesystem,
            Errno::TooManyHandles => ErrorKind::OutOfMemory,
            Errno::CrossDevice => ErrorKind::CrossesDevices,
        };
        Error { kind, errno: Some(e), message: None }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.message, self.errno) {
            (Some(m), _) => write!(f, "{}: {}", self.kind.as_str(), m),
            (None, Some(e)) => write!(f, "{} (os error {}: {})", self.kind.as_str(), e as u64, e.name()),
            (None, None) => f.write_str(self.kind.as_str()),
        }
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn errno_maps_to_kind_and_keeps_code() {
        let e = Error::from(Errno::Fault);
        assert_eq!(e.kind(), ErrorKind::BadAddress);
        assert_eq!(e.raw_os_error(), Some(Errno::Fault));
        assert_eq!(e.to_string(), "bad address (os error 2: bad address)");
        assert_eq!(Error::from(Errno::NoMemory).kind(), ErrorKind::OutOfMemory);
        assert_eq!(Error::from(Errno::PermissionDenied).kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn custom_errors_display_message() {
        let e = Error::new(ErrorKind::Unsupported, "files are not implemented yet");
        assert_eq!(e.to_string(), "unsupported: files are not implemented yet");
        assert_eq!(e.raw_os_error(), None);
        assert_eq!(Error::from(ErrorKind::WriteZero).to_string(), "write returned zero bytes");
    }
}
