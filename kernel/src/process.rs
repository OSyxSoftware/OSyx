//! User processes: creation from boot modules, exit, zombies and `wait`.
//!
//! A process owns an address space and (for now) exactly one thread, its
//! main thread, scheduled by `sched`. On exit the address space is destroyed
//! at once; the process stays in the table as a zombie holding its exit
//! status until the parent collects it with [`wait`]. Children of an exiting
//! process are re-parented to the kernel (pid 0) and released automatically
//! when they exit.
//!
//! Every frame a process causes to be allocated (page tables included) is
//! counted, and teardown checks that exactly that many frames come back.
//!
//! User address space layout:
//! * `0x0000_0000_0000_0000..0x0000_0000_0001_0000` never mapped (null guard)
//! * ELF segments at their link addresses (`osyx_elf::USER_RANGE`)
//! * anonymous memory from `MAP_BASE` upwards, a guard page after each region
//! * stack: `STACK_PAGES` pages below `STACK_TOP`, guard page below that

use crate::arch::cpu;
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{alloc_frame, free_frame, phys_to_virt, PAGE_SIZE};
use crate::sched::{self, Tid};
use crate::sync::IrqSpinLock;
use crate::{kprintln, modules};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_abi::Errno;
use osyx_elf::{ElfError, ElfImage};

pub type Pid = u64;
/// Parent id of processes started (or adopted) by the kernel.
pub const KERNEL_PID: Pid = 0;

pub const STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;
const STACK_PAGES: u64 = 64; // 256 KiB
const MAP_BASE: u64 = 0x0000_1000_0000_0000;
/// Per-process cap on anonymous memory (256 MiB).
const MAP_LIMIT_PAGES: u64 = 64 * 1024;
const MAX_ARG_BYTES: usize = 16 * 1024;
const MAX_PROCESSES: usize = 64;
const OUTPUT_TAIL_LINES: usize = 3;

#[derive(Debug)]
pub enum SpawnError {
    NotFound,
    BadImage(ElfError),
    Map(MapError),
    ArgsTooLarge,
    TooManyProcesses,
}

impl core::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpawnError::NotFound => write!(f, "no such program"),
            SpawnError::BadImage(e) => write!(f, "invalid executable: {}", e.describe()),
            SpawnError::Map(e) => write!(f, "cannot map memory: {e:?}"),
            SpawnError::ArgsTooLarge => write!(f, "arguments exceed {MAX_ARG_BYTES} bytes"),
            SpawnError::TooManyProcesses => write!(f, "process limit of {MAX_PROCESSES} reached"),
        }
    }
}

impl SpawnError {
    pub fn errno(&self) -> Errno {
        match self {
            SpawnError::NotFound => Errno::NotFound,
            SpawnError::BadImage(_) | SpawnError::ArgsTooLarge => Errno::InvalidArgument,
            SpawnError::Map(_) | SpawnError::TooManyProcesses => Errno::NoMemory,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Life {
    Running,
    Zombie(i32),
}

pub struct Process {
    pub pid: Pid,
    pub name: String,
    parent: Pid,
    /// Set when the parent exited: released immediately on exit.
    orphan: bool,
    life: Life,
    root: u64,
    regions: BTreeMap<u64, u64>,
    next_map: u64,
    mapped_pages: u64,
    /// Frames this process currently owns (root, page tables, pages).
    frames: u64,
    line: Vec<u8>,
}

struct Table {
    procs: BTreeMap<Pid, Process>,
    /// Threads blocked in `wait`, keyed by the parent they wait as.
    waiters: Vec<(Pid, Tid)>,
}

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
static TABLE: IrqSpinLock<Table> = IrqSpinLock::new(Table { procs: BTreeMap::new(), waiters: Vec::new() });
static OUTPUT_TAIL: IrqSpinLock<VecDeque<String>> = IrqSpinLock::new(VecDeque::new());

global_asm!(
    r#"
    .section .text
    // ! osyx_iret_to_user(entry: u64, user_rsp: u64)
    .global osyx_iret_to_user
osyx_iret_to_user:
    cli
    push 0x1B           // SS  = USER_DS
    push rsi            // RSP
    push 0x202          // RFLAGS: IF set
    push 0x23           // CS  = USER_CS
    push rdi            // RIP
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor edi, edi
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    iretq
    "#
);

extern "C" {
    fn osyx_iret_to_user(entry: u64, user_rsp: u64) -> !;
}

/// Main-thread entry of every process: drop to ring 3. The scheduler has
/// already loaded the process's CR3 and kernel stack.
fn user_main_thread(entry: u64, user_rsp: u64) {
    // SAFETY: entry and stack were set up by `Process::populate` in the
    // address space that is active for this thread.
    unsafe { osyx_iret_to_user(entry, user_rsp) }
}

impl Process {
    fn new(parent: Pid, name: &str) -> Result<Process, SpawnError> {
        let root = paging::new_address_space().map_err(SpawnError::Map)?;
        Ok(Process {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            name: name.to_string(),
            parent,
            orphan: false,
            life: Life::Running,
            root,
            regions: BTreeMap::new(),
            next_map: MAP_BASE,
            mapped_pages: 0,
            frames: 1,
            line: Vec::new(),
        })
    }

    fn map_zeroed(&mut self, va: u64, flags: Flags) -> Result<u64, MapError> {
        let frame = alloc_frame().ok_or(MapError::OutOfMemory)?;
        match paging::map_in(self.root, va, frame, flags) {
            Ok(tables) => {
                self.frames += 1 + tables;
                Ok(frame)
            }
            Err(e) => {
                let _ = free_frame(frame);
                Err(e)
            }
        }
    }

    /// Maps the ELF segments and the stack; returns (entry, initial rsp).
    fn populate(&mut self, image: &[u8], args: &[&str]) -> Result<(u64, u64), SpawnError> {
        let elf = ElfImage::parse_user(image).map_err(SpawnError::BadImage)?;
        for seg in elf.segments() {
            let file = elf.file_bytes(seg);
            let flags = Flags::user(seg.writable(), seg.executable());
            let mut va = seg.vaddr;
            while va < seg.page_end() {
                let frame = self.map_zeroed(va, flags).map_err(SpawnError::Map)?;
                // Copy the part of the file image that falls into this page.
                let start = va.max(seg.vaddr);
                let end = (va + PAGE_SIZE).min(seg.vaddr + file.len() as u64);
                if end > start {
                    let src = &file[(start - seg.vaddr) as usize..(end - seg.vaddr) as usize];
                    // SAFETY: the frame was just allocated and is reachable via the HHDM.
                    unsafe {
                        let dst = (phys_to_virt(frame) + (start - va)) as *mut u8;
                        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                    }
                }
                va += PAGE_SIZE;
            }
        }
        for i in 0..STACK_PAGES {
            self.map_zeroed(STACK_TOP - (i + 1) * PAGE_SIZE, Flags::user(true, false)).map_err(SpawnError::Map)?;
        }
        Ok((elf.entry(), self.build_stack(args)?))
    }

    /// Writes `bytes` at user address `va` of this (not active) address space.
    fn poke(&self, mut va: u64, mut bytes: &[u8]) -> Result<(), SpawnError> {
        while !bytes.is_empty() {
            let t = paging::translate_in(self.root, va).ok_or(SpawnError::Map(MapError::NotMapped))?;
            let room = (PAGE_SIZE - (va % PAGE_SIZE)) as usize;
            let n = room.min(bytes.len());
            // SAFETY: `t.phys` is a frame of this address space, reachable via the HHDM.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), phys_to_virt(t.phys) as *mut u8, n) };
            va += n as u64;
            bytes = &bytes[n..];
        }
        Ok(())
    }

    /// System V initial stack: argc, argv[], NULL, envp NULL, AT_NULL auxv.
    fn build_stack(&self, args: &[&str]) -> Result<u64, SpawnError> {
        if args.iter().map(|a| a.len() + 1).sum::<usize>() > MAX_ARG_BYTES {
            return Err(SpawnError::ArgsTooLarge);
        }
        let mut sp = STACK_TOP;
        let mut ptrs = Vec::with_capacity(args.len());
        for a in args {
            sp -= a.len() as u64 + 1;
            self.poke(sp, a.as_bytes())?;
            self.poke(sp + a.len() as u64, &[0])?;
            ptrs.push(sp);
        }
        let mut words: Vec<u64> = Vec::with_capacity(args.len() + 5);
        words.push(args.len() as u64);
        words.extend_from_slice(&ptrs);
        words.extend_from_slice(&[0, 0, 0, 0]); // argv NULL, envp NULL, AT_NULL pair
        sp = (sp - words.len() as u64 * 8) & !15;
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.poke(sp, &bytes)?;
        Ok(sp)
    }

    /// Releases the address space and checks the frame accounting.
    fn release_memory(&mut self) {
        // SAFETY: the address space is not active (the caller switched to the
        // kernel root or never activated it) and its frames belong to us.
        let freed = unsafe { paging::destroy_address_space(self.root) };
        if freed == self.frames {
            kprintln!("process {} ({}): released {} frames, no leak", self.pid, self.name, freed);
        } else {
            kprintln!("process {} ({}): FRAME LEAK: owned {}, released {}", self.pid, self.name, self.frames, freed);
        }
        self.frames = 0;
        self.root = 0;
    }

    fn flush_line(&mut self) {
        let line = String::from_utf8_lossy(&self.line).into_owned();
        kprintln!("[{}:{}] {}", self.name, self.pid, line);
        let mut tail = OUTPUT_TAIL.lock();
        if tail.len() == OUTPUT_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
        self.line.clear();
    }
}

/// Starts boot module `name` as a child of `parent`. `args[0]` should be the
/// program name. Returns the new pid; the process runs concurrently.
pub fn spawn(parent: Pid, name: &str, args: &[&str]) -> Result<Pid, SpawnError> {
    let image = modules::get(name).ok_or(SpawnError::NotFound)?;
    if TABLE.lock().procs.len() >= MAX_PROCESSES {
        return Err(SpawnError::TooManyProcesses);
    }
    let mut p = Process::new(parent, name)?;
    let (entry, rsp) = match p.populate(image, args) {
        Ok(v) => v,
        Err(e) => {
            p.release_memory();
            return Err(e);
        }
    };
    let (pid, root) = (p.pid, p.root);
    kprintln!("process {} ({}): spawned by {}, entry {:#x}, args {:?}", pid, name, parent, entry, args);
    TABLE.lock().procs.insert(pid, p);
    sched::spawn(name, pid, root, user_main_thread, entry, rsp);
    Ok(pid)
}

/// Waits for child `target` of `parent` (any child if `target` is 0) to exit,
/// releases it and returns (pid, exit status).
pub fn wait(parent: Pid, target: Pid) -> Result<(Pid, i32), Errno> {
    loop {
        let were_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        let outcome = {
            let mut t = TABLE.lock();
            let mut any = false;
            let mut done = None;
            for (pid, p) in t.procs.iter() {
                if p.parent == parent && !p.orphan && (target == 0 || *pid == target) {
                    any = true;
                    if let Life::Zombie(status) = p.life {
                        done = Some((*pid, status));
                        break;
                    }
                }
            }
            match (any, done) {
                (false, _) => Some(Err(Errno::NoChild)),
                (true, Some((pid, status))) => {
                    t.procs.remove(&pid);
                    Some(Ok((pid, status)))
                }
                (true, None) => {
                    t.waiters.push((parent, sched::current_tid()));
                    sched::mark_current_blocked();
                    None
                }
            }
        };
        if outcome.is_none() {
            sched::schedule();
        }
        if were_enabled {
            cpu::enable_interrupts();
        }
        if let Some(r) = outcome {
            return r;
        }
    }
}

/// Terminates the current process with `status` and never returns. Called
/// from `exit` and when a CPU exception hits user code.
pub fn exit_current(status: i32) -> ! {
    cpu::disable_interrupts();
    let pid = sched::current_pid();
    assert_ne!(pid, KERNEL_PID, "exit_current called by a kernel thread");
    // Leave the process address space before destroying it.
    // SAFETY: the kernel root maps everything the kernel needs.
    unsafe { cpu::write_cr3(paging::kernel_root()) };
    sched::set_current_root(paging::kernel_root());
    let mut to_wake = Vec::new();
    {
        let mut t = TABLE.lock();
        let Table { procs, waiters } = &mut *t;
        let p = procs.get_mut(&pid).expect("current process exists");
        if !p.line.is_empty() {
            p.flush_line();
        }
        p.release_memory();
        p.life = Life::Zombie(status);
        let (parent, orphan) = (p.parent, p.orphan);
        kprintln!("process {} ({}) exited with status {} ({:#x})", pid, p.name, status, status);
        // Children: zombies are released, running ones become kernel orphans.
        procs.retain(|_, c| !(c.parent == pid && c.life != Life::Running));
        for c in procs.values_mut().filter(|c| c.parent == pid) {
            c.parent = KERNEL_PID;
            c.orphan = true;
        }
        if orphan {
            procs.remove(&pid);
        } else {
            waiters.retain(|&(ppid, tid)| {
                if ppid == parent {
                    to_wake.push(tid);
                    false
                } else {
                    true
                }
            });
        }
    }
    for tid in to_wake {
        sched::wake(tid);
    }
    sched::exit_current()
}

/// Last lines written by user processes (for the status screen).
pub fn output_tail() -> Vec<String> {
    OUTPUT_TAIL.lock().iter().cloned().collect()
}

/// Number of processes in the table (running and zombie).
pub fn count() -> usize {
    TABLE.lock().procs.len()
}

/// `write` back end: line-buffered output to the kernel log.
pub fn write_output(bytes: &[u8]) {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let Some(p) = t.procs.get_mut(&pid) else { return };
    for &b in bytes {
        if b == b'\n' {
            p.flush_line();
        } else if p.line.len() < 4096 {
            p.line.push(b);
        }
    }
}

/// `map` back end: zeroed anonymous memory, W^X enforced.
pub fn map_anonymous(len: u64, prot: u64) -> Result<u64, Errno> {
    use osyx_abi::{PROT_EXEC, PROT_READ, PROT_WRITE};
    if len == 0 || prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(Errno::InvalidArgument);
    }
    if prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0 {
        return Err(Errno::PermissionDenied);
    }
    let pages = len.div_ceil(PAGE_SIZE);
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    if p.mapped_pages + pages > MAP_LIMIT_PAGES {
        return Err(Errno::NoMemory);
    }
    let base = p.next_map;
    let flags = Flags::user(prot & PROT_WRITE != 0, prot & PROT_EXEC != 0);
    for i in 0..pages {
        if p.map_zeroed(base + i * PAGE_SIZE, flags).is_err() {
            for j in 0..i {
                if let Ok(f) = paging::unmap_in(p.root, base + j * PAGE_SIZE, true) {
                    let _ = free_frame(f);
                    p.frames -= 1;
                }
            }
            return Err(Errno::NoMemory);
        }
    }
    p.regions.insert(base, pages);
    p.mapped_pages += pages;
    p.next_map = base + (pages + 1) * PAGE_SIZE; // leave a guard page
    Ok(base)
}

/// `unmap` back end: releases exactly one region previously returned by `map`.
pub fn unmap_region(addr: u64, len: u64) -> Result<(), Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    let pages = *p.regions.get(&addr).ok_or(Errno::InvalidArgument)?;
    if len.div_ceil(PAGE_SIZE) != pages {
        return Err(Errno::InvalidArgument);
    }
    for i in 0..pages {
        let frame = paging::unmap_in(p.root, addr + i * PAGE_SIZE, true).map_err(|_| Errno::Fault)?;
        let _ = free_frame(frame);
        p.frames -= 1;
    }
    p.regions.remove(&addr);
    p.mapped_pages -= pages;
    Ok(())
}
