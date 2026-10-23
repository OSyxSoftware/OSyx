//! Program arguments and the current directory. Environment variables are
//! not supported yet: [`var`] always returns an explicit `Unsupported` error.

use crate::error::{Error, Result};
use crate::path::{Path, PathBuf};
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

/// The current directory (absolute; inherited from the parent process).
pub fn current_dir() -> Result<PathBuf> {
    sys::fs::getcwd().map(PathBuf::from)
}

/// Changes the current directory; relative paths resolve from it.
pub fn set_current_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    sys::fs::chdir(path.as_ref().as_str())
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
    fn current_directory_round_trip() {
        crate::fs::create_dir_all("/t-cwd/sub").unwrap();
        set_current_dir("/t-cwd").unwrap();
        assert_eq!(current_dir().unwrap(), "/t-cwd");
        crate::fs::write("sub/rel.txt", "relative").unwrap();
        assert_eq!(crate::fs::read_to_string("/t-cwd/sub/rel.txt").unwrap(), "relative");
        set_current_dir("sub/..").unwrap();
        assert_eq!(current_dir().unwrap(), "/t-cwd");
        set_current_dir("/").unwrap();
        crate::fs::remove_dir_all("/t-cwd").unwrap();
    }

    #[test]
    fn env_vars_report_unsupported() {
        assert_eq!(var("PATH").unwrap_err().kind(), ErrorKind::Unsupported);
    }
}
