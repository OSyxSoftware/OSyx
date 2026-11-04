//! atlthunk.dll: the window procedure thunks ATL (since Windows 10) takes
//! from the system instead of writing code into its own memory. A thunk
//! called as a window procedure replaces its first argument (the HWND)
//! with a value of the program's choice, its window object, and jumps to
//! the program's procedure.
//!
//! Thunks come in pools of two pages: a code page, executable and never
//! written after it is set up, and the data page after it, which holds
//! each thunk's (first argument, procedure). Thunk `i` is 16 bytes of code
//! at `code + 16 * i` that loads its data from `code + 4096 + 16 * i`:
//!
//! ```text
//! mov rcx, [rip + 0xFF9]    ; 48 8B 0D F9 0F 00 00: the first argument
//! jmp qword [rip + 0xFFB]   ; FF 25 FB 0F 00 00: the procedure
//! int3 x3                   ; padding
//! ```
//!
//! The program's AtlThunkData_t pointer is the data slot; its code is 4096
//! bytes before it.
#![allow(non_snake_case)]
use crate::mem;
use alloc::vec::Vec;
use os_std::sync::Mutex;

const PAGE: u64 = 4096;
const SLOT: u64 = 16;
const CODE: [u8; 16] = [0x48, 0x8B, 0x0D, 0xF9, 0x0F, 0x00, 0x00, 0xFF, 0x25, 0xFB, 0x0F, 0x00, 0x00, 0xCC, 0xCC, 0xCC];

/// Free data slots of every pool.
static FREE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A new pool: its code page written and made executable, its data slots
/// added to the free list. False without memory.
fn grow(free: &mut Vec<u64>) -> bool {
    let Some(base) = mem::alloc_pages(2 * PAGE as usize, mem::Prot::ReadWrite) else { return false };
    // SAFETY: the fresh, writable code page.
    unsafe {
        for i in 0..PAGE / SLOT {
            core::ptr::copy_nonoverlapping(CODE.as_ptr(), (base + i * SLOT) as *mut u8, CODE.len());
        }
    }
    if mem::protect(base, PAGE as usize, mem::Prot::ExecuteRead).is_err() {
        mem::free_pages(base, 2 * PAGE as usize);
        return false;
    }
    // Handed out from the lowest address up.
    free.extend((0..PAGE / SLOT).rev().map(|i| base + PAGE + i * SLOT));
    true
}

/// AtlThunk_AllocateData: a thunk, or NULL without memory.
extern "win64" fn AtlThunk_AllocateData() -> u64 {
    let mut free = FREE.lock().unwrap();
    if free.is_empty() && !grow(&mut free) {
        return 0;
    }
    free.pop().unwrap_or(0)
}

/// AtlThunk_InitData: the thunk calls `proc` with `first` in place of the
/// first argument.
extern "win64" fn AtlThunk_InitData(thunk: u64, proc: u64, first: u64) {
    if thunk == 0 {
        return;
    }
    // SAFETY: a data slot AtlThunk_AllocateData handed out (16 writable bytes).
    unsafe {
        core::ptr::write(thunk as *mut u64, first);
        core::ptr::write((thunk + 8) as *mut u64, proc);
    }
}

/// AtlThunk_DataToCode: the thunk's code, to call as a window procedure.
extern "win64" fn AtlThunk_DataToCode(thunk: u64) -> u64 {
    if thunk == 0 { 0 } else { thunk - PAGE }
}

/// AtlThunk_FreeData: the thunk can be handed out again.
extern "win64" fn AtlThunk_FreeData(thunk: u64) {
    if thunk == 0 {
        return;
    }
    AtlThunk_InitData(thunk, 0, 0);
    FREE.lock().unwrap().push(thunk);
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![AtlThunk_AllocateData, AtlThunk_InitData, AtlThunk_DataToCode, AtlThunk_FreeData]
}
