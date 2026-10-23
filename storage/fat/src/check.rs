//! Consistency check: walks every directory and file, marks the clusters
//! their chains use and compares the result with the FAT.

use crate::{BlockDevice, FileSystem, FsError, Result, FAT_FREE};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub files: u32,
    pub directories: u32,
    pub used_clusters: u32,
    pub free_clusters: u32,
    /// Whether the FSInfo free-cluster hint matches the FAT. The
    /// specification calls it advisory, so a mismatch is not corruption, but
    /// volumes written by this driver must always be accurate.
    pub fsinfo_accurate: bool,
}

impl<D: BlockDevice> FileSystem<D> {
    /// Marks the chain at `first`; returns its length. Errors on cross-links
    /// (a cluster used twice) and loops.
    fn mark_chain(&mut self, first: u32, used: &mut [bool]) -> Result<u32> {
        let mut c = first;
        let mut len = 0;
        loop {
            if !self.geo.is_valid_cluster(c) {
                return Err(FsError::Corrupt("chain references an invalid cluster"));
            }
            if used[c as usize] {
                return Err(FsError::Corrupt("cross-linked cluster"));
            }
            used[c as usize] = true;
            len += 1;
            match self.next_cluster(c)? {
                Some(n) => c = n,
                None => return Ok(len),
            }
        }
    }

    /// Verifies the cluster accounting of the whole volume:
    /// * no cluster belongs to two chains, no chain loops;
    /// * every file's chain length matches its size;
    /// * every allocated FAT entry is reachable (no lost clusters);
    /// * and reports whether the FSInfo free count matches the FAT.
    pub fn check(&mut self) -> Result<CheckReport> {
        let mut used = vec![false; self.geo.max_cluster() as usize];
        let mut report = CheckReport::default();
        let cbytes = self.geo.cluster_bytes() as u64;
        let root = self.geo.root_cluster;
        self.mark_chain(root, &mut used)?;
        let mut pending: Vec<u32> = vec![root];
        while let Some(dir) = pending.pop() {
            report.directories += 1;
            for it in self.read_items(dir)? {
                if it.is_dot() {
                    continue;
                }
                if it.is_dir() {
                    if it.first_cluster == 0 {
                        return Err(FsError::Corrupt("directory without clusters"));
                    }
                    self.mark_chain(it.first_cluster, &mut used)?;
                    pending.push(it.first_cluster);
                } else {
                    report.files += 1;
                    let expected = (it.size as u64).div_ceil(cbytes) as u32;
                    let actual = if it.first_cluster == 0 { 0 } else { self.mark_chain(it.first_cluster, &mut used)? };
                    if actual != expected {
                        return Err(FsError::Corrupt("file chain length does not match its size"));
                    }
                }
            }
        }
        let mut allocated = 0;
        for c in 2..self.geo.max_cluster() {
            let in_fat = self.fat_get(c)? != FAT_FREE;
            if in_fat {
                allocated += 1;
            }
            if in_fat && !used[c as usize] {
                return Err(FsError::Corrupt("lost cluster (allocated but unreachable)"));
            }
            if !in_fat && used[c as usize] {
                return Err(FsError::Corrupt("reachable cluster marked free"));
            }
        }
        report.used_clusters = allocated;
        report.free_clusters = self.geo.cluster_count - allocated;
        report.fsinfo_accurate = report.free_clusters == self.free_count;
        Ok(report)
    }
}
