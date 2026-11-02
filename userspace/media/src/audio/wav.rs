//! WAVE files (RIFF and RF64): integer PCM of 8 to 32 bits, IEEE float,
//! A-law, mu-law, IMA ADPCM and Microsoft ADPCM, also inside
//! `WAVE_FORMAT_EXTENSIBLE`.

use super::{Decoder, Info};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const PCM: u16 = 1;
const MS_ADPCM: u16 = 2;
const FLOAT: u16 = 3;
const ALAW: u16 = 6;
const MULAW: u16 = 7;
const IMA_ADPCM: u16 = 0x11;
const EXTENSIBLE: u16 = 0xFFFE;

/// Frames decoded per block for the uncompressed formats.
const BLOCK_FRAMES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coding {
    Pcm,
    Float,
    Alaw,
    Mulaw,
    Ima,
    Ms,
}

pub struct WavDecoder {
    data: Vec<u8>,
    info: Info,
    coding: Coding,
    /// Byte range of the `data` chunk.
    start: usize,
    end: usize,
    block_align: usize,
    bits: u16,
    /// Frames in one ADPCM block.
    block_frames: usize,
    /// Microsoft ADPCM predictor coefficients.
    coefs: Vec<(i32, i32)>,
    /// Next byte to decode (at a block boundary for ADPCM).
    pos: usize,
    /// Frames to drop from the next ADPCM block (after a seek).
    drop: usize,
}

fn u16le(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d[p], d[p + 1]])
}

fn u32le(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

fn u64le(d: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(d[p..p + 8].try_into().unwrap())
}

impl WavDecoder {
    pub fn new(data: Vec<u8>) -> Result<WavDecoder, String> {
        if data.len() < 12 || &data[8..12] != b"WAVE" {
            return Err(String::from("not a WAVE file"));
        }
        let rf64 = &data[..4] == b"RF64";
        let mut p = 12;
        let mut fmt: Option<(usize, usize)> = None;
        let mut body: Option<(usize, usize)> = None;
        let mut big_data: Option<u64> = None;
        while p + 8 <= data.len() {
            let id = &data[p..p + 4];
            let mut len = u32le(&data, p + 4) as u64;
            if id == b"ds64" && p + 8 + 16 <= data.len() {
                big_data = Some(u64le(&data, p + 8 + 8));
            }
            if id == b"data" && (len == 0xFFFF_FFFF || rf64) {
                if let Some(b) = big_data {
                    len = b;
                }
            }
            let at = p + 8;
            let end = at.saturating_add(len as usize).min(data.len());
            match id {
                b"fmt " => fmt = Some((at, end)),
                b"data" => {
                    body = Some((at, end));
                    if fmt.is_some() {
                        break;
                    }
                }
                _ => {}
            }
            // Chunks are padded to an even size.
            p = at.saturating_add(len as usize + (len as usize & 1));
        }
        let (fa, fe) = fmt.ok_or("WAVE file without a fmt chunk")?;
        let (start, end) = body.ok_or("WAVE file without a data chunk")?;
        if fe - fa < 16 {
            return Err(String::from("WAVE fmt chunk too short"));
        }
        let mut tag = u16le(&data, fa);
        let channels = u16le(&data, fa + 2) as usize;
        let rate = u32le(&data, fa + 4);
        let block_align = u16le(&data, fa + 12) as usize;
        let bits = u16le(&data, fa + 14);
        if tag == EXTENSIBLE {
            if fe - fa < 40 {
                return Err(String::from("WAVE_FORMAT_EXTENSIBLE fmt chunk too short"));
            }
            // The sub-format GUID starts with the format tag.
            tag = u16le(&data, fa + 24);
        }
        if channels == 0 || rate == 0 || block_align == 0 {
            return Err(String::from("WAVE header with zero channels, rate or block size"));
        }
        let (coding, name) = match (tag, bits) {
            (PCM, 8 | 16 | 24 | 32) => (Coding::Pcm, format!("WAV, {bits}-bit PCM")),
            (FLOAT, 32 | 64) => (Coding::Float, format!("WAV, {bits}-bit float")),
            (ALAW, 8) => (Coding::Alaw, String::from("WAV, A-law")),
            (MULAW, 8) => (Coding::Mulaw, String::from("WAV, mu-law")),
            (IMA_ADPCM, 4) => (Coding::Ima, String::from("WAV, IMA ADPCM")),
            (MS_ADPCM, 4) => (Coding::Ms, String::from("WAV, Microsoft ADPCM")),
            _ => return Err(format!("WAVE format {tag:#x} with {bits} bits per sample is not supported")),
        };
        let mut coefs = Vec::new();
        let block_frames = match coding {
            Coding::Ima => {
                if block_align < 4 * channels {
                    return Err(String::from("IMA ADPCM block too small"));
                }
                (block_align - 4 * channels) * 2 / channels + 1
            }
            Coding::Ms => {
                if block_align < 7 * channels {
                    return Err(String::from("Microsoft ADPCM block too small"));
                }
                // cbSize, wSamplesPerBlock, wNumCoef, then the pairs.
                if fe - fa >= 22 {
                    let n = u16le(&data, fa + 20) as usize;
                    for i in 0..n {
                        let q = fa + 22 + 4 * i;
                        if q + 4 > fe {
                            break;
                        }
                        coefs.push((u16le(&data, q) as i16 as i32, u16le(&data, q + 2) as i16 as i32));
                    }
                }
                if coefs.is_empty() {
                    coefs = vec![(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];
                }
                (block_align - 7 * channels) * 2 / channels + 2
            }
            _ => {
                if block_align != channels * bits as usize / 8 {
                    return Err(String::from("WAVE block size does not match channels and bits"));
                }
                1
            }
        };
        let bytes = (end - start) as u64;
        let frames = match coding {
            Coding::Ima | Coding::Ms => {
                let full = bytes / block_align as u64;
                let rest = (bytes % block_align as u64) as usize;
                let tail = match coding {
                    Coding::Ima if rest >= 4 * channels => (rest - 4 * channels) * 2 / channels + 1,
                    Coding::Ms if rest >= 7 * channels => (rest - 7 * channels) * 2 / channels + 2,
                    _ => 0,
                };
                full * block_frames as u64 + tail as u64
            }
            _ => bytes / block_align as u64,
        };
        Ok(WavDecoder {
            data,
            info: Info { rate, channels, frames: Some(frames), codec: name },
            coding,
            start,
            end,
            block_align,
            bits,
            block_frames,
            coefs,
            pos: start,
            drop: 0,
        })
    }

    fn sample(&self, p: usize) -> f32 {
        let d = &self.data;
        match (self.coding, self.bits) {
            (Coding::Pcm, 8) => (d[p] as f32 - 128.0) / 128.0,
            (Coding::Pcm, 16) => i16::from_le_bytes([d[p], d[p + 1]]) as f32 / 32768.0,
            (Coding::Pcm, 24) => ((i32::from_le_bytes([0, d[p], d[p + 1], d[p + 2]]) >> 8) as f32) / 8_388_608.0,
            (Coding::Pcm, _) => i32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as f32 / 2_147_483_648.0,
            (Coding::Float, 32) => f32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]),
            (Coding::Float, _) => f64::from_le_bytes(d[p..p + 8].try_into().unwrap()) as f32,
            (Coding::Alaw, _) => alaw(d[p]) as f32 / 32768.0,
            (Coding::Mulaw, _) => mulaw(d[p]) as f32 / 32768.0,
            _ => 0.0,
        }
    }

    fn adpcm_block(&self, block: &[u8]) -> Vec<f32> {
        let ch = self.info.channels;
        let out = match self.coding {
            Coding::Ima => ima_block(block, ch, self.block_frames),
            _ => ms_block(block, ch, self.block_frames, &self.coefs),
        };
        out.into_iter().map(|s| s as f32 / 32768.0).collect()
    }
}

impl Decoder for WavDecoder {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        if self.pos >= self.end {
            return Ok(None);
        }
        match self.coding {
            Coding::Ima | Coding::Ms => {
                let len = self.block_align.min(self.end - self.pos);
                let block = self.data[self.pos..self.pos + len].to_vec();
                self.pos += len;
                let mut out = self.adpcm_block(&block);
                let drop = core::mem::take(&mut self.drop) * self.info.channels;
                out.drain(..drop.min(out.len()));
                Ok(Some(out))
            }
            _ => {
                let frames = ((self.end - self.pos) / self.block_align).min(BLOCK_FRAMES);
                if frames == 0 {
                    self.pos = self.end;
                    return Ok(None);
                }
                let width = self.block_align / self.info.channels;
                let mut out = Vec::with_capacity(frames * self.info.channels);
                for f in 0..frames {
                    for c in 0..self.info.channels {
                        out.push(self.sample(self.pos + f * self.block_align + c * width));
                    }
                }
                self.pos += frames * self.block_align;
                Ok(Some(out))
            }
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = frame.min(self.info.frames.unwrap_or(0));
        match self.coding {
            Coding::Ima | Coding::Ms => {
                let block = frame / self.block_frames as u64;
                self.pos = (self.start + block as usize * self.block_align).min(self.end);
                self.drop = (frame % self.block_frames as u64) as usize;
            }
            _ => self.pos = (self.start + frame as usize * self.block_align).min(self.end),
        }
        Ok(frame)
    }
}

/// G.711 A-law to 16-bit linear.
pub fn alaw(a: u8) -> i16 {
    let a = a ^ 0x55;
    let seg = (a >> 4) & 7;
    let mut t = ((a & 0x0F) as i32) << 4;
    t = match seg {
        0 => t + 8,
        1 => t + 0x108,
        _ => (t + 0x108) << (seg - 1),
    };
    (if a & 0x80 != 0 { t } else { -t }) as i16
}

/// G.711 mu-law to 16-bit linear.
pub fn mulaw(u: u8) -> i16 {
    let u = !u;
    let mut t = (((u & 0x0F) as i32) << 3) + 0x84;
    t <<= (u & 0x70) >> 4;
    (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as i16
}

const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// One IMA ADPCM block: a 4-byte header per channel, then groups of 4
/// bytes (8 samples, low nibble first) per channel in turn.
fn ima_block(block: &[u8], ch: usize, frames: usize) -> Vec<i16> {
    if block.len() < 4 * ch {
        return Vec::new();
    }
    let mut pred = vec![0i32; ch];
    let mut index = vec![0i32; ch];
    let mut out = vec![0i16; frames * ch];
    for c in 0..ch {
        pred[c] = i16::from_le_bytes([block[4 * c], block[4 * c + 1]]) as i32;
        index[c] = (block[4 * c + 2] as i32).clamp(0, 88);
        out[c] = pred[c] as i16;
    }
    let data = &block[4 * ch..];
    let mut produced = 1;
    let groups = data.len() / (4 * ch);
    for g in 0..groups {
        for c in 0..ch {
            let bytes = &data[(g * ch + c) * 4..(g * ch + c) * 4 + 4];
            for (i, &b) in bytes.iter().enumerate() {
                for (k, nib) in [b & 0x0F, b >> 4].into_iter().enumerate() {
                    let f = produced + i * 2 + k;
                    let step = IMA_STEP[index[c] as usize];
                    let mut diff = step >> 3;
                    if nib & 1 != 0 {
                        diff += step >> 2;
                    }
                    if nib & 2 != 0 {
                        diff += step >> 1;
                    }
                    if nib & 4 != 0 {
                        diff += step;
                    }
                    pred[c] = if nib & 8 != 0 { pred[c] - diff } else { pred[c] + diff }.clamp(-32768, 32767);
                    index[c] = (index[c] + IMA_INDEX[nib as usize]).clamp(0, 88);
                    if f < frames {
                        out[f * ch + c] = pred[c] as i16;
                    }
                }
            }
        }
        produced += 8;
    }
    out.truncate(produced.min(frames) * ch);
    out
}

const MS_ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];

/// One Microsoft ADPCM block: per channel a predictor index, a delta and
/// two history samples, then nibbles (high first) alternating channels.
fn ms_block(block: &[u8], ch: usize, frames: usize, coefs: &[(i32, i32)]) -> Vec<i16> {
    if block.len() < 7 * ch {
        return Vec::new();
    }
    let rd = |p: usize| i16::from_le_bytes([block[p], block[p + 1]]) as i32;
    let mut co = vec![(0, 0); ch];
    let mut delta = vec![0i32; ch];
    let mut s1 = vec![0i32; ch];
    let mut s2 = vec![0i32; ch];
    for c in 0..ch {
        co[c] = *coefs.get(block[c] as usize).unwrap_or(&(0, 0));
        delta[c] = rd(ch + 2 * c);
        s1[c] = rd(3 * ch + 2 * c);
        s2[c] = rd(5 * ch + 2 * c);
    }
    let mut out = Vec::with_capacity(frames * ch);
    out.extend(s2.iter().map(|&s| s as i16));
    out.extend(s1.iter().map(|&s| s as i16));
    let mut c = 0;
    for &b in &block[7 * ch..] {
        for nib in [b >> 4, b & 0x0F] {
            if out.len() >= frames * ch {
                break;
            }
            let signed = ((nib as i32) << 28) >> 28;
            let pred = (s1[c] * co[c].0 + s2[c] * co[c].1) >> 8;
            let s = (pred + signed * delta[c]).clamp(-32768, 32767);
            s2[c] = s1[c];
            s1[c] = s;
            delta[c] = (MS_ADAPT[nib as usize] * delta[c]) >> 8;
            if delta[c] < 16 {
                delta[c] = 16;
            }
            out.push(s as i16);
            c = (c + 1) % ch;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_endpoints() {
        assert_eq!(mulaw(0xFF), 0);
        assert_eq!(mulaw(0x00), -32124);
        assert_eq!(mulaw(0x80), 32124);
        assert_eq!(alaw(0xD5), 8);
        assert_eq!(alaw(0x55), -8);
        assert_eq!(alaw(0xAA), 32256);
    }
}
