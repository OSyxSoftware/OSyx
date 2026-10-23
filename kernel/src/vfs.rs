//! Virtual file system: a mount table of volumes and open files.
//!
//! Paths are absolute, `/`-separated UTF-8. They are normalised lexically
//! (`.`, `..`, repeated slashes) before the mount with the longest matching
//! prefix is chosen, so `..` never escapes a mount point into the volume's
//! own root by accident. Only FAT32 volumes exist for now.
//!
//! Locking: an [`OpenFile`] lives in a `KMutex` (one per handle) and each
//! volume's driver state in another; the order is open file -> volume ->
//! block device. Metadata changes are written through: every mutating
//! operation ends with a flush of the volume's FAT cache, FSInfo and the
//! disk's write cache, so a power cut (or QEMU exiting) loses nothing that a
//! call has already reported as done.

use crate::block::{self, Disk};
use crate::kprintln;
use crate::sync::{IrqSpinLock, KMutex};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use osyx_abi::{DirRecord, Errno, FileStat};
use osyx_fat::{DirEntryInfo, FileRef, FileSystem, FsError};

pub struct Mount {
    pub path: String,
    pub device: String,
    pub label: String,
    read_only: bool,
    fs: KMutex<FileSystem<Disk>>,
}

static MOUNTS: IrqSpinLock<Vec<Arc<Mount>>> = IrqSpinLock::new(Vec::new());

pub fn fs_errno(e: FsError) -> Errno {
    match e {
        FsError::Io | FsError::Corrupt(_) => Errno::Io,
        FsError::NotFound => Errno::NotFound,
        FsError::AlreadyExists => Errno::AlreadyExists,
        FsError::NotADirectory => Errno::NotADirectory,
        FsError::IsADirectory => Errno::IsADirectory,
        FsError::DirectoryNotEmpty => Errno::DirectoryNotEmpty,
        FsError::InvalidName | FsError::InvalidInput(_) => Errno::InvalidArgument,
        FsError::NoSpace => Errno::NoSpace,
        FsError::FileTooLarge => Errno::FileTooLarge,
        FsError::Unsupported(_) => Errno::NotSupported,
    }
}

/// Normalises an absolute path into its components.
pub fn normalize(path: &str) -> Result<Vec<&str>, Errno> {
    if !path.starts_with('/') || path.len() > osyx_abi::MAX_PATH {
        return Err(Errno::InvalidArgument);
    }
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    Ok(parts)
}

/// Finds the mount for `path` and the path inside that volume.
fn resolve(path: &str) -> Result<(Arc<Mount>, String), Errno> {
    let parts = normalize(path)?;
    let mounts = MOUNTS.lock();
    let mut best: Option<(&Arc<Mount>, usize)> = None;
    for m in mounts.iter() {
        let mp = normalize(&m.path).unwrap_or_default();
        if parts.len() >= mp.len()
            && parts.iter().zip(&mp).all(|(a, b)| a.eq_ignore_ascii_case(b))
            && best.is_none_or(|(_, n)| mp.len() > n)
        {
            best = Some((m, mp.len()));
        }
    }
    let (m, skip) = best.ok_or(Errno::NotFound)?;
    let mut inner = String::from("/");
    inner.push_str(&parts[skip..].join("/"));
    Ok((m.clone(), inner))
}

/// Mounts every registered disk that holds a FAT32 volume: the first
/// writable one at `/`, the others at `/vol1`, `/vol2`, ...
pub fn mount_all() -> usize {
    let mut count = 0;
    let mut devices = block::devices();
    // Writable disks first, so `/` is the data disk rather than read-only media.
    devices.sort_by_key(|d| d.read_only());
    for dev in devices {
        let name = dev.name().to_string();
        let read_only = dev.read_only();
        match FileSystem::mount(Disk(dev)) {
            Ok(fs) => {
                let path = if count == 0 { "/".to_string() } else { alloc::format!("/vol{count}") };
                let label = fs.label().to_string();
                kprintln!(
                    "vfs: {} mounted at {} (FAT32 \"{}\", {}-byte clusters, {} MiB free{})",
                    name,
                    path,
                    label,
                    fs.geometry().cluster_bytes(),
                    fs.free_bytes() >> 20,
                    if read_only { ", read-only" } else { "" }
                );
                MOUNTS.lock().push(Arc::new(Mount { path, device: name, label, read_only, fs: KMutex::new(fs) }));
                count += 1;
            }
            Err(e) => kprintln!("vfs: {}: no FAT32 volume ({})", name, e),
        }
    }
    count
}

pub fn mounts() -> Vec<Arc<Mount>> {
    MOUNTS.lock().clone()
}

/// Flushes every volume (used before the kernel stops the machine).
pub fn sync_all() {
    for m in mounts() {
        if !m.read_only {
            if let Err(e) = m.fs.lock().flush() {
                kprintln!("vfs: flushing {} failed: {}", m.path, e);
            }
        }
    }
}

impl Mount {
    /// Full consistency check of the volume (FAT chains, sizes, free count).
    pub fn check(&self) -> Result<osyx_fat::CheckReport, FsError> {
        self.fs.lock().check()
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn writable(&self) -> Result<(), Errno> {
        if self.read_only {
            Err(Errno::ReadOnly)
        } else {
            Ok(())
        }
    }

    /// Runs a mutating operation and writes the volume's metadata through.
    fn modify<T>(&self, f: impl FnOnce(&mut FileSystem<Disk>) -> osyx_fat::Result<T>) -> Result<T, Errno> {
        self.writable()?;
        let mut fs = self.fs.lock();
        let r = f(&mut fs);
        let flushed = fs.flush();
        let v = r.map_err(fs_errno)?;
        flushed.map_err(fs_errno)?;
        Ok(v)
    }
}

fn stat_of(is_dir: bool, size: u64, attributes: u8) -> FileStat {
    FileStat {
        size,
        kind: if is_dir { osyx_abi::KIND_DIR } else { osyx_abi::KIND_FILE },
        attributes: attributes as u32,
    }
}

enum Kind {
    File(FileRef),
    /// Directory listing, taken when the first `readdir` call arrives.
    Dir { path: String, entries: Option<Vec<DirEntryInfo>>, next: usize },
}

pub struct OpenFile {
    mount: Arc<Mount>,
    kind: Kind,
    pos: u64,
    read: bool,
    write: bool,
    append: bool,
}

pub fn open(path: &str, flags: u64) -> Result<OpenFile, Errno> {
    use osyx_abi::*;
    if flags & !OPEN_ALL != 0 || flags & (OPEN_READ | OPEN_WRITE) == 0 {
        return Err(Errno::InvalidArgument);
    }
    let write = flags & OPEN_WRITE != 0;
    let create = flags & OPEN_CREATE != 0;
    if !write && flags & (OPEN_CREATE | OPEN_TRUNCATE | OPEN_APPEND | OPEN_EXCLUSIVE) != 0 {
        return Err(Errno::InvalidArgument);
    }
    if flags & OPEN_EXCLUSIVE != 0 && !create {
        return Err(Errno::InvalidArgument);
    }
    let (mount, inner) = resolve(path)?;
    let existing = mount.fs.lock().metadata(&inner);
    let kind = match existing {
        Ok(md) if md.is_dir => {
            if write {
                return Err(Errno::IsADirectory);
            }
            Kind::Dir { path: inner, entries: None, next: 0 }
        }
        Ok(_) if flags & OPEN_EXCLUSIVE != 0 => return Err(Errno::AlreadyExists),
        Err(FsError::NotFound) if !create => return Err(Errno::NotFound),
        Err(e) if e != FsError::NotFound => return Err(fs_errno(e)),
        _ => {
            let opts = osyx_fat::OpenOptions {
                create,
                create_new: flags & OPEN_EXCLUSIVE != 0,
                truncate: flags & OPEN_TRUNCATE != 0,
            };
            let f = if write || create {
                mount.modify(|fs| fs.open(&inner, opts))?
            } else {
                mount.fs.lock().open(&inner, opts).map_err(fs_errno)?
            };
            Kind::File(f)
        }
    };
    Ok(OpenFile { mount, kind, pos: 0, read: flags & OPEN_READ != 0, write, append: flags & OPEN_APPEND != 0 })
}

impl OpenFile {
    fn file(&mut self) -> Result<&mut FileRef, Errno> {
        match &mut self.kind {
            Kind::File(f) => Ok(f),
            Kind::Dir { .. } => Err(Errno::IsADirectory),
        }
    }

    /// Reads at the current position into `buf` without moving it: the
    /// caller calls [`advance`](Self::advance) once the data has reached
    /// its destination, so a failed copy to user memory consumes nothing.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        if !self.read {
            return Err(Errno::BadHandle);
        }
        let pos = self.pos;
        let mount = self.mount.clone();
        let f = self.file()?;
        let n = mount.fs.lock().read(f, pos, buf).map_err(fs_errno)?;
        Ok(n)
    }

    pub fn advance(&mut self, n: usize) {
        self.pos += n as u64;
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, Errno> {
        if !self.write {
            return Err(Errno::BadHandle);
        }
        let mount = self.mount.clone();
        let append = self.append;
        let mut pos = self.pos;
        let f = self.file()?;
        let n = mount.modify(|fs| {
            if append {
                pos = fs.stat(f)?.size as u64;
            }
            fs.write(f, pos, data)
        })?;
        self.pos = pos + n as u64;
        Ok(n)
    }

    pub fn seek(&mut self, offset: i64, whence: u64) -> Result<u64, Errno> {
        let base = match whence {
            osyx_abi::SEEK_SET => 0,
            osyx_abi::SEEK_CUR => self.pos,
            osyx_abi::SEEK_END => {
                let mount = self.mount.clone();
                let f = self.file()?;
                let size = mount.fs.lock().stat(f).map_err(fs_errno)?.size;
                size as u64
            }
            _ => return Err(Errno::InvalidArgument),
        };
        if matches!(self.kind, Kind::Dir { .. }) {
            return Err(Errno::IsADirectory);
        }
        let new = (base as i128) + offset as i128;
        if !(0..=u32::MAX as i128).contains(&new) {
            return Err(Errno::InvalidArgument);
        }
        self.pos = new as u64;
        Ok(self.pos)
    }

    pub fn stat(&mut self) -> Result<FileStat, Errno> {
        let mount = self.mount.clone();
        match &mut self.kind {
            Kind::File(f) => {
                let md = mount.fs.lock().stat(f).map_err(fs_errno)?;
                Ok(stat_of(false, md.size as u64, md.attributes))
            }
            Kind::Dir { path, .. } => {
                let md = mount.fs.lock().metadata(path).map_err(fs_errno)?;
                Ok(stat_of(true, 0, md.attributes))
            }
        }
    }

    pub fn truncate(&mut self, len: u64) -> Result<(), Errno> {
        if !self.write {
            return Err(Errno::BadHandle);
        }
        let mount = self.mount.clone();
        let f = self.file()?;
        mount.modify(|fs| fs.truncate(f, len))
    }

    pub fn sync(&mut self) -> Result<(), Errno> {
        if self.mount.read_only {
            return Ok(());
        }
        self.mount.fs.lock().flush().map_err(fs_errno)
    }

    /// Encodes the next directory records into `out`; returns bytes used
    /// (0 when the listing is complete).
    pub fn readdir(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
        let mount = self.mount.clone();
        let Kind::Dir { path, entries, next } = &mut self.kind else {
            return Err(Errno::NotADirectory);
        };
        if entries.is_none() {
            *entries = Some(mount.fs.lock().read_dir(path).map_err(fs_errno)?);
        }
        let list = entries.as_ref().expect("listing loaded");
        let mut used = 0;
        while let Some(e) = list.get(*next) {
            let rec = DirRecord {
                size: e.size as u64,
                kind: if e.is_dir { osyx_abi::KIND_DIR } else { osyx_abi::KIND_FILE },
                attributes: e.attributes as u16,
                name_len: e.name.len() as u16,
            };
            match rec.encode(e.name.as_bytes(), &mut out[used..]) {
                Some(n) => used += n,
                None if used == 0 => return Err(Errno::InvalidArgument),
                None => break,
            }
            *next += 1;
        }
        Ok(used)
    }
}

pub fn stat(path: &str) -> Result<FileStat, Errno> {
    let (mount, inner) = resolve(path)?;
    let md = mount.fs.lock().metadata(&inner).map_err(fs_errno)?;
    Ok(stat_of(md.is_dir, if md.is_dir { 0 } else { md.size as u64 }, md.attributes))
}

pub fn mkdir(path: &str) -> Result<(), Errno> {
    let (mount, inner) = resolve(path)?;
    mount.modify(|fs| fs.create_dir(&inner))
}

pub fn unlink(path: &str) -> Result<(), Errno> {
    let (mount, inner) = resolve(path)?;
    mount.modify(|fs| fs.remove_file(&inner))
}

pub fn rmdir(path: &str) -> Result<(), Errno> {
    let (mount, inner) = resolve(path)?;
    if inner == "/" {
        return Err(Errno::PermissionDenied);
    }
    mount.modify(|fs| fs.remove_dir(&inner))
}

pub fn rename(from: &str, to: &str) -> Result<(), Errno> {
    let (m1, a) = resolve(from)?;
    let (m2, b) = resolve(to)?;
    if !Arc::ptr_eq(&m1, &m2) {
        return Err(Errno::CrossDevice);
    }
    if a == "/" || b == "/" {
        return Err(Errno::PermissionDenied);
    }
    m1.modify(|fs| fs.rename(&a, &b))
}
