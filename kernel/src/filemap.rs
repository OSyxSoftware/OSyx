//! Shared file views. Frames are cached by volume/inode, so independent
//! sections and processes see the same bytes, including after file growth.
//! The cache retains dirty backing after a process exits; a kernel worker
//! writes and retires unused entries outside all process/interrupt locks.
//! Lock order: CACHE -> Backing.state -> OpenFile -> filesystem. Explicit
//! truncation enters CACHE before locking its file; section mapping releases
//! Backing.state before entering the process table.
use crate::{mem, process, sched, shm::SharedMemory, sync::KMutex};
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use osyx_abi::Errno;

type Key = (u64, u64);
static CACHE: KMutex<BTreeMap<Key, Arc<Backing>>> = KMutex::new(BTreeMap::new());
static WORKER: AtomicBool = AtomicBool::new(false);

/// Serialize explicit file truncation against creation and delayed writeback.
/// Callers must not hold an open-file lock: the cache precedes file locks.
pub fn truncate<T>(key: (u64, u64), size: u64, operation: impl FnOnce() -> Result<T, Errno>) -> Result<T, Errno> {
    let mut cache = CACHE.lock();
    if let Some(backing) = cache.get(&key) {
        if Arc::strong_count(backing) != 1 {
            return Err(Errno::Busy);
        }
        if size != 0 {
            let old_size = backing.state.lock().size;
            backing.flush(0, old_size, false)?;
        }
    }
    let result = operation()?;
    cache.remove(&key);
    Ok(result)
}

struct State {
    pages: Vec<Arc<SharedMemory>>,
    size: u64,
    file: process::FileHandle,
    can_write: bool,
}
struct Backing {
    state: KMutex<State>,
}

pub struct Section {
    backing: Arc<Backing>,
    pub size: u64,
    pub writable: bool,
}

pub fn create(file: process::FileHandle, requested: u64, writable: bool) -> Result<Arc<Section>, Errno> {
    let (key, size) = {
        let mut f = file.lock();
        f.mapping_access(writable)?;
        let stat = f.stat()?;
        let size = if requested == 0 { stat.size } else { requested };
        if size == 0 {
            return Err(Errno::InvalidArgument);
        }
        if size > osyx_abi::ipc::MAX_SHM {
            return Err(Errno::NotSupported);
        }
        if size > stat.size && !writable {
            return Err(Errno::PermissionDenied);
        }
        ((stat.dev, stat.ino), size)
    };
    let backing = {
        let mut cache = CACHE.lock();
        let backing = match cache.get(&key) {
            Some(b) => b.clone(),
            None => Arc::new(Backing {
                state: KMutex::new(State { pages: Vec::new(), size: 0, file: file.clone(), can_write: writable }),
            }),
        };
        backing.grow(file, size, writable)?;
        cache.insert(key, backing.clone());
        backing
    };
    if !WORKER.swap(true, Ordering::SeqCst) {
        sched::spawn_kernel("filemap-writeback", writeback_worker, 0, 0);
    }
    Ok(Arc::new(Section { backing, size, writable }))
}

impl Backing {
    fn grow(&self, file: process::FileHandle, size: u64, writable: bool) -> Result<(), Errno> {
        let mut state = self.state.lock();
        if writable && !state.can_write {
            state.file = file;
            state.can_write = true;
        }
        if size <= state.size {
            return Ok(());
        }
        let source = state.file.clone();
        let mut source = source.lock();
        if size > source.stat()?.size {
            source.truncate(size)?;
        }
        let old_pages = state.pages.len();
        let total = size.div_ceil(mem::PAGE_SIZE) as usize;
        let mut extra = Vec::with_capacity(total - old_pages);
        for index in old_pages..total {
            let page = SharedMemory::new(mem::PAGE_SIZE)?;
            let offset = index as u64 * mem::PAGE_SIZE;
            let count = (size - offset).min(mem::PAGE_SIZE) as usize;
            // SAFETY: an exclusive newly allocated RAM page, kept by page.
            let bytes =
                unsafe { core::slice::from_raw_parts_mut(mem::phys_to_virt(page.frames()[0]) as *mut u8, count) };
            read_page(&source, offset, bytes)?;
            extra.push(page);
        }
        // A section may grow within the final page of an earlier view.
        // Load only the newly covered bytes, preserving live mapped data.
        if state.size != 0 && !state.size.is_multiple_of(mem::PAGE_SIZE) {
            let offset = state.size;
            let index = (offset / mem::PAGE_SIZE) as usize;
            let within = offset % mem::PAGE_SIZE;
            let count = (size - offset).min(mem::PAGE_SIZE - within) as usize;
            // SAFETY: the backing retains this RAM page throughout the copy.
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    (mem::phys_to_virt(state.pages[index].frames()[0]) + within) as *mut u8,
                    count,
                )
            };
            read_page(&source, offset, bytes)?;
        }
        state.pages.extend(extra);
        state.size = size;
        Ok(())
    }

    fn flush(&self, offset: u64, len: u64, sync: bool) -> Result<(), Errno> {
        let state = self.state.lock();
        if offset > state.size || len > state.size - offset {
            return Err(Errno::InvalidArgument);
        }
        if !state.can_write {
            return Ok(());
        }
        let mut file = state.file.lock();
        let mut at = offset;
        let end = offset + len;
        while at < end {
            let index = (at / mem::PAGE_SIZE) as usize;
            let within = at % mem::PAGE_SIZE;
            let count = (end - at).min(mem::PAGE_SIZE - within) as usize;
            // SAFETY: this cached RAM page remains owned while state is held.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    (mem::phys_to_virt(state.pages[index].frames()[0]) + within) as *const u8,
                    count,
                )
            };
            let mut done = 0;
            while done < count {
                let n = file.write_at(at + done as u64, &bytes[done..])?;
                if n == 0 {
                    return Err(Errno::Io);
                }
                done += n;
            }
            at += count as u64;
        }
        if sync {
            file.sync()?;
        }
        Ok(())
    }
}

fn read_page(file: &crate::vfs::OpenFile, offset: u64, out: &mut [u8]) -> Result<(), Errno> {
    let mut done = 0;
    while done < out.len() {
        let n = file.read_at(offset + done as u64, &mut out[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(())
}

impl Section {
    pub fn map(&self, offset: u64, len: u64, writable: bool) -> Result<u64, Errno> {
        if writable && !self.writable {
            return Err(Errno::PermissionDenied);
        }
        if !offset.is_multiple_of(mem::PAGE_SIZE) || len == 0 || offset >= self.size || len > self.size - offset {
            return Err(Errno::InvalidArgument);
        }
        let frames = {
            let state = self.backing.state.lock();
            let first = (offset / mem::PAGE_SIZE) as usize;
            let count = len.div_ceil(mem::PAGE_SIZE) as usize;
            state.pages[first..first + count].iter().map(|p| p.frames()[0]).collect()
        };
        let identity = (Arc::as_ptr(&self.backing) as usize, offset);
        let alias = SharedMemory::alias(frames, self.backing.clone(), identity);
        process::map_shared(alias, writable)
    }

    pub fn flush(&self, offset: u64, len: u64) -> Result<(), Errno> {
        if offset > self.size || len > self.size - offset {
            return Err(Errno::InvalidArgument);
        }
        self.backing.flush(offset, len, false)
    }
}

fn writeback_worker(_: u64, _: u64) {
    loop {
        sched::sleep_ticks(100);
        // Only the cache holds an unused backing. Holding this sleeping lock
        // through writeback prevents a new mapping reading the old disk data.
        let mut cache = CACHE.lock();
        let keys: Vec<_> = cache.keys().copied().collect();
        for key in keys {
            let backing = &cache[&key];
            if Arc::strong_count(backing) != 1 {
                continue;
            }
            let size = backing.state.lock().size;
            if backing.flush(0, size, true).is_ok() {
                cache.remove(&key);
            }
        }
    }
}
