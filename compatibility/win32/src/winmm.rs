//! winmm's multimedia timer functions. timeGetTime is the monotonic clock
//! in milliseconds. The OSyx kernel ticks at a fixed 1 kHz
//! (kernel/src/arch/pic.rs, TIMER_HZ), so waits have 1 ms resolution:
//! timeGetDevCaps reports the 1 ms floor, timeBeginPeriod accepts periods
//! the kernel already provides and refuses finer ones with TIMERR_NOCANDO
//! instead of claiming a resolution OSyx does not have.
#![allow(non_snake_case)]
use alloc::{collections::BTreeMap, vec::Vec};
use os_std::sync::Mutex;

const TIMERR_NOERROR: u32 = 0;
const TIMERR_NOCANDO: u32 = 97;
/// The kernel tick, in milliseconds.
const PERIOD_MIN: u32 = 1;
const PERIOD_MAX: u32 = 1_000_000;

/// Outstanding timeBeginPeriod requests by period.
static PERIODS: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());

extern "win64" fn timeGetTime() -> u32 {
    (crate::kernel32::now_ns() / 1_000_000) as u32
}

extern "win64" fn timeBeginPeriod(period: u32) -> u32 {
    if !(PERIOD_MIN..=PERIOD_MAX).contains(&period) {
        return TIMERR_NOCANDO;
    }
    *PERIODS.lock().unwrap().entry(period).or_insert(0) += 1;
    TIMERR_NOERROR
}

extern "win64" fn timeEndPeriod(period: u32) -> u32 {
    let mut p = PERIODS.lock().unwrap();
    match p.get_mut(&period) {
        Some(n) => {
            *n -= 1;
            if *n == 0 {
                p.remove(&period);
            }
            TIMERR_NOERROR
        }
        None => TIMERR_NOCANDO, // no matching timeBeginPeriod
    }
}

/// TIMECAPS: wPeriodMin, wPeriodMax (two UINTs).
extern "win64" fn timeGetDevCaps(caps: *mut u32, size: u32) -> u32 {
    if caps.is_null() || size < 8 {
        return TIMERR_NOCANDO;
    }
    // SAFETY: the caller's TIMECAPS of `size` >= 8 bytes.
    unsafe {
        *caps = PERIOD_MIN;
        *caps.add(1) = PERIOD_MAX;
    }
    TIMERR_NOERROR
}

const SND_PURGE: u32 = 0x40;

/// OSyx has no sound output: stopping (no sound, or SND_PURGE) succeeds and
/// every sound fails to play, as when Windows cannot find or open it.
extern "win64" fn PlaySoundW(sound: *const u16, _module: u64, flags: u32) -> i32 {
    (sound.is_null() || flags & SND_PURGE != 0) as i32
}

// ---- waveform and MIDI devices ------------------------------------------------------------
//
// OSyx has neither sound output or input nor MIDI ports, so every device
// count is zero and no device id exists. The calls answer as winmm does for
// ids and handles it does not know (checked against Windows with bogus
// arguments): MMSYSERR_BADDEVICEID for an id, MMSYSERR_INVALHANDLE for a
// handle, MMSYSERR_INVALPARAM for a header of the wrong size.

const MMSYSERR_BADDEVICEID: u32 = 2;
const MMSYSERR_INVALHANDLE: u32 = 5;
const MMSYSERR_INVALPARAM: u32 = 11;
/// sizeof(WAVEHDR) on x64.
const WAVEHDR_SIZE: u32 = 48;

/// Number of waveform output devices.
extern "win64" fn waveOutGetNumDevs() -> u32 {
    0
}

/// Number of waveform input devices.
extern "win64" fn waveInGetNumDevs() -> u32 {
    0
}

/// Number of MIDI output devices.
extern "win64" fn midiOutGetNumDevs() -> u32 {
    0
}

/// Number of MIDI input devices.
extern "win64" fn midiInGetNumDevs() -> u32 {
    0
}

/// Every *GetDevCaps*: with no device, no id is valid.
extern "win64" fn waveOutGetDevCapsA(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn waveOutGetDevCapsW(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn waveInGetDevCapsA(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn waveInGetDevCapsW(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiOutGetDevCapsA(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiOutGetDevCapsW(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiInGetDevCapsA(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiInGetDevCapsW(_id: u64, _caps: *mut u8, _size: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}

/// Opening a device: the id names no device. Nothing is written to *handle.
extern "win64" fn waveOutOpen(_handle: *mut u64, _id: u32, _format: *const u8, _callback: u64, _instance: u64, _flags: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn waveInOpen(_handle: *mut u64, _id: u32, _format: *const u8, _callback: u64, _instance: u64, _flags: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiOutOpen(_handle: *mut u64, _id: u32, _callback: u64, _instance: u64, _flags: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiInOpen(_handle: *mut u64, _id: u32, _callback: u64, _instance: u64, _flags: u32) -> u32 {
    MMSYSERR_BADDEVICEID
}

/// Wave headers: the size is checked before the handle, as winmm does.
fn wave_header(header: *const u8, size: u32) -> u32 {
    if header.is_null() || size != WAVEHDR_SIZE {
        MMSYSERR_INVALPARAM
    } else {
        MMSYSERR_INVALHANDLE
    }
}
extern "win64" fn waveOutPrepareHeader(_h: u64, header: *const u8, size: u32) -> u32 {
    wave_header(header, size)
}
extern "win64" fn waveOutUnprepareHeader(_h: u64, header: *const u8, size: u32) -> u32 {
    wave_header(header, size)
}
extern "win64" fn waveOutWrite(_h: u64, header: *const u8, size: u32) -> u32 {
    wave_header(header, size)
}

/// Handle-only calls: no handle is ever valid.
extern "win64" fn waveOutClose(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn waveOutPause(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn waveOutRestart(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn waveOutReset(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn waveOutGetPosition(_h: u64, _time: *mut u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutClose(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutReset(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutShortMsg(_h: u64, _message: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutLongMsg(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutPrepareHeader(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiOutUnprepareHeader(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInClose(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInStart(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInStop(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInReset(_h: u64) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInAddBuffer(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInPrepareHeader(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}
extern "win64" fn midiInUnprepareHeader(_h: u64, _header: *const u8, _size: u32) -> u32 {
    MMSYSERR_INVALHANDLE
}

/// The driver message calls take a device id or a handle; neither names a
/// device here.
extern "win64" fn waveOutMessage(_h: u64, _message: u32, _a: u64, _b: u64) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn waveInMessage(_h: u64, _message: u32, _a: u64, _b: u64) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiOutMessage(_h: u64, _message: u32, _a: u64, _b: u64) -> u32 {
    MMSYSERR_BADDEVICEID
}
extern "win64" fn midiInMessage(_h: u64, _message: u32, _a: u64, _b: u64) -> u32 {
    MMSYSERR_BADDEVICEID
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        timeGetTime, timeBeginPeriod, timeEndPeriod, timeGetDevCaps, PlaySoundW,
        waveOutGetNumDevs, waveInGetNumDevs, midiOutGetNumDevs, midiInGetNumDevs,
        waveOutGetDevCapsA, waveOutGetDevCapsW, waveInGetDevCapsA, waveInGetDevCapsW,
        midiOutGetDevCapsA, midiOutGetDevCapsW, midiInGetDevCapsA, midiInGetDevCapsW,
        waveOutOpen, waveInOpen, midiOutOpen, midiInOpen,
        waveOutPrepareHeader, waveOutUnprepareHeader, waveOutWrite,
        waveOutClose, waveOutPause, waveOutRestart, waveOutReset, waveOutGetPosition,
        midiOutClose, midiOutReset, midiOutShortMsg, midiOutLongMsg, midiOutPrepareHeader, midiOutUnprepareHeader,
        midiInClose, midiInStart, midiInStop, midiInReset, midiInAddBuffer, midiInPrepareHeader, midiInUnprepareHeader,
        waveOutMessage, waveInMessage, midiOutMessage, midiInMessage
    ]
}
