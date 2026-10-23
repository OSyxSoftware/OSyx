//! The mount table: canonical absolute mount point paths and what is
//! mounted there, with longest-prefix lookup.

use crate::path::is_within;
use alloc::string::String;
use alloc::vec::Vec;
use osyx_abi::Errno;

pub struct MountTable<T> {
    /// Sorted by path length, longest first, so the first match of a
    /// prefix search is the longest.
    entries: Vec<(String, T)>,
}

impl<T> Default for MountTable<T> {
    fn default() -> Self {
        MountTable { entries: Vec::new() }
    }
}

impl<T> MountTable<T> {
    pub const fn new() -> Self {
        MountTable { entries: Vec::new() }
    }

    /// Adds a mount; `AlreadyExists` if something is mounted at `path`, and
    /// `InvalidArgument` unless `path` is canonical (absolute, no `.`/`..`,
    /// no trailing or repeated `/`). Only `/` may be mounted on an empty table.
    pub fn insert(&mut self, path: &str, value: T) -> Result<(), Errno> {
        let canonical = path == "/"
            || (path.starts_with('/')
                && !path.ends_with('/')
                && path[1..].split('/').all(|c| !c.is_empty() && c != "." && c != ".."));
        if !canonical {
            return Err(Errno::InvalidArgument);
        }
        if self.entries.is_empty() && path != "/" {
            return Err(Errno::NotFound);
        }
        if self.exact(path).is_some() {
            return Err(Errno::AlreadyExists);
        }
        let pos = self.entries.iter().position(|(p, _)| p.len() < path.len()).unwrap_or(self.entries.len());
        self.entries.insert(pos, (String::from(path), value));
        Ok(())
    }

    /// Removes the mount at `path`; `Busy` if other mounts lie below it.
    pub fn remove(&mut self, path: &str) -> Result<T, Errno> {
        let i = self.entries.iter().position(|(p, _)| p == path).ok_or(Errno::InvalidArgument)?;
        if self.entries.iter().any(|(p, _)| p != path && is_within(p, path)) {
            return Err(Errno::Busy);
        }
        Ok(self.entries.remove(i).1)
    }

    pub fn exact(&self, path: &str) -> Option<&T> {
        self.entries.iter().find(|(p, _)| p == path).map(|(_, v)| v)
    }

    /// The mount whose path is the longest prefix of `path`.
    pub fn longest_prefix(&self, path: &str) -> Option<(&str, &T)> {
        self.entries.iter().find(|(p, _)| is_within(path, p)).map(|(p, v)| (p.as_str(), v))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.entries.iter().map(|(p, v)| (p.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let mut t = MountTable::new();
        assert_eq!(t.insert("/tmp", 9), Err(Errno::NotFound), "root must come first");
        t.insert("/", 0).unwrap();
        t.insert("/mnt", 1).unwrap();
        t.insert("/mnt/usb", 2).unwrap();
        t.insert("/tmp", 3).unwrap();
        assert_eq!(t.longest_prefix("/mnt/usb/a/b").map(|x| *x.1), Some(2));
        assert_eq!(t.longest_prefix("/mnt/usbx").map(|x| *x.1), Some(1));
        assert_eq!(t.longest_prefix("/mnt").map(|x| *x.1), Some(1));
        assert_eq!(t.longest_prefix("/etc/x").map(|x| x.0), Some("/"));
        assert_eq!(t.longest_prefix("/tmp/f").map(|x| *x.1), Some(3));
        assert_eq!(t.insert("/tmp", 4), Err(Errno::AlreadyExists));
        for bad in ["tmp", "/tmp/", "/a//b", "/a/./b", "/a/.."] {
            assert_eq!(t.insert(bad, 5), Err(Errno::InvalidArgument), "{bad}");
        }
    }

    #[test]
    fn removal_respects_nesting() {
        let mut t = MountTable::new();
        t.insert("/", 0).unwrap();
        t.insert("/mnt", 1).unwrap();
        t.insert("/mnt/usb", 2).unwrap();
        assert_eq!(t.remove("/mnt"), Err(Errno::Busy));
        assert_eq!(t.remove("/nope"), Err(Errno::InvalidArgument));
        assert_eq!(t.remove("/mnt/usb"), Ok(2));
        assert_eq!(t.remove("/mnt"), Ok(1));
        assert_eq!(t.len(), 1);
        assert_eq!(t.longest_prefix("/mnt/usb").map(|x| *x.1), Some(0));
    }
}
