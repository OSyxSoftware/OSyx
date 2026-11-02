//! The Opus test vectors of RFC 8251 (opus_testvectors-rfc8251.tar.gz from
//! opus-codec.org), when OSYX_OPUS_VECTORS names the folder that holds
//! them: every packet's final range must match the encoder's, and the
//! output must stay within one step of the reference decoding.

use osyx_media::audio::opus::{OpusDecoder, MAX_PACKET_SAMPLES};

#[test]
fn rfc8251_vectors() {
    let Ok(dir) = std::env::var("OSYX_OPUS_VECTORS") else {
        eprintln!("OSYX_OPUS_VECTORS not set; skipping the RFC 8251 vectors");
        return;
    };
    for v in 1..=12 {
        let bit = std::fs::read(format!("{dir}/testvector{v:02}.bit")).unwrap();
        let reference = std::fs::read(format!("{dir}/testvector{v:02}.dec")).unwrap();
        let want: Vec<i16> = reference.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let mut dec = OpusDecoder::new(2);
        let mut pcm = vec![0.0f32; MAX_PACKET_SAMPLES * 2];
        let mut got = Vec::new();
        let mut p = 0;
        let mut packet = 0;
        while p + 8 <= bit.len() {
            let len = u32::from_be_bytes(bit[p..p + 4].try_into().unwrap()) as usize;
            let range = u32::from_be_bytes(bit[p + 4..p + 8].try_into().unwrap());
            let data = &bit[p + 8..p + 8 + len];
            p += 8 + len;
            let n = dec.decode(data, &mut pcm, MAX_PACKET_SAMPLES).unwrap();
            assert_eq!(dec.range_final, range, "vector {v} packet {packet}: final range");
            got.extend(pcm[..n * 2].iter().map(|s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16));
            packet += 1;
        }
        assert_eq!(got.len(), want.len(), "vector {v}: length");
        let worst = got.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 1, "vector {v}: off by up to {worst}");
        eprintln!("vector {v}: {packet} packets, worst {worst}");
    }
}
