//! The OSyx TCP/IP stack.
//!
//! Protocol modules are pure functions over byte slices plus small state
//! machines (`arp::Cache`, `tcp::Tcb`, `dhcp::Client`, `dns::Cache`), and
//! [`stack::Stack`] ties them into interfaces, routes and sockets. Nothing
//! here touches hardware or time directly: the kernel feeds received
//! frames and the current time in, and transmits the frames the stack
//! hands out, so all of it runs (and is tested) on the host too.
//!
//! Every buffer and table is bounded; malformed input is counted and
//! dropped, never trusted.

#![no_std]

extern crate alloc;

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod stack;
pub mod tcp;
pub mod udp;
pub mod wire;

pub use stack::{NetError, SocketId, Stack};
pub use wire::{Endpoint, Ipv4Addr, MacAddr};
