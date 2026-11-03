//! Module enumeration of the current Windows process, using real loader
//! handles. Remote process memory/querying is not supported.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, loader, objects::CURRENT_PROCESS};
use alloc::vec::Vec;
extern "win64" fn K32EnumProcessModules(process: u64, modules: *mut u64, bytes: u32, needed: *mut u32) -> i32 {
    if process != CURRENT_PROCESS {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    if needed.is_null() || (modules.is_null() && bytes != 0) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // HMODULEs are pointer-sized: 4 bytes each for a 32-bit caller.
    let width = if crate::wow64::is_active() { 4 } else { 8 };
    let list = loader::summary();
    // SAFETY: the caller's out pointers; `bytes` bounds the array.
    unsafe {
        *needed = list.len() as u32 * width;
    }
    for (i, (_, base, _, _, _)) in list.iter().take((bytes / width) as usize).enumerate() {
        unsafe {
            if width == 4 {
                core::ptr::write_unaligned((modules as *mut u32).add(i), *base as u32);
            } else {
                *modules.add(i) = *base;
            }
        }
    }
    1
}
extern "win64" fn K32EnumProcessModulesEx(
    process: u64,
    modules: *mut u64,
    bytes: u32,
    needed: *mut u32,
    filter: u32,
) -> i32 {
    if filter != 0 && filter != 2 && filter != 3 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    K32EnumProcessModules(process, modules, bytes, needed)
}
/// K32EnumProcesses: the ids of every process in the kernel's process
/// table, the System Idle Process (0) first as on Windows.
extern "win64" fn K32EnumProcesses(ids: *mut u32, bytes: u32, needed: *mut u32) -> i32 {
    if needed.is_null() || (ids.is_null() && bytes != 0) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut info = alloc::vec![osyx_abi::ipc::ProcInfo::default(); 512];
    let n = os_sys::proc_list(&mut info).unwrap_or(0);
    let list: Vec<u32> = core::iter::once(0).chain(info[..n].iter().map(|p| p.pid as u32)).collect();
    let fit = list.len().min(bytes as usize / 4);
    // SAFETY: the caller's array of `bytes` bytes and out count.
    unsafe {
        core::ptr::copy_nonoverlapping(list.as_ptr(), ids, fit);
        *needed = (fit * 4) as u32;
    }
    1
}

pub fn kernel_exports() -> Vec<(&'static str, u64)> {
    export_table![K32EnumProcessModules, K32EnumProcessModulesEx, K32EnumProcesses]
}
pub fn exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("EnumProcessModules", K32EnumProcessModules as *const () as u64),
        ("EnumProcessModulesEx", K32EnumProcessModulesEx as *const () as u64),
        ("EnumProcesses", K32EnumProcesses as *const () as u64)
    ]
}
