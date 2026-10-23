//! `ping [-c count] [-s size] [-i interval_ms] [-W timeout_ms] host`: ICMP
//! echo through the kernel's ping sockets. Every line reflects a real
//! reply (with its measured round-trip time) or a real error.

#![no_std]
#![no_main]

use os_std::net::{lookup_host, PingSocket};
use os_std::prelude::*;
use os_std::thread;
use os_std::time::{Duration, Instant};

os_std::entry!(main);

fn main() -> i32 {
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let (mut count, mut size, mut interval, mut wait) = (4u32, 56usize, 1000u64, 2000u64);
    let mut host = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |what: &str| -> Option<u64> {
            let v = it.next().and_then(|v| v.parse().ok());
            if v.is_none() {
                eprintln!("ping: {what} needs a number");
            }
            v
        };
        match a.as_str() {
            "-c" => match num("-c") {
                Some(v) => count = v as u32,
                None => return 2,
            },
            "-s" => match num("-s") {
                Some(v) if v <= 65000 => size = v as usize,
                _ => return 2,
            },
            "-i" => match num("-i") {
                Some(v) => interval = v,
                None => return 2,
            },
            "-W" => match num("-W") {
                Some(v) => wait = v,
                None => return 2,
            },
            h if !h.starts_with('-') => host = Some(h.to_string()),
            other => {
                eprintln!("ping: unknown option {other}");
                return 2;
            }
        }
    }
    let Some(host) = host else {
        eprintln!("usage: ping [-c count] [-s size] [-i interval_ms] [-W timeout_ms] host");
        return 2;
    };
    let target = match lookup_host(&host) {
        Ok(a) if !a.is_empty() => a[0],
        Ok(_) => {
            eprintln!("ping: {host}: no address");
            return 2;
        }
        Err(e) => {
            eprintln!("ping: {host}: {e}");
            return 2;
        }
    };
    let sock = match PingSocket::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ping: cannot open an ICMP socket: {e}");
            return 2;
        }
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(wait)));
    println!("PING {host} ({target}): {size} data bytes");
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let (mut sent, mut received, mut errors) = (0u32, 0u32, 0u32);
    let mut rtts: Vec<u64> = Vec::new();
    let started = Instant::now();
    let mut buf = vec![0u8; size + 64];
    for seq in 1..=count {
        let t0 = Instant::now();
        if let Err(e) = sock.send(target, seq as u16, &payload) {
            println!("From {target} icmp_seq={seq}: send failed: {e}");
            errors += 1;
            sent += 1;
            thread::sleep(Duration::from_millis(interval));
            continue;
        }
        sent += 1;
        // Wait for the reply to this sequence number (older ones may still arrive).
        loop {
            match sock.recv(&mut buf) {
                Ok(r) if r.seq == seq as u16 => {
                    let us = t0.elapsed().as_micros() as u64;
                    let ok = buf[..r.len] == payload[..];
                    received += 1;
                    rtts.push(us);
                    println!(
                        "{} bytes from {}: icmp_seq={} ttl={} time={}.{:03} ms{}",
                        r.len + 8,
                        r.from,
                        r.seq,
                        r.ttl,
                        us / 1000,
                        us % 1000,
                        if ok { "" } else { " (payload differs!)" }
                    );
                    break;
                }
                Ok(r) => println!("{} bytes from {}: icmp_seq={} (late)", r.len + 8, r.from, r.seq),
                Err(e) if e.kind() == os_std::error::ErrorKind::TimedOut => {
                    println!("Request timeout for icmp_seq {seq}");
                    break;
                }
                Err(e) => {
                    println!("From {target} icmp_seq={seq}: {e}");
                    errors += 1;
                    break;
                }
            }
        }
        if seq < count {
            let spent = t0.elapsed().as_millis() as u64;
            if spent < interval {
                thread::sleep(Duration::from_millis(interval - spent));
            }
        }
    }
    let total = started.elapsed().as_millis();
    println!("--- {host} ping statistics ---");
    let loss = ((sent - received) * 100).checked_div(sent).unwrap_or(0);
    println!(
        "{sent} packets transmitted, {received} received, {}{loss}% packet loss, time {total} ms",
        if errors > 0 { format!("{errors} errors, ") } else { String::new() }
    );
    if !rtts.is_empty() {
        let min = *rtts.iter().min().unwrap_or(&0);
        let max = *rtts.iter().max().unwrap_or(&0);
        let avg = rtts.iter().sum::<u64>() / rtts.len() as u64;
        let fmt = |v: u64| format!("{}.{:03}", v / 1000, v % 1000);
        println!("rtt min/avg/max = {}/{}/{} ms", fmt(min), fmt(avg), fmt(max));
    }
    if received == 0 {
        1
    } else {
        0
    }
}
