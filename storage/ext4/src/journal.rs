//! The JBD2 journal: recovery and synchronous transactions.
//!
//! On-disk structures are big-endian. A transaction is a descriptor block
//! listing target block numbers, the block copies, and a commit block.
//! Revoke blocks cancel replays of earlier copies of a block.
//!
//! * Recovery ([`Ext4::recover_journal`]) scans from `s_start`, collects
//!   revocations, then replays every committed transaction. A read-only
//!   mount replays into an in-memory overlay and leaves the device alone;
//!   a writable mount writes the blocks in place and empties the journal.
//! * Writing ([`Ext4::commit`]): every metadata change of one VFS operation
//!   is one transaction. It is written to the log and committed, the
//!   journal superblock is pointed at it, the blocks are then written in
//!   place (checkpoint), and the journal is marked empty again. A crash at
//!   any point leaves either the old state or a committed transaction that
//!   recovery (this driver's or e2fsck's) replays.

use crate::inode::Inode;
use crate::{crc, feature, BlockDevice, Error, Ext4, Result};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

const JBD2_MAGIC: u32 = 0xC03B_3998;
const BT_DESCRIPTOR: u32 = 1;
const BT_COMMIT: u32 = 2;
const BT_SB_V1: u32 = 3;
const BT_SB_V2: u32 = 4;
const BT_REVOKE: u32 = 5;

const INCOMPAT_REVOKE: u32 = 0x1;
const INCOMPAT_64BIT: u32 = 0x2;
const INCOMPAT_ASYNC_COMMIT: u32 = 0x4;
const INCOMPAT_CSUM_V2: u32 = 0x8;
const INCOMPAT_CSUM_V3: u32 = 0x10;
const INCOMPAT_FAST_COMMIT: u32 = 0x20;
const KNOWN_INCOMPAT: u32 = INCOMPAT_REVOKE | INCOMPAT_64BIT | INCOMPAT_ASYNC_COMMIT | INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3;

const TAG_ESCAPE: u32 = 1;
const TAG_SAME_UUID: u32 = 2;
const TAG_LAST: u32 = 8;

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put_be32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

/// Journal state of a mounted volume.
#[allow(dead_code)] // uuid is used by the writer
pub struct Journal {
    /// Physical block of every journal block (index = journal block number).
    pub(crate) blocks: Vec<u64>,
    pub(crate) first: u32,
    pub(crate) sequence: u32,
    pub(crate) incompat: u32,
    pub(crate) csum_seed: u32,
    pub(crate) uuid: [u8; 16],
    /// Replayed blocks of a read-only mount (never written to the device).
    pub(crate) overlay: BTreeMap<u64, Vec<u8>>,
    /// Transactions committed through this driver.
    pub committed: u64,
}

impl Journal {
    fn csum_v23(&self) -> bool {
        self.incompat & (INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3) != 0
    }

    fn tag_size(&self) -> usize {
        let wide = self.incompat & INCOMPAT_64BIT != 0;
        if self.incompat & INCOMPAT_CSUM_V3 != 0 {
            if wide { 16 } else { 12 }
        } else if wide {
            12
        } else {
            8
        }
    }

    fn len(&self) -> u32 {
        self.blocks.len() as u32
    }

    fn next(&self, b: u32) -> u32 {
        if b + 1 >= self.len() {
            self.first
        } else {
            b + 1
        }
    }
}

/// One committed transaction found by the scan.
struct Txn {
    seq: u32,
    /// (target block, journal block of the copy, escaped)
    writes: Vec<(u64, u32, bool)>,
}

impl<D: BlockDevice> Ext4<D> {
    /// Loads the journal superblock and the block list of the journal inode.
    fn open_journal(&mut self) -> Result<Journal> {
        let inode: Inode = self.inode(self.sb.journal_inum)?;
        let bs = self.sb.block_size as u64;
        let count = inode.size / bs;
        if !(16..=1 << 24).contains(&count) {
            return Err(Error::Corrupt("journal size"));
        }
        let mut blocks = Vec::with_capacity(count as usize);
        let mut l = 0;
        while l < count {
            let run = self.map(&inode, l)?;
            let phys = run.phys.ok_or(Error::Corrupt("hole in the journal"))?;
            for i in 0..run.len.min(count - l) {
                blocks.push(phys + i);
            }
            l += run.len.min(count - l);
        }
        let jsb = self.read_block(blocks[0])?;
        if be32(&jsb, 0) != JBD2_MAGIC || !matches!(be32(&jsb, 4), BT_SB_V1 | BT_SB_V2) {
            return Err(Error::Corrupt("journal superblock magic"));
        }
        if be32(&jsb, 12) != self.sb.block_size {
            return Err(Error::Corrupt("journal block size"));
        }
        let maxlen = be32(&jsb, 16);
        let first = be32(&jsb, 20);
        if maxlen as u64 > count || first == 0 || first >= maxlen {
            return Err(Error::Corrupt("journal geometry"));
        }
        blocks.truncate(maxlen as usize);
        let v2 = be32(&jsb, 4) == BT_SB_V2;
        let incompat = if v2 { be32(&jsb, 40) } else { 0 };
        if incompat & !KNOWN_INCOMPAT != 0 {
            return Err(Error::Unsupported(if incompat & INCOMPAT_FAST_COMMIT != 0 {
                "journal fast commits"
            } else {
                "unknown journal feature"
            }));
        }
        let uuid: [u8; 16] = jsb[48..64].try_into().expect("16 bytes");
        let j = Journal {
            blocks,
            first,
            sequence: be32(&jsb, 24),
            incompat,
            csum_seed: crc::crc32c(!0, &uuid),
            uuid,
            overlay: BTreeMap::new(),
            committed: 0,
        };
        if j.csum_v23() {
            let mut copy = jsb.clone();
            copy[0xFC..0x100].fill(0);
            if crc::crc32c(!0, &copy[..1024]) != be32(&jsb, 0xFC) {
                return Err(Error::Corrupt("journal superblock checksum"));
            }
        }
        Ok(j)
    }

    fn journal_block(&mut self, j: &Journal, n: u32) -> Result<Vec<u8>> {
        let phys = *j.blocks.get(n as usize).ok_or(Error::Corrupt("journal block out of range"))?;
        self.read_block(phys)
    }

    fn block_tail_ok(&self, j: &Journal, b: &[u8]) -> bool {
        if !j.csum_v23() {
            return true;
        }
        let o = b.len() - 4;
        let mut copy = b.to_vec();
        copy[o..].fill(0);
        crc::crc32c(j.csum_seed, &copy) == be32(b, o)
    }

    /// Parses the tags of a descriptor block: (target, flags).
    fn tags(&self, j: &Journal, b: &[u8]) -> Result<Vec<(u64, u32)>> {
        let mut out = Vec::new();
        let ts = j.tag_size();
        let end = if j.csum_v23() { b.len() - 4 } else { b.len() };
        let mut off = 12;
        loop {
            if off + ts > end {
                return Err(Error::Corrupt("journal descriptor overruns its block"));
            }
            let lo = be32(b, off) as u64;
            let (flags, hi) = if j.incompat & INCOMPAT_CSUM_V3 != 0 {
                (be32(b, off + 4), if j.incompat & INCOMPAT_64BIT != 0 { be32(b, off + 8) as u64 } else { 0 })
            } else {
                (u16::from_be_bytes([b[off + 6], b[off + 7]]) as u32, if j.incompat & INCOMPAT_64BIT != 0 { be32(b, off + 8) as u64 } else { 0 })
            };
            out.push((lo | hi << 32, flags));
            off += ts;
            if flags & TAG_SAME_UUID == 0 {
                off += 16;
            }
            if flags & TAG_LAST != 0 {
                return Ok(out);
            }
        }
    }

    fn commit_ok(&self, j: &Journal, b: &[u8]) -> bool {
        if !j.csum_v23() {
            return true;
        }
        let mut copy = b.to_vec();
        copy[16..20].fill(0);
        crc::crc32c(j.csum_seed, &copy) == be32(b, 16)
    }

    /// Scans the log: committed transactions and revocations.
    fn scan(&mut self, j: &Journal, start: u32) -> Result<(Vec<Txn>, BTreeMap<u64, u32>)> {
        let mut txns = Vec::new();
        let mut revoked: BTreeMap<u64, u32> = BTreeMap::new();
        let mut seq = j.sequence;
        let mut pos = start;
        let mut current: Option<Txn> = None;
        let mut steps = 0u32;
        loop {
            steps += 1;
            if steps > j.len() * 2 {
                return Err(Error::Corrupt("journal does not terminate"));
            }
            let b = self.journal_block(j, pos)?;
            if be32(&b, 0) != JBD2_MAGIC || be32(&b, 8) != seq {
                break;
            }
            match be32(&b, 4) {
                BT_DESCRIPTOR => {
                    if !self.block_tail_ok(j, &b) {
                        break;
                    }
                    let tags = self.tags(j, &b)?;
                    let t = current.get_or_insert(Txn { seq, writes: Vec::new() });
                    for (target, flags) in tags {
                        pos = j.next(pos);
                        t.writes.push((target, pos, flags & TAG_ESCAPE != 0));
                    }
                }
                BT_REVOKE => {
                    if !self.block_tail_ok(j, &b) {
                        break;
                    }
                    let used = be32(&b, 12) as usize;
                    let rs = if j.incompat & INCOMPAT_64BIT != 0 { 8 } else { 4 };
                    if used < 16 || used > b.len() {
                        return Err(Error::Corrupt("journal revoke block"));
                    }
                    let mut o = 16;
                    while o + rs <= used {
                        let blk = if rs == 8 { (be32(&b, o) as u64) << 32 | be32(&b, o + 4) as u64 } else { be32(&b, o) as u64 };
                        let e = revoked.entry(blk).or_insert(seq);
                        *e = (*e).max(seq);
                        o += rs;
                    }
                    current.get_or_insert(Txn { seq, writes: Vec::new() });
                }
                BT_COMMIT => {
                    if !self.commit_ok(j, &b) {
                        break;
                    }
                    txns.push(current.take().unwrap_or(Txn { seq, writes: Vec::new() }));
                    seq = seq.wrapping_add(1);
                }
                _ => break,
            }
            pos = j.next(pos);
        }
        Ok((txns, revoked))
    }

    /// Replays the journal if the superblock asks for recovery.
    pub(crate) fn recover_journal(&mut self, read_only: bool) -> Result<()> {
        if !self.sb.has_compat(feature::COMPAT_HAS_JOURNAL) || self.sb.journal_inum == 0 {
            return Err(Error::Unsupported("recovery flag without an internal journal"));
        }
        let mut j = self.open_journal()?;
        let jsb = self.journal_block(&j, 0)?;
        let start = be32(&jsb, 28);
        let mut replayed = BTreeMap::new();
        let mut last_seq = j.sequence;
        if start != 0 {
            if start < j.first || start >= j.len() {
                return Err(Error::Corrupt("journal start"));
            }
            let (txns, revoked) = self.scan(&j, start)?;
            for t in &txns {
                for &(target, jblk, escaped) in &t.writes {
                    if revoked.get(&target).is_some_and(|&r| r >= t.seq) {
                        continue;
                    }
                    if target >= self.sb.blocks_count {
                        return Err(Error::Corrupt("journal block target outside the file system"));
                    }
                    let mut data = self.journal_block(&j, jblk)?;
                    if escaped {
                        put_be32(&mut data, 0, JBD2_MAGIC);
                    }
                    replayed.insert(target, data);
                }
                last_seq = t.seq.wrapping_add(1);
            }
        }
        // Blocks read before recovery may be stale.
        self.clear_cache();
        self.recovered_blocks = replayed.len();
        if read_only {
            j.overlay = replayed;
            // Replayed metadata may include group descriptors and the
            // superblock: reload them from the overlay.
            self.journal = Some(j);
            self.reload_after_replay()?;
        } else {
            for (target, data) in replayed {
                self.write_block_raw(target, &data)?;
            }
            self.dev.flush()?;
            j.sequence = last_seq;
            self.journal = Some(j);
            self.write_journal_super(0)?;
            self.reload_after_replay()?;
            let incompat = self.sb.incompat & !feature::INCOMPAT_RECOVER;
            self.set_incompat(incompat)?;
        }
        Ok(())
    }

    fn reload_after_replay(&mut self) -> Result<()> {
        let blk = self.sb.first_data_block as u64; // block holding the superblock
        let b = self.read_block(blk)?;
        let off = if self.sb.block_size == 1024 { 0 } else { 1024 };
        let mut raw = [0u8; 1024];
        raw.copy_from_slice(&b[off..off + 1024]);
        let mut sb = crate::Superblock::parse(&raw)?;
        // Keep the recovery flag of the device's copy: the overlay is not on disk.
        sb.incompat |= self.sb.incompat & feature::INCOMPAT_RECOVER;
        self.sb = sb;
        // `load_groups` reads through `read_block`, which consults the overlay.
        self.load_groups()
    }

    /// Writes the journal superblock with log start `start` (0 = empty).
    pub(crate) fn write_journal_super(&mut self, start: u32) -> Result<()> {
        let j = self.journal.as_ref().ok_or(Error::Corrupt("no journal"))?;
        let phys = j.blocks[0];
        let seq = j.sequence;
        let v23 = j.csum_v23();
        let mut b = self.read_block(phys)?;
        put_be32(&mut b, 24, seq);
        put_be32(&mut b, 28, start);
        if v23 {
            b[0xFC..0x100].fill(0);
            let c = crc::crc32c(!0, &b[..1024]);
            put_be32(&mut b, 0xFC, c);
        }
        self.write_block_raw(phys, &b)?;
        self.dev.flush()
    }

    /// Writes `blocks` (metadata block number -> new contents) as one
    /// transaction and checkpoints it. Volumes without a journal get the
    /// blocks written in place directly.
    pub(crate) fn commit(&mut self, blocks: &BTreeMap<u64, Vec<u8>>) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        if self.journal.is_none() {
            for (&b, d) in blocks {
                self.write_block_raw(b, d)?;
            }
            return self.dev.flush();
        }
        let bs = self.sb.block_size as usize;
        let (seq, first, tag_size, v23, v3, wide, room, jlen) = {
            let j = self.journal.as_ref().expect("checked");
            let end = if j.csum_v23() { bs - 4 } else { bs };
            (j.sequence, j.first, j.tag_size(), j.csum_v23(), j.incompat & INCOMPAT_CSUM_V3 != 0, j.incompat & INCOMPAT_64BIT != 0, end, j.len())
        };
        // Lay out: descriptor, copies (as many as fit per descriptor), ..., commit.
        let list: Vec<(&u64, &Vec<u8>)> = blocks.iter().collect();
        let per_desc = (room - 12 - 16) / tag_size;
        let needed = list.len() + list.len().div_ceil(per_desc) + 1;
        if needed as u32 > jlen - first {
            return Err(Error::NoSpace);
        }
        let mut log: Vec<Vec<u8>> = Vec::with_capacity(needed);
        let (j_seed, uuid) = {
            let j = self.journal.as_ref().expect("checked");
            (j.csum_seed, j.uuid)
        };
        for chunk in list.chunks(per_desc) {
            let mut d = vec![0u8; bs];
            put_be32(&mut d, 0, JBD2_MAGIC);
            put_be32(&mut d, 4, BT_DESCRIPTOR);
            put_be32(&mut d, 8, seq);
            let mut off = 12;
            let mut copies = Vec::with_capacity(chunk.len());
            for (i, (&target, data)) in chunk.iter().enumerate() {
                let mut copy = (*data).clone();
                let mut flags = 0u32;
                if be32(&copy, 0) == JBD2_MAGIC {
                    copy[0..4].fill(0);
                    flags |= TAG_ESCAPE;
                }
                if i > 0 {
                    flags |= TAG_SAME_UUID;
                }
                if i + 1 == chunk.len() {
                    flags |= TAG_LAST;
                }
                put_be32(&mut d, off, target as u32);
                if v3 {
                    put_be32(&mut d, off + 4, flags);
                    if wide {
                        put_be32(&mut d, off + 8, (target >> 32) as u32);
                    }
                    let mut c = crc::crc32c(j_seed, &seq.to_be_bytes());
                    c = crc::crc32c(c, data);
                    put_be32(&mut d, off + tag_size - 4, c);
                } else {
                    if v23 {
                        let mut c = crc::crc32c(j_seed, &seq.to_be_bytes());
                        c = crc::crc32c(c, data);
                        d[off + 4..off + 6].copy_from_slice(&(c as u16).to_be_bytes());
                    }
                    d[off + 6..off + 8].copy_from_slice(&(flags as u16).to_be_bytes());
                    if wide {
                        put_be32(&mut d, off + 8, (target >> 32) as u32);
                    }
                }
                off += tag_size;
                if i == 0 {
                    d[off..off + 16].copy_from_slice(&uuid);
                    off += 16;
                }
                copies.push(copy);
            }
            if v23 {
                let c = crc::crc32c(j_seed, &d);
                put_be32(&mut d, bs - 4, c);
            }
            log.push(d);
            log.extend(copies);
        }
        let mut c = vec![0u8; bs];
        put_be32(&mut c, 0, JBD2_MAGIC);
        put_be32(&mut c, 4, BT_COMMIT);
        put_be32(&mut c, 8, seq);
        let (secs, nsecs) = (self.clock)();
        c[0x30..0x38].copy_from_slice(&(secs as u64).to_be_bytes());
        put_be32(&mut c, 0x38, nsecs);
        if v23 {
            // Checksum type and size stay 0 for v2/v3 commit blocks.
            let k = crc::crc32c(j_seed, &c);
            put_be32(&mut c, 16, k);
        }
        log.push(c);
        let phys: Vec<u64> = {
            let j = self.journal.as_ref().expect("checked");
            (0..log.len()).map(|i| j.blocks[first as usize + i]).collect()
        };
        for (p, data) in phys.iter().zip(&log) {
            self.write_block_raw(*p, data)?;
        }
        self.dev.flush()?;
        self.write_journal_super(first)?;
        // Checkpoint.
        for (&b, d) in blocks {
            self.write_block_raw(b, d)?;
        }
        self.dev.flush()?;
        {
            let j = self.journal.as_mut().expect("checked");
            j.sequence = j.sequence.wrapping_add(1);
            j.committed += 1;
        }
        self.write_journal_super(0)
    }

    /// Prepares the journal for writing (after any recovery).
    pub(crate) fn open_journal_for_writing(&mut self) -> Result<()> {
        if !self.sb.has_compat(feature::COMPAT_HAS_JOURNAL) {
            return Ok(());
        }
        if self.journal.is_none() {
            let j = self.open_journal()?;
            self.journal = Some(j);
        }
        let start = {
            let j = self.journal.as_ref().expect("just set");
            let phys = j.blocks[0];
            be32(&self.read_block(phys)?, 28)
        };
        if start != 0 {
            return Err(Error::Corrupt("journal is not empty after recovery"));
        }
        Ok(())
    }

    /// Blocks of the journal that a transaction of `n` metadata blocks needs.
    pub fn journal_capacity(&self) -> Option<u32> {
        self.journal.as_ref().map(|j| j.len() - j.first)
    }

    pub fn journal_commits(&self) -> u64 {
        self.journal.as_ref().map_or(0, |j| j.committed)
    }

}
