//! The Win32 object namespace across processes (BaseNamedObjects) for the
//! kinds whose kernel objects can move between processes: sections and
//! events. Each process keeps its named objects in its own handle table;
//! this registry, one small file per name under `/run/winrun-objects/<uid>`,
//! records which processes hold the name (process id and a handle there that
//! stays open for the life of that process). A process opening the name
//! takes a handle to the same kernel object from one of them through its
//! handle service (`inherit::take`) and records itself in turn, so the name
//! outlives the process that made it while any holder lives.
use crate::objects;
use alloc::{format, string::String, vec::Vec};
use core::sync::atomic::Ordering;

const ROOT: &str = "/run/winrun-objects";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Section,
    Event,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Section => "section",
            Kind::Event => "event",
        }
    }
}

/// The name without its namespace prefix: there is one session, so
/// `Global\`, `Local\` and `Session\<n>\` name the same objects.
fn normalize(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for prefix in ["global\\", "local\\"] {
        if lower.starts_with(prefix) {
            return &name[prefix.len()..];
        }
    }
    if lower.starts_with("session\\") {
        if let Some(i) = name["session\\".len()..].find('\\') {
            return &name["session\\".len() + i + 1..];
        }
    }
    name
}

fn dir() -> String {
    format!("{ROOT}/{}", os_sys::getuid().0)
}

/// The registry file of `name`: an FNV-1a hash keeps any name a valid,
/// short file name; the file holds the full name to tell collisions apart.
fn file_of(name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{}/{h:016x}", dir())
}

/// Serialises updates of one registry file across processes: an exclusive
/// lock file next to it (removed when dropped). A lock older than two
/// seconds is left from a process that died holding it and is taken over.
struct FileLock(String);

impl FileLock {
    fn take(file: &str) -> FileLock {
        let lock = format!("{file}.lock");
        let started = os_std::time::Instant::now();
        loop {
            match os_sys::open(&lock, osyx_abi::OPEN_WRITE | osyx_abi::OPEN_CREATE | osyx_abi::OPEN_EXCLUSIVE, 0o600) {
                Ok(fd) => {
                    let _ = os_sys::close(fd);
                    return FileLock(lock);
                }
                Err(_) if started.elapsed() >= core::time::Duration::from_secs(2) => {
                    let _ = os_sys::unlink(&lock);
                }
                Err(_) => os_std::thread::sleep(core::time::Duration::from_millis(1)),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = os_sys::unlink(&self.0);
    }
}

/// One holder: (process id, handle in it, kind, full name).
fn entries(file: &str) -> Vec<(u64, u64, String, String)> {
    let text = os_std::fs::read_to_string(file).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(4, ' ');
            let pid = parts.next()?.parse().ok()?;
            let handle = u64::from_str_radix(parts.next()?, 16).ok()?;
            Some((pid, handle, String::from(parts.next()?), String::from(parts.next()?)))
        })
        .collect()
}

fn alive(pid: u64) -> bool {
    os_sys::proc_info(pid).is_ok()
}

fn write(file: &str, list: &[(u64, u64, String, String)]) {
    let text: String = list.iter().map(|(p, h, k, n)| format!("{p} {h:x} {k} {n}\n")).collect();
    if text.is_empty() {
        let _ = os_sys::unlink(file);
    } else {
        let _ = os_std::fs::write(file, text);
    }
}

/// Records this process as a holder of `name`: an extra handle to the
/// object at `h`, kept open, is what other processes take from.
pub fn register(name: &str, kind: Kind, h: u64) {
    let Some(object) = objects::get(h) else { return };
    let anchor = objects::insert(object);
    let n = normalize(name);
    let _ = os_std::fs::create_dir_all(&dir());
    let file = file_of(n);
    let _lock = FileLock::take(&file);
    let me = crate::kernel32::pid();
    let mut list: Vec<_> = entries(&file).into_iter().filter(|e| e.0 != me && alive(e.0)).collect();
    list.push((me, anchor, String::from(kind.tag()), String::from(n)));
    write(&file, &list);
}

/// A handle to the object another process made under `name`, recording
/// this process as a holder too; None when no live process holds it.
pub fn open(name: &str, kind: Kind) -> Option<u64> {
    let n = normalize(name);
    let file = file_of(n);
    let me = crate::kernel32::pid();
    let holders: Vec<_> = entries(&file).into_iter().filter(|e| e.0 != me && e.3 == n).collect();
    for (pid, handle, k, _) in holders {
        if k != kind.tag() {
            // The name belongs to an object of another type.
            return None;
        }
        match crate::inherit::take(pid, handle, false) {
            Ok(h) => {
                if crate::VERBOSE.load(Ordering::Relaxed) {
                    os_std::eprintln!("winrun: named {} {n:?} opened from process {pid}", kind.tag());
                }
                register(n, kind, h);
                return Some(h);
            }
            Err(_) => continue,
        }
    }
    None
}

/// Whether another live process holds `name` as an object of a type other
/// than `kind` (CreateX then fails with ERROR_INVALID_HANDLE, as on
/// Windows for a name taken by another type).
pub fn taken_by_other_kind(name: &str, kind: Kind) -> bool {
    let n = normalize(name);
    let me = crate::kernel32::pid();
    entries(&file_of(n)).iter().any(|e| e.0 != me && e.3 == n && e.2 != kind.tag() && alive(e.0))
}
