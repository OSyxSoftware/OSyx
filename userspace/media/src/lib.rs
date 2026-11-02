//! Media decoding for the viewers built into Files: sound files are decoded
//! to floating-point samples and resampled to the sound card's format, pictures
//! are decoded to RGBA frames, and PDF pages are rendered to pixels.
//! Everything here works on bytes in memory and needs no screen, so the
//! decoders are host-tested against reference decodings made by libsndfile
//! (`tests/make_fixtures.py`).

#![no_std]

extern crate alloc;

pub mod audio;
mod bits;
pub mod image;
pub mod jbig2;
pub mod jpx;
mod mq;
pub mod pdf;
pub mod vp8;
