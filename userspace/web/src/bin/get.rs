//! `get <url> [<file>] [--sha256 <hex>]`: downloads a file over HTTP or
//! HTTPS (verified TLS), like wget. Without a file name it is saved in the
//! current directory under the last part of the URL's path (`index.html`
//! for a bare host); an existing file is kept and the new one gets a
//! numbered name (`name.1`, `name.2`, ...).
#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::{env, eprintln, fs, println};

os_std::entry!(main);

/// The file name a URL suggests: its last path part, without the query or
/// fragment.
fn name_from_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let name = match rest.split_once('/') {
        Some((_, path)) => path.rsplit('/').next().unwrap_or(""),
        None => "",
    };
    if name.is_empty() || name == "." || name == ".." {
        String::from("index.html")
    } else {
        String::from(name)
    }
}

/// `name`, or the first `name.N` that does not exist yet.
fn free_name(name: &str) -> String {
    if fs::metadata(name).is_err() {
        return String::from(name);
    }
    (1..).map(|i| format!("{name}.{i}")).find(|n| fs::metadata(n).is_err()).unwrap_or_default()
}

fn main() -> i32 {
    let args: Vec<String> = env::args().collect();
    let sha = args.iter().position(|a| a == "--sha256").and_then(|i| args.get(i + 1)).cloned();
    let plain: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--") && Some(*a) != sha.as_ref()).collect();
    let Some(url) = plain.first() else {
        eprintln!("usage: get <url> [<file>] [--sha256 <hex>]");
        eprintln!("Downloads <url> into <file>, or into the current directory under the URL's file name.");
        return 2;
    };
    let out = match plain.get(1) {
        Some(f) => String::from(f.as_str()),
        None => free_name(&name_from_url(url)),
    };
    println!("get: {url} -> {out}");
    let mut shown = u64::MAX;
    let result = osyx_web::download(url, &out, sha.as_deref(), &mut |done, total| {
        // One line per MiB (and the first one).
        if shown == u64::MAX || done >> 20 != shown >> 20 {
            shown = done;
            match total {
                Some(t) if t > 0 => println!("get: {} / {} MiB ({}%)", done >> 20, t >> 20, done * 100 / t),
                _ => println!("get: {} MiB", done >> 20),
            }
        }
    });
    match result {
        Ok(n) => {
            println!("get: saved {out} ({n} bytes){}", if sha.is_some() { ", SHA-256 verified" } else { "" });
            0
        }
        Err(e) => {
            eprintln!("get: {url}: {e}");
            1
        }
    }
}
