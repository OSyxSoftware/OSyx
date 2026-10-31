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
    fn CreateEventW(attributes: usize, manual: i32, initial: i32, name: *const u16) -> u64;
    fn SetHandleInformation(handle: u64, mask: u32, flags: u32) -> i32;
    fn ResumeThread(handle: u64) -> u32;
    fn GetStdHandle(which: u32) -> u64;
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
        if child && *p.add(1) == b'h' as u16 { ExitProcess(7); }
        let mut secret = [0u16; 64];
        for (i, b) in b"C:\\tmp\\sandbox-winrun-secret".iter().enumerate() { *secret.as_mut_ptr().add(i) = *b as u16; }
        let file = CreateFileW(secret.as_ptr(), 0x8000_0000, 7, 0, 3, 0, 0);
        if child {
            if file != u64::MAX { ExitProcess(14); }
            while *p != 0 && *p != b' ' as u16 { p = p.add(1); }
            if *p == 0 { ExitProcess(15); }
            p = p.add(1);
            let mut event = 0u64;
            while *p >= b'0' as u16 && *p <= b'9' as u16 {
                event = event.wrapping_mul(10).wrapping_add((*p - b'0' as u16) as u64);
                p = p.add(1);
            }
            ExitProcess(if event != 0 && WaitForSingleObject(event, 0) == 0 { 0 } else { 16 });
        }
        if file == u64::MAX { ExitProcess(13); }
        let _ = CloseHandle(file);
        let app: [u16; 20] = [67,58,92,111,112,116,92,122,101,110,92,122,101,110,46,101,120,101,0,0];
        let mut held_cmd = [0u16; 64];
        for (i, b) in b"C:\\opt\\zen\\zen.exe -holdchild".iter().enumerate() { *held_cmd.as_mut_ptr().add(i) = *b as u16; }
        let mut held_startup = [0u64; 13];
        held_startup[0] = 104;
        let mut held_info = [0u64; 3];
        if CreateProcessW(app.as_ptr(), held_cmd.as_mut_ptr(), 0, 0, 0, 4, 0, 0, held_startup.as_ptr() as *const u8, held_info.as_mut_ptr()) == 0 { ExitProcess(18); }
        if WaitForSingleObject(held_info[0], 0) != 258 { ExitProcess(19); }
        if ResumeThread(held_info[1]) != 1 { ExitProcess(20); }
        if WaitForSingleObject(held_info[0], 20_000) != 0 { ExitProcess(21); }
        let mut held_code = 0;
        if GetExitCodeProcess(held_info[0], &mut held_code) == 0 || held_code != 7 { ExitProcess(22); }
        let _ = CloseHandle(held_info[0]);
        let _ = CloseHandle(held_info[1]);
        let event = CreateEventW(0, 1, 1, core::ptr::null());
        if event == 0 || SetHandleInformation(event, 1, 1) == 0 { ExitProcess(17); }
        let mut cmd = [0u16; 64];
        let prefix = b"C:\\opt\\zen\\zen.exe -contentproc ";
        for (i, b) in prefix.iter().enumerate() { *cmd.as_mut_ptr().add(i) = *b as u16; }
        let mut digits = [0u16; 20];
        let mut value = event;
        let mut count = 0;
        while value > 0 { *digits.as_mut_ptr().add(count) = b'0' as u16 + (value % 10) as u16; value /= 10; count += 1; }
        for i in 0..count { *cmd.as_mut_ptr().add(prefix.len() + i) = *digits.as_ptr().add(count - i - 1); }
        let mut startup = [0u64; 13];
        startup[0] = 104;
        (startup.as_mut_ptr() as *mut u8).add(60).cast::<u32>().write_unaligned(0x100);
        startup[10] = GetStdHandle((-10i32) as u32);
        startup[11] = GetStdHandle((-11i32) as u32);
        startup[12] = GetStdHandle((-12i32) as u32);
        let mut info = [0u64; 3];
        let stdin = startup[10];
        startup[10] = event;
        if CreateProcessW(app.as_ptr(), cmd.as_mut_ptr(), 0, 0, 1, 4, 0, 0, startup.as_ptr() as *const u8, info.as_mut_ptr()) != 0 { ExitProcess(24); }
        startup[10] = stdin;
        if CreateProcessW(app.as_ptr(), cmd.as_mut_ptr(), 0, 0, 1, 4, 0, 0, startup.as_ptr() as *const u8, info.as_mut_ptr()) == 0 {
            ExitProcess(10);
        }
        if WaitForSingleObject(info[0], 0) != 258 || ResumeThread(info[1]) != 1 { ExitProcess(23); }
        if WaitForSingleObject(info[0], 20_000) != 0 { ExitProcess(11); }
        let mut code = 0;
        if GetExitCodeProcess(info[0], &mut code) == 0 || code != 0 { ExitProcess(12); }
        ExitProcess(0);
    }
}
