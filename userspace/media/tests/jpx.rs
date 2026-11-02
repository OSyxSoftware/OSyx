//! The JPEG 2000 decoder against SerenityOS's test files (BSD-2-Clause,
//! see `tests/data/jpx/README`) and their PNG references.

use osyx_media::{image, jpx};

fn dir() -> String {
    format!("{}/tests/data/jpx", env!("CARGO_MANIFEST_DIR"))
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/{name}", dir())).unwrap()
}

fn png(name: &str) -> Vec<[u8; 4]> {
    image::decode(&read(name)).unwrap().frames.remove(0).px
}

/// Decodes `name` and returns the largest channel difference to `want`
/// after `map`.
fn worst(name: &str, want: &[[u8; 4]], map: impl Fn(u8) -> u8) -> Result<u8, String> {
    let img = jpx::decode(&read(name)).map_err(|e| format!("{name}: {e}"))?;
    if (img.w, img.h) != (119, 101) {
        return Err(format!("{name}: {}x{}", img.w, img.h));
    }
    let got = img.to_rgba();
    let mut worst = 0;
    for (a, b) in got.iter().zip(want) {
        for k in 0..4 {
            worst = worst.max(a[k].abs_diff(map(b[k])));
        }
    }
    Ok(worst)
}

fn check_all(names: &[&str], reference: &str, tolerance: u8, map: impl Fn(u8) -> u8 + Copy) {
    let want = png(reference);
    let mut failed = Vec::new();
    for name in names {
        match worst(name, &want, map) {
            Ok(w) if w <= tolerance => {}
            Ok(w) => failed.push(format!("{name}: off by {w}")),
            Err(e) => failed.push(e),
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn lossless_rgba() {
    check_all(
        &[
            "kakadu-lossless-rgba-u8-prog1-layers1-res6-mct.jp2",
            "openjpeg-lossless-rgba-u8-prog0-tile4x2-cblk4x16-tp3-layers3-res2.jp2",
            "openjpeg-lossless-rgba-u8-prog1-tile4x2-cblk4x16-tp3-layers3-res2.jp2",
            "openjpeg-lossless-rgba-u8-prog2-tile4x2-cblk4x16-tp3-layers3-res2.jp2",
            "openjpeg-lossless-rgba-u8-prog3-tile4x2-cblk4x16-tp3-layers3-res2.jp2",
            "openjpeg-lossless-rgba-u8-prog4-tile4x2-cblk4x16-tp3-layers3-res2.jp2",
            "jasper-rgba-u8-cbstyle-01-bypass.jp2",
            "jasper-rgba-u8-cbstyle-01-bypass-layers.jp2",
            "jasper-rgba-u8-cbstyle-01-bypass-finer-layers.jp2",
            "jasper-rgba-u8-cbstyle-02-resetprob.jp2",
            "jasper-rgba-u8-cbstyle-04-termall.jp2",
            "jasper-rgba-u8-cbstyle-05-bypass-termall.jp2",
            "jasper-rgba-u8-cbstyle-04-termall-layers.jp2",
            "jasper-rgba-u8-cbstyle-06-resetprob-termall.jp2",
            "jasper-rgba-u8-cbstyle-08-vcausal.jp2",
            "jasper-rgba-u8-cbstyle-16-pterm.jp2",
            "jasper-rgba-u8-cbstyle-32-segsym.jp2",
            "jasper-rgba-u8-cbstyle-36-termall-segsym.jp2",
            "jasper-rgba-u8-cbstyle-59-all-but-termall.jp2",
            "jasper-rgba-u8-cbstyle-63-all.jp2",
            "jasper-tile4x2-res5.jp2",
            "openjpeg-lossless-rgba-u8-prog0-SOP.jp2",
            "openjpeg-lossless-rgba-u8-prog0-EPH.jp2",
            "openjpeg-lossless-rgba-u8-prog0-EPH-SOP.jp2",
            "openjpeg-lossless-rgba-u8-prog0-EPH-empty-packets.jp2",
            "openjpeg-lossless-rgba-u8-PLT.jp2",
            "openjpeg-lossless-rgba-u8-TLM.jp2",
            "kakadu-lossless-rgba-u16-prog1-layers1-res6.jp2",
            // Files SerenityOS turns down.
            "openjpeg-lossless-RGN.jp2",
            "openjpeg-lossless-bgra-u8.jp2",
            "openjpeg-lossless-rgba-u8-prog0-tile-part-index-overflow.jp2",
        ],
        "ref.png",
        0,
        |v| v,
    );
}

#[test]
fn lossless_four_bits() {
    check_all(&["openjpeg-lossless-rgba-u4.jp2"], "ref.png", 0, |v| ((v as f32 / 17.0).round() as u8) * 17);
}

#[test]
fn lossless_other_layouts() {
    check_all(
        &["kakadu-lossless-rgb-u8-prog1-layers1-res6-mct.jp2", "jasper-rgba-u8-solid-alpha-cbstyle-04-termall.jp2"],
        "ref-rgb.png",
        0,
        |v| v,
    );
    check_all(&["kakadu-lossless-gray-alpha-u8-prog1-layers1-res6.jp2"], "ref-gray-alpha.png", 0, |v| v);
    check_all(
        &["kakadu-lossless-gray-u8-prog1-layers1-res6.jp2", "kakadu-lossless-gray-u8-prog1-layers1-res6-icc.jp2"],
        "ref-gray.png",
        0,
        |v| v,
    );
    check_all(&["openjpeg-lossless-indexed-u8-rgb-u8.jp2"], "ref-indexed.png", 0, |v| v);
}

/// CIELab converted to sRGB, against Photoshop's conversion.
#[test]
fn cielab() {
    check_all(&["kakadu-lossless-lab-alpha-u8-prog1-layers1-res6.jp2"], "ref.png", 9, |v| v);
}

/// CMYK channels as stored, against the TIFF the file was made from
/// (Pillow's decoding, `ref-cmyk.raw.gz`). Photoshop wrote the last three
/// pixels wrong.
#[test]
fn cmyk() {
    let gz = read("ref-cmyk.raw.gz");
    let want = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let img = jpx::decode(&read("kakadu-lossless-cmyk-u8-prog1-layers1-res6.jp2")).unwrap();
    assert_eq!((img.w, img.h, img.space, img.n), (119, 101, jpx::Space::Cmyk, 4));
    for i in 0..119 * 101 - 3 {
        assert_eq!(&img.color[i * 4..i * 4 + 4], &want[i * 4..i * 4 + 4], "pixel {i}");
    }
}

#[test]
fn lossy() {
    check_all(
        &["kakadu-lossy-rgba-u8-prog0-layers1-res6-mct.jp2", "openjpeg-lossy-quantization-scalar-derived.jp2"],
        "ref.png",
        5,
        |v| v,
    );
}

#[test]
fn small_files() {
    let img = jpx::decode(&read("indexed-small.jp2")).unwrap();
    assert_eq!((img.w, img.h), (3, 2));
    let px: Vec<[u8; 4]> = img.to_rgba();
    let want = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 255], [255, 255, 0]];
    for (a, b) in px.iter().zip(want) {
        assert_eq!(&a[..3], &b);
    }
    let img = jpx::decode(&read("cmyk-small.jpf")).unwrap();
    assert_eq!((img.w, img.h, img.space, img.n), (4, 2, jpx::Space::Cmyk, 4));
    let want = [
        [0, 0, 0, 0],
        [127, 127, 127, 0],
        [255, 255, 255, 0],
        [255, 255, 255, 255],
        [255, 0, 0, 0],
        [0, 255, 0, 0],
        [0, 0, 255, 0],
        [0, 0, 0, 255],
    ];
    assert_eq!(img.color.chunks(4).collect::<Vec<_>>(), want.iter().map(|c| &c[..]).collect::<Vec<_>>());
    let img = jpx::decode(&read("buggie-gray.jpf")).unwrap();
    assert_eq!((img.w, img.h, img.space), (64, 138, jpx::Space::Gray));
}

/// OpenJPEG's decoding of a generated file: width, height, channels,
/// samples.
fn openjpeg(name: &str) -> (usize, usize, usize, Vec<u8>) {
    let gz = read(&format!("gen/{name}.ref.gz"));
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let n = |i: usize| u32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().unwrap()) as usize;
    (n(0), n(1), n(2), raw[12..].to_vec())
}

/// Files written by OpenJPEG (`make_jpx_fixtures.py`): every progression
/// order, precincts, tiles, image and tile offsets, layers, 16 bits, and
/// sizes down to one pixel. Exact for the 5/3 filter; the 9/7 filter
/// rounds within one step of OpenJPEG.
#[test]
fn openjpeg_files() {
    let mut names: Vec<String> = std::fs::read_dir(format!("{}/gen", dir()))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| !n.ends_with(".gz"))
        .collect();
    names.sort();
    assert!(names.len() >= 20);
    let mut failed = Vec::new();
    for name in &names {
        let (w, h, n, want) = openjpeg(name);
        let img = match jpx::decode(&read(&format!("gen/{name}"))) {
            Ok(i) => i,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        if (img.w, img.h, img.n + img.alpha.is_some() as usize) != (w, h, n) {
            failed.push(format!("{name}: {}x{}x{} against {w}x{h}x{n}", img.w, img.h, img.n));
            continue;
        }
        let mut worst = 0;
        for i in 0..w * h {
            for k in 0..n {
                let got = if k < img.n { img.color[i * img.n + k] } else { img.alpha.as_ref().unwrap()[i] };
                worst = worst.max(got.abs_diff(want[i * n + k]));
            }
        }
        let tolerance = if name.starts_with("irr") { 1 } else { 0 };
        if worst > tolerance {
            failed.push(format!("{name}: off by {worst}"));
        }
    }
    assert!(failed.is_empty(), "{} of {} failed:\n{}", failed.len(), names.len(), failed.join("\n"));
}
