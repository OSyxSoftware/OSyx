//! Windows x64 floating-point control: the public FLOAT.H masks are
//! translated to MXCSR. Changes are per thread, saved by the scheduler.
use super::*;

const EM: u32 = 0x0008_001f;
const DENORMAL: u32 = 0x0008_0000;
const RC: u32 = 0x300;
const DN: u32 = 0x0300_0000;
const MAP: [(u32, u32); 6] = [(0x10, 0), (DENORMAL, 1), (8, 2), (4, 3), (2, 4), (1, 5)];

fn mxcsr() -> u32 {
    let mut v = 0u32;
    // SAFETY: writes four bytes to this local; SSE2 is mandatory on x64.
    unsafe { core::arch::asm!("stmxcsr [{p}]", p = in(reg) &mut v, options(nostack, preserves_flags)) };
    v
}

fn set_mxcsr(v: u32) {
    // SAFETY: callers preserve reserved bits and change only MXCSR fields.
    unsafe { core::arch::asm!("ldmxcsr [{p}]", p = in(reg) &v, options(nostack, preserves_flags)) };
}

fn control(v: u32) -> u32 {
    let mut c = (v >> 5) & RC;
    for (public, bit) in MAP {
        if v & (1 << (bit + 7)) != 0 {
            c |= public;
        }
    }
    // _DN_FLUSH, _DN_FLUSH_OPERANDS_SAVE_RESULTS, _DN_SAVE_OPERANDS_FLUSH_RESULTS.
    c | match (v & (1 << 6) != 0, v & (1 << 15) != 0) {
        (true, true) => 0x0100_0000,
        (true, false) => 0x0200_0000,
        (false, true) => 0x0300_0000,
        _ => 0,
    }
}

fn update(new: u32, mask: u32) -> u32 {
    let mut v = mxcsr();
    let c = (control(v) & !mask) | (new & mask);
    v = (v & !(3 << 13)) | ((c & RC) << 5);
    for (public, bit) in MAP {
        if mask & public != 0 {
            v = (v & !(1 << (bit + 7))) | (u32::from(c & public != 0) << (bit + 7));
        }
    }
    if mask & DN != 0 {
        v &= !((1 << 6) | (1 << 15));
        v |= match c & DN {
            0x0100_0000 => (1 << 6) | (1 << 15),
            0x0200_0000 => 1 << 6,
            0x0300_0000 => 1 << 15,
            _ => 0,
        };
    }
    set_mxcsr(v);
    control(v)
}

extern "win64" fn _control87(new: u32, mask: u32) -> u32 {
    update(new, mask & (EM | RC | DN))
}

extern "win64" fn _controlfp(new: u32, mask: u32) -> u32 {
    _control87(new, mask & !DENORMAL)
}

extern "win64" fn _controlfp_s(out: *mut u32, new: u32, mask: u32) -> i32 {
    // The Windows release CRT accepts (and ignores) x87 precision and
    // infinity bits on x64; genuinely unknown bits still fail validation.
    if new & mask & !(EM | RC | DN | 0x0007_0000) != 0 {
        if !out.is_null() {
            // SAFETY: optional output belongs to the caller.
            unsafe { *out = control(mxcsr()) };
        }
        return invalid(EINVAL, EINVAL);
    }
    let c = _controlfp(new, mask);
    if !out.is_null() {
        // SAFETY: optional output belongs to the caller.
        unsafe { *out = c };
    }
    0
}

extern "win64" fn fegetround() -> i32 {
    ((mxcsr() >> 5) & RC) as i32
}

extern "win64" fn fesetround(mode: i32) -> i32 {
    if mode < 0 || mode as u32 & !RC != 0 {
        return 1;
    }
    update(mode as u32, RC);
    0
}

extern "win64" fn _statusfp() -> u32 {
    let v = mxcsr();
    MAP.iter().fold(0, |r, &(public, bit)| r | if v & (1 << bit) != 0 { public } else { 0 })
}

extern "win64" fn _clearfp() -> u32 {
    let old = _statusfp();
    set_mxcsr(mxcsr() & !0x3f);
    old
}

extern "win64" fn _fpreset() {
    set_mxcsr(0x1f80);
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![_control87, _controlfp, _controlfp_s, fegetround, fesetround, _statusfp, _clearfp, _fpreset]
}
