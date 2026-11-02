//! Windows bitmaps: 1, 4, 8, 16, 24 and 32 bits per pixel, RLE4 and RLE8,
//! bit field masks, top-down and bottom-up rows, and the icon variant
//! (no file header, double height with an AND mask).

use super::{check_size, Frame, Picture};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

fn le16(d: &[u8], p: usize) -> u32 {
    u16::from_le_bytes([d[p], d[p + 1]]) as u32
}

fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

/// Scales a masked field to 8 bits.
fn field(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let x = (v & mask) >> shift;
    let max = (1u64 << bits) - 1;
    (x as u64 * 255 / max) as u8
}

pub fn decode(d: &[u8], icon: bool) -> Result<Picture, String> {
    let base = if icon { 0 } else { 14 };
    if d.len() < base + 16 {
        return Err(String::from("BMP too short"));
    }
    let hsize = le32(d, base) as usize;
    let (w, mut h, bpp, compression, colors_used, mut top_down);
    if hsize == 12 {
        w = le16(d, base + 4) as i32;
        h = le16(d, base + 6) as i32;
        bpp = le16(d, base + 10);
        compression = 0;
        colors_used = 0;
    } else {
        if d.len() < base + 40 {
            return Err(String::from("BMP header cut short"));
        }
        w = le32(d, base + 4) as i32;
        h = le32(d, base + 8) as i32;
        bpp = le16(d, base + 14);
        compression = le32(d, base + 16);
        colors_used = le32(d, base + 32) as usize;
    }
    top_down = h < 0;
    if icon {
        h /= 2;
        top_down = false;
    }
    let (w, h) = (w.unsigned_abs() as usize, h.unsigned_abs() as usize);
    let n = check_size(w, h)?;
    // Colour masks: BI_BITFIELDS after a 40-byte header, or in V4/V5.
    let (mut rm, mut gm, mut bm, mut am) = match bpp {
        16 => (0x7C00, 0x03E0, 0x001F, 0),
        32 => (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0),
        _ => (0, 0, 0, 0),
    };
    if compression == 3 || compression == 6 {
        let mp = base + if hsize >= 52 { 40 } else { hsize };
        if d.len() >= mp + 12 {
            rm = le32(d, mp);
            gm = le32(d, mp + 4);
            bm = le32(d, mp + 8);
            if compression == 6 || hsize >= 56 {
                am = d.get(mp + 12..mp + 16).map_or(0, |b| le32(b, 0));
            }
        }
    } else if bpp == 32 && hsize >= 56 {
        am = le32(d, base + 52);
    }
    // 32-bit icons keep alpha in the top byte.
    if icon && bpp == 32 && am == 0 {
        am = 0xFF00_0000;
    }
    let palette_at = base + hsize;
    let entry = if hsize == 12 { 3 } else { 4 };
    let pal_len = if bpp <= 8 {
        if colors_used > 0 {
            colors_used.min(256)
        } else {
            1 << bpp
        }
    } else {
        0
    };
    let palette: Vec<[u8; 3]> = (0..pal_len)
        .map(|i| {
            let o = palette_at + i * entry;
            d.get(o..o + 3).map_or([0; 3], |c| [c[2], c[1], c[0]])
        })
        .collect();
    let data_at = if icon { palette_at + pal_len * entry } else { le32(d, 10) as usize };
    let pix = d.get(data_at..).ok_or("BMP pixel data missing")?;
    let mut px = vec![[0u8, 0, 0, 255]; n];
    let row_of = |y: usize| if top_down { y } else { h - 1 - y };
    match (compression, bpp) {
        (1, 8) | (2, 4) => {
            // Run-length encoded indices, bottom-up.
            let (mut x, mut y) = (0usize, 0usize);
            let mut i = 0;
            let mut set = |x: usize, y: usize, k: u8, px: &mut Vec<[u8; 4]>| {
                if x < w && y < h {
                    let c = palette.get(k as usize).copied().unwrap_or([0; 3]);
                    px[row_of(y) * w + x] = [c[0], c[1], c[2], 255];
                }
            };
            while i + 1 < pix.len() && y < h {
                let (a, b) = (pix[i], pix[i + 1]);
                i += 2;
                if a > 0 {
                    for k in 0..a as usize {
                        let v = if bpp == 8 {
                            b
                        } else if k % 2 == 0 {
                            b >> 4
                        } else {
                            b & 15
                        };
                        set(x + k, y, v, &mut px);
                    }
                    x += a as usize;
                } else {
                    match b {
                        0 => {
                            x = 0;
                            y += 1;
                        }
                        1 => break,
                        2 => {
                            x += *pix.get(i).unwrap_or(&0) as usize;
                            y += *pix.get(i + 1).unwrap_or(&0) as usize;
                            i += 2;
                        }
                        n => {
                            let n = n as usize;
                            let bytes = if bpp == 8 { n } else { n.div_ceil(2) };
                            for k in 0..n {
                                let v = if bpp == 8 {
                                    *pix.get(i + k).unwrap_or(&0)
                                } else {
                                    let byte = *pix.get(i + k / 2).unwrap_or(&0);
                                    if k % 2 == 0 {
                                        byte >> 4
                                    } else {
                                        byte & 15
                                    }
                                };
                                set(x + k, y, v, &mut px);
                            }
                            x += n;
                            i += bytes + (bytes & 1);
                        }
                    }
                }
            }
        }
        (0 | 3 | 6, 1 | 4 | 8 | 16 | 24 | 32) => {
            let stride = (w * bpp as usize).div_ceil(32) * 4;
            for y in 0..h {
                let row = pix.get(y * stride..(y + 1) * stride).ok_or("BMP pixel data cut short")?;
                let out = &mut px[row_of(y) * w..(row_of(y) + 1) * w];
                for (x, o) in out.iter_mut().enumerate() {
                    *o = match bpp {
                        1 | 4 | 8 => {
                            let bit = x * bpp as usize;
                            let k = (row[bit / 8] >> (8 - bpp as usize - bit % 8)) & ((1 << bpp) - 1) as u8;
                            let c = palette.get(k as usize).copied().unwrap_or([0; 3]);
                            [c[0], c[1], c[2], 255]
                        }
                        16 => {
                            let v = le16(row, 2 * x);
                            [field(v, rm), field(v, gm), field(v, bm), if am != 0 { field(v, am) } else { 255 }]
                        }
                        24 => [row[3 * x + 2], row[3 * x + 1], row[3 * x], 255],
                        _ => {
                            let v = le32(row, 4 * x);
                            [field(v, rm), field(v, gm), field(v, bm), if am != 0 { field(v, am) } else { 255 }]
                        }
                    };
                }
            }
            // Icons: a 1-bit AND mask follows (unless 32-bit alpha is used).
            if icon && bpp != 32 {
                let mstride = w.div_ceil(32) * 4;
                let mask = &pix[stride * h..];
                for y in 0..h {
                    for x in 0..w {
                        if let Some(&b) = mask.get(y * mstride + x / 8) {
                            if b & (0x80 >> (x % 8)) != 0 {
                                px[row_of(y) * w + x][3] = 0;
                            }
                        }
                    }
                }
            }
        }
        _ => return Err(format!("BMP compression {compression} with {bpp} bits per pixel is not supported")),
    }
    let format = format!("BMP, {bpp}-bit{}", if matches!(compression, 1 | 2) { ", RLE" } else { "" });
    Ok(Picture { w, h, frames: vec![Frame { px, delay_ms: 0 }], format })
}
