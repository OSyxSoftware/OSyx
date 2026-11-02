//! Ogg Opus files (RFC 7845): the identification header, pre-skip and
//! output gain, multistream packets with channel mapping families 0, 1 and
//! 255, positions from granules, and seeking with 80 ms of pre-roll.

use super::{packet_samples, OpusDecoder, MAX_PACKET_SAMPLES};
use crate::audio::ogg::Packets;
use crate::audio::{Decoder, Info};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Decoded samples needed before a seek target for the decoder to settle.
const PREROLL: u64 = 3840;

struct Head {
    channels: usize,
    pre_skip: u64,
    gain_db_q8: i16,
    streams: usize,
    coupled: usize,
    mapping: Vec<u8>,
    family: u8,
}

fn parse_head(p: &[u8]) -> Result<Head, String> {
    if p.len() < 19 || !p.starts_with(b"OpusHead") {
        return Err(String::from("damaged Opus header"));
    }
    if p[8] >> 4 != 0 {
        return Err(format!("unsupported Opus header version {}", p[8]));
    }
    let channels = p[9] as usize;
    let pre_skip = u16::from_le_bytes([p[10], p[11]]) as u64;
    let gain_db_q8 = i16::from_le_bytes([p[16], p[17]]);
    let family = p[18];
    if channels == 0 {
        return Err(String::from("Opus stream without channels"));
    }
    let (streams, coupled, mapping) = if family == 0 {
        if channels > 2 {
            return Err(String::from("damaged Opus header"));
        }
        (1, channels - 1, (0..channels as u8).collect())
    } else {
        if p.len() < 21 + channels {
            return Err(String::from("damaged Opus header"));
        }
        let (s, c) = (p[19] as usize, p[20] as usize);
        if s == 0 || c > s || s + c > 255 || (family == 1 && channels > 8) {
            return Err(String::from("damaged Opus channel mapping"));
        }
        let m = p[21..21 + channels].to_vec();
        if m.iter().any(|&v| v != 255 && v as usize >= s + c) {
            return Err(String::from("damaged Opus channel mapping"));
        }
        (s, c, m)
    };
    Ok(Head { channels, pre_skip, gain_db_q8, streams, coupled, mapping, family })
}

/// Vorbis channel order (family 1) to the WAVE order (FL FR FC LFE BL BR
/// SL SR): output channel i takes coded channel order[i].
fn wave_order(family: u8, channels: usize) -> Vec<usize> {
    if family != 1 {
        return (0..channels).collect();
    }
    match channels {
        3 => vec![0, 2, 1],
        5 => vec![0, 2, 1, 3, 4],
        6 => vec![0, 2, 1, 5, 3, 4],
        7 => vec![0, 2, 1, 6, 5, 3, 4],
        8 => vec![0, 2, 1, 7, 5, 6, 3, 4],
        _ => (0..channels).collect(),
    }
}

pub struct OggOpusDecoder {
    data: Vec<u8>,
    info: Info,
    packets: Packets,
    head: Head,
    first_audio_page: usize,
    decoders: Vec<OpusDecoder>,
    order: Vec<usize>,
    gain: f32,
    /// Packets read ahead (to place the first one in time after a seek).
    queue: VecDeque<Vec<u8>>,
    /// Position (in the granule's 48 kHz samples) of the next packet's
    /// first sample, once known.
    pos: Option<i64>,
    /// First wanted sample and the end of the stream.
    from: i64,
    end: i64,
    scratch: Vec<Vec<f32>>,
}

impl OggOpusDecoder {
    pub fn new(data: Vec<u8>, serial: u32) -> Result<OggOpusDecoder, String> {
        let mut packets = Packets::new(&data, serial);
        let id = packets.next(&data).ok_or("Opus stream without headers")?;
        let head = parse_head(&id.data)?;
        let tags = packets.next(&data).ok_or("Opus stream without comment header")?;
        if !tags.data.starts_with(b"OpusTags") {
            return Err(String::from("Opus comment header missing"));
        }
        let first_audio_page = tags.page + 1;
        let end = packets.pages.iter().rev().find(|p| p.granule >= 0).map_or(0, |p| p.granule);
        let decoders = (0..head.streams).map(|s| OpusDecoder::new(if s < head.coupled { 2 } else { 1 })).collect();
        let gain = libm::powf(10.0, head.gain_db_q8 as f32 / (20.0 * 256.0));
        let codec =
            if head.streams > 1 { format!("Ogg Opus ({} streams)", head.streams) } else { String::from("Ogg Opus") };
        let mut dec = OggOpusDecoder {
            info: Info { rate: 48000, channels: head.channels, frames: None, codec },
            order: wave_order(head.family, head.channels),
            scratch: (0..head.streams).map(|_| vec![0.0; MAX_PACKET_SAMPLES * 2]).collect(),
            data,
            packets,
            head,
            first_audio_page,
            decoders,
            gain,
            queue: VecDeque::new(),
            pos: None,
            from: 0,
            end,
        };
        dec.restart();
        let start = dec.locate().unwrap_or(0);
        dec.from = start.max(0) + dec.head.pre_skip as i64;
        dec.info.frames = Some((dec.end - dec.from).max(0) as u64);
        Ok(dec)
    }

    fn restart(&mut self) {
        self.packets.seek_page(&self.data, self.first_audio_page);
        self.queue.clear();
        self.pos = None;
        for d in &mut self.decoders {
            d.reset();
        }
    }

    /// At the start of the stream: reads ahead to the first packet that
    /// ends a page with a granule position and works out where the first
    /// packet starts. A first page that holds more samples than its granule
    /// says is end trimming (it is also the last page), so the stream starts
    /// at 0 (RFC 7845 section 4.5).
    fn locate(&mut self) -> Option<i64> {
        let mut samples = self.queue.iter().map(|p| packet_samples(p) as i64).sum::<i64>();
        loop {
            let p = self.packets.next(&self.data)?;
            samples += packet_samples(&p.data) as i64;
            self.queue.push_back(p.data);
            if let Some(g) = p.granule {
                let start = (g - samples).max(0);
                self.pos = Some(start);
                return Some(start);
            }
        }
    }

    /// Samples of the packet that starts on page `p` and continues on the
    /// next one (0 if none does).
    fn continued_samples(&self, p: usize) -> usize {
        let pages = &self.packets.pages;
        let pg = pages[p];
        let segs = &self.data[pg.segs..pg.segs + pg.nsegs];
        if segs.last().is_none_or(|&s| s < 255) {
            return 0;
        }
        // The packet starts after the last segment that ends one.
        let Some(last_end) = segs.iter().rposition(|&s| s < 255) else { return 0 };
        let off: usize = segs[..=last_end].iter().map(|&s| s as usize).sum();
        let mut head = [0u8; 2];
        let mut n = 0;
        for &b in self.data[pg.body + off..pg.body + pg.len].iter().take(2) {
            head[n] = b;
            n += 1;
        }
        if n < 2 {
            if let Some(next) = pages.get(p + 1) {
                if let Some(&b) = self.data.get(next.body) {
                    head[n] = b;
                }
            }
        }
        packet_samples(&head)
    }

    fn next_packet(&mut self) -> Option<Vec<u8>> {
        if let Some(p) = self.queue.pop_front() {
            return Some(p);
        }
        self.packets.next(&self.data).map(|p| p.data)
    }

    /// Decodes one Ogg packet into interleaved output channels.
    fn decode_packet(&mut self, pkt: &[u8]) -> Result<Vec<f32>, String> {
        let streams = self.head.streams;
        let mut off = 0;
        let mut frames = 0;
        for s in 0..streams {
            let sd = s + 1 < streams;
            let rest = pkt.get(off..).unwrap_or(&[]);
            let (n, used) = if rest.is_empty() {
                let n = self.decoders[s].decode(&[], &mut self.scratch[s], frames.max(960))?;
                (n, 0)
            } else {
                self.decoders[s].decode_packet(rest, sd, &mut self.scratch[s])?
            };
            if s == 0 {
                frames = n;
            } else if n != frames {
                return Err(String::from("Opus streams of different lengths"));
            }
            off += used;
        }
        let ch = self.head.channels;
        let mut out = vec![0.0f32; frames * ch];
        for (o, &src) in self.order.iter().enumerate() {
            let m = self.head.mapping[src] as usize;
            if m == 255 {
                continue;
            }
            let (s, c, sc) = if m < 2 * self.head.coupled {
                (m / 2, m % 2, 2)
            } else {
                (self.head.coupled + m - 2 * self.head.coupled, 0, 1)
            };
            let buf = &self.scratch[s];
            for i in 0..frames {
                out[i * ch + o] = buf[i * sc + c] * self.gain;
            }
        }
        Ok(out)
    }
}

impl Decoder for OggOpusDecoder {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        let ch = self.head.channels;
        loop {
            if self.pos.is_none() && self.locate().is_none() {
                return Ok(None);
            }
            let pos = self.pos.unwrap();
            if pos >= self.end {
                return Ok(None);
            }
            let Some(pkt) = self.next_packet() else { return Ok(None) };
            let block = self.decode_packet(&pkt)?;
            let n = (block.len() / ch) as i64;
            self.pos = Some(pos + n);
            let lo = self.from.max(pos);
            let hi = self.end.min(pos + n);
            if lo >= hi {
                continue;
            }
            return Ok(Some(block[(lo - pos) as usize * ch..(hi - pos) as usize * ch].to_vec()));
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = frame.min(self.info.frames.unwrap_or(0));
        let target = self.from + frame as i64;
        let want = target - PREROLL as i64;
        // The last page that ends before the pre-roll point.
        let pages = &self.packets.pages;
        let mut start = None;
        let (mut lo, mut hi) = (self.first_audio_page, pages.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            let g = pages[..=mid].iter().rev().find(|p| p.granule >= 0).map_or(-1, |p| p.granule);
            if g >= 0 && g <= want {
                start = Some(mid);
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        match start {
            Some(p) if p + 1 < pages.len() => {
                let pos = pages[p].granule + self.continued_samples(p) as i64;
                self.packets.seek_page(&self.data, p + 1);
                self.queue.clear();
                self.pos = Some(pos);
                for d in &mut self.decoders {
                    d.reset();
                }
            }
            _ => self.restart(),
        }
        // Decode whole packets up to the target and drop them; next_block
        // trims the rest.
        self.from = target;
        loop {
            if self.pos.is_none() && self.locate().is_none() {
                break;
            }
            let pos = self.pos.unwrap();
            if self.queue.is_empty() {
                match self.packets.next(&self.data) {
                    Some(p) => self.queue.push_back(p.data),
                    None => break,
                }
            }
            let n = packet_samples(&self.queue[0]) as i64;
            if pos + n > target || pos >= self.end {
                break;
            }
            let pkt = self.queue.pop_front().unwrap();
            let block = self.decode_packet(&pkt)?;
            self.pos = Some(pos + (block.len() / self.head.channels) as i64);
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads() {
        let mut h = b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00".to_vec();
        let p = parse_head(&h).unwrap();
        assert_eq!((p.channels, p.pre_skip, p.streams, p.coupled), (2, 312, 1, 1));
        h[18] = 1;
        h[9] = 6;
        h.extend_from_slice(&[4, 2, 0, 4, 1, 2, 3, 5]);
        let p = parse_head(&h).unwrap();
        assert_eq!((p.channels, p.streams, p.coupled), (6, 4, 2));
        assert_eq!(wave_order(1, 6), vec![0, 2, 1, 5, 3, 4]);
        h[22] = 9;
        assert!(parse_head(&h).is_err(), "mapping past the coded channels");
    }
}
