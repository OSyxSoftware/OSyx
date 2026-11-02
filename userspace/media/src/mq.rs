//! The MQ arithmetic decoder of JBIG2 (ITU-T T.88 Annex E) and JPEG 2000
//! (ISO/IEC 15444-1 Annex C).

/// (Qe, next index after an MPS, after an LPS, switch the MPS).
const QE: [(u32, u8, u8, bool); 47] = [
    (0x5601, 1, 1, true),
    (0x3401, 2, 6, false),
    (0x1801, 3, 9, false),
    (0x0AC1, 4, 12, false),
    (0x0521, 5, 29, false),
    (0x0221, 38, 33, false),
    (0x5601, 7, 6, true),
    (0x5401, 8, 14, false),
    (0x4801, 9, 14, false),
    (0x3801, 10, 14, false),
    (0x3001, 11, 17, false),
    (0x2401, 12, 18, false),
    (0x1C01, 13, 20, false),
    (0x1601, 29, 21, false),
    (0x5601, 15, 14, true),
    (0x5401, 16, 14, false),
    (0x5101, 17, 15, false),
    (0x4801, 18, 16, false),
    (0x3801, 19, 17, false),
    (0x3401, 20, 18, false),
    (0x3001, 21, 19, false),
    (0x2801, 22, 19, false),
    (0x2401, 23, 20, false),
    (0x2201, 24, 21, false),
    (0x1C01, 25, 22, false),
    (0x1801, 26, 23, false),
    (0x1601, 27, 24, false),
    (0x1401, 28, 25, false),
    (0x1201, 29, 26, false),
    (0x1101, 30, 27, false),
    (0x0AC1, 31, 28, false),
    (0x09C1, 32, 29, false),
    (0x08A1, 33, 30, false),
    (0x0521, 34, 31, false),
    (0x0441, 35, 32, false),
    (0x02A1, 36, 33, false),
    (0x0221, 37, 34, false),
    (0x0141, 38, 35, false),
    (0x0111, 39, 36, false),
    (0x0085, 40, 37, false),
    (0x0049, 41, 38, false),
    (0x0025, 42, 39, false),
    (0x0015, 43, 40, false),
    (0x0009, 44, 41, false),
    (0x0005, 45, 42, false),
    (0x0001, 45, 43, false),
    (0x5601, 46, 46, false),
];

/// A context: the state index shifted left once, with the MPS in bit 0.
pub type Cx = u8;

/// The context state JPEG 2000 starts some contexts in (index 3 or 4, MPS 0).
pub const fn cx(index: u8) -> Cx {
    index << 1
}

pub struct Mq<'a> {
    data: &'a [u8],
    bp: usize,
    c: u32,
    a: u32,
    ct: u32,
    /// Bytes read past the end of the data.
    past: u32,
}

impl<'a> Mq<'a> {
    /// INITDEC.
    pub fn new(data: &'a [u8]) -> Mq<'a> {
        let mut d = Mq { data, bp: 0, c: 0, a: 0x8000, ct: 0, past: 0 };
        d.c = (d.byte(0) as u32) << 16;
        d.byte_in();
        d.c <<= 7;
        d.ct -= 7;
        d
    }

    /// Bytes past the end read as 0xFF.
    fn byte(&self, i: usize) -> u8 {
        *self.data.get(i).unwrap_or(&0xFF)
    }

    /// Whether decoding has run well past the end of the data: what
    /// follows is made up.
    pub fn overrun(&self) -> bool {
        self.past > 64
    }

    fn byte_in(&mut self) {
        if self.bp + 1 >= self.data.len() {
            self.past = self.past.saturating_add(1);
        }
        if self.byte(self.bp) == 0xFF {
            if self.byte(self.bp + 1) > 0x8F {
                self.c += 0xFF00;
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c += (self.byte(self.bp) as u32) << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c += (self.byte(self.bp) as u32) << 8;
            self.ct = 8;
        }
    }

    /// DECODE: one bit in context `cx`.
    #[inline]
    pub fn bit(&mut self, cx: &mut Cx) -> u32 {
        let (qe, nmps, nlps, switch) = QE[(*cx >> 1) as usize];
        let mps = (*cx & 1) as u32;
        self.a -= qe;
        let d;
        if (self.c >> 16) < qe {
            // LPS_EXCHANGE: the LPS sub-interval lies at the bottom.
            if self.a < qe {
                d = mps;
                *cx = (nmps << 1) | mps as u8;
            } else {
                d = 1 - mps;
                let m = if switch { 1 - mps } else { mps };
                *cx = (nlps << 1) | m as u8;
            }
            self.a = qe;
        } else {
            self.c -= qe << 16;
            if self.a & 0x8000 != 0 {
                return mps;
            }
            // MPS_EXCHANGE
            if self.a < qe {
                d = 1 - mps;
                let m = if switch { 1 - mps } else { mps };
                *cx = (nlps << 1) | m as u8;
            } else {
                d = mps;
                *cx = (nmps << 1) | mps as u8;
            }
        }
        // RENORMD
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t88_annex_h2_sequence() {
        // H.2 Test sequence for arithmetic coder: one context, I = 0, MPS = 0.
        let input = [
            0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB, 0x86, 0xF4,
            0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
        ];
        let output = [
            0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x82,
            0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46, 0xA3, 0xBF,
        ];
        let mut d = Mq::new(&input);
        let mut cx = 0;
        for &want in &output {
            let mut got = 0u8;
            for _ in 0..8 {
                got = (got << 1) | d.bit(&mut cx) as u8;
            }
            assert_eq!(got, want);
        }
    }
}
