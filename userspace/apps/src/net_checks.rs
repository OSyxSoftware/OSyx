//! Network checks run by `init net,<tcp port>,<udp port>` against
//! independent endpoints on the host: QEMU's user-mode network (DHCP, DNS
//! and the gateway at 10.0.2.2) and the echo servers of
//! `scripts/net-host.py`, reached as 10.0.2.2:<port>. The host test script
//! also connects to our TCP server through a port forward.

use super::Tests;
use os_std::error::ErrorKind;
use os_std::io::{Read, Write};
use os_std::net::{self, Ipv4Addr, PingSocket, SocketAddr, TcpListener, TcpStream, UdpSocket};
use os_std::prelude::*;
use os_std::process::Command;
use os_std::thread;
use os_std::time::{Duration, Instant};

const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// Port our server listens on (QEMU forwards a host port to it).
pub const GUEST_PORT: u16 = 7;

fn pattern(len: usize, seed: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 131 + seed * 7) % 251) as u8).collect()
}

/// Sends `data` over a fresh connection and reads the echo back.
fn echo_round_trip(addr: SocketAddr, data: &[u8]) -> os_std::error::Result<Vec<u8>> {
    let mut s = TcpStream::connect(&addr)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut got = Vec::with_capacity(data.len());
    let mut buf = vec![0u8; 16384];
    let mut sent = 0;
    while got.len() < data.len() {
        if sent < data.len() {
            let end = (sent + 32768).min(data.len());
            s.write_all(&data[sent..end])?;
            sent = end;
            if sent == data.len() {
                s.shutdown_write()?;
            }
        }
        while got.len() < sent {
            let n = s.read(&mut buf)?;
            if n == 0 {
                return Ok(got);
            }
            got.extend_from_slice(&buf[..n]);
        }
    }
    Ok(got)
}

fn run_tool(args: &[&str]) -> bool {
    let mut c = Command::new(args[0]);
    c.args(&args[1..]);
    matches!(c.spawn().and_then(|ch| ch.wait()), Ok(st) if st.code() == Some(0))
}

pub fn run(t: &mut Tests, tcp_port: u16, udp_port: u16) {
    t.check("net: DHCP lease on eth0", || {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Ok(ifs) = net::interfaces() {
                if let Some(i) = ifs.iter().find(|i| net::field(&i.name) == "eth0" && i.addr != [0; 4]) {
                    println!(
                        "net: eth0 {}/{} via DHCP after {} ms, gateway {}",
                        Ipv4Addr(i.addr),
                        i.prefix,
                        start.elapsed().as_millis(),
                        Ipv4Addr(i.gateway)
                    );
                    return i.method == net::CONFIG_DHCP && i.gateway != [0; 4];
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    });
    t.check("net: DNS server from DHCP", || net::dns_servers().is_ok_and(|s| !s.is_empty()));
    t.check("net: ping the gateway (ICMP echo, 3 replies)", || {
        let Ok(p) = PingSocket::new() else { return false };
        let _ = p.set_read_timeout(Some(Duration::from_secs(2)));
        let mut ok = 0;
        let mut buf = [0u8; 128];
        for seq in 1..=3u16 {
            let t0 = Instant::now();
            if p.send(GATEWAY, seq, b"OSyx ping payload").is_err() {
                continue;
            }
            if let Ok(r) = p.recv(&mut buf) {
                let us = t0.elapsed().as_micros();
                if r.seq == seq && r.from == GATEWAY && &buf[..r.len] == b"OSyx ping payload" {
                    println!("net: echo reply from {} seq {} ttl {} in {} us", r.from, r.seq, r.ttl, us);
                    ok += 1;
                }
            }
        }
        ok == 3
    });
    t.check("net: /bin/ping -c 3 10.0.2.2 exits 0", || run_tool(&["/bin/ping", "-c", "3", "-i", "200", "10.0.2.2"]));
    t.check("net: ping a host that does not answer ARP reports an error", || {
        let Ok(p) = PingSocket::new() else { return false };
        let _ = p.set_read_timeout(Some(Duration::from_secs(6)));
        let _ = p.send(Ipv4Addr::new(10, 0, 2, 77), 1, b"x");
        let mut buf = [0u8; 16];
        matches!(p.recv(&mut buf), Err(e) if e.kind() == ErrorKind::HostUnreachable)
    });
    t.check("net: resolve example.com through DNS", || match net::lookup_host("example.com") {
        Ok(a) => {
            println!("net: example.com -> {:?}", a);
            !a.is_empty()
        }
        Err(e) => {
            println!("net: example.com lookup failed: {e}");
            false
        }
    });
    t.check("net: second lookup is answered from the cache", || {
        let before = net::stats().map(|s| s.counters[22]).unwrap_or(0);
        let r = net::lookup_host("example.com").is_ok();
        let after = net::stats().map(|s| s.counters[22]).unwrap_or(u64::MAX);
        r && before == after
    });
    t.check("net: a name that does not exist is HostNotFound", || {
        matches!(net::lookup_host("osyx-does-not-exist.invalid"), Err(e) if e.kind() == ErrorKind::HostNotFound)
    });
    t.check("net: /bin/dns lookup example.com exits 0", || run_tool(&["/bin/dns", "lookup", "example.com"]));
    t.check("net: UDP echo with the host (5 datagrams)", || {
        let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return false };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let to = SocketAddr::new(GATEWAY, udp_port);
        let mut ok = 0;
        for i in 0..5 {
            let msg = format!("udp datagram {i} from OSyx");
            if s.send_to(msg.as_bytes(), &to).is_err() {
                continue;
            }
            let mut buf = [0u8; 256];
            if let Ok((n, from)) = s.recv_from(&mut buf) {
                if from == to && &buf[..n] == msg.as_bytes() {
                    ok += 1;
                }
            }
        }
        ok == 5
    });
    t.check("net: TCP 1 MiB echo through the host server", || {
        let data = pattern(1 << 20, 1);
        let t0 = Instant::now();
        match echo_round_trip(SocketAddr::new(GATEWAY, tcp_port), &data) {
            Ok(got) => {
                let ms = t0.elapsed().as_millis().max(1);
                println!("net: TCP echo of {} bytes took {} ms ({} KiB/s each way)", data.len(), ms, (data.len() as u128 * 1000 / ms) / 1024);
                got == data
            }
            Err(e) => {
                println!("net: TCP echo failed: {e}");
                false
            }
        }
    });
    t.check("net: 4 concurrent TCP connections keep their data apart", || {
        let workers: Vec<_> = (0..4)
            .map(|i| {
                thread::spawn(move || {
                    let data = pattern(128 * 1024, i + 10);
                    echo_round_trip(SocketAddr::new(GATEWAY, tcp_port), &data).map(|g| g == data).unwrap_or(false)
                })
            })
            .collect();
        workers.into_iter().all(|w| w.join().unwrap_or(false))
    });
    // QEMU's user network on Windows does not answer a SYN to a closed host
    // port with a reset (the connect times out), so refusal is checked
    // against our own stack; the host case is only logged.
    {
        let t0 = Instant::now();
        let r = TcpStream::connect_timeout(&SocketAddr::new(GATEWAY, tcp_port + 50), Some(Duration::from_secs(3)));
        println!("net: (info) connect to a closed host port: {:?} after {} ms", r.map(|_| ()).map_err(|e| e.kind()), t0.elapsed().as_millis());
    }
    t.check("net: TCP connect to a closed local port is refused (RST)", || {
        let own = net::interfaces().ok().and_then(|i| i.into_iter().find(|i| net::field(&i.name) == "eth0")).map(|i| Ipv4Addr(i.addr));
        let Some(own) = own else { return false };
        [Ipv4Addr::LOCALHOST, own].iter().all(|&a| {
            matches!(TcpStream::connect(&SocketAddr::new(a, 9)), Err(e) if e.kind() == ErrorKind::ConnectionRefused)
        })
    });
    t.check("net: loopback TCP between two threads", || {
        let Ok(l) = TcpListener::bind("127.0.0.1:8000") else { return false };
        let server = thread::spawn(move || {
            let Ok((mut c, _)) = l.accept() else { return false };
            let mut buf = [0u8; 64];
            let n = c.read(&mut buf).unwrap_or(0);
            c.write_all(&buf[..n]).is_ok()
        });
        let ok = TcpStream::connect("127.0.0.1:8000").and_then(|mut s| {
            s.write_all(b"over loopback")?;
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf)?;
            Ok(&buf[..n] == b"over loopback")
        });
        server.join().unwrap_or(false) && ok.unwrap_or(false)
    });
    t.check("net: non-blocking read says WouldBlock", || {
        let Ok(s) = TcpStream::connect(&SocketAddr::new(GATEWAY, tcp_port)) else { return false };
        let _ = s.set_nonblocking(true);
        let mut s = s;
        let mut buf = [0u8; 8];
        matches!(s.read(&mut buf), Err(e) if e.kind() == ErrorKind::WouldBlock)
    });
    t.check("net: serve TCP connections from the host (port forward)", || {
        let Ok(l) = TcpListener::bind(&SocketAddr::new(Ipv4Addr::UNSPECIFIED, GUEST_PORT)) else { return false };
        let _ = l.set_accept_timeout(Some(Duration::from_secs(60)));
        println!("net: listening for the host on port {}", GUEST_PORT);
        let mut served = 0;
        for _ in 0..2 {
            let Ok((mut c, peer)) = l.accept() else { break };
            let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
            let mut total = 0;
            let mut buf = [0u8; 4096];
            loop {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        total += n;
                        if c.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            println!("net: served {} from {}", total, peer);
            served += 1;
        }
        served == 2
    });
    let _ = run_tool(&["/bin/netstat", "-a"]);
    let _ = run_tool(&["/bin/net", "info"]);
    t.check("net: every socket is released (TIME-WAIT expires)", || {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if net::sockets().is_ok_and(|s| s.iter().all(|s| s.internal != 0)) {
                println!("net: all sockets released after {} ms", start.elapsed().as_millis());
                return true;
            }
            thread::sleep(Duration::from_millis(500));
        }
        false
    });
    let _ = run_tool(&["/bin/netstat", "-s"]);
}
