//! Intel Bluetooth controllers with the boot loader (Wireless-AC 8260,
//! 8265, 9260, 9560 and others before the AX series): after power-on the
//! controller runs a boot loader that needs its operational firmware
//! (`/etc/firmware/ibt-*.sfi`), sent with Intel's secure-send vendor
//! command, and afterwards a device configuration file (`ibt-*.ddc`).
//!
//! Sequence: Read Version (fc05) tells boot loader (fw variant 06) from
//! operational firmware (23); Read Boot Params (fc0d); secure send (fc09)
//! of the 128-byte CSS header, the 256-byte public key and the 256-byte
//! signature, then of the firmware's command records in 4-byte aligned
//! groups; the controller reports the result in vendor event 06; Intel
//! Reset (fc01) to the boot address boots the firmware, which reports
//! vendor event 02; then each DDC record goes out with fc8b.

use super::{Hci, HciError, EVT_VENDOR};
use crate::kprintln;
use alloc::string::String;
use alloc::vec::Vec;

const OP_RESET: u16 = 0xfc01;
const OP_READ_VERSION: u16 = 0xfc05;
const OP_SECURE_SEND: u16 = 0xfc09;
const OP_READ_BOOT_PARAMS: u16 = 0xfc0d;
/// Firmware record carrying the boot address.
const OP_WRITE_BOOT_PARAMS: u16 = 0xfc0e;
const OP_WRITE_DDC: u16 = 0xfc8b;

const FW_BOOTLOADER: u8 = 0x06;
const FW_OPERATIONAL: u8 = 0x23;
const EVT_BOOTUP: u8 = 0x02;
const EVT_SECURE_SEND_RESULT: u8 = 0x06;

/// Layout of an `.sfi` file before the command records.
const CSS_LEN: usize = 128;
const KEY_OFFSET: usize = 128;
const KEY_LEN: usize = 256;
const SIGNATURE_OFFSET: usize = 388;
const SIGNATURE_LEN: usize = 256;
const RECORDS_OFFSET: usize = SIGNATURE_OFFSET + SIGNATURE_LEN;
/// Bytes of firmware per secure-send command.
const FRAGMENT: usize = 252;

#[derive(Debug, Clone, Copy)]
pub struct Version {
    pub hw_platform: u8,
    pub hw_variant: u8,
    pub hw_revision: u8,
    pub fw_variant: u8,
    pub fw_revision: u8,
    pub fw_build: (u8, u8, u8),
}

impl Version {
    /// Return parameters of Read Version (status first).
    pub fn parse(r: &[u8]) -> Option<Version> {
        // The TLV format of newer controllers starts differently and is
        // longer; the boot loader format is exactly 10 bytes on platform 0x37.
        if r.len() != 10 || r[1] != 0x37 {
            return None;
        }
        Some(Version { hw_platform: r[1], hw_variant: r[2], hw_revision: r[3], fw_variant: r[4], fw_revision: r[5], fw_build: (r[6], r[7], r[8]) })
    }
}

/// Firmware file name (without suffix) of a boot loader controller, from
/// its version and the device revision of the boot parameters.
pub fn firmware_name(v: &Version, dev_revid: u16) -> Option<String> {
    match v.hw_variant {
        // SfP (8260), WsP (8265)
        0x0b | 0x0c => Some(alloc::format!("ibt-{}-{}", v.hw_variant, dev_revid)),
        // JfP (9560), ThP (9260), HrP, CcP
        0x11..=0x14 => Some(alloc::format!("ibt-{}-{}-{}", v.hw_variant, v.hw_revision, v.fw_revision)),
        _ => None,
    }
}

/// The groups of an `.sfi` file's command records sent with secure send
/// (type 01), each ending on a 4-byte boundary, and the boot address from
/// the Write Boot Params record.
pub fn record_groups(fw: &[u8]) -> Result<(Vec<&[u8]>, u32), &'static str> {
    if fw.len() < RECORDS_OFFSET {
        return Err("firmware file too short");
    }
    let mut groups = Vec::new();
    let mut boot_addr = 0;
    let (mut start, mut pos) = (RECORDS_OFFSET, RECORDS_OFFSET);
    while pos < fw.len() {
        if pos + 3 > fw.len() {
            return Err("truncated firmware record");
        }
        let opcode = u16::from_le_bytes([fw[pos], fw[pos + 1]]);
        let plen = fw[pos + 2] as usize;
        if pos + 3 + plen > fw.len() {
            return Err("truncated firmware record");
        }
        if opcode == OP_WRITE_BOOT_PARAMS && plen >= 4 {
            boot_addr = u32::from_le_bytes([fw[pos + 3], fw[pos + 4], fw[pos + 5], fw[pos + 6]]);
        }
        pos += 3 + plen;
        if (pos - start) % 4 == 0 {
            groups.push(&fw[start..pos]);
            start = pos;
        }
    }
    if start != pos {
        return Err("firmware records do not end on a 4-byte boundary");
    }
    Ok((groups, boot_addr))
}

/// The records of a `.ddc` file (each: length byte, then that many bytes).
pub fn ddc_records(ddc: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ddc.len() {
        let n = ddc[i] as usize + 1;
        if n < 3 || i + n > ddc.len() {
            return Err("malformed DDC file");
        }
        out.push(&ddc[i..i + n]);
        i += n;
    }
    Ok(out)
}

fn secure_send(hci: &mut Hci, kind: u8, data: &[u8]) -> Result<(), HciError> {
    for chunk in data.chunks(FRAGMENT) {
        let mut p = Vec::with_capacity(chunk.len() + 1);
        p.push(kind);
        p.extend_from_slice(chunk);
        // The boot loader takes secure send on the bulk endpoint.
        hci.send_bulk_command(OP_SECURE_SEND, &p);
        hci.answer(OP_SECURE_SEND, 2000)?;
    }
    Ok(())
}

fn read_version(hci: &mut Hci) -> Result<Version, HciError> {
    let r = hci.command(OP_READ_VERSION, &[], 2000)?;
    Version::parse(&r).ok_or(HciError::Protocol("Intel version in an unsupported (TLV) format: AX-series controllers are not supported yet"))
}

fn vendor_event(hci: &mut Hci, code: u8, timeout_ms: u64) -> Result<Option<Vec<u8>>, HciError> {
    hci.wait_event(timeout_ms, |e| e.len() >= 3 && e[0] == EVT_VENDOR && e[2] == code)
}

pub fn setup(hci: &mut Hci, unit: usize, product: u16) -> Result<(), HciError> {
    // In the boot loader events can come on the bulk endpoint too.
    hci.set_bulk_events(true);
    let r = download(hci, unit, product);
    hci.set_bulk_events(false);
    r
}

fn download(hci: &mut Hci, unit: usize, product: u16) -> Result<(), HciError> {
    let v = read_version(hci)?;
    kprintln!(
        "bluetooth: hci{}: Intel {:04x}: hardware variant {:#04x} revision {}, firmware variant {:#04x} revision {}, build {}-{}.{}",
        unit,
        product,
        v.hw_variant,
        v.hw_revision,
        v.fw_variant,
        v.fw_revision,
        v.fw_build.0,
        v.fw_build.1,
        2000 + v.fw_build.2 as u32
    );
    if v.fw_variant == FW_OPERATIONAL {
        kprintln!("bluetooth: hci{}: firmware already running", unit);
        return Ok(());
    }
    if v.fw_variant != FW_BOOTLOADER {
        return Err(HciError::Protocol("Intel controller neither in boot loader nor operational"));
    }
    let bp = hci.command(OP_READ_BOOT_PARAMS, &[], 2000)?;
    if bp.len() < 6 {
        return Err(HciError::Protocol("short Intel boot parameters"));
    }
    let dev_revid = u16::from_le_bytes([bp[4], bp[5]]);
    let name = firmware_name(&v, dev_revid).ok_or(HciError::Protocol("no firmware name known for this Intel controller"))?;
    let path = alloc::format!("/etc/firmware/{name}.sfi");
    let Some(fw) = crate::bsd::read_file(&path) else {
        kprintln!("bluetooth: hci{}: firmware {} is missing", unit, path);
        return Err(HciError::Protocol("firmware file missing"));
    };
    let (groups, boot_addr) = record_groups(&fw).map_err(HciError::Protocol)?;
    kprintln!("bluetooth: hci{}: loading {} ({} bytes, boot address {:#x})", unit, path, fw.len(), boot_addr);
    secure_send(hci, 0x00, &fw[..CSS_LEN])?;
    secure_send(hci, 0x03, &fw[KEY_OFFSET..KEY_OFFSET + KEY_LEN])?;
    secure_send(hci, 0x02, &fw[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_LEN])?;
    for g in groups {
        secure_send(hci, 0x01, g)?;
    }
    // Result: [ff, len, 06, result, opcode (2), status].
    let ev = vendor_event(hci, EVT_SECURE_SEND_RESULT, 5000)?.ok_or(HciError::Protocol("no firmware download result"))?;
    if ev.len() >= 4 && ev[3] != 0 {
        kprintln!("bluetooth: hci{}: firmware rejected (result {:#04x})", unit, ev[3]);
        return Err(HciError::Protocol("firmware rejected by the controller"));
    }
    let mut reset = alloc::vec![0x00, 0x01, 0x00, 0x01];
    reset.extend_from_slice(&boot_addr.to_le_bytes());
    hci.send_command(OP_RESET, &reset);
    vendor_event(hci, EVT_BOOTUP, 2000)?.ok_or(HciError::Protocol("firmware did not boot"))?;
    let v = read_version(hci)?;
    kprintln!("bluetooth: hci{}: firmware running (variant {:#04x})", unit, v.fw_variant);
    let ddc_path = alloc::format!("/etc/firmware/{name}.ddc");
    match crate::bsd::read_file(&ddc_path) {
        Some(ddc) => {
            let records = ddc_records(&ddc).map_err(HciError::Protocol)?;
            for r in &records {
                hci.command(OP_WRITE_DDC, r, 2000)?;
            }
            kprintln!("bluetooth: hci{}: {} DDC parameters set", unit, records.len());
        }
        None => kprintln!("bluetooth: hci{}: no {} (defaults kept)", unit, ddc_path),
    }
    Ok(())
}
