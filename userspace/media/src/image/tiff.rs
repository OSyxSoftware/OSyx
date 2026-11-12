//! TIFF (revision 6.0), the first page: both byte orders, strips and tiles,
//! chunky and planar samples.
//!
//! * Compression: none, PackBits, LZW, Deflate, CCITT Group 3 and Group 4.
//! * Pixels: bilevel and grey (1, 2, 4, 8 or 16 bits, either polarity),
//!   palette (1 to 8 bits), RGB and CMYK (8 or 16 bits), with an alpha
//!   sample (associated alpha is un-premultiplied).
//! * The horizontal differencing predictor, the fill order and the eight
//!   orientations.
//!
//! Not supported, and reported by name: BigTIFF, JPEG-compressed strips,
//! YCbCr and CIE L*a*b* pixels, floating-point samples, 12-, 24- and 32-bit
//! samples.

use super::{check_size, Frame, Picture};
use crate::pdf::ccitt;
use crate::pdf::filter::{inflate, lzw};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Directory entries and chunks (strips or tiles) looked at, at most.
const MAX_ENTRIES: usize = 4096;
const MAX_CHUNKS: usize = 1 << 20;
/// Directories followed when counting pages.
const MAX_PAGES: usize = 1000;

struct Reader<'a> {
    data: &'a [u8],
    big: bool,
}

impl<'a> Reader<'a> {
    fn u16(&self, at: usize) -> Option<u32> {
        let b: [u8; 2] = self.data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) } as u32)
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    /// The values of the directory entry at `entry`: BYTE, SHORT or LONG,
    /// in the entry itself when they fit its four bytes. Other types and
    /// values outside the file give an empty list.
    fn values(&self, entry: usize) -> Vec<u32> {
        let (Some(kind), Some(count)) = (self.u16(entry + 2), self.u32(entry + 4)) else { return Vec::new() };
        let size = match kind {
            1 => 1usize,
            3 => 2,
            4 => 4,
            _ => return Vec::new(),
        };
        let count = count as usize;
        if count > MAX_CHUNKS * 4 {
            return Vec::new();
        }
        let at = if count * size <= 4 {
            entry + 8
        } else {
            match self.u32(entry + 8) {
                Some(o) => o as usize,
                None => return Vec::new(),
            }
        };
        (0..count)
            .map_while(|i| match size {
                1 => self.data.get(at.checked_add(i)?).map(|&b| b as u32),
                2 => self.u16(at.checked_add(i * 2)?),
                _ => self.u32(at.checked_add(i * 4)?),
            })
            .collect()
    }
}

/// The fields of one image file directory this decoder uses.
#[derive(Default)]
struct Directory {
    w: usize,
    h: usize,
    bits: Vec<u32>,
    compression: u32,
    photometric: u32,
    fill_order: u32,
    strip_offsets: Vec<u32>,
    samples: usize,
    rows_per_strip: usize,
    strip_counts: Vec<u32>,
    planar: u32,
    t4_options: u32,
    predictor: u32,
    colormap: Vec<u32>,
    tile_w: usize,
    tile_h: usize,
    tile_offsets: Vec<u32>,
    tile_counts: Vec<u32>,
    extra: Vec<u32>,
    sample_format: u32,
    orientation: u32,
}

fn directory(r: &Reader, at: usize) -> Result<(Directory, usize), String> {
    let n = r.u16(at).ok_or("damaged TIFF: the directory lies outside the file")? as usize;
    let mut d = Directory {
        compression: 1,
        fill_order: 1,
        samples: 1,
        rows_per_strip: usize::MAX,
        planar: 1,
        predictor: 1,
        sample_format: 1,
        orientation: 1,
        bits: vec![1],
        ..Directory::default()
    };
    for i in 0..n.min(MAX_ENTRIES) {
        let e = at + 2 + 12 * i;
        let Some(tag) = r.u16(e) else { break };
        let v = r.values(e);
        let first = v.first().copied();
        let set = |field: &mut u32| {
            if let Some(x) = first {
                *field = x;
            }
        };
        let set_size = |field: &mut usize| {
            if let Some(x) = first {
                *field = x as usize;
            }
        };
        match tag {
            256 => set_size(&mut d.w),
            257 => set_size(&mut d.h),
            258 => d.bits = v,
            259 => set(&mut d.compression),
            262 => set(&mut d.photometric),
            266 => set(&mut d.fill_order),
            273 => d.strip_offsets = v,
            274 => set(&mut d.orientation),
            277 => set_size(&mut d.samples),
            278 => set_size(&mut d.rows_per_strip),
            279 => d.strip_counts = v,
            284 => set(&mut d.planar),
            292 => set(&mut d.t4_options),
            317 => set(&mut d.predictor),
            320 => d.colormap = v,
            322 => set_size(&mut d.tile_w),
            323 => set_size(&mut d.tile_h),
            324 => d.tile_offsets = v,
            325 => d.tile_counts = v,
            338 => d.extra = v,
            339 => set(&mut d.sample_format),
            _ => {}
        }
    }
    let next = r.u32(at + 2 + 12 * n).unwrap_or(0) as usize;
    Ok((d, next))
}

/// PackBits: a count byte `n`, then `n + 1` literal bytes (0..=127) or one
/// byte repeated `257 - n` times (129..=255); 128 does nothing.
fn packbits(data: &[u8], want: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(want);
    let mut i = 0;
    while i < data.len() && out.len() < want {
        let n = data[i] as usize;
        i += 1;
        if n < 128 {
            let end = (i + n + 1).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else if n > 128 {
            let Some(&b) = data.get(i) else { break };
            out.resize(out.len() + 257 - n, b);
            i += 1;
        }
    }
    out
}

fn compression_name(c: u32) -> &'static str {
    match c {
        1 => "uncompressed",
        3 => "CCITT Group 3",
        4 => "CCITT Group 4",
        5 => "LZW",
        8 | 32946 => "Deflate",
        32773 => "PackBits",
        _ => "",
    }
}

/// One strip or tile: where it goes and where its data is.
struct Chunk {
    x: usize,
    y: usize,
    /// Columns and rows the data holds (a tile is whole even at the edges).
    w: usize,
    h: usize,
    /// The sample it holds with planar samples; all of them otherwise.
    plane: Option<usize>,
    offset: usize,
    len: usize,
}

fn chunks(d: &Directory) -> Result<Vec<Chunk>, String> {
    let planes = if d.planar == 2 { d.samples } else { 1 };
    let plane_of = |p: usize| (d.planar == 2).then_some(p);
    let mut out = Vec::new();
    if !d.tile_offsets.is_empty() {
        if d.tile_w == 0 || d.tile_h == 0 {
            return Err(String::from("damaged TIFF: tiles without a size"));
        }
        let (across, down) = (d.w.div_ceil(d.tile_w), d.h.div_ceil(d.tile_h));
        let per_plane = across.checked_mul(down).filter(|&n| n <= MAX_CHUNKS).ok_or("damaged TIFF: too many tiles")?;
        for p in 0..planes {
            for i in 0..per_plane {
                let k = p * per_plane + i;
                let (Some(&offset), Some(&len)) = (d.tile_offsets.get(k), d.tile_counts.get(k)) else {
                    return Err(String::from("damaged TIFF: a tile is missing"));
                };
                out.push(Chunk {
                    x: i % across * d.tile_w,
                    y: i / across * d.tile_h,
                    w: d.tile_w,
                    h: d.tile_h,
                    plane: plane_of(p),
                    offset: offset as usize,
                    len: len as usize,
                });
            }
        }
    } else {
        let rows = d.rows_per_strip.clamp(1, d.h);
        let per_plane = d.h.div_ceil(rows);
        if per_plane > MAX_CHUNKS {
            return Err(String::from("damaged TIFF: too many strips"));
        }
        for p in 0..planes {
            for i in 0..per_plane {
                let k = p * per_plane + i;
                let Some(&offset) = d.strip_offsets.get(k) else {
                    return Err(String::from("damaged TIFF: a strip is missing"));
                };
                // Some writers leave the byte counts out of single-strip files.
                let len = d.strip_counts.get(k).map_or(usize::MAX, |&n| n as usize);
                let y = i * rows;
                out.push(Chunk { x: 0, y, w: d.w, h: rows.min(d.h - y), plane: plane_of(p), offset: offset as usize, len });
            }
        }
    }
    Ok(out)
}

const REVERSED: [u8; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// The decompressed bytes of a chunk: `rows` rows of `row_bytes`, zero
/// filled where the data ends early.
fn unpack(d: &Directory, data: &[u8], cols: usize, rows: usize, row_bytes: usize) -> Result<Vec<u8>, String> {
    let want = row_bytes * rows;
    let reversed;
    let data = if d.fill_order == 2 {
        reversed = data.iter().map(|&b| REVERSED[(b & 15) as usize] << 4 | REVERSED[(b >> 4) as usize]).collect::<Vec<u8>>();
        &reversed[..]
    } else {
        data
    };
    let mut out = match d.compression {
        1 => data[..data.len().min(want)].to_vec(),
        5 => lzw(data, true),
        8 | 32946 => inflate(data),
        32773 => packbits(data, want),
        3 | 4 => {
            let k = match (d.compression, d.t4_options & 1) {
                (4, _) => -1,
                (_, 1) => 1,
                _ => 0,
            };
            // White runs decode to 0 bits; PhotometricInterpretation then
            // says what a 0 looks like, as for uncompressed bilevel data.
            let p = ccitt::Params { k, columns: cols, rows: Some(rows), byte_align: d.t4_options & 4 != 0, black_is_1: true };
            ccitt::decode(data, &p)
        }
        6 | 7 => return Err(String::from("JPEG-compressed TIFF pictures are not supported")),
        c => return Err(format!("TIFF compression {c} is not supported")),
    };
    out.resize(want, 0);
    Ok(out)
}

/// Decodes the first page of the TIFF file in `data`.
pub fn decode(data: &[u8]) -> Result<Picture, String> {
    let big = data.starts_with(b"MM");
    let r = Reader { data, big };
    match r.u16(2) {
        Some(42) => {}
        Some(43) => return Err(String::from("BigTIFF pictures are not supported")),
        _ => return Err(String::from("damaged TIFF: no header")),
    }
    let first = r.u32(4).ok_or("damaged TIFF: no header")? as usize;
    let (d, mut next) = directory(&r, first)?;
    let mut pages = 1;
    while next != 0 && pages < MAX_PAGES {
        match directory(&r, next) {
            Ok((_, n)) => next = n,
            Err(_) => break,
        }
        pages += 1;
    }

    let (w, h) = (d.w, d.h);
    let pixels = check_size(w, h)?;
    let spp = d.samples;
    let bps = d.bits.first().copied().unwrap_or(1) as usize;
    if spp == 0 || spp > 8 || d.bits.iter().any(|&b| b as usize != bps) {
        return Err(String::from("TIFF pictures with samples of different sizes are not supported"));
    }
    if d.sample_format == 3 {
        return Err(String::from("floating-point TIFF pictures are not supported"));
    }
    if !matches!(bps, 1 | 2 | 4 | 8 | 16) {
        return Err(format!("TIFF pictures with {bps}-bit samples are not supported"));
    }
    if d.predictor == 2 && bps < 8 || d.predictor > 2 {
        return Err(String::from("this TIFF predictor is not supported"));
    }
    if matches!(d.compression, 3 | 4) && (bps != 1 || spp != 1) {
        return Err(String::from("damaged TIFF: fax compression of a picture that is not bilevel"));
    }
    // What the samples mean, and how many of them are colour.
    let colour = match d.photometric {
        0 | 1 | 3 => 1,
        2 => 3,
        5 => 4,
        6 => return Err(String::from("YCbCr TIFF pictures are not supported")),
        8 | 9 => return Err(String::from("CIE L*a*b* TIFF pictures are not supported")),
        p => return Err(format!("TIFF photometric interpretation {p} is not supported")),
    };
    if spp < colour {
        return Err(String::from("damaged TIFF: too few samples per pixel"));
    }
    if d.photometric == 3 && (bps > 8 || d.colormap.len() < 3 << bps) {
        return Err(String::from("damaged TIFF: no usable colour map"));
    }
    pixels.checked_mul(spp).filter(|&n| n <= super::MAX_PIXELS * 4).ok_or("this TIFF picture is too large")?;

    // Every sample as one byte: the value itself up to 8 bits, the high
    // byte of 16 bits.
    let mut samples = vec![0u8; pixels * spp];
    let high = if big { 0 } else { 1 };
    for c in chunks(&d)? {
        let in_chunk = if c.plane.is_some() { 1 } else { spp };
        // A tile may be larger than a small picture, but not without bound:
        // a damaged size must not ask for gigabytes.
        if c.w.checked_mul(c.h).is_none_or(|n| n > (4 * pixels).max(1 << 22)) {
            return Err(String::from("damaged TIFF: a tile or strip is too large"));
        }
        let row_bytes = (c.w * in_chunk * bps).div_ceil(8);
        let end = c.offset.saturating_add(c.len).min(data.len());
        let Some(raw) = data.get(c.offset..end) else { continue };
        let mut bytes = unpack(&d, raw, c.w, c.h, row_bytes)?;
        if d.predictor == 2 {
            let step = in_chunk * bps / 8;
            for row in bytes.chunks_exact_mut(row_bytes) {
                if bps == 8 {
                    for i in step..row.len() {
                        row[i] = row[i].wrapping_add(row[i - step]);
                    }
                } else {
                    // 16-bit samples are added as numbers, in the file's byte order.
                    for i in (step..row.len() - 1).step_by(2) {
                        let word = |b: &[u8], i: usize| if big { u16::from_be_bytes([b[i], b[i + 1]]) } else { u16::from_le_bytes([b[i], b[i + 1]]) };
                        let v = word(row, i).wrapping_add(word(row, i - step));
                        let v = if big { v.to_be_bytes() } else { v.to_le_bytes() };
                        row[i..i + 2].copy_from_slice(&v);
                    }
                }
            }
        }
        let cols = c.w.min(w.saturating_sub(c.x));
        for y in 0..c.h.min(h.saturating_sub(c.y)) {
            let row = &bytes[y * row_bytes..(y + 1) * row_bytes];
            let base = ((c.y + y) * w + c.x) * spp;
            for i in 0..cols * in_chunk {
                let v = match bps {
                    8 => row[i],
                    16 => row[2 * i + high],
                    _ => (row[i * bps / 8] >> (8 - bps - i * bps % 8)) & ((1 << bps) - 1) as u8,
                };
                let at = match c.plane {
                    Some(p) => base + i * spp + p,
                    None => base + i,
                };
                samples[at] = v;
            }
        }
    }

    // Small samples spread over 0..=255.
    let full = |v: u8| -> u8 {
        match bps {
            1 => v * 255,
            2 => v * 85,
            4 => v * 17,
            _ => v,
        }
    };
    // The first extra sample is alpha when it says so: 1 = associated
    // (premultiplied), 2 = unassociated.
    let alpha_kind = if spp > colour { d.extra.first().copied().unwrap_or(0) } else { 0 };
    let mut px = Vec::with_capacity(pixels);
    for s in samples.chunks_exact(spp) {
        let mut rgb = match d.photometric {
            0 => [255 - full(s[0]); 3],
            1 => [full(s[0]); 3],
            2 => [full(s[0]), full(s[1]), full(s[2])],
            3 => {
                let n = 1usize << bps;
                let entry = |k: usize| (d.colormap[k * n + s[0] as usize] >> 8) as u8;
                [entry(0), entry(1), entry(2)]
            }
            _ => {
                // CMYK: each ink takes its share of what black leaves.
                let k = 255 - s[3] as u32;
                let ink = |c: u8| (k - (c as u32 * k + 127) / 255) as u8;
                [ink(s[0]), ink(s[1]), ink(s[2])]
            }
        };
        let a = match alpha_kind {
            1 | 2 => full(s[colour]),
            _ => 255,
        };
        if alpha_kind == 1 && a != 0 && a != 255 {
            for c in &mut rgb {
                *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            }
        }
        px.push([rgb[0], rgb[1], rgb[2], a]);
    }

    let (px, w, h) = orient(px, w, h, d.orientation);
    let kind = match (d.photometric, bps) {
        (0 | 1, 1) => "bilevel",
        (0 | 1, _) => "grey",
        (2, _) => "RGB",
        (3, _) => "palette",
        _ => "CMYK",
    };
    let alpha = if matches!(alpha_kind, 1 | 2) { " with alpha" } else { "" };
    let page = if pages > 1 { format!(", page 1 of {pages}") } else { String::new() };
    let format = format!("TIFF, {bps}-bit {kind}{alpha}, {}{page}", compression_name(d.compression));
    Ok(Picture { w, h, frames: vec![Frame { px, delay_ms: 0 }], format })
}

/// Turns the stored rows into the picture the Orientation field asks for:
/// 2 mirrored, 3 upside down, 4 flipped, 5 to 8 the same four turned so
/// that the stored rows are the picture's columns.
fn orient(px: Vec<[u8; 4]>, w: usize, h: usize, orientation: u32) -> (Vec<[u8; 4]>, usize, usize) {
    if !(2..=8).contains(&orientation) {
        return (px, w, h);
    }
    let turned = orientation >= 5;
    let (ow, oh) = if turned { (h, w) } else { (w, h) };
    let mut out = Vec::with_capacity(px.len());
    for y in 0..oh {
        for x in 0..ow {
            // (column, row) in the stored picture for output (x, y).
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            out.push(px[sy * w + sx]);
        }
    }
    (out, ow, oh)
}
