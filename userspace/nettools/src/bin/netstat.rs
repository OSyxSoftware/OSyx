//! `netstat [-a] [-s]`: sockets of the system from the kernel's stack
//! (`-a` includes the resolver's own sockets, `-s` prints protocol counters).

#![no_std]
#![no_main]

use os_std::net::{self, Ipv4Addr, SockAddr};
use os_std::prelude::*;

os_std::entry!(main);

fn ep(a: &SockAddr) -> String {
    if a.addr == [0; 4] && a.port == 0 {
        String::from("*:*")
    } else if a.addr == [0; 4] {
        format!("*:{}", a.port)
    } else {
        format!("{}:{}", Ipv4Addr(a.addr), a.port)
    }
}

fn main() -> i32 {
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let all = args.iter().any(|a| a == "-a");
    if args.iter().any(|a| a == "-s") {
        return match net::stats() {
            Ok(s) => {
                for (name, v) in net::NET_STAT_NAMES.iter().zip(s.counters.iter()) {
                    println!("{name:<22} {v}");
                }
                0
            }
            Err(e) => {
                eprintln!("netstat: {e}");
                1
            }
        };
    }
    let socks = match net::sockets() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("netstat: {e}");
            return 1;
        }
    };
    println!("{:<5} {:>7} {:>7}  {:<22} {:<22} STATE", "PROTO", "RECV-Q", "SEND-Q", "LOCAL", "REMOTE");
    let mut shown = 0;
    for s in &socks {
        if s.internal != 0 && !all {
            continue;
        }
        let kind = net::field(&s.kind);
        let local = if kind == "icmp" { format!("ident {}", s.local.port) } else { ep(&s.local) };
        println!(
            "{:<5} {:>7} {:>7}  {:<22} {:<22} {}{}",
            kind,
            s.rx_queue,
            s.tx_queue,
            local,
            ep(&s.remote),
            net::field(&s.state),
            if s.internal != 0 { " (resolver)" } else { "" }
        );
        shown += 1;
    }
    println!("{shown} socket(s)");
    0
}
