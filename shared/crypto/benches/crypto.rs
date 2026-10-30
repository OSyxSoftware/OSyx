//! Benchmarks for the kernel RNG primitives: ChaCha20, SHA-256 and the DRBG.

use divan::{black_box, Bencher};
use osyx_crypto::{chacha20_block, Drbg, Sha256};

fn main() {
    divan::main();
}

const SIZES: &[usize] = &[64, 4096, 65536];

fn input(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(7)).collect()
}

#[divan::bench]
fn chacha20_one_block() -> [u8; 64] {
    chacha20_block(black_box(&[0x42; 32]), black_box(1), black_box(&[0x24; 12]))
}

#[divan::bench(args = SIZES)]
fn sha256_digest(bencher: Bencher, len: usize) {
    let data = input(len);
    bencher.bench_local(|| Sha256::digest(black_box(&data)));
}

#[divan::bench]
fn sha256_incremental_64k(bencher: Bencher) {
    let data = input(65536);
    bencher.bench_local(|| {
        let mut h = Sha256::new();
        for chunk in black_box(&data).chunks(1500) {
            h.update(chunk);
        }
        h.finish()
    });
}

#[divan::bench(args = SIZES)]
fn drbg_fill(bencher: Bencher, len: usize) {
    let mut drbg = Drbg::default();
    drbg.reseed(&[0x5a; 32]);
    let mut out = vec![0u8; len];
    bencher.bench_local(|| {
        drbg.fill(black_box(&mut out)).unwrap();
    });
}
