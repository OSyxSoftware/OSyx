//! Hardware-independent memory management algorithms of the OSyx kernel.
//!
//! Nothing in this crate touches page tables, control registers or physical
//! memory directly, so it is unit tested on the host. The kernel supplies the
//! storage (through the HHDM) and the locking.

#![no_std]

pub mod frames;
pub mod adaptive;
pub mod heap;
pub mod lz;

pub const PAGE_SIZE: u64 = 4096;

pub const fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}

pub const fn align_down(x: u64, a: u64) -> u64 {
    x & !(a - 1)
}
