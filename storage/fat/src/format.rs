//! Creating a FAT32 volume (no partition table; the volume starts at
//! sector 0), following the parameter choices of the FAT specification.

use crate::{BlockDevice, FsError, Result, FAT_EOC, SECTOR_SIZE};

const RESERVED: u32 = 32;
const NUM_FATS: u32 = 2;

/// Sectors per cluster for a volume of `total` sectors (FAT32 table of the
/// specification, with 512-byte clusters up to 260 MiB).
fn sectors_per_cluster(total: u32) -> u32 {
    match total {
        0..=532_480 => 1,
        532_481..=16_777_216 => 8,
        16_777_217..=33_554_432 => 16,
        33_554_433..=67_108_864 => 32,
        _ => 64,
    }
}

/// Formats `dev` as FAT32 with volume `label` (up to 11 ASCII characters)
/// and volume serial number `serial`.
pub fn format<D: BlockDevice>(dev: &mut D, label: &str, serial: u32) -> Result<()> {
    let total = u32::try_from(dev.sector_count()).map_err(|_| FsError::Unsupported("volume larger than 2 TiB"))?;
    if label.len() > 11 || !label.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
        return Err(FsError::InvalidInput("label must be up to 11 printable ASCII characters"));
    }
    let spc = sectors_per_cluster(total);
    // FAT size from the specification's formula.
    let tmp1 = total.checked_sub(RESERVED).ok_or(FsError::Unsupported("volume too small for FAT32"))?;
    let tmp2 = (256 * spc + NUM_FATS) / 2;
    let fat_size = tmp1.div_ceil(tmp2);
    let data_start = RESERVED + NUM_FATS * fat_size;
    let clusters = total.checked_sub(data_start).ok_or(FsError::Unsupported("volume too small for FAT32"))? / spc;
    if clusters < 65_525 {
        return Err(FsError::Unsupported("volume too small for FAT32 (needs 65525 clusters)"));
    }

    let mut label11 = [b' '; 11];
    for (i, b) in label.bytes().enumerate() {
        label11[i] = b.to_ascii_uppercase();
    }
    let mut bs = [0u8; SECTOR_SIZE];
    bs[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    bs[3..11].copy_from_slice(b"OSYX    ");
    bs[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    bs[13] = spc as u8;
    bs[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    bs[16] = NUM_FATS as u8;
    bs[21] = 0xF8; // fixed disk
    bs[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track
    bs[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads
    bs[32..36].copy_from_slice(&total.to_le_bytes());
    bs[36..40].copy_from_slice(&fat_size.to_le_bytes());
    bs[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    bs[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
    bs[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    bs[64] = 0x80; // drive number
    bs[66] = 0x29; // extended boot signature
    bs[67..71].copy_from_slice(&serial.to_le_bytes());
    bs[71..82].copy_from_slice(&label11);
    bs[82..90].copy_from_slice(b"FAT32   ");
    bs[510] = 0x55;
    bs[511] = 0xAA;

    let mut info = [0u8; SECTOR_SIZE];
    info[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    info[488..492].copy_from_slice(&(clusters - 1).to_le_bytes()); // root uses one
    info[492..496].copy_from_slice(&3u32.to_le_bytes());
    info[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

    let zero = [0u8; SECTOR_SIZE];
    for s in 0..RESERVED {
        dev.write_sectors(s as u64, &zero)?;
    }
    dev.write_sectors(0, &bs)?;
    dev.write_sectors(1, &info)?;
    dev.write_sectors(6, &bs)?;
    dev.write_sectors(7, &info)?;
    // Both FATs: entries 0 and 1 are reserved, 2 is the root directory.
    for f in 0..NUM_FATS {
        let start = RESERVED + f * fat_size;
        for s in 0..fat_size {
            dev.write_sectors((start + s) as u64, &zero)?;
        }
        let mut first = [0u8; SECTOR_SIZE];
        first[0..4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
        first[4..8].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
        first[8..12].copy_from_slice(&FAT_EOC.to_le_bytes());
        dev.write_sectors(start as u64, &first)?;
    }
    // Root directory cluster with the volume label entry.
    for s in 0..spc {
        dev.write_sectors((data_start + s) as u64, &zero)?;
    }
    let mut root = [0u8; SECTOR_SIZE];
    root[..11].copy_from_slice(&label11);
    root[11] = crate::dir::ATTR_VOLUME_ID;
    dev.write_sectors(data_start as u64, &root)?;
    dev.flush()
}
