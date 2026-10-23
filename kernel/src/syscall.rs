//! SYSCALL/SYSRET entry and the system call dispatcher (ABI: `osyx_abi`).
//!
//! Entry: the CPU loads CS/SS from STAR, saves the user RIP in RCX and RFLAGS
//! in R11, and masks RFLAGS with FMASK (interrupts off). The stub switches to
//! the process's kernel stack, saves a [`SyscallFrame`], re-enables
//! interrupts and calls [`osyx_syscall_dispatch`]. Single CPU for now: the
//! stack pointers live in globals; SMP will move them to per-CPU data
//! reached through `swapgs`.

use crate::arch::cpu;
use crate::arch::gdt::{KERNEL_CS, USER_DS};
use crate::fixup::{copy_from_user, copy_to_user};
use crate::vfs::Lookup;
use crate::{interrupts, mem, process, sched, vfs};
use osyx_vfs::Cred;
use core::arch::global_asm;
use osyx_abi::{nr, Errno, SystemInfo};

#[no_mangle]
static mut OSYX_SYSCALL_KERNEL_RSP: u64 = 0;
#[no_mangle]
static mut OSYX_SYSCALL_USER_RSP: u64 = 0;

global_asm!(
    r#"
    .section .text
    .global osyx_syscall_entry
osyx_syscall_entry:
    mov [rip + OSYX_SYSCALL_USER_RSP], rsp
    mov rsp, [rip + OSYX_SYSCALL_KERNEL_RSP]
    push qword ptr [rip + OSYX_SYSCALL_USER_RSP]
    push r11
    push rcx
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    push r9
    push r8
    push r10
    push rdx
    push rsi
    push rdi
    push rax
    mov rdi, rsp
    sti
    call osyx_syscall_dispatch
    cli
    add rsp, 8
    pop rdi
    pop rsi
    pop rdx
    pop r10
    pop r8
    pop r9
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop rcx
    pop r11
    pop rsp
    sysretq
    "#
);

extern "C" {
    fn osyx_syscall_entry();
}

/// Registers saved by the entry stub (lowest address first).
#[repr(C)]
pub struct SyscallFrame {
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
}

const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

/// Enables SYSCALL and installs the entry point.
///
/// # Safety
/// Must run once after the GDT is loaded (STAR refers to its selectors).
pub unsafe fn init() {
    cpu::wrmsr(IA32_EFER, cpu::rdmsr(IA32_EFER) | 1); // SCE
                                                      // SYSCALL: CS = STAR[47:32], SS = +8.  SYSRET: CS = STAR[63:48] + 16, SS = +8.
    let sysret_base = (USER_DS & !3) as u64 - 8;
    cpu::wrmsr(IA32_STAR, (sysret_base << 48) | ((KERNEL_CS as u64) << 32));
    cpu::wrmsr(IA32_LSTAR, osyx_syscall_entry as *const () as u64);
    // Clear IF, TF, DF, NT and AC on entry.
    cpu::wrmsr(IA32_FMASK, (1 << 9) | (1 << 8) | (1 << 10) | (1 << 14) | (1 << 18));
}

/// # Safety
/// `top` must be the top of the current process's kernel stack.
pub unsafe fn set_kernel_stack(top: u64) {
    OSYX_SYSCALL_KERNEL_RSP = top;
}

#[no_mangle]
extern "C" fn osyx_syscall_dispatch(f: &mut SyscallFrame) -> u64 {
    let r = match f.rax {
        nr::EXIT => process::exit_current(f.rdi as i32),
        nr::WRITE => sys_write(f.rdi, f.rsi, f.rdx),
        nr::MAP => process::map_anonymous(f.rdi, f.rsi),
        nr::UNMAP => process::unmap_region(f.rdi, f.rsi).map(|_| 0),
        nr::SYSINFO => sys_sysinfo(f.rdi),
        nr::CLOCK => sys_clock(f.rdi),
        nr::SPAWN => sys_spawn(f.rdi, f.rsi, f.rdx, f.r10),
        nr::WAIT => sys_wait(f.rdi, f.rsi),
        nr::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        nr::SLEEP => {
            sched::sleep_ticks(ns_to_ticks(f.rdi));
            Ok(0)
        }
        nr::THREAD_SPAWN => process::spawn_thread(f.rdi, f.rsi, f.rdx, f.r10),
        nr::THREAD_EXIT => process::thread_exit(f.rdi as i32, f.rsi),
        nr::FUTEX_WAIT => {
            let timeout = (f.rdx != 0).then(|| ns_to_ticks(f.rdx));
            crate::futex::wait(f.rdi, f.rsi as u32, timeout).map(|_| 0)
        }
        nr::FUTEX_WAKE => crate::futex::wake(f.rdi, f.rsi as u32),
        nr::SET_FS_BASE => {
            if f.rdi != 0 && !crate::fixup::user_range_ok(f.rdi, 1) {
                Err(Errno::InvalidArgument)
            } else {
                sched::set_current_fs_base(f.rdi);
                Ok(0)
            }
        }
        nr::GETTID => Ok(sched::current_tid()),
        nr::OPEN => user_path(f.rdi, f.rsi)
            .and_then(|p| vfs::open(&Lookup::current(), &p, f.rdx, f.r10 as u32))
            .and_then(process::add_handle),
        nr::CLOSE => process::close_handle(f.rdi).map(|_| 0),
        nr::READ => sys_read(f.rdi, f.rsi, f.rdx),
        nr::SEEK => with_file(f.rdi, |o| o.seek(f.rsi as i64, f.rdx)),
        nr::FSTAT => with_file(f.rdi, |o| o.stat()).and_then(|st| put_user(f.rsi, &st)),
        nr::STAT => user_path(f.rdi, f.rsi).and_then(|p| vfs::stat(&Lookup::current(), &p)).and_then(|st| put_user(f.rdx, &st)),
        nr::LSTAT => user_path(f.rdi, f.rsi).and_then(|p| vfs::lstat(&Lookup::current(), &p)).and_then(|st| put_user(f.rdx, &st)),
        nr::READDIR => sys_readdir(f.rdi, f.rsi, f.rdx),
        nr::MKDIR => user_path(f.rdi, f.rsi).and_then(|p| vfs::mkdir(&Lookup::current(), &p, f.rdx as u32)).map(|_| 0),
        nr::UNLINK => user_path(f.rdi, f.rsi).and_then(|p| vfs::unlink(&Lookup::current(), &p)).map(|_| 0),
        nr::RMDIR => user_path(f.rdi, f.rsi).and_then(|p| vfs::rmdir(&Lookup::current(), &p)).map(|_| 0),
        nr::RENAME => user_path(f.rdi, f.rsi)
            .and_then(|from| Ok((from, user_path(f.rdx, f.r10)?)))
            .and_then(|(from, to)| vfs::rename(&Lookup::current(), &from, &to))
            .map(|_| 0),
        nr::FTRUNCATE => with_file(f.rdi, |o| o.truncate(f.rsi)).map(|_| 0),
        nr::FSYNC => with_file(f.rdi, |o| o.sync()).map(|_| 0),
        nr::CHDIR => user_path(f.rdi, f.rsi)
            .and_then(|p| vfs::chdir(&Lookup::current(), &p))
            .and_then(process::set_current_cwd)
            .map(|_| 0),
        nr::GETCWD => {
            let cwd = process::current_identity().1;
            if (cwd.len() as u64) > f.rsi {
                Err(Errno::InvalidArgument)
            } else {
                copy_to_user(f.rdi, cwd.as_bytes()).map(|_| cwd.len() as u64)
            }
        }
        nr::SYMLINK => user_path(f.rdi, f.rsi)
            .and_then(|target| Ok((target, user_path(f.rdx, f.r10)?)))
            .and_then(|(target, p)| vfs::symlink(&Lookup::current(), &target, &p))
            .map(|_| 0),
        nr::READLINK => user_path(f.rdi, f.rsi).and_then(|p| vfs::readlink(&Lookup::current(), &p)).and_then(|t| {
            let n = t.len().min(f.r10 as usize);
            copy_to_user(f.rdx, &t.as_bytes()[..n]).map(|_| n as u64)
        }),
        nr::CHMOD => user_path(f.rdi, f.rsi).and_then(|p| vfs::chmod(&Lookup::current(), &p, f.rdx as u32)).map(|_| 0),
        nr::MOUNT => sys_mount(f.rdi),
        nr::UMOUNT => user_path(f.rdi, f.rsi).and_then(|p| sys_umount(&p)).map(|_| 0),
        nr::GETUID => {
            let c = process::current_identity().0;
            Ok(c.uid as u64 | (c.gid as u64) << 32)
        }
        nr::SETUID => process::set_current_cred(Cred { uid: f.rdi as u32, gid: f.rsi as u32 }).map(|_| 0),
        nr::STATFS => user_path(f.rdi, f.rsi).and_then(|p| vfs::statfs(&Lookup::current(), &p)).and_then(|st| put_user(f.rdx, &st)),        _ => Err(Errno::NotSupported),
    };
    // Safe point: a thread whose process is exiting ends here instead of
    // returning to user mode.
    if sched::current_kill_pending() {
        process::finish_current_thread(0);
    }
    osyx_abi::encode(r)
}

/// Nanoseconds to timer ticks, rounded up (at least one tick for ns > 0).
fn ns_to_ticks(ns: u64) -> u64 {
    let tick_ns = 1_000_000_000 / crate::arch::pic::TIMER_HZ as u64;
    ns.div_ceil(tick_ns)
}

/// Bounce buffer size for file data between user memory and the VFS.
const FILE_CHUNK: usize = 64 * 1024;

fn sys_write(fd: u64, buf: u64, len: u64) -> Result<u64, Errno> {
    if len > osyx_abi::MAX_WRITE as u64 {
        return Err(Errno::InvalidArgument);
    }
    if !crate::fixup::user_range_ok(buf, len) {
        return Err(Errno::Fault);
    }
    if fd >= osyx_abi::FIRST_FILE_HANDLE {
        return write_file(fd, buf, len as usize);
    }
    if fd != osyx_abi::STDOUT && fd != osyx_abi::STDERR {
        return Err(Errno::BadHandle);
    }
    let mut chunk = [0u8; 512];
    let mut done = 0;
    while done < len {
        let n = (len - done).min(chunk.len() as u64) as usize;
        copy_from_user(&mut chunk[..n], buf + done)?;
        process::write_output(&chunk[..n]);
        done += n as u64;
    }
    Ok(len)
}

/// Copies a path argument (absolute or relative, UTF-8, no NUL) out of user memory.
fn user_path(ptr: u64, len: u64) -> Result<alloc::string::String, Errno> {
    if len == 0 || len > osyx_abi::MAX_PATH as u64 {
        return Err(Errno::InvalidArgument);
    }
    let mut buf = alloc::vec![0u8; len as usize];
    copy_from_user(&mut buf, ptr)?;
    if buf.contains(&0) {
        return Err(Errno::InvalidArgument);
    }
    alloc::string::String::from_utf8(buf).map_err(|_| Errno::InvalidArgument)
}

fn with_file<T>(handle: u64, op: impl FnOnce(&mut vfs::OpenFile) -> Result<T, Errno>) -> Result<T, Errno> {
    let file = process::get_handle(handle)?;
    let mut g = file.lock();
    op(&mut g)
}

/// Copies a plain `repr(C)` value to user memory.
fn put_user<T: Copy>(addr: u64, v: &T) -> Result<u64, Errno> {
    // SAFETY: only used with padding-free repr(C) ABI structs.
    let bytes = unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) };
    copy_to_user(addr, bytes)?;
    Ok(0)
}

/// Reads in chunks; after a chunk has been delivered, a later failure ends
/// the call with the bytes delivered so far (like a short read).
fn sys_read(handle: u64, buf: u64, len: u64) -> Result<u64, Errno> {
    if handle < osyx_abi::FIRST_FILE_HANDLE {
        return Err(Errno::BadHandle);
    }
    let len = len.min(osyx_abi::MAX_READ as u64) as usize;
    if !crate::fixup::user_range_ok(buf, len as u64) {
        return Err(Errno::Fault);
    }
    with_file(handle, |o| {
        let mut k = alloc::vec![0u8; len.min(FILE_CHUNK)];
        let mut done = 0;
        while done < len {
            let want = (len - done).min(k.len());
            let n = match o.read(&mut k[..want]).and_then(|n| copy_to_user(buf + done as u64, &k[..n]).map(|_| n)) {
                Ok(n) => n,
                Err(e) if done == 0 => return Err(e),
                Err(_) => break,
            };
            o.advance(n);
            done += n;
            if n < want {
                break;
            }
        }
        Ok(done as u64)
    })
}

fn write_file(handle: u64, buf: u64, len: usize) -> Result<u64, Errno> {
    with_file(handle, |o| {
        let mut k = alloc::vec![0u8; len.min(FILE_CHUNK)];
        let mut done = 0;
        while done < len {
            let n = (len - done).min(k.len());
            let r = copy_from_user(&mut k[..n], buf + done as u64).and_then(|_| o.write(&k[..n]));
            match r {
                Ok(w) => {
                    done += w;
                    if w < n {
                        break;
                    }
                }
                Err(e) if done == 0 => return Err(e),
                Err(_) => break,
            }
        }
        Ok(done as u64)
    })
}

fn sys_readdir(handle: u64, buf: u64, len: u64) -> Result<u64, Errno> {
    let len = len.min(FILE_CHUNK as u64);
    if !crate::fixup::user_range_ok(buf, len) {
        return Err(Errno::Fault);
    }
    with_file(handle, |o| {
        let mut k = alloc::vec![0u8; len as usize];
        let n = o.readdir(&mut k)?;
        copy_to_user(buf, &k[..n])?;
        Ok(n as u64)
    })
}

fn sys_sysinfo(out: u64) -> Result<u64, Errno> {
    let (free, total) = mem::frame_stats();
    let mut info = SystemInfo {
        abi_version: osyx_abi::ABI_VERSION,
        page_size: mem::PAGE_SIZE as u32,
        total_memory: total * mem::PAGE_SIZE,
        free_memory: free * mem::PAGE_SIZE,
        uptime_ns: interrupts::uptime_ms() * 1_000_000,
        pid: sched::current_pid(),
        kernel_version: [0; 16],
    };
    let v = crate::VERSION.as_bytes();
    info.kernel_version[..v.len().min(16)].copy_from_slice(&v[..v.len().min(16)]);
    // SAFETY: SystemInfo is repr(C) plain data without padding-dependent
    // invariants; viewing it as bytes is sound.
    let bytes = unsafe {
        core::slice::from_raw_parts(&info as *const SystemInfo as *const u8, core::mem::size_of::<SystemInfo>())
    };
    copy_to_user(out, bytes)?;
    Ok(0)
}

fn sys_clock(id: u64) -> Result<u64, Errno> {
    match id {
        osyx_abi::CLOCK_MONOTONIC => Ok(interrupts::uptime_ms() * 1_000_000),
        osyx_abi::CLOCK_REALTIME => Ok(crate::time::now_ns().max(0) as u64),
        _ => Err(Errno::InvalidArgument),
    }
}

fn sys_spawn(name_ptr: u64, name_len: u64, args_ptr: u64, args_len: u64) -> Result<u64, Errno> {
    if name_len == 0 || name_len > osyx_abi::MAX_NAME as u64 || args_len > osyx_abi::MAX_SPAWN_ARGS as u64 {
        return Err(Errno::InvalidArgument);
    }
    let mut name_buf = [0u8; osyx_abi::MAX_NAME];
    copy_from_user(&mut name_buf[..name_len as usize], name_ptr)?;
    let name = core::str::from_utf8(&name_buf[..name_len as usize]).map_err(|_| Errno::InvalidArgument)?;
    if !name.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Errno::InvalidArgument);
    }
    let mut block = alloc::vec![0u8; args_len as usize];
    copy_from_user(&mut block, args_ptr)?;
    // The block is a sequence of NUL-terminated UTF-8 strings.
    if block.last().is_some_and(|&b| b != 0) {
        return Err(Errno::InvalidArgument);
    }
    let mut argv: alloc::vec::Vec<&str> = alloc::vec![name];
    if !block.is_empty() {
        for part in block[..block.len() - 1].split(|&b| b == 0) {
            argv.push(core::str::from_utf8(part).map_err(|_| Errno::InvalidArgument)?);
        }
    }
    process::spawn(sched::current_pid(), name, &argv).map_err(|e| e.errno())
}

fn sys_wait(pid: u64, status_ptr: u64) -> Result<u64, Errno> {
    // Validate the output pointer before reaping, so a bad pointer cannot
    // lose the child's status.
    if status_ptr != 0 {
        copy_to_user(status_ptr, &0i32.to_le_bytes())?;
    }
    let (child, status) = process::wait(sched::current_pid(), pid)?;
    if status_ptr != 0 {
        copy_to_user(status_ptr, &status.to_le_bytes())?;
    }
    Ok(child)
}

fn user_string(ptr: u64, len: u64, max: usize) -> Result<alloc::string::String, Errno> {
    if len > max as u64 {
        return Err(Errno::InvalidArgument);
    }
    let mut buf = alloc::vec![0u8; len as usize];
    copy_from_user(&mut buf, ptr)?;
    alloc::string::String::from_utf8(buf).map_err(|_| Errno::InvalidArgument)
}

fn sys_mount(args_ptr: u64) -> Result<u64, Errno> {
    let mut raw = [0u8; core::mem::size_of::<osyx_abi::MountArgs>()];
    copy_from_user(&mut raw, args_ptr)?;
    // SAFETY: MountArgs is plain repr(C) data of exactly this size.
    let a: osyx_abi::MountArgs = unsafe { core::ptr::read_unaligned(raw.as_ptr() as *const osyx_abi::MountArgs) };
    let source = user_string(a.source, a.source_len, osyx_abi::MAX_PATH)?;
    let target = user_path(a.target, a.target_len)?;
    let fstype = user_string(a.fstype, a.fstype_len, 16)?;
    vfs::mount(&Lookup::current(), &source, &target, &fstype, a.flags, a.size_limit).map(|_| 0)
}

fn sys_umount(p: &str) -> Result<(), Errno> {
    let cx = Lookup::current();
    if !cx.cred.is_root() {
        return Err(Errno::PermissionDenied);
    }
    vfs::unmount(&cx, p)
}