//! The mixer API over the kernel mixer.
//!
//! One mixer device exists when a sound card can play. It has one
//! destination line, the speakers (line 0xFFFF0000, the card's wave-out
//! target), with a mute switch (control 1) and a master volume (control 2,
//! 0 to 65535) that read /run/audio.status and write /dev/mixer, the same
//! state waveOutGetVolume and the desktop's volume control use. Layouts are
//! the packed (pshpack1) mmsystem structures.
use super::*;
use alloc::vec;

const MIXERR_INVALLINE: u32 = 1024;
const MIXERR_INVALCONTROL: u32 = 1025;
const MIXERR_INVALVALUE: u32 = 1026;

const DEST_LINE: u32 = 0xFFFF_0000;
const MUTE_ID: u32 = 1;
const VOLUME_ID: u32 = 2;
const MIXERLINE_COMPONENTTYPE_DST_SPEAKERS: u32 = 4;
const MIXERLINE_TARGETTYPE_WAVEOUT: u32 = 1;
const MIXERLINE_LINEF_ACTIVE: u32 = 1;
const MIXERCONTROL_CONTROLTYPE_MUTE: u32 = 0x2001_0002;
const MIXERCONTROL_CONTROLTYPE_VOLUME: u32 = 0x5003_0001;
const MIXERCONTROL_CONTROLF_UNIFORM: u32 = 1;

/// MIXER_OBJECTF_* in the top nibble of the flags.
const OBJECTF_HANDLE: u32 = 0x8000_0000;
const OBJECTF_TYPEMASK: u32 = 0x7000_0000;
const OBJECTF_MIXER: u32 = 0;
const OBJECTF_WAVEOUT: u32 = 0x1000_0000;
const OBJECTF_WAVEIN: u32 = 0x2000_0000;

static OPEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(0x4d00_0010);

fn present() -> bool {
    !wave_devices("audio").is_empty()
}

fn card_name() -> String {
    wave_device(0, "audio").map(|(_, n)| n).unwrap_or_default()
}

/// The mixer an HMIXEROBJ names: Ok(()) for mixer 0.
fn resolve(obj: u64, flags: u32) -> Result<(), u32> {
    if !present() {
        return Err(MMSYSERR_NODRIVER);
    }
    let handle = flags & OBJECTF_HANDLE != 0;
    match flags & OBJECTF_TYPEMASK {
        OBJECTF_MIXER if handle => {
            if OPEN.lock().unwrap().contains(&obj) { Ok(()) } else { Err(MMSYSERR_INVALHANDLE) }
        }
        OBJECTF_MIXER | OBJECTF_WAVEOUT if !handle => {
            if obj == 0 { Ok(()) } else { Err(MMSYSERR_BADDEVICEID) }
        }
        OBJECTF_WAVEOUT => {
            if wave_out(obj).is_some() { Ok(()) } else { Err(MMSYSERR_INVALHANDLE) }
        }
        // Recording has no mixer line of its own.
        OBJECTF_WAVEIN => Err(MMSYSERR_NODRIVER),
        _ => Err(MMSYSERR_INVALFLAG),
    }
}

fn status_field(k: &str) -> Option<u32> {
    let text = os_std::fs::read_to_string("/run/audio.status").unwrap_or_default();
    text.lines().find_map(|l| l.strip_prefix(k).map(str::trim)).and_then(|v| v.parse().ok())
}

fn write_mixer(line: &str) -> u32 {
    use os_std::io::Write;
    match os_std::fs::OpenOptions::new().write(true).open("/dev/mixer") {
        Ok(mut f) => match f.write_all(line.as_bytes()) {
            Ok(()) => MMSYSERR_NOERROR,
            Err(_) => MMSYSERR_NODRIVER,
        },
        Err(_) => MMSYSERR_NODRIVER,
    }
}

/// Text into a fixed field: ANSI bytes or UTF-16, truncated, NUL-ended.
fn put_text(b: &mut [u8], at: usize, chars: usize, s: &str, wide: bool) {
    if wide {
        for (i, c) in s.encode_utf16().take(chars - 1).enumerate() {
            b[at + 2 * i..at + 2 * i + 2].copy_from_slice(&c.to_le_bytes());
        }
    } else {
        for (i, c) in s.bytes().take(chars - 1).enumerate() {
            b[at + i] = c;
        }
    }
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn rd32(p: *const u8, at: usize) -> u32 {
    // SAFETY: a field of the caller's structure.
    unsafe { core::ptr::read_unaligned(p.add(at) as *const u32) }
}

fn copy_out(b: &[u8], out: *mut u8, cap: usize) {
    // SAFETY: the caller's structure of `cap` bytes.
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), out, b.len().min(cap)) };
}

extern "win64" fn mixerGetNumDevs() -> u32 {
    present() as u32
}

/// MIXERCAPSA (48 bytes) / MIXERCAPSW (80 bytes).
fn caps(id: u64, out: *mut u8, size: u32, wide: bool) -> u32 {
    if id != 0 && !OPEN.lock().unwrap().contains(&id) || !present() {
        return MMSYSERR_BADDEVICEID;
    }
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    let mut b = vec![0u8; if wide { 80 } else { 48 }];
    put_text(&mut b, 8, 32, &card_name(), wide);
    let tail = if wide { 72 } else { 40 };
    put32(&mut b, tail + 4, 1); // cDestinations
    copy_out(&b, out, size as usize);
    MMSYSERR_NOERROR
}

extern "win64" fn mixerGetDevCapsA(id: u64, out: *mut u8, size: u32) -> u32 {
    caps(id, out, size, false)
}

extern "win64" fn mixerGetDevCapsW(id: u64, out: *mut u8, size: u32) -> u32 {
    caps(id, out, size, true)
}

extern "win64" fn mixerOpen(out: *mut u64, id: u32, _callback: u64, _instance: u64, flags: u32) -> u32 {
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    if let Err(e) = resolve(id as u64, flags & (OBJECTF_HANDLE | OBJECTF_TYPEMASK)) {
        return e;
    }
    let h = NEXT.fetch_add(0x10, Ordering::Relaxed);
    OPEN.lock().unwrap().push(h);
    // SAFETY: the caller's out pointer.
    unsafe { *out = h };
    MMSYSERR_NOERROR
}

extern "win64" fn mixerClose(h: u64) -> u32 {
    let mut open = OPEN.lock().unwrap();
    match open.iter().position(|&x| x == h) {
        Some(i) => {
            open.remove(i);
            MMSYSERR_NOERROR
        }
        None => MMSYSERR_INVALHANDLE,
    }
}

extern "win64" fn mixerGetID(obj: u64, id: *mut u32, flags: u32) -> u32 {
    if id.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    match resolve(obj, flags) {
        Ok(()) => {
            // SAFETY: the caller's out pointer.
            unsafe { *id = 0 };
            MMSYSERR_NOERROR
        }
        Err(e) => e,
    }
}

/// MIXERLINEA (172 bytes) / MIXERLINEW (284 bytes) of the destination.
fn line_info(obj: u64, line: *mut u8, flags: u32, wide: bool) -> u32 {
    if let Err(e) = resolve(obj, flags & 0xF000_0000) {
        return e;
    }
    let size = if wide { 284 } else { 172 };
    if line.is_null() || (rd32(line, 0) as usize) < size {
        return MMSYSERR_INVALPARAM;
    }
    let found = match flags & 0xF {
        0 => rd32(line, 4) == 0,                                              // DESTINATION
        1 => return MIXERR_INVALLINE,                                         // SOURCE: none
        2 => rd32(line, 12) == DEST_LINE,                                     // LINEID
        3 => rd32(line, 28) == MIXERLINE_COMPONENTTYPE_DST_SPEAKERS,          // COMPONENTTYPE
        4 => rd32(line, if wide { 204 } else { 124 }) == MIXERLINE_TARGETTYPE_WAVEOUT, // TARGETTYPE
        _ => return MMSYSERR_INVALFLAG,
    };
    if !found {
        return MIXERR_INVALLINE;
    }
    let mut b = vec![0u8; size];
    put32(&mut b, 0, size as u32);
    put32(&mut b, 12, DEST_LINE);
    put32(&mut b, 16, MIXERLINE_LINEF_ACTIVE);
    put32(&mut b, 28, MIXERLINE_COMPONENTTYPE_DST_SPEAKERS);
    put32(&mut b, 32, 1); // cChannels
    put32(&mut b, 40, 2); // cControls
    put_text(&mut b, 44, 16, "Volume", wide);
    put_text(&mut b, if wide { 76 } else { 60 }, 64, "Master Volume", wide);
    let target = if wide { 204 } else { 124 };
    put32(&mut b, target, MIXERLINE_TARGETTYPE_WAVEOUT);
    put_text(&mut b, target + 16, 32, &card_name(), wide);
    copy_out(&b, line, size);
    MMSYSERR_NOERROR
}

extern "win64" fn mixerGetLineInfoA(obj: u64, line: *mut u8, flags: u32) -> u32 {
    line_info(obj, line, flags, false)
}

extern "win64" fn mixerGetLineInfoW(obj: u64, line: *mut u8, flags: u32) -> u32 {
    line_info(obj, line, flags, true)
}

/// MIXERCONTROLA (148 bytes) / MIXERCONTROLW (228 bytes).
fn control(id: u32, wide: bool) -> Vec<u8> {
    let size = if wide { 228 } else { 148 };
    let (bounds, metrics) = if wide { (180, 204) } else { (100, 124) };
    let mut b = vec![0u8; size];
    put32(&mut b, 0, size as u32);
    put32(&mut b, 4, id);
    put32(&mut b, 12, MIXERCONTROL_CONTROLF_UNIFORM);
    let name = if id == MUTE_ID { "Mute" } else { "Volume" };
    put_text(&mut b, 20, 16, name, wide);
    put_text(&mut b, if wide { 52 } else { 36 }, 64, name, wide);
    if id == MUTE_ID {
        put32(&mut b, 8, MIXERCONTROL_CONTROLTYPE_MUTE);
        put32(&mut b, bounds + 4, 1);
    } else {
        put32(&mut b, 8, MIXERCONTROL_CONTROLTYPE_VOLUME);
        put32(&mut b, bounds + 4, 0xFFFF);
        put32(&mut b, metrics, 100); // the kernel mixer's steps (percent)
    }
    b
}

/// MIXERLINECONTROLS (28 bytes): all controls, one by id or one by type.
fn line_controls(obj: u64, lc: *mut u8, flags: u32, wide: bool) -> u32 {
    if let Err(e) = resolve(obj, flags & 0xF000_0000) {
        return e;
    }
    if lc.is_null() || rd32(lc, 0) < 28 {
        return MMSYSERR_INVALPARAM;
    }
    let size = if wide { 228 } else { 148 };
    // SAFETY: pamxctrl, a pointer at offset 20 of the packed structure.
    let array = unsafe { core::ptr::read_unaligned(lc.add(20) as *const u64) } as *mut u8;
    let (count, each) = (rd32(lc, 12), rd32(lc, 16) as usize);
    if array.is_null() || each < size {
        return MMSYSERR_INVALPARAM;
    }
    let ids: Vec<u32> = match flags & 0xF {
        0 => {
            if rd32(lc, 4) != DEST_LINE {
                return MIXERR_INVALLINE;
            }
            if count < 2 {
                return MMSYSERR_INVALPARAM;
            }
            vec![MUTE_ID, VOLUME_ID]
        }
        1 => match rd32(lc, 8) {
            id @ (MUTE_ID | VOLUME_ID) => vec![id],
            _ => return MIXERR_INVALCONTROL,
        },
        2 => {
            if rd32(lc, 4) != DEST_LINE {
                return MIXERR_INVALLINE;
            }
            match rd32(lc, 8) {
                MIXERCONTROL_CONTROLTYPE_MUTE => vec![MUTE_ID],
                MIXERCONTROL_CONTROLTYPE_VOLUME => vec![VOLUME_ID],
                _ => return MIXERR_INVALCONTROL,
            }
        }
        _ => return MMSYSERR_INVALFLAG,
    };
    for (i, &id) in ids.iter().enumerate() {
        // SAFETY: the caller's array of `count` controls of `each` bytes.
        copy_out(&control(id, wide), unsafe { array.add(i * each) }, each);
    }
    if flags & 0xF == 1 {
        // ONEBYID reports the control's line.
        // SAFETY: the caller's MIXERLINECONTROLS.
        unsafe { core::ptr::write_unaligned(lc.add(4) as *mut u32, DEST_LINE) };
    }
    MMSYSERR_NOERROR
}

extern "win64" fn mixerGetLineControlsA(obj: u64, lc: *mut u8, flags: u32) -> u32 {
    line_controls(obj, lc, flags, false)
}

extern "win64" fn mixerGetLineControlsW(obj: u64, lc: *mut u8, flags: u32) -> u32 {
    line_controls(obj, lc, flags, true)
}

/// MIXERCONTROLDETAILS (32 bytes): the details array and its count.
fn details(cd: *mut u8) -> Result<(u32, *mut u32, usize), u32> {
    if cd.is_null() || rd32(cd, 0) < 32 {
        return Err(MMSYSERR_INVALPARAM);
    }
    let id = rd32(cd, 4);
    if id != MUTE_ID && id != VOLUME_ID {
        return Err(MIXERR_INVALCONTROL);
    }
    // SAFETY: paDetails, a pointer at offset 24 of the packed structure.
    let array = unsafe { core::ptr::read_unaligned(cd.add(24) as *const u64) } as *mut u32;
    let channels = rd32(cd, 8).max(1) as usize;
    if array.is_null() || rd32(cd, 20) < 4 {
        return Err(MMSYSERR_INVALPARAM);
    }
    Ok((id, array, channels))
}

fn get_details(obj: u64, cd: *mut u8, flags: u32) -> u32 {
    if let Err(e) = resolve(obj, flags & 0xF000_0000) {
        return e;
    }
    if flags & 0xF != 0 {
        // LISTTEXT: neither control is a list.
        return MIXERR_INVALCONTROL;
    }
    let (id, array, channels) = match details(cd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let v = if id == MUTE_ID {
        status_field("mute=").unwrap_or(0).min(1)
    } else {
        status_field("volume=").unwrap_or(0).min(100) * 0xFFFF / 100
    };
    for c in 0..channels {
        // SAFETY: the caller's MIXERCONTROLDETAILS_UNSIGNED/BOOLEAN array.
        unsafe { *array.add(c) = v };
    }
    MMSYSERR_NOERROR
}

extern "win64" fn mixerGetControlDetailsA(obj: u64, cd: *mut u8, flags: u32) -> u32 {
    get_details(obj, cd, flags)
}

extern "win64" fn mixerGetControlDetailsW(obj: u64, cd: *mut u8, flags: u32) -> u32 {
    get_details(obj, cd, flags)
}

extern "win64" fn mixerSetControlDetails(obj: u64, cd: *mut u8, flags: u32) -> u32 {
    if let Err(e) = resolve(obj, flags & 0xF000_0000) {
        return e;
    }
    if flags & 0xF != 0 {
        return MMSYSERR_INVALFLAG; // only MIXER_SETCONTROLDETAILSF_VALUE
    }
    let (id, array, _) = match details(cd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // A uniform control: the first channel's value.
    // SAFETY: the caller's details array.
    let v = unsafe { *array };
    if id == MUTE_ID {
        if v > 1 {
            return MIXERR_INVALVALUE;
        }
        write_mixer(&alloc::format!("mute={v}\n"))
    } else {
        if v > 0xFFFF {
            return MIXERR_INVALVALUE;
        }
        write_mixer(&alloc::format!("volume={}\n", (v * 100 + 0x7FFF) / 0xFFFF))
    }
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        mixerGetNumDevs,
        mixerGetDevCapsA,
        mixerGetDevCapsW,
        mixerOpen,
        mixerClose,
        mixerGetID,
        mixerGetLineInfoA,
        mixerGetLineInfoW,
        mixerGetLineControlsA,
        mixerGetLineControlsW,
        mixerGetControlDetailsA,
        mixerGetControlDetailsW,
        mixerSetControlDetails,
    ]
}
