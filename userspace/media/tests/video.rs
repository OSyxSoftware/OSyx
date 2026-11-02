//! VP8 streams written by libvpx against FFmpeg's decodings of them
//! (`make_video_fixtures.py`): inter frames, golden and alternate
//! references, several token partitions, odd sizes.

use osyx_media::vp8;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/video/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// FFmpeg's shown frames: width, height, I420 planes per frame.
fn reference(name: &str) -> (usize, usize, Vec<Vec<u8>>) {
    let gz = read(&format!("{name}.yuv.gz"));
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let n = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap()) as usize;
    let (w, h, count) = (n(0), n(4), n(8));
    let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    (w, h, (0..count).map(|k| raw[12 + k * size..12 + (k + 1) * size].to_vec()).collect())
}

/// The frames of an IVF file.
fn ivf(data: &[u8]) -> Vec<&[u8]> {
    let mut p = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut out = Vec::new();
    while p + 12 <= data.len() {
        let len = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        out.push(&data[p + 12..p + 12 + len]);
        p += 12 + len;
    }
    out
}

fn planes(f: &vp8::Yuv) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..f.h {
        out.extend_from_slice(&f.y[y * f.y_stride..][..f.w]);
    }
    let (cw, ch) = (f.w.div_ceil(2), f.h.div_ceil(2));
    for plane in [f.u, f.v] {
        for y in 0..ch {
            out.extend_from_slice(&plane[y * f.uv_stride..][..cw]);
        }
    }
    out
}

#[test]
fn vp8_streams_match_ffmpeg() {
    let mut failed = Vec::new();
    for name in ["vp8-altref", "vp8-odd", "vp8-hq"] {
        let data = read(&format!("{name}.ivf"));
        let (w, h, want) = reference(name);
        let mut dec = vp8::Decoder::new();
        let mut shown = Vec::new();
        for (k, frame) in ivf(&data).into_iter().enumerate() {
            if let Err(e) = dec.decode(frame) {
                failed.push(format!("{name} frame {k}: {e}"));
                break;
            }
            if dec.shown() {
                let f = dec.frame().unwrap();
                assert_eq!((f.w, f.h), (w, h));
                shown.push(planes(&f));
            }
        }
        if shown.len() != want.len() {
            failed.push(format!("{name}: {} shown frames against {}", shown.len(), want.len()));
        }
        for (k, (a, b)) in shown.iter().zip(&want).enumerate() {
            let off = a.iter().zip(b).filter(|(x, y)| x != y).count();
            if off > 0 {
                failed.push(format!("{name} frame {k}: {off} samples differ"));
                break;
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
