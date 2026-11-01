//! Read-only SetupAPI inventory. No device installation or registry emulation.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{fail, out_w},
    teb,
};
use alloc::{collections::BTreeMap, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::{sync::Mutex, system};
use osyx_abi::ipc::DeviceInfo;

static SETS: Mutex<BTreeMap<u64, Vec<DeviceInfo>>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x4000_0000);

/// SP_DEVINFO_DATA / SP_DEVICE_INTERFACE_DATA end with a ULONG_PTR: 28
/// bytes for a 32-bit (WoW64) caller, 32 for a 64-bit one.
fn data_size() -> u32 {
    if crate::wow64::is_active() { 28 } else { 32 }
}

/// SP_DEVINFO_DATA.Reserved (at 24): the device's index in its set.
///
/// # Safety
/// `data` must be the caller's SP_DEVINFO_DATA.
unsafe fn reserved(data: u64) -> u64 {
    // SAFETY: as documented.
    unsafe { if crate::wow64::is_active() { teb::get::<u32>(data, 24) as u64 } else { teb::get::<u64>(data, 24) } }
}

/// # Safety
/// As [`reserved`].
unsafe fn set_reserved(data: u64, v: u64) {
    // SAFETY: as documented.
    unsafe {
        if crate::wow64::is_active() {
            teb::put(data, 24, v as u32)
        } else {
            teb::put(data, 24, v)
        }
    }
}

pub(crate) extern "win64" fn SetupDiGetClassDevsW(guid: u64, enumerator: u64, _parent: u64, flags: u32) -> u64 {
    if guid != 0 || enumerator != 0 || flags & !6 != 0 || flags & 4 == 0 {
        return fail(ERROR_NOT_SUPPORTED, u64::MAX);
    }
    match system::devices() {
        Ok(list) => {
            let h = NEXT.fetch_add(4, Ordering::Relaxed);
            SETS.lock().unwrap().insert(h, list);
            h
        }
        Err(_) => fail(ERROR_GEN_FAILURE, u64::MAX),
    }
}

fn class_id(class: &str) -> u32 {
    match class {
        "Display" => 0x4d36e968,
        "Keyboard" => 0x4d36e96b,
        "Mouse" => 0x4d36e96f,
        "Net" => 0x4d36e972,
        "USB" => 0x36fc9e60,
        "Storage" => 0x4d36e97b,
        _ => 0x4d36e97d,
    }
}

extern "win64" fn SetupDiEnumDeviceInfo(set: u64, index: u32, data: u64) -> i32 {
    if data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let sets = SETS.lock().unwrap();
    let Some(list) = sets.get(&set) else {
        return fail(ERROR_INVALID_HANDLE, 0);
    };
    let Some(d) = list.get(index as usize) else {
        return fail(ERROR_NO_MORE_ITEMS, 0);
    };
    let usb = system::field_str(&d.class) == "USB";
    // SP_DEVINFO_DATA's GUID, DevInst, and private index.
    unsafe {
        core::ptr::write_bytes((data + 4) as *mut u8, 0, data_size() as usize - 4);
        teb::put(data, 4, class_id(system::field_str(&d.class)));
        teb::put(data, 8, if usb { 0xc465u16 } else { 0xe325u16 });
        teb::put(data, 10, if usb { 0x11cfu16 } else { 0x11ceu16 });
        let tail: [u8; 8] =
            if usb { [0x80, 0x56, 0x44, 0x45, 0x53, 0x54, 0, 0] } else { [0xbf, 0xc1, 8, 0, 0x2b, 0xe1, 3, 0x18] };
        core::ptr::copy_nonoverlapping(tail.as_ptr(), (data + 12) as *mut u8, 8);
        teb::put(data, 20, index + 1);
        set_reserved(data, index as u64);
    }
    1
}

pub(crate) extern "win64" fn SetupDiGetDeviceRegistryPropertyW(
    set: u64,
    data: u64,
    prop: u32,
    kind: *mut u32,
    buf: *mut u16,
    bytes: u32,
    required: *mut u32,
) -> i32 {
    if data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let sets = SETS.lock().unwrap();
    let Some(list) = sets.get(&set) else {
        return fail(ERROR_INVALID_HANDLE, 0);
    };
    let index = unsafe { reserved(data) } as usize;
    let Some(d) = list.get(index) else {
        return fail(ERROR_INVALID_PARAMETER, 0);
    };
    let text = match prop {
        0 | 12 => system::field_str(&d.description),
        1 => system::field_str(&d.hardware_id),
        7 => system::field_str(&d.class),
        13 => system::field_str(&d.location),
        _ => return fail(ERROR_NOT_SUPPORTED, 0),
    };
    let n = text.encode_utf16().count() + if prop == 1 { 2 } else { 1 };
    unsafe {
        if !required.is_null() {
            *required = n as u32 * 2;
        }
        if !kind.is_null() {
            *kind = if prop == 1 { 7 } else { 1 };
        }
    }
    if buf.is_null() || bytes < (n * 2) as u32 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    out_w(text, buf, bytes / 2);
    if prop == 1 {
        unsafe {
            *buf.add(n - 1) = 0;
        }
    }
    1
}

extern "win64" fn SetupDiDestroyDeviceInfoList(set: u64) -> i32 {
    if SETS.lock().unwrap().remove(&set).is_some() {
        1
    } else {
        fail(ERROR_INVALID_HANDLE, 0)
    }
}

extern "win64" fn SetupDiEnumDeviceInterfaces(set: u64, _dev: u64, guid: u64, _index: u32, data: u64) -> i32 {
    if guid == 0 || data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !SETS.lock().unwrap().contains_key(&set) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    fail(ERROR_NO_MORE_ITEMS, 0)
}

extern "win64" fn SetupDiGetDeviceInterfaceDetailW(
    set: u64,
    iface_data: u64,
    _detail: u64,
    _detail_size: u32,
    _required: *mut u32,
    _dev_data: u64,
) -> i32 {
    if iface_data == 0 || unsafe { teb::get::<u32>(iface_data, 0) } != data_size() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !SETS.lock().unwrap().contains_key(&set) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    fail(ERROR_NO_MORE_ITEMS, 0)
}

/// SetupDiGetDeviceInterfaceDetailA: as the Unicode form (no interface is
/// ever enumerated, see SetupDiEnumDeviceInterfaces).
extern "win64" fn SetupDiGetDeviceInterfaceDetailA(set: u64, iface_data: u64, detail: u64, detail_size: u32, required: *mut u32, dev_data: u64) -> i32 {
    SetupDiGetDeviceInterfaceDetailW(set, iface_data, detail, detail_size, required, dev_data)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn SetupDiGetDevicePropertyW(
    set: u64,
    data: u64,
    key: u64,
    prop_type: *mut u32,
    _buf: *mut u8,
    _size: u32,
    _required: *mut u32,
    flags: u32,
) -> i32 {
    if data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() || key == 0 || prop_type.is_null() || flags != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let sets = SETS.lock().unwrap();
    let Some(list) = sets.get(&set) else {
        return fail(ERROR_INVALID_HANDLE, 0);
    };
    let index = unsafe { reserved(data) } as usize;
    if index >= list.len() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    fail(ERROR_NOT_FOUND, 0)
}

extern "win64" fn SetupDiOpenDevRegKey(
    set: u64,
    data: u64,
    _scope: u32,
    _hw_profile: u32,
    _key_type: u32,
    _sam: u32,
) -> u64 {
    if data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    if !SETS.lock().unwrap().contains_key(&set) {
        return fail(ERROR_INVALID_HANDLE, u64::MAX);
    }
    fail(ERROR_FILE_NOT_FOUND, u64::MAX)
}

/// SetupDiGetClassDevsA: the enumerator name in UTF-16.
extern "win64" fn SetupDiGetClassDevsA(guid: u64, enumerator: *const u8, parent: u64, flags: u32) -> u64 {
    // SAFETY: NULL or a NUL-terminated name.
    let e = unsafe { crate::path::ansi_to_wide(enumerator) };
    SetupDiGetClassDevsW(guid, e.as_ref().map_or(0, |e| e.as_ptr() as u64), parent, flags)
}

/// SetupDiGetDeviceRegistryPropertyA: string properties in ANSI (sizes in
/// bytes, the multi-string's double NUL kept).
#[allow(clippy::too_many_arguments)]
extern "win64" fn SetupDiGetDeviceRegistryPropertyA(
    set: u64,
    data: u64,
    prop: u32,
    kind: *mut u32,
    buf: *mut u8,
    bytes: u32,
    required: *mut u32,
) -> i32 {
    let mut need = 0u32;
    let mut t = 0u32;
    let first = SetupDiGetDeviceRegistryPropertyW(set, data, prop, &mut t, core::ptr::null_mut(), 0, &mut need);
    if first == 0 && teb::last_error() != ERROR_INSUFFICIENT_BUFFER {
        return 0;
    }
    let mut w = alloc::vec![0u16; need as usize / 2];
    if SetupDiGetDeviceRegistryPropertyW(set, data, prop, &mut t, w.as_mut_ptr(), need, &mut need) == 0 {
        return 0;
    }
    let units = &w[..need as usize / 2];
    let a: Vec<u8> = alloc::string::String::from_utf16_lossy(units).into_bytes();
    // SAFETY: NULL or the caller's out values and buffer.
    unsafe {
        if !kind.is_null() {
            *kind = t;
        }
        if !required.is_null() {
            *required = a.len() as u32;
        }
        if buf.is_null() || bytes < a.len() as u32 {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::copy_nonoverlapping(a.as_ptr(), buf, a.len());
    }
    1
}

/// The device instance ID of a device: its hardware ID and its position
/// among the devices ("PCI\VEN_8086&DEV_100E\1").
fn instance_id(set: u64, data: u64) -> Result<alloc::string::String, u32> {
    if data == 0 || unsafe { teb::get::<u32>(data, 0) } != data_size() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let sets = SETS.lock().unwrap();
    let list = sets.get(&set).ok_or(ERROR_INVALID_HANDLE)?;
    // SAFETY: the SP_DEVINFO_DATA of the size just checked.
    let index = unsafe { reserved(data) } as usize;
    let d = list.get(index).ok_or(ERROR_INVALID_PARAMETER)?;
    Ok(alloc::format!("{}\\{}", system::field_str(&d.hardware_id).to_uppercase(), index + 1))
}

extern "win64" fn SetupDiGetDeviceInstanceIdW(set: u64, data: u64, out: *mut u16, size: u32, required: *mut u32) -> i32 {
    let id = match instance_id(set, data) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    let n = id.encode_utf16().count() as u32 + 1;
    if !required.is_null() {
        // SAFETY: the caller's out value.
        unsafe { *required = n };
    }
    if out.is_null() || size < n {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    out_w(&id, out, size);
    1
}

extern "win64" fn SetupDiGetDeviceInstanceIdA(set: u64, data: u64, out: *mut u8, size: u32, required: *mut u32) -> i32 {
    let id = match instance_id(set, data) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    let n = id.len() as u32 + 1;
    if !required.is_null() {
        // SAFETY: the caller's out value.
        unsafe { *required = n };
    }
    if out.is_null() || size < n {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    crate::kernel32::out_a(&id, out, size);
    1
}

/// The setup classes Windows defines, by name: (name, GUID).
const CLASSES: &[(&str, [u8; 16])] = &[
    ("Display", guid(0x4d36e968, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Keyboard", guid(0x4d36e96b, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Mouse", guid(0x4d36e96f, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Net", guid(0x4d36e972, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Media", guid(0x4d36e96c, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("DiskDrive", guid(0x4d36e967, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Ports", guid(0x4d36e978, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("System", guid(0x4d36e97d, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("Monitor", guid(0x4d36e96e, 0xe325, 0x11ce, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18])),
    ("USB", guid(0x36fc9e60, 0xc465, 0x11cf, [0x80, 0x56, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00])),
    ("HIDClass", guid(0x745a17a0, 0x74d3, 0x11d0, [0xb6, 0xfe, 0x00, 0xa0, 0xc9, 0x0f, 0x57, 0xda])),
    ("Bluetooth", guid(0xe0cbf06c, 0xcd8b, 0x4647, [0xbb, 0x8a, 0x26, 0x3b, 0x43, 0xf0, 0xf9, 0x74])),
    ("AudioEndpoint", guid(0xc166523c, 0xfe0c, 0x4a94, [0xa5, 0x86, 0xf1, 0xa8, 0x0c, 0xfb, 0xbf, 0x3e])),
];

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// SetupDiClassGuidsFromNameA/W: the GUIDs of the setup classes with that
/// name (one at most here); `required` is how many there are.
fn class_guids(name: Option<alloc::string::String>, out: *mut u8, size: u32, required: *mut u32) -> i32 {
    let Some(name) = name else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let found: Vec<&[u8; 16]> = CLASSES.iter().filter(|c| c.0.eq_ignore_ascii_case(&name)).map(|c| &c.1).collect();
    if !required.is_null() {
        // SAFETY: the caller's out value.
        unsafe { *required = found.len() as u32 };
    }
    if (size as usize) < found.len() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    for (i, g) in found.iter().enumerate() {
        // SAFETY: the caller's array of `size` GUIDs.
        unsafe { core::ptr::copy_nonoverlapping(g.as_ptr(), out.add(16 * i), 16) };
    }
    1
}

extern "win64" fn SetupDiClassGuidsFromNameA(name: *const u8, out: *mut u8, size: u32, required: *mut u32) -> i32 {
    // SAFETY: a NUL-terminated name.
    class_guids(unsafe { crate::path::ansi_to_string(name) }, out, size, required)
}

extern "win64" fn SetupDiClassGuidsFromNameW(name: *const u16, out: *mut u8, size: u32, required: *mut u32) -> i32 {
    // SAFETY: a NUL-terminated name.
    class_guids(unsafe { crate::path::wide_to_string(name) }, out, size, required)
}

/// CM_Get_DevNode_Status: a device node (the DevInst of SP_DEVINFO_DATA,
/// 1 + its index among the devices) is started with its driver loaded
/// (DN_DRIVER_LOADED | DN_STARTED) and has no problem. Unknown nodes are
/// CR_NO_SUCH_DEVNODE.
extern "win64" fn CM_Get_DevNode_Status(status: *mut u32, problem: *mut u32, devinst: u32, flags: u32) -> u32 {
    const CR_SUCCESS: u32 = 0;
    const CR_INVALID_POINTER: u32 = 0x3;
    const CR_INVALID_FLAG: u32 = 0x4;
    const CR_NO_SUCH_DEVNODE: u32 = 0xD;
    if status.is_null() || problem.is_null() {
        return CR_INVALID_POINTER;
    }
    if flags != 0 {
        return CR_INVALID_FLAG;
    }
    let count = system::devices().map_or(0, |l| l.len());
    if devinst == 0 || devinst as usize > count {
        return CR_NO_SUCH_DEVNODE;
    }
    // SAFETY: the caller's out values.
    unsafe {
        *status = 0x2 | 0x8;
        *problem = 0;
    }
    CR_SUCCESS
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        SetupDiGetClassDevsA,
        SetupDiGetDeviceInterfaceDetailA,
        SetupDiGetDeviceRegistryPropertyA,
        SetupDiGetDeviceInstanceIdW,
        SetupDiGetDeviceInstanceIdA,
        SetupDiClassGuidsFromNameA,
        SetupDiClassGuidsFromNameW,
        CM_Get_DevNode_Status,
        SetupDiGetClassDevsW,
        SetupDiEnumDeviceInfo,
        SetupDiEnumDeviceInterfaces,
        SetupDiGetDeviceInterfaceDetailW,
        SetupDiGetDeviceRegistryPropertyW,
        SetupDiGetDevicePropertyW,
        SetupDiOpenDevRegKey,
        SetupDiDestroyDeviceInfoList
    ]
}
