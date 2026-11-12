//! Packages: single applications installed from a repository, signed like
//! system updates.
//!
//! A repository is a directory of files (a GitHub Releases download
//! directory will do):
//!
//! * `packages.txt` and `packages.sig`: the signed list of what there is;
//! * for each package `<name>.pkg.txt` (its manifest), `<name>.pkg.sig`
//!   (the Ed25519 signature of the manifest) and `<name>.pkg.bin` (its
//!   files concatenated in manifest order).
//!
//! ```text
//! osyx-packages 1
//! package <name> <version> <description>
//! ```
//!
//! ```text
//! osyx-package 1
//! name hello
//! version 1.0.0
//! description Says hello
//! payload <sha256> <size>
//! file <sha256> <size> <mode, octal> <path>
//! ```
//!
//! A package owns `opt/pkg/<name>/` and its launcher entry
//! `system/share/applications/pkg-<name>.app`, and may write nowhere else
//! ([`allowed_path`]): it cannot replace a part of the system or of another
//! package. Signatures are checked against the release key built into the
//! system and the keys the administrator added ([`trusted_keys`]).

use crate::{hex, newer, parse_hex, verify, Entry};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MAGIC: &str = "osyx-package 1";
pub const INDEX_MAGIC: &str = "osyx-packages 1";
pub const INDEX: &str = "packages.txt";
pub const INDEX_SIGNATURE: &str = "packages.sig";
/// Where a package's own files go, with its name appended.
pub const PREFIX: &str = "opt/pkg/";
/// Suffix of an installed file set aside while its replacement moves in.
pub const KEPT_SUFFIX: &str = ".osyx-old";
/// Where launcher entries live.
pub const APPLICATIONS: &str = "system/share/applications/";

/// The repository's file names of package `name`: manifest, signature,
/// payload.
pub fn file_names(name: &str) -> [String; 3] {
    [format!("{name}.pkg.txt"), format!("{name}.pkg.sig"), format!("{name}.pkg.bin")]
}

/// A package name: lower case letters, digits and hyphens, starting with a
/// letter or digit, at most 40 characters.
pub fn valid_name(name: &str) -> bool {
    let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let b = name.as_bytes();
    !b.is_empty() && b.len() <= 40 && ok(b[0]) && b.iter().all(|&c| ok(c) || c == b'-')
}

/// The launcher entry of package `name`.
pub fn launcher_entry(name: &str) -> String {
    format!("{APPLICATIONS}pkg-{name}.app")
}

/// May package `name` write `path`? Only its own directory and its
/// launcher entry; relative, no `.`/`..`/empty components or backslashes.
pub fn allowed_path(name: &str, path: &str) -> bool {
    let clean = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control())
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && !path.ends_with(crate::STAGED_SUFFIX)
        && !path.ends_with(KEPT_SUFFIX);
    let own = format!("{PREFIX}{name}/");
    valid_name(name) && clean && ((path.starts_with(&own) && path.len() > own.len()) || path == launcher_entry(name))
}

fn valid_version(v: &str) -> bool {
    !v.is_empty() && v.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u64>().is_ok())
}

/// One line of text: no control characters, not empty, not too long.
fn valid_description(d: &str) -> bool {
    !d.trim().is_empty() && d.len() <= 200 && !d.chars().any(|c| c.is_control())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub payload_size: u64,
    pub payload_sha256: [u8; 32],
    pub files: Vec<Entry>,
}

impl Package {
    pub fn to_text(&self) -> String {
        let mut s = format!(
            "{MAGIC}\nname {}\nversion {}\ndescription {}\npayload {} {}\n",
            self.name,
            self.version,
            self.description,
            hex(&self.payload_sha256),
            self.payload_size
        );
        for f in &self.files {
            s.push_str(&format!("file {} {} {:o} {}\n", hex(&f.sha256), f.size, f.mode, f.path));
        }
        s
    }

    pub fn parse(text: &str) -> Result<Package, String> {
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) {
            return Err(String::from("not an OSyx package manifest"));
        }
        let (mut name, mut version, mut description, mut payload) = (None, None, None, None);
        let mut files: Vec<Entry> = Vec::new();
        for (n, line) in lines.enumerate() {
            let bad = || format!("package manifest line {}: {line}", n + 2);
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            match word {
                "name" if name.is_none() && valid_name(rest) => name = Some(rest.to_string()),
                "version" if version.is_none() && valid_version(rest) => version = Some(rest.to_string()),
                "description" if description.is_none() && valid_description(rest) => description = Some(rest.to_string()),
                "payload" if payload.is_none() => {
                    let mut f = rest.split(' ');
                    let sha = f.next().and_then(parse_hex::<32>).ok_or_else(bad)?;
                    let size: u64 = f.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
                    payload = Some((sha, size));
                }
                "file" => {
                    // The name comes first, so that paths can be checked.
                    let name = name.as_deref().ok_or("package manifest: files before the name")?;
                    let mut f = rest.splitn(4, ' ');
                    let sha256 = f.next().and_then(parse_hex::<32>).ok_or_else(bad)?;
                    let size = f.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
                    let mode = f.next().and_then(|s| u32::from_str_radix(s, 8).ok()).filter(|m| *m <= 0o777).ok_or_else(bad)?;
                    let path = f.next().ok_or_else(bad)?;
                    if !allowed_path(name, path) {
                        return Err(format!("package manifest line {}: package {name} may not write {path}", n + 2));
                    }
                    if files.iter().any(|e| e.path == path) {
                        return Err(format!("package manifest line {}: {path} listed twice", n + 2));
                    }
                    files.push(Entry { path: path.to_string(), size, mode, sha256 });
                }
                "" if line.is_empty() => {}
                _ => return Err(bad()),
            }
        }
        let name = name.ok_or("package manifest without a name")?;
        let version = version.ok_or("package manifest without a version")?;
        let description = description.ok_or("package manifest without a description")?;
        let (payload_sha256, payload_size) = payload.ok_or("package manifest without a payload")?;
        if files.is_empty() {
            return Err(String::from("package manifest without files"));
        }
        if files.iter().try_fold(0u64, |sum, f| sum.checked_add(f.size)) != Some(payload_size) {
            return Err(String::from("the files do not add up to the payload size"));
        }
        Ok(Package { name, version, description, payload_size, payload_sha256, files })
    }
}

/// One package of a repository's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub version: String,
    pub description: String,
}

/// A repository's list of packages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub packages: Vec<Listing>,
}

impl Index {
    pub fn to_text(&self) -> String {
        let mut s = format!("{INDEX_MAGIC}\n");
        for p in &self.packages {
            s.push_str(&format!("package {} {} {}\n", p.name, p.version, p.description));
        }
        s
    }

    pub fn parse(text: &str) -> Result<Index, String> {
        let mut lines = text.lines();
        if lines.next() != Some(INDEX_MAGIC) {
            return Err(String::from("not an OSyx package list"));
        }
        let mut packages: Vec<Listing> = Vec::new();
        for (n, line) in lines.enumerate() {
            if line.is_empty() {
                continue;
            }
            let bad = || format!("package list line {}: {line}", n + 2);
            let mut f = line.splitn(4, ' ');
            let (Some("package"), Some(name), Some(version), Some(description)) = (f.next(), f.next(), f.next(), f.next()) else {
                return Err(bad());
            };
            if !valid_name(name) || !valid_version(version) || !valid_description(description) || packages.iter().any(|p| p.name == name) {
                return Err(bad());
            }
            packages.push(Listing { name: name.to_string(), version: version.to_string(), description: description.to_string() });
        }
        Ok(Index { packages })
    }

    pub fn find(&self, name: &str) -> Option<&Listing> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// What to do about a package, given what is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Current,
    /// The repository has a newer version.
    Upgradable,
}

pub fn state(installed: Option<&str>, available: &str) -> State {
    match installed {
        None => State::NotInstalled,
        Some(have) if newer(available, have) => State::Upgradable,
        Some(_) => State::Current,
    }
}

/// The keys a signature may be made with: the built-in release key and
/// the keys in `extra` (`/etc/osyx/packages.keys`: one key in hex per
/// line, `#` starts a comment). Lines that are not keys are ignored.
pub fn trusted_keys(extra: &str) -> Vec<[u8; 32]> {
    let mut keys: Vec<[u8; 32]> = crate::public_key().into_iter().collect();
    for line in extra.lines() {
        let word = line.split('#').next().unwrap_or("").trim();
        if let Some(key) = parse_hex::<32>(word) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

/// Does `signature` sign `data` with one of `keys`?
pub fn verify_any(data: &[u8], signature: &[u8], keys: &[[u8; 32]]) -> bool {
    keys.iter().any(|key| verify(data, signature, key))
}

/// Files of the installed version `old` that version `new` no longer has:
/// an upgrade removes them.
pub fn stale_files<'a>(old: &'a Package, new: &Package) -> Vec<&'a str> {
    old.files.iter().map(|f| f.path.as_str()).filter(|p| !new.files.iter().any(|f| f.path == *p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{public_from_secret, sign};
    use alloc::vec;

    fn entry(path: &str, size: u64) -> Entry {
        Entry { path: path.to_string(), size, mode: 0o644, sha256: [9; 32] }
    }

    fn hello() -> Package {
        Package {
            name: "hello".to_string(),
            version: "1.2.0".to_string(),
            description: "Says hello, in a window".to_string(),
            payload_size: 30,
            payload_sha256: [3; 32],
            files: vec![entry("opt/pkg/hello/hello.ax", 10), entry("system/share/applications/pkg-hello.app", 20)],
        }
    }

    #[test]
    fn a_manifest_survives_writing_and_reading() {
        let p = hello();
        assert_eq!(Package::parse(&p.to_text()).unwrap(), p);
        assert!(Package::parse(&p.to_text().replace(" 30\n", " 31\n")).is_err(), "sizes must add up");
        assert!(Package::parse("osyx-update 1\nversion 1\n").is_err(), "an update is not a package");
        assert!(Package::parse(&p.to_text().replace("version 1.2.0", "version 1.x")).is_err());
        assert!(Package::parse(&p.to_text().replace("description Says", "description Says\u{7}")).is_err());
        let twice = format!("{}name other\n", p.to_text());
        assert!(Package::parse(&twice).is_err(), "one name only");
        let mut empty = p.clone();
        empty.files.clear();
        empty.payload_size = 0;
        assert!(Package::parse(&empty.to_text()).is_err());
    }

    #[test]
    fn a_package_writes_only_its_own_directory_and_launcher_entry() {
        for ok in ["opt/pkg/hello/hello.ax", "opt/pkg/hello/data/a b.txt", "system/share/applications/pkg-hello.app"] {
            assert!(allowed_path("hello", ok), "{ok}");
        }
        for bad in [
            "bin/axis",
            "system/bin/desktop",
            "etc/passwd",
            "opt/pkg/hello",
            "opt/pkg/hello/",
            "opt/pkg/other/x",
            "opt/pkg/hello-2/x",
            "opt/pkg/hello/../other/x",
            "/opt/pkg/hello/x",
            "opt/pkg/hello/x.osyx-new",
            "opt/pkg/hello/x.osyx-old",
            "system/share/applications/pkg-other.app",
            "system/share/applications/10-terminal.app",
            "esp/EFI/OSYX/KERNEL.ELF",
            "opt\\pkg\\hello\\x",
        ] {
            assert!(!allowed_path("hello", bad), "{bad}");
        }
        // A manifest that tries is refused whole.
        let evil = hello().to_text().replace("opt/pkg/hello/hello.ax", "system/bin/desktop");
        assert!(Package::parse(&evil).unwrap_err().contains("may not write"));
        let other = hello().to_text().replace("name hello", "name other");
        assert!(Package::parse(&other).is_err(), "the files belong to another name");
        for name in ["", "Hello", "-x", "a/b", "a.b", "a b", "ąę", &"x".repeat(41)] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(valid_name("7zip-extras"));
    }

    #[test]
    fn a_list_survives_writing_and_reading_and_orders_versions() {
        let index = Index {
            packages: vec![
                Listing { name: "hello".to_string(), version: "1.2.0".to_string(), description: "Says hello".to_string() },
                Listing { name: "notes".to_string(), version: "0.10.0".to_string(), description: "Keeps notes".to_string() },
            ],
        };
        assert_eq!(Index::parse(&index.to_text()).unwrap(), index);
        assert_eq!(index.find("notes").unwrap().version, "0.10.0");
        assert!(index.find("absent").is_none());
        assert!(Index::parse(&format!("{}package hello 2.0 again\n", index.to_text())).is_err(), "a name twice");
        assert!(Index::parse("osyx-packages 1\npackage hello\n").is_err());
        assert!(Index::parse("osyx-package 1\n").is_err());
        assert_eq!(Index::parse("osyx-packages 1\n").unwrap(), Index::default());
        assert_eq!(state(None, "1.0"), State::NotInstalled);
        assert_eq!(state(Some("0.9.0"), "0.10.0"), State::Upgradable);
        assert_eq!(state(Some("1.0"), "1.0.0"), State::Current);
        assert_eq!(state(Some("2.0"), "1.0"), State::Current);
    }

    #[test]
    fn signatures_are_checked_against_every_trusted_key() {
        let secret = [5u8; 32];
        let public = public_from_secret(&secret);
        let text = hello().to_text();
        let signature = sign(text.as_bytes(), &secret);
        // The built-in key did not sign this.
        let built_in = trusted_keys("");
        assert_eq!(built_in.len(), 1);
        assert!(!verify_any(text.as_bytes(), &signature, &built_in));
        // An added key did.
        let extra = format!("# my repository\n{}  # test key\nnot a key\n{}\n", hex(&public), hex(&public));
        let keys = trusted_keys(&extra);
        assert_eq!(keys.len(), 2, "the built-in key, and the added one once");
        assert!(verify_any(text.as_bytes(), &signature, &keys));
        // One changed byte of the manifest, and nothing verifies.
        let tampered = text.replace("1.2.0", "1.2.1");
        assert!(!verify_any(tampered.as_bytes(), &signature, &keys));
        assert!(!verify_any(text.as_bytes(), &signature[..63], &keys));
    }

    #[test]
    fn an_upgrade_removes_what_the_new_version_dropped() {
        let old = hello();
        let mut new = hello();
        new.files[0].path = "opt/pkg/hello/hello2.ax".to_string();
        assert_eq!(stale_files(&old, &new), vec!["opt/pkg/hello/hello.ax"]);
        assert!(stale_files(&old, &old).is_empty());
    }
}
