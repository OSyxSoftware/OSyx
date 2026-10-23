//! `nc`: small TCP/UDP client and server for testing the stack.
//!
//! ```text
//! nc HOST PORT [TEXT]          TCP: connect, send TEXT (or nothing), print the reply until EOF
//! nc -u HOST PORT TEXT         UDP: send TEXT, print one reply (2 s timeout)
//! nc -l PORT [COUNT]           TCP echo server for COUNT connections (default 1)
//! nc -lu PORT [COUNT]          UDP echo server for COUNT datagrams
//! nc -b HOST PORT BYTES        TCP: send BYTES of pattern data, expect them echoed, report throughput
//! ```

#![no_std]
#![no_main]

use os_std::net::{TcpListener, TcpStream, UdpSocket};
use os_std::prelude::*;
use os_std::time::{Duration, Instant};

os_std::entry!(main);

fn main() -> i32 {
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r = match a.as_slice() {
        ["-l", port, rest @ ..] => tcp_server(port, rest.first().and_then(|c| c.parse().ok()).unwrap_or(1)),
        ["-lu", port, rest @ ..] => udp_server(port, rest.first().and_then(|c| c.parse().ok()).unwrap_or(1)),
        ["-u", host, port, text @ ..] => udp_client(host, port, &text.join(" ")),
        ["-b", host, port, bytes] => bulk(host, port, bytes.parse().unwrap_or(0)),
        [host, port, text @ ..] => tcp_client(host, port, &text.join(" ")),
        _ => {
            eprintln!("usage: nc HOST PORT [TEXT] | nc -u HOST PORT TEXT | nc -l PORT [N] | nc -lu PORT [N] | nc -b HOST PORT BYTES");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nc: {e}");
            1
        }
    }
}

type R = os_std::error::Result<()>;

fn tcp_client(host: &str, port: &str, text: &str) -> R {
    let addr = format!("{host}:{port}");
    let t = Instant::now();
    let mut s = TcpStream::connect(addr.as_str())?;
    let us = t.elapsed().as_micros() as u64;
    println!("connected to {} from {} in {}.{:03} ms", s.peer_addr()?, s.local_addr()?, us / 1000, us % 1000);
    if !text.is_empty() {
        s.write_all(text.as_bytes())?;
        s.write_all(b"\n")?;
    }
    s.shutdown_write()?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut got = Vec::new();
    s.read_to_end(&mut got)?;
    print!("{}", String::from_utf8_lossy(&got));
    println!("[{} bytes received, connection closed by peer]", got.len());
    Ok(())
}

fn udp_client(host: &str, port: &str, text: &str) -> R {
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    let to = format!("{host}:{port}");
    s.send_to(text.as_bytes(), to.as_str())?;
    let mut buf = [0u8; 2048];
    let (n, from) = s.recv_from(&mut buf)?;
    println!("{} bytes from {from}: {}", n, String::from_utf8_lossy(&buf[..n]));
    Ok(())
}

fn tcp_server(port: &str, count: u32) -> R {
    let l = TcpListener::bind(format!("0.0.0.0:{port}").as_str())?;
    println!("listening on {}", l.local_addr()?);
    for _ in 0..count {
        let (mut c, peer) = l.accept()?;
        println!("connection from {peer}");
        let mut buf = [0u8; 4096];
        let mut total = 0;
        loop {
            let n = c.read(&mut buf)?;
            if n == 0 {
                break;
            }
            c.write_all(&buf[..n])?;
            total += n;
        }
        println!("{peer}: echoed {total} bytes");
    }
    Ok(())
}

fn udp_server(port: &str, count: u32) -> R {
    let s = UdpSocket::bind(format!("0.0.0.0:{port}").as_str())?;
    println!("udp listening on {}", s.local_addr()?);
    let mut buf = [0u8; 2048];
    for _ in 0..count {
        let (n, from) = s.recv_from(&mut buf)?;
        println!("{n} bytes from {from}");
        s.send_to(&buf[..n], &from)?;
    }
    Ok(())
}

fn bulk(host: &str, port: &str, bytes: usize) -> R {
    let mut s = TcpStream::connect(format!("{host}:{port}").as_str())?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let data: Vec<u8> = (0..bytes).map(|i| (i * 31 % 251) as u8).collect();
    let t = Instant::now();
    let mut sent = 0;
    let mut got = Vec::with_capacity(bytes);
    let mut buf = vec![0u8; 16384];
    // Alternate writes and reads so neither side's buffers fill up.
    while got.len() < bytes {
        if sent < bytes {
            let end = (sent + 16384).min(bytes);
            s.write_all(&data[sent..end])?;
            sent = end;
            if sent == bytes {
                s.shutdown_write()?;
            }
        }
        while got.len() < sent {
            let n = s.read(&mut buf)?;
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
    }
    let ms = t.elapsed().as_millis().max(1) as u64;
    let ok = got == data;
    println!(
        "bulk: {} bytes sent and {} echoed in {} ms ({} KiB/s), data {}",
        sent,
        got.len(),
        ms,
        (bytes as u64 * 2 * 1000 / ms) / 1024,
        if ok { "intact" } else { "CORRUPTED" }
    );
    if ok {
        Ok(())
    } else {
        Err(os_std::error::Error::new(os_std::error::ErrorKind::InvalidData, "echoed data differs"))
    }
}
