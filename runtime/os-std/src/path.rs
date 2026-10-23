//! Paths: `/`-separated UTF-8 strings ([`Path`] borrowed, [`PathBuf`] owned).
//!
//! OSyx paths are always UTF-8, so unlike Rust's `std::path` these types are
//! thin wrappers around `str` / `String`. File system calls require absolute
//! paths; there is no current directory yet.

use alloc::borrow::{Borrow, ToOwned};
use alloc::string::String;
use core::fmt;
use core::ops::Deref;

pub const SEPARATOR: char = '/';

#[repr(transparent)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    inner: str,
}

impl Path {
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> &Path {
        // SAFETY: Path is a repr(transparent) wrapper around str.
        unsafe { &*(s.as_ref() as *const str as *const Path) }
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }

    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf { inner: String::from(&self.inner) }
    }

    pub fn is_absolute(&self) -> bool {
        self.inner.starts_with(SEPARATOR)
    }

    /// `self` followed by `path` (or `path` alone if it is absolute).
    pub fn join<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let mut buf = self.to_path_buf();
        buf.push(path);
        buf
    }

    /// The last component, if any (`None` for `/` and paths ending in `..`).
    pub fn file_name(&self) -> Option<&str> {
        match self.inner.trim_end_matches(SEPARATOR).rsplit(SEPARATOR).next() {
            Some("") | Some("..") | None => None,
            Some(name) => Some(name),
        }
    }

    /// The path without its last component (`None` for `/` and for a bare name).
    pub fn parent(&self) -> Option<&Path> {
        let trimmed = self.inner.trim_end_matches(SEPARATOR);
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.rfind(SEPARATOR) {
            Some(0) => Some(Path::new("/")),
            Some(i) => Some(Path::new(&trimmed[..i])),
            None => Some(Path::new("")),
        }
    }

    /// Extension of the file name, without the dot.
    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name()?;
        match name.rfind('.') {
            Some(0) | None => None,
            Some(i) => Some(&name[i + 1..]),
        }
    }

    /// Non-empty components, `.` dropped.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.inner.split(SEPARATOR).filter(|c| !c.is_empty() && *c != ".")
    }

    pub fn display(&self) -> &str {
        &self.inner
    }

    /// True if the path names an existing file or directory.
    pub fn exists(&self) -> bool {
        crate::fs::metadata(self).is_ok()
    }

    pub fn is_dir(&self) -> bool {
        crate::fs::metadata(self).is_ok_and(|m| m.is_dir())
    }

    pub fn is_file(&self) -> bool {
        crate::fs::metadata(self).is_ok_and(|m| m.is_file())
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner)
    }
}

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathBuf {
    inner: String,
}

impl PathBuf {
    pub fn new() -> PathBuf {
        PathBuf::default()
    }

    /// Appends `path` as a new component; an absolute `path` replaces `self`.
    pub fn push<P: AsRef<Path>>(&mut self, path: P) {
        let p = path.as_ref().as_str();
        if p.starts_with(SEPARATOR) {
            self.inner.clear();
        } else if !self.inner.is_empty() && !self.inner.ends_with(SEPARATOR) {
            self.inner.push(SEPARATOR);
        }
        self.inner.push_str(p);
    }

    /// Removes the last component; false if there was none.
    pub fn pop(&mut self) -> bool {
        match self.parent().map(|p| p.inner.len()) {
            Some(len) => {
                self.inner.truncate(len);
                true
            }
            None => false,
        }
    }

    pub fn into_string(self) -> String {
        self.inner
    }
}

impl Deref for PathBuf {
    type Target = Path;
    fn deref(&self) -> &Path {
        Path::new(&self.inner)
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self
    }
}

impl ToOwned for Path {
    type Owned = PathBuf;
    fn to_owned(&self) -> PathBuf {
        self.to_path_buf()
    }
}

impl fmt::Debug for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl fmt::Display for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner)
    }
}

impl From<String> for PathBuf {
    fn from(inner: String) -> PathBuf {
        PathBuf { inner }
    }
}

impl From<&str> for PathBuf {
    fn from(s: &str) -> PathBuf {
        PathBuf { inner: String::from(s) }
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl AsRef<Path> for str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<Path> for String {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<str> for Path {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl PartialEq<str> for Path {
    fn eq(&self, other: &str) -> bool {
        &self.inner == other
    }
}

impl PartialEq<&str> for PathBuf {
    fn eq(&self, other: &&str) -> bool {
        self.inner == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_parent_and_names() {
        let p = Path::new("/docs").join("notes").join("a.txt");
        assert_eq!(p, "/docs/notes/a.txt");
        assert_eq!(p.file_name(), Some("a.txt"));
        assert_eq!(p.extension(), Some("txt"));
        assert_eq!(p.parent().unwrap(), "/docs/notes");
        assert_eq!(Path::new("/a").parent().unwrap(), "/");
        assert_eq!(Path::new("/").parent(), None);
        assert_eq!(Path::new("/").file_name(), None);
        assert_eq!(Path::new("/x/..").file_name(), None);
        assert_eq!(Path::new(".hidden").extension(), None);
        assert_eq!(Path::new("/a").join("/b"), "/b");
        assert!(Path::new("/a").is_absolute() && !Path::new("a").is_absolute());
        let mut b = PathBuf::from("/x/y/");
        assert!(b.pop());
        assert_eq!(b, "/x");
        assert!(b.pop() && b == "/" && !b.pop());
        assert_eq!(Path::new("/a//./b/").components().collect::<alloc::vec::Vec<_>>(), ["a", "b"]);
    }
}
