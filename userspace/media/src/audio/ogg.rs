//! Ogg (RFC 3533): pages with their CRC, packets split over segments and
//! pages, and the logical stream of the first codec OSyx can play.

use super::vorbis::VorbisDecoder;
use super::Decoder;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// One page header.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    /// Offset of `OggS`.
    pub at: usize,
    /// Offset of the first byte of data.
    pub body: usize,
    pub len: usize,
    pub continued: bool,
    pub first: bool,
    pub last: bool,
    /// -1 when no packet ends on this page.
    pub granule: i64,
    pub serial: u32,
    /// Segment table position and count.
    pub segs: usize,
    pub nsegs: usize,
}

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut r = (i as u32) << 24;
        for _ in 0..8 {
            r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
        }
        *e = r;
    }
    t
}

/// Parses and checks the page at `p`.
pub fn page_at(data: &[u8], p: usize, crc: &[u32; 256]) -> Option<Page> {
    let h = data.get(p..p + 27)?;
    if &h[..4] != b"OggS" || h[4] != 0 {
        return None;
    }
    let nsegs = h[26] as usize;
    let segs = p + 27;
    let table = data.get(segs..segs + nsegs)?;
    let len: usize = table.iter().map(|&s| s as usize).sum();
    let body = segs + nsegs;
    let page = data.get(p..body + len)?;
    let want = u32::from_le_bytes([h[22], h[23], h[24], h[25]]);
    let mut c = 0u32;
    for (i, &b) in page.iter().enumerate() {
        let b = if (22..26).contains(&i) { 0 } else { b };
        c = (c << 8) ^ crc[((c >> 24) as u8 ^ b) as usize];
    }
    if c != want {
        return None;
    }
    Some(Page {
        at: p,
        body,
        len,
        continued: h[5] & 1 != 0,
        first: h[5] & 2 != 0,
        last: h[5] & 4 != 0,
        granule: i64::from_le_bytes(h[6..14].try_into().unwrap()),
        serial: u32::from_le_bytes([h[14], h[15], h[16], h[17]]),
        segs,
        nsegs,
    })
}

/// Packets of one logical stream.
pub struct Packets {
    pub pages: Vec<Page>,
    serial: u32,
    /// Next page and segment to read.
    page: usize,
    seg: usize,
}

/// A packet and, when it is the last one completed on its page, that
/// page's granule position.
pub struct Packet {
    pub data: Vec<u8>,
    pub granule: Option<i64>,
    /// Index of the page the packet ends on.
    pub page: usize,
}

impl Packets {
    /// All valid pages of the stream with this serial number.
    pub fn new(data: &[u8], serial: u32) -> Packets {
        let crc = crc_table();
        let mut pages = Vec::new();
        let mut p = 0;
        while p + 27 <= data.len() {
            match page_at(data, p, &crc) {
                Some(pg) => {
                    if pg.serial == serial {
                        pages.push(pg);
                    }
                    p = pg.body + pg.len;
                }
                None => {
                    // Resynchronise on the next capture pattern.
                    p = match data[p + 1..].windows(4).position(|w| w == b"OggS") {
                        Some(o) => p + 1 + o,
                        None => break,
                    };
                }
            }
        }
        Packets { pages, serial, page: 0, seg: 0 }
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// Continues at the start of page `page`, skipping the tail of a packet
    /// that began on an earlier page.
    pub fn seek_page(&mut self, data: &[u8], page: usize) {
        self.page = page;
        self.seg = 0;
        if let Some(pg) = self.pages.get(page) {
            if pg.continued {
                // Skip segments up to and including the first one < 255.
                while self.seg < pg.nsegs {
                    let s = data[pg.segs + self.seg];
                    self.seg += 1;
                    if s < 255 {
                        break;
                    }
                }
                if self.seg >= pg.nsegs && data[pg.segs + pg.nsegs - 1] == 255 {
                    // The whole page continues a packet: start at the next one.
                    self.seek_page(data, page + 1);
                }
            }
        }
    }

    pub fn next(&mut self, data: &[u8]) -> Option<Packet> {
        let mut out = Vec::new();
        loop {
            let pg = *self.pages.get(self.page)?;
            let offset: usize = (0..self.seg).map(|i| data[pg.segs + i] as usize).sum();
            let mut pos = pg.body + offset;
            while self.seg < pg.nsegs {
                let s = data[pg.segs + self.seg] as usize;
                out.extend_from_slice(&data[pos..pos + s]);
                pos += s;
                self.seg += 1;
                if s < 255 {
                    // Packet complete. The page's granule position belongs to
                    // the last packet completed on it: no other one follows
                    // when only 255-byte segments (a continued packet) remain.
                    let last = (self.seg..pg.nsegs).all(|i| data[pg.segs + i] == 255);
                    let page = self.page;
                    if self.seg == pg.nsegs {
                        self.page += 1;
                        self.seg = 0;
                    }
                    let granule = (last && pg.granule >= 0).then_some(pg.granule);
                    return Some(Packet { data: out, granule, page });
                }
            }
            self.page += 1;
            self.seg = 0;
        }
    }
}

/// The first stream in `data` with a codec OSyx decodes.
pub fn open(data: Vec<u8>) -> Result<Box<dyn Decoder>, String> {
    let crc = crc_table();
    let mut p = 0;
    let mut other = None;
    while let Some(pg) = page_at(&data, p, &crc) {
        if !pg.first {
            break;
        }
        let body = &data[pg.body..pg.body + pg.len];
        if body.len() >= 7 && body[0] == 1 && &body[1..7] == b"vorbis" {
            return Ok(Box::new(VorbisDecoder::new(data, pg.serial)?));
        }
        if body.starts_with(b"OpusHead") {
            other = Some("Opus");
        } else if body.starts_with(b"\x7FFLAC") {
            other = Some("FLAC in Ogg");
        } else if body.starts_with(b"\x80theora") {
            other = Some("Theora video");
        } else if body.starts_with(b"Speex") {
            other = Some("Speex");
        }
        p = pg.body + pg.len;
    }
    Err(match other {
        Some(c) => alloc::format!("this Ogg file holds {c}, which OSyx cannot play yet"),
        None => String::from("no Vorbis stream in this Ogg file"),
    })
}
