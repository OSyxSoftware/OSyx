//! Hashed directories (`dir_index`): the name hashes (legacy, half-MD4 and
//! TEA, signed and unsigned), lookups through the index, adding entries
//! with leaf and index node splits, and turning a full one-block directory
//! into an indexed one, as Linux does.
//!
//! Layout (ext4 disk format, "Hash Tree Directories"): block 0 holds `.`,
//! `..` (whose record covers the rest of the block), an 8-byte root info
//! and the root's `dx_entry` array; index nodes start with an empty 8-byte
//! dirent. Each array starts with (limit, count, block of the first
//! child); the following entries are (hash, block), sorted, where a hash
//! with bit 0 set continues the previous child's hash (a collision split
//! across leaves). With `metadata_csum` an 8-byte `dx_tail` follows the
//! `limit` entries.

use super::rec_size;
use crate::dir::{entries, has_tail};
use crate::inode::{Inode, FL_INDEX};
use crate::{crc, feature, le16, le32, put16, put32, BlockDevice, Error, Ext4, Result};
use alloc::vec;
use alloc::vec::Vec;

pub(crate) const HASH_LEGACY: u8 = 0;
pub(crate) const HASH_HALF_MD4: u8 = 1;
pub(crate) const HASH_TEA: u8 = 2;
/// Added to the three above when the volume hashes unsigned chars.
const UNSIGNED: u8 = 3;
const LEGACY_U: u8 = HASH_LEGACY + UNSIGNED;
const HALF_MD4_U: u8 = HASH_HALF_MD4 + UNSIGNED;
const TEA_U: u8 = HASH_TEA + UNSIGNED;
/// `s_flags` bit: names hash as unsigned chars.
const FLAG_UNSIGNED_HASH: u32 = 2;
/// Offsets in the superblock.
const SB_HASH_SEED: usize = 0xEC;
const SB_DEF_HASH_VERSION: usize = 0xFC;
const SB_FLAGS: usize = 0x160;
/// Offset of the (limit, count) pair in the root and in index nodes.
const ROOT_COUNT: usize = 32;
const NODE_COUNT: usize = 8;
/// The largest hash; a hash equal to it is moved one step down.
const EOF_32: u32 = 0x7FFF_FFFF;

/// Packs name bytes into words the way the kernel's `str2hashbuf` does.
fn str2hashbuf(msg: &[u8], unsigned: bool, num: usize) -> Vec<u32> {
    let len = msg.len() as u32;
    let mut pad = len | (len << 8);
    pad |= pad << 16;
    let mut val = pad;
    let mut out = Vec::with_capacity(num);
    let take = msg.len().min(num * 4);
    for (i, &c) in msg[..take].iter().enumerate() {
        let c = if unsigned { c as u32 } else { c as i8 as i32 as u32 };
        val = c.wrapping_add(val << 8);
        if i % 4 == 3 {
            out.push(val);
            val = pad;
        }
    }
    if out.len() < num {
        out.push(val);
    }
    while out.len() < num {
        out.push(pad);
    }
    out
}

fn half_md4(buf: &mut [u32; 4], x: &[u32]) {
    const K2: u32 = 0o13240474631;
    const K3: u32 = 0o15666365641;
    let f = |x: u32, y: u32, z: u32| z ^ (x & (y ^ z));
    let g = |x: u32, y: u32, z: u32| (x & y).wrapping_add((x ^ y) & z);
    let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
    let (mut a, mut b, mut c, mut d) = (buf[0], buf[1], buf[2], buf[3]);
    macro_rules! round {
        ($fun:expr, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $s:expr) => {
            $a = $a.wrapping_add($fun($b, $c, $d)).wrapping_add($x).rotate_left($s);
        };
    }
    round!(f, a, b, c, d, x[0], 3);
    round!(f, d, a, b, c, x[1], 7);
    round!(f, c, d, a, b, x[2], 11);
    round!(f, b, c, d, a, x[3], 19);
    round!(f, a, b, c, d, x[4], 3);
    round!(f, d, a, b, c, x[5], 7);
    round!(f, c, d, a, b, x[6], 11);
    round!(f, b, c, d, a, x[7], 19);
    round!(g, a, b, c, d, x[1].wrapping_add(K2), 3);
    round!(g, d, a, b, c, x[3].wrapping_add(K2), 5);
    round!(g, c, d, a, b, x[5].wrapping_add(K2), 9);
    round!(g, b, c, d, a, x[7].wrapping_add(K2), 13);
    round!(g, a, b, c, d, x[0].wrapping_add(K2), 3);
    round!(g, d, a, b, c, x[2].wrapping_add(K2), 5);
    round!(g, c, d, a, b, x[4].wrapping_add(K2), 9);
    round!(g, b, c, d, a, x[6].wrapping_add(K2), 13);
    round!(h, a, b, c, d, x[3].wrapping_add(K3), 3);
    round!(h, d, a, b, c, x[7].wrapping_add(K3), 9);
    round!(h, c, d, a, b, x[2].wrapping_add(K3), 11);
    round!(h, b, c, d, a, x[6].wrapping_add(K3), 15);
    round!(h, a, b, c, d, x[1].wrapping_add(K3), 3);
    round!(h, d, a, b, c, x[5].wrapping_add(K3), 9);
    round!(h, c, d, a, b, x[0].wrapping_add(K3), 11);
    round!(h, b, c, d, a, x[4].wrapping_add(K3), 15);
    buf[0] = buf[0].wrapping_add(a);
    buf[1] = buf[1].wrapping_add(b);
    buf[2] = buf[2].wrapping_add(c);
    buf[3] = buf[3].wrapping_add(d);
}

fn tea(buf: &mut [u32; 4], x: &[u32]) {
    const DELTA: u32 = 0x9E37_79B9;
    let (mut b0, mut b1) = (buf[0], buf[1]);
    let (a, b, c, d) = (x[0], x[1], x[2], x[3]);
    let mut sum = 0u32;
    for _ in 0..16 {
        sum = sum.wrapping_add(DELTA);
        b0 = b0.wrapping_add((b1 << 4).wrapping_add(a) ^ b1.wrapping_add(sum) ^ (b1 >> 5).wrapping_add(b));
        b1 = b1.wrapping_add((b0 << 4).wrapping_add(c) ^ b0.wrapping_add(sum) ^ (b0 >> 5).wrapping_add(d));
    }
    buf[0] = buf[0].wrapping_add(b0);
    buf[1] = buf[1].wrapping_add(b1);
}

fn legacy(name: &[u8], unsigned: bool) -> u32 {
    let (mut h0, mut h1) = (0x12A3_FE2Du32, 0x37AB_E8F9u32);
    for &c in name {
        let c = if unsigned { c as u32 } else { c as i8 as i32 as u32 };
        let mut h = h1.wrapping_add(h0 ^ c.wrapping_mul(7_152_373));
        if h & 0x8000_0000 != 0 {
            h = h.wrapping_sub(0x7FFF_FFFF);
        }
        h1 = h0;
        h0 = h;
    }
    h0 << 1
}

/// The (major, minor) hash of `name` (`ext4fs_dirhash`). `version`
/// includes the unsigned offset; `None` for versions OSyx does not know.
pub(crate) fn dirhash(name: &[u8], version: u8, seed: &[u32; 4]) -> Option<(u32, u32)> {
    let mut buf =
        if seed.iter().any(|&s| s != 0) { *seed } else { [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476] };
    let (hash, minor) = match version {
        HASH_LEGACY | LEGACY_U => (legacy(name, version == LEGACY_U), 0),
        HASH_HALF_MD4 | HALF_MD4_U => {
            let mut p = name;
            loop {
                half_md4(&mut buf, &str2hashbuf(p, version == HALF_MD4_U, 8));
                if p.len() <= 32 {
                    break;
                }
                p = &p[32..];
            }
            (buf[1], buf[2])
        }
        HASH_TEA | TEA_U => {
            let mut p = name;
            loop {
                tea(&mut buf, &str2hashbuf(p, version == TEA_U, 4));
                if p.len() <= 16 {
                    break;
                }
                p = &p[16..];
            }
            (buf[0], buf[1])
        }
        _ => return None,
    };
    let mut hash = hash & !1;
    if hash == EOF_32 << 1 {
        hash = (EOF_32 - 1) << 1;
    }
    Some((hash, minor))
}

/// One index block on the way down.
#[derive(Clone, Debug)]
struct Level {
    lblk: u64,
    /// Offset of (limit, count).
    count_off: usize,
    /// (hash, child logical block); the first hash is implied by the parent.
    entries: Vec<(u32, u32)>,
    /// The entry followed.
    at: usize,
}

/// A parsed index and the path to one leaf.
struct Path {
    version: u8,
    levels: Vec<Level>,
}

fn limit_for(bs: usize, count_off: usize, csum: bool) -> usize {
    (bs - count_off) / 8 - csum as usize
}

fn read_entries(b: &[u8], count_off: usize) -> Option<Vec<(u32, u32)>> {
    let limit = le16(b, count_off) as usize;
    let count = le16(b, count_off + 2) as usize;
    if count == 0 || count > limit || count_off + limit * 8 > b.len() {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    out.push((0, le32(b, count_off + 4)));
    for i in 1..count {
        out.push((le32(b, count_off + 8 * i), le32(b, count_off + 8 * i + 4)));
    }
    Some(out)
}

impl<D: BlockDevice> Ext4<D> {
    fn hash_seed(&self) -> [u32; 4] {
        let r = &self.sb.raw;
        core::array::from_fn(|i| le32(r, SB_HASH_SEED + 4 * i))
    }

    /// The hash version for a root's stored version.
    fn effective_version(&self, stored: u8) -> u8 {
        if stored <= HASH_TEA && le32(&self.sb.raw, SB_FLAGS) & FLAG_UNSIGNED_HASH != 0 {
            stored + UNSIGNED
        } else {
            stored
        }
    }

    /// Whether new hashed directories can use the volume's default hash.
    pub(crate) fn default_hash_known(&self) -> bool {
        let version = self.effective_version(self.sb.raw[SB_DEF_HASH_VERSION]);
        dirhash(b"", version, &[0; 4]).is_some()
    }

    /// Whether `dir` is a usable hashed directory.
    pub(crate) fn is_indexed(&self, dir: &Inode) -> bool {
        dir.flags & FL_INDEX != 0 && self.sb.has_compat(feature::COMPAT_DIR_INDEX)
    }

    /// Follows the index of `dir` towards `hash`; `None` when the index
    /// cannot be used (damaged, or a hash OSyx does not implement).
    fn dx_probe(&mut self, dir: &Inode, hash_of: &[u8]) -> Result<Option<(Path, u32)>> {
        let bs = self.sb.block_size as usize;
        let (_, root) = self.dir_block(dir, 0)?;
        // ".", then ".." covering the rest of the block, then the info.
        if le16(&root, 4) != 12
            || root[6] != 1
            || root[8] != b'.'
            || le16(&root, 16) as usize != bs - 12
            || root[29] != 8
        {
            return Ok(None);
        }
        let stored = root[28];
        let levels_below = root[30] as usize;
        let max_levels = if self.sb.has_incompat(feature::INCOMPAT_LARGEDIR) { 2 } else { 1 };
        if levels_below > max_levels {
            return Ok(None);
        }
        let version = self.effective_version(stored);
        let Some((hash, _)) = dirhash(hash_of, version, &self.hash_seed()) else { return Ok(None) };
        let mut levels = Vec::new();
        let mut lblk = 0u64;
        let mut count_off = ROOT_COUNT;
        let mut block = root;
        let nblocks = self.dir_blocks(dir)?;
        for depth in 0..=levels_below {
            let Some(list) = read_entries(&block, count_off) else { return Ok(None) };
            let at = list.iter().rposition(|&(h, _)| h <= hash).unwrap_or(0);
            let child = list[at].1 as u64;
            if child >= nblocks || child == 0 {
                return Ok(None);
            }
            levels.push(Level { lblk, count_off, entries: list, at });
            if depth == levels_below {
                break;
            }
            let (_, b) = self.dir_block(dir, child)?;
            // An index node: an empty dirent covering the block.
            if le32(&b, 0) != 0 || le16(&b, 4) as usize != bs || b[6] != 0 {
                return Ok(None);
            }
            lblk = child;
            count_off = NODE_COUNT;
            block = b;
        }
        Ok(Some((Path { version, levels }, hash)))
    }

    /// Leaves that may hold `name`: the one the index points at and those
    /// continuing its hash. `None`: search linearly.
    pub(crate) fn dx_leaves(&mut self, dir: &Inode, name: &[u8]) -> Result<Option<Vec<u64>>> {
        if !self.is_indexed(dir) {
            return Ok(None);
        }
        // "." and ".." live only in the root block, outside the index.
        if name == b"." || name == b".." {
            return Ok(Some(vec![0]));
        }
        let Some((path, hash)) = self.dx_probe(dir, name)? else { return Ok(None) };
        let bottom = path.levels.last().unwrap();
        let mut out = vec![bottom.entries[bottom.at].1 as u64];
        // Collisions continue in the next entries (within this node; a
        // collision run crossing nodes is searched through the parent).
        let mut i = bottom.at + 1;
        loop {
            match bottom.entries.get(i) {
                Some(&(h, b)) if h & 1 == 1 && h & !1 == hash => out.push(b as u64),
                Some(_) => break,
                None => {
                    if path.levels.len() < 2 {
                        break;
                    }
                    // The parent's next child starts the next node.
                    let parent = &path.levels[path.levels.len() - 2];
                    match parent.entries.get(parent.at + 1) {
                        Some(&(h, node)) if h & 1 == 1 && h & !1 == hash => {
                            let (_, nb) = self.dir_block(dir, node as u64)?;
                            if let Some(list) = read_entries(&nb, NODE_COUNT) {
                                out.extend(
                                    list.iter()
                                        .take_while(|(h2, _)| *h2 == 0 || (h2 & 1 == 1 && h2 & !1 == hash))
                                        .map(|&(_, b)| b as u64),
                                );
                            }
                        }
                        _ => {}
                    }
                    break;
                }
            }
            i += 1;
        }
        Ok(Some(out))
    }

    /// Writes an index block: `.`/`..`/info kept for the root, an empty
    /// dirent for a node, then the entries and the checksum tail.
    fn put_dx_block(
        &mut self,
        dir: &Inode,
        lblk: u64,
        count_off: usize,
        entries_: &[(u32, u32)],
        levels_below: Option<u8>,
    ) -> Result<()> {
        let bs = self.sb.block_size as usize;
        let csum = self.sb.metadata_csum();
        let limit = limit_for(bs, count_off, csum);
        if entries_.len() > limit {
            return Err(Error::Corrupt("directory index node overfilled"));
        }
        let (phys, _) = self.dir_block(dir, lblk)?;
        let mut b = if count_off == ROOT_COUNT { self.tx_read(phys)? } else { vec![0u8; bs] };
        if count_off == NODE_COUNT {
            put32(&mut b, 0, 0);
            put16(&mut b, 4, bs as u16);
            b[6] = 0;
            b[7] = 0;
        }
        if let Some(l) = levels_below {
            b[30] = l;
        }
        b[count_off..].fill(0);
        put16(&mut b, count_off, limit as u16);
        put16(&mut b, count_off + 2, entries_.len() as u16);
        put32(&mut b, count_off + 4, entries_[0].1);
        for (i, &(h, blk)) in entries_.iter().enumerate().skip(1) {
            put32(&mut b, count_off + 8 * i, h);
            put32(&mut b, count_off + 8 * i + 4, blk);
        }
        self.seal_dx(dir, &mut b, count_off);
        self.tx_put(phys, b);
        Ok(())
    }

    /// The `dx_tail` checksum of an index block.
    pub(crate) fn seal_dx(&self, dir: &Inode, b: &mut [u8], count_off: usize) {
        if !self.sb.metadata_csum() {
            return;
        }
        let limit = le16(b, count_off) as usize;
        let count = le16(b, count_off + 2) as usize;
        let tail = count_off + limit * 8;
        if tail + 8 > b.len() {
            return;
        }
        let mut c = crc::crc32c(dir.csum_seed(self.csum_seed), &b[..count_off + count * 8]);
        c = crc::crc32c(c, &b[tail..tail + 4]);
        c = crc::crc32c(c, &[0; 4]);
        put32(b, tail + 4, c);
    }

    /// Adds `name` to the hashed directory `dir`.
    pub(crate) fn dx_add(&mut self, dir: &mut Inode, name: &[u8], ino: u32, ft: u8) -> Result<()> {
        let Some((mut path, hash)) = self.dx_probe(dir, name)? else {
            return Err(Error::Unsupported("a hashed directory with an index OSyx cannot use"));
        };
        let bottom = path.levels.len() - 1;
        let leaf = path.levels[bottom].entries[path.levels[bottom].at].1 as u64;
        if self.insert_in_leaf(dir, leaf, name, ino, ft)? {
            return Ok(());
        }
        // Split the leaf by hash, the new entry included.
        let filetype = self.filetype();
        let (phys, _) = self.dir_block(dir, leaf)?;
        let b = self.tx_read(phys)?;
        let seed = self.hash_seed();
        let mut all: Vec<(u32, u32, u32, Vec<u8>, u8)> = Vec::new();
        for e in entries(&b, filetype, self.sb.inodes_count)? {
            if e.ino != 0 {
                let (h, m) = dirhash(e.name, path.version, &seed).ok_or(Error::Unsupported("directory hash"))?;
                all.push((h, m, e.ino, e.name.to_vec(), e.file_type));
            }
        }
        let (_, minor) = dirhash(name, path.version, &seed).ok_or(Error::Unsupported("directory hash"))?;
        all.push((hash, minor, ino, name.to_vec(), if filetype { ft } else { 0 }));
        all.sort_by_key(|e| (e.0, e.1));
        let total: usize = all.iter().map(|e| rec_size(e.3.len())).sum();
        let mut moved = 0;
        let mut split = all.len();
        while split > 1 && moved < total / 2 {
            split -= 1;
            moved += rec_size(all[split].3.len());
        }
        let continued = all[split].0 == all[split - 1].0;
        let split_hash = all[split].0 | continued as u32;
        let lower: Vec<(u32, &[u8], u8)> = all[..split].iter().map(|e| (e.2, e.3.as_slice(), e.4)).collect();
        let upper: Vec<(u32, &[u8], u8)> = all[split..].iter().map(|e| (e.2, e.3.as_slice(), e.4)).collect();
        let mut lb = self.new_dir_block(&lower);
        self.seal_dir_block(dir, &mut lb);
        self.tx_put(phys, lb);
        let mut ub = self.new_dir_block(&upper);
        self.seal_dir_block(dir, &mut ub);
        let new_leaf = self.append_dir_block(dir, ub)?;
        self.dx_insert(dir, &mut path, bottom, split_hash, new_leaf as u32)
    }

    /// Puts `(hash, child)` after the followed entry of level `li`,
    /// splitting full index blocks.
    fn dx_insert(&mut self, dir: &mut Inode, path: &mut Path, li: usize, hash: u32, child: u32) -> Result<()> {
        let bs = self.sb.block_size as usize;
        let csum = self.sb.metadata_csum();
        let level = path.levels[li].clone();
        let limit = limit_for(bs, level.count_off, csum);
        let mut list = level.entries.clone();
        list.insert(level.at + 1, (hash, child));
        let levels_below = path.levels.len() as u8 - 1;
        if list.len() <= limit {
            let root_levels = (li == 0).then_some(levels_below);
            return self.put_dx_block(dir, level.lblk, level.count_off, &list, root_levels);
        }
        if li == 0 {
            // A full root.
            let max_levels = if self.sb.has_incompat(feature::INCOMPAT_LARGEDIR) { 2 } else { 1 };
            if levels_below as usize >= max_levels {
                return Err(Error::NoSpace);
            }
            if levels_below == 0 {
                // Push the root's entries down into a new node.
                let node = self.append_dir_block(dir, vec![0u8; bs])?;
                self.put_dx_block(dir, node, NODE_COUNT, &list, None)?;
                return self.put_dx_block(dir, 0, ROOT_COUNT, &[(0, node as u32)], Some(1));
            }
            return Err(Error::NoSpace);
        }
        // A full node: halves, the upper one in a new node under the parent.
        let half = list.len() / 2;
        let (lo, hi) = list.split_at(half);
        let up_hash = hi[0].0;
        let mut hi_list = hi.to_vec();
        hi_list[0].0 = 0;
        self.put_dx_block(dir, level.lblk, NODE_COUNT, lo, None)?;
        let node = self.append_dir_block(dir, vec![0u8; bs])?;
        self.put_dx_block(dir, node, NODE_COUNT, &hi_list, None)?;
        self.dx_insert(dir, path, li - 1, up_hash, node as u32)
    }

    /// Adds an entry to leaf `lblk` if it fits.
    fn insert_in_leaf(&mut self, dir: &Inode, lblk: u64, name: &[u8], ino: u32, ft: u8) -> Result<bool> {
        let (phys, _) = self.dir_block(dir, lblk)?;
        let mut b = self.tx_read(phys)?;
        if self.place_entry(&mut b, name, ino, ft)? {
            self.seal_dir_block(dir, &mut b);
            self.tx_put(phys, b);
            return Ok(true);
        }
        Ok(false)
    }

    /// Turns the full one-block directory `dir` into a hashed one holding
    /// its entries and `name` (Linux's `make_indexed_dir`).
    pub(crate) fn make_indexed(&mut self, dir: &mut Inode, name: &[u8], ino: u32, ft: u8) -> Result<()> {
        let filetype = self.filetype();
        let stored = self.sb.raw[SB_DEF_HASH_VERSION];
        let version = self.effective_version(stored);
        let seed = self.hash_seed();
        if dirhash(b"", version, &seed).is_none() {
            return Err(Error::Unsupported("directory hash"));
        }
        let (phys0, _) = self.dir_block(dir, 0)?;
        let b0 = self.tx_read(phys0)?;
        let list = entries(&b0, filetype, self.sb.inodes_count)?;
        let (dot, dotdot) = match (list.first(), list.get(1)) {
            (Some(a), Some(b)) if a.name == b"." && b.name == b".." => ((a.ino, a.file_type), (b.ino, b.file_type)),
            _ => return Err(Error::Corrupt("directory without . and ..")),
        };
        let mut all: Vec<(u32, u32, u32, Vec<u8>, u8)> = Vec::new();
        for e in &list[2..] {
            if e.ino != 0 {
                let (h, m) = dirhash(e.name, version, &seed).unwrap();
                all.push((h, m, e.ino, e.name.to_vec(), e.file_type));
            }
        }
        let (h, m) = dirhash(name, version, &seed).unwrap();
        all.push((h, m, ino, name.to_vec(), if filetype { ft } else { 0 }));
        all.sort_by_key(|e| (e.0, e.1));
        // Two leaves split by size, like a leaf split.
        let total: usize = all.iter().map(|e| rec_size(e.3.len())).sum();
        let mut moved = 0;
        let mut split = all.len();
        while split > 1 && moved < total / 2 {
            split -= 1;
            moved += rec_size(all[split].3.len());
        }
        let continued = all[split].0 == all[split - 1].0;
        let split_hash = all[split].0 | continued as u32;
        let lower: Vec<(u32, &[u8], u8)> = all[..split].iter().map(|e| (e.2, e.3.as_slice(), e.4)).collect();
        let upper: Vec<(u32, &[u8], u8)> = all[split..].iter().map(|e| (e.2, e.3.as_slice(), e.4)).collect();
        let mut lb = self.new_dir_block(&lower);
        self.seal_dir_block(dir, &mut lb);
        let l1 = self.append_dir_block(dir, lb)?;
        let mut ub = self.new_dir_block(&upper);
        self.seal_dir_block(dir, &mut ub);
        let l2 = self.append_dir_block(dir, ub)?;
        // The root: "." (12 bytes), ".." over the rest, the info.
        let bs = self.sb.block_size as usize;
        let mut root = vec![0u8; bs];
        put32(&mut root, 0, dot.0);
        put16(&mut root, 4, 12);
        root[6] = 1;
        root[7] = if filetype { dot.1 } else { 0 };
        root[8] = b'.';
        put32(&mut root, 12, dotdot.0);
        put16(&mut root, 16, (bs - 12) as u16);
        root[18] = 2;
        root[19] = if filetype { dotdot.1 } else { 0 };
        root[20] = b'.';
        root[21] = b'.';
        root[28] = stored;
        root[29] = 8;
        root[30] = 0;
        self.tx_put(phys0, root);
        dir.flags |= FL_INDEX;
        self.put_dx_block(dir, 0, ROOT_COUNT, &[(0, l1 as u32), (split_hash, l2 as u32)], Some(0))
    }

    /// Whether a directory block is an index block (no dirent tail).
    pub(crate) fn dx_count_off(&self, dir: &Inode, b: &[u8]) -> Option<usize> {
        if !self.is_indexed(dir) || (self.sb.metadata_csum() && has_tail(b)) {
            return None;
        }
        let bs = b.len();
        if le16(b, 4) == 12 && b[8] == b'.' && le16(b, 16) as usize == bs - 12 && b[29] == 8 {
            Some(ROOT_COUNT)
        } else if le32(b, 0) == 0 && le16(b, 4) as usize == bs && b[6] == 0 {
            Some(NODE_COUNT)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_e2fsprogs() {
        // `debugfs -R "dx_hash -h <alg> <name>"` (signed chars, no seed).
        let zero = [0u32; 4];
        let cases: [(&[u8], u8, u32, u32); 12] = [
            (b"file-1.txt", HASH_LEGACY, 0x4384_8dde, 0),
            (b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH", HASH_LEGACY, 0xac36_d6e0, 0),
            ("zażółć".as_bytes(), HASH_LEGACY, 0x7684_ca98, 0),
            (b"x", HASH_LEGACY, 0xb26f_0bdc, 0),
            (b"file-1.txt", HASH_HALF_MD4, 0x8315_43b8, 0xd928_d810),
            (b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH", HASH_HALF_MD4, 0xbb14_4a76, 0x630d_8be9),
            ("zażółć".as_bytes(), HASH_HALF_MD4, 0x515a_4704, 0x0b28_4286),
            (b"x", HASH_HALF_MD4, 0x497e_f0fe, 0xec1a_e8f9),
            (b"file-1.txt", HASH_TEA, 0xb854_bdde, 0xc0d0_570f),
            (b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH", HASH_TEA, 0x8ada_2f52, 0xdb4c_1bb9),
            ("zażółć".as_bytes(), HASH_TEA, 0x3b08_4286, 0x02d6_2288),
            (b"x", HASH_TEA, 0xe958_e760, 0x9772_c62c),
        ];
        for (name, v, h, m) in cases {
            assert_eq!(dirhash(name, v, &zero), Some((h, m)), "{} v{v}", core::str::from_utf8(name).unwrap());
        }
        // With a seed (the UUID 0b5e0b5e-0000-4000-8000-000000000001).
        let seed = [0x5e0b_5e0b, 0x0040_0000, 0x0000_0080, 0x0100_0000];
        assert_eq!(dirhash(b"file-1.txt", HASH_HALF_MD4, &seed), Some((0xa68e_0d82, 0x7f76_ad21)));
        assert_eq!(dirhash(b"x", 6, &zero), None, "siphash is not implemented");
    }
}
