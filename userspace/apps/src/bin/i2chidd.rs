//! i2chidd: finds HID over I2C devices (laptop touchpads, keyboards) in the
//! ACPI tables and hands them to the kernel driver.
//!
//! The firmware's AML runs here, in user space, like in `batteryd`: byte
//! code this interpreter does not handle ends this process, never the
//! system. `osyx-i2chid` evaluates `_HID`/`_CID`, `_STA`, `_CRS`, the HID
//! over I2C `_DSM` and the controller's `SSCN`/`FMCN`; the description
//! goes to the kernel with `firmware(I2C_HID)`, which checks it again and
//! logs why it leaves a device alone.
//!
//! The program stays resident: it repeats the discovery with backoff
//! (30 s doubling to 5 min) so a namespace that was not fully readable at
//! boot, a controller that came out of reset late, or a device the kernel
//! refused once all get another chance. The kernel skips devices it
//! already drives, so a quiet pass is one encode and one syscall.
//! `--once` runs a single pass, prints every line and exits with the
//! result (the `tests,i2c` mode).
#![no_std]
#![no_main]

use os_std::prelude::*;
use os_std::time::{Duration, Instant};
use os_std::{env, thread};
use osyx_abi::firmware as op;
use osyx_battery::Hardware;

os_std::entry!(main);

/// The machine through `nr::FIRMWARE`.
struct Machine {
    start: Instant,
}

impl Machine {
    fn new() -> Machine {
        Machine { start: Instant::now() }
    }
}

fn fw(code: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    os_sys::firmware(code, a, b, c, d).unwrap_or(u64::MAX)
}

impl Hardware for Machine {
    fn port_in(&self, port: u16, width: u8) -> u64 {
        fw(op::PORT, port as u64, width as u64, 0, 0)
    }
    fn port_out(&self, port: u16, width: u8, value: u64) {
        fw(op::PORT, port as u64, width as u64, value, 1);
    }
    fn phys_read(&self, address: u64, width: u8) -> u64 {
        fw(op::PHYS, address, width as u64, 0, 0)
    }
    fn phys_write(&self, address: u64, width: u8, value: u64) {
        fw(op::PHYS, address, width as u64, value, 1);
    }
    fn pci_read(&self, bdf: u64, offset: u64, width: u8) -> u64 {
        fw(op::PCI, bdf, offset, width as u64, 0)
    }
    fn pci_write(&self, bdf: u64, offset: u64, width: u8, value: u64) {
        fw(op::PCI, bdf, offset, 0x100 | width as u64, value);
    }
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn sleep_ms(&self, ms: u64) {
        thread::sleep(Duration::from_millis(ms));
    }
}

/// The `n`-th ACPI table with signature `sig`.
fn table(sig: &[u8; 4], n: u64) -> Option<Vec<u8>> {
    let sig = u32::from_le_bytes(*sig) as u64;
    let len = os_sys::firmware(op::TABLE, sig, n, 0, 0).ok()? as usize;
    let mut buf = vec![0u8; len];
    os_sys::firmware(op::TABLE, sig, n, buf.as_mut_ptr() as u64, len as u64).ok()?;
    Some(buf)
}

struct Pass {
    devices: usize,
    claimed: usize,
    /// Load problems and per-device discovery lines, printed when the
    /// result changes.
    problems: Vec<String>,
    lines: Vec<String>,
}

/// One discovery and registration. `None`: no DSDT or the kernel refused
/// the description itself; the next pass may still work.
fn pass() -> Option<Pass> {
    let Some(dsdt) = table(b"DSDT", 0) else {
        eprintln!("i2chidd: no DSDT");
        return None;
    };
    let mut tables = vec![dsdt];
    for n in 0.. {
        let Some(ssdt) = table(b"SSDT", n) else { break };
        tables.push(ssdt);
    }
    let mut problems = Vec::new();
    let machine = Machine::new();
    let mut ctx = osyx_i2chid::load(machine, &tables, &mut |problem| problems.push(String::from(problem)));
    let mut lines = Vec::new();
    let devices = osyx_i2chid::enumerate(&mut ctx, &mut |line| lines.push(String::from(line)));
    if devices.is_empty() {
        return Some(Pass { devices: 0, claimed: 0, problems, lines });
    }
    let blob = match osyx_i2c::spec::encode(&devices) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("i2chidd: cannot describe {} device(s) to the kernel: {e:?}", devices.len());
            return None;
        }
    };
    match os_sys::firmware(op::I2C_HID, blob.as_ptr() as u64, blob.len() as u64, 0, 0) {
        Ok(claimed) => Some(Pass { devices: devices.len(), claimed: claimed as usize, problems, lines }),
        Err(e) => {
            eprintln!("i2chidd: the kernel refused the description: {e:?}");
            None
        }
    }
}

fn main() -> i32 {
    let _ = os_std::system::thread_priority(0, os_std::system::priority::BELOW_NORMAL);
    let once = env::args().any(|a| a == "--once");
    let mut previous: Option<(usize, usize)> = None;
    let mut wait_secs = 30u64;
    loop {
        let status = pass();
        if let Some(p) = &status {
            let key = (p.devices, p.claimed);
            if once || previous != Some(key) {
                for problem in &p.problems {
                    eprintln!("i2chidd: {problem}");
                }
                for line in &p.lines {
                    println!("i2chidd: {line}");
                }
                if p.devices == 0 {
                    println!("i2chidd: no HID over I2C devices in the ACPI tables");
                } else {
                    println!("i2chidd: the kernel claimed {} of {} device(s)", p.claimed, p.devices);
                }
                previous = Some(key);
                wait_secs = 30;
            }
        } else {
            // The namespace or the kernel may come around: look again soon.
            previous = None;
            wait_secs = 30;
        }
        if once {
            return if status.is_some() { 0 } else { 1 };
        }
        thread::sleep(Duration::from_secs(wait_secs));
        wait_secs = (wait_secs * 2).min(300);
    }
}
