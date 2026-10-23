//! Windows error codes and NTSTATUS values used by the runtime, and their
//! mapping from OSyx errors. Values are the documented public constants.

use osyx_abi::Errno;

pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
pub const ERROR_TOO_MANY_OPEN_FILES: u32 = 4;
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
pub const ERROR_NOT_SAME_DEVICE: u32 = 17;
pub const ERROR_WRITE_PROTECT: u32 = 19;
pub const ERROR_GEN_FAILURE: u32 = 31;
pub const ERROR_SHARING_VIOLATION: u32 = 32;
pub const ERROR_HANDLE_EOF: u32 = 38;
pub const ERROR_NOT_SUPPORTED: u32 = 50;
pub const ERROR_FILE_EXISTS: u32 = 80;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_DISK_FULL: u32 = 112;
pub const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub const ERROR_INVALID_NAME: u32 = 123;
pub const ERROR_MOD_NOT_FOUND: u32 = 126;
pub const ERROR_PROC_NOT_FOUND: u32 = 127;
pub const ERROR_DIR_NOT_EMPTY: u32 = 145;
pub const ERROR_ALREADY_EXISTS: u32 = 183;
pub const ERROR_ENVVAR_NOT_FOUND: u32 = 203;
pub const ERROR_FILENAME_EXCED_RANGE: u32 = 206;
pub const ERROR_LOCK_VIOLATION: u32 = 33;
pub const ERROR_NOT_LOCKED: u32 = 158;
pub const ERROR_MORE_DATA: u32 = 234;
pub const ERROR_INVALID_FUNCTION: u32 = 1;
pub const ERROR_NOT_FOUND: u32 = 1168;
pub const ERROR_REQUEST_ABORTED: u32 = 1235;
pub const ERROR_NOACCESS: u32 = 998;
pub const ERROR_IO_INCOMPLETE: u32 = 996;
pub const ERROR_NO_MORE_ITEMS: u32 = 259;
pub const ERROR_DIRECTORY: u32 = 267;
pub const ERROR_INVALID_ADDRESS: u32 = 487;
pub const ERROR_TIMEOUT: u32 = 1460;
pub const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
pub const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
pub const ERROR_CANNOT_FIND_WND_CLASS: u32 = 1407;
pub const ERROR_DLL_INIT_FAILED: u32 = 1114;

pub type NtStatus = u32;
pub const STATUS_SUCCESS: NtStatus = 0;
pub const STATUS_TIMEOUT: NtStatus = 0x0000_0102;
pub const STATUS_END_OF_FILE: NtStatus = 0xC000_0011;
pub const STATUS_NOT_IMPLEMENTED: NtStatus = 0xC000_0002;
pub const STATUS_INVALID_INFO_CLASS: NtStatus = 0xC000_0003;
pub const STATUS_INFO_LENGTH_MISMATCH: NtStatus = 0xC000_0004;
pub const STATUS_INVALID_HANDLE: NtStatus = 0xC000_0008;
pub const STATUS_INVALID_PARAMETER: NtStatus = 0xC000_000D;
pub const STATUS_NO_SUCH_FILE: NtStatus = 0xC000_000F;
pub const STATUS_NO_MEMORY: NtStatus = 0xC000_0017;
pub const STATUS_CONFLICTING_ADDRESSES: NtStatus = 0xC000_0018;
pub const STATUS_ACCESS_DENIED: NtStatus = 0xC000_0022;
pub const STATUS_OBJECT_NAME_INVALID: NtStatus = 0xC000_0033;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = 0xC000_0034;
pub const STATUS_OBJECT_NAME_COLLISION: NtStatus = 0xC000_0035;
pub const STATUS_OBJECT_PATH_NOT_FOUND: NtStatus = 0xC000_003A;
pub const STATUS_SHARING_VIOLATION: NtStatus = 0xC000_0043;
pub const STATUS_DISK_FULL: NtStatus = 0xC000_007F;
pub const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BB;
pub const STATUS_FILE_IS_A_DIRECTORY: NtStatus = 0xC000_00BA;
pub const STATUS_NOT_A_DIRECTORY: NtStatus = 0xC000_0103;
pub const STATUS_DIRECTORY_NOT_EMPTY: NtStatus = 0xC000_0101;
pub const STATUS_INVALID_PAGE_PROTECTION: NtStatus = 0xC000_0045;
pub const STATUS_MEMORY_NOT_ALLOCATED: NtStatus = 0xC000_00A0;
pub const STATUS_UNSUCCESSFUL: NtStatus = 0xC000_0001;
pub const STATUS_DLL_NOT_FOUND: NtStatus = 0xC000_0135;
pub const STATUS_ENTRYPOINT_NOT_FOUND: NtStatus = 0xC000_0139;
pub const STATUS_DLL_INIT_FAILED: NtStatus = 0xC000_0142;
pub const STATUS_INVALID_IMAGE_FORMAT: NtStatus = 0xC000_007B;

/// NTSTATUS → Win32 error (`RtlNtStatusToDosError`), for the statuses the
/// runtime produces.
pub fn status_to_win32(s: NtStatus) -> u32 {
    match s {
        STATUS_SUCCESS => ERROR_SUCCESS,
        STATUS_TIMEOUT => ERROR_TIMEOUT,
        STATUS_END_OF_FILE => ERROR_HANDLE_EOF,
        STATUS_NOT_IMPLEMENTED => ERROR_CALL_NOT_IMPLEMENTED,
        STATUS_INVALID_HANDLE => ERROR_INVALID_HANDLE,
        STATUS_INVALID_PARAMETER | STATUS_INVALID_INFO_CLASS | STATUS_INFO_LENGTH_MISMATCH => ERROR_INVALID_PARAMETER,
        STATUS_NO_SUCH_FILE | STATUS_OBJECT_NAME_NOT_FOUND => ERROR_FILE_NOT_FOUND,
        STATUS_OBJECT_PATH_NOT_FOUND => ERROR_PATH_NOT_FOUND,
        STATUS_OBJECT_NAME_INVALID => ERROR_INVALID_NAME,
        STATUS_OBJECT_NAME_COLLISION => ERROR_ALREADY_EXISTS,
        STATUS_NO_MEMORY => ERROR_NOT_ENOUGH_MEMORY,
        STATUS_CONFLICTING_ADDRESSES | STATUS_MEMORY_NOT_ALLOCATED => ERROR_INVALID_ADDRESS,
        STATUS_ACCESS_DENIED | STATUS_FILE_IS_A_DIRECTORY => ERROR_ACCESS_DENIED,
        STATUS_SHARING_VIOLATION => ERROR_SHARING_VIOLATION,
        STATUS_DISK_FULL => ERROR_DISK_FULL,
        STATUS_NOT_SUPPORTED => ERROR_NOT_SUPPORTED,
        STATUS_NOT_A_DIRECTORY => ERROR_DIRECTORY,
        STATUS_DIRECTORY_NOT_EMPTY => ERROR_DIR_NOT_EMPTY,
        STATUS_INVALID_PAGE_PROTECTION => ERROR_INVALID_PARAMETER,
        STATUS_DLL_NOT_FOUND => ERROR_MOD_NOT_FOUND,
        STATUS_ENTRYPOINT_NOT_FOUND => ERROR_PROC_NOT_FOUND,
        STATUS_DLL_INIT_FAILED => ERROR_DLL_INIT_FAILED,
        STATUS_INVALID_IMAGE_FORMAT => 193, // ERROR_BAD_EXE_FORMAT
        _ => ERROR_GEN_FAILURE,
    }
}

/// OSyx kernel error → NTSTATUS. `path` selects the file-name flavour of
/// "not found".
pub fn errno_to_status(e: Errno) -> NtStatus {
    match e {
        Errno::InvalidArgument => STATUS_INVALID_PARAMETER,
        Errno::Fault => STATUS_INVALID_PARAMETER,
        Errno::NoMemory => STATUS_NO_MEMORY,
        Errno::BadHandle => STATUS_INVALID_HANDLE,
        Errno::NotSupported => STATUS_NOT_SUPPORTED,
        Errno::PermissionDenied | Errno::ReadOnly => STATUS_ACCESS_DENIED,
        Errno::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        Errno::TimedOut => STATUS_TIMEOUT,
        Errno::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
        Errno::NotADirectory => STATUS_OBJECT_PATH_NOT_FOUND,
        Errno::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
        Errno::DirectoryNotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        Errno::NoSpace => STATUS_DISK_FULL,
        Errno::Busy => STATUS_SHARING_VIOLATION,
        _ => STATUS_UNSUCCESSFUL,
    }
}

pub fn errno_to_win32(e: Errno) -> u32 {
    match e {
        Errno::NotFound => ERROR_FILE_NOT_FOUND,
        Errno::NotADirectory => ERROR_PATH_NOT_FOUND,
        Errno::AlreadyExists => ERROR_ALREADY_EXISTS,
        Errno::TooManyHandles => ERROR_TOO_MANY_OPEN_FILES,
        Errno::CrossDevice => ERROR_NOT_SAME_DEVICE,
        Errno::ReadOnly => ERROR_WRITE_PROTECT,
        e => status_to_win32(errno_to_status(e)),
    }
}
