//! Logical-to-physical block mapping (extent trees and classic block
//! maps) and file data reads.

use crate::inode::Inode;
use crate::{crc, le16, le32, BlockDevice, Error, Ext4, Result};
use alloc::vec;

pub(crate) const EXTENT_MAGIC: u16 = 0xF30A;
const MAX_DEPTH: u16 = 5;
/// Length of an "unwritten" extent is stored with this bias.
const UNINIT_BIAS: u16 = 32768;

/// A stretch of logical blocks starting at the one asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    /// Physical block of the first logical block, `None` for a hole.
    pub phys: Option<u64>,
    /// Number of logical blocks with the same treatment.
    pub len: u64,
    /// Allocated but unwritten (reads as zeros).
    pub uninit: bool,
}

pub(crate) struct Header {
    pub entries: u16,
    pub max: u16,
    pub depth: u16,
}

pub(crate) fn parse_header(b: &[u8], room: usize) -> Result<Header> {
    if b.len() < 12 || le16(b, 0) != EXTENT_MAGIC {
        return Err(Error::Corrupt("bad extent header magic"));
    }
    let h = Header { entries: le16(b, 2), max: le16(b, 4), depth: le16(b, 6) };
    if h.entries > h.max || 12 + h.max as usize * 12 > room || h.depth > MAX_DEPTH {
        return Err(Error::Corrupt("bad extent header"));
    }
    Ok(h)
}

/// One leaf entry: (logical start, length, physical start, unwritten).
pub(crate) fn leaf_entry(b: &[u8], i: usize) -> (u32, u16, u64, bool) {
    let o = 12 + 12 * i;
    let raw_len = le16(b, o + 4);
    let (len, uninit) = if raw_len > UNINIT_BIAS { (raw_len - UNINIT_BIAS, true) } else { (raw_len, false) };
    let start = le32(b, o + 8) as u64 | (le16(b, o + 6) as u64) << 32;
    (le32(b, o), len, start, uninit)
}

/// One index entry: (logical start, child block).
pub(crate) fn index_entry(b: &[u8], i: usize) -> (u32, u64) {
    let o = 12 + 12 * i;
    (le32(b, o), le32(b, o + 4) as u64 | (le16(b, o + 8) as u64) << 32)
}

impl<D: BlockDevice> Ext4<D> {
    /// Reads and verifies an extent tree block of `inode`.
    pub(crate) fn extent_block(&mut self, inode: &Inode, blk: u64, want_depth: u16) -> crate::Result<alloc::vec::Vec<u8>> {
        let b = self.read_block(blk)?;
        let h = parse_header(&b, b.len())?;
        if h.depth != want_depth {
            return Err(Error::Corrupt("extent tree depth mismatch"));
        }
        if self.sb.metadata_csum() {
            let tail = 12 + 12 * h.max as usize;
            if tail + 4 > b.len() {
                return Err(Error::Corrupt("extent block has no room for its checksum"));
            }
            if crc::crc32c(inode.csum_seed(self.csum_seed), &b[..tail]) != le32(&b, tail) {
                return Err(Error::Corrupt("extent block checksum"));
            }
        }
        Ok(b)
    }

    fn check_extent(&self, start: u64, len: u64) -> Result<()> {
        if len == 0 || start < self.sb.first_data_block as u64 || start.checked_add(len).is_none_or(|e| e > self.sb.blocks_count) {
            return Err(Error::Corrupt("extent outside the file system"));
        }
        Ok(())
    }

    fn map_extents(&mut self, inode: &Inode, lblk: u64) -> Result<Run> {
        if lblk > u32::MAX as u64 {
            return Ok(Run { phys: None, len: u64::MAX / 2, uninit: false });
        }
        let lblk32 = lblk as u32;
        let mut node = inode.block.to_vec();
        let mut h = parse_header(&node, 60)?;
        let mut hole_end = u64::MAX / 2;
        loop {
            if h.depth == 0 {
                let mut found = None;
                for i in 0..h.entries as usize {
                    let (start, len, phys, uninit) = leaf_entry(&node, i);
                    if start > lblk32 {
                        hole_end = hole_end.min(start as u64);
                        break;
                    }
                    found = Some((start, len, phys, uninit));
                }
                return match found {
                    Some((start, len, phys, uninit)) if (lblk32 as u64) < start as u64 + len as u64 => {
                        self.check_extent(phys, len as u64)?;
                        let skip = (lblk32 - start) as u64;
                        Ok(Run { phys: Some(phys + skip), len: len as u64 - skip, uninit })
                    }
                    _ => Ok(Run { phys: None, len: hole_end - lblk, uninit: false }),
                };
            }
            let mut child = None;
            for i in 0..h.entries as usize {
                let (start, blk) = index_entry(&node, i);
                if start > lblk32 {
                    hole_end = hole_end.min(start as u64);
                    break;
                }
                child = Some(blk);
            }
            let Some(blk) = child else {
                return Ok(Run { phys: None, len: hole_end - lblk, uninit: false });
            };
            self.check_extent(blk, 1)?;
            node = self.extent_block(inode, blk, h.depth - 1)?;
            h = parse_header(&node, node.len())?;
        }
    }

    fn map_indirect(&mut self, inode: &Inode, lblk: u64) -> Result<Run> {
        let apb = self.sb.block_size as u64 / 4;
        let hole = Run { phys: None, len: 1, uninit: false };
        let ptr = |b: &[u8], i: u64| le32(b, i as usize * 4) as u64;
        let mut path: [u64; 4] = [0; 4];
        let (top, depth) = if lblk < 12 {
            (ptr(&inode.block, lblk), 0)
        } else if lblk - 12 < apb {
            path[0] = lblk - 12;
            (ptr(&inode.block, 12), 1)
        } else if lblk - 12 - apb < apb * apb {
            let r = lblk - 12 - apb;
            path[0] = r / apb;
            path[1] = r % apb;
            (ptr(&inode.block, 13), 2)
        } else if lblk - 12 - apb - apb * apb < apb * apb * apb {
            let r = lblk - 12 - apb - apb * apb;
            path[0] = r / (apb * apb);
            path[1] = r / apb % apb;
            path[2] = r % apb;
            (ptr(&inode.block, 14), 3)
        } else {
            return Ok(hole);
        };
        let mut b = top;
        for &idx in path.iter().take(depth) {
            if b == 0 {
                return Ok(hole);
            }
            self.check_extent(b, 1)?;
            let data = self.read_block(b)?;
            b = ptr(&data, idx);
        }
        if b == 0 {
            return Ok(hole);
        }
        self.check_extent(b, 1)?;
        Ok(Run { phys: Some(b), len: 1, uninit: false })
    }

    pub(crate) fn map(&mut self, inode: &Inode, lblk: u64) -> Result<Run> {
        if inode.uses_extents() {
            self.map_extents(inode, lblk)
        } else {
            self.map_indirect(inode, lblk)
        }
    }

    pub(crate) fn read_inode_data(&mut self, inode: &Inode, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= inode.size {
            return Ok(0);
        }
        let bs = self.sb.block_size as u64;
        let len = (buf.len() as u64).min(inode.size - offset) as usize;
        let mut done = 0usize;
        let mut scratch = vec![0u8; bs as usize];
        while done < len {
            let pos = offset + done as u64;
            let lblk = pos / bs;
            let within = (pos % bs) as usize;
            let run = self.map(inode, lblk)?;
            let run_bytes = (run.len.saturating_mul(bs) - within as u64).min((len - done) as u64) as usize;
            match run.phys {
                Some(phys) if !run.uninit => {
                    if within == 0 && run_bytes >= bs as usize {
                        let blocks = (run_bytes / bs as usize).min(256);
                        let n = blocks * bs as usize;
                        self.read_blocks_raw(phys, &mut buf[done..done + n])?;
                        done += n;
                    } else {
                        self.read_blocks_raw(phys, &mut scratch)?;
                        let n = (bs as usize - within).min(len - done);
                        buf[done..done + n].copy_from_slice(&scratch[within..within + n]);
                        done += n;
                    }
                }
                _ => {
                    let n = run_bytes.max(1).min(len - done);
                    buf[done..done + n].fill(0);
                    done += n;
                }
            }
        }
        Ok(len)
    }
}
