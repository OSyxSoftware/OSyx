//! `dns lookup NAME...` resolves names through the kernel resolver;
//! `dns cache` lists cached answers, `dns servers` the servers in use.

#![no_std]
#![no_main]

use os_std::net::{self, Ipv4Addr};
use os_std::prelude::*;
use os_std::time::Instant;

os_std::entry!(main);

fn main() -> i32 {
    let args: Vec<String> = os_std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match a.as_slice() {
        ["lookup", names @ ..] if !names.is_empty() => {
            let mut status = 0;
            for name in names {
                let t = Instant::now();
                match net::lookup_host(name) {
                    Ok(addrs) => {
                        let us = t.elapsed().as_micros() as u64;
                        let list: Vec<String> = addrs.iter().map(|a| format!("{a}")).collect();
                        println!("{name}: {} ({}.{:03} ms)", list.join(", "), us / 1000, us % 1000);
                    }
                    Err(e) => {
                        println!("{name}: {e}");
                        status = 1;
                    }
                }
            }
            status
        }
        ["cache"] => match net::dns_cache() {
            Ok(entries) => {
                for e in entries {
                    let what = if e.count == 0 { String::from("does not exist") } else { format!("{} ({} address(es))", Ipv4Addr(e.addr), e.count) };
                    println!("{:<40} {what}, {} s left", net::field(&e.name), e.ttl_left);
                }
                0
            }
            Err(e) => {
                eprintln!("dns: {e}");
                1
            }
        },
        ["servers"] => match net::dns_servers() {
            Ok(s) => {
                for a in s {
                    println!("{a}");
                }
                0
            }
            Err(e) => {
                eprintln!("dns: {e}");
                1
            }
        },
        _ => {
            eprintln!("usage: dns lookup NAME... | dns cache | dns servers");
            2
        }
    }
}
