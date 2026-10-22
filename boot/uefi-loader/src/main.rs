//! OSyx Boot Manager: a graphical UEFI application (BOOTX64.EFI) that loads the
//! OSyx kernel ELF from the boot volume, builds its initial page tables and
//! BootInfo, exits boot services and jumps to the kernel.
//!
//! Boot stages (each advances the progress bar only once it has completed):
//!   1. firmware services + graphics   2. kernel located   3. kernel read
//!   4. kernel validated and loaded     5. memory prepared  6. boot services exited

#![no_std]
#![no_main]
// BootError carries a 128-byte inline message because the loader has no heap
// to box it in; it only travels up the (cold) error path, so the size is fine.
#![allow(clippy::result_large_err)]

mod efi;
mod paging;
mod screen;
mod serial;

use core::arch::asm;
use core::convert::Infallible;
use core::ffi::c_void;
use core::ptr::null_mut;

use efi::{memtype, BootServices, File, Handle, Status, SystemTable};
use osyx_bootinfo::{
    BootInfo, KernelImageInfo, MemoryDescriptor, MemoryKind, MemoryMapInfo, BOOTINFO_MAGIC, BOOTINFO_VERSION,
    FLAG_ACPI, FLAG_FRAMEBUFFER, FLAG_INIT_MODULE,
};
use osyx_elf::{KernelImage, PAGE_SIZE};
use paging::{TableBuilder, HHDM_OFFSET};
use screen::Screen;
use serial::StrBuf;

const KERNEL_PATH: &str = "\\EFI\\OSYX\\KERNEL.ELF";
const CMDLINE_PATH: &str = "\\EFI\\OSYX\\CMDLINE.TXT";
const INIT_PATH: &str = "\\EFI\\OSYX\\INIT.ELF";
const INIT_MAX: u64 = 64 << 20;
const CMDLINE_MAX: usize = 4096;
const STACK_PAGES: u64 = 32; // 128 KiB boot stack for the kernel
const STAGES: u32 = 6;
const LOADER_VERSION: u32 = 0x00_01_00; // 0.1.0 as major << 16 | minor << 8 | patch
const GIB: u64 = 1 << 30;

// BootInfo region layout (one page-aligned OSYX_BOOTINFO allocation).
const CMDLINE_OFFSET: u64 = 256;
const MAP_OFFSET: u64 = CMDLINE_OFFSET + CMDLINE_MAX as u64;

struct BootError {
    status: Status,
    title: &'static str,
    detail: StrBuf<128>,
}

macro_rules! fail {
    ($st:expr, $title:expr, $($arg:tt)*) => {
        return Err(BootError { status: $st, title: $title, detail: fmt_buf!(128, $($arg)*) })
    };
}

const fn progress(stage: u32) -> u32 {
    stage * 1000 / STAGES
}

/// UEFI image entry point.
///
/// # Safety
/// Must only be called by UEFI firmware, with the image handle and a valid
/// system table, as specified for `EFI_IMAGE_ENTRY_POINT`.
#[no_mangle]
pub unsafe extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    serial::init();
    log!("OSyx Boot Manager {} starting", env!("CARGO_PKG_VERSION"));
    if st.is_null() {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: the firmware passes a valid system table to the image entry point.
    let st = unsafe { &*st };
    // SAFETY: boot services are valid until ExitBootServices.
    let bs = unsafe { &*st.boot_services };
    // SAFETY: disables the 5 minute watchdog the firmware arms before StartImage.
    unsafe { (bs.set_watchdog_timer)(0, 0, 0, core::ptr::null()) };

    let mut screen: Option<Screen> = None;
    let err = match boot(image, st, bs, &mut screen) {
        Ok(never) => match never {},
        Err(e) => e,
    };
    log!("BOOT FAILED: {}: {} [{}]", err.title, err.detail.as_str(), err.status.name());
    if let Some(s) = screen.as_mut() {
        s.error(err.title, &[err.detail.as_str(), "Press any key to return to the firmware"]);
    }
    wait_for_key(st, bs, 30_000);
    err.status
}

fn boot(
    image: Handle,
    st: &SystemTable,
    bs: &BootServices,
    screen: &mut Option<Screen>,
) -> Result<Infallible, BootError> {
    // ---- Stage 1: firmware services, configuration, graphics -------------
    let nx = check_cpu()?;
    let root = open_boot_volume(bs, image)?;
    let cmdline = read_cmdline(bs, root);
    let opts = Options::parse(cmdline.as_slice());
    log!("command line: {:?}", core::str::from_utf8(cmdline.as_slice()).unwrap_or("<invalid utf-8>"));

    match Screen::init(bs, opts.mode, opts.fast) {
        Ok(s) => {
            let fb = s.info();
            log!("graphics: {}x{} stride {} {:?} at {:#x}", fb.width, fb.height, fb.stride, fb.format, fb.base);
            *screen = Some(s);
        }
        Err(e) => log!("graphics unavailable ({e:?}); continuing with serial output only"),
    }
    let stage = |screen: &mut Option<Screen>, n: u32, text: &str| {
        log!("stage {n}/{STAGES}: {text}");
        if let Some(s) = screen.as_mut() {
            s.stage(bs, progress(n), text);
        }
    };
    if let Some(s) = screen.as_mut() {
        s.intro(bs);
    }
    stage(screen, 1, "Locating kernel");

    // ---- Stage 2: locate kernel ------------------------------------------
    let file = match open_file(root, KERNEL_PATH) {
        Ok(f) => f,
        Err(st) => fail!(st, "Kernel not found", "{} could not be opened ({})", KERNEL_PATH, st.name()),
    };
    let size = file_size(file).map_err(|st| BootError {
        status: st,
        title: "Kernel not readable",
        detail: fmt_buf!(128, "cannot query size of {} ({})", KERNEL_PATH, st.name()),
    })?;
    if size == 0 || size > 256 << 20 {
        fail!(Status::LOAD_ERROR, "Kernel image is invalid", "{} has implausible size {} bytes", KERNEL_PATH, size);
    }
    stage(screen, 2, "Loading kernel");

    // ---- Stage 3: read kernel --------------------------------------------
    let data = alloc_pool(bs, size as usize)?;
    if let Err(st) = read_exact(file, data) {
        fail!(st, "Kernel not readable", "read of {} failed ({})", KERNEL_PATH, st.name());
    }
    // SAFETY: file handle opened above; closing is best-effort.
    unsafe { ((*file).close)(file) };
    log!("kernel file: {} bytes", size);
    stage(screen, 3, "Verifying kernel image");

    // ---- Stage 4: validate and load segments ------------------------------
    let kernel = match KernelImage::parse(data) {
        Ok(k) => k,
        Err(e) => fail!(Status::LOAD_ERROR, "Kernel image is invalid", "{}: {}", KERNEL_PATH, e.describe()),
    };
    let span = kernel.virt_span();
    let kphys = alloc_pages(bs, memtype::OSYX_KERNEL, span / PAGE_SIZE)?;
    // SAFETY: `kphys` is a fresh, identity-mapped allocation of `span` bytes;
    // segment ranges were validated to lie inside [virt_base, virt_base+span).
    unsafe {
        core::ptr::write_bytes(kphys as *mut u8, 0, span as usize);
        for seg in kernel.segments() {
            let bytes = kernel.file_bytes(seg);
            let dst = (kphys + (seg.vaddr - kernel.virt_base())) as *mut u8;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }
    }
    for seg in kernel.segments() {
        log!(
            "segment {:#x}..{:#x} {}{}{}",
            seg.vaddr,
            seg.vaddr + seg.mem_size,
            if seg.flags & osyx_elf::PF_R != 0 { 'r' } else { '-' },
            if seg.writable() { 'w' } else { '-' },
            if seg.executable() { 'x' } else { '-' }
        );
    }
    log!("kernel loaded at phys {:#x}, entry {:#x}", kphys, kernel.entry());
    let init = load_init_module(bs, root)?;
    stage(screen, 4, "Preparing memory");

    // ---- Stage 5: page tables, stack, BootInfo, memory map ---------------
    let (map_buf, map_cap, desc_size) = alloc_memory_map_buffer(bs)?;
    let fb = screen.as_ref().map(|s| s.info());
    let mut phys_top = highest_ram_address(bs, map_buf, map_cap)?;
    if let Some(fb) = fb {
        phys_top = phys_top.max(fb.base + fb.size);
    }
    let hhdm_size = phys_top.max(4 * GIB).div_ceil(GIB) * GIB;

    let table_pages = paging::tables_needed(hhdm_size, span);
    let table_pool = alloc_pages(bs, memtype::OSYX_KERNEL_DATA, table_pages)?;
    // SAFETY: `table_pool` holds `table_pages` freshly allocated pages.
    let mut tables = unsafe { TableBuilder::new(table_pool, table_pages, nx) }.map_err(paging_err)?;
    tables.map_physical(hhdm_size).map_err(paging_err)?;
    tables.map_kernel(&kernel, kphys).map_err(paging_err)?;
    log!("page tables: {} pages, HHDM {:#x} covers {} GiB, NX {}", tables.used(), HHDM_OFFSET, hhdm_size / GIB, nx);

    let stack = alloc_pages(bs, memtype::OSYX_KERNEL_DATA, STACK_PAGES)?;
    let stack_top = HHDM_OFFSET + stack + STACK_PAGES * PAGE_SIZE;

    let max_entries = map_cap / desc_size + 16;
    let region_bytes = MAP_OFFSET + max_entries as u64 * core::mem::size_of::<MemoryDescriptor>() as u64;
    let region = alloc_pages(bs, memtype::OSYX_BOOTINFO, region_bytes.div_ceil(PAGE_SIZE))?;
    // SAFETY: fresh allocation of at least `region_bytes` bytes.
    unsafe { core::ptr::write_bytes(region as *mut u8, 0, region_bytes as usize) };
    let cmd = cmdline.as_slice();
    // SAFETY: the command line area is CMDLINE_MAX bytes and `cmd` is at most that.
    unsafe { core::ptr::copy_nonoverlapping(cmd.as_ptr(), (region + CMDLINE_OFFSET) as *mut u8, cmd.len()) };

    let rsdp = find_acpi_rsdp(st);
    let mut flags = 0;
    if fb.is_some() {
        flags |= FLAG_FRAMEBUFFER;
    }
    if rsdp != 0 {
        flags |= FLAG_ACPI;
    }
    if init.is_some() {
        flags |= FLAG_INIT_MODULE;
    }
    let info = BootInfo {
        magic: BOOTINFO_MAGIC,
        version: BOOTINFO_VERSION,
        size: core::mem::size_of::<BootInfo>() as u32,
        bootloader_version: LOADER_VERSION,
        flags,
        framebuffer: fb.unwrap_or(osyx_bootinfo::FramebufferInfo {
            base: 0,
            size: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: osyx_bootinfo::PixelFormat::BltOnly,
        }),
        memory_map: MemoryMapInfo {
            entries: HHDM_OFFSET + region + MAP_OFFSET,
            count: 0, // filled in after ExitBootServices
            entry_size: core::mem::size_of::<MemoryDescriptor>() as u64,
        },
        kernel: KernelImageInfo {
            phys_base: kphys,
            size: span,
            virt_base: kernel.virt_base(),
            entry_point: kernel.entry(),
        },
        acpi_rsdp: rsdp,
        cmdline_ptr: if cmd.is_empty() { 0 } else { HHDM_OFFSET + region + CMDLINE_OFFSET },
        cmdline_len: cmd.len() as u64,
        hhdm_offset: HHDM_OFFSET,
        hhdm_size,
        init_module: init.map_or(0, |(phys, _)| HHDM_OFFSET + phys),
        init_module_size: init.map_or(0, |(_, size)| size),
    };
    let info_ptr = region as *mut BootInfo;
    // SAFETY: region is 8-byte (page) aligned and large enough.
    unsafe { info_ptr.write(info) };
    stage(screen, 5, "Starting kernel");

    // ---- Stage 6: ExitBootServices ---------------------------------------
    let map_size = exit_boot_services(bs, image, map_buf, map_cap)?;
    // From here on: no firmware calls, no allocation.
    // SAFETY: the descriptor array lives in our BOOTINFO region with room for
    // `max_entries` entries; the UEFI map buffer holds `map_size` bytes.
    let count = unsafe {
        let out = core::slice::from_raw_parts_mut((region + MAP_OFFSET) as *mut MemoryDescriptor, max_entries);
        convert_memory_map(map_buf, map_size, desc_size, out)
    };
    // SAFETY: info_ptr written above.
    unsafe { (*info_ptr).memory_map.count = count as u64 };
    if let Some(s) = screen.as_mut() {
        s.handoff();
    }
    log!("boot services exited; {} memory regions; jumping to kernel", count);
    // SAFETY: page tables map the kernel, the stack (via HHDM) and identity
    // map this trampoline; BootInfo is reachable through the HHDM.
    unsafe { enter_kernel(tables.pml4, stack_top, kernel.entry(), HHDM_OFFSET + region, nx) }
}

fn paging_err(e: paging::PagingError) -> BootError {
    BootError { status: Status::LOAD_ERROR, title: "Cannot build page tables", detail: fmt_buf!(128, "{e:?}") }
}

/// Verifies paging mode assumptions and reports NX support.
fn check_cpu() -> Result<bool, BootError> {
    let cr4: u64;
    // SAFETY: reading CR4 has no side effects; UEFI runs in ring 0.
    unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
    if cr4 & (1 << 12) != 0 {
        fail!(Status::UNSUPPORTED, "Unsupported CPU mode", "firmware enabled 5-level paging (LA57)");
    }
    #[allow(unused_unsafe)]
    // SAFETY: CPUID is available on every x86-64 CPU.
    let ext = unsafe { core::arch::x86_64::__cpuid(0x8000_0001) };
    Ok(ext.edx & (1 << 20) != 0)
}

struct Options {
    fast: bool,
    mode: Option<(u32, u32)>,
}

impl Options {
    fn parse(cmdline: &[u8]) -> Options {
        let text = core::str::from_utf8(cmdline).unwrap_or("");
        let mut o = Options { fast: false, mode: None };
        for word in text.split_ascii_whitespace() {
            if word == "boot.fast" {
                o.fast = true;
            } else if let Some(m) = word.strip_prefix("boot.mode=") {
                if let Some((w, h)) = m.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                        o.mode = Some((w, h));
                    }
                }
            }
        }
        o
    }
}

struct Cmdline {
    buf: [u8; CMDLINE_MAX],
    len: usize,
}

impl Cmdline {
    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Reads the optional command line file; absence is not an error.
fn read_cmdline(bs: &BootServices, root: *mut File) -> Cmdline {
    let mut c = Cmdline { buf: [0; CMDLINE_MAX], len: 0 };
    let Ok(f) = open_file(root, CMDLINE_PATH) else { return c };
    match file_size(f) {
        Ok(n) if n as usize <= CMDLINE_MAX => {
            if read_exact(f, &mut c.buf[..n as usize]).is_ok() {
                c.len = n as usize;
                while c.len > 0 && c.buf[c.len - 1].is_ascii_whitespace() {
                    c.len -= 1;
                }
            }
        }
        Ok(n) => log!("{CMDLINE_PATH} ignored: {n} bytes exceeds {CMDLINE_MAX}"),
        Err(st) => log!("{CMDLINE_PATH} ignored: {}", st.name()),
    }
    // SAFETY: handle opened above.
    unsafe { ((*f).close)(f) };
    let _ = bs;
    c
}

/// Loads the optional initial user program into bootloader-reclaimable pages.
/// Its contents are validated by the kernel, not here. Returns (phys, size).
fn load_init_module(bs: &BootServices, root: *mut File) -> Result<Option<(u64, u64)>, BootError> {
    let Ok(f) = open_file(root, INIT_PATH) else {
        log!("no {INIT_PATH}; kernel starts without a user program");
        return Ok(None);
    };
    let size = match file_size(f) {
        Ok(n) if n > 0 && n <= INIT_MAX => n,
        Ok(n) => fail!(Status::LOAD_ERROR, "Init program is invalid", "{} has implausible size {} bytes", INIT_PATH, n),
        Err(st) => fail!(st, "Init program not readable", "cannot query size of {} ({})", INIT_PATH, st.name()),
    };
    let phys = alloc_pages(bs, memtype::OSYX_BOOTINFO, size.div_ceil(PAGE_SIZE))?;
    // SAFETY: fresh allocation of at least `size` bytes, identity mapped.
    let buf = unsafe { core::slice::from_raw_parts_mut(phys as *mut u8, size as usize) };
    if let Err(st) = read_exact(f, buf) {
        fail!(st, "Init program not readable", "read of {} failed ({})", INIT_PATH, st.name());
    }
    // SAFETY: handle opened above.
    unsafe { ((*f).close)(f) };
    log!("init module: {} bytes at phys {:#x}", size, phys);
    Ok(Some((phys, size)))
}

fn open_boot_volume(bs: &BootServices, image: Handle) -> Result<*mut File, BootError> {
    let mut loaded: *mut efi::LoadedImage = null_mut();
    // SAFETY: HandleProtocol on our own image handle with a typed out pointer.
    let st = unsafe { (bs.handle_protocol)(image, &efi::LOADED_IMAGE_GUID, &mut loaded as *mut _ as *mut *mut c_void) };
    if st.is_error() || loaded.is_null() {
        fail!(st, "Boot volume unavailable", "LoadedImage protocol missing ({})", st.name());
    }
    let mut fs: *mut efi::SimpleFileSystem = null_mut();
    // SAFETY: `loaded` is valid; device_handle is the volume we were loaded from.
    let st = unsafe {
        (bs.handle_protocol)((*loaded).device_handle, &efi::SIMPLE_FS_GUID, &mut fs as *mut _ as *mut *mut c_void)
    };
    if st.is_error() || fs.is_null() {
        fail!(st, "Boot volume unavailable", "boot device has no file system ({})", st.name());
    }
    let mut root: *mut File = null_mut();
    // SAFETY: valid protocol instance.
    let st = unsafe { ((*fs).open_volume)(fs, &mut root) };
    if st.is_error() || root.is_null() {
        fail!(st, "Boot volume unavailable", "OpenVolume failed ({})", st.name());
    }
    Ok(root)
}

fn open_file(root: *mut File, path: &str) -> Result<*mut File, Status> {
    let name = efi::ucs2::<128>(path);
    let mut f: *mut File = null_mut();
    // SAFETY: `root` is an open directory handle and `name` is NUL-terminated.
    let st = unsafe { ((*root).open)(root, &mut f, name.as_ptr(), efi::FILE_MODE_READ, 0) };
    st.ok()?;
    if f.is_null() {
        return Err(Status::NOT_FOUND);
    }
    Ok(f)
}

fn file_size(f: *mut File) -> Result<u64, Status> {
    let mut buf = [0u64; 128];
    let mut size = core::mem::size_of_val(&buf);
    // SAFETY: buffer is 8-byte aligned and `size` bytes long.
    let st = unsafe { ((*f).get_info)(f, &efi::FILE_INFO_GUID, &mut size, buf.as_mut_ptr() as *mut c_void) };
    st.ok()?;
    if size < core::mem::size_of::<efi::FileInfoHeader>() {
        return Err(Status::LOAD_ERROR);
    }
    // SAFETY: firmware filled at least the fixed header.
    Ok(unsafe { (*(buf.as_ptr() as *const efi::FileInfoHeader)).file_size })
}

fn read_exact(f: *mut File, buf: &mut [u8]) -> Result<(), Status> {
    let mut done = 0;
    while done < buf.len() {
        let mut n = buf.len() - done;
        // SAFETY: `n` bytes are writable at `buf[done..]`.
        let st = unsafe { ((*f).read)(f, &mut n, buf[done..].as_mut_ptr() as *mut c_void) };
        st.ok()?;
        if n == 0 {
            return Err(Status::LOAD_ERROR); // unexpected end of file
        }
        done += n;
    }
    Ok(())
}

fn alloc_pool(bs: &BootServices, size: usize) -> Result<&'static mut [u8], BootError> {
    let mut p: *mut u8 = null_mut();
    // SAFETY: plain pool allocation.
    let st = unsafe { (bs.allocate_pool)(memtype::LOADER_DATA, size, &mut p) };
    if st.is_error() || p.is_null() {
        fail!(st, "Out of memory", "AllocatePool({} bytes) failed ({})", size, st.name());
    }
    // SAFETY: zero-initialise before exposing as a slice.
    unsafe {
        core::ptr::write_bytes(p, 0, size);
        Ok(core::slice::from_raw_parts_mut(p, size))
    }
}

fn alloc_pages(bs: &BootServices, ty: u32, pages: u64) -> Result<u64, BootError> {
    let mut addr = 0u64;
    // SAFETY: plain page allocation.
    let st = unsafe { (bs.allocate_pages)(efi::ALLOCATE_ANY_PAGES, ty, pages as usize, &mut addr) };
    if st.is_error() {
        fail!(st, "Out of memory", "AllocatePages({} pages) failed ({})", pages, st.name());
    }
    Ok(addr)
}

/// Allocates a buffer large enough for the UEFI memory map plus headroom for
/// the descriptors our own later allocations add. Returns (buf, cap, desc_size).
fn alloc_memory_map_buffer(bs: &BootServices) -> Result<(*mut u8, usize, usize), BootError> {
    let (mut size, mut key, mut dsize, mut ver) = (0usize, 0usize, 0usize, 0u32);
    // SAFETY: probing call with a zero-sized buffer.
    let st = unsafe { (bs.get_memory_map)(&mut size, null_mut(), &mut key, &mut dsize, &mut ver) };
    if st != Status::BUFFER_TOO_SMALL || dsize < core::mem::size_of::<efi::MemoryDescriptor>() {
        fail!(st, "Memory map unavailable", "GetMemoryMap probe failed ({})", st.name());
    }
    let cap = size + 64 * dsize;
    let buf = alloc_pool(bs, cap)?;
    Ok((buf.as_mut_ptr(), cap, dsize))
}

fn highest_ram_address(bs: &BootServices, buf: *mut u8, cap: usize) -> Result<u64, BootError> {
    let (mut size, mut key, mut dsize, mut ver) = (cap, 0usize, 0usize, 0u32);
    // SAFETY: `buf` holds `cap` bytes.
    let st = unsafe { (bs.get_memory_map)(&mut size, buf as *mut _, &mut key, &mut dsize, &mut ver) };
    if st.is_error() {
        fail!(st, "Memory map unavailable", "GetMemoryMap failed ({})", st.name());
    }
    // Only RAM-backed types count: firmware may report Reserved ranges far
    // above RAM (QEMU's HyperTransport hole just below 1 TiB) that the
    // kernel can map on demand, and covering them would waste page tables.
    let mut top = 0;
    for i in 0..size / dsize {
        // SAFETY: i * dsize + size_of::<descriptor>() <= size.
        let d = unsafe { &*(buf.add(i * dsize) as *const efi::MemoryDescriptor) };
        let ram = matches!(
            d.ty,
            memtype::LOADER_CODE
                | memtype::LOADER_DATA
                | memtype::BOOT_SERVICES_CODE
                | memtype::BOOT_SERVICES_DATA
                | memtype::RUNTIME_SERVICES_CODE
                | memtype::RUNTIME_SERVICES_DATA
                | memtype::CONVENTIONAL
                | memtype::ACPI_RECLAIM
                | memtype::ACPI_NVS
                | memtype::PERSISTENT
        );
        if ram {
            top = u64::max(top, d.physical_start + d.number_of_pages * PAGE_SIZE);
        }
    }
    Ok(top)
}

/// Gets the final memory map and exits boot services. Returns the map size.
fn exit_boot_services(bs: &BootServices, image: Handle, buf: *mut u8, cap: usize) -> Result<usize, BootError> {
    for attempt in 0..3 {
        let (mut size, mut key, mut dsize, mut ver) = (cap, 0usize, 0usize, 0u32);
        // SAFETY: `buf` holds `cap` bytes.
        let st = unsafe { (bs.get_memory_map)(&mut size, buf as *mut _, &mut key, &mut dsize, &mut ver) };
        if st.is_error() {
            fail!(st, "Memory map unavailable", "final GetMemoryMap failed ({})", st.name());
        }
        // SAFETY: `key` belongs to the map just retrieved; no allocation in between.
        let st = unsafe { (bs.exit_boot_services)(image, key) };
        if !st.is_error() {
            return Ok(size);
        }
        // The map changed between the two calls (e.g. a timer event allocated); retry.
        if attempt == 2 {
            fail!(st, "Cannot exit boot services", "ExitBootServices failed ({})", st.name());
        }
    }
    unreachable!()
}

fn kind_of(ty: u32) -> MemoryKind {
    match ty {
        memtype::CONVENTIONAL | memtype::BOOT_SERVICES_CODE | memtype::BOOT_SERVICES_DATA => MemoryKind::Usable,
        memtype::LOADER_CODE | memtype::LOADER_DATA | memtype::OSYX_BOOTINFO => MemoryKind::BootloaderReclaimable,
        memtype::OSYX_KERNEL => MemoryKind::KernelImage,
        memtype::OSYX_KERNEL_DATA => MemoryKind::KernelData,
        memtype::ACPI_RECLAIM => MemoryKind::AcpiReclaimable,
        memtype::ACPI_NVS => MemoryKind::AcpiNvs,
        memtype::MMIO | memtype::MMIO_PORT_SPACE => MemoryKind::Mmio,
        memtype::UNUSABLE => MemoryKind::Unusable,
        _ => MemoryKind::Reserved,
    }
}

/// Converts the UEFI map to sorted, merged OSyx descriptors. Allocation free;
/// safe to call after ExitBootServices.
///
/// # Safety
/// `src` must hold `size` bytes of descriptors of stride `dsize`.
unsafe fn convert_memory_map(src: *const u8, size: usize, dsize: usize, out: &mut [MemoryDescriptor]) -> usize {
    let mut n = 0;
    for i in 0..size / dsize {
        let d = &*(src.add(i * dsize) as *const efi::MemoryDescriptor);
        if d.number_of_pages == 0 || n == out.len() {
            continue;
        }
        out[n] = MemoryDescriptor {
            phys_start: d.physical_start,
            page_count: d.number_of_pages,
            kind: kind_of(d.ty),
            _reserved: 0,
        };
        n += 1;
    }
    // Insertion sort: the map has at most a few hundred entries and is usually
    // already sorted, which makes this linear in practice.
    for i in 1..n {
        let mut j = i;
        while j > 0 && out[j - 1].phys_start > out[j].phys_start {
            out.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut m = 0;
    for i in 0..n {
        if m > 0 && out[m - 1].kind == out[i].kind && out[m - 1].end() == out[i].phys_start {
            out[m - 1].page_count += out[i].page_count;
        } else {
            out[m] = out[i];
            m += 1;
        }
    }
    m
}

fn find_acpi_rsdp(st: &SystemTable) -> u64 {
    let mut v1 = 0;
    for i in 0..st.number_of_table_entries {
        // SAFETY: the configuration table has `number_of_table_entries` entries.
        let t = unsafe { &*st.configuration_table.add(i) };
        if t.vendor_guid == efi::ACPI_20_GUID {
            return t.vendor_table as u64;
        }
        if t.vendor_guid == efi::ACPI_10_GUID {
            v1 = t.vendor_table as u64;
        }
    }
    v1
}

fn wait_for_key(st: &SystemTable, bs: &BootServices, timeout_ms: usize) {
    let input = st.con_in;
    if input.is_null() {
        return;
    }
    let mut key = efi::InputKey { scan_code: 0, unicode_char: 0 };
    for _ in 0..timeout_ms / 10 {
        // SAFETY: valid console input protocol from the system table.
        if !unsafe { ((*input).read_key_stroke)(input, &mut key) }.is_error() {
            return;
        }
        // SAFETY: plain boot-services call.
        unsafe { (bs.stall)(10_000) };
    }
}

/// Switches to the kernel page tables and stack and jumps to `entry` with
/// `boot_info` in RDI (System V ABI first argument).
///
/// # Safety
/// `pml4` must identity map the currently executing code and map `stack_top`
/// and `entry`; boot services must already be exited.
unsafe fn enter_kernel(pml4: u64, stack_top: u64, entry: u64, boot_info: u64, nx: bool) -> ! {
    const IA32_EFER: u32 = 0xC000_0080;
    if nx {
        let (lo, hi): (u32, u32);
        asm!("rdmsr", in("ecx") IA32_EFER, out("eax") lo, out("edx") hi, options(nomem, nostack));
        let efer = ((hi as u64) << 32 | lo as u64) | (1 << 11); // NXE
        asm!("wrmsr", in("ecx") IA32_EFER, in("eax") efer as u32, in("edx") (efer >> 32) as u32, options(nomem, nostack));
    }
    asm!(
        "cli",
        "mov cr3, {pml4}",
        "mov rsp, {stack}",
        "xor ebp, ebp",
        // Fake return address: the kernel entry sees RSP % 16 == 8 as after a call.
        "push 0",
        "jmp {entry}",
        pml4 = in(reg) pml4,
        stack = in(reg) stack_top,
        entry = in(reg) entry,
        in("rdi") boot_info,
        options(noreturn)
    )
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("PANIC: {info}");
    loop {
        // SAFETY: halting is always sound.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}
