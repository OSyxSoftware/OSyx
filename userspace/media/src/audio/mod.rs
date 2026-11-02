//! Sound files: format detection, decoders and conversion to the sound
//! card's format.
//!
//! A decoder turns a whole file held in memory into blocks of interleaved
//! samples in -1.0..=1.0 at the file's own rate and channel count.
//! [`resample::Converter`] then turns them into 48 kHz stereo 16-bit frames
//! for `/dev/audio`.

pub mod flac;
pub mod mp3;
mod mp3_tables;
pub mod ogg;
pub mod resample;
pub mod vorbis;
mod vorbis_table;
pub mod wav;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// Frames per second.
    pub rate: u32,
    pub channels: usize,
    /// Length in frames, when the file says or it can be worked out.
    pub frames: Option<u64>,
    /// The codec and container, for display ("FLAC", "MP3", ...).
    pub codec: String,
}

impl Info {
    /// Length in milliseconds, if known.
    pub fn duration_ms(&self) -> Option<u64> {
        self.frames.map(|f| f * 1000 / self.rate.max(1) as u64)
    }
}

pub trait Decoder: Send {
    fn info(&self) -> &Info;

    /// The next block of interleaved samples (`info().channels` per frame),
    /// `None` at the end of the stream.
    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String>;

    /// Positions the decoder so that the next block starts at `frame`
    /// (clamped to the length). Returns the frame it starts at.
    fn seek(&mut self, frame: u64) -> Result<u64, String>;
}

/// Which decoder reads `data`, judged by its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    Flac,
    Mp3,
    Ogg,
}

pub fn sniff(data: &[u8]) -> Option<Format> {
    if data.len() >= 12 && (&data[..4] == b"RIFF" || &data[..4] == b"RF64") && &data[8..12] == b"WAVE" {
        return Some(Format::Wav);
    }
    if data.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if data.starts_with(b"OggS") {
        return Some(Format::Ogg);
    }
    // MP3: an ID3v2 tag, or a frame header right at the start.
    if data.starts_with(b"ID3") || mp3::find_frame(data, 0) == Some(0) {
        return Some(Format::Mp3);
    }
    // FLAC with an ID3v2 tag in front.
    if let Some(skip) = id3v2_len(data) {
        if data.get(skip..skip + 4) == Some(b"fLaC") {
            return Some(Format::Flac);
        }
    }
    None
}

/// The size of an ID3v2 tag at the start of `data`, footer included.
pub fn id3v2_len(data: &[u8]) -> Option<usize> {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return None;
    }
    let size = data[6..10].iter().fold(0usize, |a, &b| (a << 7) | (b & 0x7F) as usize);
    let footer = if data[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// A decoder for `data`.
pub fn open(data: Vec<u8>) -> Result<Box<dyn Decoder>, String> {
    match sniff(&data) {
        Some(Format::Wav) => Ok(Box::new(wav::WavDecoder::new(data)?)),
        Some(Format::Flac) => Ok(Box::new(flac::FlacDecoder::new(data)?)),
        Some(Format::Mp3) => Ok(Box::new(mp3::Mp3Decoder::new(data)?)),
        Some(Format::Ogg) => ogg::open(data),
        None => Err(String::from("not a sound file OSyx can play (WAV, FLAC, MP3 and Ogg Vorbis are supported)")),
    }
}
