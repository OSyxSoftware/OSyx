//! Program arguments. Environment variables are not supported yet: [`var`]
//! always returns an explicit `Unsupported` error.

use crate::error::{Error, Result};
use crate::sys;
use alloc::string::String;
use alloc::vec::Vec;

/// Iterator over the program arguments (`argv[0]` first).
pub struct Args {
    inner: alloc::vec::IntoIter<String>,
}

impl Iterator for Args {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        self.inner.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for Args {}

/// Returns the arguments the process was started with. Arguments that are not
/// valid UTF-8 are converted lossily.
pub fn args() -> Args {
    let v: Vec<String> = sys::args();
    Args { inner: v.into_iter() }
}

/// Environment variables do not exist on OSyx yet.
pub fn var(_key: &str) -> Result<String> {
    Err(Error::unsupported("environment variables are not implemented"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn args_include_program_name() {
        let a = args();
        assert!(a.len() >= 1);
    }

    #[test]
    fn env_vars_report_unsupported() {
        assert_eq!(var("PATH").unwrap_err().kind(), ErrorKind::Unsupported);
    }
}
