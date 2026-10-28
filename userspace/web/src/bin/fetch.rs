//! `fetch <url> [<output file>] [--sha256 <hex>]`: downloads over HTTP or
//! HTTPS (verified TLS). Without an output file the body goes to standard
//! output.
#![no_std]
#![no_main]

use os_std::io::Write;
use os_std::prelude::*;
use os_std::{env, eprintln, println};

os_std::entry!(main);

struct Stdout;

impl osyx_web::Sink for Stdout {
    fn start(&mut self, _url: &str, _length: Option<u64>) -> Result<(), String> {
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
        os_std::io::stdout().write_all(bytes).map_err(|e| format!("{e}"))
    }
}

fn main() -> i32 {
    let args: Vec<String> = env::args().collect();
    let sha = args.iter().position(|a| a == "--sha256").and_then(|i| args.get(i + 1)).cloned();
    let plain: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--") && Some(*a) != sha.as_ref()).collect();
    let Some(url) = plain.first() else {
        eprintln!("usage: fetch <url> [<output file>] [--sha256 <hex>]");
        return 2;
    };
    let result = match plain.get(1) {
        Some(out) => osyx_web::download(url, out, sha.as_deref(), &mut |done, total| match total {
            Some(t) => eprintln!("fetch: {} / {} KiB", done >> 10, t >> 10),
            None => eprintln!("fetch: {} KiB", done >> 10),
        }),
        None => osyx_web::get(url, &mut Stdout),
    };
    match result {
        Ok(n) => {
            eprintln!("fetch: {n} bytes from {url}");
            println!("FETCH_RESULT ok {n}");
            0
        }
        Err(e) => {
            eprintln!("fetch: {e}");
            println!("FETCH_RESULT failed {e}");
            1
        }
    }
}
