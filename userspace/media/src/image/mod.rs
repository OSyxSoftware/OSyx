//! Pictures for the image viewer built into Files: PNG, JPEG, GIF
//! (animated), BMP, ICO and JBIG2, decoded to straight-alpha RGBA frames.

mod bmp;
mod gif;
mod png;

use alloc::string::String;
use alloc::vec::Vec;

/// Largest picture accepted, in pixels per frame.
pub const MAX_PIXELS: usize = 64 << 20;
/// Most frames kept from an animation.
const MAX_FRAMES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// RGBA, straight alpha, row by row.
    pub px: Vec<[u8; 4]>,
    /// How long to show the frame (0 for a still picture).
    pub delay_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub frames: Vec<Frame>,
    /// "PNG, 8-bit RGBA", for display.
    pub format: String,
}

pub(crate) fn check_size(w: usize, h: usize) -> Result<usize, String> {
    match w.checked_mul(h) {
        Some(n) if n > 0 && n <= MAX_PIXELS => Ok(n),
        _ => Err(alloc::format!("a {w} x {h} picture is too large")),
    }
}

/// Decodes the picture in `data`, judged by its first bytes.
pub fn decode(data: &[u8]) -> Result<Picture, String> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        png::decode(data)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        jpeg(data)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        gif::decode(data)
    } else if data.starts_with(b"BM") {
        bmp::decode(data, false)
    } else if data.len() >= 6 && data[..4] == [0, 0, 1, 0] {
        ico(data)
    } else if data.starts_with(&[0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A]) {
        jbig2(data)
    } else {
        Err(String::from("not a picture OSyx can show (PNG, JPEG, GIF, BMP, ICO and JBIG2 are supported)"))
    }
}

/// The first page of a JBIG2 file, black on white.
fn jbig2(data: &[u8]) -> Result<Picture, String> {
    let pages = crate::jbig2::decode(data, None, false).map_err(|e| alloc::format!("damaged JBIG2: {e}"))?;
    let page = pages.first().ok_or("JBIG2 file without pages")?;
    check_size(page.w, page.h)?;
    let px = page.px.iter().map(|&b| if b != 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
    let format = match pages.len() {
        1 => String::from("JBIG2"),
        n => alloc::format!("JBIG2, page 1 of {n}"),
    };
    Ok(Picture { w: page.w, h: page.h, frames: alloc::vec![Frame { px, delay_ms: 0 }], format })
}

fn jpeg(data: &[u8]) -> Result<Picture, String> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_strict_mode(false)
        .set_max_width(1 << 16)
        .set_max_height(1 << 16);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    d.decode_headers().map_err(|e| alloc::format!("damaged JPEG: {e:?}"))?;
    let info = d.info().ok_or("damaged JPEG")?;
    let (w, h) = (info.width as usize, info.height as usize);
    check_size(w, h)?;
    let raw = d.decode().map_err(|e| alloc::format!("damaged JPEG: {e:?}"))?;
    if raw.len() < w * h * 4 {
        return Err(String::from("damaged JPEG"));
    }
    let px = raw.chunks_exact(4).take(w * h).map(|c| [c[0], c[1], c[2], 255]).collect();
    Ok(Picture {
        w,
        h,
        frames: alloc::vec![Frame { px, delay_ms: 0 }],
        format: alloc::format!("JPEG, {} components", info.components),
    })
}

/// Windows icons: the largest image (BMP without its file header, or PNG).
fn ico(data: &[u8]) -> Result<Picture, String> {
    let n = u16::from_le_bytes([data[4], data[5]]) as usize;
    let mut best: Option<(usize, usize, usize)> = None;
    for i in 0..n {
        let e = 6 + 16 * i;
        let Some(d) = data.get(e..e + 16) else { break };
        let w = if d[0] == 0 { 256 } else { d[0] as usize };
        let size = u32::from_le_bytes([d[8], d[9], d[10], d[11]]) as usize;
        let off = u32::from_le_bytes([d[12], d[13], d[14], d[15]]) as usize;
        if off.checked_add(size).is_some_and(|end| end <= data.len()) && best.is_none_or(|b| w > b.0) {
            best = Some((w, off, size));
        }
    }
    let (_, off, size) = best.ok_or("an icon without images")?;
    let img = &data[off..off + size];
    let mut p = if img.starts_with(b"\x89PNG") { png::decode(img)? } else { bmp::decode(img, true)? };
    p.format = alloc::format!("ICO ({})", p.format);
    Ok(p)
}
