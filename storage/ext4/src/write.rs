//! Writing. (Stage 1: only what journal recovery needs; creating and
//! changing files comes with the write support.)

use crate::{put32, BlockDevice, Error, Ext4, Result};

/// What [`Ext4::create`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind<'a> {
    File,
    Dir,
    Symlink(&'a str),
}

impl<D: BlockDevice> Ext4<D> {
    pub(crate) fn start_writing(&mut self) -> Result<()> {
        Err(Error::Unsupported("ext4 write support is not enabled"))
    }

    pub(crate) fn finish_writing(&mut self) -> Result<()> {
        Ok(())
    }

    /// Writes the primary superblock with new incompatible-feature flags.
    pub(crate) fn set_incompat(&mut self, incompat: u32) -> Result<()> {
        self.sb.incompat = incompat;
        put32(&mut self.sb.raw, 96, incompat);
        self.write_superblock()
    }

    /// Writes `self.sb.raw` (checksum updated) to the primary location.
    pub(crate) fn write_superblock(&mut self) -> Result<()> {
        self.sb.update_checksum();
        let blk = self.sb.first_data_block as u64;
        let mut b = self.read_block(blk)?;
        let off = if self.sb.block_size == 1024 { 0 } else { 1024 };
        b[off..off + 1024].copy_from_slice(&self.sb.raw);
        self.write_block_raw(blk, &b)?;
        self.dev.flush()
    }
}
