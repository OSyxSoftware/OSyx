//! User processes (single CPU, run to completion).
//!
//! A process owns an address space whose lower half holds its ELF segments,
//! a stack and anonymous `map` regions. [`run`] enters ring 3 with `iretq`
//! after saving the kernel's callee-saved registers in `KERNEL_CTX`; the
//! process leaves through `exit` (or a fatal CPU exception), which restores
//! that context so [`run`] returns the exit status. There is no scheduler yet:
//! timer interrupts are serviced while user code runs, but control returns to
//! the same process.
//!
//! User address space layout:
//! * `0x0000_0000_0000_0000..0x0000_0000_0001_0000` never mapped (null guard)
//! * ELF segments at their link addresses (`osyx_elf::USER_RANGE`)
//! * anonymous memory from `MAP_BASE` upwards, a guard page after each region
//! * stack: `STACK_PAGES` pages below `STACK_TOP`, guard page below that

use crate::arch::{cpu, gdt};
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{alloc_frame, free_frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqSpinLock;
use crate::{kprintln, syscall};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use osyx_abi::Errno;
use osyx_elf::{ElfError, ElfImage};

pub const STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;
const STACK_PAGES: u64 = 64; // 256 KiB
const MAP_BASE: u64 = 0x0000_1000_0000_0000;
/// Per-process cap on anonymous memory (256 MiB).
const MAP_LIMIT_PAGES: u64 = 64 * 1024;
const KERNEL_STACK_SIZE: usize = 64 * 1024;
const MAX_ARG_BYTES: usize = 16 * 1024;
const OUTPUT_TAIL_LINES: usize = 3;

#[derive(Debug)]
pub enum SpawnError {
    BadImage(ElfError),
    Map(MapError),
    ArgsTooLarge,
}

impl core::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpawnError::BadImage(e) => write!(f, "invalid executable: {}", e.describe()),
            SpawnError::Map(e) => write!(f, "cannot map memory: {e:?}"),
            SpawnError::ArgsTooLarge => write!(f, "arguments exceed {MAX_ARG_BYTES} bytes"),
        }
    }
}

pub struct Process {
    pub pid: u64,
    pub name: String,
    root: u64,
    regions: BTreeMap<u64, u64>,
    next_map: u64,
    mapped_pages: u64,
    kernel_stack: Vec<u128>,
    line: Vec<u8>,
}

#[repr(C)]
struct KernelContext {
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rsp: u64,
}

static mut KERNEL_CTX: KernelContext = KernelContext { rbx: 0, rbp: 0, r12: 0, r13: 0, r14: 0, r15: 0, rsp: 0 };
static NEXT_PID: AtomicU64 = AtomicU64::new(1);
static CURRENT: IrqSpinLock<Option<Process>> = IrqSpinLock::new(None);
static OUTPUT_TAIL: IrqSpinLock<VecDeque<String>> = IrqSpinLock::new(VecDeque::new());

global_asm!(
    r#"
    .section .text
    // u32 osyx_enter_user(ctx: *mut KernelContext, entry: u64, user_rsp: u64)
    .global osyx_enter_user
osyx_enter_user:
    mov [rdi + 0], rbx
    mov [rdi + 8], rbp
    mov [rdi + 16], r12
    mov [rdi + 24], r13
    mov [rdi + 32], r14
    mov [rdi + 40], r15
    mov [rdi + 48], rsp
    push 0x1B           // SS  = USER_DS
    push rdx            // RSP
    push 0x202          // RFLAGS: IF set
    push 0x23           // CS  = USER_CS
    push rsi            // RIP
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

    // ! osyx_leave_user(ctx: *const KernelContext, status: i32)
    // Resumes the kernel as if osyx_enter_user returned `status`.
    .global osyx_leave_user
osyx_leave_user:
    mov rbx, [rdi + 0]
    mov rbp, [rdi + 8]
    mov r12, [rdi + 16]
    mov r13, [rdi + 24]
    mov r14, [rdi + 32]
    mov r15, [rdi + 40]
    mov rsp, [rdi + 48]
    mov eax, esi
    ret
    "#
);

extern "C" {
    fn osyx_enter_user(ctx: *mut KernelContext, entry: u64, user_rsp: u64) -> i32;
    fn osyx_leave_user(ctx: *const KernelContext, status: i32) -> !;
}

impl Process {
    fn create(name: &str, image: &[u8], args: &[&str]) -> Result<(Process, u64, u64), SpawnError> {
        let elf = ElfImage::parse_user(image).map_err(SpawnError::BadImage)?;
        let root = paging::new_address_space().map_err(SpawnError::Map)?;
        let mut p = Process {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            name: name.to_string(),
            root,
            regions: BTreeMap::new(),
            next_map: MAP_BASE,
            mapped_pages: 0,
            kernel_stack: alloc::vec![0u128; KERNEL_STACK_SIZE / 16],
            line: Vec::new(),
        };
        match p.populate(&elf, args) {
            Ok(rsp) => Ok((p, elf.entry(), rsp)),
            Err(e) => {
                p.destroy();
                Err(e)
            }
        }
    }

    fn map_zeroed(&mut self, va: u64, flags: Flags) -> Result<u64, MapError> {
        let frame = alloc_frame().ok_or(MapError::OutOfMemory)?;
        if let Err(e) = paging::map_in(self.root, va, frame, flags) {
            let _ = free_frame(frame);
            return Err(e);
        }
        Ok(frame)
    }

    /// Maps the ELF segments and the stack; returns the initial stack pointer.
    fn populate(&mut self, elf: &ElfImage, args: &[&str]) -> Result<u64, SpawnError> {
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
        self.build_stack(args)
    }

    /// Writes `bytes` at user address `va` of this (not yet active) address space.
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

    fn destroy(self) {
        // SAFETY: the address space is not active (run switched back to the
        // kernel root) and all its user frames belong to this process.
        let freed = unsafe { paging::destroy_address_space(self.root) };
        kprintln!("process {} ({}) released {} frames", self.pid, self.name, freed);
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

/// Loads `image` and runs it to completion. Returns its exit status.
pub fn run(name: &str, image: &[u8], args: &[&str]) -> Result<i32, SpawnError> {
    let (free_before, _) = crate::mem::frame_stats();
    let (p, entry, rsp) = Process::create(name, image, args)?;
    let root = p.root;
    let kstack_top = (p.kernel_stack.as_ptr() as u64 + KERNEL_STACK_SIZE as u64) & !15;
    kprintln!("process {} ({}): entry {:#x}, stack {:#x}, args {:?}", p.pid, name, entry, rsp, args);
    *CURRENT.lock() = Some(p);
    // SAFETY: the kernel stack lives in the process (heap memory that does not
    // move while CURRENT owns it); the root maps the kernel half; KERNEL_CTX is
    // only touched here and by `exit_current`, never concurrently (single CPU).
    let status = unsafe {
        gdt::set_kernel_stack(kstack_top);
        syscall::set_kernel_stack(kstack_top);
        cpu::write_cr3(root);
        let s = osyx_enter_user(&raw mut KERNEL_CTX, entry, rsp);
        cpu::write_cr3(paging::kernel_root());
        s
    };
    cpu::enable_interrupts();
    let mut p = CURRENT.lock().take().expect("process vanished");
    if !p.line.is_empty() {
        p.flush_line();
    }
    kprintln!("process {} ({}) exited with status {} ({:#x})", p.pid, p.name, status, status);
    p.destroy();
    let (free_after, _) = crate::mem::frame_stats();
    if free_after == free_before {
        kprintln!("process teardown: no frame leak ({} frames free)", free_after);
    } else {
        kprintln!("process teardown: FRAME LEAK, {} free before, {} after", free_before, free_after);
    }
    Ok(status)
}

/// Last lines written by user processes (for the status screen).
pub fn output_tail() -> Vec<String> {
    OUTPUT_TAIL.lock().iter().cloned().collect()
}

/// Terminates the current process and resumes `run`.
///
/// # Safety
/// Must be called on the current process's kernel stack (from a system call
/// or an exception raised in ring 3).
pub unsafe fn exit_current(status: i32) -> ! {
    cpu::disable_interrupts();
    osyx_leave_user(&raw const KERNEL_CTX, status)
}

pub fn current_pid() -> u64 {
    CURRENT.lock().as_ref().map_or(0, |p| p.pid)
}

/// `write` back end: line-buffered output to the kernel log.
pub fn write_output(bytes: &[u8]) {
    let mut g = CURRENT.lock();
    let Some(p) = g.as_mut() else { return };
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
    let mut g = CURRENT.lock();
    let p = g.as_mut().ok_or(Errno::NotSupported)?;
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
    let mut g = CURRENT.lock();
    let p = g.as_mut().ok_or(Errno::NotSupported)?;
    let pages = *p.regions.get(&addr).ok_or(Errno::InvalidArgument)?;
    if len.div_ceil(PAGE_SIZE) != pages {
        return Err(Errno::InvalidArgument);
    }
    for i in 0..pages {
        let frame = paging::unmap_in(p.root, addr + i * PAGE_SIZE, true).map_err(|_| Errno::Fault)?;
        let _ = free_frame(frame);
    }
    p.regions.remove(&addr);
    p.mapped_pages -= pages;
    Ok(())
}
