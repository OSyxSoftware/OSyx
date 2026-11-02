//! FLAC (native `fLaC` streams): constant, verbatim, fixed and LPC
//! subframes, Rice residuals with escapes, wasted bits, the three stereo
//! decorrelation modes, CRC checks, and seeking by bisection on frame
//! headers.

use super::{id3v2_len, Decoder, Info};
use crate::bits::MsbReader;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub struct FlacDecoder {
    data: Vec<u8>,
    info: Info,
    bits: u32,
    /// First byte after the metadata blocks.
    first_frame: usize,
    pos: usize,
    max_block: usize,
    /// Frames to drop from the next decoded block (after a seek).
    drop: usize,
}

/// A parsed frame header.
#[derive(Clone, Copy, Debug)]
struct Header {
    block: usize,
    rate: u32,
    /// 0..=7 independent channels (count - 1), 8 left/side, 9 side/right,
    /// 10 mid/side.
    assignment: u32,
    bits: u32,
    /// The first sample of the frame (fixed block size streams store the
    /// frame number, already multiplied here).
    first_sample: u64,
    /// Bytes of the header, CRC-8 included.
    len: usize,
}

const CRC8_POLY: u8 = 0x07;

fn crc8(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ CRC8_POLY } else { c << 1 };
        }
    }
    c
}

fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
        }
    }
    c
}

impl FlacDecoder {
    pub fn new(data: Vec<u8>) -> Result<FlacDecoder, String> {
        let start = id3v2_len(&data).unwrap_or(0);
        if data.get(start..start + 4) != Some(b"fLaC") {
            return Err(String::from("not a FLAC stream"));
        }
        let mut p = start + 4;
        let mut streaminfo = None;
        loop {
            if p + 4 > data.len() {
                return Err(String::from("FLAC metadata is cut short"));
            }
            let last = data[p] & 0x80 != 0;
            let kind = data[p] & 0x7F;
            let len = u32::from_be_bytes([0, data[p + 1], data[p + 2], data[p + 3]]) as usize;
            let body = p + 4;
            if body + len > data.len() {
                return Err(String::from("FLAC metadata block runs past the end"));
            }
            if kind == 0 {
                if len < 34 {
                    return Err(String::from("FLAC STREAMINFO too short"));
                }
                streaminfo = Some(body);
            }
            p = body + len;
            if last {
                break;
            }
        }
        let si = streaminfo.ok_or("FLAC stream without STREAMINFO")?;
        let mut r = MsbReader::new(&data[si..si + 34]);
        let _min_block = r.read(16).unwrap();
        let max_block = r.read(16).unwrap() as usize;
        r.skip(48).unwrap();
        let rate = r.read(20).unwrap();
        let channels = r.read(3).unwrap() as usize + 1;
        let bits = r.read(5).unwrap() + 1;
        let total = ((r.read(4).unwrap() as u64) << 32) | r.read(32).unwrap() as u64;
        if rate == 0 {
            return Err(String::from("FLAC stream with a sample rate of 0"));
        }
        Ok(FlacDecoder {
            data,
            info: Info { rate, channels, frames: (total != 0).then_some(total), codec: format!("FLAC, {bits}-bit") },
            bits,
            first_frame: p,
            pos: p,
            max_block: max_block.max(16),
            drop: 0,
        })
    }

    /// Parses a frame header at `p`, checking its CRC-8.
    fn header(&self, p: usize) -> Option<Header> {
        let d = &self.data;
        if p + 4 > d.len() || d[p] != 0xFF || d[p + 1] & 0xFE != 0xF8 {
            return None;
        }
        let variable = d[p + 1] & 1 != 0;
        let bs_code = d[p + 2] >> 4;
        let rate_code = d[p + 2] & 0x0F;
        let assignment = (d[p + 3] >> 4) as u32;
        let size_code = (d[p + 3] >> 1) & 7;
        if d[p + 3] & 1 != 0 || assignment > 10 || rate_code == 15 || size_code == 3 || size_code == 7 || bs_code == 0 {
            return None;
        }
        // UTF-8 style coded frame or sample number.
        let mut q = p + 4;
        let first = *d.get(q)?;
        let extra = match first {
            0x00..=0x7F => 0,
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            0xF8..=0xFB => 4,
            0xFC..=0xFD => 5,
            0xFE => 6,
            _ => return None,
        };
        let mut num: u64 = if extra == 0 { first as u64 } else { (first & (0x3F >> extra)) as u64 };
        q += 1;
        for _ in 0..extra {
            let b = *d.get(q)?;
            if b & 0xC0 != 0x80 {
                return None;
            }
            num = (num << 6) | (b & 0x3F) as u64;
            q += 1;
        }
        let block = match bs_code {
            1 => 192,
            2..=5 => 576 << (bs_code - 2),
            6 => {
                q += 1;
                *d.get(q - 1)? as usize + 1
            }
            7 => {
                q += 2;
                u16::from_be_bytes([*d.get(q - 2)?, *d.get(q - 1)?]) as usize + 1
            }
            _ => 256 << (bs_code - 8),
        };
        let rate = match rate_code {
            0 => self.info.rate,
            1 => 88200,
            2 => 176400,
            3 => 192000,
            4 => 8000,
            5 => 16000,
            6 => 22050,
            7 => 24000,
            8 => 32000,
            9 => 44100,
            10 => 48000,
            11 => 96000,
            12 => {
                q += 1;
                *d.get(q - 1)? as u32 * 1000
            }
            13 => {
                q += 2;
                u16::from_be_bytes([*d.get(q - 2)?, *d.get(q - 1)?]) as u32
            }
            _ => {
                q += 2;
                u16::from_be_bytes([*d.get(q - 2)?, *d.get(q - 1)?]) as u32 * 10
            }
        };
        let bits = match size_code {
            0 => self.bits,
            1 => 8,
            2 => 12,
            4 => 16,
            5 => 20,
            _ => 24,
        };
        if crc8(d.get(p..q)?) != *d.get(q)? {
            return None;
        }
        let first_sample = if variable { num } else { num * self.max_block_for_fixed(block) as u64 };
        Some(Header { block, rate, assignment, bits, first_sample, len: q + 1 - p })
    }

    /// Fixed block size streams number frames; all but the last frame have
    /// the stream's block size.
    fn max_block_for_fixed(&self, block: usize) -> usize {
        if self.max_block >= 16 && block <= self.max_block {
            self.max_block
        } else {
            block
        }
    }

    /// The next valid frame header at or after `p`.
    fn find_frame(&self, mut p: usize) -> Option<(usize, Header)> {
        while p + 2 <= self.data.len() {
            let d = &self.data;
            match d[p..].windows(2).position(|w| w[0] == 0xFF && w[1] & 0xFE == 0xF8) {
                Some(off) => {
                    let at = p + off;
                    if let Some(h) = self.header(at) {
                        if h.rate == self.info.rate && channel_count(h.assignment) == self.info.channels {
                            return Some((at, h));
                        }
                    }
                    p = at + 1;
                }
                None => return None,
            }
        }
        None
    }

    /// Decodes the frame at `self.pos`: interleaved samples.
    fn decode_frame(&mut self) -> Result<Option<Vec<f32>>, String> {
        let Some((at, h)) = self.find_frame(self.pos) else {
            self.pos = self.data.len();
            return Ok(None);
        };
        let ch = self.info.channels;
        let mut r = MsbReader::at(&self.data, (at + h.len) * 8);
        let mut chans: Vec<Vec<i64>> = Vec::with_capacity(ch);
        for c in 0..ch {
            // The side channel has one more bit.
            let side = matches!((h.assignment, c), (8, 1) | (9, 0) | (10, 1));
            let bits = h.bits + side as u32;
            chans.push(subframe(&mut r, h.block, bits).ok_or("corrupt FLAC subframe")?);
        }
        r.align();
        let end = r.byte_pos() + 2;
        if end > self.data.len() {
            return Err(String::from("FLAC frame cut short"));
        }
        let crc = u16::from_be_bytes([self.data[end - 2], self.data[end - 1]]);
        if crc16(&self.data[at..end - 2]) != crc {
            // Damaged frame: skip it and resynchronise.
            self.pos = at + 1;
            return Ok(Some(vec![0.0; h.block * ch]));
        }
        self.pos = end;
        match h.assignment {
            8 => {
                for i in 0..h.block {
                    chans[1][i] = chans[0][i] - chans[1][i];
                }
            }
            9 => {
                for i in 0..h.block {
                    chans[0][i] += chans[1][i];
                }
            }
            10 => {
                for i in 0..h.block {
                    let side = chans[1][i];
                    let mid = (chans[0][i] << 1) | (side & 1);
                    chans[0][i] = (mid + side) >> 1;
                    chans[1][i] = (mid - side) >> 1;
                }
            }
            _ => {}
        }
        let scale = 1.0 / (1u64 << (h.bits - 1)) as f32;
        let mut out = Vec::with_capacity(h.block * ch);
        for i in 0..h.block {
            for c in chans.iter() {
                out.push(c[i] as f32 * scale);
            }
        }
        Ok(Some(out))
    }
}

fn channel_count(assignment: u32) -> usize {
    if assignment <= 7 {
        assignment as usize + 1
    } else {
        2
    }
}

/// One subframe of `block` samples of `bits` bits.
fn subframe(r: &mut MsbReader, block: usize, bits: u32) -> Option<Vec<i64>> {
    if r.read(1)? != 0 {
        return None;
    }
    let kind = r.read(6)?;
    let wasted = if r.bit()? { r.unary()? + 1 } else { 0 };
    let bits = bits.checked_sub(wasted)?;
    let mut s = vec![0i64; block];
    match kind {
        0 => {
            let v = r.signed(bits)? as i64;
            s.fill(v);
        }
        1 => {
            for x in s.iter_mut() {
                *x = signed_wide(r, bits)?;
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            if order > block {
                return None;
            }
            for x in s.iter_mut().take(order) {
                *x = signed_wide(r, bits)?;
            }
            residual(r, &mut s, order)?;
            for i in order..block {
                let p = match order {
                    0 => 0,
                    1 => s[i - 1],
                    2 => 2 * s[i - 1] - s[i - 2],
                    3 => 3 * s[i - 1] - 3 * s[i - 2] + s[i - 3],
                    _ => 4 * s[i - 1] - 6 * s[i - 2] + 4 * s[i - 3] - s[i - 4],
                };
                s[i] += p;
            }
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            if order > block {
                return None;
            }
            for x in s.iter_mut().take(order) {
                *x = signed_wide(r, bits)?;
            }
            let precision = r.read(4)? + 1;
            if precision == 16 {
                return None;
            }
            let shift = r.signed(5)?;
            if shift < 0 {
                return None;
            }
            let mut coefs = [0i64; 32];
            for c in coefs.iter_mut().take(order) {
                *c = r.signed(precision)? as i64;
            }
            residual(r, &mut s, order)?;
            for i in order..block {
                let mut sum = 0i64;
                for j in 0..order {
                    sum += coefs[j] * s[i - 1 - j];
                }
                s[i] += sum >> shift;
            }
        }
        _ => return None,
    }
    if wasted > 0 {
        for x in s.iter_mut() {
            *x <<= wasted;
        }
    }
    Some(s)
}

/// A signed value of up to 33 bits (a 32-bit side channel).
fn signed_wide(r: &mut MsbReader, bits: u32) -> Option<i64> {
    if bits <= 32 {
        return r.signed(bits).map(|v| v as i64);
    }
    let hi = r.read(bits - 32)? as u64;
    let lo = r.read(32)? as u64;
    let v = (hi << 32) | lo;
    Some(((v << (64 - bits)) as i64) >> (64 - bits))
}

/// Rice-coded residual into `s[order..]`.
fn residual(r: &mut MsbReader, s: &mut [i64], order: usize) -> Option<()> {
    let method = r.read(2)?;
    let param_bits = match method {
        0 => 4,
        1 => 5,
        _ => return None,
    };
    let escape = (1 << param_bits) - 1;
    let partitions = 1usize << r.read(4)?;
    let block = s.len();
    if block % partitions != 0 {
        return None;
    }
    let per = block / partitions;
    if per < order {
        return None;
    }
    let mut i = order;
    for part in 0..partitions {
        let n = if part == 0 { per - order } else { per };
        let k = r.read(param_bits)?;
        if k == escape {
            let width = r.read(5)?;
            for _ in 0..n {
                s[i] = r.signed(width)? as i64;
                i += 1;
            }
        } else {
            for _ in 0..n {
                let q = r.unary()? as u64;
                let v = (q << k) | r.read(k)? as u64;
                s[i] = ((v >> 1) as i64) ^ -((v & 1) as i64);
                i += 1;
            }
        }
    }
    Some(())
}

impl Decoder for FlacDecoder {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        let Some(mut block) = self.decode_frame()? else { return Ok(None) };
        let drop = core::mem::take(&mut self.drop) * self.info.channels;
        block.drain(..drop.min(block.len()));
        Ok(Some(block))
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = match self.info.frames {
            Some(t) => frame.min(t),
            None => frame,
        };
        // Bisect on byte offsets for the last frame starting at or before
        // the target.
        let (mut lo, mut hi) = (self.first_frame, self.data.len());
        let mut best = (self.first_frame, 0u64);
        while hi > lo + 16 {
            let mid = lo + (hi - lo) / 2;
            match self.find_frame(mid) {
                Some((at, h)) if at < hi => {
                    if h.first_sample <= frame {
                        best = (at, h.first_sample);
                        lo = at + 1;
                    } else {
                        hi = mid;
                    }
                }
                _ => hi = mid,
            }
        }
        if let Some((at, h)) = self.find_frame(self.first_frame) {
            if best.1 == 0 {
                best = (at, h.first_sample);
            }
        }
        self.pos = best.0;
        self.drop = (frame - best.1.min(frame)) as usize;
        // A target past the frame found is skipped one frame at a time.
        while let Some((at, h)) = self.find_frame(self.pos) {
            if self.drop < h.block {
                self.pos = at;
                break;
            }
            self.drop -= h.block;
            // Skipping needs the frame's end: decode it.
            self.pos = at;
            if self.decode_frame()?.is_none() {
                break;
            }
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crcs() {
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }
}
