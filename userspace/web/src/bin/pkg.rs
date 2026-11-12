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
//! of them were written; if a rename fails, the files of the installed
//! version are put back. A package is installed only in the version the
//! signed list names. A package can write only `/opt/pkg/<name>/` and
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
use sha2::{Digest, Sha256};

os_std::entry!(main);

const CONF: &str = "/etc/osyx/packages.conf";
const UPDATE_CONF: &str = "/etc/osyx/update.conf";
const KEYS: &str = "/etc/osyx/packages.keys";
const DB: &str = "/var/lib/osyx/pkg";
const CACHE: &str = "/var/cache/osyx/pkg";
const LOG: &str = "/var/log/osyx-pkg.log";
const LOCK: &str = "/var/lib/osyx/pkg/.lock";
const MANIFEST_MAX: usize = 4 << 20;
const STAGED: &str = osyx_update::STAGED_SUFFIX;
/// Suffix of an installed file set aside while its replacement moves in.
const KEPT: &str = package::KEPT_SUFFIX;

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

/// A repository asked for over HTTPS is not answered over plain HTTP: a
/// redirect there would let someone on the path serve an older signed
/// list. `from` is where the answer came from.
fn no_downgrade(secure: bool, from: &str) -> Result<(), String> {
    if secure && !from.starts_with("https://") {
        return Err(format!("redirected from HTTPS to {from}: refused"));
    }
    Ok(())
}

/// One `pkg` changes what is installed at a time: the lock is a file made
/// with "create new" that names its owner, and removed when the work is
/// done. A lock is taken over only when its owner is no longer running,
/// however old it is: an installation may take long.
struct Lock {
    /// What this process wrote into the file.
    token: String,
}

/// Whether process `pid` is a running `pkg`. `None`: it cannot be told.
fn pkg_runs(pid: u64) -> Option<bool> {
    let all = os_std::system::processes().ok()?;
    match all.iter().find(|p| p.pid == pid) {
        Some(p) => Some(p.state == os_std::system::PROC_RUNNING && os_std::system::process_name(p).ends_with("pkg")),
        // A full list may have left it out.
        None if all.len() >= 128 => None,
        None => Some(false),
    }
}

impl Lock {
    fn take() -> Result<Lock, String> {
        fs::create_dir_all(DB).map_err(|e| format!("{DB}: {e}"))?;
        let pid = os_std::process::id().map_err(|e| format!("{e}"))?;
        let nanos = os_std::time::SystemTime::now().duration_since(os_std::time::SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let token = format!("{pid} {nanos}\n");
        let busy = || format!("another pkg is installing or removing ({LOCK} exists); try again when it is done");
        for _ in 0..2 {
            match fs::OpenOptions::new().write(true).create_new(true).open(LOCK) {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(token.as_bytes()).and_then(|()| f.sync_all()) {
                        drop(f);
                        let _ = fs::remove_file(LOCK);
                        return Err(format!("{LOCK}: {e}"));
                    }
                    return Ok(Lock { token });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // The owner writes its name right after making the
                    // file: an empty one is given a moment.
                    let mut held = String::new();
                    for _ in 0..10 {
                        held = fs::read_to_string(LOCK).unwrap_or_default();
                        if held.ends_with('\n') {
                            break;
                        }
                        os_std::thread::sleep(os_std::time::Duration::from_millis(50));
                    }
                    let owner = held.split_whitespace().next().and_then(|p| p.parse::<u64>().ok());
                    let Some(owner) = owner.filter(|_| held.ends_with('\n')) else {
                        return Err(format!("{LOCK} does not say whose it is; remove it when no pkg is running"));
                    };
                    if owner != pid && pkg_runs(owner) != Some(false) {
                        return Err(busy());
                    }
                    // Its owner died. Removed only if it is still that
                    // lock: another pkg may have taken it over meanwhile.
                    if fs::read_to_string(LOCK).is_ok_and(|now| now == held) {
                        let _ = fs::remove_file(LOCK);
                    }
                }
                Err(e) => return Err(format!("{LOCK}: {e}")),
            }
        }
        Err(busy())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Only this process's own lock.
        if fs::read_to_string(LOCK).is_ok_and(|held| held == self.token) {
            let _ = fs::remove_file(LOCK);
        }
    }
}

/// Files of earlier versions that could not be removed when a package was
/// upgraded (`<name>.stale`, one path per line): tried again whenever the
/// package is installed, upgraded or removed.
fn stale_list(name: &str) -> String {
    format!("{DB}/{name}.stale")
}

/// Tries to remove the files in `name`'s stale list and those in `more`,
/// except the files in `live` (the installed version has them again);
/// what still will not go stays in the list. Returns what is left.
fn clear_stale(name: &str, more: &[String], live: &[String]) -> Vec<String> {
    let list = stale_list(name);
    let mut paths: Vec<String> = fs::read_to_string(&list).unwrap_or_default().lines().map(String::from).filter(|l| !l.is_empty()).collect();
    for p in more {
        if !paths.contains(p) {
            paths.push(p.clone());
        }
    }
    paths.retain(|p| !live.contains(p));
    // Only a package's own paths, whatever the list says.
    paths.retain(|p| package::allowed_path(name, p.trim_start_matches('/')) && remove_file(p).is_err());
    if paths.is_empty() {
        let _ = fs::remove_file(&list);
    } else {
        let _ = fs::write(&list, paths.join("\n") + "\n");
    }
    paths
}

/// A small download kept in memory.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    struct Buf(Vec<u8>, bool);
    impl osyx_web::Sink for Buf {
        fn start(&mut self, from: &str, length: Option<u64>) -> Result<(), String> {
            no_downgrade(self.1, from)?;
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
    let mut b = Buf(Vec::new(), url.starts_with("https://"));
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

/// Downloads a payload of exactly `size` bytes with SHA-256 `sha256` to
/// `path`. More bytes than the signed manifest names are refused as they
/// arrive, and nothing is left behind on any failure.
fn download_payload(url: &str, path: &str, size: u64, sha256: &[u8; 32]) -> Result<(), String> {
    struct Limited {
        file: fs::File,
        hash: Sha256,
        left: u64,
        secure: bool,
    }
    impl osyx_web::Sink for Limited {
        fn start(&mut self, from: &str, length: Option<u64>) -> Result<(), String> {
            no_downgrade(self.secure, from)?;
            match length {
                Some(n) if n != self.left => Err(format!("the payload is {n} bytes, the manifest says {}", self.left)),
                _ => Ok(()),
            }
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
            if bytes.len() as u64 > self.left {
                return Err(String::from("the payload is longer than the manifest says"));
            }
            self.left -= bytes.len() as u64;
            self.hash.update(bytes);
            self.file.write_all(bytes).map_err(|e| format!("write: {e}"))
        }
    }
    let file = fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut sink = Limited { file, hash: Sha256::new(), left: size, secure: url.starts_with("https://") };
    let result = osyx_web::get(url, &mut sink).map_err(|e| format!("{url}: {e}")).and_then(|_| {
        if sink.left != 0 {
            return Err(format!("the payload is {} bytes short", sink.left));
        }
        if <[u8; 32]>::from(sink.hash.clone().finalize()) != *sha256 {
            return Err(String::from("the payload does not match its SHA-256 in the manifest"));
        }
        sink.file.sync_all().map_err(|e| format!("{path}: {e}"))
    });
    drop(sink);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Removes `path` (one that is already gone is fine) and then its
/// directories while they are empty, up to (not including) `/opt/pkg` or
/// the launcher directory.
fn remove_file(path: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{path}: {e}")),
    }
    remove_empty_dirs(path);
    Ok(())
}

fn remove_empty_dirs(path: &str) {
    let mut dir = path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if !parent.starts_with("/opt/pkg/") || fs::remove_dir(parent).is_err() {
            break;
        }
        dir = parent;
    }
}

/// Tries again to remove what an earlier upgrade of the installed package
/// `p` could not, also when there is nothing to install.
fn retry_stale(p: &Package) {
    if fs::metadata(stale_list(&p.name)).is_err() {
        return;
    }
    let live: Vec<String> = p.files.iter().map(|f| format!("/{}", f.path)).collect();
    let left_over = clear_stale(&p.name, &[], &live);
    if left_over.is_empty() {
        say(&format!("pkg: {}: files of an older version were removed", p.name));
    } else {
        say(&format!("pkg: {}: files of an older version still cannot be removed: {}", p.name, left_over.join(", ")));
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
    // The signed list says which version the repository has now: a
    // manifest of another version, however well signed, is one the
    // repository withdrew or has not published yet.
    match index(url)?.find(name) {
        Some(listed) if listed.version == p.version => {}
        Some(listed) => return Err(format!("{manifest} is version {}, the repository lists {}: refused", p.version, listed.version)),
        None => return Err(format!("the repository does not list {name}: refused")),
    }
    let old = installed(name);
    if let Some(old) = &old {
        if old.version == p.version {
            retry_stale(old);
            return Ok(format!("{name} {} is already installed", p.version));
        }
        if !osyx_update::newer(&p.version, &old.version) {
            return Err(format!("{name} {} is installed and the repository has the older {}: not installed", old.version, p.version));
        }
    }
    fs::create_dir_all(CACHE).map_err(|e| format!("{CACHE}: {e}"))?;
    let payload = format!("{CACHE}/{bin}");
    download_payload(&format!("{url}/{bin}"), &payload, p.payload_size, &p.payload_sha256).map_err(|e| format!("download: {e}"))?;
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
    // The record of the new version, written beside the old one.
    let record = format!("{DB}/{name}.txt");
    let new_record = format!("{record}{STAGED}");
    let result = result.and_then(|()| {
        fs::create_dir_all(DB).map_err(|e| format!("{DB}: {e}"))?;
        let mut f = fs::File::create(&new_record).map_err(|e| format!("{new_record}: {e}"))?;
        f.write_all(text.as_bytes()).and_then(|()| f.sync_all()).map_err(|e| format!("{new_record}: {e}"))
    });
    let unstage = |staged: &[String]| {
        for to in staged {
            let _ = fs::remove_file(format!("{to}{STAGED}"));
            remove_empty_dirs(to);
        }
        let _ = fs::remove_file(&new_record);
    };
    if let Err(e) = result {
        unstage(&staged);
        return Err(e);
    }
    // Commit. Each file of the installed version steps aside before the
    // new one takes its place, so that a failure half way puts every one
    // of them back: the old version stays whole.
    let mut done: Vec<(&String, bool)> = Vec::new();
    let commit = (|| -> Result<(), String> {
        for to in &staged {
            let had = fs::metadata(to).is_ok();
            if had {
                fs::rename(to, format!("{to}{KEPT}")).map_err(|e| format!("{to}: {e}"))?;
            }
            done.push((to, had));
            fs::rename(format!("{to}{STAGED}"), to).map_err(|e| format!("{to}: {e}"))?;
        }
        fs::rename(&new_record, &record).map_err(|e| format!("{record}: {e}"))
    })();
    if let Err(e) = commit {
        for (to, had) in done.iter().rev() {
            let _ = fs::remove_file(to);
            if *had {
                let _ = fs::rename(format!("{to}{KEPT}"), to);
            }
        }
        unstage(&staged);
        return Err(format!("{e}; the installed version was put back"));
    }
    for (to, had) in &done {
        if *had {
            let _ = fs::remove_file(format!("{to}{KEPT}"));
        }
    }
    // What the new version no longer has, and what an earlier upgrade
    // could not remove. A file that will not go is kept on a list and
    // tried again next time, and reported.
    let dropped: Vec<String> = old.iter().flat_map(|old| package::stale_files(old, &p)).map(|path| format!("/{path}")).collect();
    let live: Vec<String> = p.files.iter().map(|f| format!("/{}", f.path)).collect();
    let left_over = clear_stale(name, &dropped, &live);
    if !left_over.is_empty() {
        say(&format!("pkg: {name}: files of an older version could not be removed and will be tried again: {}", left_over.join(", ")));
    }
    Ok(match old {
        Some(old) => format!("{name} upgraded from {} to {} ({} files)", old.version, p.version, p.files.len()),
        None => format!("{name} {} installed ({} files)", p.version, p.files.len()),
    })
}

fn remove(name: &str) -> Result<String, String> {
    let p = installed(name).ok_or_else(|| format!("{name} is not installed"))?;
    // The record goes last, and only when every file went: what could not
    // be removed can be tried again.
    let mut failed: Vec<String> = p.files.iter().filter_map(|f| remove_file(&format!("/{}", f.path)).err()).collect();
    failed.extend(clear_stale(name, &[], &[]).into_iter().map(|p| format!("{p} cannot be removed")));
    if !failed.is_empty() {
        return Err(format!("{name} is still installed: {}", failed.join("; ")));
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
            let _lock = Lock::take()?;
            let url = repository()?;
            for name in names {
                say(&install(&url, name)?);
            }
            Ok(())
        }
        Some("upgrade") if names.is_empty() => {
            need_root()?;
            let _lock = Lock::take()?;
            let url = repository()?;
            let list = index(&url)?;
            let mut any = false;
            for have in all_installed() {
                let upgradable = list.find(&have.name).is_some_and(|p| package::state(Some(&have.version), &p.version) == State::Upgradable);
                if upgradable {
                    say(&install(&url, &have.name)?);
                    any = true;
                } else {
                    retry_stale(&have);
                }
            }
            if !any {
                println!("every installed package is up to date");
            }
            Ok(())
        }
        Some("remove") if !names.is_empty() => {
            need_root()?;
            let _lock = Lock::take()?;
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
