//! The decoders against libsndfile's decodings of the same files
//! (`make_fixtures.py`).

use osyx_media::audio::{self, Decoder};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// libsndfile's decoding: interleaved 16-bit samples.
fn reference(stem: &str) -> Vec<i16> {
    let gz = data(&format!("{stem}.ref.gz"));
    // gzip: 10-byte header, deflate data, 8-byte trailer.
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    raw.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

fn decode_all(dec: &mut dyn Decoder) -> Vec<f32> {
    let mut out = Vec::new();
    while let Some(b) = dec.next_block().unwrap() {
        out.extend(b);
    }
    out
}

fn to16(v: f32) -> i16 {
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// Signal-to-error ratio in dB of `got` against `want`.
fn snr(got: &[f32], want: &[i16]) -> f64 {
    let (mut s, mut e) = (0.0f64, 0.0f64);
    for (g, w) in got.iter().zip(want) {
        let w = *w as f64 / 32768.0;
        s += w * w;
        e += (*g as f64 - w) * (*g as f64 - w);
    }
    if e == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (s / e).log10()
    }
}

/// Decodes `file` and compares: same length, rate and channels, and every
/// sample within `tolerance` (16-bit steps) of the reference.
fn check(file: &str, rate: u32, channels: usize, tolerance: i32) -> Vec<f32> {
    let stem = file.rsplit_once('.').unwrap().0;
    let want = reference(stem);
    let mut dec = audio::open(data(file)).unwrap();
    assert_eq!(dec.info().rate, rate, "{file}");
    assert_eq!(dec.info().channels, channels, "{file}");
    let got = decode_all(dec.as_mut());
    assert_eq!(got.len(), want.len(), "{file}: sample count");
    if let Some(f) = dec.info().frames {
        assert_eq!(f as usize * channels, want.len(), "{file}: announced length");
    }
    let worst = got.iter().zip(&want).map(|(g, w)| (to16(*g) as i32 - *w as i32).abs()).max().unwrap();
    assert!(worst <= tolerance, "{file}: off by up to {worst} (SNR {:.1} dB)", snr(&got, &want));
    got
}

/// Seeking lands exactly: what follows equals the straight decoding.
fn check_seek(file: &str, whole: &[f32], channels: usize, frame: u64) {
    let mut dec = audio::open(data(file)).unwrap();
    assert_eq!(dec.seek(frame).unwrap(), frame);
    let got = decode_all(dec.as_mut());
    let want = &whole[frame as usize * channels..];
    assert_eq!(got.len(), want.len(), "{file}: length after seeking to {frame}");
    let worst = got.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-6, "{file}: after seeking to {frame} off by {worst}");
}

#[test]
fn wav() {
    for (f, rate, ch, tol) in [
        ("pcm16-stereo-44100.wav", 44100, 2, 0),
        ("pcm8-mono-8000.wav", 8000, 1, 0),
        ("pcm24-stereo-48000.wav", 48000, 2, 1),
        ("float-mono-22050.wav", 22050, 1, 1),
        ("ulaw-mono-8000.wav", 8000, 1, 0),
        ("alaw-mono-8000.wav", 8000, 1, 0),
        ("ima-adpcm-stereo-22050.wav", 22050, 2, 0),
        ("ms-adpcm-mono-11025.wav", 11025, 1, 0),
    ] {
        let whole = check(f, rate, ch, tol);
        let frames = (whole.len() / ch) as u64;
        for at in [0, 1, 1017, frames / 2, frames - 3, frames] {
            check_seek(f, &whole, ch, at);
        }
    }
}

#[test]
fn flac() {
    for (f, rate, ch) in [
        ("flac16-stereo-44100.flac", 44100, 2),
        ("flac24-mono-96000.flac", 96000, 1),
        ("flac8-stereo-8000.flac", 8000, 2),
    ] {
        let whole = check(f, rate, ch, 1);
        let frames = (whole.len() / ch) as u64;
        for at in [0, 1, 4095, 4096, 4097, frames / 2, frames - 3, frames].into_iter().filter(|&a| a <= frames) {
            check_seek(f, &whole, ch, at);
        }
    }
}

#[test]
fn mp3() {
    for (f, rate, ch) in
        [("mp3-stereo-44100.mp3", 44100, 2), ("mp3-mono-22050.mp3", 22050, 1), ("mp3-mono-8000.mp3", 8000, 1)]
    {
        let whole = check(f, rate, ch, 2);
        let frames = (whole.len() / ch) as u64;
        for at in [0, 1, 1151, 1152, 5000, frames / 2, frames - 3, frames].into_iter().filter(|&a| a <= frames) {
            check_seek(f, &whole, ch, at);
        }
    }
}

#[test]
fn vorbis() {
    for (f, rate, ch) in [("vorbis-stereo-44100.ogg", 44100, 2), ("vorbis-mono-22050.ogg", 22050, 1)] {
        let whole = check(f, rate, ch, 2);
        let frames = (whole.len() / ch) as u64;
        for at in [0, 1, 1023, 4100, frames / 2, frames - 3, frames].into_iter().filter(|&a| a <= frames) {
            check_seek(f, &whole, ch, at);
        }
    }
}

#[test]
fn aac() {
    // FFmpeg's encoder with noise substitution: its noise matches only when
    // decoded from the start, so that file is not seeked.
    for (f, rate, ch, seekable) in [
        ("aac-stereo-44100.m4a", 44100, 2, false),
        ("aac-mono-22050.aac", 22050, 1, false),
        ("aac-stereo-48000.aac", 48000, 2, true),
        ("aac-5.1-48000.m4a", 48000, 6, true),
        ("aac-mono-8000.mp4", 8000, 1, false),
    ] {
        let whole = check(f, rate, ch, 1);
        if seekable {
            let frames = (whole.len() / ch) as u64;
            for at in [0, 1, 1023, 1024, 3000, frames / 2, frames - 3, frames] {
                check_seek(f, &whole, ch, at);
            }
        }
    }
}
