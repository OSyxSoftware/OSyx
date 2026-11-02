//! The WebP decoders against libwebp's decodings of files it wrote
//! (`make_webp_fixtures.py`): lossy, lossless, alpha and animations.

use osyx_media::image;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/webp/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// libwebp's frames: width, height, (duration, RGBA) per frame.
fn reference(name: &str) -> (usize, usize, Vec<(u32, Vec<u8>)>) {
    let gz = read(&format!("{name}.ref.gz"));
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let n = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap()) as usize;
    let (w, h, count) = (n(0), n(4), n(8));
    let mut frames = Vec::new();
    let mut p = 12;
    for _ in 0..count {
        let d = n(p) as u32;
        frames.push((d, raw[p + 4..p + 4 + w * h * 4].to_vec()));
        p += 4 + w * h * 4;
    }
    (w, h, frames)
}

#[test]
fn match_libwebp() {
    let mut names: Vec<String> = std::fs::read_dir(format!("{}/tests/data/webp", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".webp"))
        .map(|n| n.trim_end_matches(".webp").to_string())
        .collect();
    names.sort();
    assert!(names.len() >= 19);
    let mut failed = Vec::new();
    for name in &names {
        let pic = match image::decode(&read(&format!("{name}.webp"))) {
            Ok(p) => p,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        let (w, h, frames) = reference(name);
        if (pic.w, pic.h, pic.frames.len()) != (w, h, frames.len()) {
            failed.push(format!("{name}: {}x{} x{} against {w}x{h} x{}", pic.w, pic.h, pic.frames.len(), frames.len()));
            continue;
        }
        for (k, (f, (d, want))) in pic.frames.iter().zip(&frames).enumerate() {
            let got: Vec<u8> = f.px.iter().flatten().copied().collect();
            let off = got.iter().zip(want).filter(|(a, b)| a != b).count();
            let worst = got.iter().zip(want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            if off > 0 || (frames.len() > 1 && f.delay_ms != *d) {
                failed.push(format!(
                    "{name} frame {k}: {off} samples differ, by up to {worst}; delay {} against {d}",
                    f.delay_ms
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{} of {} failed:\n{}", failed.len(), names.len(), failed.join("\n"));
}
