//! `net`: network interfaces and their configuration.
//!
//! ```text
//! net list                         interfaces, one line each
//! net info [iface]                 addresses, DHCP lease, counters, routes, DNS, ARP
//! net configure IFACE dhcp         (root) start DHCP
//! net configure IFACE A.B.C.D/P [gw G] [dns D1[,D2]]   (root) static address
//! net configure IFACE down         (root) remove the address
//! ```

#![no_std]
#![no_main]

use os_std::net::{self, Ipv4Addr, NetConfig, NetIface};
use os_std::prelude::*;

os_std::entry!(main);

fn main() -> i32 {
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r = match a.as_slice() {
        [] | ["list"] => list(),
        ["info"] => info(None),
        ["info", name] => info(Some(name)),
        ["configure", rest @ ..] => configure(rest),
        _ => {
            eprintln!("usage: net list | net info [iface] | net configure IFACE dhcp|down|A.B.C.D/P [gw G] [dns D1,D2]");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("net: {e}");
            1
        }
    }
}

fn mac(m: &[u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

fn method(i: &NetIface) -> &'static str {
    match i.method {
        net::CONFIG_STATIC => "static",
        net::CONFIG_DHCP => "dhcp",
        _ => "none",
    }
}

fn flags(i: &NetIface) -> String {
    let mut f = Vec::new();
    if i.flags & net::IFF_UP != 0 {
        f.push("UP");
    }
    if i.flags & net::IFF_LINK != 0 {
        f.push("LINK");
    } else {
        f.push("NO-CARRIER");
    }
    if i.flags & net::IFF_LOOPBACK != 0 {
        f.push("LOOPBACK");
    }
    if i.flags & net::IFF_INTERRUPTS != 0 {
        f.push("MSI-X");
    }
    f.join(",")
}

fn addr(i: &NetIface) -> String {
    if i.addr == [0; 4] {
        String::from("-")
    } else {
        format!("{}/{}", Ipv4Addr(i.addr), i.prefix)
    }
}

fn list() -> os_std::error::Result<()> {
    println!("{:<6} {:<18} {:<19} {:<7} {:>5}  FLAGS", "IFACE", "ADDRESS", "MAC", "METHOD", "MTU");
    for i in net::interfaces()? {
        println!("{:<6} {:<18} {:<19} {:<7} {:>5}  {}", net::field(&i.name), addr(&i), mac(&i.mac), method(&i), i.mtu, flags(&i));
    }
    Ok(())
}

fn info(only: Option<&str>) -> os_std::error::Result<()> {
    let ifaces = net::interfaces()?;
    let routes = net::routes()?;
    for i in &ifaces {
        let name = net::field(&i.name);
        if only.is_some_and(|o| o != name) {
            continue;
        }
        println!("{name}: <{}> mtu {}", flags(i), i.mtu);
        if i.flags & net::IFF_LOOPBACK == 0 {
            println!("    ether {}", mac(&i.mac));
        }
        println!("    inet {}  config {}", addr(i), method(i));
        if i.method == net::CONFIG_DHCP {
            let phase = net::DHCP_PHASE_NAMES.get(i.dhcp_phase as usize).copied().unwrap_or("?");
            if i.lease_left > 0 {
                println!("    dhcp {phase}, lease expires in {} s", i.lease_left);
            } else {
                println!("    dhcp {phase}");
            }
        }
        if i.gateway != [0; 4] {
            println!("    gateway {}", Ipv4Addr(i.gateway));
        }
        println!("    RX packets {}  bytes {}  errors {}  dropped {}", i.rx_packets, i.rx_bytes, i.rx_errors, i.rx_dropped);
        println!("    TX packets {}  bytes {}  dropped {}", i.tx_packets, i.tx_bytes, i.tx_dropped);
        if i.interrupts > 0 {
            println!("    interrupts {}", i.interrupts);
        }
    }
    if only.is_none() {
        println!("routes:");
        for r in &routes {
            let ifname = ifaces.get(r.iface as usize).map(|i| net::field(&i.name)).unwrap_or("?");
            let via = if r.gateway == [0; 4] { String::from("direct") } else { format!("via {}", Ipv4Addr(r.gateway)) };
            let origin = ["connected", "static", "dhcp"].get(r.origin as usize).copied().unwrap_or("?");
            println!("    {}/{} {via} dev {ifname} ({origin})", Ipv4Addr(r.dest), r.prefix);
        }
        let dns = net::dns_servers()?;
        let dns: Vec<String> = dns.iter().map(|d| format!("{d}")).collect();
        println!("dns: {}", if dns.is_empty() { String::from("none") } else { dns.join(", ") });
        let arp = net::arp_cache()?;
        if !arp.is_empty() {
            println!("neighbours:");
            for e in arp {
                let ifname = ifaces.get(e.iface as usize).map(|i| net::field(&i.name)).unwrap_or("?");
                if e.resolved != 0 {
                    println!("    {} at {} on {ifname} ({} s left)", Ipv4Addr(e.addr), mac(&e.mac), e.expires_ms / 1000);
                } else {
                    println!("    {} on {ifname}: resolving", Ipv4Addr(e.addr));
                }
            }
        }
    }
    Ok(())
}

fn parse_list(s: &str) -> Option<Vec<Ipv4Addr>> {
    s.split(',').map(Ipv4Addr::parse).collect()
}

fn configure(a: &[&str]) -> os_std::error::Result<()> {
    use os_std::error::{Error, ErrorKind};
    let bad = |m| Error::new(ErrorKind::InvalidInput, m);
    let (name, rest) = a.split_first().ok_or(bad("missing interface"))?;
    let ifaces = net::interfaces()?;
    let iface = ifaces.iter().find(|i| net::field(&i.name) == *name).ok_or(bad("no such interface"))?;
    let mut c = NetConfig { iface: iface.index as u32, ..Default::default() };
    match rest {
        ["dhcp"] => c.method = net::CONFIG_DHCP as u32,
        ["down"] => c.method = net::CONFIG_DOWN as u32,
        [cidr, opts @ ..] => {
            let (ip, p) = cidr.split_once('/').unwrap_or((cidr, "24"));
            c.method = net::CONFIG_STATIC as u32;
            c.addr = Ipv4Addr::parse(ip).ok_or(bad("bad address"))?.0;
            c.prefix = p.parse().ok().filter(|&p: &u8| p <= 32).ok_or(bad("bad prefix"))?;
            let mut o = opts.iter();
            while let Some(k) = o.next() {
                let v = o.next().ok_or(bad("option without value"))?;
                match *k {
                    "gw" => c.gateway = Ipv4Addr::parse(v).ok_or(bad("bad gateway"))?.0,
                    "dns" => {
                        let l = parse_list(v).ok_or(bad("bad DNS list"))?;
                        for (slot, a) in c.dns.iter_mut().zip(&l) {
                            *slot = a.0;
                        }
                        c.dns_count = l.len().min(2) as u8;
                    }
                    _ => return Err(bad("unknown option (gw, dns)")),
                }
            }
        }
        [] => return Err(bad("missing dhcp, down or an address")),
    }
    net::configure(&c)?;
    println!("{name}: configuration applied");
    Ok(())
}
