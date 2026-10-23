//! Checksums used by ext4 metadata.

const fn crc32c_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC32C: [u32; 256] = crc32c_table();

/// CRC-32C (Castagnoli) update without pre/post inversion, as ext4's
/// `ext4_chksum` computes it: callers pass the running value (initially
/// `!0` or a seed) and use the result directly.
pub fn crc32c(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = CRC32C[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// CRC-16 (ANSI, reflected polynomial 0xA001) used by the older
/// `uninit_bg` / `gdt_csum` group descriptor checksums.
pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // Standard CRC-32C check value: inverted in, inverted out.
        assert_eq!(!crc32c(!0, b"123456789"), 0xE306_9283);
        // CRC-16/ARC check value.
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    }
}
