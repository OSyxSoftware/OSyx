//! The package manager (`/bin/pkg`): single applications from a
//! repository, signed like system updates.
//!
//! ```text
//! pkg list              what the repository has, and what is installed
//! pkg info NAME         a package's version, size and files
//! pkg install NAME...   install or upgrade (root)
//! pkg upgrade           upgrade every installed package (root)
//! pkg remove NAME...    remove (root)
//! pkg installed         what is installed, without asking the repository
//! ```
//!
//! The repository is `url=` in `/etc/osyx/packages.conf`, else the system
//! update's (`/etc/osyx/update.conf`). Its list and every package manifest
//! must carry an Ed25519 signature of the OSyx release key or of a key in
//! `/etc/osyx/packages.keys`; the payload is checked against the
//! manifest's SHA-256 and every file against its own. A package's files
//! are written next to their targets and renamed into place only when all
//! of them were written. A package can write only `/opt/pkg/<name>/` and
//! its launcher entry (osyx_update::package::allowed_path). What is
//! installed is recorded in `/var/lib/osyx/pkg/<name>.txt`.
//!
//! `elevate` does not show the output of the program it starts, so what
//! root's `pkg` did, and why it refused, is also in
//! `/var/log/osyx-pkg.log`.
#![no_std]
#![no_main]

use os_std::io::{Read, Write};
use os_std::prelude::*;
use os_std::{env, fs, os, println};
use osyx_update::package::{self, Index, Package, State};
use osyx_update::hex;
use sha2::{Digest, Sha256};

os_std::entry!(main);

const CONF: &str = "/etc/osyx/packages.conf";
const UPDATE_CONF: &str = "/etc/osyx/update.conf";
const KEYS: &str = "/etc/osyx/packages.keys";
const DB: &str = "/var/lib/osyx/pkg";
const CACHE: &str = "/var/cache/osyx/pkg";
const LOG: &str = "/var/log/osyx-pkg.log";
const MANIFEST_MAX: usize = 4 << 20;
const STAGED: &str = osyx_update::STAGED_SUFFIX;

fn setting(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| String::from(v.trim())))
        .filter(|v| !v.is_empty())
}

fn repository() -> Result<String, String> {
    for conf in [CONF, UPDATE_CONF] {
        if let Some(url) = fs::read_to_string(conf).ok().and_then(|t| setting(&t, "url")) {
            return Ok(String::from(osyx_update::update_url(&url)));
        }
    }
    Err(format!("no repository: put url=... in {CONF}"))
}

fn keys() -> Vec<[u8; 32]> {
    package::trusted_keys(&fs::read_to_string(KEYS).unwrap_or_default())
}

/// A small download kept in memory.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    struct Buf(Vec<u8>);
    impl osyx_web::Sink for Buf {
        fn start(&mut self, _url: &str, length: Option<u64>) -> Result<(), String> {
            match length {
                Some(n) if n as usize > MANIFEST_MAX => Err(format!("{n} bytes is too large")),
                _ => Ok(()),
            }
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
            if self.0.len() + bytes.len() > MANIFEST_MAX {
                return Err(String::from("too large"));
            }
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }
    let mut b = Buf(Vec::new());
    osyx_web::get(url, &mut b).map_err(|e| format!("{url}: {e}"))?;
    Ok(b.0)
}

/// A signed text file of the repository; refused unless the signature is
/// of a trusted key.
fn signed(url: &str, file: &str, signature: &str) -> Result<String, String> {
    let data = fetch(&format!("{url}/{file}"))?;
    let sig = fetch(&format!("{url}/{signature}"))?;
    if !package::verify_any(&data, &sig, &keys()) {
        return Err(format!("{file} is not signed with a trusted key ({KEYS} lists the added ones): refused"));
    }
    String::from_utf8(data).map_err(|_| format!("{file} is not text"))
}

fn index(url: &str) -> Result<Index, String> {
    Index::parse(&signed(url, package::INDEX, package::INDEX_SIGNATURE)?)
}

fn installed(name: &str) -> Option<Package> {
    Package::parse(&fs::read_to_string(format!("{DB}/{name}.txt")).ok()?).ok()
}

fn all_installed() -> Vec<Package> {
    let mut names: Vec<String> = fs::read_dir(DB)
        .map(|d| d.filter_map(|e| String::from(e.file_name()).strip_suffix(".txt").map(String::from)).collect())
        .unwrap_or_default();
    names.sort();
    names.iter().filter_map(|n| installed(n)).collect()
}

/// Prints a result; root's results are kept in the log too.
fn say(line: &str) {
    println!("{line}");
    if os::getuid().0 == 0 {
        let _ = fs::create_dir_all("/var/log");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(LOG) {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }
}

fn need_root() -> Result<(), String> {
    if os::getuid().0 != 0 {
        return Err(String::from("this needs root: run it with elevate"));
    }
    Ok(())
}

/// Removes `path` and then its directories while they are empty, up to
/// (not including) `/opt/pkg` or the launcher directory.
fn remove_file(path: &str) {
    let _ = fs::remove_file(path);
    let mut dir = path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if !parent.starts_with("/opt/pkg/") || fs::remove_dir(parent).is_err() {
            break;
        }
        dir = parent;
    }
}

fn install(url: &str, name: &str) -> Result<String, String> {
    if !package::valid_name(name) {
        return Err(format!("{name:?} is not a package name"));
    }
    let [manifest, signature, bin] = package::file_names(name);
    let text = signed(url, &manifest, &signature)?;
    let p = Package::parse(&text)?;
    if p.name != name {
        return Err(format!("{manifest} is the manifest of package {}: refused", p.name));
    }
    let old = installed(name);
    if let Some(old) = &old {
        if old.version == p.version {
            return Ok(format!("{name} {} is already installed", p.version));
        }
        if !osyx_update::newer(&p.version, &old.version) {
            return Err(format!("{name} {} is installed and the repository has the older {}: not installed", old.version, p.version));
        }
    }
    fs::create_dir_all(CACHE).map_err(|e| format!("{CACHE}: {e}"))?;
    let payload = format!("{CACHE}/{bin}");
    osyx_web::download(&format!("{url}/{bin}"), &payload, Some(&hex(&p.payload_sha256)), &mut |_, _| {}).map_err(|e| format!("download: {e}"))?;
    // Every file next to its target first, checked against its SHA-256.
    let mut src = fs::File::open(&payload).map_err(|e| format!("{payload}: {e}"))?;
    let mut staged: Vec<String> = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    let result = (|| -> Result<(), String> {
        for e in &p.files {
            let to = format!("/{}", e.path);
            let tmp = format!("{to}{STAGED}");
            if let Some((dir, _)) = to.rsplit_once('/') {
                fs::create_dir_all(dir).map_err(|err| format!("{dir}: {err}"))?;
            }
            let mut out = fs::File::create(&tmp).map_err(|err| format!("{tmp}: {err}"))?;
            staged.push(to);
            let mut h = Sha256::new();
            let mut left = e.size;
            while left > 0 {
                let want = (left as usize).min(buf.len());
                let n = src.read(&mut buf[..want]).map_err(|err| format!("{payload}: {err}"))?;
                if n == 0 {
                    return Err(format!("{payload} ends inside {}", e.path));
                }
                h.update(&buf[..n]);
                out.write_all(&buf[..n]).map_err(|err| format!("{tmp}: {err}"))?;
                left -= n as u64;
            }
            if <[u8; 32]>::from(h.finalize()) != e.sha256 {
                return Err(format!("{} does not match its SHA-256 in the manifest", e.path));
            }
            out.sync_all().map_err(|err| format!("{tmp}: {err}"))?;
            drop(out);
            fs::set_permissions(&tmp, fs::Permissions::from_mode(e.mode)).map_err(|err| format!("{tmp}: {err}"))?;
        }
        Ok(())
    })();
    drop(src);
    let _ = fs::remove_file(&payload);
    if let Err(e) = result {
        for to in &staged {
            remove_file(&format!("{to}{STAGED}"));
        }
        return Err(e);
    }
    for to in &staged {
        fs::rename(format!("{to}{STAGED}"), to).map_err(|e| format!("{to}: {e}"))?;
    }
    if let Some(old) = &old {
        for path in package::stale_files(old, &p) {
            remove_file(&format!("/{path}"));
        }
    }
    fs::create_dir_all(DB).map_err(|e| format!("{DB}: {e}"))?;
    fs::write(format!("{DB}/{name}.txt"), &text).map_err(|e| format!("{DB}/{name}.txt: {e}"))?;
    Ok(match old {
        Some(old) => format!("{name} upgraded from {} to {} ({} files)", old.version, p.version, p.files.len()),
        None => format!("{name} {} installed ({} files)", p.version, p.files.len()),
    })
}

fn remove(name: &str) -> Result<String, String> {
    let p = installed(name).ok_or_else(|| format!("{name} is not installed"))?;
    for f in &p.files {
        remove_file(&format!("/{}", f.path));
    }
    let _ = fs::remove_dir(format!("/{}{name}", package::PREFIX));
    fs::remove_file(format!("{DB}/{name}.txt")).map_err(|e| format!("{DB}/{name}.txt: {e}"))?;
    Ok(format!("{name} {} removed ({} files)", p.version, p.files.len()))
}

fn run(args: &[String]) -> Result<(), String> {
    let names = &args[1.min(args.len())..];
    match args.first().map(String::as_str) {
        Some("list") => {
            let list = index(&repository()?)?;
            if list.packages.is_empty() {
                println!("the repository has no packages");
            }
            for p in &list.packages {
                let have = installed(&p.name).map(|i| i.version);
                let note = match package::state(have.as_deref(), &p.version) {
                    State::NotInstalled => String::new(),
                    State::Current => String::from("  [installed]"),
                    State::Upgradable => format!("  [installed {}, upgrade available]", have.unwrap_or_default()),
                };
                println!("{:<16} {:<10} {}{note}", p.name, p.version, p.description);
            }
            Ok(())
        }
        Some("installed") => {
            let all = all_installed();
            if all.is_empty() {
                println!("no packages are installed");
            }
            for p in all {
                println!("{:<16} {:<10} {}", p.name, p.version, p.description);
            }
            Ok(())
        }
        Some("info") if names.len() == 1 => {
            let url = repository()?;
            let [manifest, signature, _] = package::file_names(&names[0]);
            let p = Package::parse(&signed(&url, &manifest, &signature)?)?;
            println!("{} {}\n{}\n{} bytes in {} files:", p.name, p.version, p.description, p.payload_size, p.files.len());
            for f in &p.files {
                println!("  /{} ({} bytes, mode {:o})", f.path, f.size, f.mode);
            }
            match installed(&p.name) {
                Some(i) => println!("installed: {}", i.version),
                None => println!("not installed"),
            }
            Ok(())
        }
        Some("install") if !names.is_empty() => {
            need_root()?;
            let url = repository()?;
            for name in names {
                say(&install(&url, name)?);
            }
            Ok(())
        }
        Some("upgrade") if names.is_empty() => {
            need_root()?;
            let url = repository()?;
            let list = index(&url)?;
            let mut any = false;
            for have in all_installed() {
                if let Some(p) = list.find(&have.name) {
                    if package::state(Some(&have.version), &p.version) == State::Upgradable {
                        say(&install(&url, &have.name)?);
                        any = true;
                    }
                }
            }
            if !any {
                println!("every installed package is up to date");
            }
            Ok(())
        }
        Some("remove") if !names.is_empty() => {
            need_root()?;
            for name in names {
                say(&remove(name)?);
            }
            Ok(())
        }
        _ => Err(String::from("usage: pkg list | installed | info NAME | install NAME... | upgrade | remove NAME...")),
    }
}

fn main() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            say(&format!("pkg: {e}"));
            1
        }
    }
}
