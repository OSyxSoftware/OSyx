//! Boot modules: the files the loader read from `\EFI\OSYX\BIN\`, copied
//! into kernel memory before bootloader memory is reclaimed. Programs run
//! from the root file system; the modules only matter when there is no
//! usable root partition: they are then installed into `/bin` of the
//! fallback tmpfs root (see `storage`).

use crate::kprintln;
use crate::sync::BootCell;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use osyx_bootinfo::BootInfo;

static MODULES: BootCell<Vec<(String, Vec<u8>)>> = BootCell::new();

/// Copies the module table and contents out of bootloader memory.
///
/// # Safety
/// Must run once, before bootloader memory is reclaimed; `info` must be the
/// validated BootInfo.
pub unsafe fn init(info: &BootInfo) -> usize {
    let mut list = Vec::new();
    for m in info.modules() {
        let name = m.name().to_string();
        let mut data = alloc::vec![0u8; m.size as usize];
        // SAFETY: the loader placed `size` bytes at this HHDM address.
        core::ptr::copy_nonoverlapping(m.addr as *const u8, data.as_mut_ptr(), data.len());
        kprintln!("module {}: {} bytes", name, data.len());
        list.push((name, data));
    }
    let n = list.len();
    MODULES.init(list);
    n
}

/// Every module as (lower-case file name, contents).
pub fn all() -> &'static [(String, Vec<u8>)] {
    MODULES.get().map_or(&[], |l| l.as_slice())
}
