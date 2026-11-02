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

/// FFmpeg's decoded sound: rate, channels, 16-bit samples.
fn pcm(name: &str) -> (u32, usize, Vec<i16>) {
    let gz = read(&format!("{name}.pcm.gz"));
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let n = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap()) as usize;
    let s = raw[12..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    (n(0) as u32, n(4), s)
}

/// WebM and Matroska movies: every picture matches FFmpeg's (through the
/// same colour conversion), and the sound stays within a few steps of
/// FFmpeg's decoders.
#[test]
fn movies_match_ffmpeg() {
    use osyx_media::video::Movie;
    let mut report = Vec::new();
    for (file, name) in
        [("movie-opus.webm", "movie-opus"), ("movie-vorbis.webm", "movie-vorbis"), ("movie-mkv.mkv", "movie-mkv")]
    {
        let movie = Movie::open(read(file)).unwrap();
        let (w, h, want) = reference(name);
        let mut video = movie.video().unwrap().unwrap();
        let mut k = 0;
        while let Some(p) = video.next_picture() {
            let p = p.unwrap();
            let planes = &want[k];
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let yuv = vp8::Yuv {
                w,
                h,
                y: &planes[..w * h],
                u: &planes[w * h..w * h + cw * ch],
                v: &planes[w * h + cw * ch..],
                y_stride: w,
                uv_stride: cw,
            };
            assert_eq!(p.px, vp8::to_rgba(&yuv), "{name} picture {k}");
            assert_eq!(p.ms, k as u64 * 40, "{name} picture {k} time");
            k += 1;
        }
        assert_eq!(k, want.len(), "{name}: pictures");
        let (rate, ch, samples) = pcm(name);
        let mut dec = movie.audio().unwrap().unwrap();
        assert_eq!((dec.info().rate, dec.info().channels), (rate, ch), "{name}");
        let mut got = Vec::new();
        while let Some(b) = dec.next_block().unwrap() {
            got.extend(b);
        }
        let n = got.len().min(samples.len());
        let worst = (0..n).map(|i| ((got[i] * 32768.0).round() as i32 - samples[i] as i32).abs()).max().unwrap();
        report.push(format!("{name}: {} samples against {}, worst {worst}", got.len() / ch, samples.len() / ch));
        // FFmpeg's own Opus decoder is not libopus: a few steps apart.
        assert!(worst <= 8 && got.len().abs_diff(samples.len()) <= ch, "{}", report.last().unwrap());
    }
    println!("{}", report.join("\n"));
}

/// Seeking lands on the right picture and sample.
#[test]
fn movie_seeking() {
    use osyx_media::video::Movie;
    let movie = Movie::open(read("movie-opus.webm")).unwrap();
    let (w, h, want) = reference("movie-opus");
    let mut video = movie.video().unwrap().unwrap();
    video.seek(530);
    let p = video.next_picture().unwrap().unwrap();
    // The picture showing at 530 ms is the one from 520 ms.
    assert_eq!(p.ms, 520);
    let planes = &want[13];
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let yuv = vp8::Yuv {
        w,
        h,
        y: &planes[..w * h],
        u: &planes[w * h..w * h + cw * ch],
        v: &planes[w * h + cw * ch..],
        y_stride: w,
        uv_stride: cw,
    };
    assert_eq!(p.px, vp8::to_rgba(&yuv));
    let (rate, ch, samples) = pcm("movie-opus");
    let mut dec = movie.audio().unwrap().unwrap();
    let at = dec.seek(rate as u64 / 2).unwrap() as usize;
    let b = dec.next_block().unwrap().unwrap();
    let worst = (0..b.len().min(2000))
        .map(|i| ((b[i] * 32768.0).round() as i32 - samples[at * ch + i] as i32).abs())
        .max()
        .unwrap();
    // After the pre-roll the decoder has converged to FFmpeg's output.
    assert!(worst <= 8, "worst {worst}");
}
