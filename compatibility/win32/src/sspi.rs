//! SSPI (`secur32.dll` / `sspicli.dll`): the security-support provider
//! interface and its two function tables.
//!
//! Credentials and contexts come from security packages; OSyx registers
//! none yet, so the package queries answer as Windows does for a package
//! that is not installed (SEC_E_SECPKG_NOT_FOUND) and every call on a
//! credential or context handle finds no such handle
//! (SEC_E_INVALID_HANDLE). Buffers SSPI hands out are freed with
//! FreeContextBuffer.
#![allow(non_snake_case)]

use alloc::vec::Vec;

type SecStatus = u32;
const SEC_E_OK: SecStatus = 0;
const SEC_E_INVALID_HANDLE: SecStatus = 0x8009_0301;
const SEC_E_SECPKG_NOT_FOUND: SecStatus = 0x8009_0305;
const SEC_E_INSUFFICIENT_MEMORY: SecStatus = 0x8009_0300;
const SEC_E_INVALID_TOKEN: SecStatus = 0x8009_0308;

/// The registered packages (none): EnumerateSecurityPackages returns an
/// empty array the caller frees with FreeContextBuffer.
fn enumerate(count: *mut u32, info: *mut u64) -> SecStatus {
    if count.is_null() || info.is_null() {
        return SEC_E_INVALID_TOKEN;
    }
    let Some(p) = crate::kernel32::heap_alloc(8, true) else { return SEC_E_INSUFFICIENT_MEMORY };
    // SAFETY: the caller's out pointers.
    unsafe {
        *count = 0;
        *info = p;
    }
    SEC_E_OK
}

extern "win64" fn EnumerateSecurityPackagesW(count: *mut u32, info: *mut u64) -> SecStatus {
    enumerate(count, info)
}

extern "win64" fn EnumerateSecurityPackagesA(count: *mut u32, info: *mut u64) -> SecStatus {
    enumerate(count, info)
}

fn not_found(info: *mut u64) -> SecStatus {
    if !info.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *info = 0 };
    }
    SEC_E_SECPKG_NOT_FOUND
}

extern "win64" fn QuerySecurityPackageInfoW(_name: *const u16, info: *mut u64) -> SecStatus {
    not_found(info)
}

extern "win64" fn QuerySecurityPackageInfoA(_name: *const u8, info: *mut u64) -> SecStatus {
    not_found(info)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn AcquireCredentialsHandleW(
    _principal: *const u16,
    _package: *const u16,
    _usage: u32,
    _logon_id: u64,
    _auth_data: u64,
    _get_key: u64,
    _get_key_arg: u64,
    _credential: *mut [u64; 2],
    _expiry: *mut u64,
) -> SecStatus {
    SEC_E_SECPKG_NOT_FOUND
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn AcquireCredentialsHandleA(
    _principal: *const u8,
    _package: *const u8,
    _usage: u32,
    _logon_id: u64,
    _auth_data: u64,
    _get_key: u64,
    _get_key_arg: u64,
    _credential: *mut [u64; 2],
    _expiry: *mut u64,
) -> SecStatus {
    SEC_E_SECPKG_NOT_FOUND
}

/// Calls on a credential or context handle: no package made one.
extern "win64" fn FreeCredentialsHandle(_credential: *mut [u64; 2]) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn InitializeSecurityContextW(
    _credential: *mut [u64; 2],
    _context: *mut [u64; 2],
    _target: *const u16,
    _req: u32,
    _reserved1: u32,
    _data_rep: u32,
    _input: u64,
    _reserved2: u32,
    _new_context: *mut [u64; 2],
    _output: u64,
    _attrs: *mut u32,
    _expiry: *mut u64,
) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn InitializeSecurityContextA(
    _credential: *mut [u64; 2],
    _context: *mut [u64; 2],
    _target: *const u8,
    _req: u32,
    _reserved1: u32,
    _data_rep: u32,
    _input: u64,
    _reserved2: u32,
    _new_context: *mut [u64; 2],
    _output: u64,
    _attrs: *mut u32,
    _expiry: *mut u64,
) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn AcceptSecurityContext(
    _credential: *mut [u64; 2],
    _context: *mut [u64; 2],
    _input: u64,
    _req: u32,
    _data_rep: u32,
    _new_context: *mut [u64; 2],
    _output: u64,
    _attrs: *mut u32,
    _expiry: *mut u64,
) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn CompleteAuthToken(_context: *mut [u64; 2], _token: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn DeleteSecurityContext(_context: *mut [u64; 2]) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn ApplyControlToken(_context: *mut [u64; 2], _input: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn QueryContextAttributesW(_context: *mut [u64; 2], _attr: u32, _buffer: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn QueryContextAttributesA(_context: *mut [u64; 2], _attr: u32, _buffer: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn SetContextAttributesW(_context: *mut [u64; 2], _attr: u32, _buffer: u64, _size: u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn SetContextAttributesA(_context: *mut [u64; 2], _attr: u32, _buffer: u64, _size: u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn QueryCredentialsAttributesW(_credential: *mut [u64; 2], _attr: u32, _buffer: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn QueryCredentialsAttributesA(_credential: *mut [u64; 2], _attr: u32, _buffer: u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn ImpersonateSecurityContext(_context: *mut [u64; 2]) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn RevertSecurityContext(_context: *mut [u64; 2]) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn MakeSignature(_context: *mut [u64; 2], _qop: u32, _message: u64, _seq: u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn VerifySignature(_context: *mut [u64; 2], _message: u64, _seq: u32, _qop: *mut u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn EncryptMessage(_context: *mut [u64; 2], _qop: u32, _message: u64, _seq: u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn DecryptMessage(_context: *mut [u64; 2], _message: u64, _seq: u32, _qop: *mut u32) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

extern "win64" fn QuerySecurityContextToken(_context: *mut [u64; 2], token: *mut u64) -> SecStatus {
    if !token.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *token = 0 };
    }
    SEC_E_INVALID_HANDLE
}

extern "win64" fn ExportSecurityContext(_context: *mut [u64; 2], _flags: u32, _packed: u64, _token: *mut u64) -> SecStatus {
    SEC_E_INVALID_HANDLE
}

/// Frees a buffer SSPI allocated (the heap block it came from).
extern "win64" fn FreeContextBuffer(p: u64) -> SecStatus {
    if p != 0 {
        crate::kernel32::heap_free(p);
    }
    SEC_E_OK
}

/// SecurityFunctionTable (A and W share the layout), version 1:
/// dwVersion (padded to 8 bytes) and 28 entry points.
#[repr(C)]
pub(crate) struct SspiTable {
    version: u64,
    entries: [*const (); 28],
}

// SAFETY: immutable function pointers.
unsafe impl Sync for SspiTable {}

/// Reserved slots and entry points newer than version 1 stay NULL.
const NONE: *const () = core::ptr::null();

macro_rules! table {
    ($enumerate:ident, $query_cred:ident, $acquire:ident, $init:ident, $query_ctx:ident, $query_pkg:ident, $set_ctx:ident) => {
        SspiTable {
            version: 1,
            entries: [
                $enumerate as *const (),
                $query_cred as *const (),
                $acquire as *const (),
                FreeCredentialsHandle as *const (),
                NONE,
                $init as *const (),
                AcceptSecurityContext as *const (),
                CompleteAuthToken as *const (),
                DeleteSecurityContext as *const (),
                ApplyControlToken as *const (),
                $query_ctx as *const (),
                ImpersonateSecurityContext as *const (),
                RevertSecurityContext as *const (),
                MakeSignature as *const (),
                VerifySignature as *const (),
                FreeContextBuffer as *const (),
                $query_pkg as *const (),
                NONE,
                NONE,
                ExportSecurityContext as *const (),
                NONE, // ImportSecurityContext
                NONE, // AddCredentials
                NONE,
                QuerySecurityContextToken as *const (),
                EncryptMessage as *const (),
                DecryptMessage as *const (),
                $set_ctx as *const (),
                NONE, // SetCredentialsAttributes
            ],
        }
    };
}

static TABLE_W: SspiTable = table!(
    EnumerateSecurityPackagesW,
    QueryCredentialsAttributesW,
    AcquireCredentialsHandleW,
    InitializeSecurityContextW,
    QueryContextAttributesW,
    QuerySecurityPackageInfoW,
    SetContextAttributesW
);

static TABLE_A: SspiTable = table!(
    EnumerateSecurityPackagesA,
    QueryCredentialsAttributesA,
    AcquireCredentialsHandleA,
    InitializeSecurityContextA,
    QueryContextAttributesA,
    QuerySecurityPackageInfoA,
    SetContextAttributesA
);

extern "win64" fn InitSecurityInterfaceW() -> *const SspiTable {
    &TABLE_W
}

extern "win64" fn InitSecurityInterfaceA() -> *const SspiTable {
    &TABLE_A
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        InitSecurityInterfaceW,
        InitSecurityInterfaceA,
        EnumerateSecurityPackagesW,
        EnumerateSecurityPackagesA,
        QuerySecurityPackageInfoW,
        QuerySecurityPackageInfoA,
        AcquireCredentialsHandleW,
        AcquireCredentialsHandleA,
        FreeCredentialsHandle,
        InitializeSecurityContextW,
        InitializeSecurityContextA,
        AcceptSecurityContext,
        CompleteAuthToken,
        DeleteSecurityContext,
        ApplyControlToken,
        QueryContextAttributesW,
        QueryContextAttributesA,
        SetContextAttributesW,
        SetContextAttributesA,
        QueryCredentialsAttributesW,
        QueryCredentialsAttributesA,
        ImpersonateSecurityContext,
        RevertSecurityContext,
        MakeSignature,
        VerifySignature,
        EncryptMessage,
        DecryptMessage,
        QuerySecurityContextToken,
        ExportSecurityContext,
        FreeContextBuffer,
    ]
}
