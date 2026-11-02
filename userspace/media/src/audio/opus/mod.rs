//! Opus (RFC 6716): SILK, CELT and hybrid frames, mode transitions, and
//! Ogg Opus files (RFC 7845). Output is always 48 kHz.

pub mod celt;
mod celt_tables;
pub mod oggopus;
pub mod range;
pub mod silk;
mod silk_tables;

use alloc::vec;
use alloc::vec::Vec;
use celt::Celt;
use celt_tables::WINDOW;
use range::RangeDecoder;
use silk::{DecControl, Silk};

/// Samples per channel of the longest packet (120 ms at 48 kHz).
pub const MAX_PACKET_SAMPLES: usize = 5760;

const F20: usize = 960;
const F10: usize = 480;
const F5: usize = 240;
const F2_5: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Silk,
    Hybrid,
    Celt,
}

/// Audio bandwidth: narrow, medium, wide, super-wide, full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bandwidth {
    Narrow,
    Medium,
    Wide,
    SuperWide,
    Full,
}

/// What the first byte of a packet says.
#[derive(Clone, Copy, Debug)]
pub struct Toc {
    pub mode: Mode,
    pub bandwidth: Bandwidth,
    /// Samples per frame at 48 kHz.
    pub frame_size: usize,
    pub stereo: bool,
}

pub fn toc(b: u8) -> Toc {
    let (mode, bandwidth, frame_size);
    if b & 0x80 != 0 {
        mode = Mode::Celt;
        bandwidth = match (b >> 5) & 3 {
            0 => Bandwidth::Narrow,
            1 => Bandwidth::Wide,
            2 => Bandwidth::SuperWide,
            _ => Bandwidth::Full,
        };
        frame_size = (48000 << ((b >> 3) & 3)) / 400;
    } else if b & 0x60 == 0x60 {
        mode = Mode::Hybrid;
        bandwidth = if b & 0x10 != 0 { Bandwidth::Full } else { Bandwidth::SuperWide };
        frame_size = if b & 0x08 != 0 { 960 } else { 480 };
    } else {
        mode = Mode::Silk;
        bandwidth = match (b >> 5) & 3 {
            0 => Bandwidth::Narrow,
            1 => Bandwidth::Medium,
            _ => Bandwidth::Wide,
        };
        frame_size = match (b >> 3) & 3 {
            3 => 2880,
            s => (48000 << s) / 100,
        };
    }
    Toc { mode, bandwidth, frame_size, stereo: b & 4 != 0 }
}

fn parse_size(data: &[u8]) -> Option<(usize, usize)> {
    match data {
        [] => None,
        [b, ..] if *b < 252 => Some((*b as usize, 1)),
        [_] => None,
        [a, b, ..] => Some((4 * *b as usize + *a as usize, 2)),
    }
}

/// The frames of a packet (RFC 6716 section 3.2).
pub fn frames(packet: &[u8]) -> Option<(Toc, Vec<&[u8]>)> {
    parse_packet(packet, false).map(|(t, f, _)| (t, f))
}

/// The frames of a packet, and the bytes it takes. A self-delimited
/// packet (Appendix B, used for all but the last stream of a multistream
/// packet) also gives the size of its last frame.
pub fn parse_packet(packet: &[u8], self_delimited: bool) -> Option<(Toc, Vec<&[u8]>, usize)> {
    let t = toc(*packet.first()?);
    let mut data = &packet[1..];
    let mut sizes = Vec::new();
    let mut cbr = false;
    let count;
    let mut last = data.len();
    match packet[0] & 3 {
        0 => count = 1,
        1 => {
            count = 2;
            cbr = true;
            if !self_delimited {
                if data.len() & 1 != 0 {
                    return None;
                }
                sizes.push(data.len() / 2);
                last = data.len() / 2;
            }
        }
        2 => {
            count = 2;
            let (s, n) = parse_size(data)?;
            data = &data[n..];
            if s > data.len() {
                return None;
            }
            sizes.push(s);
            last = data.len() - s;
        }
        _ => {
            let ch = *data.first()?;
            data = &data[1..];
            count = (ch & 0x3F) as usize;
            if count == 0 || t.frame_size * count > MAX_PACKET_SAMPLES {
                return None;
            }
            let mut len = data.len() as isize;
            if ch & 0x40 != 0 {
                // Padding, counted down from the length (RFC 8251 section 4).
                loop {
                    let p = *data.first()?;
                    data = &data[1..];
                    len -= 1;
                    len -= if p == 255 { 254 } else { p as isize };
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return None;
            }
            let mut len = len as usize;
            cbr = ch & 0x80 == 0;
            if !cbr {
                let mut l = len as isize;
                for _ in 0..count - 1 {
                    let (s, n) = parse_size(data)?;
                    data = &data[n..];
                    len = len.checked_sub(n)?;
                    if s > len {
                        return None;
                    }
                    sizes.push(s);
                    l -= (n + s) as isize;
                }
                if l < 0 {
                    return None;
                }
                last = l as usize;
            } else if !self_delimited {
                last = len / count;
                if last * count != len {
                    return None;
                }
                sizes.extend(core::iter::repeat_n(last, count - 1));
            } else {
                last = len;
            }
        }
    }
    if self_delimited {
        let (s, n) = parse_size(data)?;
        data = &data[n..];
        if cbr {
            if s * count > data.len() {
                return None;
            }
            sizes.clear();
            sizes.extend(core::iter::repeat_n(s, count - 1));
        } else if s > last {
            return None;
        }
        last = s;
    } else if last > 1275 {
        return None;
    }
    sizes.push(last);
    let header = packet.len() - data.len();
    let mut out = Vec::with_capacity(sizes.len());
    let mut used = header;
    for s in sizes {
        out.push(data.get(..s)?);
        data = &data[s..];
        used += s;
    }
    Some((t, out, used))
}

/// Samples per channel at 48 kHz in a packet, from its first bytes.
pub fn packet_samples(packet: &[u8]) -> usize {
    let Some(&b) = packet.first() else { return 0 };
    let n = match b & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => packet.get(1).map_or(0, |c| (c & 0x3F) as usize),
    };
    n * toc(b).frame_size
}

/// Cross-fades `a` into `b` over `n` samples with the squared CELT window.
fn smooth_fade(a: &[f32], b: &[f32], out: &mut [f32], n: usize, channels: usize) {
    for c in 0..channels {
        for i in 0..n {
            let w = WINDOW[i] * WINDOW[i];
            let k = i * channels + c;
            out[k] = w * b[k] + (1.0 - w) * a[k];
        }
    }
}

/// One Opus stream (one or two channels).
pub struct OpusDecoder {
    channels: usize,
    celt: Celt,
    silk: Silk,
    ctl: DecControl,
    mode: Mode,
    prev_mode: Option<Mode>,
    bandwidth: Bandwidth,
    stream_channels: usize,
    frame_size: usize,
    prev_redundancy: bool,
    /// Final range of the last frame, which encoders report for checking.
    pub range_final: u32,
}

impl OpusDecoder {
    pub fn new(channels: usize) -> OpusDecoder {
        OpusDecoder {
            channels,
            celt: Celt::new(channels),
            silk: Silk::new(),
            ctl: DecControl::new(channels),
            mode: Mode::Celt,
            prev_mode: None,
            bandwidth: Bandwidth::Full,
            stream_channels: channels,
            frame_size: F2_5,
            prev_redundancy: false,
            range_final: 0,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.prev_mode = None;
        self.frame_size = F2_5;
        self.prev_redundancy = false;
        self.range_final = 0;
    }

    /// Decodes a packet into interleaved samples in `pcm` (room for
    /// MAX_PACKET_SAMPLES frames); returns the frames decoded. An empty
    /// packet is a lost one of `lost_frames` frames.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [f32], lost_frames: usize) -> Result<usize, &'static str> {
        if packet.is_empty() {
            return self.decode_frame(None, pcm, lost_frames);
        }
        self.decode_packet(packet, false, pcm).map(|(n, _)| n)
    }

    /// Decodes one packet, self-delimited or not; returns the frames
    /// decoded and the bytes of `packet` it took.
    pub fn decode_packet(
        &mut self,
        packet: &[u8],
        self_delimited: bool,
        pcm: &mut [f32],
    ) -> Result<(usize, usize), &'static str> {
        let (t, frames, used) = parse_packet(packet, self_delimited).ok_or("invalid Opus packet")?;
        self.mode = t.mode;
        self.bandwidth = t.bandwidth;
        self.frame_size = t.frame_size;
        self.stream_channels = if t.stereo { 2 } else { 1 };
        let mut done = 0;
        for f in frames {
            let n = self.decode_frame(Some(f), &mut pcm[done * self.channels..], MAX_PACKET_SAMPLES - done)?;
            done += n;
        }
        Ok((done, used))
    }

    /// opus_decode_frame of the reference decoder.
    fn decode_frame(&mut self, data: Option<&[u8]>, pcm: &mut [f32], frame_size: usize) -> Result<usize, &'static str> {
        let ch = self.channels;
        let mut frame_size = frame_size;
        let data = data.filter(|d| d.len() > 1);
        let mut len = data.map_or(0, |d| d.len());
        if data.is_none() {
            frame_size = frame_size.min(self.frame_size);
        }
        let audiosize;
        let mode;
        let mut own = None;
        if let Some(d) = data {
            audiosize = self.frame_size;
            mode = self.mode;
            own = Some(RangeDecoder::new(d));
        } else {
            audiosize = frame_size;
            match self.prev_mode {
                None => {
                    pcm[..audiosize * ch].fill(0.0);
                    return Ok(audiosize);
                }
                Some(m) => mode = m,
            }
        }
        // CELT or hybrid concealment of more than 20 ms: 20 ms at a time.
        if data.is_none() && frame_size > F20 && mode != Mode::Silk {
            let mut n = 0;
            while n < frame_size {
                let r = self.decode_frame(None, &mut pcm[n * ch..], F20)?;
                if r != F20 {
                    return Err("concealment failed");
                }
                n += F20;
            }
            return Ok(frame_size);
        }
        let mut pcm_transition = vec![0.0f32; F5 * ch];
        let mut transition = false;
        if data.is_some()
            && self.prev_mode.is_some()
            && ((mode == Mode::Celt && self.prev_mode != Some(Mode::Celt) && !self.prev_redundancy)
                || (mode != Mode::Celt && self.prev_mode == Some(Mode::Celt)))
        {
            transition = true;
            if mode == Mode::Celt {
                self.decode_frame(None, &mut pcm_transition, F5.min(audiosize))?;
            }
        }
        if audiosize > frame_size {
            return Err("output buffer too small");
        }
        let frame_size = audiosize;

        let mut pcm_silk = vec![0i16; F10.max(frame_size) * ch];
        let mut redundant_audio = vec![0.0f32; F5 * ch];

        if mode != Mode::Celt {
            if self.prev_mode == Some(Mode::Celt) {
                self.silk.reset();
            }
            self.ctl.payload_size_ms = 10.max(1000 * audiosize / 48000);
            if data.is_some() {
                self.ctl.channels_internal = self.stream_channels;
                self.ctl.internal_rate = match (mode, self.bandwidth) {
                    (Mode::Silk, Bandwidth::Narrow) => 8000,
                    (Mode::Silk, Bandwidth::Medium) => 12000,
                    _ => 16000,
                };
            }
            let lost = data.is_none();
            let mut decoded = 0;
            loop {
                let first = decoded == 0;
                let out = &mut pcm_silk[decoded * ch..];
                let n = match self.silk.decode(&mut self.ctl, lost, first, own.as_mut(), out) {
                    Ok(n) => n,
                    Err(e) => {
                        if lost {
                            out[..frame_size * ch].fill(0);
                            frame_size
                        } else {
                            return Err(e);
                        }
                    }
                };
                decoded += n;
                if decoded >= frame_size {
                    break;
                }
            }
        }

        let mut redundancy = false;
        let mut redundancy_bytes = 0;
        let mut celt_to_silk = false;
        if let Some(dec) = own.as_mut() {
            if mode != Mode::Celt && dec.tell() + 17 + 20 * (self.mode == Mode::Hybrid) as i32 <= 8 * len as i32 {
                redundancy = if mode == Mode::Hybrid { dec.bit_logp(12) } else { true };
                if redundancy {
                    celt_to_silk = dec.bit_logp(1);
                    redundancy_bytes = if mode == Mode::Hybrid {
                        dec.uint(256) as usize + 2
                    } else {
                        len.saturating_sub((dec.tell() as usize + 7) >> 3)
                    };
                    if redundancy_bytes > len || (len - redundancy_bytes) * 8 < dec.tell() as usize {
                        len = 0;
                        redundancy_bytes = 0;
                        redundancy = false;
                    } else {
                        len -= redundancy_bytes;
                    }
                    dec.shrink(redundancy_bytes);
                }
            }
        }
        let start_band = if mode != Mode::Celt { 17 } else { 0 };
        self.celt.end = match self.bandwidth {
            Bandwidth::Narrow => 13,
            Bandwidth::Medium | Bandwidth::Wide => 17,
            Bandwidth::SuperWide => 19,
            Bandwidth::Full => 21,
        };
        self.celt.stream_channels = self.stream_channels;

        if redundancy {
            transition = false;
        }
        if transition && mode != Mode::Celt {
            self.decode_frame(None, &mut pcm_transition, F5.min(audiosize))?;
        }
        let mut redundant_rng = 0;
        let red = data.map(|d| &d[len..len + redundancy_bytes]);
        if redundancy && celt_to_silk {
            self.celt.start = 0;
            self.celt.decode(None, red, redundancy_bytes, &mut redundant_audio, F5)?;
            redundant_rng = self.celt.rng;
        }
        self.celt.start = start_band;

        if mode != Mode::Silk {
            let celt_frame = F20.min(frame_size);
            if Some(mode) != self.prev_mode && self.prev_mode.is_some() && !self.prev_redundancy {
                self.celt.reset();
            }
            self.celt.decode(own.as_mut(), data, len, pcm, celt_frame)?;
        } else {
            pcm[..frame_size * ch].fill(0.0);
            if self.prev_mode == Some(Mode::Hybrid) && !(redundancy && celt_to_silk && self.prev_redundancy) {
                self.celt.start = 0;
                let silence = [0xFFu8, 0xFF];
                self.celt.decode(None, Some(&silence), 2, pcm, F2_5)?;
            }
        }

        if mode != Mode::Celt {
            for (o, &s) in pcm[..frame_size * ch].iter_mut().zip(&pcm_silk) {
                *o += s as f32 * (1.0 / 32768.0);
            }
        }

        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt.start = 0;
            self.celt.decode(None, red, redundancy_bytes, &mut redundant_audio, F5)?;
            redundant_rng = self.celt.rng;
            let at = ch * (frame_size - F2_5);
            let a = pcm[at..at + F2_5 * ch].to_vec();
            smooth_fade(&a, &redundant_audio[ch * F2_5..], &mut pcm[at..], F2_5, ch);
        }
        if redundancy && celt_to_silk {
            pcm[..F2_5 * ch].copy_from_slice(&redundant_audio[..F2_5 * ch]);
            let b = pcm[ch * F2_5..ch * 2 * F2_5].to_vec();
            smooth_fade(&redundant_audio[ch * F2_5..], &b, &mut pcm[ch * F2_5..], F2_5, ch);
        }
        if transition {
            if audiosize >= F5 {
                pcm[..F2_5 * ch].copy_from_slice(&pcm_transition[..F2_5 * ch]);
                let b = pcm[ch * F2_5..ch * 2 * F2_5].to_vec();
                smooth_fade(&pcm_transition[ch * F2_5..], &b, &mut pcm[ch * F2_5..], F2_5, ch);
            } else {
                let b = pcm[..F2_5 * ch].to_vec();
                smooth_fade(&pcm_transition, &b, pcm, F2_5, ch);
            }
        }
        self.range_final = match own.as_ref() {
            Some(d) if len > 1 => d.range() ^ redundant_rng,
            _ => 0,
        };
        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        Ok(audiosize)
    }
}
