//! Shared executable images (the idea behind Windows `SEC_IMAGE`).
//!
//! The first process that loads a PE file at some base address lays it out
//! and relocates it itself, then publishes the result: its pages move into an
//! [`ImageSection`] and stay mapped read-only. Later processes of the same
//! user that load the same file at the same base map those frames instead of
//! building their own copy. Writes (import binding, `.data`) copy the touched
//! page into the writing process (copy-on-write, see `process::cow_fault`),
//! so a large DLL costs physical memory once plus its written pages.
//!
//! Sections are only shared within one user id: the kernel does not verify
//! the published bytes, so a process may only affect processes it could
//! already reach. A key also carries the file's size and modification time,
//! and any write or truncation of the file removes its entries from the
//! lookup table (mappings keep their section until they go).
//!
//! Lock order: TABLE (process) -> CACHE. The cache is a short spin lock; a
//! section's frames are freed when the last mapping drops it.

use crate::mem::free_frame;
use crate::sync::IrqSpinLock;
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    /// Load address: relocated bytes differ per base.
    pub base: u64,
    pub pages: u64,
    pub uid: u32,
}

pub struct ImageSection {
    key: Key,
    frames: Vec<u64>,
}

static CACHE: IrqSpinLock<BTreeMap<Key, Weak<ImageSection>>> = IrqSpinLock::new(BTreeMap::new());
/// Frames held by live sections (diagnostics).
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// Successful lookups and publications (diagnostics).
static HITS: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: AtomicU64 = AtomicU64::new(0);

impl ImageSection {
    /// Takes ownership of `frames` (one per page of the image).
    pub fn new(key: Key, frames: Vec<u64>) -> Arc<ImageSection> {
        debug_assert_eq!(frames.len() as u64, key.pages);
        FRAMES.fetch_add(frames.len() as u64, Ordering::Relaxed);
        Arc::new(ImageSection { key, frames })
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    /// The shared frame of page `index`.
    pub fn frame(&self, index: u64) -> Option<u64> {
        self.frames.get(index as usize).copied()
    }

    pub fn frames(&self) -> &[u64] {
        &self.frames
    }
}

impl Drop for ImageSection {
    fn drop(&mut self) {
        for &f in &self.frames {
            let _ = free_frame(f);
        }
        FRAMES.fetch_sub(self.frames.len() as u64, Ordering::Relaxed);
    }
}

pub fn lookup(key: &Key) -> Option<Arc<ImageSection>> {
    let mut cache = CACHE.lock();
    let found = cache.get(key).and_then(Weak::upgrade);
    if found.is_none() {
        cache.remove(key);
    } else {
        HITS.fetch_add(1, Ordering::Relaxed);
    }
    found
}

/// Makes `section` findable. An entry that is still alive stays (both are
/// equivalent; the later one is simply used by its own process only).
pub fn insert(section: &Arc<ImageSection>) {
    let mut cache = CACHE.lock();
    if cache.get(&section.key).is_some_and(|w| w.strong_count() != 0) {
        return;
    }
    // Drop entries of sections that are gone.
    cache.retain(|_, w| w.strong_count() != 0);
    cache.insert(section.key, Arc::downgrade(section));
    PUBLISHED.fetch_add(1, Ordering::Relaxed);
}

/// The base of a live section of the same file and user as `key` (its
/// `base` and `pages` are ignored), so a loader can place it there too.
pub fn base_of(key: &Key) -> Option<u64> {
    let same = |k: &Key| (k.dev, k.ino, k.size, k.mtime_ns, k.uid) == (key.dev, key.ino, key.size, key.mtime_ns, key.uid);
    CACHE.lock().iter().find(|(k, w)| same(k) && w.strong_count() != 0).map(|(k, _)| k.base)
}

/// The file `(dev, ino)` changed: new loads must not find its old images.
pub fn invalidate(dev: u64, ino: u64) {
    CACHE.lock().retain(|k, _| (k.dev, k.ino) != (dev, ino));
}

/// (live sections, frames they hold, lookups served, publications).
pub fn stats() -> (u64, u64, u64, u64) {
    let live = CACHE.lock().values().filter(|w| w.strong_count() != 0).count() as u64;
    (live, FRAMES.load(Ordering::Relaxed), HITS.load(Ordering::Relaxed), PUBLISHED.load(Ordering::Relaxed))
}

