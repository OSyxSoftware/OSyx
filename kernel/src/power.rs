//! Power off and reboot.
//!
//! Every volume is synced and unmounted first. Power off uses the ACPI
//! PM1a control register of the chipset's power management block, whose
//! base is read from the PCI configuration of an ICH9 LPC bridge (QEMU q35)
//! or a PIIX4 power management function (QEMU i440fx); the sleep type for
//! S5 is 0 as in QEMU's DSDT. OSyx does not parse ACPI tables yet, so on
//! other chipsets power off reports `NotSupported` (after the volumes were
//! already made clean). Reboot uses the reset control register 0xCF9, then
//! the keyboard controller, then a triple fault.

use crate::arch::cpu;
use crate::{kprintln, pci};
use osyx_abi::Errno;

const SLP_EN: u16 = 1 << 13;
const SLP_TYP_S5: u16 = 0;

/// The PM1a control port, if the chipset is one we know.
fn pm1a_control() -> Option<u16> {
    for d in pci::enumerate() {
        let base_reg = match (d.vendor, d.device) {
            (0x8086, 0x2918) => 0x40, // ICH9 LPC: PMBASE
            (0x8086, 0x7113) => 0x40, // PIIX4 PM: PMBA
            _ => continue,
        };
        let base = (d.addr.read32(base_reg) & 0xFF80) as u16;
        if base != 0 {
            return Some(base + 4);
        }
    }
    None
}

fn shutdown_volumes(what: &str) {
    kprintln!("power: {what}: unmounting volumes");
    crate::vfs::shutdown();
}

/// Never returns on success.
pub fn request(reboot: bool) -> Result<u64, Errno> {
    if reboot {
        shutdown_volumes("reboot");
        kprintln!("power: resetting the machine");
        // SAFETY: the reset control register and the 8042 pulse reset the
        // CPU; nothing needs to survive.
        unsafe {
            cpu::outb(0xCF9, 0x02);
            cpu::outb(0xCF9, 0x06);
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
            cpu::outb(0x64, 0xFE);
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
            // Triple fault: an empty IDT and an interrupt.
            let empty: [u8; 10] = [0; 10];
            core::arch::asm!("lidt [{}]", "int3", in(reg) &empty, options(nostack));
        }
        cpu::halt_forever()
    }
    let Some(port) = pm1a_control() else {
        shutdown_volumes("power off");
        kprintln!("power: no known ACPI power management block; volumes are clean, power off by hand");
        return Err(Errno::NotSupported);
    };
    shutdown_volumes("power off");
    kprintln!("power: entering ACPI S5 through PM1a_CNT at {:#x}", port);
    // SAFETY: writing SLP_TYP|SLP_EN to PM1a_CNT powers the machine off.
    unsafe {
        let v = cpu_inw(port);
        cpu_outw(port, (v & !(7 << 10)) | (SLP_TYP_S5 << 10) | SLP_EN);
    }
    for _ in 0..10_000_000 {
        core::hint::spin_loop();
    }
    kprintln!("power: still running after the S5 request");
    Err(Errno::NotSupported)
}

/// # Safety
/// Port I/O on a power management register.
unsafe fn cpu_inw(port: u16) -> u16 {
    let v: u16;
    core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

/// # Safety
/// See [`cpu_inw`].
unsafe fn cpu_outw(port: u16, v: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags));
}
