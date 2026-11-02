//! PNG (ISO/IEC 15948): every colour type and bit depth, palettes with
//! tRNS transparency, Adam7 interlacing, CRC-checked chunks.

use super::{check_size, Frame, Picture};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

fn crc32(data: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for part in data {
        for &b in *part {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
    }
    !c
}

fn be32(d: &[u8], p: usize) -> u32 {
    u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Undoes the row filters of one (sub-)image in place; `stride` bytes per
/// row without the filter byte, `bpp` bytes per pixel (at least 1).
fn unfilter(raw: &[u8], w_bytes: usize, rows: usize, bpp: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; w_bytes * rows];
    for y in 0..rows {
        let src = raw.get(y * (w_bytes + 1)..(y + 1) * (w_bytes + 1)).ok_or("PNG image data is cut short")?;
        let kind = src[0];
        let (done, cur) = out.split_at_mut(y * w_bytes);
        let prev = if y == 0 { None } else { Some(&done[(y - 1) * w_bytes..]) };
        let cur = &mut cur[..w_bytes];
        let src = &src[1..];
        // One loop per filter type: the common byte-at-a-time work stays
        // free of per-byte branching.
        match (kind, prev) {
            (0, _) => cur.copy_from_slice(src),
            (1, _) => {
                cur[..bpp.min(w_bytes)].copy_from_slice(&src[..bpp.min(w_bytes)]);
                for i in bpp..w_bytes {
                    cur[i] = src[i].wrapping_add(cur[i - bpp]);
                }
            }
            (2, None) => cur.copy_from_slice(src),
            (2, Some(up)) => {
                for i in 0..w_bytes {
                    cur[i] = src[i].wrapping_add(up[i]);
                }
            }
            (3, _) => {
                for i in 0..w_bytes {
                    let a = if i >= bpp { cur[i - bpp] as u16 } else { 0 };
                    let b = prev.map_or(0, |p| p[i] as u16);
                    cur[i] = src[i].wrapping_add(((a + b) / 2) as u8);
                }
            }
            (4, _) => {
                for i in 0..w_bytes {
                    let a = if i >= bpp { cur[i - bpp] } else { 0 };
                    let b = prev.map_or(0, |p| p[i]);
                    let c = if i >= bpp { prev.map_or(0, |p| p[i - bpp]) } else { 0 };
                    cur[i] = src[i].wrapping_add(paeth(a, b, c));
                }
            }
            _ => return Err(format!("unknown PNG row filter {kind}")),
        }
    }
    Ok(out)
}

pub fn decode(data: &[u8]) -> Result<Picture, String> {
    let mut p = 8;
    let mut ihdr: Option<(usize, usize, u8, u8, u8)> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    while p + 12 <= data.len() {
        let len = be32(data, p) as usize;
        let kind = &data[p + 4..p + 8];
        let body = data.get(p + 8..p + 8 + len).ok_or("PNG chunk runs past the end")?;
        let crc = data.get(p + 8 + len..p + 12 + len).map(|c| be32(c, 0)).ok_or("PNG chunk runs past the end")?;
        if crc32(&[kind, body]) != crc {
            // Ancillary chunks with a bad CRC are skipped; critical ones fail.
            if kind[0] & 0x20 == 0 {
                return Err(format!("damaged PNG ({} chunk)", String::from_utf8_lossy(kind)));
            }
        } else {
            match kind {
                b"IHDR" if len >= 13 => {
                    ihdr = Some((be32(body, 0) as usize, be32(body, 4) as usize, body[8], body[9], body[12]));
                }
                b"PLTE" => palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                b"tRNS" => trns = body.to_vec(),
                b"IDAT" => idat.extend_from_slice(body),
                b"IEND" => break,
                _ => {}
            }
        }
        p += 12 + len;
    }
    let (w, h, depth, color, interlace) = ihdr.ok_or("PNG without a header")?;
    check_size(w, h)?;
    let channels = match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err(format!("PNG colour type {color} is invalid")),
    };
    let valid_depth = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !valid_depth {
        return Err(format!("PNG bit depth {depth} is invalid for colour type {color}"));
    }
    let raw =
        miniz_oxide::inflate::decompress_to_vec_zlib(&idat).map_err(|_| String::from("damaged PNG image data"))?;
    let bits = channels * depth as usize;
    let bpp = bits.div_ceil(8).max(1);
    let mut px = vec![[0u8; 4]; w * h];
    // Grayscale and RGB transparency: one colour key.
    let key16 = |i: usize| trns.get(2 * i..2 * i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let sample = |row: &[u8], x: usize, c: usize| -> u16 {
        let idx = x * channels + c;
        match depth {
            16 => u16::from_be_bytes([row[2 * idx], row[2 * idx + 1]]),
            8 => row[idx] as u16,
            d => {
                let bit = idx * d as usize;
                ((row[bit / 8] >> (8 - d as usize - bit % 8)) & ((1 << d) - 1)) as u16
            }
        }
    };
    let to8 = |v: u16| -> u8 {
        match depth {
            16 => (v >> 8) as u8,
            8 => v as u8,
            d => (v as u32 * 255 / ((1u32 << d) - 1)) as u8,
        }
    };
    let mut put_row = |row: &[u8], y: usize, xs: &mut dyn Iterator<Item = usize>| {
        for (i, x) in xs.enumerate() {
            let out = &mut px[y * w + x];
            *out = match color {
                0 => {
                    let v = sample(row, i, 0);
                    let a = if key16(0) == Some(v) { 0 } else { 255 };
                    let g = to8(v);
                    [g, g, g, a]
                }
                2 => {
                    let (r, g, b) = (sample(row, i, 0), sample(row, i, 1), sample(row, i, 2));
                    let a = if (key16(0), key16(1), key16(2)) == (Some(r), Some(g), Some(b)) { 0 } else { 255 };
                    [to8(r), to8(g), to8(b), a]
                }
                3 => {
                    let k = sample(row, i, 0) as usize;
                    let c = palette.get(k).copied().unwrap_or([0; 3]);
                    [c[0], c[1], c[2], trns.get(k).copied().unwrap_or(255)]
                }
                4 => {
                    let g = to8(sample(row, i, 0));
                    [g, g, g, to8(sample(row, i, 1))]
                }
                _ => [to8(sample(row, i, 0)), to8(sample(row, i, 1)), to8(sample(row, i, 2)), to8(sample(row, i, 3))],
            };
        }
    };
    if interlace == 0 {
        let w_bytes = (w * bits).div_ceil(8);
        let img = unfilter(&raw, w_bytes, h, bpp)?;
        // 8-bit pictures without a colour key: straight conversions.
        match (color, depth, trns.is_empty()) {
            (6, 8, _) => {
                for (o, c) in px.iter_mut().zip(img.chunks_exact(4)) {
                    *o = [c[0], c[1], c[2], c[3]];
                }
            }
            (2, 8, true) => {
                for (o, c) in px.iter_mut().zip(img.chunks_exact(3)) {
                    *o = [c[0], c[1], c[2], 255];
                }
            }
            (0, 8, true) => {
                for (o, &g) in px.iter_mut().zip(img.iter()) {
                    *o = [g, g, g, 255];
                }
            }
            _ => {
                for y in 0..h {
                    put_row(&img[y * w_bytes..(y + 1) * w_bytes], y, &mut (0..w));
                }
            }
        }
    } else {
        // Adam7: seven passes of sub-images.
        const PASSES: [(usize, usize, usize, usize); 7] =
            [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
        let mut off = 0;
        for (x0, y0, dx, dy) in PASSES {
            let pw = (w + dx - 1 - x0) / dx;
            let ph = (h + dy - 1 - y0) / dy;
            if w <= x0 || h <= y0 || pw == 0 || ph == 0 {
                continue;
            }
            let w_bytes = (pw * bits).div_ceil(8);
            let len = (w_bytes + 1) * ph;
            let img = unfilter(raw.get(off..off + len).ok_or("PNG image data is cut short")?, w_bytes, ph, bpp)?;
            off += len;
            for j in 0..ph {
                put_row(&img[j * w_bytes..(j + 1) * w_bytes], y0 + j * dy, &mut (0..pw).map(|i| x0 + i * dx));
            }
        }
    }
    let kind = match color {
        0 => "grayscale",
        2 => "RGB",
        3 => "indexed",
        4 => "grayscale + alpha",
        _ => "RGBA",
    };
    let format = format!("PNG, {depth}-bit {kind}{}", if interlace != 0 { ", interlaced" } else { "" });
    Ok(Picture { w, h, frames: vec![Frame { px, delay_ms: 0 }], format })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_iend() {
        assert_eq!(crc32(&[b"IEND", b""]), 0xAE42_6082);
    }
}
