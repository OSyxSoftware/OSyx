//! User processes: creation from executable files, `execve`, exit, zombies
//! and `wait`.
//!
//! A process owns an address space and one or more threads scheduled by
//! `sched`. On exit the address space is destroyed at once; the process
//! stays in the table as a zombie holding its exit status until the parent
//! collects it with [`wait`]. Children of an exiting process are re-parented
//! to the kernel (pid 0) and released automatically when they exit.
//!
//! Programs are static ELF64 executables read from the VFS (see `exec`).
//! `execve` builds the new image in a fresh address space and only then
//! swaps it in, so a failure leaves the caller untouched.
//!
//! Every frame a process causes to be allocated (page tables included) is
//! counted, and teardown checks that exactly that many frames come back.
//!
//! User address space layout:
//! * `0x0000_0000_0000_0000..0x0000_0000_0001_0000` never mapped (null guard)
//! * ELF segments at their link addresses, below `MAP_BASE`
//! * anonymous memory from `MAP_BASE` upwards, a guard page after each region
//! * stack: `STACK_PAGES` pages below `STACK_TOP`, guard page below that;
//!   at its top the System V start-up data (`osyx_abi::startup`)

use crate::arch::cpu;
use crate::exec::{self, ExecRequest, Program};
use crate::kprintln;
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{alloc_frame, free_frame, phys_to_virt, PAGE_SIZE};
use crate::object::Object;
use crate::sched::{self, Tid};
use crate::shm::SharedMemory;
use crate::sync::{IrqSpinLock, KMutex};
use crate::vfs::{self, Lookup};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_abi::{auxv, startup, Errno};
use osyx_elf::{AddressRange, ElfError, ElfImage};
use osyx_vfs::Cred;

pub type Pid = u64;
/// Parent id of processes started (or adopted) by the kernel.
pub const KERNEL_PID: Pid = 0;

pub const STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;
const STACK_PAGES: u64 = 128; // 512 KiB
/// Fixed mappings must end below the main stack and its guard page.
const STACK_LIMIT: u64 = STACK_TOP - (STACK_PAGES + 1) * PAGE_SIZE;
const MAP_BASE: u64 = 0x0000_1000_0000_0000;
/// Where program segments may lie: above the null guard, below `MAP_BASE`.
const PROGRAM_RANGE: AddressRange = AddressRange { start: osyx_elf::USER_RANGE.start, end: MAP_BASE };
/// Per-process cap on anonymous memory (2 GiB). Mappings are backed by
/// frames at once, so physical memory remains the real bound; the cap
/// keeps one process from taking all of it (a browser's main library
/// alone is about 170 MiB of image).
const MAP_LIMIT_PAGES: u64 = 512 * 1024;
const MAX_PROCESSES: usize = 64;
const OUTPUT_TAIL_LINES: usize = 3;

/// Why a program could not be started or executed.
#[derive(Debug)]
pub enum ExecError {
    /// Finding or reading the program, or the arguments (an `Errno` of `exec`).
    Load(Errno),
    BadImage(ElfError),
    Map(MapError),
    /// The start-up data does not fit on the stack.
    ArgsTooLarge,
    TooManyProcesses,
    /// `execve` in a process with more than one thread.
    Busy,
}

impl core::fmt::Display for ExecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ExecError::Load(e) => write!(f, "{}", e.name()),
            ExecError::BadImage(e) => write!(f, "invalid executable: {}", e.describe()),
            ExecError::Map(e) => write!(f, "cannot map memory: {e:?}"),
            ExecError::ArgsTooLarge => write!(f, "arguments do not fit on the stack"),
            ExecError::TooManyProcesses => write!(f, "process limit of {MAX_PROCESSES} reached"),
            ExecError::Busy => write!(f, "process has other threads"),
        }
    }
}

impl ExecError {
    pub fn errno(&self) -> Errno {
        match self {
            ExecError::Load(e) => *e,
            ExecError::BadImage(_) => Errno::BadExecutable,
            ExecError::ArgsTooLarge => Errno::InvalidArgument,
            ExecError::Map(_) | ExecError::TooManyProcesses => Errno::NoMemory,
            ExecError::Busy => Errno::Busy,
        }
    }
}

/// A page-table root and the number of frames it owns (itself, page
/// tables and mapped pages).
struct Space {
    root: u64,
    frames: u64,
}

impl Space {
    fn new() -> Result<Space, MapError> {
        Ok(Space { root: paging::new_address_space()?, frames: 1 })
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

    /// Maps the ELF segments, the stack and the start-up data; returns
    /// (entry, initial rsp).
    fn populate(
        &mut self,
        elf: &ElfImage,
        prog: &Program,
        req: &ExecRequest,
        cred: Cred,
    ) -> Result<(u64, u64), ExecError> {
        for seg in elf.segments() {
            let file = elf.file_bytes(seg);
            let flags = Flags::user(seg.writable(), seg.executable());
            let mut va = seg.vaddr;
            while va < seg.page_end() {
                let frame = self.map_zeroed(va, flags).map_err(ExecError::Map)?;
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
            self.map_zeroed(STACK_TOP - (i + 1) * PAGE_SIZE, Flags::user(true, false)).map_err(ExecError::Map)?;
        }
        let (uid, gid) = (cred.uid as u64, cred.gid as u64);
        let mut aux = Vec::with_capacity(11);
        if let Some(phdr) = elf.phdr_vaddr() {
            aux.push((auxv::AT_PHDR, phdr));
        }
        aux.extend_from_slice(&[
            (auxv::AT_PHENT, 56),
            (auxv::AT_PHNUM, elf.phnum() as u64),
            (auxv::AT_PAGESZ, PAGE_SIZE),
            (auxv::AT_ENTRY, elf.entry()),
            (auxv::AT_UID, uid),
            (auxv::AT_EUID, uid),
            (auxv::AT_GID, gid),
            (auxv::AT_EGID, gid),
            (auxv::AT_SECURE, 0),
        ]);
        let argv: Vec<&[u8]> = req.argv.iter().map(|s| s.as_bytes()).collect();
        let envp: Vec<&[u8]> = req.envp.iter().map(|s| s.as_bytes()).collect();
        let image = startup::Image {
            argv: &argv,
            envp: &envp,
            aux: &aux,
            random: exec::random_bytes(),
            execfn: prog.path.as_bytes(),
        };
        // Keep at least a quarter of the stack for the program itself.
        let size = image.size();
        if size as u64 > STACK_PAGES * PAGE_SIZE * 3 / 4 {
            return Err(ExecError::ArgsTooLarge);
        }
        let mut bytes = alloc::vec![0u8; size];
        let sp = image.build(STACK_TOP, &mut bytes).ok_or(ExecError::ArgsTooLarge)?;
        self.poke(sp, &bytes).map_err(ExecError::Map)?;
        Ok((elf.entry(), sp))
    }

    /// Writes `bytes` at user address `va` of this (not active) address space.
    fn poke(&self, mut va: u64, mut bytes: &[u8]) -> Result<(), MapError> {
        while !bytes.is_empty() {
            let t = paging::translate_in(self.root, va).ok_or(MapError::NotMapped)?;
            let room = (PAGE_SIZE - (va % PAGE_SIZE)) as usize;
            let n = room.min(bytes.len());
            // SAFETY: `t.phys` is a frame of this address space, reachable via the HHDM.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), phys_to_virt(t.phys) as *mut u8, n) };
            va += n as u64;
            bytes = &bytes[n..];
        }
        Ok(())
    }

    /// Destroys the address space; returns (frames owned, frames freed).
    ///
    /// # Safety
    /// The space must not be active on any CPU.
    unsafe fn release(&mut self) -> (u64, u64) {
        // SAFETY: forwarded caller contract; the frames belong to us.
        let freed = unsafe { paging::destroy_address_space(self.root) };
        let owned = self.frames;
        self.frames = 0;
        self.root = 0;
        (owned, freed)
    }
}

/// Reads, validates and maps `req`'s program into a new address space as
/// `cx` (whose credentials and current directory apply); returns the space,
/// the entry point, the initial rsp and the canonical program path.
fn build_image(cx: &Lookup, req: &ExecRequest) -> Result<(Space, u64, u64, String), ExecError> {
    let prog = exec::load(cx, &req.path).map_err(ExecError::Load)?;
    let elf = ElfImage::parse_with(&prog.image, PROGRAM_RANGE).map_err(|e| {
        kprintln!("exec {}: rejected: {}", prog.path, e.describe());
        ExecError::BadImage(e)
    })?;
    let mut space = Space::new().map_err(ExecError::Map)?;
    match space.populate(&elf, &prog, req, cx.cred) {
        Ok((entry, sp)) => Ok((space, entry, sp, prog.path)),
        Err(e) => {
            // SAFETY: the space was never activated.
            unsafe { space.release() };
            Err(e)
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
    space: Space,
    regions: BTreeMap<u64, u64>,
    /// Shared memory mapped into this process: address -> (object, pages).
    /// The frames belong to the object; they are unmapped (never freed)
    /// before the address space is destroyed.
    shared: BTreeMap<u64, (Arc<SharedMemory>, u64)>,
    next_map: u64,
    mapped_pages: u64,
    line: Vec<u8>,
    /// Live threads; the process is finalised when the last one ends.
    threads: Vec<Tid>,
    exited_cpu_ticks: u64,
    /// Set by `exit`: the status reported once the last thread has ended.
    exit_status: Option<i32>,
    /// Open objects by handle number: 0-2 only if standard streams were
    /// redirected at spawn, the rest from `FIRST_FILE_HANDLE`.
    handles: BTreeMap<u64, Object>,
    /// User and group for permission checks (inherited by children).
    cred: Cred,
    /// Canonical absolute current directory (inherited by children).
    cwd: String,
}

/// An open file shared by the handle table and in-flight system calls.
pub type FileHandle = Arc<KMutex<vfs::OpenFile>>;

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
    // ! osyx_iret_to_user(entry: u64, user_rsp: u64, arg: u64)
    // Enters ring 3 at `entry` with rsp = user_rsp, rdi = arg and every
    // other general purpose register zero.
    .global osyx_iret_to_user
osyx_iret_to_user:
    cli
    push 0x1B           // SS  = USER_DS
    push rsi            // RSP
    push 0x202          // RFLAGS: IF set
    push 0x23           // CS  = USER_CS
    push rdi            // RIP
    mov rdi, rdx
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
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
    fn osyx_iret_to_user(entry: u64, user_rsp: u64, arg: u64) -> !;
}

/// Where a user thread starts in ring 3.
struct UserStart {
    rip: u64,
    rsp: u64,
    arg: u64,
}

/// Kernel-side entry of every user thread: drop to ring 3. The scheduler has
/// already loaded the process's CR3, kernel stack and FS base.
fn user_thread_entry(start: u64, _: u64) {
    // SAFETY: `start` is a `Box<UserStart>` leaked by `start_thread`.
    let s = unsafe { alloc::boxed::Box::from_raw(start as *mut UserStart) };
    let (rip, rsp, arg) = (s.rip, s.rsp, s.arg);
    drop(s);
    // SAFETY: the entry and stack lie in the user half of the address space
    // that is active for this thread; a bad value only faults in ring 3.
    unsafe { osyx_iret_to_user(rip, rsp, arg) }
}

/// Creates a thread of process `p` (caller holds no locks and has
/// interrupts disabled, so the tid is recorded before the thread can run).
fn start_thread(p: &mut Process, rip: u64, rsp: u64, arg: u64, fs_base: u64) -> Tid {
    let start = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(UserStart { rip, rsp, arg })) as u64;
    let tid = sched::spawn(&p.name, p.pid, p.space.root, user_thread_entry, start, 0, fs_base);
    p.threads.push(tid);
    tid
}

/// Logs the frame accounting of a released address space.
fn report_release(pid: Pid, name: &str, what: &str, (owned, freed): (u64, u64)) {
    if freed == owned {
        kprintln!("process {} ({}): {}released {} frames, no leak", pid, name, what, freed);
    } else {
        kprintln!("process {} ({}): {}FRAME LEAK: owned {}, released {}", pid, name, what, owned, freed);
    }
}

impl Process {
    fn new(parent: Pid, name: &str, cred: Cred, cwd: String, space: Space) -> Process {
        Process {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            name: name.to_string(),
            parent,
            orphan: false,
            life: Life::Running,
            space,
            regions: BTreeMap::new(),
            shared: BTreeMap::new(),
            next_map: MAP_BASE,
            mapped_pages: 0,
            line: Vec::new(),
            threads: Vec::new(),
            exited_cpu_ticks: 0,
            exit_status: None,
            handles: BTreeMap::new(),
            cred,
            cwd,
        }
    }

    /// Removes every shared memory mapping (the frames stay with their
    /// objects; page tables stay with the address space).
    fn unmap_shared(&mut self) {
        unmap_shared_from(self.space.root, core::mem::take(&mut self.shared));
    }

    /// Releases the address space and checks the frame accounting.
    fn release_memory(&mut self) {
        self.unmap_shared();
        // SAFETY: the address space is not active (the caller switched to the
        // kernel root or never activated it).
        let r = unsafe { self.space.release() };
        report_release(self.pid, &self.name, "", r);
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

fn unmap_shared_from(root: u64, shared: BTreeMap<u64, (Arc<SharedMemory>, u64)>) {
    for (addr, (shm, pages)) in shared {
        for i in 0..pages {
            let _ = paging::unmap_in(root, addr + i * PAGE_SIZE, true);
        }
        drop(shm);
    }
}

/// Starts the program of `req` as a child of `parent`, with the parent's
/// credentials and current directory (root and `/` for the kernel).
/// Returns the new pid; the process runs concurrently.
pub fn spawn(parent: Pid, req: &ExecRequest) -> Result<Pid, ExecError> {
    spawn_with_stdio(parent, req, [None, None, None])
}

/// [`spawn`] with the child's handles 0, 1 and 2 set to `stdio`.
pub fn spawn_with_stdio(parent: Pid, req: &ExecRequest, stdio: [Option<Object>; 3]) -> Result<Pid, ExecError> {
    if TABLE.lock().procs.len() >= MAX_PROCESSES {
        return Err(ExecError::TooManyProcesses);
    }
    let cx = {
        let t = TABLE.lock();
        match t.procs.get(&parent) {
            Some(p) => Lookup { cred: p.cred, cwd: p.cwd.clone(), case_insensitive: false },
            None => Lookup::kernel(),
        }
    };
    let (space, entry, rsp, path) = build_image(&cx, req)?;
    let mut p = Process::new(parent, req.name(), cx.cred, cx.cwd, space);
    for (i, o) in stdio.into_iter().enumerate() {
        if let Some(o) = o {
            p.handles.insert(i as u64, o);
        }
    }
    let pid = p.pid;
    const LOGGED_ARGS: usize = 8;
    let more = match req.argv.len() {
        n if n > LOGGED_ARGS => alloc::format!(" and {} more", n - LOGGED_ARGS),
        _ => String::new(),
    };
    let shown = &req.argv[..req.argv.len().min(LOGGED_ARGS)];
    kprintln!(
        "process {} ({}): {} spawned by {}, entry {:#x}, args {:?}{}",
        pid,
        p.name,
        path,
        parent,
        entry,
        shown,
        more
    );
    // With interrupts off the main thread cannot run (and exit) before the
    // process and its thread list are in the table.
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    start_thread(&mut p, entry, rsp, 0, 0);
    TABLE.lock().procs.insert(pid, p);
    if were_enabled {
        cpu::enable_interrupts();
    }
    Ok(pid)
}

/// `thread_spawn` back end: a new thread in the current process.
pub fn spawn_thread(entry: u64, stack_top: u64, arg: u64, fs_base: u64) -> Result<Tid, Errno> {
    let user = paging::USER_START..osyx_abi::USER_END;
    if !user.contains(&entry) || !user.contains(&(stack_top.wrapping_sub(16))) || !stack_top.is_multiple_of(16) {
        return Err(Errno::InvalidArgument);
    }
    if fs_base != 0 && !user.contains(&fs_base) {
        return Err(Errno::InvalidArgument);
    }
    let pid = sched::current_pid();
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    let r = {
        let mut t = TABLE.lock();
        match t.procs.get_mut(&pid) {
            Some(p) if p.exit_status.is_some() => Err(Errno::Interrupted),
            Some(p) if p.threads.len() >= osyx_abi::MAX_THREADS => Err(Errno::NoMemory),
            Some(p) => Ok(start_thread(p, entry, stack_top, arg, fs_base)),
            None => Err(Errno::NotSupported),
        }
    };
    if were_enabled {
        cpu::enable_interrupts();
    }
    r
}

/// `execve` back end: replaces the current process's program. Returns only
/// on failure, with the old program untouched. On success the old address
/// space is released and the thread drops to ring 3 at the new entry point;
/// the kernel stack of this system call is abandoned, so the caller must
/// not own anything that needs dropping (`req` is consumed here).
pub fn exec_current(req: ExecRequest) -> ExecError {
    let pid = sched::current_pid();
    let one_thread = |t: &Table| t.procs.get(&pid).is_some_and(|p| p.threads.len() == 1);
    if !one_thread(&TABLE.lock()) {
        return ExecError::Busy;
    }
    let cx = Lookup::current();
    let (mut space, entry, rsp, path) = match build_image(&cx, &req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let name = String::from(req.name());
    drop(req);
    cpu::disable_interrupts();
    let old = {
        let mut t = TABLE.lock();
        if !one_thread(&t) {
            None
        } else {
            let p = t.procs.get_mut(&pid).expect("current process exists");
            core::mem::swap(&mut p.space, &mut space);
            p.regions.clear();
            let old_shared = core::mem::take(&mut p.shared);
            p.next_map = MAP_BASE;
            p.mapped_pages = 0;
            p.name = name.clone();
            kprintln!("process {} ({}): exec {}, entry {:#x}", pid, name, path, entry);
            Some((p.space.root, old_shared))
        }
    };
    let Some((root, old_shared)) = old else {
        cpu::enable_interrupts();
        // SAFETY: the new space was never activated.
        unsafe { space.release() };
        return ExecError::Busy;
    };
    // `space` now holds the old image: leave it, then destroy it.
    // SAFETY: the new root maps the kernel half like every process root.
    unsafe { cpu::write_cr3(root) };
    sched::set_current_root(root);
    sched::rename_current(&name);
    sched::set_current_fs_base(0);
    sched::set_current_gs_base(0);
    sched::reset_current_fpu();
    crate::user_fault::clear_process(pid);
    unmap_shared_from(space.root, old_shared);
    // SAFETY: the old space is no longer active anywhere.
    let r = unsafe { space.release() };
    report_release(pid, &name, "old image ", r);
    drop((name, path));
    // SAFETY: entry and stack were mapped into the now active address space.
    unsafe { osyx_iret_to_user(entry, rsp, 0) }
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
                (true, None) if sched::current_kill_pending() => Some(Err(Errno::Interrupted)),
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
/// from `exit` and when a CPU exception hits user code. Other threads of the
/// process are asked to end at their next safe point; the last thread to end
/// tears the process down.
pub fn exit_current(status: i32) -> ! {
    cpu::disable_interrupts();
    let pid = sched::current_pid();
    assert_ne!(pid, KERNEL_PID, "exit_current called by a kernel thread");
    let me = sched::current_tid();
    let others: Vec<Tid> = {
        let mut t = TABLE.lock();
        let p = t.procs.get_mut(&pid).expect("current process exists");
        p.exit_status.get_or_insert(status);
        p.threads.iter().copied().filter(|&t| t != me).collect()
    };
    for t in others {
        sched::request_kill(t);
    }
    finish_current_thread(status)
}

/// `thread_exit` back end: publishes `done_word` (store 1 + futex wake, now
/// that the thread no longer runs on its user stack) and ends the thread.
pub fn thread_exit(code: i32, done_word: u64) -> ! {
    if done_word != 0 && crate::fixup::copy_to_user(done_word, &1u32.to_le_bytes()).is_ok() {
        let _ = crate::futex::wake(done_word, u32::MAX);
    }
    finish_current_thread(code)
}

/// Ends the current user thread. The last thread of a process finalises it:
/// address space released, zombie with the exit status, parent woken.
pub fn finish_current_thread(code: i32) -> ! {
    cpu::disable_interrupts();
    let pid = sched::current_pid();
    let me = sched::current_tid();
    crate::user_fault::clear_thread(pid,me);
    let last = {
        let mut t = TABLE.lock();
        let p = t.procs.get_mut(&pid).expect("current process exists");
        p.exited_cpu_ticks = p.exited_cpu_ticks.saturating_add(sched::cpu_ticks(me));
        p.threads.retain(|&t| t != me);
        p.threads.is_empty()
    };
    if !last {
        sched::exit_current();
    }
    crate::user_fault::clear_process(pid);
    // Leave the process address space before destroying it.
    // SAFETY: the kernel root maps everything the kernel needs.
    unsafe { cpu::write_cr3(paging::kernel_root()) };
    sched::set_current_root(paging::kernel_root());
    crate::futex::forget_process(pid);
    let mut to_wake = Vec::new();
    let closed;
    {
        let mut t = TABLE.lock();
        let Table { procs, waiters } = &mut *t;
        let p = procs.get_mut(&pid).expect("current process exists");
        let status = *p.exit_status.get_or_insert(code);
        if !p.line.is_empty() {
            p.flush_line();
        }
        p.release_memory();
        closed = core::mem::take(&mut p.handles);
        if !closed.is_empty() {
            kprintln!("process {} ({}): closed {} open handles", pid, p.name, closed.len());
        }
        p.life = Life::Zombie(status);
        let (parent, orphan) = (p.parent, p.orphan);
        // Threads of this process that were blocked in `wait` are gone.
        waiters.retain(|&(ppid, _)| ppid != pid);
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
    // Volume metadata is written through, so dropping the files needs no I/O.
    // Pipe and channel ends wake their peers as they go.
    drop(closed);
    crate::display::owner_exited(pid);
    for tid in to_wake {
        sched::wake(tid);
    }
    sched::exit_current()
}

/// Adds an open file to the current process; returns the lowest free handle.
pub fn add_handle(f: vfs::OpenFile) -> Result<u64, Errno> {
    add_object(Object::File(Arc::new(KMutex::new(f))))
}

/// Adds objects to the current process: all of them or none; returns their
/// handles (lowest free numbers from `FIRST_FILE_HANDLE`).
pub fn add_objects(objects: Vec<Object>) -> Result<Vec<u64>, Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    if p.handles.len() + objects.len() > osyx_abi::MAX_HANDLES {
        return Err(Errno::TooManyHandles);
    }
    let mut out = Vec::with_capacity(objects.len());
    let mut h = osyx_abi::FIRST_FILE_HANDLE;
    for o in objects {
        while p.handles.contains_key(&h) {
            h += 1;
        }
        p.handles.insert(h, o);
        out.push(h);
    }
    Ok(out)
}

pub fn add_object(o: Object) -> Result<u64, Errno> {
    add_objects(alloc::vec![o]).map(|v| v[0])
}

/// The open file behind handle `h` (`BadHandle` for other objects).
pub fn get_handle(h: u64) -> Result<FileHandle, Errno> {
    match get_object(h)? {
        Object::File(f) => Ok(f),
        _ => Err(Errno::BadHandle),
    }
}

/// The object behind handle `h` of the current process.
pub fn get_object(h: u64) -> Result<Object, Errno> {
    let pid = sched::current_pid();
    let t = TABLE.lock();
    t.procs.get(&pid).and_then(|p| p.handles.get(&h)).cloned().ok_or(Errno::BadHandle)
}

/// Maps all of `shm` into the current process (read-only unless
/// `writable`); returns the address.
pub fn map_shared(shm: Arc<SharedMemory>, writable: bool) -> Result<u64, Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    let pages = shm.pages();
    if p.mapped_pages + pages > MAP_LIMIT_PAGES {
        return Err(Errno::NoMemory);
    }
    let base = p.next_map;
    let mut flags = Flags::user(writable, false);
    flags.write_combining = shm.write_combining;
    for (i, &frame) in shm.frames().iter().enumerate() {
        match paging::map_in(p.space.root, base + i as u64 * PAGE_SIZE, frame, flags) {
            Ok(tables) => p.space.frames += tables,
            Err(_) => {
                for j in 0..i as u64 {
                    let _ = paging::unmap_in(p.space.root, base + j * PAGE_SIZE, true);
                }
                return Err(Errno::NoMemory);
            }
        }
    }
    p.shared.insert(base, (shm, pages));
    p.mapped_pages += pages;
    p.next_map = base + (pages + 1) * PAGE_SIZE; // leave a guard page
    Ok(base)
}

/// Ends process `pid` on behalf of the current process (root, or the same
/// user). Its threads stop at their next safe point.
pub fn kill(pid: Pid) -> Result<(), Errno> {
    let me = sched::current_pid();
    let (my_cred, _) = current_identity();
    let threads: Vec<Tid> = {
        let mut t = TABLE.lock();
        let p = t.procs.get_mut(&pid).ok_or(Errno::NotFound)?;
        if !my_cred.is_root() && p.cred.uid != my_cred.uid {
            return Err(Errno::PermissionDenied);
        }
        if p.life != Life::Running {
            return Err(Errno::NotFound);
        }
        p.exit_status.get_or_insert(osyx_abi::KILLED_STATUS);
        p.threads.clone()
    };
    kprintln!("process {} killed by {}", pid, me);
    for t in threads {
        sched::request_kill(t);
    }
    Ok(())
}

/// One record per process (for `proc_list`).
pub fn list() -> Vec<osyx_abi::ipc::ProcInfo> {
    let t = TABLE.lock();
    t.procs
        .values()
        .map(|p| {
            let mut info = osyx_abi::ipc::ProcInfo {
                pid: p.pid,
                parent: p.parent,
                uid: p.cred.uid,
                gid: p.cred.gid,
                threads: p.threads.len() as u32,
                state: if p.life == Life::Running { osyx_abi::ipc::PROC_RUNNING } else { osyx_abi::ipc::PROC_ZOMBIE },
                frames: p.space.frames,
                name: [0; 32],
                cpu_ticks: p.exited_cpu_ticks.saturating_add(p.threads.iter().map(|&tid| sched::cpu_ticks(tid)).sum()),
            };
            osyx_abi::ipc::fill_field(&mut info.name, &p.name);
            info
        })
        .collect()
}

/// Removes a handle. A system call still using the file keeps it alive
/// until that call returns.
pub fn close_handle(h: u64) -> Result<(), Errno> {
    let pid = sched::current_pid();
    let removed = TABLE.lock().procs.get_mut(&pid).and_then(|p| p.handles.remove(&h));
    removed.map(drop).ok_or(Errno::BadHandle)
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

/// Page flags for `prot`, W^X enforced.
fn prot_flags(prot: u64) -> Result<Flags, Errno> {
    use osyx_abi::{PROT_EXEC, PROT_READ, PROT_WRITE};
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(Errno::InvalidArgument);
    }
    if prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0 {
        return Err(Errno::PermissionDenied);
    }
    Ok(Flags::user(prot & PROT_WRITE != 0, prot & PROT_EXEC != 0))
}

/// `map` back end: zeroed anonymous memory, W^X enforced.
pub fn map_anonymous(len: u64, prot: u64) -> Result<u64, Errno> {
    map_ex(0, len, prot, 0)
}

/// `map_ex` back end: like `map`, at a fixed address (`MAP_FIXED`) or
/// 64 KiB aligned (`MAP_ALIGN_64K`).
pub fn map_ex(addr: u64, len: u64, prot: u64, mflags: u64) -> Result<u64, Errno> {
    use osyx_abi::{MAP_ALIGN_64K, MAP_FIXED};
    if len == 0 || mflags & !(MAP_FIXED | MAP_ALIGN_64K) != 0 {
        return Err(Errno::InvalidArgument);
    }
    let flags = prot_flags(prot)?;
    let pages = len.div_ceil(PAGE_SIZE);
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    if p.mapped_pages + pages > MAP_LIMIT_PAGES {
        return Err(Errno::NoMemory);
    }
    let base = if mflags & MAP_FIXED != 0 {
        let end = addr.checked_add(pages * PAGE_SIZE).ok_or(Errno::InvalidArgument)?;
        if !addr.is_multiple_of(PAGE_SIZE) || addr < paging::USER_START || end > STACK_LIMIT {
            return Err(Errno::InvalidArgument);
        }
        // Nothing may be mapped there yet (program, stack, other regions).
        if (0..pages).any(|i| paging::translate_in(p.space.root, addr + i * PAGE_SIZE).is_some()) {
            return Err(Errno::AlreadyExists);
        }
        addr
    } else if mflags & MAP_ALIGN_64K != 0 {
        p.next_map.next_multiple_of(0x10000)
    } else {
        p.next_map
    };
    for i in 0..pages {
        if p.space.map_zeroed(base + i * PAGE_SIZE, flags).is_err() {
            for j in 0..i {
                if let Ok(f) = paging::unmap_in(p.space.root, base + j * PAGE_SIZE, true) {
                    let _ = free_frame(f);
                    p.space.frames -= 1;
                }
            }
            return Err(Errno::NoMemory);
        }
    }
    p.regions.insert(base, pages);
    p.mapped_pages += pages;
    // Leave a guard page; a fixed region below the cursor does not move it.
    p.next_map = p.next_map.max(base + (pages + 1) * PAGE_SIZE);
    Ok(base)
}

/// `protect` back end: new protection for whole pages that all lie in one
/// anonymous region (W^X enforced; shared memory and program images keep
/// theirs).
pub fn protect(addr: u64, len: u64, prot: u64) -> Result<(), Errno> {
    let flags = prot_flags(prot)?;
    if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::InvalidArgument);
    }
    let pages = len.div_ceil(PAGE_SIZE);
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    let (&start, &n) = p.regions.range(..=addr).next_back().ok_or(Errno::InvalidArgument)?;
    if addr + pages * PAGE_SIZE > start + n * PAGE_SIZE {
        return Err(Errno::InvalidArgument);
    }
    let root = p.space.root;
    for i in 0..pages {
        let va = addr + i * PAGE_SIZE;
        // Re-map the same frame with the new flags (the page tables exist).
        let frame = paging::unmap_in(root, va, true).map_err(|_| Errno::Fault)?;
        paging::map_in(root, va, frame, flags).map_err(|_| Errno::Fault)?;
    }
    Ok(())
}

/// `unmap` back end: releases exactly one region previously returned by
/// `map` or `shm_map`.
pub fn unmap_region(addr: u64, len: u64) -> Result<(), Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    if let Some((_, pages)) = p.shared.get(&addr) {
        if len.div_ceil(PAGE_SIZE) != *pages {
            return Err(Errno::InvalidArgument);
        }
        let pages = *pages;
        let entry = p.shared.remove(&addr).expect("checked above");
        p.mapped_pages -= pages;
        let root = p.space.root;
        let mut one = BTreeMap::new();
        one.insert(addr, entry);
        unmap_shared_from(root, one);
        return Ok(());
    }
    let pages = *p.regions.get(&addr).ok_or(Errno::InvalidArgument)?;
    if len.div_ceil(PAGE_SIZE) != pages {
        return Err(Errno::InvalidArgument);
    }
    for i in 0..pages {
        let frame = paging::unmap_in(p.space.root, addr + i * PAGE_SIZE, true).map_err(|_| Errno::Fault)?;
        let _ = free_frame(frame);
        p.space.frames -= 1;
    }
    p.regions.remove(&addr);
    p.mapped_pages -= pages;
    Ok(())
}

/// Credentials and current directory of the calling process (root and `/`
/// for kernel threads).
pub fn current_identity() -> (Cred, String) {
    let pid = sched::current_pid();
    let t = TABLE.lock();
    t.procs.get(&pid).map_or((Cred::ROOT, String::from("/")), |p| (p.cred, p.cwd.clone()))
}

pub fn set_current_cwd(cwd: String) -> Result<(), Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    p.cwd = cwd;
    Ok(())
}

/// `setuid`: only root may change identity (permanently, for this process
/// and its future children).
pub fn set_current_cred(cred: Cred) -> Result<(), Errno> {
    let pid = sched::current_pid();
    let mut t = TABLE.lock();
    let p = t.procs.get_mut(&pid).ok_or(Errno::NotSupported)?;
    if !p.cred.is_root() {
        return Err(Errno::PermissionDenied);
    }
    p.cred = cred;
    Ok(())
}
