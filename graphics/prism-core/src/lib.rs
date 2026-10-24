//! PRISM compositor core.
//!
//! Everything PRISM decides is implemented here, independent of the
//! kernel: client and resource bookkeeping, protocol validation, window
//! stacking, focus, decorations, pointer grabs, keyboard routing, damage
//! tracking and software composition into a back buffer. The `prism`
//! program (userspace) adds the platform: the IPC channels, shared memory
//! mappings, the input device and the framebuffer. Because the core is
//! plain Rust over byte slices and pixel buffers, its behaviour is unit
//! tested on the development host.

#![no_std]

extern crate alloc;

pub mod compositor;
pub mod damage;
pub mod draw;
pub mod theme;
pub mod taskbar;

#[cfg(test)]
mod tests;

pub use compositor::{ClientId, Compositor, Config, PixelSource, WindowId};
