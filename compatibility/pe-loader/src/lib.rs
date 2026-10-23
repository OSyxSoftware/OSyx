//! OSyx PE32+ loader core: API set schema, image layout and relocation,
//! import binding with forwarders, and module initialisation order.
//!
//! `no_std` + `alloc` and free of system calls: the Windows runtime
//! (`userspace/win32`) supplies a [`link::Host`] that maps memory and opens
//! files; the host tests drive the same code against in-memory images.

#![no_std]

extern crate alloc;

pub mod apiset;
pub mod image;
pub mod link;

pub use link::{BuiltinModule, Host, LinkError, Linker, Module, Source};
