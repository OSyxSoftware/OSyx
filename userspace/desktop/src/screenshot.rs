//! Screenshot encoding for PRISM: 0x00RRGGBB pixels to a PNG file
//! (8-bit RGB, zlib-compressed with miniz_oxide).

use os_std::prelude::*;

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for data in chunks {
        for &b in *data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
            }
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// PNG of a `width` x `height` image stored row by row in `pixels`.
pub fn encode_png(pixels: &[u32], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    // Each row: filter type 1 ("sub"), which compresses flat UI areas well.
    let mut raw = Vec::with_capacity(h * (1 + w * 3));
    for row in pixels.chunks(w).take(h) {
        raw.push(1);
        let mut prev = [0u8; 3];
        for &p in row {
            let rgb = [(p >> 16) as u8, (p >> 8) as u8, p as u8];
            raw.extend_from_slice(&[rgb[0].wrapping_sub(prev[0]), rgb[1].wrapping_sub(prev[1]), rgb[2].wrapping_sub(prev[2])]);
            prev = rgb;
        }
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 3);
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, filters, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// (year, month, day) of a day count since 1970-01-01 (Howard Hinnant's
/// civil-from-days).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}
