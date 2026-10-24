//! dwmapi (the Desktop Window Manager's per-window attributes and frame
//! timing), imm32 and wtsapi32.
//!
//! PRISM composes every window, so composition is always on. Attributes a
//! program sets are kept per window and read back where Windows reports
//! them (DWMWA_CLOAKED); PRISM does not draw caption colours, rounded
//! corners or backdrops, and cloaking does not hide a window. Windows have
//! no invisible resize borders, so the extended frame is the window
//! rectangle. Frame timing is a 60 Hz clock on the performance counter;
//! DwmFlush waits for its next tick. Taskbar thumbnails are accepted and
//! never shown.
//!
//! OSyx has no input method editors: threads have no input context
//! (ImmGetContext returns NULL, as for a thread with IMM disabled) and
//! keyboard layouts are not IMEs. There are no terminal-services sessions
//! to switch, so a session notification is registered and never sent.
//!
//! There is no GPU: d3d11 creates no device (DXGI_ERROR_UNSUPPORTED, as
//! for a driver type the machine lacks), dxgi makes no factory and the
//! shader compiler compiles nothing, so programs take their software paths.
#![allow(non_snake_case)]
use crate::error::*;
use crate::kernel32::{fail, Bool, FALSE, TRUE};
use crate::teb;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use os_std::sync::Mutex;

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_HANDLE: u32 = 0x8007_0006;
/// What Windows 10.0.26300 returns for a DWM_TIMING_INFO of the wrong size.
const DWM_E_BAD_TIMING_SIZE: u32 = 0x8898_0090;

const DWMWA_NCRENDERING_ENABLED: u32 = 1;
const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const DWMWA_CLOAK: u32 = 13;
const DWMWA_CLOAKED: u32 = 14;
const DWMWA_VISIBLE_FRAME_BORDER_THICKNESS: u32 = 37;
const DWM_CLOAKED_APP: u32 = 1;
/// Attributes a program can set (each a DWORD or BOOL).
const SETTABLE: &[u32] = &[2, 3, 4, 6, 7, 8, 10, 11, 12, DWMWA_CLOAK, 15, 16, 17, 19, 20, 33, 34, 35, 36, 38];

/// (HWND, attribute) → the value last set.
static ATTRS: Mutex<BTreeMap<(u64, u32), u32>> = Mutex::new(BTreeMap::new());

/// One refresh of the composition clock, in performance-counter units (ns).
const REFRESH_NS: u64 = 1_000_000_000 / 60;

extern "win64" fn DwmIsCompositionEnabled(out: *mut Bool) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's BOOL.
    unsafe { *out = TRUE };
    S_OK
}

extern "win64" fn DwmSetWindowAttribute(hwnd: u64, attr: u32, value: *const u8, size: u32) -> u32 {
    if !crate::user32::is_window(hwnd) {
        return E_HANDLE;
    }
    if !SETTABLE.contains(&attr) || value.is_null() || size != 4 {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's 4-byte value.
    let v = unsafe { teb::get::<u32>(value as u64, 0) };
    let mut attrs = ATTRS.lock().unwrap();
    attrs.retain(|&(w, _), _| crate::user32::is_window(w));
    attrs.insert((hwnd, attr), v);
    S_OK
}

extern "win64" fn DwmGetWindowAttribute(hwnd: u64, attr: u32, out: *mut u8, size: u32) -> u32 {
    if !crate::user32::is_window(hwnd) {
        return E_HANDLE;
    }
    if out.is_null() {
        return E_INVALIDARG;
    }
    let dword = |v: u32| {
        if size != 4 {
            return E_INVALIDARG;
        }
        // SAFETY: the caller's 4-byte buffer.
        unsafe { teb::put(out as u64, 0, v) };
        S_OK
    };
    match attr {
        DWMWA_NCRENDERING_ENABLED => dword(1),
        DWMWA_CLOAKED => {
            let cloaked = ATTRS.lock().unwrap().get(&(hwnd, DWMWA_CLOAK)).is_some_and(|&v| v != 0);
            dword(if cloaked { DWM_CLOAKED_APP } else { 0 })
        }
        DWMWA_VISIBLE_FRAME_BORDER_THICKNESS => dword(0),
        DWMWA_EXTENDED_FRAME_BOUNDS => {
            if size != 16 {
                return E_INVALIDARG;
            }
            match crate::user32::window_rect(hwnd) {
                Some(r) => {
                    // SAFETY: the caller's RECT.
                    unsafe { teb::put(out as u64, 0, r) };
                    S_OK
                }
                None => E_HANDLE,
            }
        }
        _ => E_INVALIDARG,
    }
}

/// The frame already is the whole client area: nothing to extend.
extern "win64" fn DwmExtendFrameIntoClientArea(hwnd: u64, _margins: *const i32) -> u32 {
    if crate::user32::is_window(hwnd) { S_OK } else { E_INVALIDARG }
}

/// Waits for the next tick of the composition clock.
extern "win64" fn DwmFlush() -> u32 {
    let now = crate::kernel32::now_ns();
    let next = (now / REFRESH_NS + 1) * REFRESH_NS;
    os_std::thread::sleep(core::time::Duration::from_nanos(next - now));
    S_OK
}

/// DWM_TIMING_INFO (292 bytes, packed): the refresh rate and period, the
/// last vertical blank and the refresh count; frame statistics are zero.
extern "win64" fn DwmGetCompositionTimingInfo(hwnd: u64, info: *mut u8) -> u32 {
    if hwnd != 0 || info.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's structure starts with its size.
    let size = unsafe { teb::get::<u32>(info as u64, 0) };
    if size != 292 {
        return DWM_E_BAD_TIMING_SIZE;
    }
    let now = crate::kernel32::now_ns();
    let count = now / REFRESH_NS;
    let vblank = count * REFRESH_NS;
    // SAFETY: the caller's 292-byte DWM_TIMING_INFO.
    unsafe {
        core::ptr::write_bytes(info.add(4), 0, 288);
        let p = info as u64;
        teb::put(p, 4, 60u32); // rateRefresh
        teb::put(p, 8, 1u32);
        teb::put(p, 12, REFRESH_NS); // qpcRefreshPeriod
        teb::put(p, 20, 60u32); // rateCompose
        teb::put(p, 24, 1u32);
        teb::put(p, 28, vblank); // qpcVBlank
        teb::put(p, 36, count); // cRefresh
        teb::put(p, 48, vblank + REFRESH_NS); // qpcCompose
    }
    S_OK
}

/// Taskbar thumbnails and peek bitmaps: accepted for a window, never shown.
extern "win64" fn DwmSetIconicThumbnail(hwnd: u64, _bitmap: u64, _flags: u32) -> u32 {
    if crate::user32::is_window(hwnd) { S_OK } else { E_INVALIDARG }
}

extern "win64" fn DwmSetIconicLivePreviewBitmap(hwnd: u64, _bitmap: u64, _offset: *const i32, _flags: u32) -> u32 {
    if crate::user32::is_window(hwnd) { S_OK } else { E_INVALIDARG }
}

extern "win64" fn DwmInvalidateIconicBitmaps(hwnd: u64) -> u32 {
    if crate::user32::is_window(hwnd) { S_OK } else { E_INVALIDARG }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        DwmIsCompositionEnabled,
        DwmSetWindowAttribute,
        DwmGetWindowAttribute,
        DwmExtendFrameIntoClientArea,
        DwmFlush,
        DwmGetCompositionTimingInfo,
        DwmSetIconicThumbnail,
        DwmSetIconicLivePreviewBitmap,
        DwmInvalidateIconicBitmaps,
    ]
}

// ---- imm32 ----------------------------------------------------------------------------------

extern "win64" fn ImmGetContext(_hwnd: u64) -> u64 {
    0 // no input context
}

extern "win64" fn ImmReleaseContext(_hwnd: u64, _himc: u64) -> Bool {
    TRUE
}

extern "win64" fn ImmAssociateContextEx(hwnd: u64, _himc: u64, _flags: u32) -> Bool {
    crate::user32::is_window(hwnd) as Bool
}

extern "win64" fn ImmGetDefaultIMEWnd(_hwnd: u64) -> u64 {
    0
}

/// No layout is an IME: no properties, description or IME file.
extern "win64" fn ImmGetProperty(_hkl: u64, _index: u32) -> u32 {
    0
}

extern "win64" fn ImmGetDescriptionW(_hkl: u64, _buf: *mut u16, _len: u32) -> u32 {
    0
}

extern "win64" fn ImmGetIMEFileNameW(_hkl: u64, _buf: *mut u16, _len: u32) -> u32 {
    0
}

/// Every remaining call takes an input context, and there never is one.
extern "win64" fn ImmGetOpenStatus(_himc: u64) -> Bool {
    FALSE
}

extern "win64" fn ImmSetOpenStatus(_himc: u64, _open: Bool) -> Bool {
    FALSE
}

extern "win64" fn ImmNotifyIME(_himc: u64, _action: u32, _index: u32, _value: u32) -> Bool {
    FALSE
}

extern "win64" fn ImmGetCompositionStringW(_himc: u64, _index: u32, _buf: *mut u8, _len: u32) -> i32 {
    0
}

extern "win64" fn ImmGetCompositionStringA(_himc: u64, _index: u32, _buf: *mut u8, _len: u32) -> i32 {
    0
}

extern "win64" fn ImmSetCompositionWindow(_himc: u64, _form: *const u8) -> Bool {
    FALSE
}

extern "win64" fn ImmSetCandidateWindow(_himc: u64, _form: *const u8) -> Bool {
    FALSE
}

extern "win64" fn ImmGetCompositionFontW(_himc: u64, _font: *mut u8) -> Bool {
    FALSE
}

extern "win64" fn ImmSetCompositionFontW(_himc: u64, _font: *const u8) -> Bool {
    FALSE
}

pub fn imm32_exports() -> Vec<(&'static str, u64)> {
    export_table![
        ImmGetContext,
        ImmReleaseContext,
        ImmAssociateContextEx,
        ImmGetDefaultIMEWnd,
        ImmGetProperty,
        ImmGetDescriptionW,
        ImmGetIMEFileNameW,
        ImmGetOpenStatus,
        ImmSetOpenStatus,
        ImmNotifyIME,
        ImmGetCompositionStringW,
        ImmGetCompositionStringA,
        ImmSetCompositionWindow,
        ImmSetCandidateWindow,
        ImmGetCompositionFontW,
        ImmSetCompositionFontW,
    ]
}

// ---- d3d11, dxgi, d3dcompiler ----------------------------------------------------------------

const DXGI_ERROR_UNSUPPORTED: u32 = 0x887A_0004;
const E_FAIL: u32 = 0x8000_4005;

fn clear_out(p: *mut u64) {
    if !p.is_null() {
        // SAFETY: the caller's interface out pointer.
        unsafe { *p = 0 };
    }
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn D3D11CreateDevice(
    _adapter: u64,
    _driver: u32,
    _software: u64,
    _flags: u32,
    _levels: *const u32,
    _n: u32,
    _sdk: u32,
    device: *mut u64,
    level: *mut u32,
    context: *mut u64,
) -> u32 {
    clear_out(device);
    clear_out(context);
    if !level.is_null() {
        // SAFETY: the caller's D3D_FEATURE_LEVEL.
        unsafe { *level = 0 };
    }
    DXGI_ERROR_UNSUPPORTED
}

extern "win64" fn CreateDXGIFactory1(_riid: *const u8, out: *mut u64) -> u32 {
    clear_out(out);
    DXGI_ERROR_UNSUPPORTED
}

extern "win64" fn CreateDXGIFactory(riid: *const u8, out: *mut u64) -> u32 {
    CreateDXGIFactory1(riid, out)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn D3DCompile(
    _src: *const u8,
    _len: usize,
    _name: *const u8,
    _defines: *const u8,
    _include: u64,
    _entry: *const u8,
    _target: *const u8,
    _flags1: u32,
    _flags2: u32,
    code: *mut u64,
    errors: *mut u64,
) -> u32 {
    clear_out(code);
    clear_out(errors);
    E_FAIL
}

pub fn d3d11_exports() -> Vec<(&'static str, u64)> {
    export_table![D3D11CreateDevice]
}

pub fn dxgi_exports() -> Vec<(&'static str, u64)> {
    export_table![CreateDXGIFactory1, CreateDXGIFactory]
}

pub fn d3dcompiler_exports() -> Vec<(&'static str, u64)> {
    export_table![D3DCompile]
}

// ---- wtsapi32 -------------------------------------------------------------------------------

extern "win64" fn WTSRegisterSessionNotification(hwnd: u64, _flags: u32) -> Bool {
    if crate::user32::is_window(hwnd) { TRUE } else { fail(ERROR_INVALID_PARAMETER, FALSE) }
}

extern "win64" fn WTSUnRegisterSessionNotification(hwnd: u64) -> Bool {
    if crate::user32::is_window(hwnd) { TRUE } else { fail(ERROR_INVALID_PARAMETER, FALSE) }
}

pub fn wtsapi32_exports() -> Vec<(&'static str, u64)> {
    export_table![WTSRegisterSessionNotification, WTSUnRegisterSessionNotification]
}
