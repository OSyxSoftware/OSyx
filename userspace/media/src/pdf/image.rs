//! Image XObjects and inline images (ISO 32000-1, 8.9): samples of 1 to 16
//! bits in any colour space with Decode arrays, JPEG (DCTDecode), stencil
//! masks, explicit and colour key masks, and soft masks.

use super::color::{self, Space};
use super::document::Document;
use super::filter;
use super::object::{key, Dict, Object};
#[allow(unused_imports)]
use super::Float;
use alloc::vec;
use alloc::vec::Vec;

pub struct Image {
    pub w: usize,
    pub h: usize,
    /// RGBA, straight alpha.
    pub px: Vec<[u8; 4]>,
    /// A stencil mask: only the alpha matters, painted in the fill colour.
    pub stencil: bool,
}

fn get<'a>(d: &'a Dict, long: &str, short: &str) -> Option<&'a Object> {
    key(d, long).or_else(|| key(d, short))
}

/// Inline image abbreviations for colour space names.
fn expand_space(o: &Object) -> Object {
    match o.as_name() {
        Some(b"G") => Object::Name(b"DeviceGray".to_vec()),
        Some(b"RGB") => Object::Name(b"DeviceRGB".to_vec()),
        Some(b"CMYK") => Object::Name(b"DeviceCMYK".to_vec()),
        Some(b"I") => Object::Name(b"Indexed".to_vec()),
        _ => o.clone(),
    }
}

/// Decodes the JPEG in `data` to (w, h, components, samples).
fn jpeg(data: &[u8], gray: bool) -> Option<(usize, usize, usize, Vec<u8>)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let out = if gray { ColorSpace::Luma } else { ColorSpace::RGB };
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(out)
        .set_strict_mode(false)
        .set_max_width(1 << 15)
        .set_max_height(1 << 15);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let px = d.decode().ok()?;
    let info = d.info()?;
    let comps = if gray { 1 } else { 3 };
    let (w, h) = (info.width as usize, info.height as usize);
    (px.len() >= w * h * comps).then_some((w, h, comps, px))
}

/// Unpacks `n` components of `bpc` bits per sample, rows padded to bytes.
fn unpack(data: &[u8], w: usize, h: usize, n: usize, bpc: u32) -> Vec<u32> {
    let row_bits = w * n * bpc as usize;
    let row_bytes = row_bits.div_ceil(8);
    let mut out = Vec::with_capacity(w * h * n);
    for y in 0..h {
        let row = data.get(y * row_bytes..(y + 1) * row_bytes);
        for i in 0..w * n {
            let v = match (row, bpc) {
                (None, _) => 0,
                (Some(r), 8) => *r.get(i).unwrap_or(&0) as u32,
                (Some(r), 16) => {
                    u16::from_be_bytes([*r.get(2 * i).unwrap_or(&0), *r.get(2 * i + 1).unwrap_or(&0)]) as u32
                }
                (Some(r), b) => {
                    let bit = i * b as usize;
                    let byte = *r.get(bit / 8).unwrap_or(&0) as u32;
                    (byte >> (8 - b as usize - bit % 8)) & ((1 << b) - 1)
                }
            };
            out.push(v);
        }
    }
    out
}

/// The alpha of a 1-bit (or n-bit) mask image, resampled to `w` by `h`.
/// `paint_zero`: sample 0 means opaque (stencil semantics).
fn mask_alpha(doc: &Document, m: &Object, w: usize, h: usize, soft: bool) -> Option<Vec<u8>> {
    let Object::Stream(s) = doc.resolve(m) else { return None };
    let d = &s.dict;
    let mw = doc.get_key(d, "Width").as_i64()?.max(1) as usize;
    let mh = doc.get_key(d, "Height").as_i64()?.max(1) as usize;
    let (data, codec) = filter::decode(&s.data, d);
    let bpc = if soft { doc.get_key(d, "BitsPerComponent").as_i64().unwrap_or(8) as u32 } else { 1 };
    let vals: Vec<u8> = match codec {
        Some((name, _)) if name == b"DCTDecode" => jpeg(&data, true)?.3,
        Some(_) => return None,
        None => {
            let max = ((1u64 << bpc) - 1) as u32;
            unpack(&data, mw, mh, 1, bpc).into_iter().map(|v| (v * 255 / max.max(1)) as u8).collect()
        }
    };
    let decode: Vec<f64> =
        doc.get_key(d, "Decode").as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()).unwrap_or_default();
    let inverted = decode.first().copied() == Some(1.0);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let my = (y * mh / h).min(mh - 1);
        for x in 0..w {
            let mx = (x * mw / w).min(mw - 1);
            let v = *vals.get(my * mw + mx).unwrap_or(&0);
            out[y * w + x] = if soft {
                if inverted {
                    255 - v
                } else {
                    v
                }
            } else {
                // Explicit masks: 1 masks out (with the default Decode).
                let masked = (v >= 128) != inverted;
                if masked {
                    0
                } else {
                    255
                }
            };
        }
    }
    Some(out)
}

/// Decodes an image. `d` is its dictionary (inline abbreviations allowed),
/// `raw` its still encoded data, `resources_space` resolves named colour
/// spaces.
pub fn decode(doc: &Document, d: &Dict, raw: &[u8], resolve_space: &dyn Fn(&Object) -> Option<Space>) -> Option<Image> {
    let w = get(d, "Width", "W").map(|o| doc.resolve(o)).and_then(|o| o.as_i64())?.max(1) as usize;
    let h = get(d, "Height", "H").map(|o| doc.resolve(o)).and_then(|o| o.as_i64())?.max(1) as usize;
    if w * h > 40_000_000 {
        return None;
    }
    let is_mask = matches!(get(d, "ImageMask", "IM").map(|o| doc.resolve(o)), Some(Object::Bool(true)));
    let bpc = if is_mask {
        1
    } else {
        get(d, "BitsPerComponent", "BPC").map(|o| doc.resolve(o)).and_then(|o| o.as_i64()).unwrap_or(8) as u32
    };
    let decode_arr: Vec<f64> = get(d, "Decode", "D")
        .map(|o| doc.resolve(o))
        .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
        .unwrap_or_default();
    // Inline images spell their filters with abbreviations too.
    let mut fd = d.clone();
    if let Some(f) = key(d, "F") {
        fd.insert(b"Filter".to_vec(), f.clone());
    }
    if let Some(p) = key(d, "DP") {
        fd.insert(b"DecodeParms".to_vec(), p.clone());
    }
    let (data, codec) = filter::decode(raw, &fd);
    if is_mask {
        let inverted = decode_arr.first().copied() == Some(1.0);
        let bits = unpack(&data, w, h, 1, 1);
        let px = bits.into_iter().map(|b| [0, 0, 0, if (b == 0) != inverted { 255 } else { 0 }]).collect();
        return Some(Image { w, h, px, stencil: true });
    }
    let cs_obj = get(d, "ColorSpace", "CS").map(|o| expand_space(&doc.resolve(o)));
    let space = cs_obj.as_ref().and_then(|o| color::space(doc, o, 0).or_else(|| resolve_space(o)));
    let mut px: Vec<[u8; 4]> = match codec {
        Some((name, _)) if name == b"DCTDecode" => {
            let gray = space.as_ref().is_some_and(|s| s.components() == 1);
            let (jw, jh, comps, s) = jpeg(&data, gray)?;
            let inverted = decode_arr.first().copied() == Some(1.0) && decode_arr.get(1).copied() == Some(0.0);
            let mut px = Vec::with_capacity(w * h);
            for y in 0..h {
                for x in 0..w {
                    let (sx, sy) = ((x * jw / w).min(jw - 1), (y * jh / h).min(jh - 1));
                    let i = (sy * jw + sx) * comps;
                    let mut c = if comps == 1 { [s[i], s[i], s[i]] } else { [s[i], s[i + 1], s[i + 2]] };
                    if inverted {
                        c = [255 - c[0], 255 - c[1], 255 - c[2]];
                    }
                    px.push([c[0], c[1], c[2], 255]);
                }
            }
            px
        }
        Some(_) => return None,
        None => {
            let space = space.unwrap_or(Space::Gray);
            let n = space.components().max(1);
            let max = ((1u64 << bpc) - 1) as f64;
            let dec = if decode_arr.len() >= 2 * n { decode_arr.clone() } else { space.default_decode(bpc) };
            let default_decode = dec == space.default_decode(bpc);
            let samples = unpack(&data, w, h, n, bpc);
            match (&space, default_decode && bpc == 8) {
                // Common cases without per-sample conversions.
                (Space::Gray, true) => samples.iter().map(|&v| [v as u8, v as u8, v as u8, 255]).collect(),
                (Space::Rgb, true) => {
                    samples.chunks_exact(3).map(|c| [c[0] as u8, c[1] as u8, c[2] as u8, 255]).collect()
                }
                (Space::Cmyk, true) => samples
                    .chunks_exact(4)
                    .map(|c| {
                        let k = 255 - c[3];
                        let f = |v: u32| ((255 - v) * k / 255) as u8;
                        [f(c[0]), f(c[1]), f(c[2]), 255]
                    })
                    .collect(),
                _ if n == 1 && bpc <= 8 => {
                    // One component of at most 8 bits: a table of every value.
                    let table: Vec<[u8; 3]> = (0..=max as u32)
                        .map(|v| space.to_rgb(&[dec[0] + v as f64 * (dec[1] - dec[0]) / max]))
                        .collect();
                    samples
                        .iter()
                        .map(|&v| {
                            let c = table[(v as usize).min(table.len() - 1)];
                            [c[0], c[1], c[2], 255]
                        })
                        .collect()
                }
                _ => {
                    let mut comps = vec![0.0; n];
                    samples
                        .chunks_exact(n)
                        .map(|s| {
                            for k in 0..n {
                                comps[k] = dec[2 * k] + s[k] as f64 * (dec[2 * k + 1] - dec[2 * k]) / max;
                            }
                            let c = space.to_rgb(&comps);
                            [c[0], c[1], c[2], 255]
                        })
                        .collect()
                }
            }
        }
    };
    px.resize(w * h, [0, 0, 0, 255]);
    // Masks.
    if let Some(sm) = key(d, "SMask") {
        if let Some(a) = mask_alpha(doc, sm, w, h, true) {
            for (p, a) in px.iter_mut().zip(a) {
                p[3] = a;
            }
        }
    } else if let Some(m) = key(d, "Mask") {
        match doc.resolve(m) {
            Object::Array(ranges) => {
                // Colour key masking on the raw samples.
                let r: Vec<u32> =
                    ranges.iter().filter_map(|o| doc.resolve(o).as_i64()).map(|v| v.max(0) as u32).collect();
                if codec_is_raw(d) {
                    let n = r.len() / 2;
                    if n > 0 {
                        let samples = unpack(&data, w, h, n, bpc);
                        for (i, s) in samples.chunks_exact(n).enumerate() {
                            if s.iter().enumerate().all(|(k, &v)| v >= r[2 * k] && v <= r[2 * k + 1]) {
                                px[i][3] = 0;
                            }
                        }
                    }
                }
            }
            other => {
                if let Some(a) = mask_alpha(doc, &other, w, h, false) {
                    for (p, a) in px.iter_mut().zip(a) {
                        p[3] = a;
                    }
                }
            }
        }
    }
    Some(Image { w, h, px, stencil: false })
}

fn codec_is_raw(d: &Dict) -> bool {
    !filter::filters(d).iter().any(|(n, _)| filter::IMAGE_FILTERS.contains(&n.as_slice()))
}
