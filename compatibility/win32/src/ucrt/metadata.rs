//! Windows CRT stat layouts, readonly-mode changes and temporary names.
//! File data comes from Win32 handles; paths retain the runtime's C: map.
use super::*;

k32fn!(fn info = "GetFileInformationByHandle"(h: u64, p: *mut u32) -> i32);
k32fn!(fn kind = "GetFileType"(h: u64) -> u32);
k32fn!(fn create = "CreateFileW"(p: *const u16, a: u32, s: u32, sa: u64, d: u32, f: u32, t: u64) -> u64);
k32fn!(fn close = "CloseHandle"(h: u64) -> i32);
k32fn!(fn drives = "GetLogicalDrives"() -> u32);
k32fn!(fn set_attributes = "SetFileAttributesW"(p: *const u16, a: u32) -> i32);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Header {
    dev: u32,
    ino: u16,
    mode: u16,
    nlink: i16,
    uid: i16,
    gid: i16,
    rdev: u32,
}

#[repr(C)]
#[derive(Default)]
struct Stat64 {
    header: Header,
    size: i64,
    atime: i64,
    mtime: i64,
    ctime: i64,
}
#[repr(C)]
#[derive(Default)]
struct Stat64i32 {
    header: Header,
    size: i32,
    atime: i64,
    mtime: i64,
    ctime: i64,
}

const _: () = assert!(core::mem::size_of::<Stat64>() == 56);
const _: () = assert!(core::mem::size_of::<Stat64i32>() == 48);

fn stat(h: u64, executable: bool) -> Result<Stat64, i32> {
    let ft = kind(h);
    if ft == 2 || ft == 3 {
        return Ok(Stat64 {
            header: Header { mode: if ft == 2 { 0x2000 } else { 0x1000 }, nlink: 1, ..Header::default() },
            ..Stat64::default()
        });
    }
    let mut b = [0u32; 13];
    if info(h, b.as_mut_ptr()) == 0 {
        dosmaperr(get_last_error());
        return Err(errno());
    }
    let directory = b[0] & 0x10 != 0;
    let mut mode = if directory { 0x4000 } else { 0x8000 } | 0o444;
    if b[0] & 1 == 0 {
        mode |= 0o222;
    }
    if directory || executable {
        mode |= 0o111;
    }
    let seconds = |i| {
        let ticks = b[i] as u64 | (b[i + 1] as u64) << 32;
        ((ticks as i128 - 116_444_736_000_000_000i128) / 10_000_000) as i64
    };
    Ok(Stat64 {
        header: Header { dev: 2, rdev: 2, mode, nlink: b[10].min(i16::MAX as u32) as i16, ..Header::default() },
        size: (b[9] as u64 | (b[8] as u64) << 32) as i64,
        atime: seconds(3),
        mtime: seconds(5),
        ctime: seconds(1),
    })
}

fn by_path(p: *const u16) -> Result<Stat64, i32> {
    // SAFETY: callers validate a NUL-terminated path.
    let path = unsafe { wstr(p) };
    let name = alloc::string::String::from_utf16_lossy(path).to_ascii_lowercase();
    let executable = [".exe", ".com", ".bat", ".cmd"].iter().any(|s| name.ends_with(s));
    // FILE_FLAG_BACKUP_SEMANTICS permits directory metadata handles.
    let h = create(p, 0, 7, 0, 3, 0x0200_0000, 0);
    if h == u64::MAX {
        dosmaperr(get_last_error());
        return Err(errno());
    }
    let value = stat(h, executable);
    close(h);
    value
}

extern "win64" fn _wstat64(p: *const u16, out: *mut Stat64) -> i32 {
    validate!(!p.is_null() && !out.is_null(), EINVAL, -1);
    match by_path(p) {
        Ok(s) => {
            /* SAFETY: validated destination with the _stat64 layout. */
            unsafe { *out = s };
            0
        }
        Err(_) => -1,
    }
}

fn write_small(out: *mut Stat64i32, s: Stat64) -> i32 {
    let Ok(size) = i32::try_from(s.size) else {
        set_errno(ERANGE);
        return -1;
    };
    // SAFETY: callers validate the destination with the _stat64i32 layout.
    unsafe { *out = Stat64i32 { header: s.header, size, atime: s.atime, mtime: s.mtime, ctime: s.ctime } };
    0
}

extern "win64" fn _wstat64i32(p: *const u16, out: *mut Stat64i32) -> i32 {
    validate!(!p.is_null() && !out.is_null(), EINVAL, -1);
    by_path(p).map_or(-1, |s| write_small(out, s))
}

extern "win64" fn _fstat64i32(fd: i32, out: *mut Stat64i32) -> i32 {
    validate!(!out.is_null(), EINVAL, -1);
    let Some(h) = lowio::handle_of(fd) else { return invalid(EBADF, -1) };
    stat(h, false).map_or(-1, |s| write_small(out, s))
}

extern "win64" fn _getdrives() -> u32 {
    drives()
}

extern "win64" fn _wchmod(p: *const u16, mode: i32) -> i32 {
    validate!(!p.is_null(), EINVAL, -1);
    validate!(mode & !0x180 == 0, EINVAL, -1);
    let a = fs::get_file_attributes(p);
    if a == u32::MAX {
        dosmaperr(get_last_error());
        return -1;
    }
    let attrs = if mode & 0x80 != 0 { a & !1 } else { a | 1 };
    if set_attributes(p, attrs) == 0 {
        dosmaperr(get_last_error());
        -1
    } else {
        0
    }
}

/// Choose an absent name; like the UCRT this does not create a file.
/// The caller must still create it atomically (e.g. _O_CREAT|_O_EXCL).
fn mktemp<T: Copy + Into<u32> + From<u8>>(p: *mut T, cap: usize) -> i32 {
    validate!(!p.is_null() && cap > 0, EINVAL, EINVAL);
    let mut n = 0;
    // SAFETY: read no more than the declared buffer capacity.
    while n < cap && unsafe { (*p.add(n)).into() } != 0 {
        n += 1;
    }
    if n == cap || n < 6 || (n - 6..n).any(|i| unsafe { (*p.add(i)).into() } != b'X' as u32) {
        // SAFETY: cap > 0.
        unsafe { *p = T::from(0) };
        return invalid(EINVAL, EINVAL);
    }
    let pid = crate::kernel32::pid() % 100_000;
    for letter in b'a'..=b'z' {
        let mut number = pid;
        // SAFETY: the six trailing Xs lie within the checked buffer.
        unsafe {
            *p.add(n - 6) = T::from(letter);
            for i in (n - 5..n).rev() {
                *p.add(i) = T::from(b'0' + (number % 10) as u8);
                number /= 10;
            }
        }
        let units: Vec<u16> = (0..n).map(|i| unsafe { (*p.add(i)).into() } as u16).collect();
        let mut path: Vec<u16> = if core::mem::size_of::<T>() == 1 {
            let bytes: Vec<u8> = units.iter().map(|&c| c as u8).collect();
            alloc::string::String::from_utf8_lossy(&bytes).encode_utf16().collect()
        } else {
            units
        };
        path.push(0);
        if fs::get_file_attributes(path.as_ptr()) == u32::MAX {
            let error = get_last_error();
            if error == 2 || error == 3 {
                return 0;
            }
            // SAFETY: cap > 0.
            unsafe { *p = T::from(0) };
            dosmaperr(error);
            return errno();
        }
    }
    // SAFETY: cap > 0.
    unsafe { *p = T::from(0) };
    set_errno(EEXIST);
    EEXIST
}

extern "win64" fn _mktemp_s(p: *mut u8, n: usize) -> i32 {
    mktemp(p, n)
}
extern "win64" fn _wmktemp_s(p: *mut u16, n: usize) -> i32 {
    mktemp(p, n)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![_wstat64, _wstat64i32, _fstat64i32, _getdrives, _wchmod, _mktemp_s, _wmktemp_s]
}
