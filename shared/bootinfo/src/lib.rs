//! OSyx boot protocol: the `repr(C)` structures handed from the UEFI
//! bootloader to the kernel.
//!
//! The bootloader owns every buffer referenced from [`BootInfo`] until
//! hand-off; afterwards the kernel owns them. All memory they live in is
//! reported in the memory map as [`MemoryKind::BootloaderReclaimable`] and may
//! be reused once the kernel has finished reading the structures.
//!
//! Address conventions at kernel entry:
//! * The kernel runs on page tables built by the bootloader. All physical
//!   memory below `max(4 GiB, highest memory-map address)` is mapped at
//!   `hhdm_offset + phys` (the higher-half direct map, HHDM), writable and
//!   non-executable. The same range is identity mapped so the bootloader's
//!   trampoline survives the CR3 switch; the kernel must remove that identity
//!   map once it runs on its own stack.
//! * Fields named `*_virt` / `entries` / `cmdline_ptr` hold HHDM virtual
//!   addresses. `framebuffer.base`, `acpi_rsdp` and `kernel.phys_base` are
//!   physical addresses.
//! * `rdi` holds the HHDM address of the `BootInfo`; `rsp` points to the top
//!   of a 16-byte aligned kernel stack of at least 64 KiB.

#![no_std]

/// "OSYXBOOT" in ASCII, little endian.
pub const BOOTINFO_MAGIC: u64 = 0x544F_4F42_5859_534F;
/// Bumped on any layout change of the structures in this crate.
pub const BOOTINFO_VERSION: u32 = 2;

/// Boot flag: the framebuffer described in [`BootInfo::framebuffer`] is valid.
pub const FLAG_FRAMEBUFFER: u32 = 1 << 0;
/// Boot flag: `acpi_rsdp` holds a valid physical address.
pub const FLAG_ACPI: u32 = 1 << 1;
/// Boot flag: `init_module` describes a loaded user program image.
pub const FLAG_INIT_MODULE: u32 = 1 << 2;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Byte order in memory: red, green, blue, reserved.
    Rgb = 0,
    /// Byte order in memory: blue, green, red, reserved.
    Bgr = 1,
    /// Arbitrary bit masks; not supported by the OSyx boot graphics.
    Bitmask = 2,
    /// No linear framebuffer is available.
    BltOnly = 3,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramebufferInfo {
    /// Physical address of the first pixel.
    pub base: u64,
    /// Size of the framebuffer in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels (not bytes) per scan line.
    pub stride: u32,
    pub format: PixelFormat,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free RAM the kernel may use immediately.
    Usable = 0,
    /// Firmware / hardware reserved; never touch.
    Reserved = 1,
    AcpiReclaimable = 2,
    AcpiNvs = 3,
    /// Bootloader data (including BootInfo); reclaimable after hand-off.
    BootloaderReclaimable = 4,
    /// Pages holding the loaded kernel image.
    KernelImage = 5,
    /// Memory-mapped I/O.
    Mmio = 6,
    /// Defective memory reported by firmware.
    Unusable = 7,
    /// Live structures the kernel inherits: initial page tables and boot stack.
    KernelData = 8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryDescriptor {
    pub phys_start: u64,
    /// Length in 4 KiB pages.
    pub page_count: u64,
    pub kind: MemoryKind,
    pub _reserved: u32,
}

impl MemoryDescriptor {
    pub const PAGE_SIZE: u64 = 4096;

    pub const fn byte_len(&self) -> u64 {
        self.page_count * Self::PAGE_SIZE
    }

    pub const fn end(&self) -> u64 {
        self.phys_start + self.byte_len()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryMapInfo {
    /// HHDM address of an array of [`MemoryDescriptor`], sorted by `phys_start`
    /// with no overlapping entries.
    pub entries: u64,
    pub count: u64,
    /// Always `size_of::<MemoryDescriptor>()`; stored so the layout can be checked.
    pub entry_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelImageInfo {
    /// Physical address of the contiguous block holding all loaded segments.
    pub phys_base: u64,
    /// Size of that block in bytes.
    pub size: u64,
    /// Lowest virtual address of the loaded segments (`phys_base` maps here).
    pub virt_base: u64,
    pub entry_point: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    /// `size_of::<BootInfo>()` as seen by the bootloader.
    pub size: u32,
    pub bootloader_version: u32,
    pub flags: u32,
    pub framebuffer: FramebufferInfo,
    pub memory_map: MemoryMapInfo,
    pub kernel: KernelImageInfo,
    /// Physical address of the ACPI RSDP, or 0 when `FLAG_ACPI` is clear.
    pub acpi_rsdp: u64,
    /// HHDM address of a UTF-8 command line, or 0.
    pub cmdline_ptr: u64,
    pub cmdline_len: u64,
    /// Virtual address at which physical address 0 is mapped.
    pub hhdm_offset: u64,
    /// Exclusive upper bound of the physical range covered by the HHDM.
    pub hhdm_size: u64,
    /// HHDM address of the initial user program (`\EFI\OSYX\INIT.ELF`), or 0.
    /// Lives in bootloader-reclaimable memory: copy it before reclaiming.
    pub init_module: u64,
    pub init_module_size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootInfoError {
    BadMagic,
    VersionMismatch { found: u32 },
    SizeMismatch { found: u32 },
    EntrySizeMismatch { found: u64 },
    EmptyMemoryMap,
}

impl BootInfo {
    /// Checks the header fields. Does not dereference any pointer in the structure.
    pub fn validate(&self) -> Result<(), BootInfoError> {
        if self.magic != BOOTINFO_MAGIC {
            return Err(BootInfoError::BadMagic);
        }
        if self.version != BOOTINFO_VERSION {
            return Err(BootInfoError::VersionMismatch { found: self.version });
        }
        if self.size as usize != core::mem::size_of::<BootInfo>() {
            return Err(BootInfoError::SizeMismatch { found: self.size });
        }
        if self.memory_map.entry_size != core::mem::size_of::<MemoryDescriptor>() as u64 {
            return Err(BootInfoError::EntrySizeMismatch { found: self.memory_map.entry_size });
        }
        if self.memory_map.entries == 0 || self.memory_map.count == 0 {
            return Err(BootInfoError::EmptyMemoryMap);
        }
        Ok(())
    }

    pub fn has_framebuffer(&self) -> bool {
        self.flags & FLAG_FRAMEBUFFER != 0
    }

    /// # Safety
    /// `memory_map.entries` must point to `memory_map.count` valid, readable
    /// `MemoryDescriptor`s that stay unmodified for the returned lifetime
    /// (guaranteed by the bootloader until the kernel reclaims that memory).
    pub unsafe fn memory_map<'a>(&self) -> &'a [MemoryDescriptor] {
        core::slice::from_raw_parts(self.memory_map.entries as *const MemoryDescriptor, self.memory_map.count as usize)
    }

    /// # Safety
    /// `cmdline_ptr` must be 0 or point to `cmdline_len` readable bytes that
    /// stay unmodified for the returned lifetime.
    pub unsafe fn cmdline<'a>(&self) -> &'a str {
        if self.cmdline_ptr == 0 || self.cmdline_len == 0 {
            return "";
        }
        let bytes = core::slice::from_raw_parts(self.cmdline_ptr as *const u8, self.cmdline_len as usize);
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    fn sample() -> BootInfo {
        BootInfo {
            magic: BOOTINFO_MAGIC,
            version: BOOTINFO_VERSION,
            size: size_of::<BootInfo>() as u32,
            bootloader_version: 1,
            flags: 0,
            framebuffer: FramebufferInfo {
                base: 0,
                size: 0,
                width: 0,
                height: 0,
                stride: 0,
                format: PixelFormat::BltOnly,
            },
            memory_map: MemoryMapInfo { entries: 0x1000, count: 1, entry_size: size_of::<MemoryDescriptor>() as u64 },
            kernel: KernelImageInfo { phys_base: 0, size: 0, virt_base: 0, entry_point: 0 },
            acpi_rsdp: 0,
            cmdline_ptr: 0,
            cmdline_len: 0,
            hhdm_offset: 0,
            hhdm_size: 0,
            init_module: 0,
            init_module_size: 0,
        }
    }

    #[test]
    fn layout_is_stable() {
        assert_eq!(size_of::<MemoryDescriptor>(), 24);
        assert_eq!(size_of::<FramebufferInfo>(), 32);
        assert_eq!(size_of::<MemoryMapInfo>(), 24);
        assert_eq!(size_of::<KernelImageInfo>(), 32);
        assert_eq!(size_of::<BootInfo>(), 168);
        assert_eq!(align_of::<BootInfo>(), 8);
    }

    #[test]
    fn valid_header_passes() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn rejects_bad_header() {
        let mut b = sample();
        b.magic ^= 1;
        assert_eq!(b.validate(), Err(BootInfoError::BadMagic));

        let mut b = sample();
        b.version = 99;
        assert_eq!(b.validate(), Err(BootInfoError::VersionMismatch { found: 99 }));

        let mut b = sample();
        b.size = 1;
        assert_eq!(b.validate(), Err(BootInfoError::SizeMismatch { found: 1 }));

        let mut b = sample();
        b.memory_map.count = 0;
        assert_eq!(b.validate(), Err(BootInfoError::EmptyMemoryMap));

        let mut b = sample();
        b.memory_map.entry_size = 8;
        assert_eq!(b.validate(), Err(BootInfoError::EntrySizeMismatch { found: 8 }));
    }
}
