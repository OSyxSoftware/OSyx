//! Concurrent process/thread mapping churn for the per-process memory locks.
use os_std::{os::raw, prelude::*, process::Command, system, thread, time::{Duration, Instant}};

pub fn run() -> i32 {
    let mut children = Vec::new();
    for _ in 0..2 {
        match Command::new("/bin/worker").arg("memory-locks").spawn() {
            Ok(child) => children.push(child),
            Err(e) => { println!("memory-locks: spawn failed: {e}"); return 1; }
        }
    }
    let mut failed = 0;
    for child in children {
        failed += !matches!(child.wait(), Ok(status) if status.success()) as i32;
    }
    println!("memory-locks: processes=2 failed={failed}");
    failed
}

pub fn worker() -> i32 {
    let cpus = system::cpu_info().map_or(1, |info| info.0);
    let mut threads = Vec::new();
    for cpu in 0..cpus.min(4) {
        threads.push(thread::spawn(move || churn(cpu)));
    }
    let mut failed = 0;
    for task in threads {
        failed += !matches!(task.join(), Ok(Ok(()))) as i32;
    }
    failed
}

fn churn(cpu: u32) -> Result<(), raw::Errno> {
    system::thread_affinity(0, 1u64 << cpu).map_err(|_| raw::Errno::InvalidArgument)?;
    const LEN: usize = 256 * 1024;
    let start = Instant::now();
    let mut rounds = 0;
    while start.elapsed() < Duration::from_secs(6) {
        let page = raw::map(LEN, raw::PROT_READ | raw::PROT_WRITE)?;
        let result = (|| {
            // SAFETY: this thread alone owns the entire live mapping. Touch
            // each page to exercise demand faults, including after discard.
            unsafe {
                for offset in (0..LEN).step_by(4096) {
                    if page.add(offset).read_volatile() != 0 { return Err(raw::Errno::Io); }
                    page.add(offset).write_volatile(0x5a);
                }
                raw::protect(page, LEN, raw::PROT_READ)?;
                for offset in (0..LEN).step_by(4096) {
                    if page.add(offset).read_volatile() != 0x5a { return Err(raw::Errno::Io); }
                }
                raw::protect(page, LEN, raw::PROT_READ | raw::PROT_WRITE)?;
                raw::discard(page, LEN)?;
                for offset in (0..LEN).step_by(4096) {
                    if page.add(offset).read_volatile() != 0 { return Err(raw::Errno::Io); }
                }
            }
            Ok(())
        })();
        // SAFETY: no pointer to this private mapping escaped the thread.
        unsafe { raw::unmap(page, LEN)?; }
        result?;
        rounds += 1;
    }
    println!("memory-locks: cpu={cpu} rounds={rounds} ms={}", start.elapsed().as_millis());
    if rounds == 0 { return Err(raw::Errno::Io); }
    Ok(())
}
