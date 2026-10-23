//! Files and directories on the volumes the kernel has mounted.
//!
//! ```ignore
//! use os_std::fs;
//! fs::write("/notes/hello.txt", "Hello from OSyx")?;
//! let text = fs::read_to_string("/notes/hello.txt")?;
//! for entry in fs::read_dir("/notes")? {
//!     println!("{} {}", entry.file_name(), entry.metadata().len());
//! }
//! ```
//!
//! Paths are absolute (there is no current directory yet). On OSyx they are
//! resolved by the kernel VFS; the data volume is FAT32, so names are
//! case-insensitive and there are no permissions beyond the read-only
//! attribute. The host back end maps paths below a scratch directory and only
//! exists to unit-test this module.

use crate::error::{Error, ErrorKind, Result};
use crate::io::{Read, Seek, SeekFrom, Write};
use crate::path::{Path, PathBuf};
use crate::sys::fs as imp;
use alloc::string::String;
use alloc::vec::Vec;
use osyx_abi::FileStat;

/// An open file (or, on OSyx, a directory opened for reading). Closed on drop.
pub struct File {
    inner: imp::RawFile,
}

impl File {
    /// Opens an existing file for reading.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<File> {
        OpenOptions::new().read(true).open(path)
    }

    /// Opens a file for writing, creating it or truncating it to zero length.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<File> {
        OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }

    /// Creates a new file for reading and writing; fails if it exists.
    pub fn create_new<P: AsRef<Path>>(path: P) -> Result<File> {
        OpenOptions::new().read(true).write(true).create_new(true).open(path)
    }

    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    pub fn metadata(&self) -> Result<Metadata> {
        self.inner.stat().map(Metadata)
    }

    /// Truncates or extends (with zeros) the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> Result<()> {
        self.inner.set_len(len)
    }

    /// Writes the file's metadata and flushes the disk's write cache.
    pub fn sync_all(&self) -> Result<()> {
        self.inner.sync()
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.write(buf)
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let (offset, whence) = match pos {
            SeekFrom::Start(n) => (i64::try_from(n).map_err(|_| ErrorKind::InvalidInput)?, osyx_abi::SEEK_SET),
            SeekFrom::Current(n) => (n, osyx_abi::SEEK_CUR),
            SeekFrom::End(n) => (n, osyx_abi::SEEK_END),
        };
        self.inner.seek(offset, whence)
    }
}

impl core::fmt::Debug for File {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("File")
    }
}

/// How to open a file, like `std::fs::OpenOptions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions::default()
    }

    pub fn read(&mut self, on: bool) -> &mut OpenOptions {
        self.read = on;
        self
    }

    pub fn write(&mut self, on: bool) -> &mut OpenOptions {
        self.write = on;
        self
    }

    /// Every write goes to the end of the file (implies `write`).
    pub fn append(&mut self, on: bool) -> &mut OpenOptions {
        self.append = on;
        self
    }

    pub fn truncate(&mut self, on: bool) -> &mut OpenOptions {
        self.truncate = on;
        self
    }

    pub fn create(&mut self, on: bool) -> &mut OpenOptions {
        self.create = on;
        self
    }

    /// Create the file; fail with `AlreadyExists` if it exists.
    pub fn create_new(&mut self, on: bool) -> &mut OpenOptions {
        self.create_new = on;
        self
    }

    fn flags(&self) -> Result<u64> {
        use osyx_abi::*;
        let write = self.write || self.append;
        if !self.read && !write {
            return Err(Error::new(ErrorKind::InvalidInput, "neither read nor write access requested"));
        }
        if (self.truncate || self.create || self.create_new) && !write {
            return Err(Error::new(ErrorKind::InvalidInput, "create and truncate need write access"));
        }
        if self.truncate && self.append {
            return Err(Error::new(ErrorKind::InvalidInput, "truncate and append are exclusive"));
        }
        let mut flags = 0;
        for (on, bit) in [
            (self.read, OPEN_READ),
            (write, OPEN_WRITE),
            (self.append, OPEN_APPEND),
            (self.truncate, OPEN_TRUNCATE),
            (self.create || self.create_new, OPEN_CREATE),
            (self.create_new, OPEN_EXCLUSIVE),
        ] {
            if on {
                flags |= bit;
            }
        }
        Ok(flags)
    }

    pub fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let flags = self.flags()?;
        imp::open(path_str(path.as_ref())?, flags).map(|inner| File { inner })
    }
}

/// Size, kind and attributes of a file or directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata(FileStat);

impl Metadata {
    pub fn is_dir(&self) -> bool {
        self.0.kind == osyx_abi::KIND_DIR
    }

    pub fn is_file(&self) -> bool {
        self.0.kind == osyx_abi::KIND_FILE
    }

    /// Size in bytes (0 for directories).
    pub fn len(&self) -> u64 {
        self.0.size
    }

    pub fn is_empty(&self) -> bool {
        self.0.size == 0
    }

    /// The `osyx_abi::ATTR_*` bits (FAT attributes on the data volume).
    pub fn attributes(&self) -> u32 {
        self.0.attributes
    }

    pub fn readonly(&self) -> bool {
        self.0.attributes & osyx_abi::ATTR_READ_ONLY != 0
    }
}

/// One entry of [`read_dir`].
#[derive(Clone, Debug)]
pub struct DirEntry {
    dir: PathBuf,
    name: String,
    stat: FileStat,
}

impl DirEntry {
    pub fn file_name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(self.name.as_str())
    }

    /// Metadata as of the listing (no further system call).
    pub fn metadata(&self) -> Metadata {
        Metadata(self.stat)
    }
}

/// Iterator over a directory listing, without `.` and `..`. The listing is
/// read completely when [`read_dir`] is called.
pub struct ReadDir {
    entries: alloc::vec::IntoIter<DirEntry>,
}

impl Iterator for ReadDir {
    type Item = DirEntry;
    fn next(&mut self) -> Option<DirEntry> {
        self.entries.next()
    }
}

fn path_str(p: &Path) -> Result<&str> {
    if !p.is_absolute() {
        return Err(Error::new(ErrorKind::InvalidInput, "paths must be absolute"));
    }
    if p.as_str().len() > osyx_abi::MAX_PATH {
        return Err(Error::new(ErrorKind::InvalidInput, "path too long"));
    }
    Ok(p.as_str())
}

pub fn metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    imp::stat(path_str(path.as_ref())?).map(Metadata)
}

/// True if `path` exists; errors other than `NotFound` are reported.
pub fn exists<P: AsRef<Path>>(path: P) -> Result<bool> {
    match metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The whole contents of a file.
pub fn read<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = Vec::with_capacity(f.metadata().map_or(0, |m| m.len() as usize));
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// The whole contents of a UTF-8 text file.
pub fn read_to_string<P: AsRef<Path>>(path: P) -> Result<String> {
    let mut s = String::new();
    File::open(path)?.read_to_string(&mut s)?;
    Ok(s)
}

/// Creates or replaces the file at `path` with `contents`.
pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
    File::create(path)?.write_all(contents.as_ref())
}

/// Copies a file's contents; returns the number of bytes copied.
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<u64> {
    let mut src = File::open(from)?;
    let mut dst = File::create(to)?;
    let mut buf = alloc::vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        match src.read(&mut buf)? {
            0 => return Ok(total),
            n => {
                dst.write_all(&buf[..n])?;
                total += n as u64;
            }
        }
    }
}

pub fn read_dir<P: AsRef<Path>>(path: P) -> Result<ReadDir> {
    let dir = path.as_ref();
    let list = imp::read_dir(path_str(dir)?)?;
    let entries: Vec<DirEntry> = list
        .into_iter()
        .filter(|(name, _)| name != "." && name != "..")
        .map(|(name, stat)| DirEntry { dir: dir.to_path_buf(), name, stat })
        .collect();
    Ok(ReadDir { entries: entries.into_iter() })
}

pub fn create_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    imp::mkdir(path_str(path.as_ref())?)
}

/// Creates `path` and any missing parents.
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    path_str(path)?;
    let mut cur = PathBuf::from("/");
    for c in path.components() {
        if c == ".." {
            return Err(Error::new(ErrorKind::InvalidInput, "create_dir_all does not accept `..`"));
        }
        cur.push(c);
        match create_dir(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists && metadata(&cur)?.is_dir() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    imp::unlink(path_str(path.as_ref())?)
}

/// Removes an empty directory.
pub fn remove_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    imp::rmdir(path_str(path.as_ref())?)
}

/// Removes a directory and everything below it.
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    for entry in read_dir(path)? {
        if entry.metadata().is_dir() {
            remove_dir_all(entry.path())?;
        } else {
            remove_file(entry.path())?;
        }
    }
    remove_dir(path)
}

/// Moves `from` to `to` on the same volume, replacing an existing file.
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    imp::rename(path_str(from.as_ref())?, path_str(to.as_ref())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A fresh directory per test (tests run in parallel in one process).
    fn scratch(name: &str) -> PathBuf {
        let p = PathBuf::from(alloc::format!("/{name}"));
        let _ = remove_dir_all(&p);
        create_dir(&p).unwrap();
        p
    }

    #[test]
    fn write_read_append_and_metadata() {
        let d = scratch("t-basic");
        let f = d.join("a.txt");
        write(&f, "hello").unwrap();
        assert_eq!(read_to_string(&f).unwrap(), "hello");
        OpenOptions::new().append(true).open(&f).unwrap().write_all(b", OSyx").unwrap();
        assert_eq!(read(&f).unwrap(), b"hello, OSyx");
        let m = metadata(&f).unwrap();
        assert!(m.is_file() && !m.is_dir() && m.len() == 11);
        assert!(metadata(&d).unwrap().is_dir());
        assert!(exists(&f).unwrap() && !exists(d.join("missing")).unwrap());
        assert_eq!(File::open(d.join("missing")).unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(File::create_new(&f).unwrap_err().kind(), ErrorKind::AlreadyExists);
    }

    #[test]
    fn seek_set_len_and_partial_reads() {
        let d = scratch("t-seek");
        let p = d.join("data.bin");
        let mut f = File::create_new(&p).unwrap();
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        f.write_all(&data).unwrap();
        assert_eq!(f.seek(SeekFrom::Start(100)).unwrap(), 100);
        let mut buf = [0u8; 4];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[100..104]);
        assert_eq!(f.seek(SeekFrom::Current(-4)).unwrap(), 100);
        assert_eq!(f.seek(SeekFrom::End(-1)).unwrap(), 9_999);
        f.set_len(50).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 50);
        f.set_len(60).unwrap();
        f.sync_all().unwrap();
        drop(f);
        let back = read(&p).unwrap();
        assert_eq!(&back[..50], &data[..50]);
        assert_eq!(&back[50..], &[0u8; 10]);
    }

    #[test]
    fn directories_listing_rename_and_removal() {
        let d = scratch("t-dirs");
        create_dir_all(d.join("x/y/z")).unwrap();
        create_dir_all(d.join("x/y")).unwrap();
        write(d.join("x/one.txt"), [1u8; 3]).unwrap();
        write(d.join("x/y/two.txt"), vec![2u8; 5]).unwrap();
        let mut names: Vec<(String, bool, u64)> =
            read_dir(d.join("x")).unwrap().map(|e| (String::from(e.file_name()), e.metadata().is_dir(), e.metadata().len())).collect();
        names.sort();
        assert_eq!(names, [(String::from("one.txt"), false, 3), (String::from("y"), true, 0)]);
        rename(d.join("x/one.txt"), d.join("x/y/moved.txt")).unwrap();
        assert!(!exists(d.join("x/one.txt")).unwrap());
        assert_eq!(copy(d.join("x/y/moved.txt"), d.join("copy.txt")).unwrap(), 3);
        assert_eq!(remove_dir(d.join("x")).unwrap_err().kind(), ErrorKind::DirectoryNotEmpty);
        remove_dir_all(d.join("x")).unwrap();
        assert!(!exists(d.join("x")).unwrap());
        remove_file(d.join("copy.txt")).unwrap();
        assert_eq!(read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn invalid_options_and_paths_are_rejected() {
        assert_eq!(OpenOptions::new().open("/x").unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(OpenOptions::new().read(true).create(true).open("/x").unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(
            OpenOptions::new().append(true).truncate(true).open("/x").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(read("relative.txt").unwrap_err().kind(), ErrorKind::InvalidInput);
        let long = alloc::format!("/{}", "a".repeat(osyx_abi::MAX_PATH));
        assert_eq!(metadata(long).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(create_dir_all("/a/../b").unwrap_err().kind(), ErrorKind::InvalidInput);
    }
}
