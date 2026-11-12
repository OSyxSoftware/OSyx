//! Writing WebM: one VP8 video track, for screen recordings.
//!
//! The file can be written as it is recorded. [`Writer::start`] gives the
//! head of the file, [`Writer::frame`] the bytes to append when a cluster is
//! complete (a cluster starts at each key frame), and [`Writer::finish`]
//! the tail (the last cluster and the cues for seeking) with the places to
//! overwrite near the start: the segment's size, the duration and the
//! position of the cues. A file whose recording was cut off before
//! `finish` still plays: the segment's size is "unknown" until then.

use alloc::vec::Vec;

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const VOID: u32 = 0xEC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;

/// The longest a cluster may run: block times are 16 bits of milliseconds.
const CLUSTER_MS: u64 = 30_000;

fn id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count();
    out.extend_from_slice(&bytes[skip..]);
}

/// A size in the shortest form.
fn size(out: &mut Vec<u8>, n: u64) {
    let len = (1..8).find(|&l| n < (1u64 << (7 * l)) - 1).unwrap_or(8);
    let v = n | 1u64 << (7 * len);
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// A size in eight bytes, for fields overwritten later.
fn size8(n: u64) -> [u8; 8] {
    (n | 1u64 << 56).to_be_bytes()
}

fn element(out: &mut Vec<u8>, i: u32, data: &[u8]) {
    id(out, i);
    size(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn uint(out: &mut Vec<u8>, i: u32, v: u64) {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(7);
    element(out, i, &bytes[skip..]);
}

/// Bytes to overwrite at an offset from the start of the file.
pub struct Patch {
    pub at: u64,
    pub bytes: Vec<u8>,
}

pub struct Writer {
    /// Bytes given out so far.
    written: u64,
    /// Where the segment's data starts.
    segment: u64,
    size_at: u64,
    seek_at: u64,
    duration_at: u64,
    /// The open cluster: its time, its blocks.
    cluster_ms: u64,
    cluster: Vec<u8>,
    /// (time, cluster position in the segment) of each key frame.
    cues: Vec<(u64, u64)>,
    last_ms: u64,
    frames: u64,
}

impl Writer {
    /// A writer and the head of the file, for a `width` x `height` picture.
    pub fn start(width: u32, height: u32) -> (Writer, Vec<u8>) {
        let mut out = Vec::new();
        let mut head = Vec::new();
        uint(&mut head, 0x4286, 1); // EBMLVersion
        uint(&mut head, 0x42F7, 1); // EBMLReadVersion
        uint(&mut head, 0x42F2, 4); // EBMLMaxIDLength
        uint(&mut head, 0x42F3, 8); // EBMLMaxSizeLength
        element(&mut head, 0x4282, b"webm"); // DocType
        uint(&mut head, 0x4287, 2); // DocTypeVersion
        uint(&mut head, 0x4285, 2); // DocTypeReadVersion
        element(&mut out, EBML, &head);
        id(&mut out, SEGMENT);
        let size_at = out.len() as u64;
        out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // unknown
        let segment = out.len() as u64;
        // Room for the seek head that points at the cues.
        let seek_at = out.len() as u64;
        let seek_len = Self::seek_head(0).len();
        id(&mut out, VOID);
        size(&mut out, seek_len as u64 - 2);
        out.resize(out.len() + seek_len - 2, 0);
        let mut info = Vec::new();
        uint(&mut info, 0x2A_D7B1, 1_000_000); // TimecodeScale: milliseconds
        element(&mut info, 0x4D80, b"OSyx"); // MuxingApp
        element(&mut info, 0x5741, b"OSyx screen recorder"); // WritingApp
        id(&mut info, 0x4489); // Duration
        size(&mut info, 8);
        let duration_in_info = info.len();
        info.extend_from_slice(&0f64.to_be_bytes());
        id(&mut out, INFO);
        size(&mut out, info.len() as u64);
        let duration_at = (out.len() + duration_in_info) as u64;
        out.extend_from_slice(&info);
        let mut video = Vec::new();
        uint(&mut video, 0xB0, width as u64); // PixelWidth
        uint(&mut video, 0xBA, height as u64); // PixelHeight
        let mut entry = Vec::new();
        uint(&mut entry, 0xD7, 1); // TrackNumber
        uint(&mut entry, 0x73C5, 1); // TrackUID
        uint(&mut entry, 0x83, 1); // TrackType: video
        uint(&mut entry, 0x9C, 0); // FlagLacing
        element(&mut entry, 0x86, b"V_VP8"); // CodecID
        element(&mut entry, 0xE0, &video);
        let mut tracks = Vec::new();
        element(&mut tracks, 0xAE, &entry);
        element(&mut out, TRACKS, &tracks);
        let w = Writer {
            written: out.len() as u64,
            segment,
            size_at,
            seek_at,
            duration_at,
            cluster_ms: 0,
            cluster: Vec::new(),
            cues: Vec::new(),
            last_ms: 0,
            frames: 0,
        };
        (w, out)
    }

    /// A seek head with one entry: the cues at `position` in the segment.
    /// Always the same length.
    fn seek_head(position: u64) -> Vec<u8> {
        let mut seek = Vec::new();
        element(&mut seek, 0x53AB, &CUES.to_be_bytes()); // SeekID
        id(&mut seek, 0x53AC); // SeekPosition
        size(&mut seek, 8);
        seek.extend_from_slice(&position.to_be_bytes());
        let mut entry = Vec::new();
        element(&mut entry, 0x4DBB, &seek);
        let mut out = Vec::new();
        element(&mut out, SEEK_HEAD, &entry);
        out
    }

    /// The open cluster as bytes, closing it.
    fn flush(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.cluster.is_empty() {
            return out;
        }
        let mut body = Vec::with_capacity(self.cluster.len() + 12);
        uint(&mut body, 0xE7, self.cluster_ms); // Timecode
        body.append(&mut self.cluster);
        element(&mut out, CLUSTER, &body);
        self.written += out.len() as u64;
        out
    }

    /// Adds a frame shown at `ms` from the start; returns bytes to append
    /// to the file (empty while the cluster is open). The first frame must
    /// be a key frame, and times must not go back.
    pub fn frame(&mut self, ms: u64, data: &[u8], key: bool) -> Vec<u8> {
        let ms = ms.max(self.last_ms);
        let mut out = Vec::new();
        if key || self.cluster.is_empty() || ms - self.cluster_ms > CLUSTER_MS {
            out = self.flush();
            self.cluster_ms = ms;
            if key {
                self.cues.push((ms, self.written - self.segment));
            }
        }
        id(&mut self.cluster, 0xA3); // SimpleBlock
        size(&mut self.cluster, data.len() as u64 + 4);
        self.cluster.push(0x81); // track 1
        self.cluster.extend_from_slice(&((ms - self.cluster_ms) as i16).to_be_bytes());
        self.cluster.push(if key { 0x80 } else { 0 });
        self.cluster.extend_from_slice(data);
        self.last_ms = ms;
        self.frames += 1;
        out
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Ends the file `ms` after its start: the bytes to append, and what
    /// to overwrite near the start of the file.
    pub fn finish(mut self, ms: u64) -> (Vec<u8>, Vec<Patch>) {
        let mut out = self.flush();
        let cues_at = self.written - self.segment;
        let mut cues = Vec::new();
        for &(time, position) in &self.cues {
            let mut place = Vec::new();
            uint(&mut place, 0xF7, 1); // CueTrack
            uint(&mut place, 0xF1, position); // CueClusterPosition
            let mut point = Vec::new();
            uint(&mut point, 0xB3, time); // CueTime
            element(&mut point, 0xB7, &place); // CueTrackPositions
            element(&mut cues, 0xBB, &point); // CuePoint
        }
        let before = out.len();
        element(&mut out, CUES, &cues);
        self.written += (out.len() - before) as u64;
        let duration = ms.max(self.last_ms) as f64;
        let patches = alloc::vec![
            Patch { at: self.size_at, bytes: size8(self.written - self.segment).to_vec() },
            Patch { at: self.seek_at, bytes: Self::seek_head(cues_at) },
            Patch { at: self.duration_at, bytes: duration.to_be_bytes().to_vec() },
        ];
        (out, patches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::mkv;
    use crate::vp8::encode::Encoder;
    use crate::vp8::Decoder;

    fn picture(w: usize, h: usize, t: usize) -> Vec<u32> {
        (0..w * h).map(|i| (((i % w) * 255 / w) as u32) << 16 | (((i / w + t * 9) % h * 255 / h) as u32) << 8 | 0x40).collect()
    }

    /// Records `n` frames at 100 ms; returns the file as written and as
    /// finished.
    fn record(n: usize) -> (Vec<u8>, Vec<u8>) {
        let (w, h) = (64, 48);
        let mut enc = Encoder::new(w, h, 24).unwrap();
        let (mut writer, mut file) = Writer::start(w as u32, h as u32);
        for t in 0..n {
            let packet = enc.encode(&picture(w, h, t), w, t % 5 == 0);
            file.extend(writer.frame(t as u64 * 100, &packet.data, packet.key));
        }
        assert_eq!(writer.frames(), n as u64);
        let cut_off = file.clone();
        let (tail, patches) = writer.finish(n as u64 * 100);
        file.extend(tail);
        for p in patches {
            file[p.at as usize..p.at as usize + p.bytes.len()].copy_from_slice(&p.bytes);
        }
        (cut_off, file)
    }

    #[test]
    fn a_recording_demuxes_and_decodes() {
        let (_, file) = record(12);
        let d = mkv::demux(&file).unwrap();
        assert_eq!(d.doc_type, "webm");
        assert_eq!(d.duration_ns, Some(1_200_000_000));
        assert_eq!(d.tracks.len(), 1);
        let t = &d.tracks[0];
        assert_eq!(t.kind, mkv::Kind::Video { w: 64, h: 48 });
        assert_eq!(t.codec, "V_VP8");
        assert_eq!(t.frames.len(), 12);
        let mut dec = Decoder::new();
        for (i, f) in t.frames.iter().enumerate() {
            assert_eq!(f.ns, i as i64 * 100_000_000);
            assert_eq!(f.key, i % 5 == 0, "frame {i}");
            dec.decode(&file[f.start..f.start + f.len]).unwrap();
            assert_eq!(dec.frame().unwrap().w, 64);
        }
    }

    /// For checking against another decoder by hand: with
    /// `OSYX_WEBM_OUT=file.webm` set, writes a larger recording there.
    #[test]
    fn writes_a_sample_for_other_players() {
        extern crate std;
        let Ok(path) = std::env::var("OSYX_WEBM_OUT") else { return };
        let (w, h) = (322, 246);
        let mut enc = Encoder::new(w, h, 20).unwrap();
        let (mut writer, mut file) = Writer::start(w as u32, h as u32);
        let mut raw = Vec::new();
        for t in 0..40usize {
            let mut px = picture(w, h, 0);
            // A box that moves over a still background with sharp stripes.
            for y in 0..h {
                for x in 0..w {
                    if y > h / 2 && x < w / 2 && (x / 3 + y / 5) % 2 == 0 {
                        px[y * w + x] = 0x00f0_f0f0;
                    }
                    if x >= 10 + t * 6 && x < 60 + t * 6 && y >= 20 + t && y < 60 + t {
                        px[y * w + x] = 0x004c_8dff;
                    }
                }
            }
            let packet = enc.encode(&px, w, t % 15 == 0);
            file.extend(writer.frame(t as u64 * 100, &packet.data, packet.key));
            // What a decoder must show, as raw planar 4:2:0.
            let [(y, ys), (u, us), (v, vs)] = enc.reconstruction();
            for r in 0..h {
                raw.extend_from_slice(&y[r * ys..r * ys + w]);
            }
            for (p, s) in [(u, us), (v, vs)] {
                for r in 0..h / 2 {
                    raw.extend_from_slice(&p[r * s..r * s + w / 2]);
                }
            }
        }
        let (tail, patches) = writer.finish(4000);
        file.extend(tail);
        for p in patches {
            file[p.at as usize..p.at as usize + p.bytes.len()].copy_from_slice(&p.bytes);
        }
        std::fs::write(&path, &file).unwrap();
        std::fs::write(std::format!("{path}.yuv"), &raw).unwrap();
    }

    #[test]
    fn a_recording_that_was_cut_off_still_demuxes() {
        let (cut_off, _) = record(12);
        let d = mkv::demux(&cut_off).unwrap();
        // The last cluster (frames 10 and 11) was never written.
        assert_eq!(d.tracks[0].frames.len(), 10);
    }

    #[test]
    fn sizes_are_in_the_shortest_form_and_long_gaps_start_a_cluster() {
        let mut out = Vec::new();
        size(&mut out, 126);
        size(&mut out, 127);
        size(&mut out, 16382);
        size(&mut out, 16383);
        assert_eq!(out, [0xFE, 0x40, 0x7F, 0x7F, 0xFE, 0x20, 0x3F, 0xFF]);
        let (mut writer, _) = Writer::start(16, 16);
        assert!(writer.frame(0, &[1], true).is_empty());
        assert!(writer.frame(20_000, &[2], false).is_empty());
        // More than 30 s after the cluster's start: a new cluster.
        assert!(!writer.frame(40_000, &[3], false).is_empty());
        // Time never goes back.
        writer.frame(10, &[4], false);
        assert_eq!(writer.last_ms, 40_000);
    }
}
