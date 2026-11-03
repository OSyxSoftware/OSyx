//! Network overload and recovery check run by `init netstress,<tcp>,<udp>`
//! (driven by `scripts/net-stress.ps1`) against the helper of
//! `scripts/net-bench-host.py`.
//!
//! It checks that the network works, then overloads it: hundreds of
//! connections closed on one side while the peer never closes (each closed
//! end stays in FIN-WAIT-2), connections whose buffers fill in both
//! directions and are
//! closed with data unread, a UDP flood out and in, and blocked readers.
//! After the load stops the network must work again by itself, and again
//! after a reconnect (`net::configure` with DHCP, what Settings does).
//!
//! Every step prints `netstress: <step> ok|FAILED <detail>`; the last line
//! is `netstress: done, <n> failed`.

use os_std::io::{Read, Write};
use os_std::net::{self, Ipv4Addr, NetConfig, PingSocket, SocketAddr, TcpListener, TcpStream, UdpSocket};
use os_std::prelude::*;
use os_std::thread;
use os_std::time::{Duration, Instant};

const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// Both ends of each count: more than the 1024-entry socket table holds,
/// so closed ends must give up their slots (four times the old table).
const ORPHANS: usize = 600;

fn command(s: &mut TcpStream, cmd: u8, len: u64) -> os_std::error::Result<()> {
    let mut h = [0u8; 9];
    h[0] = cmd;
    h[1..].copy_from_slice(&len.to_le_bytes());
    s.write_all(&h)
}

/// One round of what a program does on the network: a UDP socket, a DNS
/// query that goes to the server, a TCP echo and a ping. Err names what
/// failed.
fn health(tcp: SocketAddr, udp: SocketAddr, round: usize) -> Result<(), String> {
    // Every part is tried, so a failure names all that is broken.
    let parts = [health_udp(udp, round), health_dns(round), health_tcp(tcp, round), health_ping(round)];
    let errors: Vec<String> = parts.into_iter().filter_map(Result::err).collect();
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

fn health_udp(udp: SocketAddr, round: usize) -> Result<(), String> {
    let u = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("UDP socket: {e}"))?;
    let _ = u.set_read_timeout(Some(Duration::from_secs(2)));
    let mut msg = [0u8; 32];
    msg[0] = b'E';
    msg[1..9].copy_from_slice(&(round as u64).to_le_bytes());
    u.send_to(&msg, &udp).map_err(|e| format!("UDP send: {e}"))?;
    let mut buf = [0u8; 64];
    loop {
        match u.recv_from(&mut buf) {
            Ok((32, _)) if buf[..32] == msg => break,
            Ok(_) => continue,
            Err(e) => return Err(format!("UDP echo: {e}")),
        }
    }
    Ok(())
}

fn health_dns(round: usize) -> Result<(), String> {
    // A name never asked before: the answer cannot come from the cache.
    match net::lookup_host(&format!("osyx-stress-{round}.invalid")) {
        Err(e) if e.kind() == os_std::error::ErrorKind::HostNotFound => {}
        r => return Err(format!("DNS: {:?}", r.map(|_| ()))),
    }
    Ok(())
}

fn health_tcp(tcp: SocketAddr, round: usize) -> Result<(), String> {
    let mut s = TcpStream::connect_timeout(&tcp, Some(Duration::from_secs(3))).map_err(|e| format!("TCP connect: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    command(&mut s, b'E', 0).map_err(|e| format!("TCP send: {e}"))?;
    let data: Vec<u8> = (0..16384).map(|i| (i * 7 + round) as u8).collect();
    s.write_all(&data).map_err(|e| format!("TCP send: {e}"))?;
    let mut got = vec![0u8; data.len()];
    let mut n = 0;
    while n < got.len() {
        match s.read(&mut got[n..]) {
            Ok(0) => return Err(String::from("TCP echo: closed early")),
            Ok(k) => n += k,
            Err(e) => return Err(format!("TCP echo: {e}")),
        }
    }
    if got != data {
        return Err(String::from("TCP echo: wrong data"));
    }
    Ok(())
}

fn health_ping(round: usize) -> Result<(), String> {
    let mut buf = [0u8; 64];
    let p = PingSocket::new().map_err(|e| format!("ping socket: {e}"))?;
    let _ = p.set_read_timeout(Some(Duration::from_secs(2)));
    p.send(HOST, round as u16, b"stress").map_err(|e| format!("ping: {e}"))?;
    p.recv(&mut buf).map_err(|e| format!("ping reply: {e}"))?;
    Ok(())
}

/// Retries `health` for up to `secs`; the time it took to pass.
fn wait_healthy(tcp: SocketAddr, udp: SocketAddr, secs: u64, round: &mut usize) -> Result<Duration, String> {
    let t0 = Instant::now();
    let mut last = String::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        *round += 1;
        match health(tcp, udp, *round) {
            Ok(()) => return Ok(t0.elapsed()),
            Err(e) => last = e,
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err(last)
}

fn eth0() -> Option<net::NetIface> {
    net::interfaces().ok()?.into_iter().find(|i| net::field(&i.name) == "eth0")
}

fn socket_states() -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for s in net::sockets().unwrap_or_default() {
        let st = String::from(net::field(&s.state));
        match counts.iter_mut().find(|(k, _)| *k == st) {
            Some((_, n)) => *n += 1,
            None => counts.push((st, 1)),
        }
    }
    counts.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
}

pub fn run(tcp_port: u16, udp_port: u16) -> i32 {
    let tcp = SocketAddr::new(HOST, tcp_port);
    let udp = SocketAddr::new(HOST, udp_port);
    let mut failed = 0;
    let mut report = |step: &str, r: Result<String, String>| match r {
        Ok(d) => println!("netstress: {step} ok {d}"),
        Err(d) => {
            println!("netstress: {step} FAILED {d}");
            failed += 1;
        }
    };
    let mut round = 0;
    let r = wait_healthy(tcp, udp, 60, &mut round).map(|d| format!("after {} ms", d.as_millis()));
    report("network works before the load", r);

    // 1. Connections this side closes while the peer keeps its end open
    //    and never sends a FIN (each closed end waits in FIN-WAIT-2). The
    //    peer is a listener in this program that holds every connection
    //    to the end: hundreds of host connections would overload QEMU's
    //    user network on Windows instead.
    let listener = TcpListener::bind("127.0.0.1:0").ok();
    let peer = listener.as_ref().and_then(|l| l.local_addr().ok());
    let mut held = Vec::new();
    let mut opened = 0;
    let mut refused = String::new();
    if let (Some(l), Some(peer)) = (&listener, peer) {
        for _ in 0..ORPHANS {
            match TcpStream::connect_timeout(&peer, Some(Duration::from_secs(3))) {
                Ok(s) => {
                    if let Ok((c, _)) = l.accept() {
                        held.push(c);
                    }
                    drop(s);
                    opened += 1;
                }
                Err(e) => {
                    refused = format!(", then {e}");
                    break;
                }
            }
        }
    }
    println!("netstress: (info) {opened} connections closed while their peer holds them{refused}; sockets: {}", socket_states());

    // 2. Full buffers both ways, closed with data unread (a reset), while
    //    readers sit blocked on idle sockets; 3. UDP floods out and in.
    let blockers: Vec<_> = (0..8)
        .map(|_| {
            thread::spawn(move || {
                let Ok(u) = UdpSocket::bind("0.0.0.0:0") else { return };
                let _ = u.set_read_timeout(Some(Duration::from_secs(8)));
                let mut b = [0u8; 16];
                let _ = u.recv_from(&mut b);
            })
        })
        .collect();
    let fillers: Vec<_> = (0..8)
        .map(|i| {
            thread::spawn(move || {
                let Ok(mut s) = TcpStream::connect_timeout(&tcp, Some(Duration::from_secs(3))) else { return 0 };
                let _ = s.set_write_timeout(Some(Duration::from_secs(4)));
                if command(&mut s, b'E', 0).is_err() {
                    return 0;
                }
                let chunk = vec![i as u8; 64 * 1024];
                let mut sent = 0;
                while sent < 16 << 20 {
                    match s.write(&chunk) {
                        Ok(n) => sent += n,
                        Err(_) => break,
                    }
                }
                sent
            })
        })
        .collect();
    let sink = UdpSocket::bind("0.0.0.0:0").ok();
    if let Some(sink) = &sink {
        let mut req = [0u8; 7];
        req[0] = b'D';
        req[1..5].copy_from_slice(&100_000u32.to_le_bytes());
        req[5..7].copy_from_slice(&1400u16.to_le_bytes());
        let _ = sink.send_to(&req, &udp);
    }
    if let Ok(out) = UdpSocket::bind("0.0.0.0:0") {
        let mut msg = vec![b'S'; 1400];
        msg[0] = b'S';
        for _ in 0..50_000 {
            let _ = out.send_to(&msg, &udp);
        }
    }
    let filled: usize = fillers.into_iter().map(|t| t.join().unwrap_or(0)).sum();
    for b in blockers {
        let _ = b.join();
    }
    println!("netstress: (info) load done: {} KiB pushed into full buffers; sockets: {}", filled / 1024, socket_states());
    drop(sink);

    // 4. The load is over: the network has to come back by itself.
    let r = wait_healthy(tcp, udp, 30, &mut round).map(|d| format!("after {} ms", d.as_millis()));
    report("network works again after the load, without a reconnect", r);
    println!("netstress: (info) sockets: {}", socket_states());

    // 5. Reconnect the way Settings does (DHCP again), twice: once while
    //    the interface is up, once after taking it down.
    for (step, down_first) in [("reconnect (DHCP again) brings the network back", false), ("down, then reconnect, brings the network back", true)] {
        let r = (|| {
            let i = eth0().ok_or(String::from("no eth0"))?;
            let c = NetConfig { iface: i.index as u32, method: net::CONFIG_DHCP as u32, ..Default::default() };
            if down_first {
                let down = NetConfig { iface: i.index as u32, method: net::CONFIG_DOWN as u32, ..Default::default() };
                net::configure(&down).map_err(|e| format!("down: {e}"))?;
                thread::sleep(Duration::from_millis(500));
                if health(tcp, udp, 9999).is_ok() {
                    return Err(String::from("still reachable while down"));
                }
            }
            net::configure(&c).map_err(|e| format!("configure: {e}"))?;
            let t0 = Instant::now();
            while eth0().is_none_or(|i| i.addr == [0; 4]) {
                if t0.elapsed() > Duration::from_secs(20) {
                    return Err(String::from("no DHCP lease"));
                }
                thread::sleep(Duration::from_millis(100));
            }
            let lease = t0.elapsed();
            wait_healthy(tcp, udp, 30, &mut round)?;
            Ok(format!("lease after {} ms, working after {} ms", lease.as_millis(), t0.elapsed().as_millis()))
        })();
        report(step, r);
    }
    drop(held);
    let st = net::stats().map(|s| s.counters).unwrap_or_default();
    println!("netstress: (info) stats: tcp resets {} retransmits {}, sockets {}", st[16], st[17], st[27]);
    println!("netstress: done, {failed} failed");
    failed as i32
}
