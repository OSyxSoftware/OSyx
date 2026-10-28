//! Creates a new ext4 volume ("mkfs"): 4 KiB blocks, extents, a JBD2
//! journal, sparse superblock backups, 256-byte inodes and `lost+found`.
//!
//! The layout is the classic one (no `flex_bg`, no metadata checksums, no
//! resize inode): every group holds, in order, an optional superblock and
//! descriptor-table backup, its block bitmap, inode bitmap and inode table.
//! Inode tables are zeroed, so no lazy-init state is left behind. The
//! result is accepted by `e2fsck -fn` and mounts writable here.

use crate::inode::{FL_EXTENTS, S_IFDIR, S_IFREG};
use crate::{feature, put16, put32, BlockDevice, Error, Result, SECTOR};
use alloc::vec;
use alloc::vec::Vec;

const BLOCK: u64 = 4096;
const BLOCKS_PER_GROUP: u64 = 32768;
const INODE_SIZE: u64 = 256;
const JOURNAL_INO: u32 = 8;
const LOST_FOUND_INO: u32 = 11;
const FIRST_INO: u32 = 11;
/// Bytes written per device request while zeroing.
const ZERO_RUN: u64 = 1 << 20;

/// What to create.
#[derive(Clone, Copy, Debug)]
pub struct FormatOptions<'a> {
    /// Volume label (at most 16 bytes are stored).
    pub label: &'a str,
    /// Volume UUID.
    pub uuid: [u8; 16],
    /// Creation time (seconds since the Unix epoch).
    pub time: u32,
}

/// Summary of a new volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Formatted {
    pub blocks: u64,
    pub groups: u32,
    pub inodes: u32,
    pub journal_blocks: u32,
}

struct Group {
    start: u64,
    blocks: u64,
    has_super: bool,
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
}

fn has_super(g: u32) -> bool {
    if g <= 1 {
        return true;
    }
    [3u32, 5, 7].iter().any(|&b| {
        let mut p = b;
        while p < g {
            p = p.saturating_mul(b);
        }
        p == g
    })
}

fn write_blocks<D: BlockDevice>(dev: &mut D, block: u64, data: &[u8]) -> Result<()> {
    dev.write_sectors(block * (BLOCK / SECTOR as u64), data)
}

fn zero_blocks<D: BlockDevice>(dev: &mut D, mut block: u64, mut count: u64) -> Result<()> {
    let zeros = vec![0u8; ZERO_RUN as usize];
    while count > 0 {
        let n = count.min(ZERO_RUN / BLOCK);
        write_blocks(dev, block, &zeros[..(n * BLOCK) as usize])?;
        block += n;
        count -= n;
    }
    Ok(())
}

fn set_bit(map: &mut [u8], bit: u64) {
    map[(bit / 8) as usize] |= 1 << (bit % 8);
}

/// An inode with one extent of `len` blocks at `start` (or none).
fn inode(mode: u16, links: u16, size: u64, start: u64, len: u64, time: u32) -> [u8; INODE_SIZE as usize] {
    let mut b = [0u8; INODE_SIZE as usize];
    put16(&mut b, 0, mode);
    put32(&mut b, 4, size as u32);
    for o in [8, 12, 16] {
        put32(&mut b, o, time);
    }
    put16(&mut b, 26, links);
    put32(&mut b, 28, (len * (BLOCK / SECTOR as u64)) as u32);
    put32(&mut b, 32, FL_EXTENTS);
    // Extent header and one leaf entry in i_block.
    put16(&mut b, 40, 0xF30A);
    put16(&mut b, 42, if len > 0 { 1 } else { 0 });
    put16(&mut b, 44, 4);
    if len > 0 {
        put32(&mut b, 52, 0);
        put16(&mut b, 56, len as u16);
        put16(&mut b, 58, (start >> 32) as u16);
        put32(&mut b, 60, start as u32);
    }
    put32(&mut b, 108, (size >> 32) as u32);
    put16(&mut b, 128, 32);
    b
}

fn dir_entry(b: &mut [u8], at: usize, ino: u32, rec_len: usize, name: &str) {
    put32(b, at, ino);
    put16(b, at + 4, rec_len as u16);
    b[at + 6] = name.len() as u8;
    b[at + 7] = 2; // directory
    b[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
}

fn jbd2_superblock(blocks: u32) -> Vec<u8> {
    let mut b = vec![0u8; BLOCK as usize];
    let be = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut b, 0, 0xC03B_3998);
    be(&mut b, 4, 4); // superblock v2
    be(&mut b, 12, BLOCK as u32);
    be(&mut b, 16, blocks);
    be(&mut b, 20, 1); // first log block
    be(&mut b, 24, 1); // next sequence
    be(&mut b, 28, 0); // empty
    be(&mut b, 40, 0x1); // incompat: revoke
    be(&mut b, 64, 1); // users
    b
}

/// Journal size for a volume of `blocks` blocks.
fn journal_blocks(blocks: u64) -> u64 {
    match blocks {
        b if b >= 262_144 => 16_384,
        b if b >= 65_536 => 4_096,
        _ => 1_024,
    }
}

/// Writes a new, empty ext4 volume over the whole device. Everything on the
/// device is lost.
pub fn format<D: BlockDevice>(dev: &mut D, opts: &FormatOptions<'_>) -> Result<Formatted> {
    let total = dev.sector_count() * SECTOR as u64 / BLOCK;
    if total < 4096 {
        return Err(Error::InvalidInput("volume smaller than 16 MiB"));
    }
    if total >> 32 != 0 {
        return Err(Error::InvalidInput("volume of 16 TiB or more"));
    }
    let gdt_for = |groups: u64| (groups * 32).div_ceil(BLOCK);
    let bytes_per_inode: u64 = if total * BLOCK >= 32 << 30 { 65_536 } else { 16_384 };
    let ipg = (BLOCKS_PER_GROUP * BLOCK / bytes_per_inode).next_multiple_of(16) as u32;
    let itable = ipg as u64 * INODE_SIZE / BLOCK;
    let mut groups = total.div_ceil(BLOCKS_PER_GROUP);
    // A last group too small for its own metadata is dropped (like mke2fs).
    let tail = total - (groups - 1) * BLOCKS_PER_GROUP;
    if groups > 1 && tail < 1 + gdt_for(groups) + 2 + itable + 64 {
        groups -= 1;
    }
    let blocks = if groups * BLOCKS_PER_GROUP < total { groups * BLOCKS_PER_GROUP } else { total };
    let gdt = gdt_for(groups);
    let layout: Vec<Group> = (0..groups)
        .map(|g| {
            let start = g * BLOCKS_PER_GROUP;
            let n = (blocks - start).min(BLOCKS_PER_GROUP);
            let has_super = has_super(g as u32);
            let meta = start + if has_super { 1 + gdt } else { 0 };
            Group { start, blocks: n, has_super, block_bitmap: meta, inode_bitmap: meta + 1, inode_table: meta + 2 }
        })
        .collect();
    let g0 = &layout[0];
    let mut next = g0.inode_table + itable;
    let root_block = next;
    let lost_found = next + 1;
    next += 1 + 4;
    let jblocks = journal_blocks(blocks).min(g0.start + g0.blocks - next - 16);
    if jblocks < 1024 {
        return Err(Error::InvalidInput("volume too small for a journal"));
    }
    let journal = next;
    next += jblocks;
    let used_g0_data = next - (g0.inode_table + itable);

    // Bitmaps and inode tables.
    let mut free_blocks = 0u64;
    let mut descs = vec![0u8; (gdt * BLOCK) as usize];
    for (g, l) in layout.iter().enumerate() {
        let overhead = l.inode_table + itable - l.start;
        let used = overhead + if g == 0 { used_g0_data } else { 0 };
        let mut bmap = vec![0u8; BLOCK as usize];
        for b in 0..used {
            set_bit(&mut bmap, b);
        }
        // Bits past the end of a short last group are marked in use.
        for b in l.blocks..BLOCK * 8 {
            set_bit(&mut bmap, b);
        }
        let mut imap = vec![0u8; BLOCK as usize];
        if g == 0 {
            for i in 0..FIRST_INO as u64 {
                set_bit(&mut imap, i);
            }
        }
        for i in ipg as u64..BLOCK * 8 {
            set_bit(&mut imap, i);
        }
        write_blocks(dev, l.block_bitmap, &bmap)?;
        write_blocks(dev, l.inode_bitmap, &imap)?;
        zero_blocks(dev, l.inode_table, itable)?;
        let free = l.blocks - used;
        free_blocks += free;
        let d = &mut descs[g * 32..g * 32 + 32];
        put32(d, 0, l.block_bitmap as u32);
        put32(d, 4, l.inode_bitmap as u32);
        put32(d, 8, l.inode_table as u32);
        put16(d, 12, free as u16);
        put16(d, 14, (if g == 0 { ipg - FIRST_INO } else { ipg }) as u16);
        put16(d, 16, if g == 0 { 2 } else { 0 });
    }

    // Reserved inodes, root, journal and lost+found (all in group 0).
    let t = opts.time;
    let mut table = vec![0u8; (FIRST_INO as u64 * INODE_SIZE).next_multiple_of(BLOCK) as usize];
    let mut put_inode = |ino: u32, raw: &[u8; INODE_SIZE as usize]| {
        let at = ((ino - 1) as u64 * INODE_SIZE) as usize;
        table[at..at + INODE_SIZE as usize].copy_from_slice(raw);
    };
    put_inode(2, &inode(S_IFDIR | 0o755, 3, BLOCK, root_block, 1, t));
    let journal_inode = inode(S_IFREG | 0o600, 1, jblocks * BLOCK, journal, jblocks, t);
    put_inode(JOURNAL_INO, &journal_inode);
    put_inode(LOST_FOUND_INO, &inode(S_IFDIR | 0o700, 2, 4 * BLOCK, lost_found, 4, t));
    write_blocks(dev, g0.inode_table, &table)?;

    let mut root = vec![0u8; BLOCK as usize];
    dir_entry(&mut root, 0, 2, 12, ".");
    dir_entry(&mut root, 12, 2, 12, "..");
    dir_entry(&mut root, 24, LOST_FOUND_INO, BLOCK as usize - 24, "lost+found");
    write_blocks(dev, root_block, &root)?;
    let mut lf = vec![0u8; BLOCK as usize];
    dir_entry(&mut lf, 0, LOST_FOUND_INO, 12, ".");
    dir_entry(&mut lf, 12, 2, BLOCK as usize - 12, "..");
    write_blocks(dev, lost_found, &lf)?;
    let mut empty = vec![0u8; BLOCK as usize];
    put16(&mut empty, 4, BLOCK as u16);
    for b in 1..4 {
        write_blocks(dev, lost_found + b, &empty)?;
    }
    zero_blocks(dev, journal + 1, jblocks - 1)?;
    write_blocks(dev, journal, &jbd2_superblock(jblocks as u32))?;

    // Superblock and its backups.
    let inodes = ipg * groups as u32;
    let mut sb = [0u8; 1024];
    put32(&mut sb, 0, inodes);
    put32(&mut sb, 4, blocks as u32);
    put32(&mut sb, 8, (blocks / 20) as u32);
    put32(&mut sb, 12, free_blocks as u32);
    put32(&mut sb, 16, inodes - FIRST_INO);
    put32(&mut sb, 20, 0);
    put32(&mut sb, 24, 2);
    put32(&mut sb, 28, 2);
    put32(&mut sb, 32, BLOCKS_PER_GROUP as u32);
    put32(&mut sb, 36, BLOCKS_PER_GROUP as u32);
    put32(&mut sb, 40, ipg);
    put32(&mut sb, 48, t);
    put16(&mut sb, 54, 0xFFFF);
    put16(&mut sb, 56, crate::MAGIC);
    put16(&mut sb, 58, 1); // clean
    put16(&mut sb, 60, 1); // continue on errors
    put32(&mut sb, 64, t);
    put32(&mut sb, 76, 1);
    put32(&mut sb, 84, FIRST_INO);
    put16(&mut sb, 88, INODE_SIZE as u16);
    put32(&mut sb, 92, feature::COMPAT_HAS_JOURNAL);
    put32(&mut sb, 96, feature::INCOMPAT_FILETYPE | feature::INCOMPAT_EXTENTS);
    put32(
        &mut sb,
        100,
        feature::RO_COMPAT_SPARSE_SUPER | feature::RO_COMPAT_LARGE_FILE | feature::RO_COMPAT_DIR_NLINK | feature::RO_COMPAT_EXTRA_ISIZE,
    );
    sb[104..120].copy_from_slice(&opts.uuid);
    let label = opts.label.as_bytes();
    let n = label.len().min(16);
    sb[120..120 + n].copy_from_slice(&label[..n]);
    put32(&mut sb, 224, JOURNAL_INO);
    // Directory hash seed (unused without dir_index, set like mke2fs).
    sb[236..252].copy_from_slice(&opts.uuid);
    sb[252] = 1;
    // Journal inode backup: i_block, then i_size_high and i_size.
    sb[253] = 1;
    sb[268..268 + 60].copy_from_slice(&journal_inode[40..100]);
    put32(&mut sb, 268 + 60, ((jblocks * BLOCK) >> 32) as u32);
    put32(&mut sb, 268 + 64, (jblocks * BLOCK) as u32);
    put32(&mut sb, 264, t);
    put16(&mut sb, 348, 32);
    put16(&mut sb, 350, 32);
    for (g, l) in layout.iter().enumerate() {
        if !l.has_super {
            continue;
        }
        let mut copy = sb;
        put16(&mut copy, 90, g as u16);
        let mut first = vec![0u8; BLOCK as usize];
        if g == 0 {
            first[1024..2048].copy_from_slice(&copy);
        } else {
            first[..1024].copy_from_slice(&copy);
        }
        write_blocks(dev, l.start, &first)?;
        write_blocks(dev, l.start + 1, &descs)?;
    }
    dev.flush()?;
    Ok(Formatted { blocks, groups: groups as u32, inodes, journal_blocks: jblocks as u32 })
}
