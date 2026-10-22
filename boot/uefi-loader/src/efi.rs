//! Minimal hand-written UEFI 2.x bindings: exactly the tables, protocols and
//! services the OSyx loader uses. Layouts follow the UEFI 2.10 specification.

#![allow(dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Event = *mut c_void;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status(pub usize);

impl Status {
    const ERR: usize = 1 << (usize::BITS - 1);
    pub const SUCCESS: Status = Status(0);
    pub const LOAD_ERROR: Status = Status(Self::ERR | 1);
    pub const INVALID_PARAMETER: Status = Status(Self::ERR | 2);
    pub const UNSUPPORTED: Status = Status(Self::ERR | 3);
    pub const BUFFER_TOO_SMALL: Status = Status(Self::ERR | 5);
    pub const NOT_READY: Status = Status(Self::ERR | 6);
    pub const OUT_OF_RESOURCES: Status = Status(Self::ERR | 9);
    pub const NOT_FOUND: Status = Status(Self::ERR | 14);
    pub const ABORTED: Status = Status(Self::ERR | 21);

    pub fn is_error(self) -> bool {
        self.0 & Self::ERR != 0
    }

    pub fn ok(self) -> Result<(), Status> {
        if self.is_error() {
            Err(self)
        } else {
            Ok(())
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Status::SUCCESS => "EFI_SUCCESS",
            Status::LOAD_ERROR => "EFI_LOAD_ERROR",
            Status::INVALID_PARAMETER => "EFI_INVALID_PARAMETER",
            Status::UNSUPPORTED => "EFI_UNSUPPORTED",
            Status::BUFFER_TOO_SMALL => "EFI_BUFFER_TOO_SMALL",
            Status::NOT_READY => "EFI_NOT_READY",
            Status::OUT_OF_RESOURCES => "EFI_OUT_OF_RESOURCES",
            Status::NOT_FOUND => "EFI_NOT_FOUND",
            Status::ABORTED => "EFI_ABORTED",
            _ => "EFI error",
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const GOP_GUID: Guid = Guid(0x9042a9de, 0x23dc, 0x4a38, [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a]);
pub const LOADED_IMAGE_GUID: Guid = Guid(0x5b1b31a1, 0x9562, 0x11d2, [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
pub const SIMPLE_FS_GUID: Guid = Guid(0x964e5b22, 0x6459, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
pub const FILE_INFO_GUID: Guid = Guid(0x09576e92, 0x6d3f, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
pub const ACPI_20_GUID: Guid = Guid(0x8868e871, 0xe4f1, 0x11d3, [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81]);
pub const ACPI_10_GUID: Guid = Guid(0xeb9d2d30, 0x2d88, 0x11d3, [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d]);

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut SimpleTextInput,
    pub console_out_handle: Handle,
    pub con_out: *mut c_void,
    pub standard_error_handle: Handle,
    pub std_err: *mut c_void,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *const ConfigurationTable,
}

#[repr(C)]
pub struct ConfigurationTable {
    pub vendor_guid: Guid,
    pub vendor_table: *mut c_void,
}

#[repr(C)]
pub struct InputKey {
    pub scan_code: u16,
    pub unicode_char: u16,
}

#[repr(C)]
pub struct SimpleTextInput {
    pub reset: unsafe extern "efiapi" fn(*mut SimpleTextInput, bool) -> Status,
    pub read_key_stroke: unsafe extern "efiapi" fn(*mut SimpleTextInput, *mut InputKey) -> Status,
    pub wait_for_key: Event,
}

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const ALLOCATE_MAX_ADDRESS: u32 = 1;

/// UEFI memory types (UEFI 2.10, table 7.10).
pub mod memtype {
    pub const RESERVED: u32 = 0;
    pub const LOADER_CODE: u32 = 1;
    pub const LOADER_DATA: u32 = 2;
    pub const BOOT_SERVICES_CODE: u32 = 3;
    pub const BOOT_SERVICES_DATA: u32 = 4;
    pub const RUNTIME_SERVICES_CODE: u32 = 5;
    pub const RUNTIME_SERVICES_DATA: u32 = 6;
    pub const CONVENTIONAL: u32 = 7;
    pub const UNUSABLE: u32 = 8;
    pub const ACPI_RECLAIM: u32 = 9;
    pub const ACPI_NVS: u32 = 10;
    pub const MMIO: u32 = 11;
    pub const MMIO_PORT_SPACE: u32 = 12;
    pub const PAL_CODE: u32 = 13;
    pub const PERSISTENT: u32 = 14;
    // OS-defined types (0x8000_0000..=0xFFFF_FFFF) used by the OSyx loader.
    pub const OSYX_KERNEL: u32 = 0x8000_0001;
    pub const OSYX_BOOTINFO: u32 = 0x8000_0002;
    pub const OSYX_KERNEL_DATA: u32 = 0x8000_0003;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub ty: u32,
    pub _pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

type Unused = usize;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: Unused,
    pub restore_tpl: Unused,
    pub allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: unsafe extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map:
        unsafe extern "efiapi" fn(*mut usize, *mut MemoryDescriptor, *mut usize, *mut usize, *mut u32) -> Status,
    pub allocate_pool: unsafe extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: unsafe extern "efiapi" fn(*mut u8) -> Status,
    pub create_event: Unused,
    pub set_timer: Unused,
    pub wait_for_event: unsafe extern "efiapi" fn(usize, *const Event, *mut usize) -> Status,
    pub signal_event: Unused,
    pub close_event: Unused,
    pub check_event: Unused,
    pub install_protocol_interface: Unused,
    pub reinstall_protocol_interface: Unused,
    pub uninstall_protocol_interface: Unused,
    pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: Unused,
    pub register_protocol_notify: Unused,
    pub locate_handle: Unused,
    pub locate_device_path: Unused,
    pub install_configuration_table: Unused,
    pub load_image: Unused,
    pub start_image: Unused,
    pub exit: Unused,
    pub unload_image: Unused,
    pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: Unused,
    pub stall: unsafe extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: Unused,
    pub disconnect_controller: Unused,
    pub open_protocol: Unused,
    pub close_protocol: Unused,
    pub open_protocol_information: Unused,
    pub protocols_per_handle: Unused,
    pub locate_handle_buffer: Unused,
    pub locate_protocol: unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *mut c_void,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: Unused,
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut File) -> Status,
}

pub const FILE_MODE_READ: u64 = 1;

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub open: unsafe extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    pub close: unsafe extern "efiapi" fn(*mut File) -> Status,
    pub delete: Unused,
    pub read: unsafe extern "efiapi" fn(*mut File, *mut usize, *mut c_void) -> Status,
    pub write: Unused,
    pub get_position: Unused,
    pub set_position: Unused,
    pub get_info: unsafe extern "efiapi" fn(*mut File, *const Guid, *mut usize, *mut c_void) -> Status,
    pub set_info: Unused,
    pub flush: Unused,
}

/// Fixed-size prefix of EFI_FILE_INFO (the variable-length name follows).
#[repr(C)]
pub struct FileInfoHeader {
    pub size: u64,
    pub file_size: u64,
    pub physical_size: u64,
    pub times: [u8; 48],
    pub attribute: u64,
}

pub const PIXEL_RGB: u32 = 0;
pub const PIXEL_BGR: u32 = 1;
pub const PIXEL_BITMASK: u32 = 2;
pub const PIXEL_BLT_ONLY: u32 = 3;

#[repr(C)]
pub struct GopModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: u32,
    pub pixel_bitmask: [u32; 4],
    pub pixels_per_scan_line: u32,
}

#[repr(C)]
pub struct GopMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *const GopModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *const GopModeInfo) -> Status,
    pub set_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    pub blt: Unused,
    pub mode: *const GopMode,
}

/// Encodes an ASCII path into a NUL-terminated UCS-2 buffer.
pub fn ucs2<const N: usize>(s: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (i, b) in s.bytes().take(N - 1).enumerate() {
        out[i] = b as u16;
    }
    out
}
