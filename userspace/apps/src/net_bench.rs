//! Network benchmark run by `init netbench,<tcp port>,<udp port>[,<MiB>[,<only>]]`
//! (driven by `scripts/net-bench.ps1`); `<only>` runs just the benchmarks
//! whose name starts with it (`tcp`, `udp_rr`, `loopback`, ...), e.g. under
//! the kernel profiler. It measures the stack's throughput
//! and latency against the helper of `scripts/net-bench-host.py` on the
//! host (reached as 10.0.2.2 through QEMU's user network) and, without any
//! device in the way, over loopback between two threads.
//!
//! Every result is one line `netbench: <name> <value> <unit>`; the script
//! collects them and compares runs.
//!
//! Host protocol: a TCP connection starts with one command byte and a
//! little-endian u64 length. `U` uploads that many bytes (the host answers
//! with the u64 count it received), `D` downloads that many bytes, `E`
//! echoes until the guest closes. UDP datagrams start with a command byte:
//! `E` is echoed, `B` resets the sink counter, `S` is counted, `Z` is
//! answered with `R` and the u64 count since `B`, and `D` followed by a u32
//! count and a u16 size makes the host send that many datagrams back.

use os_std::io::{Read, Write};
use os_std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use os_std::prelude::*;
use os_std::thread;
use os_std::time::{Duration, Instant};

const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const CHUNK: usize = 64 * 1024;
const RR_ROUNDS: usize = 2000;
const RR_SIZE: usize = 64;
const UDP_DATAGRAMS: u32 = 20_000;
const UDP_SIZE: usize = 1400;

fn report(name: &str, value: f64, unit: &str) {
    println!("netbench: {} {:.2} {}", name, value, unit);
}

fn mib_per_s(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / d.as_secs_f64().max(1e-9)
}

fn command(s: &mut TcpStream, cmd: u8, len: u64) -> os_std::error::Result<()> {
    let mut h = [0u8; 9];
    h[0] = cmd;
    h[1..].copy_from_slice(&len.to_le_bytes());
    s.write_all(&h)
}

fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> bool {
    let mut got = 0;
    while got < buf.len() {
        match s.read(&mut buf[got..]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => got += n,
        }
    }
    true
}

/// Average, median and 99th percentile of round-trip samples in µs.
fn report_rtt(name: &str, mut samples: Vec<u64>) {
    if samples.is_empty() {
        println!("netbench: {} failed", name);
        return;
    }
    samples.sort_unstable();
    let avg = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
    report(&format!("{name}_avg"), avg, "us");
    report(&format!("{name}_p50"), samples[samples.len() / 2] as f64, "us");
    report(&format!("{name}_p99"), samples[samples.len() * 99 / 100] as f64, "us");
}

fn tcp_upload(addr: SocketAddr, bytes: usize) -> Option<Duration> {
    let mut s = TcpStream::connect(&addr).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
    command(&mut s, b'U', bytes as u64).ok()?;
    let data = vec![0x5Au8; CHUNK];
    let t0 = Instant::now();
    let mut sent = 0;
    while sent < bytes {
        let n = CHUNK.min(bytes - sent);
        s.write_all(&data[..n]).ok()?;
        sent += n;
    }
    let mut count = [0u8; 8];
    // The host answers once it has everything: the time covers delivery.
    let ok = read_exact(&mut s, &mut count) && u64::from_le_bytes(count) == bytes as u64;
    ok.then(|| t0.elapsed())
}

fn tcp_download(addr: SocketAddr, bytes: usize) -> Option<Duration> {
    let mut s = TcpStream::connect(&addr).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
    command(&mut s, b'D', bytes as u64).ok()?;
    let mut buf = vec![0u8; CHUNK];
    let t0 = Instant::now();
    let mut got = 0;
    while got < bytes {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => got += n,
        }
    }
    Some(t0.elapsed())
}

/// Request/response round trips of `RR_SIZE` bytes over one connection.
fn tcp_rr(mut s: TcpStream, rounds: usize) -> Vec<u64> {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let msg = [0x42u8; RR_SIZE];
    let mut buf = [0u8; RR_SIZE];
    let mut samples = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let t0 = Instant::now();
        if s.write_all(&msg).is_err() || !read_exact(&mut s, &mut buf) {
            return Vec::new();
        }
        samples.push(t0.elapsed().as_micros() as u64);
    }
    samples
}

fn udp_rr(s: &UdpSocket, rounds: usize) -> Vec<u64> {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let mut msg = [0u8; RR_SIZE];
    msg[0] = b'E';
    let mut buf = [0u8; 256];
    let mut samples = Vec::with_capacity(rounds);
    for i in 0..rounds {
        msg[1..5].copy_from_slice(&(i as u32).to_le_bytes());
        let t0 = Instant::now();
        if s.send(&msg).is_err() {
            continue;
        }
        // A lost datagram costs one timeout; a stale echo is skipped.
        loop {
            match s.recv(&mut buf) {
                Ok(n) if n == RR_SIZE && buf[1..5] == msg[1..5] => {
                    samples.push(t0.elapsed().as_micros() as u64);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
    samples
}

/// Sends datagrams as fast as the stack takes them; returns (time, count
/// the host received).
fn udp_upload(s: &UdpSocket, count: u32) -> Option<(Duration, u64)> {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 64];
    s.send(b"B").ok()?;
    let mut msg = vec![0u8; UDP_SIZE];
    msg[0] = b'S';
    let t0 = Instant::now();
    for _ in 0..count {
        let _ = s.send(&msg);
    }
    let elapsed = t0.elapsed();
    // Let the last datagrams drain, then ask for the count.
    thread::sleep(Duration::from_millis(200));
    for _ in 0..3 {
        s.send(b"Z").ok()?;
        if let Ok(n) = s.recv(&mut buf) {
            if n == 9 && buf[0] == b'R' {
                let got = u64::from_le_bytes(buf[1..9].try_into().ok()?);
                return Some((elapsed, got));
            }
        }
    }
    None
}

/// The host sends `count` datagrams; returns (time to the last one received,
/// count received).
fn udp_download(s: &UdpSocket, count: u32) -> Option<(Duration, u64)> {
    let mut req = [0u8; 7];
    req[0] = b'D';
    req[1..5].copy_from_slice(&count.to_le_bytes());
    req[5..7].copy_from_slice(&(UDP_SIZE as u16).to_le_bytes());
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = vec![0u8; 2048];
    s.send(&req).ok()?;
    let t0 = Instant::now();
    let mut got = 0u64;
    let mut last = t0;
    while got < count as u64 {
        match s.recv(&mut buf) {
            Ok(n) if n == UDP_SIZE => {
                got += 1;
                last = Instant::now();
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    (got > 0).then(|| (last.duration_since(t0), got))
}

fn loopback_tcp(bytes: usize) -> Option<Duration> {
    let l = TcpListener::bind("127.0.0.1:0").ok()?;
    let addr = l.local_addr().ok()?;
    let sink = thread::spawn(move || {
        let Ok((mut c, _)) = l.accept() else { return 0 };
        let mut buf = vec![0u8; CHUNK];
        let mut got = 0;
        loop {
            match c.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        let _ = c.write_all(&(got as u64).to_le_bytes());
        got
    });
    let mut s = TcpStream::connect(&addr).ok()?;
    let data = vec![0xA5u8; CHUNK];
    let t0 = Instant::now();
    let mut sent = 0;
    while sent < bytes {
        let n = CHUNK.min(bytes - sent);
        s.write_all(&data[..n]).ok()?;
        sent += n;
    }
    s.shutdown_write().ok()?;
    let mut count = [0u8; 8];
    let ok = read_exact(&mut s, &mut count);
    let elapsed = t0.elapsed();
    let got = sink.join().unwrap_or(0);
    (ok && got == bytes).then_some(elapsed)
}

fn loopback_tcp_rr(rounds: usize) -> Vec<u64> {
    let Ok(l) = TcpListener::bind("127.0.0.1:0") else { return Vec::new() };
    let Ok(addr) = l.local_addr() else { return Vec::new() };
    let echo = thread::spawn(move || {
        let Ok((mut c, _)) = l.accept() else { return };
        let mut buf = [0u8; RR_SIZE];
        while read_exact(&mut c, &mut buf) {
            if c.write_all(&buf).is_err() {
                break;
            }
        }
    });
    let samples = match TcpStream::connect(&addr) {
        Ok(s) => tcp_rr(s, rounds),
        Err(_) => Vec::new(),
    };
    let _ = echo.join();
    samples
}

fn loopback_udp_rr(rounds: usize) -> Vec<u64> {
    let (Ok(a), Ok(b)) = (UdpSocket::bind("127.0.0.1:0"), UdpSocket::bind("127.0.0.1:0")) else { return Vec::new() };
    let (Ok(aa), Ok(ba)) = (a.local_addr(), b.local_addr()) else { return Vec::new() };
    if a.connect(&ba).is_err() || b.connect(&aa).is_err() {
        return Vec::new();
    }
    let _ = b.set_read_timeout(Some(Duration::from_secs(2)));
    let echo = thread::spawn(move || {
        let mut buf = [0u8; 256];
        for _ in 0..rounds {
            match b.recv(&mut buf) {
                Ok(n) => {
                    // `E` keeps the reply shape `udp_rr` expects.
                    let _ = b.send(&buf[..n]);
                }
                Err(_) => break,
            }
        }
    });
    let samples = udp_rr(&a, rounds);
    let _ = echo.join();
    samples
}

pub fn run(tcp_port: u16, udp_port: u16, mib: usize, only: &str) -> i32 {
    let bytes = mib << 20;
    let want = |name: &str| name.starts_with(only);
    let tcp = SocketAddr::new(HOST, tcp_port);
    let udp = SocketAddr::new(HOST, udp_port);
    let mut failed = 0;
    let mut fail = |name: &str| {
        println!("netbench: {} failed", name);
        failed += 1;
    };
    // Wait for DHCP: the first connect needs an address and a route.
    let start = Instant::now();
    while TcpStream::connect_timeout(&tcp, Some(Duration::from_secs(2))).is_err() {
        if start.elapsed() > Duration::from_secs(60) {
            println!("netbench: the host helper is unreachable");
            return 1;
        }
        thread::sleep(Duration::from_millis(200));
    }
    // One warm-up transfer each way: ARP, slow start and the host's first
    // allocations stay out of the numbers.
    let _ = tcp_upload(tcp, 1 << 20);
    let _ = tcp_download(tcp, 1 << 20);

    if want("tcp_upload") {
        match tcp_upload(tcp, bytes) {
            Some(d) => report("tcp_upload", mib_per_s(bytes, d), "MiB/s"),
            None => fail("tcp_upload"),
        }
    }
    if want("tcp_download") {
        match tcp_download(tcp, bytes) {
            Some(d) => report("tcp_download", mib_per_s(bytes, d), "MiB/s"),
            None => fail("tcp_download"),
        }
    }
    if want("tcp_rr") {
        match TcpStream::connect(&tcp).and_then(|mut s| command(&mut s, b'E', 0).map(|_| s)) {
            Ok(s) => report_rtt("tcp_rr", tcp_rr(s, RR_ROUNDS)),
            Err(_) => fail("tcp_rr"),
        }
    }
    match UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect(&udp).map(|_| s)) {
        Ok(s) => {
            if want("udp_rr") {
                report_rtt("udp_rr", udp_rr(&s, RR_ROUNDS));
            }
            if want("udp_upload") {
                match udp_upload(&s, UDP_DATAGRAMS) {
                    Some((d, got)) => {
                        report("udp_upload_offered", mib_per_s(UDP_DATAGRAMS as usize * UDP_SIZE, d), "MiB/s");
                        report("udp_upload_delivered", got as f64 * 100.0 / UDP_DATAGRAMS as f64, "%");
                    }
                    None => fail("udp_upload"),
                }
            }
            if want("udp_download") {
                match udp_download(&s, UDP_DATAGRAMS) {
                    Some((d, got)) => {
                        report("udp_download", mib_per_s(got as usize * UDP_SIZE, d), "MiB/s");
                        report("udp_download_received", got as f64 * 100.0 / UDP_DATAGRAMS as f64, "%");
                    }
                    None => fail("udp_download"),
                }
            }
        }
        Err(_) => fail("udp"),
    }
    if want("loopback_tcp") {
        match loopback_tcp(bytes * 4) {
            Some(d) => report("loopback_tcp", mib_per_s(bytes * 4, d), "MiB/s"),
            None => fail("loopback_tcp"),
        }
    }
    if want("loopback_tcp_rr") {
        report_rtt("loopback_tcp_rr", loopback_tcp_rr(RR_ROUNDS));
    }
    if want("loopback_udp_rr") {
        report_rtt("loopback_udp_rr", loopback_udp_rr(RR_ROUNDS));
    }
    println!("netbench: done, {} failed", failed);
    failed
}
