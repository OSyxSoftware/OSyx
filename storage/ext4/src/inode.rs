//! Inodes: parsing, checksums, metadata.

use crate::{crc, feature, le16, le32, put16, put32, Error, Result, Superblock};

pub const S_IFMT: u16 = 0xF000;
pub const S_IFREG: u16 = 0x8000;
pub const S_IFDIR: u16 = 0x4000;
pub const S_IFLNK: u16 = 0xA000;
pub const S_IFCHR: u16 = 0x2000;
pub const S_IFBLK: u16 = 0x6000;
pub const S_IFIFO: u16 = 0x1000;
pub const S_IFSOCK: u16 = 0xC000;

pub const FL_INDEX: u32 = 0x1000;
pub const FL_HUGE_FILE: u32 = 0x40000;
pub const FL_EXTENTS: u32 = 0x80000;
pub const FL_EA_INODE: u32 = 0x200000;
pub const FL_INLINE_DATA: u32 = 0x1000_0000;
pub const FL_ENCRYPT: u32 = 0x800;
pub const FL_CASEFOLD: u32 = 0x4000_0000;

/// A parsed inode (the fields the driver uses; `raw` keeps the rest).
#[derive(Clone, Debug)]
pub struct Inode {
    pub ino: u32,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub links: u16,
    /// Allocated storage in 512-byte units.
    pub blocks_512: u64,
    pub flags: u32,
    pub generation: u32,
    pub block: [u8; 60],
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
    pub dtime: u32,
    pub extra_isize: u16,
    pub raw: alloc::vec::Vec<u8>,
}

/// Metadata as callers see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub ino: u32,
    /// File type bits (`S_IF*`) and permissions, as on disk.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub links: u16,
    pub blocks_512: u64,
    pub atime_ns: i64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
}

impl Stat {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
    pub fn perm(&self) -> u16 {
        self.mode & 0o7777
    }
}

fn decode_time(raw: &[u8], base: usize, extra: Option<usize>) -> (i64, u32) {
    let secs = le32(raw, base) as i32 as i64;
    match extra {
        Some(e) => {
            let x = le32(raw, e);
            (secs + (((x & 3) as i64) << 32), x >> 2)
        }
        None => (secs, 0),
    }
}

fn encode_time(raw: &mut [u8], base: usize, extra: Option<usize>, t: (i64, u32)) {
    put32(raw, base, t.0 as u32);
    if let Some(e) = extra {
        let epoch = ((t.0 - (t.0 as i32 as i64)) >> 32) as u32 & 3;
        put32(raw, e, (t.1.min(999_999_999) << 2) | epoch);
    }
}

impl Inode {
    pub fn parse(ino: u32, raw: &[u8], sb: &Superblock) -> Result<Inode> {
        let isz = sb.inode_size as usize;
        let extra_isize = if isz > 128 { le16(raw, 128) } else { 0 };
        if isz > 128 && (extra_isize as usize > isz - 128 || extra_isize % 4 != 0) {
            return Err(Error::Corrupt("inode extra size"));
        }
        let fits = |end: usize| isz > 128 && 128 + extra_isize as usize >= end;
        let huge = sb.has_ro_compat(feature::RO_COMPAT_HUGE_FILE);
        let flags = le32(raw, 32);
        let mut blocks = le32(raw, 28) as u64;
        if huge {
            blocks |= (le16(raw, 116) as u64) << 32;
            if flags & FL_HUGE_FILE != 0 {
                blocks *= sb.block_size as u64 / 512;
            }
        }
        let i = Inode {
            ino,
            mode: le16(raw, 0),
            uid: le16(raw, 2) as u32 | (le16(raw, 120) as u32) << 16,
            gid: le16(raw, 24) as u32 | (le16(raw, 122) as u32) << 16,
            size: le32(raw, 4) as u64 | (le32(raw, 108) as u64) << 32,
            links: le16(raw, 26),
            blocks_512: blocks,
            flags,
            generation: le32(raw, 100),
            block: raw[40..100].try_into().expect("60 bytes"),
            atime: decode_time(raw, 8, fits(144).then_some(140)),
            ctime: decode_time(raw, 12, fits(136).then_some(132)),
            mtime: decode_time(raw, 16, fits(140).then_some(136)),
            dtime: le32(raw, 20),
            extra_isize,
            raw: raw[..isz].to_vec(),
        };
        if i.flags & FL_INLINE_DATA != 0 {
            return Err(Error::Unsupported("inline data"));
        }
        if i.flags & (FL_ENCRYPT | FL_CASEFOLD) != 0 {
            return Err(Error::Unsupported("encrypted or case-folded inode"));
        }
        Ok(i)
    }

    /// Writes the parsed fields back into `raw` (checksum not included).
    pub fn store(&mut self, sb: &Superblock) {
        let isz = sb.inode_size as usize;
        let fits = |end: usize| isz > 128 && 128 + self.extra_isize as usize >= end;
        let raw = &mut self.raw;
        put16(raw, 0, self.mode);
        put16(raw, 2, self.uid as u16);
        put16(raw, 120, (self.uid >> 16) as u16);
        put16(raw, 24, self.gid as u16);
        put16(raw, 122, (self.gid >> 16) as u16);
        put32(raw, 4, self.size as u32);
        put32(raw, 108, (self.size >> 32) as u32);
        put16(raw, 26, self.links);
        let mut blocks = self.blocks_512;
        let mut flags = self.flags & !FL_HUGE_FILE;
        if sb.has_ro_compat(feature::RO_COMPAT_HUGE_FILE) && blocks >= 1 << 48 {
            blocks /= sb.block_size as u64 / 512;
            flags |= FL_HUGE_FILE;
        }
        put32(raw, 28, blocks as u32);
        put16(raw, 116, (blocks >> 32) as u16);
        put32(raw, 32, flags);
        put32(raw, 100, self.generation);
        raw[40..100].copy_from_slice(&self.block);
        put32(raw, 20, self.dtime);
        encode_time(raw, 8, fits(144).then_some(140), self.atime);
        encode_time(raw, 12, fits(136).then_some(132), self.ctime);
        encode_time(raw, 16, fits(140).then_some(136), self.mtime);
        if isz > 128 {
            put16(raw, 128, self.extra_isize);
        }
    }

    fn checksum(raw: &[u8], ino: u32, generation: u32, seed: u32, sb: &Superblock) -> u32 {
        let isz = sb.inode_size as usize;
        let mut c = crc::crc32c(seed, &ino.to_le_bytes());
        c = crc::crc32c(c, &generation.to_le_bytes());
        c = crc::crc32c(c, &raw[..124]);
        c = crc::crc32c(c, &[0, 0]);
        c = crc::crc32c(c, &raw[126..128]);
        if isz > 128 {
            let extra = le16(raw, 128) as usize;
            if extra >= 4 {
                c = crc::crc32c(c, &raw[128..130]);
                c = crc::crc32c(c, &[0, 0]);
                c = crc::crc32c(c, &raw[132..isz]);
            } else {
                c = crc::crc32c(c, &raw[128..isz]);
            }
        }
        c
    }

    pub fn checksum_ok(&self, raw: &[u8], seed: u32, sb: &Superblock) -> bool {
        let c = Self::checksum(raw, self.ino, self.generation, seed, sb);
        let lo = le16(raw, 124) as u32;
        if sb.inode_size > 128 && le16(raw, 128) >= 4 {
            lo | (le16(raw, 130) as u32) << 16 == c
        } else {
            lo == c & 0xFFFF
        }
    }

    pub fn update_checksum(&mut self, seed: u32, sb: &Superblock) {
        if !sb.metadata_csum() {
            return;
        }
        let c = Self::checksum(&self.raw, self.ino, self.generation, seed, sb);
        put16(&mut self.raw, 124, c as u16);
        if sb.inode_size > 128 && self.extra_isize >= 4 {
            put16(&mut self.raw, 130, (c >> 16) as u16);
        }
    }

    /// Seed for checksums of blocks owned by this inode (directory and
    /// extent blocks).
    pub fn csum_seed(&self, fs_seed: u32) -> u32 {
        let c = crc::crc32c(fs_seed, &self.ino.to_le_bytes());
        crc::crc32c(c, &self.generation.to_le_bytes())
    }

    pub fn file_type(&self) -> u16 {
        self.mode & S_IFMT
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == S_IFDIR
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == S_IFLNK
    }

    pub fn uses_extents(&self) -> bool {
        self.flags & FL_EXTENTS != 0
    }

    /// Block number of the extended-attribute block (0: none).
    pub fn file_acl(&self) -> u64 {
        le32(&self.raw, 104) as u64 | (le16(&self.raw, 118) as u64) << 32
    }

    /// Target stored inside the inode: no data blocks apart from a
    /// possible extended-attribute block.
    pub fn is_fast_symlink(&self, block_size: u32) -> bool {
        let ea_blocks = if self.file_acl() != 0 { block_size as u64 / 512 } else { 0 };
        self.is_symlink() && self.size < 60 && self.blocks_512.saturating_sub(ea_blocks) == 0
    }

    pub fn stat(&self, _sb: &Superblock) -> Stat {
        let ns = |t: (i64, u32)| t.0.saturating_mul(1_000_000_000).saturating_add(t.1 as i64);
        Stat {
            ino: self.ino,
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            size: self.size,
            links: self.links,
            blocks_512: self.blocks_512,
            atime_ns: ns(self.atime),
            mtime_ns: ns(self.mtime),
            ctime_ns: ns(self.ctime),
        }
    }
}
