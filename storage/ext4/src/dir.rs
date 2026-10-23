//! Directories: linear entry blocks (hashed `dir_index` directories are
//! read linearly too: their index blocks look like empty entry blocks).

use crate::inode::Inode;
use crate::{crc, feature, le16, le32, BlockDevice, Error, Ext4, Result};
use alloc::string::String;
use alloc::vec::Vec;

/// Bytes of the checksum "tail" entry at the end of a leaf block.
pub(crate) const TAIL: usize = 12;

pub const FT_UNKNOWN: u8 = 0;
pub const FT_REG: u8 = 1;
pub const FT_DIR: u8 = 2;
pub const FT_CHR: u8 = 3;
pub const FT_BLK: u8 = 4;
pub const FT_FIFO: u8 = 5;
pub const FT_SOCK: u8 = 6;
pub const FT_SYMLINK: u8 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The name (non-UTF-8 bytes are shown as U+FFFD).
    pub name: String,
    pub ino: u32,
    /// `FT_*` (0 when the volume does not record types).
    pub file_type: u8,
}

/// A raw entry inside a directory block.
#[allow(dead_code)] // offsets are used by the writer
pub(crate) struct Raw<'a> {
    pub off: usize,
    pub ino: u32,
    pub rec_len: usize,
    pub name: &'a [u8],
    pub file_type: u8,
}

pub(crate) fn has_tail(b: &[u8]) -> bool {
    let o = b.len() - TAIL;
    le32(b, o) == 0 && le16(b, o + 4) as usize == TAIL && b[o + 6] == 0 && b[o + 7] == 0xDE
}

/// Parses every entry of a directory block, validating the layout.
pub(crate) fn entries(b: &[u8], filetype: bool, inodes: u32) -> Result<Vec<Raw<'_>>> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < b.len() {
        if off + 8 > b.len() {
            return Err(Error::Corrupt("directory entry header crosses the block"));
        }
        let ino = le32(b, off);
        let rec_len = le16(b, off + 4) as usize;
        let (name_len, ft) = if filetype { (b[off + 6] as usize, b[off + 7]) } else { (le16(b, off + 6) as usize, 0) };
        if rec_len < 8 || !rec_len.is_multiple_of(4) || off + rec_len > b.len() {
            return Err(Error::Corrupt("directory entry length"));
        }
        if name_len + 8 > rec_len {
            return Err(Error::Corrupt("directory entry name longer than the entry"));
        }
        if ino > inodes {
            return Err(Error::Corrupt("directory entry inode out of range"));
        }
        if ino != 0 && name_len == 0 {
            return Err(Error::Corrupt("directory entry without a name"));
        }
        out.push(Raw { off, ino, rec_len, name: &b[off + 8..off + 8 + name_len], file_type: ft });
        off += rec_len;
    }
    Ok(out)
}

impl<D: BlockDevice> Ext4<D> {
    /// Reads directory block `lblk` of `dir`, verifying its checksum tail.
    pub(crate) fn dir_block(&mut self, dir: &Inode, lblk: u64) -> Result<(u64, Vec<u8>)> {
        let run = self.map(dir, lblk)?;
        let phys = match run.phys {
            Some(p) if !run.uninit => p,
            _ => return Err(Error::Corrupt("hole in a directory")),
        };
        let b = self.read_block(phys)?;
        if self.sb.metadata_csum() && has_tail(&b) {
            let o = b.len() - TAIL;
            if crc::crc32c(dir.csum_seed(self.csum_seed), &b[..o]) != le32(&b, o + 8) {
                return Err(Error::Corrupt("directory block checksum"));
            }
        }
        Ok((phys, b))
    }

    pub(crate) fn dir_blocks(&self, dir: &Inode) -> Result<u64> {
        let bs = self.sb.block_size as u64;
        if !dir.size.is_multiple_of(bs) {
            return Err(Error::Corrupt("directory size is not a whole number of blocks"));
        }
        Ok(dir.size / bs)
    }

    fn dir_inode(&mut self, ino: u32) -> Result<Inode> {
        let d = self.inode(ino)?;
        if !d.is_dir() {
            return Err(Error::NotADirectory);
        }
        Ok(d)
    }

    /// Entries of directory `ino` without `.` and `..`.
    pub fn read_dir(&mut self, ino: u32) -> Result<Vec<DirEntry>> {
        let dir = self.dir_inode(ino)?;
        let filetype = self.sb.has_incompat(feature::INCOMPAT_FILETYPE);
        let mut out = Vec::new();
        for lblk in 0..self.dir_blocks(&dir)? {
            let (_, b) = self.dir_block(&dir, lblk)?;
            for e in entries(&b, filetype, self.sb.inodes_count)? {
                if e.ino == 0 || e.name == b"." || e.name == b".." {
                    continue;
                }
                out.push(DirEntry { name: String::from_utf8_lossy(e.name).into_owned(), ino: e.ino, file_type: e.file_type });
            }
        }
        Ok(out)
    }

    /// The inode number of `name` in directory `dir` (bytes compared exactly).
    pub fn lookup(&mut self, dir: u32, name: &str) -> Result<u32> {
        let d = self.dir_inode(dir)?;
        let filetype = self.sb.has_incompat(feature::INCOMPAT_FILETYPE);
        for lblk in 0..self.dir_blocks(&d)? {
            let (_, b) = self.dir_block(&d, lblk)?;
            for e in entries(&b, filetype, self.sb.inodes_count)? {
                if e.ino != 0 && e.name == name.as_bytes() {
                    return Ok(e.ino);
                }
            }
        }
        Err(Error::NotFound)
    }

    /// Resolves an absolute path inside the volume (no symlinks followed;
    /// for tools and tests).
    pub fn resolve(&mut self, path: &str) -> Result<u32> {
        let mut ino = crate::ROOT_INO;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            ino = self.lookup(ino, c)?;
        }
        Ok(ino)
    }
}
