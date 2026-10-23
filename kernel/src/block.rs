//! Block layer: a registry of disks and range-checked sector I/O on them.
//!
//! Drivers implement [`BlockDev`]; everything above (file systems, tests)
//! goes through [`read`] / [`write`], which validate the range, enforce
//! read-only devices and split transfers into what the driver accepts.

use crate::pci;
use crate::sync::IrqSpinLock;
use crate::{drivers, kprintln};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Out of range, or a buffer that is not a whole number of sectors.
    InvalidRequest,
    ReadOnly,
    Io,
}

/// A disk of 512-byte sectors. Implementations serialise their own I/O.
pub trait BlockDev: Send + Sync {
    fn name(&self) -> &str;
    fn describe(&self) -> String;
    fn sector_count(&self) -> u64;
    fn read_only(&self) -> bool;
    /// Largest transfer, in sectors, of one `read_blocks` / `write_blocks`.
    fn max_sectors(&self) -> usize;
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError>;
    fn flush(&self) -> Result<(), BlockError>;
}

static DEVICES: IrqSpinLock<Vec<Arc<dyn BlockDev>>> = IrqSpinLock::new(Vec::new());

fn check(dev: &dyn BlockDev, lba: u64, len: usize) -> Result<(), BlockError> {
    if !len.is_multiple_of(SECTOR_SIZE) {
        return Err(BlockError::InvalidRequest);
    }
    let sectors = (len / SECTOR_SIZE) as u64;
    match lba.checked_add(sectors) {
        Some(end) if end <= dev.sector_count() => Ok(()),
        _ => Err(BlockError::InvalidRequest),
    }
}

pub fn read(dev: &dyn BlockDev, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
    check(dev, lba, buf.len())?;
    let step = dev.max_sectors() * SECTOR_SIZE;
    for (i, chunk) in buf.chunks_mut(step).enumerate() {
        dev.read_blocks(lba + (i * dev.max_sectors()) as u64, chunk)?;
    }
    Ok(())
}

pub fn write(dev: &dyn BlockDev, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
    if dev.read_only() {
        return Err(BlockError::ReadOnly);
    }
    check(dev, lba, buf.len())?;
    let step = dev.max_sectors() * SECTOR_SIZE;
    for (i, chunk) in buf.chunks(step).enumerate() {
        dev.write_blocks(lba + (i * dev.max_sectors()) as u64, chunk)?;
    }
    Ok(())
}

pub fn register(dev: Arc<dyn BlockDev>) {
    DEVICES.lock().push(dev);
}

pub fn devices() -> Vec<Arc<dyn BlockDev>> {
    DEVICES.lock().clone()
}

/// Starts a driver for every supported storage controller among `pci`
/// and registers its disks as vda, vdb, ...
pub fn probe(pci: &[pci::Device]) -> usize {
    let mut found = 0;
    for d in pci {
        if d.vendor != drivers::virtio::VENDOR || !drivers::virtio_blk::DEVICE_IDS.contains(&d.device) {
            continue;
        }
        let name = format!("vd{}", (b'a' + found as u8) as char);
        match drivers::virtio_blk::VirtioBlk::probe(d, name) {
            Ok(dev) => {
                register(Arc::new(dev));
                found += 1;
            }
            Err(e) => kprintln!("virtio-blk at {}: not usable: {:?}", d.addr, e),
        }
    }
    found
}

/// Adapter that lets a file system driver use a registered disk.
pub struct Disk(pub Arc<dyn BlockDev>);

impl osyx_fat::BlockDevice for Disk {
    fn sector_count(&self) -> u64 {
        self.0.sector_count()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> osyx_fat::Result<()> {
        read(&*self.0, lba, buf).map_err(|_| osyx_fat::FsError::Io)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> osyx_fat::Result<()> {
        write(&*self.0, lba, buf).map_err(|_| osyx_fat::FsError::Io)
    }

    fn flush(&mut self) -> osyx_fat::Result<()> {
        self.0.flush().map_err(|_| osyx_fat::FsError::Io)
    }
}
