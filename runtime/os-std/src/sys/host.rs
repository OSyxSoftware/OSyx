//! Host back end, used only to unit-test OSyx Std on the development machine.
//! It is implemented with the host's Rust std; nothing here runs on OSyx.

use crate::error::{Error, ErrorKind, Result};
use alloc::string::String;
use alloc::vec::Vec;
use osyx_abi::SystemInfo;
use std::io::Write as _;

pub const NAME: &str = "host";

pub fn write(fd: u64, buf: &[u8]) -> Result<usize> {
    let r = match fd {
        osyx_abi::STDOUT => std::io::stdout().write(buf),
        osyx_abi::STDERR => std::io::stderr().write(buf),
        _ => return Err(Error::from(osyx_abi::Errno::BadHandle)),
    };
    r.map_err(|_| Error::new(ErrorKind::Other, "host write failed"))
}

pub fn exit(code: i32) -> ! {
    std::process::exit(code)
}

pub fn monotonic_ns() -> Result<u64> {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    Ok(start.elapsed().as_nanos() as u64)
}

pub fn sysinfo() -> Result<SystemInfo> {
    Err(Error::unsupported("sysinfo is only available on OSyx"))
}

pub fn args() -> Vec<String> {
    std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect()
}

pub fn spawn(_name: &str, _args: &[u8]) -> Result<u64> {
    Err(Error::unsupported("spawning OSyx boot modules is only possible on OSyx"))
}

pub fn wait(_pid: u64) -> Result<(u64, i32)> {
    Err(Error::unsupported("waiting for OSyx processes is only possible on OSyx"))
}

pub fn sleep_ns(ns: u64) {
    std::thread::sleep(std::time::Duration::from_nanos(ns))
}

pub fn yield_now() {
    std::thread::yield_now()
}
