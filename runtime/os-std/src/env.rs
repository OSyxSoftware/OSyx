//! Program arguments, environment variables, the current directory and the
//! program path.
//!
//! The environment starts as the `envp` block the kernel put on the start-up
//! stack (`spawn` / `execve` pass it from the parent) and lives in the
//! process from then on: [`set_var`] and [`remove_var`] change this copy,
//! which [`Command`](crate::process::Command) children inherit by default.
//! Unlike `std` on Unix the copy is protected by a lock, so changing it while
//! other threads run is safe.

use crate::error::{Error, ErrorKind, Result};
use crate::path::{Path, PathBuf};
use crate::sync::Mutex;
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

/// Absolute path of the running program's file, as the kernel loaded it
/// (symbolic links resolved).
pub fn current_exe() -> Result<PathBuf> {
    sys::current_exe().map(PathBuf::from)
}

/// The variables in start-up order; `None` until first used.
static ENV: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

fn with_env<R>(f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
    let mut g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| parse_env(sys::initial_env())))
}

/// `KEY=value` strings to pairs. Entries without `=` or with an empty key
/// are ignored; a later duplicate key wins (as with `std`).
pub(crate) fn parse_env(list: Vec<String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in list {
        let Some((k, v)) = entry.split_once('=') else { continue };
        if k.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(key, _)| key == k) {
            Some(slot) => slot.1 = String::from(v),
            None => out.push((String::from(k), String::from(v))),
        }
    }
    out
}

/// The value of environment variable `key` (`NotFound` if it is not set).
pub fn var(key: &str) -> Result<String> {
    with_env(|env| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
        .ok_or(Error::new(ErrorKind::NotFound, "environment variable not set"))
}

/// Iterator over a snapshot of the environment.
pub struct Vars {
    inner: alloc::vec::IntoIter<(String, String)>,
}

impl Iterator for Vars {
    type Item = (String, String);
    fn next(&mut self) -> Option<(String, String)> {
        self.inner.next()
    }
}

/// All environment variables (a snapshot).
pub fn vars() -> Vars {
    Vars { inner: with_env(|env| env.clone()).into_iter() }
}

fn check_key(key: &str) {
    assert!(!key.is_empty() && !key.contains(['=', '\0']), "invalid environment variable name {key:?}");
}

/// Sets `key` to `value`.
///
/// # Panics
/// If `key` is empty or contains `=` or NUL, or `value` contains NUL.
pub fn set_var(key: &str, value: &str) {
    check_key(key);
    assert!(!value.contains('\0'), "environment variable value contains NUL");
    with_env(|env| match env.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = String::from(value),
        None => env.push((String::from(key), String::from(value))),
    });
}

/// Removes `key` (nothing happens if it is not set).
///
/// # Panics
/// If `key` is empty or contains `=` or NUL.
pub fn remove_var(key: &str) {
    check_key(key);
    with_env(|env| env.retain(|(k, _)| k != key));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

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
    fn env_parsing_rules() {
        let list = ["A=1", "noequals", "=empty", "B=x=y", "A=2", "C="].map(|s| s.to_string()).to_vec();
        let env = parse_env(list);
        let as_strs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(as_strs, [("A", "2"), ("B", "x=y"), ("C", "")]);
    }

    #[test]
    fn set_get_remove_variables() {
        assert_eq!(var("OSYX_TEST_NOT_SET").unwrap_err().kind(), ErrorKind::NotFound);
        set_var("OSYX_TEST_VAR", "one");
        assert_eq!(var("OSYX_TEST_VAR").unwrap(), "one");
        set_var("OSYX_TEST_VAR", "two=2");
        assert_eq!(var("OSYX_TEST_VAR").unwrap(), "two=2");
        assert_eq!(vars().filter(|(k, _)| k == "OSYX_TEST_VAR").count(), 1);
        remove_var("OSYX_TEST_VAR");
        assert!(var("OSYX_TEST_VAR").is_err());
        remove_var("OSYX_TEST_VAR");
    }

    #[test]
    #[should_panic(expected = "invalid environment variable name")]
    fn set_var_rejects_bad_names() {
        set_var("A=B", "x");
    }

    #[test]
    fn current_exe_is_known() {
        assert!(current_exe().is_ok());
    }
}
