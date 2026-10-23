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
    let list = loader::summary();
    unsafe {
        *needed = list.len() as u32 * 8;
    }
    for (i, (_, base, _, _, _)) in list.iter().take(bytes as usize / 8).enumerate() {
        unsafe {
            *modules.add(i) = *base;
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
pub fn kernel_exports() -> Vec<(&'static str, u64)> {
    export_table![K32EnumProcessModules, K32EnumProcessModulesEx]
}
pub fn exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("EnumProcessModules", K32EnumProcessModules as *const () as u64),
        ("EnumProcessModulesEx", K32EnumProcessModulesEx as *const () as u64)
    ]
}
