//! The SILK layer of Opus (placeholder while it is being ported).

use super::range::RangeDecoder;

pub struct DecControl {
    pub channels_api: usize,
    pub channels_internal: usize,
    pub internal_rate: u32,
    pub payload_size_ms: usize,
}

impl DecControl {
    pub fn new(channels: usize) -> DecControl {
        DecControl { channels_api: channels, channels_internal: channels, internal_rate: 16000, payload_size_ms: 20 }
    }
}

pub struct Silk;

impl Silk {
    pub fn new() -> Silk {
        Silk
    }

    pub fn reset(&mut self) {}

    pub fn decode(
        &mut self,
        _ctl: &mut DecControl,
        _lost: bool,
        _first: bool,
        _ec: Option<&mut RangeDecoder>,
        _out: &mut [i16],
    ) -> Result<usize, &'static str> {
        Err("SILK is not ported yet")
    }
}
