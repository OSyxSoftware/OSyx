//! First start of an installed system (`/bin/firstboot`), run by init
//! beside the desktop session whenever `/etc/osyx/firstboot.conf` exists.
//! It finishes the tasks the installer recorded there and keeps state in
//! `/etc/osyx/firstboot.state`: a task that failed (no network, a failed
//! download or installation) is retried on the next start; one that
//! finished never runs again. Everything is logged to
//! `/var/log/osyx-firstboot.log`.
//!
//! `zen=1`: Zen Browser. The official Windows installer of a release OSyx
//! was tested with is downloaded over verified HTTPS from GitHub, checked
//! against the SHA-256 digest GitHub publishes for that asset, and unpacked:
//! the installer is a 32-bit 7-Zip self-extractor (OSyx runs only 64-bit
//! Windows programs), so its archive is read natively (`osyx-7z`, CRC32 of
//! every file checked) and `core/` becomes `/opt/zen`. The installer's own
//! code never runs. Zen is then added to the applications, the desktop,
//! the registry's browser associations and as the default browser.
#![no_std]
#![no_main]

use os_std::io::Write;
use os_std::prelude::*;
use os_std::io::{Read, Seek, SeekFrom};
use os_std::time::Duration;
use os_std::{fs, println, thread};

os_std::entry!(main);

const CONF: &str = "/etc/osyx/firstboot.conf";
const STATE: &str = "/etc/osyx/firstboot.state";
const LOG: &str = "/var/log/osyx-firstboot.log";

const ZEN_VERSION: &str = "1.23.4b";
const ZEN_URL: &str = "https://github.com/zen-browser/desktop/releases/download/1.23.4b/zen.installer.exe";
/// The digest GitHub publishes for that release asset.
const ZEN_SHA256: &str = "f8d6f7b52f5a08eb44ebc487fad2cf14c78887f38cd21552f0801e3e8f8269df";
const ZEN_CACHE: &str = "/var/cache/osyx/zen.installer.exe";
const ZEN_DIR: &str = "/opt/zen";
/// Attempts within one start before giving up until the next start.
const DOWNLOAD_TRIES: u32 = 3;

struct Log(Option<fs::File>);

impl Log {
    fn line(&mut self, s: &str) {
        println!("firstboot: {s}");
        if let Some(f) = &mut self.0 {
            let _ = f.write_all(format!("{s}\n").as_bytes());
        }
    }
}

fn setting(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| String::from(v.trim())))
}

fn main() -> i32 {
    let Ok(conf) = fs::read_to_string(CONF) else {
        return 0;
    };
    let _ = fs::create_dir_all("/var/log");
    let mut log = Log(fs::OpenOptions::new().append(true).create(true).open(LOG).ok());
    let state = fs::read_to_string(STATE).unwrap_or_default();
    let attempts: u32 = setting(&state, "zen.attempts").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut new_state = String::new();
    let mut pending = 0;
    if setting(&conf, "zen").as_deref() == Some("1") && setting(&state, "zen").as_deref() != Some("done") {
        log.line(&format!("Zen Browser {ZEN_VERSION}: attempt {}", attempts + 1));
        match zen_task(&mut log) {
            Ok(()) => {
                log.line("Zen Browser: installed");
                new_state.push_str("zen=done\n");
            }
            Err(e) => {
                log.line(&format!("Zen Browser: not installed: {e}; retrying on the next start"));
                new_state.push_str(&format!("zen=pending\nzen.attempts={}\nzen.error={e}\n", attempts + 1));
                pending += 1;
            }
        }
    } else {
        log.line("nothing to do");
    }
    if let Err(e) = fs::write(STATE, new_state.as_bytes()) {
        log.line(&format!("cannot record the state in {STATE}: {e}"));
    }
    println!("FIRSTBOOT_RESULT {}", if pending == 0 { "ok" } else { "pending" });
    pending
}

/// The installer in the cache, downloading it when it is missing or wrong.
fn zen_installer(log: &mut Log) -> Result<(), String> {
    if osyx_web::sha256_file(ZEN_CACHE).is_ok_and(|h| h == ZEN_SHA256) {
        log.line(&format!("using the verified installer in {ZEN_CACHE}"));
        return Ok(());
    }
    let _ = fs::create_dir_all("/var/cache/osyx");
    let mut last = String::new();
    for attempt in 1..=DOWNLOAD_TRIES {
        log.line(&format!("downloading {ZEN_URL} (try {attempt} of {DOWNLOAD_TRIES})"));
        let mut shown = 0u64;
        let result = osyx_web::download(ZEN_URL, ZEN_CACHE, Some(ZEN_SHA256), &mut |done, total| {
            if done == 0 || done >> 24 != shown >> 24 {
                shown = done;
                match total {
                    Some(t) => println!("firstboot: download {} / {} MiB", done >> 20, t >> 20),
                    None => println!("firstboot: download {} MiB", done >> 20),
                }
            }
        });
        match result {
            Ok(n) => {
                log.line(&format!("downloaded {n} bytes, SHA-256 verified"));
                return Ok(());
            }
            Err(e) => {
                log.line(&format!("download failed: {e}"));
                last = e;
                if attempt < DOWNLOAD_TRIES {
                    thread::sleep(Duration::from_secs(10 * attempt as u64));
                }
            }
        }
    }
    Err(format!("download failed: {last}"))
}

fn zen_task(log: &mut Log) -> Result<(), String> {
    os_std::net::lookup_host("github.com").map_err(|e| format!("no network: cannot resolve github.com ({e})"))?;
    zen_installer(log)?;
    if fs::metadata(format!("{ZEN_DIR}/zen.exe")).is_err() {
        unpack(log)?;
    }
    register(log)
}

struct Installer(fs::File, u64);

impl osyx_7z::Source for Installer {
    fn size(&self) -> u64 {
        self.1
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> osyx_7z::Result<usize> {
        self.0.seek(SeekFrom::Start(offset)).map_err(|_| osyx_7z::Error::Io)?;
        self.0.read(buf).map_err(|_| osyx_7z::Error::Io)
    }
}

/// Writes the archive's `core/` tree below a directory.
struct Unpack {
    root: String,
    file: Option<fs::File>,
    files: usize,
    bytes: u64,
}

impl Unpack {
    /// Target path of an archive entry, `None` outside `core/`.
    fn target(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix("core")?;
        if rest.is_empty() {
            return Some(self.root.clone());
        }
        rest.strip_prefix('/').map(|r| format!("{}/{r}", self.root))
    }
}

impl osyx_7z::Sink for Unpack {
    fn dir(&mut self, path: &str) -> osyx_7z::Result<()> {
        if let Some(t) = self.target(path) {
            fs::create_dir_all(&t).map_err(|_| osyx_7z::Error::Sink)?;
        }
        Ok(())
    }
    fn file_start(&mut self, path: &str, _size: u64) -> osyx_7z::Result<()> {
        self.file = None;
        if let Some(t) = self.target(path) {
            if let Some((parent, _)) = t.rsplit_once('/') {
                fs::create_dir_all(parent).map_err(|_| osyx_7z::Error::Sink)?;
            }
            self.file = Some(fs::File::create(&t).map_err(|_| osyx_7z::Error::Sink)?);
            self.files += 1;
        }
        Ok(())
    }
    fn file_data(&mut self, data: &[u8]) -> osyx_7z::Result<()> {
        if let Some(f) = &mut self.file {
            f.write_all(data).map_err(|_| osyx_7z::Error::Sink)?;
            self.bytes += data.len() as u64;
        }
        Ok(())
    }
    fn file_end(&mut self) -> osyx_7z::Result<()> {
        if let Some(f) = self.file.take() {
            f.sync_all().map_err(|_| osyx_7z::Error::Sink)?;
        }
        Ok(())
    }
}

/// Unpacks the verified installer's `core/` into `/opt/zen` (through a
/// temporary directory, so an interrupted run leaves no half-installed Zen).
fn unpack(log: &mut Log) -> Result<(), String> {
    let partial = format!("{ZEN_DIR}.partial");
    let _ = fs::remove_dir_all(&partial);
    let file = fs::File::open(ZEN_CACHE).map_err(|e| format!("{ZEN_CACHE}: {e}"))?;
    let len = file.metadata().map_err(|e| format!("{ZEN_CACHE}: {e}"))?.len();
    let mut src = Installer(file, len);
    let base = osyx_7z::find(&mut src, 8 << 20).map_err(|e| format!("installer archive: {e}"))?;
    log.line(&format!("unpacking the installer's archive (at byte {base}) into {ZEN_DIR}"));
    let mut sink = Unpack { root: partial.clone(), file: None, files: 0, bytes: 0 };
    osyx_7z::extract(src, base, &mut sink).map_err(|e| format!("unpacking: {e}"))?;
    let exe = format!("{partial}/zen.exe");
    fs::metadata(&exe).map_err(|_| String::from("the archive has no core/zen.exe"))?;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).map_err(|e| format!("{exe}: {e}"))?;
    let _ = fs::remove_dir_all(ZEN_DIR);
    fs::rename(&partial, ZEN_DIR).map_err(|e| format!("{ZEN_DIR}: {e}"))?;
    log.line(&format!("unpacked {} files ({} MiB) into {ZEN_DIR}, CRC32 verified", sink.files, sink.bytes >> 20));
    Ok(())
}

/// Registry lines (the format of compatibility/win32/src/registry.rs) that
/// make Zen the handler of web links and pages, as scripts/seed-registry.py
/// writes them for preinstalled Zen.
fn zen_registry() -> String {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let mut lines: Vec<String> = Vec::new();
    let mut value = |path: &str, name: &str, text: &str| {
        let parts: Vec<&str> = path.split('\\').collect();
        for i in 1..=parts.len() {
            let k = format!("K\t{}", hex(parts[..i].join("\\").as_bytes()));
            if !lines.contains(&k) {
                lines.push(k);
            }
        }
        let data: Vec<u8> = text.encode_utf16().chain(core::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect();
        lines.push(format!("V\t{}\t{}\t1\t{}", hex(path.as_bytes()), hex(name.as_bytes()), hex(&data)));
    };
    let command = r#""C:\system\bin\zen" "%1""#;
    let classes = r"SOFTWARE\Classes";
    for scheme in ["http", "https"] {
        value(&format!(r"{classes}\{scheme}"), "", "URL:Zen Browser");
        value(&format!(r"{classes}\{scheme}"), "URL Protocol", "");
        value(&format!(r"{classes}\{scheme}\shell\open\command"), "", command);
    }
    value(&format!(r"{classes}\ZenHTML"), "", "Zen HTML Document");
    value(&format!(r"{classes}\ZenHTML\shell\open\command"), "", command);
    for ext in [".htm", ".html", ".xhtml"] {
        value(&format!(r"{classes}\{ext}"), "", "ZenHTML");
    }
    let client = r"SOFTWARE\Clients\StartMenuInternet";
    value(client, "", "Zen");
    value(&format!(r"{client}\Zen"), "", "Zen Browser");
    value(&format!(r"{client}\Zen\shell\open\command"), "", r#""C:\system\bin\zen""#);
    let caps = format!(r"{client}\Zen\Capabilities");
    value(&caps, "ApplicationName", "Zen Browser");
    value(&caps, "ApplicationDescription", "Browse the web with Zen");
    for scheme in ["http", "https"] {
        value(&format!(r"{caps}\URLAssociations"), scheme, scheme);
    }
    for ext in [".htm", ".html", ".xhtml"] {
        value(&format!(r"{caps}\FileAssociations"), ext, "ZenHTML");
    }
    value(r"SOFTWARE\RegisteredApplications", "Zen Browser", &caps);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Adds Zen to the applications, the user's desktop and as default browser.
fn register(log: &mut Log) -> Result<(), String> {
    if fs::metadata("/system/bin/zen").is_err() {
        return Err(String::from("the Zen launcher /system/bin/zen is missing"));
    }
    let entry = "Name=Zen Browser\nExec=/system/bin/zen\nIcon=zen\nComment=Browse the web with Zen (default browser)\n";
    let w = |path: &str, data: &str| fs::write(path, data.as_bytes()).map_err(|e| format!("{path}: {e}"));
    w("/system/share/applications/95-zen.app", entry)?;
    w("/etc/osyx/default-browser", "/system/bin/zen\n")?;
    let desktop = "/home/user/Desktop/95-zen.app";
    w(desktop, entry)?;
    // Later lines of a hive replace earlier values of the same name.
    let hive = "/etc/osyx/registry/machine.reg";
    let mut f = fs::OpenOptions::new().append(true).create(true).open(hive).map_err(|e| format!("{hive}: {e}"))?;
    f.write_all(zen_registry().as_bytes()).map_err(|e| format!("{hive}: {e}"))?;
    log.line("Zen Browser added to the applications, the desktop and as the default browser");
    Ok(())
}
