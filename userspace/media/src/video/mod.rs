//! Video files for the player built into Files: WebM and Matroska with VP8
//! video and Vorbis or Opus sound. The demuxer indexes the whole file in
//! memory; the video stream decodes frames to RGBA in order, from the key
//! frame before a seek; the sound track is an [`audio::Decoder`] like any
//! sound file, whose position is the player's clock.

pub mod mkv;

use crate::audio::opus::oggopus::OggOpusDecoder;
use crate::audio::vorbis::VorbisDecoder;
use crate::audio::{self, Info};
use crate::vp8;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// An opened movie.
pub struct Movie {
    data: Arc<Vec<u8>>,
    pub doc_type: String,
    pub duration_ms: Option<u64>,
    pub tracks: Vec<mkv::Track>,
}

/// Whether `data` looks like Matroska or WebM.
pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

impl Movie {
    pub fn open(data: Vec<u8>) -> Result<Movie, String> {
        let d = mkv::demux(&data).map_err(String::from)?;
        let last = d.tracks.iter().filter_map(|t| t.frames.last().map(|f| f.ns.max(0) as u64)).max();
        let duration_ms = d.duration_ns.or(last).map(|ns| ns / 1_000_000);
        Ok(Movie { data: Arc::new(data), doc_type: d.doc_type, duration_ms, tracks: d.tracks })
    }

    fn track(&self, video: bool) -> Option<&mkv::Track> {
        self.tracks.iter().find(|t| {
            !t.encoded
                && !t.frames.is_empty()
                && match t.kind {
                    mkv::Kind::Video { .. } => video,
                    mkv::Kind::Audio { .. } => !video,
                    mkv::Kind::Other => false,
                }
        })
    }

    /// The first video track, if it is one this player decodes.
    pub fn video(&self) -> Option<Result<VideoStream, String>> {
        let t = self.track(true)?;
        if t.codec != "V_VP8" {
            return Some(Err(alloc::format!("{} video is not supported", codec_name(&t.codec))));
        }
        let mkv::Kind::Video { w, h } = t.kind else { return None };
        Some(Ok(VideoStream {
            data: self.data.clone(),
            frames: t.frames.clone(),
            next: 0,
            dec: vp8::Decoder::new(),
            w: w as usize,
            h: h as usize,
            from_ns: i64::MIN,
        }))
    }

    /// The first sound track as a decoder.
    pub fn audio(&self) -> Option<Result<Box<dyn audio::Decoder>, String>> {
        let t = self.track(false)?;
        Some(TrackAudio::new(self.data.clone(), t, self.duration_ms).map(|a| Box::new(a) as Box<dyn audio::Decoder>))
    }

    /// "WebM, VP8 640x360, Opus 48000 Hz stereo" for display.
    pub fn describe(&self) -> String {
        let mut s = String::from(if self.doc_type == "webm" { "WebM" } else { "Matroska" });
        for t in &self.tracks {
            match t.kind {
                mkv::Kind::Video { w, h } => s += &alloc::format!(", {} {w}x{h}", codec_name(&t.codec)),
                mkv::Kind::Audio { rate, channels } => {
                    s += &alloc::format!(", {} {} Hz, {channels} ch", codec_name(&t.codec), rate as u32)
                }
                mkv::Kind::Other => {}
            }
        }
        s
    }
}

fn codec_name(id: &str) -> &str {
    match id {
        "V_VP8" => "VP8",
        "V_VP9" => "VP9",
        "V_AV1" => "AV1",
        "V_MPEG4/ISO/AVC" => "H.264",
        "V_MPEGH/ISO/HEVC" => "H.265",
        "A_VORBIS" => "Vorbis",
        "A_OPUS" => "Opus",
        "A_AAC" => "AAC",
        "A_FLAC" => "FLAC",
        other => other.trim_start_matches("V_").trim_start_matches("A_"),
    }
}

/// A decoded picture and its time.
pub struct Picture {
    pub ms: u64,
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

pub struct VideoStream {
    data: Arc<Vec<u8>>,
    frames: Vec<mkv::Frame>,
    next: usize,
    dec: vp8::Decoder,
    pub w: usize,
    pub h: usize,
    /// Frames before this time are decoded but not returned (after a seek).
    from_ns: i64,
}

impl VideoStream {
    /// The next shown frame, `None` at the end.
    pub fn next_picture(&mut self) -> Option<Result<Picture, String>> {
        while let Some(f) = self.frames.get(self.next).copied() {
            self.next += 1;
            let bytes = &self.data[f.start..(f.start + f.len).min(self.data.len())];
            if let Err(e) = self.dec.decode(bytes) {
                // A damaged frame: wait for the next key frame.
                while self.frames.get(self.next).is_some_and(|f| !f.key) {
                    self.next += 1;
                }
                if self.next >= self.frames.len() {
                    return Some(Err(String::from(e)));
                }
                continue;
            }
            if !self.dec.shown() {
                continue;
            }
            // The frame lasts until the next one: keep the last before the
            // seek target.
            let next_ns = self.frames.get(self.next).map_or(i64::MAX, |n| n.ns);
            if next_ns <= self.from_ns {
                continue;
            }
            let yuv = self.dec.frame()?;
            let px = vp8::to_rgba(&yuv);
            return Some(Ok(Picture { ms: f.ns.max(0) as u64 / 1_000_000, w: yuv.w, h: yuv.h, px }));
        }
        None
    }

    /// Continues from the key frame at or before `ms`; frames before `ms`
    /// are skipped.
    pub fn seek(&mut self, ms: u64) {
        let ns = ms as i64 * 1_000_000;
        let k = self.frames.iter().rposition(|f| f.key && f.ns <= ns).unwrap_or(0);
        self.next = k;
        self.dec = vp8::Decoder::new();
        self.from_ns = ns;
    }
}

enum Codec {
    Vorbis(Box<VorbisDecoder>),
    Opus(Box<OggOpusDecoder>),
}

/// A Vorbis or Opus track played like a sound file.
struct TrackAudio {
    data: Arc<Vec<u8>>,
    frames: Vec<mkv::Frame>,
    /// Stream position of the first sample each frame's decoding gives.
    positions: Vec<i64>,
    codec: Codec,
    info: Info,
    next: usize,
    /// First sample to hand out.
    want: i64,
    preroll: i64,
}

impl TrackAudio {
    fn new(data: Arc<Vec<u8>>, t: &mkv::Track, duration_ms: Option<u64>) -> Result<TrackAudio, String> {
        let (codec, rate, channels, codec_name) = match t.codec.as_str() {
            "A_VORBIS" => {
                let h = mkv::xiph_headers(&t.private).map_err(String::from)?;
                if h.len() != 3 {
                    return Err(String::from("bad Vorbis headers"));
                }
                let v = VorbisDecoder::from_headers(h[0], h[1], h[2])?;
                let i = audio::Decoder::info(&v).clone();
                (Codec::Vorbis(Box::new(v)), i.rate, i.channels, "Vorbis")
            }
            "A_OPUS" => {
                let o = OggOpusDecoder::from_head(&t.private)?;
                let ch = audio::Decoder::info(&o).channels;
                (Codec::Opus(Box::new(o)), 48000, ch, "Opus")
            }
            other => return Err(alloc::format!("{} sound is not supported", codec_name(other))),
        };
        let to_samples = |ns: i64| (ns as i128 * rate as i128 / 1_000_000_000) as i64;
        let delay = match &codec {
            Codec::Opus(o) if t.codec_delay_ns == 0 => o.pre_skip() as i64,
            _ => to_samples(t.codec_delay_ns as i64),
        };
        // Exact positions from the packets' own lengths, anchored at the
        // first packet that gives samples: block times are rounded (to the
        // millisecond, typically), and a start less than that before zero
        // is rounding, not delay.
        let bytes = |f: &mkv::Frame| &data[f.start..(f.start + f.len).min(data.len())];
        let mut lengths = Vec::with_capacity(t.frames.len());
        let mut prev_bs = None;
        for f in &t.frames {
            lengths.push(match &codec {
                Codec::Opus(_) => crate::audio::opus::packet_samples(bytes(f)) as i64,
                Codec::Vorbis(v) => match v.blocksize_of(bytes(f)) {
                    Some(bs) => {
                        let n = prev_bs.map_or(0, |p: usize| (p / 4 + bs / 4) as i64);
                        prev_bs = Some(bs);
                        n
                    }
                    None => 0,
                },
            });
        }
        let mut positions = vec![0i64; t.frames.len()];
        if let Some(k0) = lengths.iter().position(|&n| n > 0) {
            let ms = rate as i64 / 1000 + 1;
            let mut p = to_samples(t.frames[k0].ns) - delay;
            if p < 0 && p > -ms {
                p = 0;
            }
            // Frames before the first that gives samples share its position.
            for v in positions.iter_mut().take(k0) {
                *v = p;
            }
            for k in k0..t.frames.len() {
                positions[k] = p;
                p += lengths[k];
            }
        }
        // Longer than the 80 ms the format asks for: the decoder converges
        // further before the wanted sample.
        let preroll = to_samples(t.seek_preroll_ns.max(250_000_000) as i64);
        let frames = duration_ms.map(|ms| ms * rate as u64 / 1000);
        Ok(TrackAudio {
            data,
            frames: t.frames.clone(),
            positions,
            codec,
            info: Info { rate, channels, frames, codec: String::from(codec_name) },
            next: 0,
            want: 0,
            preroll,
        })
    }

    fn ns_to_samples(&self, ns: i64) -> i64 {
        (ns as i128 * self.info.rate as i128 / 1_000_000_000) as i64
    }
}

impl audio::Decoder for TrackAudio {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        let ch = self.info.channels;
        loop {
            let k = self.next;
            let Some(f) = self.frames.get(k).copied() else { return Ok(None) };
            self.next += 1;
            let bytes = &self.data[f.start..(f.start + f.len).min(self.data.len())];
            let block = match &mut self.codec {
                Codec::Vorbis(v) => v.decode_raw(bytes),
                Codec::Opus(o) => o.decode_raw(bytes)?,
            };
            if block.is_empty() {
                continue;
            }
            let mut n = (block.len() / ch) as i64;
            if f.discard_ns > 0 {
                n = (n - self.ns_to_samples(f.discard_ns)).max(0);
            }
            let start = self.positions[k];
            let lo = (self.want.max(0) - start).clamp(0, n);
            let hi = match self.info.frames {
                Some(total) => (total as i64 - start).clamp(0, n),
                None => n,
            };
            if lo >= hi {
                continue;
            }
            return Ok(Some(block[lo as usize * ch..hi as usize * ch].to_vec()));
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = self.info.frames.map_or(frame, |t| frame.min(t)) as i64;
        let goal = frame - self.preroll;
        self.next = self.positions.iter().rposition(|&p| p <= goal).unwrap_or(0);
        self.want = frame;
        match &mut self.codec {
            Codec::Vorbis(v) => v.reset_raw(),
            Codec::Opus(o) => o.reset_raw(),
        }
        Ok(frame as u64)
    }
}
