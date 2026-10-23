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
    /// The GPT entry, if this device is a partition.
    fn partition(&self) -> Option<&osyx_part::Partition> {
        None
    }
}

static DEVICES: IrqSpinLock<Vec<Arc<dyn BlockDev>>> = IrqSpinLock::new(Vec::new());

fn check(dev: &(impl BlockDev + ?Sized), lba: u64, len: usize) -> Result<(), BlockError> {
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

/// A GPT partition of a disk, registered as `<disk><number>` (e.g. `vda2`).
pub struct Partition {
    name: String,
    parent: Arc<dyn BlockDev>,
    start: u64,
    sectors: u64,
    pub info: osyx_part::Partition,
}

impl BlockDev for Partition {
    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> String {
        format!("partition {} of {} ({}, \"{}\")", self.info.number, self.parent.name(), self.info.type_guid, self.info.name)
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_only(&self) -> bool {
        self.parent.read_only()
    }

    fn max_sectors(&self) -> usize {
        self.parent.max_sectors()
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        check(self, lba, buf.len())?;
        self.parent.read_blocks(self.start + lba, buf)
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check(self, lba, buf.len())?;
        self.parent.write_blocks(self.start + lba, buf)
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.parent.flush()
    }

    fn partition(&self) -> Option<&osyx_part::Partition> {
        Some(&self.info)
    }
}

struct Reader<'a>(&'a dyn BlockDev);

impl osyx_part::SectorReader for Reader<'_> {
    fn sectors(&self) -> u64 {
        self.0.sector_count()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), osyx_part::GptError> {
        read(self.0, lba, buf).map_err(|_| osyx_part::GptError::Io)
    }
}

/// Reads the GPT of every whole disk and registers its partitions.
pub fn scan_partitions() -> usize {
    let mut found = 0;
    for disk in devices().into_iter().filter(|d| d.partition().is_none()) {
        match osyx_part::parse(&mut Reader(&*disk)) {
            Ok(t) => {
                if t.used_backup {
                    kprintln!("gpt {}: primary table damaged, using the backup", disk.name());
                }
                for p in t.partitions {
                    let part = Partition {
                        name: format!("{}{}", disk.name(), p.number),
                        parent: disk.clone(),
                        start: p.first_lba,
                        sectors: p.sectors(),
                        info: p,
                    };
                    kprintln!("gpt {}: {} {} MiB, {}", disk.name(), part.name, part.sectors >> 11, part.describe());
                    register(Arc::new(part));
                    found += 1;
                }
            }
            Err(osyx_part::GptError::NotGpt) => {}
            Err(e) => kprintln!("gpt {}: unusable partition table: {:?}", disk.name(), e),
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

impl osyx_ext4::BlockDevice for Disk {
    fn sector_count(&self) -> u64 {
        self.0.sector_count()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> osyx_ext4::Result<()> {
        read(&*self.0, lba, buf).map_err(|_| osyx_ext4::Error::Io)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> osyx_ext4::Result<()> {
        write(&*self.0, lba, buf).map_err(|_| osyx_ext4::Error::Io)
    }

    fn flush(&mut self) -> osyx_ext4::Result<()> {
        self.0.flush().map_err(|_| osyx_ext4::Error::Io)
    }
}