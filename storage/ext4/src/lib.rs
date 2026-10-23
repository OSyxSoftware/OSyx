//! ext2 / ext3 / ext4 file system driver, written from the on-disk format
//! description (Linux `Documentation/filesystems/ext4`), independent of any
//! existing implementation.
//!
//! Reading supports: block sizes 1-64 KiB, 32/64-byte group descriptors
//! (`64bit`), `flex_bg`, `meta_bg`, sparse superblock backups, extents and
//! classic direct/indirect block maps, linear and hashed (`dir_index`)
//! directories, fast and slow symbolic links, sparse and large files
//! (`large_file`, `huge_file`), nanosecond timestamps, and `metadata_csum`
//! / `gdt_csum` checksums, which are verified on every metadata read.
//!
//! Every on-disk value is range-checked before use; corrupt metadata
//! yields [`Error::Corrupt`], never a panic or an out-of-range access.
//! Features the driver does not implement are refused at mount time
//! ([`Error::Unsupported`]); features that only matter for writing make the
//! volume read-only (see [`Ext4::write_refusal`]).
//!
//! The engine is single-threaded (`&mut self`); the kernel wraps a volume
//! in a sleeping mutex.

#![no_std]

extern crate alloc;

pub mod crc;
pub mod dir;
pub mod inode;
mod map;
pub mod journal;
mod write;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

pub use dir::DirEntry;
pub use inode::{Inode, Stat};
pub use write::NewKind;

pub const SECTOR: usize = 512;
pub const ROOT_INO: u32 = 2;
const MAGIC: u16 = 0xEF53;

pub mod feature {
    pub const COMPAT_HAS_JOURNAL: u32 = 0x4;
    pub const COMPAT_EXT_ATTR: u32 = 0x8;
    pub const COMPAT_RESIZE_INODE: u32 = 0x10;
    pub const COMPAT_DIR_INDEX: u32 = 0x20;
    pub const COMPAT_SPARSE_SUPER2: u32 = 0x200;
    pub const COMPAT_FAST_COMMIT: u32 = 0x400;
    pub const COMPAT_ORPHAN_FILE: u32 = 0x1000;

    pub const INCOMPAT_COMPRESSION: u32 = 0x1;
    pub const INCOMPAT_FILETYPE: u32 = 0x2;
    pub const INCOMPAT_RECOVER: u32 = 0x4;
    pub const INCOMPAT_JOURNAL_DEV: u32 = 0x8;
    pub const INCOMPAT_META_BG: u32 = 0x10;
    pub const INCOMPAT_EXTENTS: u32 = 0x40;
    pub const INCOMPAT_64BIT: u32 = 0x80;
    pub const INCOMPAT_MMP: u32 = 0x100;
    pub const INCOMPAT_FLEX_BG: u32 = 0x200;
    pub const INCOMPAT_EA_INODE: u32 = 0x400;
    pub const INCOMPAT_DIRDATA: u32 = 0x1000;
    pub const INCOMPAT_CSUM_SEED: u32 = 0x2000;
    pub const INCOMPAT_LARGEDIR: u32 = 0x4000;
    pub const INCOMPAT_INLINE_DATA: u32 = 0x8000;
    pub const INCOMPAT_ENCRYPT: u32 = 0x10000;
    pub const INCOMPAT_CASEFOLD: u32 = 0x20000;

    pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x1;
    pub const RO_COMPAT_LARGE_FILE: u32 = 0x2;
    pub const RO_COMPAT_BTREE_DIR: u32 = 0x4;
    pub const RO_COMPAT_HUGE_FILE: u32 = 0x8;
    pub const RO_COMPAT_GDT_CSUM: u32 = 0x10;
    pub const RO_COMPAT_DIR_NLINK: u32 = 0x20;
    pub const RO_COMPAT_EXTRA_ISIZE: u32 = 0x40;
    pub const RO_COMPAT_HAS_SNAPSHOT: u32 = 0x80;
    pub const RO_COMPAT_QUOTA: u32 = 0x100;
    pub const RO_COMPAT_BIGALLOC: u32 = 0x200;
    pub const RO_COMPAT_METADATA_CSUM: u32 = 0x400;
    pub const RO_COMPAT_REPLICA: u32 = 0x800;
    pub const RO_COMPAT_READONLY: u32 = 0x1000;
    pub const RO_COMPAT_PROJECT: u32 = 0x2000;
    pub const RO_COMPAT_SHARED_BLOCKS: u32 = 0x4000;
    pub const RO_COMPAT_VERITY: u32 = 0x8000;
    pub const RO_COMPAT_ORPHAN_PRESENT: u32 = 0x10000;

    /// Incompatible features this driver can read.
    pub const INCOMPAT_READ: u32 = INCOMPAT_FILETYPE
        | INCOMPAT_RECOVER
        | INCOMPAT_META_BG
        | INCOMPAT_EXTENTS
        | INCOMPAT_64BIT
        | INCOMPAT_FLEX_BG
        | INCOMPAT_EA_INODE
        | INCOMPAT_CSUM_SEED
        | INCOMPAT_LARGEDIR;
    /// Read-only-compatible features this driver keeps correct when writing.
    pub const RO_COMPAT_WRITE: u32 = RO_COMPAT_SPARSE_SUPER
        | RO_COMPAT_LARGE_FILE
        | RO_COMPAT_HUGE_FILE
        | RO_COMPAT_GDT_CSUM
        | RO_COMPAT_DIR_NLINK
        | RO_COMPAT_EXTRA_ISIZE
        | RO_COMPAT_METADATA_CSUM;
    /// Compatible features whose on-disk state a writer must maintain;
    /// the others it may ignore safely.
    pub const COMPAT_WRITE_KNOWN: u32 =
        0x1 | 0x2 | COMPAT_HAS_JOURNAL | COMPAT_EXT_ATTR | COMPAT_RESIZE_INODE | COMPAT_DIR_INDEX | COMPAT_ORPHAN_FILE;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io,
    /// Not an ext2/3/4 volume.
    NotExt,
    Corrupt(&'static str),
    Unsupported(&'static str),
    NotFound,
    NotADirectory,
    IsADirectory,
    AlreadyExists,
    DirectoryNotEmpty,
    InvalidName,
    InvalidInput(&'static str),
    NoSpace,
    FileTooLarge,
    ReadOnly,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io => f.write_str("I/O error"),
            Error::NotExt => f.write_str("not an ext2/3/4 file system"),
            Error::Corrupt(w) => write!(f, "corrupt file system: {w}"),
            Error::Unsupported(w) => write!(f, "unsupported: {w}"),
            Error::NotFound => f.write_str("not found"),
            Error::NotADirectory => f.write_str("not a directory"),
            Error::IsADirectory => f.write_str("is a directory"),
            Error::AlreadyExists => f.write_str("already exists"),
            Error::DirectoryNotEmpty => f.write_str("directory not empty"),
            Error::InvalidName => f.write_str("invalid name"),
            Error::InvalidInput(w) => write!(f, "invalid input: {w}"),
            Error::NoSpace => f.write_str("no space left"),
            Error::FileTooLarge => f.write_str("file too large"),
            Error::ReadOnly => f.write_str("read-only volume"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Storage the driver runs on: 512-byte sectors.
pub trait BlockDevice {
    fn sector_count(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<()>;
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl BlockDevice for Vec<u8> {
    fn sector_count(&self) -> u64 {
        (self.len() / SECTOR) as u64
    }
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let o = (lba as usize).checked_mul(SECTOR).ok_or(Error::Io)?;
        let src = self.get(o..o.checked_add(buf.len()).ok_or(Error::Io)?).ok_or(Error::Io)?;
        buf.copy_from_slice(src);
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<()> {
        let o = (lba as usize).checked_mul(SECTOR).ok_or(Error::Io)?;
        let dst = self.get_mut(o..o.checked_add(buf.len()).ok_or(Error::Io)?).ok_or(Error::Io)?;
        dst.copy_from_slice(buf);
        Ok(())
    }
}

pub(crate) fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub(crate) fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// The parsed superblock (and its raw bytes, which writes update).
#[derive(Clone)]
pub struct Superblock {
    pub raw: [u8; 1024],
    pub inodes_count: u32,
    pub blocks_count: u64,
    pub first_data_block: u32,
    pub block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub state: u16,
    pub rev_level: u32,
    pub first_ino: u32,
    pub inode_size: u16,
    pub compat: u32,
    pub incompat: u32,
    pub ro_compat: u32,
    pub uuid: [u8; 16],
    pub desc_size: u32,
    pub reserved_gdt_blocks: u32,
    pub journal_inum: u32,
    pub first_meta_bg: u32,
    pub backup_bgs: [u32; 2],
    pub min_extra_isize: u16,
}

impl Superblock {
    pub fn parse(raw: &[u8; 1024]) -> Result<Superblock> {
        if le16(raw, 56) != MAGIC {
            return Err(Error::NotExt);
        }
        let rev_level = le32(raw, 76);
        if rev_level > 1 {
            return Err(Error::Unsupported("superblock revision above 1"));
        }
        let log = le32(raw, 24);
        if log > 6 {
            return Err(Error::Corrupt("block size above 64 KiB"));
        }
        let block_size = 1024u32 << log;
        let incompat = if rev_level == 0 { 0 } else { le32(raw, 96) };
        let (inode_size, first_ino) = if rev_level == 0 { (128, 11) } else { (le16(raw, 88), le32(raw, 84)) };
        if inode_size < 128 || !inode_size.is_power_of_two() || inode_size as u32 > block_size {
            return Err(Error::Corrupt("inode size"));
        }
        let blocks_count = le32(raw, 4) as u64
            | if incompat & feature::INCOMPAT_64BIT != 0 { (le32(raw, 0x150) as u64) << 32 } else { 0 };
        let desc_size = if incompat & feature::INCOMPAT_64BIT != 0 {
            let d = le16(raw, 254) as u32;
            if !(64..=1024).contains(&d) || !d.is_power_of_two() {
                return Err(Error::Corrupt("group descriptor size"));
            }
            d
        } else {
            32
        };
        let sb = Superblock {
            raw: *raw,
            inodes_count: le32(raw, 0),
            blocks_count,
            first_data_block: le32(raw, 20),
            block_size,
            blocks_per_group: le32(raw, 32),
            inodes_per_group: le32(raw, 40),
            state: le16(raw, 58),
            rev_level,
            first_ino,
            inode_size,
            compat: if rev_level == 0 { 0 } else { le32(raw, 92) },
            incompat,
            ro_compat: if rev_level == 0 { 0 } else { le32(raw, 100) },
            uuid: raw[104..120].try_into().expect("16 bytes"),
            desc_size,
            reserved_gdt_blocks: le16(raw, 206) as u32,
            journal_inum: le32(raw, 224),
            first_meta_bg: le32(raw, 260),
            backup_bgs: [le32(raw, 0x24C), le32(raw, 0x250)],
            min_extra_isize: le16(raw, 348),
        };
        let bits = block_size * 8;
        if sb.blocks_per_group == 0 || sb.blocks_per_group > bits || sb.inodes_per_group == 0 || sb.inodes_per_group > bits {
            return Err(Error::Corrupt("blocks or inodes per group"));
        }
        if sb.first_data_block != if block_size == 1024 { 1 } else { 0 } {
            return Err(Error::Corrupt("first data block"));
        }
        if sb.first_ino < 11 || sb.first_ino > sb.inodes_count {
            return Err(Error::Corrupt("first non-reserved inode"));
        }
        if sb.blocks_count <= sb.first_data_block as u64 {
            return Err(Error::Corrupt("block count"));
        }
        let groups = sb.group_count();
        if (groups as u64) * sb.inodes_per_group as u64 != sb.inodes_count as u64 {
            return Err(Error::Corrupt("inode count does not match the groups"));
        }
        if sb.ro_compat & feature::RO_COMPAT_METADATA_CSUM != 0 && sb.raw[0x175] != 1 {
            return Err(Error::Unsupported("checksum type other than crc32c"));
        }
        Ok(sb)
    }

    pub fn group_count(&self) -> u32 {
        (self.blocks_count - self.first_data_block as u64).div_ceil(self.blocks_per_group as u64) as u32
    }

    pub fn has_compat(&self, f: u32) -> bool {
        self.compat & f != 0
    }

    pub fn has_incompat(&self, f: u32) -> bool {
        self.incompat & f != 0
    }

    pub fn has_ro_compat(&self, f: u32) -> bool {
        self.ro_compat & f != 0
    }

    pub fn metadata_csum(&self) -> bool {
        self.has_ro_compat(feature::RO_COMPAT_METADATA_CSUM)
    }

    pub fn label(&self) -> String {
        let n = &self.raw[120..136];
        let end = n.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&n[..end]).into_owned()
    }

    /// Stored superblock checksum matches (or the volume has none).
    fn checksum_ok(&self) -> bool {
        !self.metadata_csum() || crc::crc32c(!0, &self.raw[..0x3FC]) == le32(&self.raw, 0x3FC)
    }

    pub(crate) fn update_checksum(&mut self) {
        if self.metadata_csum() {
            let c = crc::crc32c(!0, &self.raw[..0x3FC]);
            put32(&mut self.raw, 0x3FC, c);
        }
    }

    /// Does group `g` hold a superblock backup (and so a GDT copy)?
    pub fn has_super(&self, g: u32) -> bool {
        if g == 0 {
            return true;
        }
        if self.has_compat(feature::COMPAT_SPARSE_SUPER2) {
            return g == self.backup_bgs[0] || g == self.backup_bgs[1];
        }
        if g <= 1 || !self.has_ro_compat(feature::RO_COMPAT_SPARSE_SUPER) {
            return true;
        }
        for base in [3u32, 5, 7] {
            let mut p = base;
            while p < g {
                match p.checked_mul(base) {
                    Some(n) => p = n,
                    None => break,
                }
            }
            if p == g {
                return true;
            }
        }
        false
    }

    pub fn group_first_block(&self, g: u32) -> u64 {
        self.first_data_block as u64 + g as u64 * self.blocks_per_group as u64
    }

    pub fn descs_per_block(&self) -> u32 {
        self.block_size / self.desc_size
    }
}

/// One block group descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupDesc {
    pub block_bitmap: u64,
    pub inode_bitmap: u64,
    pub inode_table: u64,
    pub free_blocks: u32,
    pub free_inodes: u32,
    pub used_dirs: u32,
    pub flags: u16,
    pub itable_unused: u32,
    pub block_bitmap_csum: u32,
    pub inode_bitmap_csum: u32,
}

pub const BG_INODE_UNINIT: u16 = 1;
pub const BG_BLOCK_UNINIT: u16 = 2;
pub const BG_INODE_ZEROED: u16 = 4;

impl GroupDesc {
    fn parse(b: &[u8], wide: bool) -> GroupDesc {
        let hi32 = |o: usize| if wide { le32(b, o) as u64 } else { 0 };
        let hi16 = |o: usize| if wide { le16(b, o) as u32 } else { 0 };
        GroupDesc {
            block_bitmap: le32(b, 0) as u64 | hi32(32) << 32,
            inode_bitmap: le32(b, 4) as u64 | hi32(36) << 32,
            inode_table: le32(b, 8) as u64 | hi32(40) << 32,
            free_blocks: le16(b, 12) as u32 | hi16(44) << 16,
            free_inodes: le16(b, 14) as u32 | hi16(46) << 16,
            used_dirs: le16(b, 16) as u32 | hi16(48) << 16,
            flags: le16(b, 18),
            itable_unused: le16(b, 28) as u32 | hi16(50) << 16,
            block_bitmap_csum: le16(b, 24) as u32 | hi16(56) << 16,
            inode_bitmap_csum: le16(b, 26) as u32 | hi16(58) << 16,
        }
    }

    #[allow(dead_code)] // used by the writer
    pub(crate) fn store(&self, b: &mut [u8], wide: bool) {
        put32(b, 0, self.block_bitmap as u32);
        put32(b, 4, self.inode_bitmap as u32);
        put32(b, 8, self.inode_table as u32);
        put16(b, 12, self.free_blocks as u16);
        put16(b, 14, self.free_inodes as u16);
        put16(b, 16, self.used_dirs as u16);
        put16(b, 18, self.flags);
        put16(b, 24, self.block_bitmap_csum as u16);
        put16(b, 26, self.inode_bitmap_csum as u16);
        put16(b, 28, self.itable_unused as u16);
        if wide {
            put32(b, 32, (self.block_bitmap >> 32) as u32);
            put32(b, 36, (self.inode_bitmap >> 32) as u32);
            put32(b, 40, (self.inode_table >> 32) as u32);
            put16(b, 44, (self.free_blocks >> 16) as u16);
            put16(b, 46, (self.free_inodes >> 16) as u16);
            put16(b, 48, (self.used_dirs >> 16) as u16);
            put16(b, 50, (self.itable_unused >> 16) as u16);
            put16(b, 56, (self.block_bitmap_csum >> 16) as u16);
            put16(b, 58, (self.inode_bitmap_csum >> 16) as u16);
        }
    }
}

/// Small write-through cache of metadata blocks.
struct Cache {
    blocks: BTreeMap<u64, (Vec<u8>, u64)>,
    clock: u64,
    capacity: usize,
}

impl Cache {
    fn get(&mut self, b: u64) -> Option<&Vec<u8>> {
        self.clock += 1;
        let c = self.clock;
        self.blocks.get_mut(&b).map(|e| {
            e.1 = c;
            &e.0
        })
    }

    fn put(&mut self, b: u64, data: Vec<u8>) {
        if self.blocks.len() >= self.capacity && !self.blocks.contains_key(&b) {
            if let Some((&old, _)) = self.blocks.iter().min_by_key(|(_, e)| e.1) {
                self.blocks.remove(&old);
            }
        }
        self.clock += 1;
        self.blocks.insert(b, (data, self.clock));
    }

    #[allow(dead_code)] // used by the writer
    fn forget(&mut self, b: u64) {
        self.blocks.remove(&b);
    }
}

/// Mount-time options.
#[derive(Clone, Copy, Debug, Default)]
pub struct MountOptions {
    /// Never write to the device.
    pub read_only: bool,
}

/// A mounted volume.
pub struct Ext4<D: BlockDevice> {
    pub(crate) dev: D,
    pub(crate) sb: Superblock,
    pub(crate) groups: Vec<GroupDesc>,
    pub(crate) csum_seed: u32,
    cache: Cache,
    pub(crate) read_only: bool,
    /// Why writing is refused, if it is.
    pub(crate) write_refusal: Option<&'static str>,
    /// Journal state for writable mounts.
    pub(crate) journal: Option<journal::Journal>,
    /// Clock for timestamps: seconds and nanoseconds since the Unix epoch.
    pub(crate) clock: fn() -> (i64, u32),
    /// Inodes removed while still open: freed on their last release.
    #[allow(dead_code)] // used by the writer
    pub(crate) orphans: BTreeMap<u32, u32>,
    #[allow(dead_code)] // used by the writer
    pub(crate) open_counts: BTreeMap<u32, u32>,
}

fn zero_clock() -> (i64, u32) {
    (0, 0)
}

impl<D: BlockDevice> Ext4<D> {
    /// Mounts the volume on `dev`. A volume that is not cleanly unmounted,
    /// needs journal recovery that cannot be done, or uses features that
    /// are only safe to read is mounted read-only; [`Ext4::write_refusal`]
    /// says why.
    pub fn mount(mut dev: D, opts: MountOptions) -> Result<Self> {
        let mut raw = [0u8; 1024];
        if dev.sector_count() < 4 {
            return Err(Error::NotExt);
        }
        dev.read_sectors(2, &mut raw)?;
        let sb = Superblock::parse(&raw)?;
        if !sb.checksum_ok() {
            return Err(Error::Corrupt("superblock checksum"));
        }
        let unknown = sb.incompat & !feature::INCOMPAT_READ;
        if unknown != 0 {
            return Err(Error::Unsupported(match unknown {
                u if u & feature::INCOMPAT_INLINE_DATA != 0 => "inline_data",
                u if u & feature::INCOMPAT_ENCRYPT != 0 => "encrypt",
                u if u & feature::INCOMPAT_CASEFOLD != 0 => "casefold",
                u if u & feature::INCOMPAT_COMPRESSION != 0 => "compression",
                u if u & feature::INCOMPAT_JOURNAL_DEV != 0 => "external journal device",
                u if u & feature::INCOMPAT_MMP != 0 => "multiple mount protection",
                u if u & feature::INCOMPAT_DIRDATA != 0 => "dirdata",
                _ => "unknown incompatible feature",
            }));
        }
        if sb.has_ro_compat(feature::RO_COMPAT_BIGALLOC) {
            return Err(Error::Unsupported("bigalloc"));
        }
        let fs_bytes = sb.blocks_count.checked_mul(sb.block_size as u64).ok_or(Error::Corrupt("block count"))?;
        if fs_bytes > dev.sector_count() * SECTOR as u64 {
            return Err(Error::Corrupt("file system larger than the device"));
        }
        let csum_seed = if sb.has_incompat(feature::INCOMPAT_CSUM_SEED) { le32(&sb.raw, 0x270) } else { crc::crc32c(!0, &sb.uuid) };
        let mut fs = Ext4 {
            dev,
            sb,
            groups: Vec::new(),
            csum_seed,
            cache: Cache { blocks: BTreeMap::new(), clock: 0, capacity: 256 },
            read_only: true,
            write_refusal: None,
            journal: None,
            clock: zero_clock,
            orphans: BTreeMap::new(),
            open_counts: BTreeMap::new(),
        };
        fs.load_groups()?;
        let root = fs.inode(ROOT_INO)?;
        if !root.is_dir() {
            return Err(Error::Corrupt("root inode is not a directory"));
        }
        fs.write_refusal = fs.why_not_writable();
        if !opts.read_only {
            if let Some(why) = fs.write_refusal {
                return Err(Error::Unsupported(why));
            }
            fs.read_only = false;
        }
        if fs.sb.has_incompat(feature::INCOMPAT_RECOVER) {
            // A journal with unreplayed transactions: the metadata on disk
            // may be stale. Replay it (in memory for read-only mounts).
            fs.recover_journal(fs.read_only)?;
        }
        if !fs.read_only {
            fs.start_writing()?;
        }
        Ok(fs)
    }

    /// Why this volume cannot be mounted writable (`None`: it can).
    pub fn write_refusal(&self) -> Option<&'static str> {
        self.write_refusal
    }

    fn why_not_writable(&self) -> Option<&'static str> {
        let sb = &self.sb;
        if sb.ro_compat & !feature::RO_COMPAT_WRITE != 0 {
            return Some(match sb.ro_compat & !feature::RO_COMPAT_WRITE {
                f if f & feature::RO_COMPAT_QUOTA != 0 => "quota",
                f if f & feature::RO_COMPAT_ORPHAN_PRESENT != 0 => "orphan file has entries (needs e2fsck)",
                f if f & feature::RO_COMPAT_READONLY != 0 => "marked read-only",
                f if f & feature::RO_COMPAT_VERITY != 0 => "verity",
                f if f & feature::RO_COMPAT_PROJECT != 0 => "project quota",
                _ => "a read-only-compatible feature this driver does not maintain",
            });
        }
        if sb.compat & !feature::COMPAT_WRITE_KNOWN != 0 {
            return Some(match sb.compat & !feature::COMPAT_WRITE_KNOWN {
                f if f & feature::COMPAT_FAST_COMMIT != 0 => "fast_commit journal",
                f if f & feature::COMPAT_SPARSE_SUPER2 != 0 => "sparse_super2",
                _ => "a compatible feature whose metadata this driver does not maintain",
            });
        }
        if sb.has_incompat(feature::INCOMPAT_EA_INODE) {
            return Some("ea_inode");
        }
        if sb.has_incompat(feature::INCOMPAT_META_BG) {
            return Some("meta_bg (writing)");
        }
        if !sb.has_incompat(feature::INCOMPAT_EXTENTS) {
            return Some("block-mapped (ext2/ext3) files cannot be written");
        }
        if sb.state & 2 != 0 {
            return Some("the file system has errors (run e2fsck)");
        }
        if sb.state & 1 == 0 && !sb.has_incompat(feature::INCOMPAT_RECOVER) {
            return Some("not cleanly unmounted (run e2fsck)");
        }
        if sb.has_compat(feature::COMPAT_HAS_JOURNAL) && sb.journal_inum == 0 {
            return Some("external journal");
        }
        None
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    pub fn groups(&self) -> &[GroupDesc] {
        &self.groups
    }

    pub fn block_size(&self) -> usize {
        self.sb.block_size as usize
    }

    pub fn label(&self) -> String {
        self.sb.label()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// (free blocks, free inodes) from the superblock counters.
    pub fn free_counts(&self) -> (u64, u32) {
        (le32(&self.sb.raw, 12) as u64 | if self.sb.has_incompat(feature::INCOMPAT_64BIT) { (le32(&self.sb.raw, 0x158) as u64) << 32 } else { 0 }, le32(&self.sb.raw, 16))
    }

    /// Location of group-descriptor block `i` (`meta_bg` aware).
    pub(crate) fn desc_block(&self, i: u32) -> u64 {
        let sb = &self.sb;
        if !sb.has_incompat(feature::INCOMPAT_META_BG) || i < sb.first_meta_bg {
            sb.first_data_block as u64 + 1 + i as u64
        } else {
            let g = i * sb.descs_per_block();
            sb.group_first_block(g) + sb.has_super(g) as u64
        }
    }

    pub(crate) fn load_groups(&mut self) -> Result<()> {
        let count = self.sb.group_count();
        let per = self.sb.descs_per_block();
        let ds = self.sb.desc_size as usize;
        let wide = ds >= 64;
        let mut groups = Vec::with_capacity(count as usize);
        for i in 0..count.div_ceil(per) {
            let blk = self.desc_block(i);
            let data = self.read_block(blk)?;
            for j in 0..per {
                let g = i * per + j;
                if g >= count {
                    break;
                }
                let raw = &data[j as usize * ds..(j as usize + 1) * ds];
                if !self.desc_checksum_ok(g, raw) {
                    return Err(Error::Corrupt("group descriptor checksum"));
                }
                let d = GroupDesc::parse(raw, wide);
                let fs_blocks = self.sb.blocks_count;
                let itable_blocks = (self.sb.inodes_per_group as u64 * self.sb.inode_size as u64).div_ceil(self.sb.block_size as u64);
                let bad = |b: u64, n: u64| b < self.sb.first_data_block as u64 || b.checked_add(n).is_none_or(|e| e > fs_blocks);
                if bad(d.block_bitmap, 1) || bad(d.inode_bitmap, 1) || bad(d.inode_table, itable_blocks) {
                    return Err(Error::Corrupt("group metadata outside the file system"));
                }
                if d.free_inodes > self.sb.inodes_per_group || d.itable_unused > self.sb.inodes_per_group {
                    return Err(Error::Corrupt("group counters"));
                }
                groups.push(d);
            }
        }
        self.groups = groups;
        Ok(())
    }

    pub(crate) fn desc_checksum(&self, g: u32, raw: &[u8]) -> Option<u16> {
        let ds = self.sb.desc_size as usize;
        if self.sb.metadata_csum() {
            let mut c = crc::crc32c(self.csum_seed, &g.to_le_bytes());
            c = crc::crc32c(c, &raw[..30]);
            c = crc::crc32c(c, &[0, 0]);
            if ds > 32 {
                c = crc::crc32c(c, &raw[32..ds]);
            }
            Some((c & 0xFFFF) as u16)
        } else if self.sb.has_ro_compat(feature::RO_COMPAT_GDT_CSUM) {
            let mut c = crc::crc16(!0, &self.sb.uuid);
            c = crc::crc16(c, &g.to_le_bytes());
            c = crc::crc16(c, &raw[..30]);
            if ds > 32 {
                c = crc::crc16(c, &raw[32..ds]);
            }
            Some(c)
        } else {
            None
        }
    }

    fn desc_checksum_ok(&self, g: u32, raw: &[u8]) -> bool {
        self.desc_checksum(g, raw).is_none_or(|c| c == le16(raw, 30))
    }

    /// Reads one file system block (cached).
    pub(crate) fn read_block(&mut self, b: u64) -> Result<Vec<u8>> {
        if b >= self.sb.blocks_count {
            return Err(Error::Corrupt("block number beyond the end of the file system"));
        }
        if let Some(j) = &self.journal {
            if let Some(d) = j.overlay.get(&b) {
                return Ok(d.clone());
            }
        }
        if let Some(d) = self.cache.get(b) {
            return Ok(d.clone());
        }
        let bs = self.sb.block_size as usize;
        let mut buf = vec![0u8; bs];
        self.dev.read_sectors(b * (bs / SECTOR) as u64, &mut buf)?;
        self.cache.put(b, buf.clone());
        Ok(buf)
    }

    /// Reads `count` consecutive blocks straight from the device (file
    /// data; bypasses the cache and the journal overlay).
    pub(crate) fn read_blocks_raw(&mut self, b: u64, out: &mut [u8]) -> Result<()> {
        let bs = self.sb.block_size as u64;
        let n = out.len() as u64 / bs;
        if b.checked_add(n).is_none_or(|e| e > self.sb.blocks_count) {
            return Err(Error::Corrupt("data block beyond the end of the file system"));
        }
        if let Some(j) = &self.journal {
            if (b..b + n).any(|x| j.overlay.contains_key(&x)) {
                for i in 0..n {
                    let blk = self.read_block(b + i)?;
                    out[(i * bs) as usize..((i + 1) * bs) as usize].copy_from_slice(&blk);
                }
                return Ok(());
            }
        }
        self.dev.read_sectors(b * (bs / SECTOR as u64), out)
    }

    /// Writes a block to the device directly (used for data blocks and by
    /// the journal checkpoint); keeps the cache coherent.
    pub(crate) fn write_block_raw(&mut self, b: u64, data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if b >= self.sb.blocks_count {
            return Err(Error::Corrupt("write beyond the end of the file system"));
        }
        let bs = self.sb.block_size as usize;
        self.dev.write_sectors(b * (bs / SECTOR) as u64, data)?;
        if self.cache.blocks.contains_key(&b) {
            self.cache.put(b, data.to_vec());
        }
        Ok(())
    }

    #[allow(dead_code)] // used by the writer
    pub(crate) fn forget_cached(&mut self, b: u64) {
        self.cache.forget(b);
    }

    pub(crate) fn clear_cache(&mut self) {
        self.cache.blocks.clear();
    }

    /// The inode with number `ino`.
    pub fn inode(&mut self, ino: u32) -> Result<Inode> {
        let (blk, off) = self.inode_location(ino)?;
        let data = self.read_block(blk)?;
        let isz = self.sb.inode_size as usize;
        let ino_raw = &data[off..off + isz];
        let inode = Inode::parse(ino, ino_raw, &self.sb)?;
        if self.sb.metadata_csum() && !inode.checksum_ok(ino_raw, self.csum_seed, &self.sb) {
            return Err(Error::Corrupt("inode checksum"));
        }
        Ok(inode)
    }

    pub(crate) fn inode_location(&self, ino: u32) -> Result<(u64, usize)> {
        if ino == 0 || ino > self.sb.inodes_count {
            return Err(Error::Corrupt("inode number out of range"));
        }
        let ipg = self.sb.inodes_per_group;
        let g = (ino - 1) / ipg;
        let idx = (ino - 1) % ipg;
        let gd = &self.groups[g as usize];
        if gd.flags & BG_INODE_UNINIT != 0 && self.has_group_csums() {
            return Err(Error::Corrupt("reference to an inode in an uninitialised group"));
        }
        let isz = self.sb.inode_size as u64;
        let byte = idx as u64 * isz;
        let bs = self.sb.block_size as u64;
        Ok((gd.inode_table + byte / bs, (byte % bs) as usize))
    }

    pub(crate) fn has_group_csums(&self) -> bool {
        self.sb.metadata_csum() || self.sb.has_ro_compat(feature::RO_COMPAT_GDT_CSUM)
    }

    /// Metadata of `ino`.
    pub fn stat(&mut self, ino: u32) -> Result<Stat> {
        let i = self.inode(ino)?;
        if i.mode == 0 && i.links == 0 {
            return Err(Error::Corrupt("reference to a free inode"));
        }
        Ok(i.stat(&self.sb))
    }

    /// Reads file data at `offset`; returns the bytes read (0 at the end).
    pub fn read(&mut self, ino: u32, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(Error::IsADirectory);
        }
        self.read_inode_data(&inode, offset, buf)
    }

    /// The target of a symbolic link.
    pub fn readlink(&mut self, ino: u32) -> Result<String> {
        let inode = self.inode(ino)?;
        if !inode.is_symlink() {
            return Err(Error::InvalidInput("not a symbolic link"));
        }
        if inode.size > 4096 || inode.size == 0 {
            return Err(Error::Corrupt("symbolic link length"));
        }
        let bytes = if inode.is_fast_symlink(self.sb.block_size) {
            inode.block[..inode.size as usize].to_vec()
        } else {
            let mut b = vec![0u8; inode.size as usize];
            let n = self.read_inode_data(&inode, 0, &mut b)?;
            if n != b.len() {
                return Err(Error::Corrupt("symbolic link shorter than its size"));
            }
            b
        };
        String::from_utf8(bytes).map_err(|_| Error::Corrupt("symbolic link target is not UTF-8"))
    }

    /// Physical block holding logical block `lblk` of `ino` (`None`: hole
    /// or unwritten). For tools and tests.
    pub fn physical_block(&mut self, ino: u32, lblk: u64) -> Result<Option<u64>> {
        let inode = self.inode(ino)?;
        let run = self.map(&inode, lblk)?;
        Ok(run.phys.filter(|_| !run.uninit))
    }

    /// Flushes and returns the device (writes leave a clean volume).
    pub fn unmount(mut self) -> Result<D> {
        self.finish_writing()?;
        Ok(self.dev)
    }

    /// Sets the clock used for timestamps of changed inodes.
    pub fn set_clock(&mut self, clock: fn() -> (i64, u32)) {
        self.clock = clock;
    }
}
