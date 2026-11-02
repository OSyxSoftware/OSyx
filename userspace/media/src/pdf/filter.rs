//! Stream filters (ISO 32000-1, 7.4): FlateDecode and LZWDecode with PNG
//! and TIFF predictors, ASCIIHexDecode, ASCII85Decode and RunLengthDecode.
//! Image codecs (DCTDecode and the like) stay encoded here and are handled
//! by the image code.

use super::object::{key, Dict, Object};
#[allow(unused_imports)]
use super::Float;
use alloc::vec;
use alloc::vec::Vec;

/// Filters that turn bytes into an image rather than into bytes.
pub const IMAGE_FILTERS: [&[u8]; 4] = [b"DCTDecode", b"JPXDecode", b"CCITTFaxDecode", b"JBIG2Decode"];

/// The filter names and their parameter dictionaries of a stream.
pub fn filters(d: &Dict) -> Vec<(Vec<u8>, Option<Dict>)> {
    let names: Vec<Vec<u8>> = match key(d, "Filter").or_else(|| key(d, "F").filter(|_| key(d, "Length").is_none())) {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(a)) => a.iter().filter_map(|o| o.as_name().map(|n| n.to_vec())).collect(),
        _ => Vec::new(),
    };
    let parms: Vec<Option<Dict>> = match key(d, "DecodeParms").or_else(|| key(d, "DP")) {
        Some(Object::Dict(p)) => vec![Some(p.clone())],
        Some(Object::Array(a)) => a.iter().map(|o| o.as_dict().cloned()).collect(),
        _ => Vec::new(),
    };
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let n = match n.as_slice() {
                b"Fl" => b"FlateDecode".to_vec(),
                b"LZW" => b"LZWDecode".to_vec(),
                b"AHx" => b"ASCIIHexDecode".to_vec(),
                b"A85" => b"ASCII85Decode".to_vec(),
                b"RL" => b"RunLengthDecode".to_vec(),
                b"DCT" => b"DCTDecode".to_vec(),
                b"CCF" => b"CCITTFaxDecode".to_vec(),
                _ => n,
            };
            (n, parms.get(i).cloned().flatten())
        })
        .collect()
}

/// Applies the byte filters in order. Stops before the first image filter
/// and returns its name with the data still encoded for it.
pub fn decode(data: &[u8], d: &Dict) -> (Vec<u8>, Option<(Vec<u8>, Option<Dict>)>) {
    let mut cur = data.to_vec();
    for (name, parms) in filters(d) {
        if IMAGE_FILTERS.contains(&name.as_slice()) {
            return (cur, Some((name, parms)));
        }
        cur = match name.as_slice() {
            b"FlateDecode" => predict(inflate(&cur), parms.as_ref()),
            b"LZWDecode" => {
                let early =
                    parms.as_ref().and_then(|p| key(p, "EarlyChange")).and_then(Object::as_i64).unwrap_or(1) != 0;
                predict(lzw(&cur, early), parms.as_ref())
            }
            b"ASCIIHexDecode" => ascii_hex(&cur),
            b"ASCII85Decode" => ascii85(&cur),
            b"RunLengthDecode" => run_length(&cur),
            // Crypt with the Identity filter, or something unknown: as is.
            _ => cur,
        };
    }
    (cur, None)
}

/// Largest decoded size of one stream.
const MAX_INFLATE: usize = 256 << 20;

/// zlib data; damaged streams give what could be recovered.
pub fn inflate(data: &[u8]) -> Vec<u8> {
    use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
    use miniz_oxide::inflate::TINFLStatus;
    let raw = data.len() < 2 || (data[0] & 0x0F) != 8 || ((data[0] as u16) << 8 | data[1] as u16) % 31 != 0;
    let flags = if raw { 0 } else { inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER }
        | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let mut out = vec![0u8; (data.len() * 4).max(1024)];
    let mut st = DecompressorOxide::new();
    let (mut inp, mut outp) = (0usize, 0usize);
    loop {
        let (status, used_in, used_out) = decompress(&mut st, &data[inp..], &mut out, outp, flags);
        inp += used_in;
        outp += used_out;
        match status {
            TINFLStatus::HasMoreOutput => {
                // A deflate bomb: keep what fits.
                if out.len() >= MAX_INFLATE {
                    break;
                }
                let n = (out.len() * 2).min(MAX_INFLATE);
                out.resize(n, 0);
            }
            _ => break,
        }
    }
    out.truncate(outp);
    out
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

/// Undoes a PNG (10..15) or TIFF (2) predictor.
fn predict(data: Vec<u8>, parms: Option<&Dict>) -> Vec<u8> {
    let Some(p) = parms else { return data };
    let get = |k: &str, def: i64| key(p, k).and_then(Object::as_i64).unwrap_or(def);
    let predictor = get("Predictor", 1);
    if predictor < 2 {
        return data;
    }
    let colors = get("Colors", 1).clamp(1, 32) as usize;
    let bpc = get("BitsPerComponent", 8).clamp(1, 16) as usize;
    let columns = get("Columns", 1).max(1) as usize;
    let bpp = (colors * bpc).div_ceil(8);
    let row = (colors * bpc * columns).div_ceil(8);
    if predictor == 2 {
        let mut out = data;
        if bpc == 8 {
            for r in out.chunks_mut(row) {
                for i in bpp..r.len() {
                    r[i] = r[i].wrapping_add(r[i - bpp]);
                }
            }
        } else if bpc == 16 {
            for r in out.chunks_mut(row) {
                for i in (2 * colors..r.len().saturating_sub(1)).step_by(2) {
                    let prev = u16::from_be_bytes([r[i - 2 * colors], r[i + 1 - 2 * colors]]);
                    let cur = u16::from_be_bytes([r[i], r[i + 1]]).wrapping_add(prev);
                    r[i..i + 2].copy_from_slice(&cur.to_be_bytes());
                }
            }
        } else {
            // Sub-byte components: add sample by sample.
            for r in out.chunks_mut(row) {
                let n = columns * colors;
                let mask = (1u32 << bpc) - 1;
                let get = |r: &[u8], i: usize| -> u32 {
                    let bit = i * bpc;
                    ((r[bit / 8] as u32) >> (8 - bpc - bit % 8)) & mask
                };
                for i in colors..n {
                    if (i * bpc + bpc).div_ceil(8) > r.len() {
                        break;
                    }
                    let v = (get(r, i) + get(r, i - colors)) & mask;
                    let bit = i * bpc;
                    let shift = 8 - bpc - bit % 8;
                    r[bit / 8] = (r[bit / 8] & !((mask as u8) << shift)) | ((v as u8) << shift);
                }
            }
        }
        return out;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.is_empty() {
            break;
        }
        let kind = chunk[0];
        let mut cur = chunk[1..].to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match kind {
                1 => cur[i].wrapping_add(a),
                2 => cur[i].wrapping_add(b),
                3 => cur[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => cur[i].wrapping_add(paeth(a, b, c)),
                _ => cur[i],
            };
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

pub fn lzw(data: &[u8], early: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = (0..=255u16).map(|b| vec![b as u8]).collect();
    table.push(Vec::new());
    table.push(Vec::new());
    let mut width = 9u32;
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut prev: Option<Vec<u8>> = None;
    for &b in data {
        acc = (acc << 8) | b as u32;
        nbits += 8;
        while nbits >= width {
            let code = (acc >> (nbits - width)) & ((1 << width) - 1);
            nbits -= width;
            match code {
                256 => {
                    table.truncate(258);
                    width = 9;
                    prev = None;
                    continue;
                }
                257 => return out,
                _ => {}
            }
            let entry = if (code as usize) < table.len() {
                table[code as usize].clone()
            } else if let Some(p) = &prev {
                let mut e = p.clone();
                e.push(p[0]);
                e
            } else {
                return out;
            };
            out.extend_from_slice(&entry);
            if let Some(p) = prev.take() {
                if table.len() < 4096 {
                    let mut n = p;
                    n.push(entry[0]);
                    table.push(n);
                }
            }
            prev = Some(entry);
            let next = table.len() as u32 + early as u32;
            width = if next >= 2048 {
                12
            } else if next >= 1024 {
                11
            } else if next >= 512 {
                10
            } else {
                9
            };
        }
    }
    out
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &c in data {
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            b'>' => break,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | v),
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0;
    let body = data.strip_prefix(b"<~").unwrap_or(data);
    for &c in body {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = (c - b'!') as u32;
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    out
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let l = data[i];
        i += 1;
        match l {
            128 => break,
            0..=127 => {
                let n = l as usize + 1;
                out.extend_from_slice(&data[i..(i + n).min(data.len())]);
                i += n;
            }
            _ => {
                if let Some(&b) = data.get(i) {
                    out.extend(core::iter::repeat_n(b, 257 - l as usize));
                }
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_filters() {
        assert_eq!(ascii_hex(b"48 65 6C6c6F>"), b"Hello");
        assert_eq!(ascii85(b"<~87cURD]i,\"Ebo80~>"), b"Hello World!");
        assert_eq!(run_length(&[2, b'a', b'b', b'c', 254, b'x', 128]), b"abcxxx");
        // "-----A---B" in LZW from the PDF reference (7.4.4.2).
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&enc, true), b"-----A---B");
    }

    #[test]
    fn png_predictor() {
        let mut p = Dict::new();
        p.insert(b"Predictor".to_vec(), Object::Int(12));
        p.insert(b"Columns".to_vec(), Object::Int(3));
        // Row 1: "Up" on zeros; row 2: Up adds the row above.
        let data = vec![2, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(predict(data, Some(&p)), [1, 2, 3, 2, 3, 4]);
    }
}
