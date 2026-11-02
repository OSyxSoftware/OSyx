//! Bit readers over byte slices.

/// Reads bits most significant first (FLAC, MPEG audio).
#[derive(Clone)]
pub struct MsbReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> MsbReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        MsbReader { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], bit: usize) -> Self {
        MsbReader { data, pos: bit }
    }

    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub fn seek_bits(&mut self, bit: usize) {
        self.pos = bit;
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.bits_left() < n {
            return None;
        }
        self.pos += n;
        Some(())
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    pub fn byte_pos(&self) -> usize {
        self.pos / 8
    }

    /// Up to 32 bits.
    pub fn read(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Some(0);
        }
        if self.bits_left() < n as usize {
            return None;
        }
        let v = self.peek_unchecked(n);
        self.pos += n as usize;
        Some(v)
    }

    /// Like `read`, but bits past the end read as zero (MPEG Huffman data
    /// may be decoded right up to its end).
    pub fn read_lenient(&mut self, n: u32) -> u32 {
        let v = self.peek_unchecked(n);
        self.pos += n as usize;
        v
    }

    /// The next `n` (1..=32) bits without consuming them, zero past the end.
    pub fn peek_unchecked(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let mut w: u64 = 0;
        for i in 0..8 {
            w = (w << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        let shift = 64 - (self.pos % 8) as u32 - n;
        ((w >> shift) & ((1u64 << n) - 1)) as u32
    }

    pub fn bit(&mut self) -> Option<bool> {
        self.read(1).map(|b| b != 0)
    }

    pub fn signed(&mut self, n: u32) -> Option<i32> {
        let v = self.read(n)?;
        Some(if n == 0 { 0 } else { ((v << (32 - n)) as i32) >> (32 - n) })
    }

    /// Zero bits before the next one bit (FLAC unary codes).
    pub fn unary(&mut self) -> Option<u32> {
        let mut n = 0;
        loop {
            if self.bits_left() == 0 {
                return None;
            }
            let avail = self.bits_left().min(32) as u32;
            let w = self.peek_unchecked(avail) << (32 - avail);
            if w != 0 {
                let z = w.leading_zeros();
                self.pos += z as usize + 1;
                return Some(n + z);
            }
            n += avail;
            self.pos += avail as usize;
        }
    }
}

/// Reads bits least significant first (Ogg, Vorbis).
#[derive(Clone)]
pub struct LsbReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LsbReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        LsbReader { data, pos: 0 }
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// Up to 32 bits.
    pub fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if self.bits_left() < n as usize {
            self.pos = self.data.len() * 8;
            return None;
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Some(v)
    }

    /// The next `n` (1..=32) bits, zero past the end.
    pub fn peek(&self, n: u32) -> u32 {
        let byte = self.pos / 8;
        let mut w: u64 = 0;
        for i in (0..8).rev() {
            w = (w << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        let v = w >> (self.pos % 8);
        (v & ((1u64 << n) - 1)) as u32
    }

    pub fn consume(&mut self, n: usize) -> Option<()> {
        if self.bits_left() < n {
            self.pos = self.data.len() * 8;
            return None;
        }
        self.pos += n;
        Some(())
    }

    pub fn bit(&mut self) -> Option<bool> {
        self.read(1).map(|b| b != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_first() {
        let d = [0b1010_0000, 0xFF, 0x01];
        let mut r = MsbReader::new(&d);
        assert_eq!(r.read(1), Some(1));
        assert_eq!(r.read(3), Some(0b010));
        assert_eq!(r.read(4), Some(0));
        assert_eq!(r.signed(4), Some(-1));
        assert_eq!(r.read(4), Some(0xF));
        assert_eq!(r.unary(), Some(7));
        assert_eq!(r.read(1), None);
    }

    #[test]
    fn lsb_first() {
        let d = [0b1010_0101, 0x0F];
        let mut r = LsbReader::new(&d);
        assert_eq!(r.read(1), Some(1));
        assert_eq!(r.read(3), Some(0b010));
        assert_eq!(r.read(8), Some(0xFA));
        assert_eq!(r.read(4), Some(0));
        assert_eq!(r.read(1), None);
    }
}
