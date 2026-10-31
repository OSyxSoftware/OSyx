//! Identification data for AMD display hardware.
//!
//! The display core on AMD APUs is DCN (Display Core Next). Programming it
//! needs register bases from the VBIOS IP discovery table, display firmware
//! and the DCN state tables; none of that is hardware-independent. This
//! module only carries the parts needed before a driver can start: the PCI
//! vendor/class check, which DCN generation a known device belongs to, and
//! the location of the ATOM header in a legacy VBIOS image.
//!
//! The device list is a small, confirmed subset. Unknown IDs return `None`;
//! add entries only together with a reference for the mapping.

/// PCI vendor ID of Advanced Micro Devices.
pub const AMD_VENDOR: u16 = 0x1002;

/// PCI class code of a display controller (`0x03`; subclass 0x00 or 0x80).
pub const DISPLAY_CLASS: u8 = 0x03;

/// The 0x55AA header of a legacy option ROM.
pub const ROM_SIGNATURE: [u8; 2] = [0x55, 0xAA];

/// The four-byte text that starts the ATOM ROM header inside a VBIOS image.
pub const ATOM_SIGNATURE: [u8; 4] = *b"ATOM";

/// Display Core Next generations of the devices listed below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcnVersion {
    /// Raven, Raven2, Picasso.
    Dcn1,
    /// Renoir, Cezanne.
    Dcn21,
    /// Rembrandt.
    Dcn31,
}

impl DcnVersion {
    /// The display core of a known AMD display device.
    pub fn for_device(device: u16) -> Option<DcnVersion> {
        match device {
            0x15DD | 0x15D8 => Some(DcnVersion::Dcn1),
            0x1636 | 0x1638 => Some(DcnVersion::Dcn21),
            0x1681 => Some(DcnVersion::Dcn31),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DcnVersion::Dcn1 => "DCN 1.0",
            DcnVersion::Dcn21 => "DCN 2.1",
            DcnVersion::Dcn31 => "DCN 3.1",
        }
    }
}

/// True for an AMD PCI function that drives a display.
pub fn is_amd_display(vendor: u16, class: u8) -> bool {
    vendor == AMD_VENDOR && class == DISPLAY_CLASS
}

/// Finds the ATOM ROM header in a VBIOS image read from the expansion ROM
/// BAR.
///
/// The image must start with the legacy ROM signature. The ATOM header is
/// pointed to by the little-endian u16 at offset 0x48. Its four-byte
/// signature follows the common table header. This is identification only:
/// no command or data tables are interpreted yet.
/// See Linux's `check_atom_bios` in `drivers/gpu/drm/amd/amdgpu/amdgpu_bios.c`.
pub fn atom_header_offset(image: &[u8]) -> Option<usize> {
    if image.len() < 0x4a || image[..2] != ROM_SIGNATURE {
        return None;
    }
    let header = u16::from_le_bytes([image[0x48], image[0x49]]) as usize;
    if header == 0 { return None; }
    let signature = image.get(header.checked_add(4)?..header.checked_add(8)?)?;
    if signature == ATOM_SIGNATURE || signature == b"MOTA" { Some(header) } else { None }
}

/// Find a matching ATOM VBIOS in an ACPI VFCT table. PCI coordinates and
/// IDs must all match; an image for another GPU is never used as a fallback.
/// Layout: AMD's UEFI_ACPI_VFCT and VFCT_IMAGE_HEADER in Linux atombios.h.
pub fn vfct_vbios(table: &[u8], bus: u8, device: u8, function: u8, vendor_id: u16, device_id: u16) -> Option<&[u8]> {
    const VFCT_HEADER: usize = 0x4c;
    const IMAGE_HEADER: usize = 28;
    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?))
    }
    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?))
    }
    if table.get(..4)? != b"VFCT" { return None; }
    let length = u32_at(table, 4)? as usize;
    let table = table.get(..length)?;
    if table.len() < VFCT_HEADER || table.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) != 0 { return None; }
    let mut offset = u32_at(table, 0x34)? as usize;
    if offset < VFCT_HEADER || offset >= table.len() { return None; }
    let lib_offset = u32_at(table, 0x38)? as usize;
    if lib_offset > table.len() || (lib_offset != 0 && lib_offset == offset) { return None; }
    let end = if lib_offset > offset { lib_offset } else { table.len() };
    while offset < end {
        let header_end = offset.checked_add(IMAGE_HEADER)?;
        if header_end > end { return None; }
        let image_len = u32_at(table, offset + 24)? as usize;
        let image_end = header_end.checked_add(image_len)?;
        if image_end > end { return None; }
        let matches = u32_at(table, offset)? == bus as u32
            && u32_at(table, offset + 4)? == device as u32
            && u32_at(table, offset + 8)? == function as u32
            && u16_at(table, offset + 12)? == vendor_id
            && u16_at(table, offset + 14)? == device_id;
        let image = &table[header_end..image_end];
        if matches && atom_header_offset(image).is_some() { return Some(image); }
        offset = image_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_devices_and_known_generations() {
        assert!(is_amd_display(AMD_VENDOR, DISPLAY_CLASS));
        assert!(!is_amd_display(AMD_VENDOR, 0x0C)); // audio function
        assert!(!is_amd_display(0x8086, DISPLAY_CLASS));
        assert_eq!(DcnVersion::for_device(0x15D8), Some(DcnVersion::Dcn1));
        assert_eq!(DcnVersion::for_device(0x15DD), Some(DcnVersion::Dcn1));
        assert_eq!(DcnVersion::for_device(0x1636), Some(DcnVersion::Dcn21));
        assert_eq!(DcnVersion::for_device(0x1681), Some(DcnVersion::Dcn31));
        assert_eq!(DcnVersion::for_device(0x1234), None);
        assert_eq!(DcnVersion::Dcn1.name(), "DCN 1.0");
        assert_eq!(DcnVersion::Dcn21.name(), "DCN 2.1");
        assert_eq!(DcnVersion::Dcn31.name(), "DCN 3.1");
    }

    #[test]
    fn atom_header_requires_a_legacy_rom() {
        let mut rom = [0u8; 256];
        rom[..2].copy_from_slice(&ROM_SIGNATURE);
        rom[0x48..0x4a].copy_from_slice(&0x80u16.to_le_bytes());
        rom[0x84..0x88].copy_from_slice(&ATOM_SIGNATURE);
        assert_eq!(atom_header_offset(&rom), Some(0x80));

        rom[0x84..0x88].copy_from_slice(b"MOTA");
        assert_eq!(atom_header_offset(&rom), Some(0x80));

        rom[0] = 0;
        assert_eq!(atom_header_offset(&rom), None);

        assert_eq!(atom_header_offset(&[]), None);
    }

    #[test]
    fn atom_header_uses_pointer_and_checks_bounds() {
        let mut rom = [0u8; 16 * 1024];
        rom[..2].copy_from_slice(&ROM_SIGNATURE);
        rom[0x30..0x34].copy_from_slice(&ATOM_SIGNATURE); // unrelated text
        assert_eq!(atom_header_offset(&rom), None);
        rom[0x48..0x4a].copy_from_slice(&0x3000u16.to_le_bytes());
        rom[0x3004..0x3008].copy_from_slice(&ATOM_SIGNATURE);
        assert_eq!(atom_header_offset(&rom), Some(0x3000));
        assert_eq!(atom_header_offset(&rom[..0x49]), None);
        assert_eq!(atom_header_offset(&rom[..0x3007]), None);
        rom[0x48..0x4a].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(atom_header_offset(&rom), None);
    }

    #[test]
    fn vfct_selects_only_the_matching_valid_gpu_image() {
        fn checksum(table: &mut [u8]) {
            table[9] = 0;
            table[9] = 0u8.wrapping_sub(table.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
        }
        fn image(table: &mut [u8], at: usize, bus: u32, dev: u32, id: u16) {
            table[at..at + 4].copy_from_slice(&bus.to_le_bytes());
            table[at + 4..at + 8].copy_from_slice(&dev.to_le_bytes());
            table[at + 12..at + 14].copy_from_slice(&AMD_VENDOR.to_le_bytes());
            table[at + 14..at + 16].copy_from_slice(&id.to_le_bytes());
            table[at + 24..at + 28].copy_from_slice(&128u32.to_le_bytes());
            let rom = &mut table[at + 28..at + 156];
            rom[..2].copy_from_slice(&ROM_SIGNATURE);
            rom[0x48..0x4a].copy_from_slice(&0x50u16.to_le_bytes());
            rom[0x54..0x58].copy_from_slice(&ATOM_SIGNATURE);
        }
        let mut table = [0u8; 76 + 156 * 2];
        table[..4].copy_from_slice(b"VFCT");
        let len = table.len() as u32;
        table[4..8].copy_from_slice(&len.to_le_bytes());
        table[0x34..0x38].copy_from_slice(&76u32.to_le_bytes());
        image(&mut table, 76, 1, 0, 0x15d8);
        image(&mut table, 76 + 156, 2, 0, 0x1636);
        checksum(&mut table);
        assert_eq!(vfct_vbios(&table, 2, 0, 0, AMD_VENDOR, 0x1636).map(|v| v.len()), Some(128));
        assert!(vfct_vbios(&table, 1, 0, 1, AMD_VENDOR, 0x15d8).is_none());
        assert!(vfct_vbios(&table, 3, 0, 0, AMD_VENDOR, 0x15d8).is_none());
        assert!(vfct_vbios(&table[..100], 1, 0, 0, AMD_VENDOR, 0x15d8).is_none());
        for length in 0..table.len() {
            assert!(vfct_vbios(&table[..length], 1, 0, 0, AMD_VENDOR, 0x15d8).is_none());
        }
        table[76 + 24..76 + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        checksum(&mut table);
        assert!(vfct_vbios(&table, 1, 0, 0, AMD_VENDOR, 0x15d8).is_none());
        table[9] ^= 1;
        assert!(vfct_vbios(&table, 1, 0, 0, AMD_VENDOR, 0x15d8).is_none());
    }
}
