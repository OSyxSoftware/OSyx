//! Program loading for `spawn` and `execve`: the argument block, finding
//! the executable on the VFS and reading it into memory.
//!
//! * A name without `/` is searched in [`PROGRAM_PATH`] (`/bin`, then
//!   `/usr/bin`); a path with `/` is resolved like any other path, relative
//!   ones from the caller's current directory.
//! * The file must be a regular file with execute permission for the
//!   caller (root: at least one execute bit) and every directory on the way
//!   must be searchable. Set-user-ID and set-group-ID bits are not honoured.
//! * Only the ELF headers and the `PT_LOAD` data are read (not debug
//!   information), and all of it is validated before anything of the
//!   caller changes, so a failing `execve` leaves the old program running.

use crate::fixup::copy_from_user;
use crate::mem::{phys_to_virt, PAGE_SIZE};
use crate::shm::SharedMemory;
use crate::sync::KMutex;
use crate::vfs::{self, Lookup};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use osyx_abi::{Errno, ExecArgs, MAX_EXEC_ARGS, MAX_EXEC_STRINGS, MAX_PATH, PROGRAM_PATH};
use osyx_elf::Segment;

const MAX_CACHED_PAGES: usize = 16 * 1024;
const MAX_CACHED_IMAGE_BYTES: usize = 64 << 20;
struct PageCache {
    images: BTreeMap<(u64, u64), BTreeMap<u64, Arc<SharedMemory>>>,
    pages: usize,
    generation: u64,
}
static SHARED_PAGES: KMutex<PageCache> =
    KMutex::new(PageCache { images: BTreeMap::new(), pages: 0, generation: 0 });
struct ImageCache {
    images: BTreeMap<(u64, u64), Arc<Vec<u8>>>,
    generation: u64,
}
static CACHED_IMAGES: KMutex<ImageCache> = KMutex::new(ImageCache { images: BTreeMap::new(), generation: 0 });

/// Largest executable file accepted.
pub const MAX_IMAGE: u64 = 64 << 20;

/// What to run: the program as named by the caller, `argv` (with `argv[0]`)
/// and the environment.
pub struct ExecRequest {
    pub path: String,
    pub argv: Vec<String>,
    pub envp: Vec<String>,
}

impl ExecRequest {
    /// A request built inside the kernel (the init program).
    pub fn new(path: &str, argv: &[&str], envp: &[&str]) -> Result<ExecRequest, Errno> {
        let r = ExecRequest {
            path: String::from(path),
            argv: argv.iter().map(|s| String::from(*s)).collect(),
            envp: envp.iter().map(|s| String::from(*s)).collect(),
        };
        r.check()?;
        Ok(r)
    }

    /// Copies and validates the [`ExecArgs`] block at user address `ptr`.
    pub fn from_user(ptr: u64) -> Result<ExecRequest, Errno> {
        let mut raw = [0u8; core::mem::size_of::<ExecArgs>()];
        copy_from_user(&mut raw, ptr)?;
        // SAFETY: ExecArgs is plain repr(C) data of exactly this size.
        let a: ExecArgs = unsafe { core::ptr::read_unaligned(raw.as_ptr() as *const ExecArgs) };
        if a.path_len == 0 || a.path_len > MAX_PATH as u64 {
            return Err(Errno::InvalidArgument);
        }
        if a.argv_len > MAX_EXEC_ARGS as u64 || a.envp_len > MAX_EXEC_ARGS as u64 - a.argv_len {
            return Err(Errno::InvalidArgument);
        }
        let path = String::from_utf8(user_bytes(a.path, a.path_len)?).map_err(|_| Errno::InvalidArgument)?;
        if path.contains('\0') {
            return Err(Errno::InvalidArgument);
        }
        let mut argv = split_block(&user_bytes(a.argv, a.argv_len)?)?;
        if argv.is_empty() {
            argv.push(path.clone());
        }
        let envp = split_block(&user_bytes(a.envp, a.envp_len)?)?;
        let r = ExecRequest { path, argv, envp };
        r.check()?;
        Ok(r)
    }

    fn check(&self) -> Result<(), Errno> {
        let bytes: usize = self.argv.iter().chain(&self.envp).map(|s| s.len() + 1).sum();
        let bad = self.path.is_empty()
            || self.path.len() > MAX_PATH
            || self.argv.len() > MAX_EXEC_STRINGS
            || self.envp.len() > MAX_EXEC_STRINGS
            || bytes > MAX_EXEC_ARGS
            || self.argv.iter().chain(&self.envp).any(|s| s.contains('\0'));
        if bad {
            Err(Errno::InvalidArgument)
        } else {
            Ok(())
        }
    }

    /// Process name: the last component of the program path.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').find(|c| !c.is_empty()).unwrap_or(&self.path)
    }
}

fn user_bytes(ptr: u64, len: u64) -> Result<Vec<u8>, Errno> {
    let mut buf = alloc::vec![0u8; len as usize];
    copy_from_user(&mut buf, ptr)?;
    Ok(buf)
}

/// NUL-terminated UTF-8 strings back to back (empty block: no strings).
fn split_block(block: &[u8]) -> Result<Vec<String>, Errno> {
    let Some((&0, body)) = block.split_last() else {
        return if block.is_empty() { Ok(Vec::new()) } else { Err(Errno::InvalidArgument) };
    };
    let parts = body.split(|&b| b == 0);
    if parts.clone().count() > MAX_EXEC_STRINGS {
        return Err(Errno::InvalidArgument);
    }
    parts.map(|p| String::from_utf8(p.to_vec()).map_err(|_| Errno::InvalidArgument)).collect()
}

/// An executable read into memory.
pub struct Program {
    /// Canonical absolute path it was read from (`AT_EXECFN`).
    pub path: String,
    pub image: Arc<Vec<u8>>,
    key: (u64, u64),
}

pub fn invalidate(key: (u64, u64)) {
    crate::image::invalidate(key.0, key.1);
    let mut cache = SHARED_PAGES.lock();
    cache.generation += 1;
    if let Some(pages) = cache.images.remove(&key) {
        cache.pages -= pages.len();
    }
    drop(cache);
    let mut cache = CACHED_IMAGES.lock();
    cache.generation += 1;
    cache.images.remove(&key);
}

pub fn clear() {
    let mut cache = SHARED_PAGES.lock();
    cache.generation += 1;
    cache.images.clear();
    cache.pages = 0;
    drop(cache);
    let mut cache = CACHED_IMAGES.lock();
    cache.generation += 1;
    cache.images.clear();
}

pub fn shared_pages(prog: &Program, seg: &Segment) -> Result<Vec<Arc<SharedMemory>>, Errno> {
    let file = &prog.image[seg.file_offset as usize..(seg.file_offset + seg.file_size) as usize];
    let count = ((seg.page_end() - seg.vaddr) / PAGE_SIZE) as usize;
    let mut pages = Vec::with_capacity(count);
    let mut misses = Vec::new();
    let key = prog.key;
    let generation = {
        let cache = SHARED_PAGES.lock();
        for index in 0..count {
            let va = seg.vaddr + index as u64 * PAGE_SIZE;
            if let Some(page) = cache.images.get(&key).and_then(|pages| pages.get(&va)) {
                pages.push(Some(page.clone()));
            } else {
                misses.push((index, va));
                pages.push(None);
            }
        }
        cache.generation
    };

    for &(index, va) in &misses {
        let page = SharedMemory::new(PAGE_SIZE)?;
        let end = (va + PAGE_SIZE).min(seg.vaddr + file.len() as u64);
        if end > va {
            let offset = (va - seg.vaddr) as usize;
            let src = &file[offset..offset + (end - va) as usize];
            // SAFETY: the new page is exclusively owned and reachable via the HHDM.
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), phys_to_virt(page.frames()[0]) as *mut u8, src.len());
            }
        }
        pages[index] = Some(page);
    }

    if !misses.is_empty() {
        let mut cache = SHARED_PAGES.lock();
        if cache.generation == generation {
            for &(index, va) in &misses {
                let cached = cache.images.get(&key).and_then(|pages| pages.get(&va)).cloned();
                if let Some(cached) = cached {
                    pages[index] = Some(cached);
                } else if cache.pages < MAX_CACHED_PAGES {
                    let page = pages[index].as_ref().expect("miss was filled").clone();
                    cache.images.entry(key).or_default().insert(va, page);
                    cache.pages += 1;
                }
            }
        }
    }

    Ok(pages.into_iter().map(|page| page.expect("miss was filled")).collect())
}

/// Finds `name` as described in the module documentation and reads it.
pub fn load(cx: &Lookup, name: &str) -> Result<Program, Errno> {
    if name.contains('/') {
        return read_executable(cx, name);
    }
    osyx_vfs::path::validate_name(name)?;
    let mut denied = false;
    for dir in PROGRAM_PATH {
        match read_executable(cx, &alloc::format!("{dir}/{name}")) {
            Err(Errno::NotFound | Errno::NotADirectory) => {}
            Err(Errno::PermissionDenied) => denied = true,
            r => return r,
        }
    }
    Err(if denied { Errno::PermissionDenied } else { Errno::NotFound })
}

fn read_executable(cx: &Lookup, p: &str) -> Result<Program, Errno> {
    let mut f = vfs::open_exec(cx, p)?;
    let stat = f.stat()?;
    let size = stat.size;
    if size > MAX_IMAGE {
        return Err(Errno::FileTooLarge);
    }
    let key = (stat.dev, stat.ino);
    let path = String::from(f.path());
    let generation = {
        let cache = CACHED_IMAGES.lock();
        if let Some(image) = cache.images.get(&key).cloned() {
            return Ok(Program { path, image, key });
        }
        cache.generation
    };
    let mut head = [0u8; 64];
    let ranges = if read_at(&mut f, 0, &mut head)? == head.len() { load_ranges(&mut f, &head, size)? } else { None };
    let image = Arc::new(match ranges {
        Some(ranges) => read_ranges(&mut f, &ranges)?,
        None => read_whole(&mut f, size)?,
    });
    let mut cache = CACHED_IMAGES.lock();
    if cache.generation == generation {
        let cached = cache.images.values().map(|image| image.len()).sum::<usize>();
        if cached.saturating_add(image.len()) <= MAX_CACHED_IMAGE_BYTES {
            cache.images.insert(key, image.clone());
        }
    }
    Ok(Program { path, image, key })
}

/// Reads into `buf` from `offset`; returns the bytes read (short at the end
/// of the file).
fn read_at(f: &mut vfs::OpenFile, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
    f.seek(offset as i64, osyx_abi::SEEK_SET)?;
    let mut done = 0;
    while done < buf.len() {
        let n = f.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        f.advance(n);
        done += n;
    }
    Ok(done)
}

fn read_whole(f: &mut vfs::OpenFile, size: u64) -> Result<Vec<u8>, Errno> {
    let mut image = alloc::vec![0u8; size as usize];
    // A file that shrank meanwhile is taken as it is now.
    let n = read_at(f, 0, &mut image)?;
    image.truncate(n);
    Ok(image)
}

/// Most program headers looked at.
const MAX_PHNUM: u64 = 256;

/// The file ranges the loader needs from an ELF64 file: the headers and
/// every `PT_LOAD` segment's data (not, say, debug information). `None` if
/// the header table does not look sane: the whole file is then read and the
/// ELF parser reports what is wrong with it.
fn load_ranges(f: &mut vfs::OpenFile, head: &[u8; 64], size: u64) -> Result<Option<Vec<(u64, u64)>>, Errno> {
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"));
    let (phoff, phentsize) = (u64_at(head, 32), u16::from_le_bytes([head[54], head[55]]));
    let phnum = u16::from_le_bytes([head[56], head[57]]) as u64;
    if &head[0..4] != b"\x7fELF" || phentsize != 56 || phnum > MAX_PHNUM {
        return Ok(None);
    }
    let Some(ph_end) = phoff.checked_add(phnum * 56).filter(|&e| e <= size) else { return Ok(None) };
    let mut table = alloc::vec![0u8; (phnum * 56) as usize];
    if read_at(f, phoff, &mut table)? != table.len() {
        return Ok(None);
    }
    let mut ranges = alloc::vec![(0, ph_end.max(64))];
    for ph in table.as_chunks::<56>().0 {
        if u32::from_le_bytes([ph[0], ph[1], ph[2], ph[3]]) != 1 {
            continue;
        }
        let (offset, filesz) = (u64_at(ph, 8), u64_at(ph, 32));
        match offset.checked_add(filesz) {
            Some(end) if end <= size => ranges.push((offset, end)),
            _ => return Ok(None),
        }
    }
    Ok(Some(ranges))
}

/// A buffer as long as the furthest range, holding the file's bytes in
/// every range and zeros elsewhere.
fn read_ranges(f: &mut vfs::OpenFile, ranges: &[(u64, u64)]) -> Result<Vec<u8>, Errno> {
    let len = ranges.iter().map(|r| r.1).max().unwrap_or(0);
    let mut image = alloc::vec![0u8; len as usize];
    for &(start, end) in ranges {
        if read_at(f, start, &mut image[start as usize..end as usize])? != (end - start) as usize {
            // The file shrank meanwhile: the parser sees it as truncated.
            image.truncate(start as usize);
            break;
        }
    }
    Ok(image)
}

/// The 16 `AT_RANDOM` bytes. Without an entropy source they are derived
/// from the time stamp counter: unpredictable enough for nothing but
/// distinguishing processes, which is all a program can expect then.
pub fn random_bytes() -> [u8; osyx_abi::startup::RANDOM_LEN] {
    let mut r = [0u8; osyx_abi::startup::RANDOM_LEN];
    if crate::random::fill(&mut r).is_err() {
        let mut h = osyx_crypto::Sha256::new();
        // SAFETY: RDTSC is available on every x86-64 CPU.
        h.update(&unsafe { core::arch::x86_64::_rdtsc() }.to_le_bytes());
        h.update(&crate::interrupts::ticks().to_le_bytes());
        r.copy_from_slice(&h.finish()[..osyx_abi::startup::RANDOM_LEN]);
    }
    r
}
