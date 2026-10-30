//! Minimal PE fixture for the experimental content-process launch path.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> ! { loop {} }

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetCommandLineW() -> *const u16;
    fn CreateProcessW(app: *const u16, cmd: *mut u16, psa: usize, tsa: usize, inherit: i32, flags: u32, env: usize, cwd: usize, startup: *const u8, info: *mut u64) -> i32;
    fn WaitForSingleObject(handle: u64, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(handle: u64, code: *mut u32) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, security: usize, disposition: u32, flags: u32, template: u64) -> u64;
    fn CloseHandle(handle: u64) -> i32;
    fn ExitProcess(code: u32) -> !;
}

#[no_mangle]
pub extern "system" fn start() -> ! {
    unsafe {
        let mut p = GetCommandLineW();
        let mut child = false;
        while *p != 0 {
            if *p == b'-' as u16 { child = true; break; }
            p = p.add(1);
        }
        let mut secret = [0u16; 64];
        for (i, b) in b"C:\\tmp\\sandbox-winrun-secret".iter().enumerate() { secret[i] = *b as u16; }
        let file = CreateFileW(secret.as_ptr(), 0x8000_0000, 7, 0, 3, 0, 0);
        if child { ExitProcess(if file == u64::MAX { 0 } else { 14 }); }
        if file == u64::MAX { ExitProcess(13); }
        let _ = CloseHandle(file);
        let app: [u16; 20] = [67,58,92,111,112,116,92,122,101,110,92,122,101,110,46,101,120,101,0,0];
        let mut cmd: [u16; 35] = [67,58,92,111,112,116,92,122,101,110,92,122,101,110,46,101,120,101,32,45,99,111,110,116,101,110,116,112,114,111,99,0,0,0,0];
        let mut startup = [0u64; 13];
        startup[0] = 104;
        let mut info = [0u64; 3];
        if CreateProcessW(app.as_ptr(), cmd.as_mut_ptr(), 0, 0, 0, 0, 0, 0, startup.as_ptr() as *const u8, info.as_mut_ptr()) == 0 {
            ExitProcess(10);
        }
        if WaitForSingleObject(info[0], 20_000) != 0 { ExitProcess(11); }
        let mut code = 0;
        if GetExitCodeProcess(info[0], &mut code) == 0 || code != 0 { ExitProcess(12); }
        ExitProcess(0);
    }
}
