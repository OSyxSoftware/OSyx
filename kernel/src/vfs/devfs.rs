//! devfs: a read-only view of the device registry (`crate::devices`).
//!
//! Nodes come and go with driver registrations; directories (`disks`) exist
//! while some node lies below them. Opening a node hands the VFS a
//! [`DeviceFile`], which performs the I/O; devfs itself holds no data.

use super::{Filesystem, Result};
use crate::devices::{self, DeviceFile, DeviceKind, DeviceNode};
use crate::sync::IrqSpinLock;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use osyx_abi::{Errno, FileStat, KIND_BLOCK_DEVICE, KIND_CHAR_DEVICE, KIND_DIR};
use osyx_vfs::{DirEntry, FsUsage};

const ROOT: u64 = 1;

/// Directory inode <-> path below /dev ("" is the root), and the next number.
type DirTable = (BTreeMap<u64, String>, BTreeMap<String, u64>, u64);

pub struct DevFs {
    dirs: IrqSpinLock<DirTable>,
    created: i64,
}

impl DevFs {
    pub fn new() -> DevFs {
        let mut by_ino = BTreeMap::new();
        let mut by_path = BTreeMap::new();
        by_ino.insert(ROOT, String::new());
        by_path.insert(String::new(), ROOT);
        DevFs { dirs: IrqSpinLock::new((by_ino, by_path, ROOT + 1)), created: crate::time::now_ns() }
    }

    fn dir_path(&self, ino: u64) -> Option<String> {
        self.dirs.lock().0.get(&ino).cloned()
    }

    fn dir_ino(&self, path: &str) -> u64 {
        let mut d = self.dirs.lock();
        if let Some(&i) = d.1.get(path) {
            return i;
        }
        let i = d.2;
        d.2 += 1;
        d.0.insert(i, path.to_string());
        d.1.insert(path.to_string(), i);
        i
    }

    fn node(&self, ino: u64) -> Option<Arc<DeviceNode>> {
        devices::list().into_iter().find(|n| n.ino == ino)
    }
}

fn child(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        alloc::format!("{dir}/{name}")
    }
}

impl Filesystem for DevFs {
    fn fs_type(&self) -> &'static str {
        "devfs"
    }

    fn root(&self) -> u64 {
        ROOT
    }

    fn lookup(&self, dir: u64, name: &str) -> Result<u64> {
        let d = self.dir_path(dir).ok_or(Errno::NotADirectory)?;
        let p = child(&d, name);
        let nodes = devices::list();
        if let Some(n) = nodes.iter().find(|n| n.path == p) {
            return Ok(n.ino);
        }
        let prefix = alloc::format!("{p}/");
        if nodes.iter().any(|n| n.path.starts_with(&prefix)) {
            return Ok(self.dir_ino(&p));
        }
        Err(Errno::NotFound)
    }

    fn stat(&self, ino: u64) -> Result<FileStat> {
        let base = FileStat { ino, atime_ns: self.created, mtime_ns: self.created, ctime_ns: self.created, ..Default::default() };
        if let Some(path) = self.dir_path(ino) {
            // A directory whose last node was removed no longer exists.
            if !path.is_empty() && !devices::list().iter().any(|n| n.path.starts_with(&alloc::format!("{path}/"))) {
                return Err(Errno::NotFound);
            }
            return Ok(FileStat { kind: KIND_DIR, mode: 0o755, nlink: 2, ..base });
        }
        let n = self.node(ino).ok_or(Errno::NotFound)?;
        let (kind, size) = match &n.kind {
            DeviceKind::Char(_) => (KIND_CHAR_DEVICE, 0),
            DeviceKind::Block(b) => (KIND_BLOCK_DEVICE, b.sector_count() * crate::block::SECTOR_SIZE as u64),
        };
        Ok(FileStat { kind, size, mode: n.mode, uid: n.uid, gid: n.gid, rdev: n.rdev, nlink: 1, ..base })
    }

    fn read_dir(&self, dir: u64) -> Result<Vec<DirEntry>> {
        let d = self.dir_path(dir).ok_or(Errno::NotADirectory)?;
        let prefix = if d.is_empty() { String::new() } else { alloc::format!("{d}/") };
        let mut out: BTreeMap<String, DirEntry> = BTreeMap::new();
        for n in devices::list() {
            let Some(rest) = n.path.strip_prefix(&prefix) else { continue };
            match rest.split_once('/') {
                None => {
                    let kind = match n.kind {
                        DeviceKind::Char(_) => KIND_CHAR_DEVICE,
                        DeviceKind::Block(_) => KIND_BLOCK_DEVICE,
                    };
                    out.insert(rest.to_string(), DirEntry { name: rest.to_string(), ino: n.ino, kind });
                }
                Some((sub, _)) => {
                    let ino = self.dir_ino(&child(&d, sub));
                    out.insert(sub.to_string(), DirEntry { name: sub.to_string(), ino, kind: KIND_DIR });
                }
            }
        }
        Ok(out.into_values().collect())
    }

    fn read(&self, _ino: u64, _offset: u64, _buf: &mut [u8]) -> Result<usize> {
        Err(Errno::InvalidArgument)
    }

    fn device(&self, ino: u64) -> Option<DeviceFile> {
        self.node(ino).map(DeviceFile)
    }

    fn create(&self, _dir: u64, _name: &str, _what: osyx_vfs::NewNode<'_>, _mode: u32, _cred: &osyx_vfs::Cred) -> Result<u64> {
        Err(Errno::PermissionDenied)
    }

    fn unlink(&self, _dir: u64, _name: &str) -> Result<()> {
        Err(Errno::PermissionDenied)
    }

    fn rmdir(&self, _dir: u64, _name: &str) -> Result<()> {
        Err(Errno::PermissionDenied)
    }

    fn rename(&self, _sdir: u64, _sname: &str, _ddir: u64, _dname: &str) -> Result<()> {
        Err(Errno::PermissionDenied)
    }

    fn usage(&self) -> FsUsage {
        FsUsage { block_size: 4096, total_inodes: devices::count() as u64, ..Default::default() }
    }
}
