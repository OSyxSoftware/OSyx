#![no_std]
extern crate alloc;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoApic { pub address: u32, pub gsi_base: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Override { pub source: u8, pub gsi: u32, pub low: bool, pub level: bool }
pub struct Madt { pub controllers: Vec<IoApic>, pub overrides: Vec<Override> }

/// Parse ACPI MADT I/O APICs and ISA overrides. Reserved polarity/trigger
/// encodings and truncated records are rejected, never silently guessed.
pub fn parse_madt(bytes: &[u8]) -> Option<Madt> {
    if bytes.get(..4)? != b"APIC" { return None; }
    let length = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    let bytes = bytes.get(..length)?;
    if length < 44 || bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 { return None; }
    let mut out = Madt { controllers: Vec::new(), overrides: Vec::new() };
    let mut at = 44;
    while at < length {
        let kind = *bytes.get(at)?;
        let size = *bytes.get(at + 1)? as usize;
        if size < 2 { return None; }
        let record = bytes.get(at..at.checked_add(size)?)?;
        let word = |offset| -> Option<u32> { Some(u32::from_le_bytes(record.get(offset..offset + 4)?.try_into().ok()?)) };
        match kind {
            1 => {
                if size != 12 { return None; }
                let address = word(4)?;
                let gsi_base = word(8)?;
                if address == 0 || address & 0xfff != 0 || out.controllers.iter().any(|c| c.address == address || c.gsi_base == gsi_base) { return None; }
                out.controllers.push(IoApic { address, gsi_base });
            }
            2 => {
                if size != 10 { return None; }
                let flags = u16::from_le_bytes([record[8], record[9]]);
                if record[2] != 0 || record[3] > 15 || flags & !15 != 0 || flags & 3 == 2 || (flags >> 2) & 3 == 2 { return None; }
                if out.overrides.iter().any(|o| o.source == record[3]) { return None; }
                out.overrides.push(Override { source: record[3], gsi: word(4)?, low: flags & 3 == 3, level: (flags >> 2) & 3 == 3 });
            }
            _ => {}
        }
        at += size;
    }
    Some(out)
}

impl Madt {
    pub fn isa(&self, source: u8) -> Option<Override> {
        if source > 15 { return None; }
        Some(self.overrides.iter().find(|o| o.source == source).copied().unwrap_or(Override { source, gsi: source as u32, low: false, level: false }))
    }
}

/// Fixed-delivery physical destination, initially masked. xAPIC redirection
/// entries support only an eight-bit destination even in x2APIC mode.
pub fn redirection(vector: u8, destination: u32, low: bool, level: bool) -> Option<u64> {
    if vector < 32 || vector == 255 || destination > 255 { return None; }
    Some(vector as u64 | (1 << 16) | ((low as u64) << 13) | ((level as u64) << 15) | ((destination as u64) << 56))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table() -> Vec<u8> {
        let mut b = alloc::vec![0u8; 66];
        b[..4].copy_from_slice(b"APIC"); b[4..8].copy_from_slice(&66u32.to_le_bytes());
        b[44] = 1; b[45] = 12; b[48..52].copy_from_slice(&0xfec00000u32.to_le_bytes());
        b[56] = 2; b[57] = 10; b[59] = 0; b[60..64].copy_from_slice(&2u32.to_le_bytes());
        checksum(&mut b); b
    }
    fn checksum(b: &mut [u8]) { b[9] = 0; b[9] = 0u8.wrapping_sub(b.iter().fold(0u8, |a, x| a.wrapping_add(*x))); }
    #[test] fn overrides_and_entry_encoding() {
        let b = table(); let m = parse_madt(&b).unwrap();
        assert_eq!(m.controllers[0].address, 0xfec00000);
        assert_eq!(m.isa(0).unwrap().gsi, 2);
        assert_eq!(m.isa(1).unwrap().gsi, 1);
        assert!(m.isa(16).is_none());
        let mut low_level = b.clone(); low_level[64] = 15; checksum(&mut low_level);
        let irq = parse_madt(&low_level).unwrap().isa(0).unwrap();
        assert!(irq.low && irq.level);
        assert_eq!(redirection(80, 2, true, true), Some((2 << 56) | (1 << 16) | (1 << 15) | (1 << 13) | 80));
        assert!(redirection(31, 0, false, false).is_none());
        assert!(redirection(80, 256, false, false).is_none());
    }
    #[test] fn malformed_records_and_flags() {
        let b = table();
        for len in 0..b.len() { assert!(parse_madt(&b[..len]).is_none()); }
        for flags in [2u16, 8, 16, 0xffff] {
            let mut x = b.clone(); x[64..66].copy_from_slice(&flags.to_le_bytes()); checksum(&mut x);
            assert!(parse_madt(&x).is_none());
        }
        for size in [0, 1, 11, 255] {
            let mut x = b.clone(); x[45] = size; checksum(&mut x); assert!(parse_madt(&x).is_none());
        }
        let mut x = b; x[9] ^= 1; assert!(parse_madt(&x).is_none());
    }
}
