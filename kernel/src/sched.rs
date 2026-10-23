//! Threads and the preemptive round-robin scheduler (single CPU).
//!
//! * Every thread has its own 64 KiB kernel stack (the boot thread keeps the
//!   loader's stack), a saved stack pointer and a 512-byte FXSAVE area.
//! * [`schedule`] picks the next ready thread and calls `osyx_switch`, which
//!   saves the callee-saved registers and RFLAGS on the old stack and resumes
//!   the new one. Caller-saved registers are already saved by whoever called
//!   `schedule` (the compiler, or the interrupt stub when preempting).
//! * The timer interrupt calls [`tick`]: it wakes sleepers and, when the
//!   running thread has used its time slice and another thread is ready,
//!   switches from inside the interrupt handler. The interrupted thread later
//!   resumes inside that handler and returns with `iretq`.
//! * x87/SSE state is switched eagerly (`fxsave`/`fxrstor` on every switch).
//! * CR3, TSS.RSP0 and the SYSCALL kernel stack follow the incoming thread.
//! * A thread that exits cannot free the stack it runs on; the next thread
//!   frees it right after the switch ([`after_switch`]).
//!
//! Every lock in the kernel disables interrupts while held, so a thread can
//! never be preempted while holding one. `SCHED` is the innermost lock any
//! scheduler path takes except HEAP (thread creation/reaping allocate).

use crate::arch::{cpu, gdt};
use crate::mem::paging;
use crate::sync::IrqSpinLock;
use crate::{interrupts, syscall};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::global_asm;

pub type Tid = u64;

/// Timer ticks (10 ms each) a thread may run before it can be preempted.
pub const TIME_SLICE_TICKS: u32 = 2;
const KERNEL_STACK_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked,
    Dead,
}

/// FXSAVE image (x87 + SSE registers, MXCSR).
#[repr(C, align(16))]
pub struct FpuState([u8; 512]);

impl FpuState {
    /// The state after `fninit` + default MXCSR: all exceptions masked,
    /// round to nearest.
    fn initial() -> Box<FpuState> {
        let mut s = Box::new(FpuState([0; 512]));
        s.0[0..2].copy_from_slice(&0x037Fu16.to_le_bytes()); // FCW
        s.0[24..28].copy_from_slice(&0x1F80u32.to_le_bytes()); // MXCSR
        s
    }
}

pub struct Thread {
    pub tid: Tid,
    pub name: String,
    /// Owning process, 0 for kernel threads.
    pub pid: u64,
    state: State,
    rsp: u64,
    /// Page table root active while this thread runs.
    root: u64,
    /// Empty for the boot thread, which runs on the loader-provided stack.
    _kstack: Vec<u128>,
    kstack_top: u64,
    fpu: Box<FpuState>,
    wake_at: Option<u64>,
    joiners: Vec<Tid>,
    slice: u32,
    /// FS base (user TLS thread pointer), switched with the thread.
    fs_base: u64,
    /// Set when the owning process is exiting: the thread ends at its next
    /// safe point (return to user mode or an interrupted blocking call).
    kill_pending: bool,
}

const IA32_FS_BASE: u32 = 0xC000_0100;

struct Scheduler {
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: VecDeque<Tid>,
    current: Tid,
    idle: Tid,
    next_tid: Tid,
    reap: Option<Tid>,
    switches: u64,
}

static SCHED: IrqSpinLock<Option<Scheduler>> = IrqSpinLock::new(None);

global_asm!(
    r#"
    .section .text
    // void osyx_switch(u64 *save_rsp, u64 new_rsp)
    .global osyx_switch
osyx_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    pushfq
    mov [rdi], rsp
    mov rsp, rsi
    popfq
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

    // First "return" of a new thread: r12 = entry, r13/r14 = arguments.
    .global osyx_thread_trampoline
osyx_thread_trampoline:
    mov rdi, r12
    mov rsi, r13
    mov rdx, r14
    call osyx_thread_start
    ud2
    "#
);

extern "C" {
    fn osyx_switch(save_rsp: *mut u64, new_rsp: u64);
    fn osyx_thread_trampoline();
}

/// Thread entry point: `entry(a, b)`; returning ends the thread.
pub type Entry = fn(u64, u64);

#[no_mangle]
extern "C" fn osyx_thread_start(entry: u64, a: u64, b: u64) -> ! {
    after_switch();
    cpu::enable_interrupts();
    // SAFETY: `entry` was produced from an `Entry` in `spawn`.
    let f: Entry = unsafe { core::mem::transmute::<u64, Entry>(entry) };
    f(a, b);
    exit_current()
}

fn idle_loop(_: u64, _: u64) {
    loop {
        cpu::enable_and_halt();
    }
}

#[allow(clippy::too_many_arguments)]
fn new_thread(tid: Tid, name: &str, pid: u64, root: u64, entry: Entry, a: u64, b: u64, fs_base: u64) -> Box<Thread> {
    let mut stack = alloc::vec![0u128; KERNEL_STACK_SIZE / 16];
    let top = stack.as_mut_ptr() as u64 + KERNEL_STACK_SIZE as u64;
    // Frame consumed by osyx_switch: rflags, r15, r14, r13, r12, rbx, rbp, ret.
    let frame: [u64; 8] = [0x2, 0, b, a, entry as *const () as u64, 0, 0, osyx_thread_trampoline as *const () as u64];
    let rsp = top - 64;
    // SAFETY: the frame lies inside the freshly allocated stack.
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, 8) };
    Box::new(Thread {
        tid,
        name: name.to_string(),
        pid,
        state: State::Ready,
        rsp,
        root,
        _kstack: stack,
        kstack_top: top,
        fpu: FpuState::initial(),
        wake_at: None,
        joiners: Vec::new(),
        slice: TIME_SLICE_TICKS,
        fs_base,
        kill_pending: false,
    })
}

/// Turns the boot flow into thread 1 ("kmain") and creates the idle thread.
pub fn init() {
    let root = paging::kernel_root();
    let boot = Box::new(Thread {
        tid: 1,
        name: "kmain".to_string(),
        pid: 0,
        state: State::Running,
        rsp: 0,
        root,
        _kstack: Vec::new(),
        kstack_top: 0,
        fpu: FpuState::initial(),
        wake_at: None,
        joiners: Vec::new(),
        slice: TIME_SLICE_TICKS,
        fs_base: 0,
        kill_pending: false,
    });
    let idle = new_thread(2, "idle", 0, root, idle_loop, 0, 0, 0);
    let mut threads = BTreeMap::new();
    threads.insert(1, boot);
    threads.insert(2, idle);
    *SCHED.lock() = Some(Scheduler {
        threads,
        ready: VecDeque::with_capacity(64),
        current: 1,
        idle: 2,
        next_tid: 3,
        reap: None,
        switches: 0,
    });
}

/// Creates a ready thread running `entry(a, b)` with page table `root` and
/// FS base `fs_base`.
#[allow(clippy::too_many_arguments)]
pub fn spawn(name: &str, pid: u64, root: u64, entry: Entry, a: u64, b: u64, fs_base: u64) -> Tid {
    let mut g = SCHED.lock();
    let s = g.as_mut().expect("scheduler not initialised");
    let tid = s.next_tid;
    s.next_tid += 1;
    let t = new_thread(tid, name, pid, root, entry, a, b, fs_base);
    s.threads.insert(tid, t);
    s.ready.push_back(tid);
    tid
}

/// Spawns a kernel thread (kernel page tables, no process).
pub fn spawn_kernel(name: &str, entry: Entry, a: u64, b: u64) -> Tid {
    spawn(name, 0, paging::kernel_root(), entry, a, b, 0)
}

type SwitchPlan = (*mut u64, u64, *mut FpuState, *const FpuState);

impl Scheduler {
    fn pick_next(&mut self) -> Option<SwitchPlan> {
        let cur = self.current;
        if let Some(t) = self.threads.get_mut(&cur) {
            if t.state == State::Running {
                t.state = State::Ready;
                if cur != self.idle {
                    self.ready.push_back(cur);
                }
            }
        }
        let next = loop {
            match self.ready.pop_front() {
                Some(n) if self.threads.get(&n).is_some_and(|t| t.state == State::Ready) => break n,
                Some(_) => continue,
                None => break self.idle,
            }
        };
        let nt = self.threads.get_mut(&next).expect("next thread exists");
        nt.state = State::Running;
        nt.slice = TIME_SLICE_TICKS;
        if next == cur {
            return None;
        }
        let (new_rsp, new_root, new_top, new_fs) = (nt.rsp, nt.root, nt.kstack_top, nt.fs_base);
        let new_fpu: *const FpuState = &*nt.fpu;
        let old = self.threads.get_mut(&cur).expect("current thread exists");
        if old.state == State::Dead {
            self.reap = Some(cur);
        }
        // SAFETY: IA32_FS_BASE exists on every x86-64 CPU; the kernel never
        // uses FS, so the value only matters to each thread's user code.
        unsafe {
            old.fs_base = cpu::rdmsr(IA32_FS_BASE);
            cpu::wrmsr(IA32_FS_BASE, new_fs);
        }
        let old_rsp: *mut u64 = &mut old.rsp;
        let old_fpu: *mut FpuState = &mut *old.fpu;
        self.current = next;
        self.switches += 1;
        if new_root != paging::current_root() {
            // SAFETY: every thread root maps the kernel half (shared PDPTs).
            unsafe { cpu::write_cr3(new_root) };
        }
        if new_top != 0 {
            // SAFETY: the thread's kernel stack lives as long as the thread.
            unsafe {
                gdt::set_kernel_stack(new_top);
                syscall::set_kernel_stack(new_top);
            }
        }
        Some((old_rsp, new_rsp, old_fpu, new_fpu))
    }
}

/// Gives the CPU to the next ready thread (round robin). Returns when the
/// current thread is scheduled again (immediately if nothing else is ready).
pub fn schedule() {
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    // Interrupts are already off, so dropping the guard keeps them off.
    let plan = SCHED.lock().as_mut().and_then(Scheduler::pick_next);
    if let Some((old_rsp, new_rsp, old_fpu, new_fpu)) = plan {
        // SAFETY: the pointers refer to boxed fields of threads that stay
        // alive across the switch (a dead thread is reaped only after it).
        unsafe {
            cpu::fxsave(old_fpu as *mut u8);
            cpu::fxrstor(new_fpu as *const u8);
            osyx_switch(old_rsp, new_rsp);
        }
        after_switch();
    }
    if were_enabled {
        cpu::enable_interrupts();
    }
}

/// Runs on the new thread right after every switch: frees the thread that
/// exited before it.
fn after_switch() {
    let dead = SCHED.lock().as_mut().and_then(|s| s.reap.take().and_then(|t| s.threads.remove(&t)));
    drop(dead);
}

/// Timer interrupt hook: wakes sleepers and preempts an expired time slice.
pub fn tick(now: u64) {
    let resched = {
        let mut g = SCHED.lock();
        let Some(s) = g.as_mut() else { return };
        for (tid, t) in s.threads.iter_mut() {
            if t.state == State::Blocked && t.wake_at.is_some_and(|w| w <= now) {
                t.wake_at = None;
                t.state = State::Ready;
                s.ready.push_back(*tid);
            }
        }
        let idle = s.idle;
        let cur = s.threads.get_mut(&s.current).expect("current thread exists");
        cur.slice = cur.slice.saturating_sub(1);
        (cur.slice == 0 || cur.tid == idle) && !s.ready.is_empty()
    };
    if resched {
        schedule();
    }
}

/// Gives up the rest of the time slice.
pub fn yield_now() {
    schedule();
}

/// Blocks the current thread for at least `ticks` timer ticks.
pub fn sleep_ticks(ticks: u64) {
    if ticks == 0 {
        return yield_now();
    }
    let were_enabled = cpu::interrupts_enabled();
    cpu::disable_interrupts();
    {
        let mut g = SCHED.lock();
        let s = g.as_mut().expect("scheduler not initialised");
        let t = s.threads.get_mut(&s.current).expect("current thread exists");
        t.wake_at = Some(interrupts::ticks() + ticks);
        t.state = State::Blocked;
    }
    schedule();
    if were_enabled {
        cpu::enable_interrupts();
    }
}

/// Marks the current thread blocked. The caller must have registered a
/// wake-up (see [`wake`]) and must call [`schedule`] next, all with
/// interrupts disabled so the wake-up cannot be lost.
pub fn mark_current_blocked() {
    mark_current_blocked_until(None);
}

/// Like [`mark_current_blocked`], but the timer also wakes the thread at
/// tick `deadline`.
pub fn mark_current_blocked_until(deadline: Option<u64>) {
    let mut g = SCHED.lock();
    let s = g.as_mut().expect("scheduler not initialised");
    let t = s.threads.get_mut(&s.current).expect("current thread exists");
    t.state = State::Blocked;
    t.wake_at = deadline;
}

/// Asks thread `tid` to end at its next safe point and wakes it if blocked
/// so that it gets there.
pub fn request_kill(tid: Tid) {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    if let Some(t) = s.threads.get_mut(&tid) {
        t.kill_pending = true;
        if t.state == State::Blocked {
            t.state = State::Ready;
            t.wake_at = None;
            s.ready.push_back(tid);
        }
    }
}

/// True if the current thread has been asked to end.
pub fn current_kill_pending() -> bool {
    SCHED.lock().as_ref().is_some_and(|s| s.threads.get(&s.current).is_some_and(|t| t.kill_pending))
}

/// Sets the FS base of the current thread and loads it into the CPU.
pub fn set_current_fs_base(value: u64) {
    let mut g = SCHED.lock();
    let s = g.as_mut().expect("scheduler not initialised");
    let t = s.threads.get_mut(&s.current).expect("current thread exists");
    t.fs_base = value;
    // SAFETY: see `pick_next`; the kernel does not use FS.
    unsafe { cpu::wrmsr(IA32_FS_BASE, value) };
}

/// Loads the initial x87/SSE state into the CPU for the current thread
/// (`execve`: nothing of the old program's registers survives). The saved
/// copy is refreshed from the CPU at the next switch.
pub fn reset_current_fpu() {
    let init = FpuState::initial();
    // SAFETY: a valid, 16-byte aligned FXSAVE image.
    unsafe { cpu::fxrstor(&*init as *const FpuState as *const u8) };
}

pub fn rename_current(name: &str) {
    let mut g = SCHED.lock();
    if let Some(t) = g.as_mut().and_then(|s| s.threads.get_mut(&s.current)) {
        t.name = name.to_string();
    }
}

/// Makes a blocked thread ready again (no effect on other states).
pub fn wake(tid: Tid) {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    if let Some(t) = s.threads.get_mut(&tid) {
        if t.state == State::Blocked {
            t.state = State::Ready;
            t.wake_at = None;
            s.ready.push_back(tid);
        }
    }
}

/// Ends the current thread. Its stack is freed by the next thread.
pub fn exit_current() -> ! {
    cpu::disable_interrupts();
    let joiners = {
        let mut g = SCHED.lock();
        let s = g.as_mut().expect("scheduler not initialised");
        let t = s.threads.get_mut(&s.current).expect("current thread exists");
        assert!(t.kstack_top != 0, "the boot thread cannot exit");
        t.state = State::Dead;
        core::mem::take(&mut t.joiners)
    };
    for j in joiners {
        wake(j);
    }
    schedule();
    unreachable!("dead thread was scheduled again");
}

/// Waits until thread `tid` has exited (returns at once if it does not exist).
pub fn join(tid: Tid) {
    loop {
        let were_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        let waiting = {
            let mut g = SCHED.lock();
            let s = g.as_mut().expect("scheduler not initialised");
            let me = s.current;
            match s.threads.get_mut(&tid) {
                Some(t) if t.state != State::Dead => {
                    t.joiners.push(me);
                    s.threads.get_mut(&me).expect("current thread exists").state = State::Blocked;
                    true
                }
                _ => false,
            }
        };
        if waiting {
            schedule();
        }
        if were_enabled {
            cpu::enable_interrupts();
        }
        if !waiting {
            return;
        }
    }
}

pub fn current_tid() -> Tid {
    SCHED.lock().as_ref().map_or(1, |s| s.current)
}

pub fn current_pid() -> u64 {
    SCHED.lock().as_ref().and_then(|s| s.threads.get(&s.current).map(|t| t.pid)).unwrap_or(0)
}

/// Changes the page table root recorded for the current thread (used when a
/// process leaves its address space on exit).
pub fn set_current_root(root: u64) {
    let mut g = SCHED.lock();
    if let Some(s) = g.as_mut() {
        if let Some(t) = s.threads.get_mut(&s.current) {
            t.root = root;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub threads: usize,
    pub ready: usize,
    pub switches: u64,
}

/// Logs every thread with its state (diagnostics).
pub fn dump() {
    let g = SCHED.lock();
    let Some(s) = g.as_ref() else { return };
    for t in s.threads.values() {
        crate::kprintln!("  thread {:3} {:<10} pid {:2} {:?}", t.tid, t.name, t.pid, t.state);
    }
}

pub fn stats() -> Stats {
    let g = SCHED.lock();
    let s = g.as_ref().expect("scheduler not initialised");
    Stats { threads: s.threads.len(), ready: s.ready.len(), switches: s.switches }
}
