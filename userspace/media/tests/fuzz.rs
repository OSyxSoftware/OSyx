//! Damaged JBIG2 and JPEG 2000 data: random byte changes, bit flips and
//! truncations of the test files must fail cleanly, never panic. Slow, so
//! opt-in: `cargo test --release --test fuzz -- --ignored`
//! (`FUZZ_N` sets the mutations per file, 30 by default).

fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

fn run(dir: &str, ext: &str, decode: fn(&[u8])) {
    let mut paths: Vec<_> = std::fs::read_dir(format!("{}/tests/data/{dir}", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    paths.sort();
    let n: usize = std::env::var("FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for p in &paths {
        let base = std::fs::read(p).unwrap();
        for it in 0..n {
            let mut d = base.clone();
            match it % 3 {
                0 => {
                    for _ in 0..next(&mut seed) % 8 + 1 {
                        let i = next(&mut seed) as usize % d.len();
                        d[i] = next(&mut seed) as u8;
                    }
                }
                1 => d.truncate(next(&mut seed) as usize % d.len()),
                _ => {
                    let i = next(&mut seed) as usize % d.len();
                    d[i] ^= 1 << (next(&mut seed) % 8);
                }
            }
            if std::panic::catch_unwind(|| decode(&d)).is_err() {
                panic!("{} mutation {it}", p.display());
            }
        }
    }
}

#[test]
#[ignore]
fn jpx() {
    run("jpx", "jp2", |d| {
        let _ = osyx_media::jpx::decode(d);
    });
}

#[test]
#[ignore]
fn jbig2() {
    run("jbig2", "jbig2", |d| {
        let _ = osyx_media::jbig2::decode(d, None, false);
    });
}

#[test]
#[ignore]
fn webp() {
    run("webp", "webp", |d| {
        let _ = osyx_media::image::decode(d);
    });
}

#[test]
#[ignore]
fn vp8() {
    run("video", "ivf", |d| {
        let mut dec = osyx_media::vp8::Decoder::new();
        let mut p = 32;
        while p + 12 <= d.len() {
            let len = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            let end = (p + 12 + len).min(d.len());
            let _ = dec.decode(&d[p + 12..end]);
            p = end;
        }
    });
}
