//! `timesync [--daemon] [server]` sets the clock from a network time server
//! (SNTP, RFC 4330; `pool.ntp.org` by default). init starts
//! `timesync --daemon` as root: it waits for a network, sets the clock and
//! does it again every 6 hours (every minute while no server answers).
//! Without `--daemon` it asks once; setting the clock needs root.

#![no_std]
#![no_main]

use os_std::net::{self, UdpSocket};
use os_std::prelude::*;
use os_std::time::{Duration, SystemTime, UNIX_EPOCH};
use os_std::{eprintln, os, println, thread};

os_std::entry!(main);

const DEFAULT_SERVER: &str = "pool.ntp.org";
const NTP_PORT: u16 = 123;
/// Seconds from 1900-01-01 (the NTP epoch) to 1970-01-01.
const NTP_UNIX: u64 = 2_208_988_800;
const NS: u64 = 1_000_000_000;
const RESYNC_SECS: u64 = 6 * 3600;
const RETRY_SECS: u64 = 60;

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

/// NTP 32.32 fixed-point timestamp of a Unix time.
fn to_ntp(unix_ns: u64) -> u64 {
    let secs = unix_ns / NS + NTP_UNIX;
    let frac = ((unix_ns % NS) << 32) / NS;
    (secs << 32) | frac
}

/// Unix time of an NTP timestamp. Era 1 starts in 2036: seconds that
/// would fall before 2020 belong to it.
fn from_ntp(t: u64) -> u64 {
    let mut secs = t >> 32;
    if secs < NTP_UNIX + 1_577_836_800 {
        secs += 1 << 32;
    }
    (secs - NTP_UNIX) * NS + (((t & 0xFFFF_FFFF) * NS) >> 32)
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Asks `server` for the time; returns the Unix time (ns) it is now and the
/// round-trip delay (ns).
fn query(server: &str) -> Result<(u64, u64), String> {
    let addr = *net::lookup_host(server).map_err(|e| format!("{server}: {e}"))?.first().ok_or_else(|| format!("{server}: no address"))?;
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("socket: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| format!("socket: {e}"))?;
    let mut req = [0u8; 48];
    req[0] = 0x23; // leap indicator 0, version 4, mode 3 (client)
    let t0 = now_ns();
    let sent = to_ntp(t0);
    req[40..48].copy_from_slice(&sent.to_be_bytes());
    sock.send_to(&req, &(addr, NTP_PORT)).map_err(|e| format!("{addr}: {e}"))?;
    let mut buf = [0u8; 128];
    let (n, _) = sock.recv_from(&mut buf).map_err(|e| format!("{addr}: no answer ({e})"))?;
    let t3 = now_ns();
    let r = &buf[..n];
    if n < 48 || r[0] & 7 != 4 || r[0] >> 6 == 3 || !(1..=15).contains(&r[1]) {
        return Err(format!("{addr}: not a usable server answer"));
    }
    if be64(&r[24..32]) != sent {
        return Err(format!("{addr}: the answer is for another request"));
    }
    let (t1, t2) = (from_ntp(be64(&r[32..40])), from_ntp(be64(&r[40..48])));
    // RFC 4330: offset = ((t1 - t0) + (t2 - t3)) / 2, delay = (t3 - t0) - (t2 - t1).
    let offset = ((t1 as i128 - t0 as i128) + (t2 as i128 - t3 as i128)) / 2;
    let delay = (t3 as i128 - t0 as i128) - (t2 as i128 - t1 as i128);
    Ok(((t3 as i128 + offset) as u64, delay.max(0) as u64))
}

/// One synchronisation; returns a log line.
fn sync(server: &str) -> Result<String, String> {
    let before = now_ns();
    let (now, delay) = query(server)?;
    os::set_time(now).map_err(|e| format!("cannot set the clock: {e}"))?;
    let off_ms = (now as i128 - before as i128) / 1_000_000;
    Ok(format!("clock set from {server} (it was off by {off_ms} ms, round trip {} ms)", delay / 1_000_000))
}

fn main() -> i32 {
    let _ = os_std::system::thread_priority(0, os_std::system::priority::LOW);
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let daemon = args.iter().any(|a| a == "--daemon");
    let server = args.iter().find(|a| !a.starts_with("--")).map_or(DEFAULT_SERVER, |s| s.as_str());
    if !daemon {
        return match sync(server) {
            Ok(line) => {
                println!("timesync: {line}");
                0
            }
            Err(e) => {
                eprintln!("timesync: {e}");
                1
            }
        };
    }
    let mut failing = false;
    loop {
        let wait = match sync(server) {
            Ok(line) => {
                println!("timesync: {line}");
                failing = false;
                RESYNC_SECS
            }
            Err(e) => {
                // No network yet is the usual case right after boot.
                if !failing {
                    println!("timesync: {e}; retrying every {RETRY_SECS} s");
                }
                failing = true;
                RETRY_SECS
            }
        };
        thread::sleep(Duration::from_secs(wait));
    }
}
