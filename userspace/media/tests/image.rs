//! The picture decoders against Pillow's decodings of the same files
//! (`make_image_fixtures.py`): exact for lossless formats, within a few
//! steps for JPEG.

use osyx_media::image;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/image/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Pillow's first frame: width, height, frame count, RGBA.
fn reference(name: &str) -> (usize, usize, usize, Vec<u8>) {
    let gz = data(&format!("{name}.ref.gz"));
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let n = |i: usize| u32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().unwrap()) as usize;
    (n(0), n(1), n(2), raw[12..].to_vec())
}

fn check(name: &str, tolerance: u8) {
    let pic = image::decode(&data(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let (w, h, frames, want) = reference(name);
    assert_eq!((pic.w, pic.h), (w, h), "{name}: size");
    assert_eq!(pic.frames.len(), frames, "{name}: frames");
    let got: Vec<u8> = pic.frames[0].px.iter().flatten().copied().collect();
    let mut worst = 0u8;
    for (i, (a, b)) in got.iter().zip(&want).enumerate() {
        // Fully transparent pixels may differ in colour.
        if i % 4 != 3 && got[i - i % 4 + 3] == 0 && want[i - i % 4 + 3] == 0 {
            continue;
        }
        worst = worst.max(a.abs_diff(*b));
    }
    assert!(worst <= tolerance, "{name}: off by up to {worst} ({})", pic.format);
}

#[test]
fn lossless_formats_match_exactly() {
    for name in [
        "rgba8.png",
        "rgb8.png",
        "gray8.png",
        "graya8.png",
        "gray1.png",
        "pal4.png",
        "pal-trns.png",
        "rgba16-adam7.png",
        "gray16.png",
        "rgb8-adam7.png",
        "rgb24.bmp",
        "pal8.bmp",
        "mono.bmp",
        "icon-png.ico",
        "icon-bmp.ico",
        "anim.gif",
        "interlaced.gif",
    ] {
        check(name, 0);
    }
}

#[test]
fn jpeg_is_close() {
    check("photo.jpg", 3);
    check("gray.jpg", 3);
}

#[test]
fn gif_animation_has_its_delays() {
    let pic = image::decode(&data("anim.gif")).unwrap();
    let delays: Vec<u32> = pic.frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, [100, 200, 300, 400]);
    assert_ne!(pic.frames[0].px, pic.frames[1].px);
}

#[test]
fn bmp_rle8() {
    // A 4x2 RLE8 bitmap: row 0 (bottom) = 1 1 2 2, row 1 = absolute 3 4 5 + run 1x6.
    let mut f = Vec::new();
    let pixels: &[u8] = &[2, 1, 2, 2, 0, 0, 0, 3, 3, 4, 5, 0, 1, 6, 0, 1];
    let palette: Vec<u8> = (0..8u8).flat_map(|i| [i * 30, i * 20, i * 10, 0]).collect();
    let off = 14 + 40 + palette.len();
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&((off + pixels.len()) as u32).to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&(off as u32).to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&4i32.to_le_bytes());
    f.extend_from_slice(&2i32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&8u16.to_le_bytes());
    f.extend_from_slice(&1u32.to_le_bytes());
    f.extend_from_slice(&[0; 12]);
    f.extend_from_slice(&8u32.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&palette);
    f.extend_from_slice(pixels);
    let pic = image::decode(&f).unwrap();
    let idx = |k: u8| [k * 10, k * 20, k * 30, 255];
    let px = &pic.frames[0].px;
    // Top row is the second row of the file (bottom-up).
    assert_eq!(&px[..4], &[idx(3), idx(4), idx(5), idx(6)]);
    assert_eq!(&px[4..], &[idx(1), idx(1), idx(2), idx(2)]);
}

#[test]
fn damaged_or_huge_pictures_fail_cleanly() {
    let mut png = data("rgba8.png");
    let n = png.len();
    png[n / 2] ^= 0xFF;
    assert!(image::decode(&png).is_err());
    // A GIF claiming 65535 x 65535.
    let mut gif = data("interlaced.gif");
    gif[6..10].copy_from_slice(&[0xFF; 4]);
    assert!(image::decode(&gif).is_err());
    assert!(image::decode(b"not a picture").is_err());
}

#[test]
fn tiff_files_match_their_references() {
    for name in [
        "rgb8.tif",
        "rgb8-lzw.tif",
        "rgb8-packbits.tif",
        "rgba8-deflate.tif",
        "grey8.tif",
        "pal4-lzw.tif",
        "mono-g4.tif",
        "mono-g3.tif",
        "mono.tif",
        "mono-miniswhite.tif",
        "mono-lsb.tif",
        "rgb8-be-tiles.tif",
        "rgb8-planar-pred.tif",
        "rgb8-pred-strips.tif",
        "rgba8-tiles-lzw.tif",
        "rgb16-be-pred.tif",
        "rgb16-le-pred.tif",
        "grey16-be.tif",
        "grey4.tif",
        "rgba8-assoc.tif",
        "rgb8-rotated.tif",
        "pages.tif",
    ] {
        check(name, 0);
    }
    // Pillow rounds the CMYK inks its own way.
    check("cmyk8-lzw.tif", 1);
}

#[test]
fn tiff_formats_are_described_and_unsupported_ones_named() {
    let format = |name: &str| image::decode(&data(name)).unwrap().format;
    assert_eq!(format("rgb8-lzw.tif"), "TIFF, 8-bit RGB, LZW");
    assert_eq!(format("mono-g4.tif"), "TIFF, 1-bit bilevel, CCITT Group 4");
    assert_eq!(format("rgba8-assoc.tif"), "TIFF, 8-bit RGB with alpha, uncompressed");
    assert_eq!(format("pages.tif"), "TIFF, 8-bit RGB, uncompressed, page 1 of 2");

    // The compression field (tag 259, a SHORT in the entry) set to JPEG.
    let mut jpeg = data("rgb8.tif");
    let ifd = u32::from_le_bytes(jpeg[4..8].try_into().unwrap()) as usize;
    let entries = u16::from_le_bytes(jpeg[ifd..ifd + 2].try_into().unwrap()) as usize;
    let entry = (0..entries).map(|i| ifd + 2 + 12 * i).find(|&e| jpeg[e..e + 2] == 259u16.to_le_bytes()).unwrap();
    jpeg[entry + 8] = 7;
    assert_eq!(image::decode(&jpeg).unwrap_err(), "JPEG-compressed TIFF pictures are not supported");
    assert_eq!(image::decode(b"II+\0\x08\0\0\0").unwrap_err(), "BigTIFF pictures are not supported");
}

#[test]
fn damaged_tiff_files_do_not_panic() {
    for name in ["rgb8-lzw.tif", "mono-g4.tif", "rgb8-be-tiles.tif", "rgb8-planar-pred.tif", "rgb16-le-pred.tif", "pal4-lzw.tif"] {
        let good = data(name);
        // Every prefix, and every byte of the header and directory changed.
        for cut in (0..good.len()).step_by(7) {
            let _ = image::decode(&good[..cut]);
        }
        for i in 0..good.len() {
            for bits in [0x01u8, 0x80, 0xFF] {
                let mut bad = good.clone();
                bad[i] ^= bits;
                if let Ok(p) = image::decode(&bad) {
                    assert_eq!(p.frames[0].px.len(), p.w * p.h, "{name}: byte {i}");
                }
            }
        }
    }
}
