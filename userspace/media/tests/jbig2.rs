//! The JBIG2 decoder against SerenityOS's test streams (BSD-2-Clause, see
//! `tests/data/jbig2/README`): every `bitmap-*.jbig2` encodes the same
//! 399 x 400 picture as `bitmap.bmp` with a different set of coding tools,
//! and `annex-h.jbig2` is the T.88 Annex H example.

mod common;

use osyx_media::jbig2::{self, Bitmap};

fn dir() -> String {
    format!("{}/tests/data/jbig2", env!("CARGO_MANIFEST_DIR"))
}

/// The 1-bit BMP, black as 1.
fn reference() -> Bitmap {
    let d = std::fs::read(format!("{}/bitmap.bmp", dir())).unwrap();
    let off = u32::from_le_bytes(d[10..14].try_into().unwrap()) as usize;
    let w = i32::from_le_bytes(d[18..22].try_into().unwrap()) as usize;
    let h = i32::from_le_bytes(d[22..26].try_into().unwrap()) as usize;
    // Palette entry 0 is white.
    assert_eq!(&d[54..58], &[0xFF, 0xFF, 0xFF, 0]);
    let stride = w.div_ceil(32) * 4;
    let mut px = vec![0; w * h];
    for y in 0..h {
        let row = &d[off + (h - 1 - y) * stride..];
        for x in 0..w {
            px[y * w + x] = (row[x / 8] >> (7 - x % 8)) & 1;
        }
    }
    Bitmap { w, h, px }
}

#[test]
fn every_coding_tool_decodes_the_reference_picture() {
    let want = reference();
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("bitmap") && n.ends_with(".jbig2"))
        .collect();
    names.sort();
    assert!(names.len() > 100);
    let mut failed = Vec::new();
    for name in &names {
        let data = std::fs::read(format!("{}/{name}", dir())).unwrap();
        match jbig2::decode(&data, None, false) {
            Ok(pages) if pages.len() == 1 && pages[0] == want => {}
            Ok(pages) => {
                let p = &pages[0];
                let diff = if (p.w, p.h) == (want.w, want.h) {
                    p.px.iter().zip(&want.px).filter(|(a, b)| a != b).count()
                } else {
                    usize::MAX
                };
                failed.push(format!("{name}: {} pages, {}x{}, {diff} pixels differ", pages.len(), p.w, p.h));
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    assert!(failed.is_empty(), "{} of {} failed:\n{}", failed.len(), names.len(), failed.join("\n"));
}

#[test]
fn annex_h_example() {
    let data = std::fs::read(format!("{}/annex-h.jbig2", dir())).unwrap();
    let pages = jbig2::decode(&data, None, false).unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!((pages[0].w, pages[0].h), (64, 56));
    assert_eq!(pages[0], pages[1]);
    assert_eq!((pages[2].w, pages[2].h), (37, 8));
    for y in 0..8 {
        for x in 0..37 {
            assert_eq!(pages[2].px[y * 37 + x], pages[1].px[(y + 1) * 64 + x + 4], "({x}, {y})");
        }
    }
}

/// Splits a sequential JBIG2 file into its segments (header and data).
fn segments(file: &[u8]) -> Vec<(u8, u32, &[u8])> {
    let mut p = if file[8] & 2 != 0 { 9 } else { 13 };
    let mut out = Vec::new();
    while p < file.len() {
        let start = p;
        let number = u32::from_be_bytes(file[p..p + 4].try_into().unwrap());
        let flags = file[p + 4];
        let refs = (file[p + 5] >> 5) as usize;
        assert!(refs <= 4);
        let size = if number <= 256 {
            1
        } else if number <= 65536 {
            2
        } else {
            4
        };
        let mut q = p + 6 + refs * size;
        let page = if flags & 0x40 != 0 {
            q += 4;
            u32::from_be_bytes(file[q - 4..q].try_into().unwrap())
        } else {
            q += 1;
            file[q - 1] as u32
        };
        let len = u32::from_be_bytes(file[q..q + 4].try_into().unwrap()) as usize;
        p = q + 4 + len;
        out.push((flags & 0x3F, page, &file[start..p]));
    }
    out
}

#[test]
fn pdf_jbig2decode_with_globals() {
    let file = std::fs::read(format!("{}/bitmap-symbol-global.jbig2", dir())).unwrap();
    let mut globals = Vec::new();
    let mut page = Vec::new();
    for (kind, page_no, bytes) in segments(&file) {
        // End of page and end of file are left out of embedded streams.
        if kind == 49 || kind == 51 {
            continue;
        }
        if page_no == 0 { &mut globals } else { &mut page }.extend_from_slice(bytes);
    }
    assert!(!globals.is_empty());
    let pdf = osyx_media::pdf::Pdf::open(common::pdf_with_image(
        399,
        400,
        "/Width 399 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode          /DecodeParms << /JBIG2Globals 6 0 R >>",
        &page,
        &[&globals],
    ))
    .unwrap();
    let bmp = pdf.render(0, 1.0, &|_: &str| None).unwrap();
    let want = reference();
    assert_eq!((bmp.w, bmp.h), (want.w, want.h));
    let off = bmp.px.iter().zip(&want.px).filter(|(rgb, &black)| (rgb[0] < 128) != (black == 1)).count();
    // Resampling may move a few edge pixels.
    assert!(off < want.w * want.h / 200, "{off} pixels differ");
}

#[test]
fn jbig2_files_open_as_pictures() {
    let data = std::fs::read(format!("{}/bitmap.jbig2", dir())).unwrap();
    let pic = osyx_media::image::decode(&data).unwrap();
    let want = reference();
    assert_eq!((pic.w, pic.h, pic.format.as_str()), (want.w, want.h, "JBIG2"));
    for (p, &b) in pic.frames[0].px.iter().zip(&want.px) {
        assert_eq!(p[0] == 0, b == 1);
    }
}
