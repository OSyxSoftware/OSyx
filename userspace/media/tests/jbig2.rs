//! The JBIG2 decoder against SerenityOS's test streams (BSD-2-Clause, see
//! `tests/data/jbig2/README`): every `bitmap-*.jbig2` encodes the same
//! 399 x 400 picture as `bitmap.bmp` with a different set of coding tools,
//! and `annex-h.jbig2` is the T.88 Annex H example.

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
