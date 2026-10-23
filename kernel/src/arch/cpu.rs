//! Thin wrappers around privileged x86-64 instructions and CPU identification.

use core::arch::asm;
use core::fmt;

/// # Safety
/// Port I/O can reconfigure hardware; callers must own the device.
#[inline]
pub unsafe fn outb(port: u16, v: u8) {
    asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn outl(port: u16, v: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

/// # Safety
/// Reading an MSR that does not exist raises #GP.
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    (hi as u64) << 32 | lo as u64
}

/// # Safety
/// Writing MSRs reconfigures the CPU; the caller must know the semantics.
pub unsafe fn wrmsr(msr: u32, v: u64) {
    asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nomem, nostack, preserves_flags));
}

/// Enables SSE for user code: CR0.EM = 0, CR0.MP = 1, CR4.OSFXSR and
/// CR4.OSXMMEXCPT = 1. The kernel itself is compiled soft-float and never
/// touches x87/SSE state except through `fxsave`/`fxrstor` on context switch.
///
/// # Safety
/// Must run once during early boot.
pub unsafe fn enable_sse() {
    asm!(
        "mov {t}, cr0",
        "and {t}, ~4",
        "or {t}, 2",
        "mov cr0, {t}",
        "mov {t}, cr4",
        "or {t}, 0x600",
        "mov cr4, {t}",
        t = out(reg) _,
        options(nostack)
    );
}

/// Saves the x87/SSE state into a 512-byte, 16-byte aligned area.
///
/// # Safety
/// `area` must be valid for 512 bytes of writes and 16-byte aligned; SSE must
/// be enabled.
pub unsafe fn fxsave(area: *mut u8) {
    asm!("fxsave64 [{}]", in(reg) area, options(nostack, preserves_flags));
}

/// Loads x87/SSE state saved by [`fxsave`] (or a valid initial image).
///
/// # Safety
/// `area` must hold a valid FXSAVE image, 16-byte aligned.
pub unsafe fn fxrstor(area: *const u8) {
    asm!("fxrstor64 [{}]", in(reg) area, options(nostack, preserves_flags));
}

pub fn read_cr2() -> u64 {
    let v;
    // SAFETY: reading CR2 has no side effects in ring 0.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr3() -> u64 {
    let v;
    // SAFETY: reading CR3 has no side effects in ring 0.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// `v` must point to a valid PML4 that maps the running kernel.
pub unsafe fn write_cr3(v: u64) {
    asm!("mov cr3, {}", in(reg) v, options(nostack, preserves_flags));
}

pub fn read_cr0() -> u64 {
    let v;
    // SAFETY: as above.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn invlpg(va: u64) {
    // SAFETY: invalidating a TLB entry is always safe.
    unsafe { asm!("invlpg [{}]", in(reg) va, options(nostack, preserves_flags)) };
}

pub fn interrupts_enabled() -> bool {
    let flags: u64;
    // SAFETY: reads RFLAGS.
    unsafe { asm!("pushfq; pop {}", out(reg) flags, options(nomem, preserves_flags)) };
    flags & (1 << 9) != 0
}

pub fn disable_interrupts() {
    // SAFETY: masking interrupts cannot break memory safety.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

pub fn enable_interrupts() {
    // SAFETY: the IDT is installed before anything calls this.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Atomically enables interrupts and halts until the next one.
pub fn enable_and_halt() {
    // SAFETY: `sti; hlt` is the canonical race-free idle sequence.
    unsafe { asm!("sti; hlt", options(nomem, nostack)) };
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: halting with interrupts off parks the CPU.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

pub fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    #[allow(unused_unsafe)]
    // SAFETY: CPUID exists on every x86-64 CPU.
    let r = unsafe { core::arch::x86_64::__cpuid_count(leaf, sub) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

pub struct CpuInfo {
    pub vendor: [u8; 12],
    pub brand: [u8; 48],
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub ecx1: u32,
    pub edx1: u32,
    pub ebx7: u32,
    pub edx_ext: u32,
}

impl CpuInfo {
    pub fn detect() -> CpuInfo {
        let (max_leaf, b, c, d) = cpuid(0, 0);
        let mut vendor = [0u8; 12];
        vendor[0..4].copy_from_slice(&b.to_le_bytes());
        vendor[4..8].copy_from_slice(&d.to_le_bytes());
        vendor[8..12].copy_from_slice(&c.to_le_bytes());
        let (eax1, _, ecx1, edx1) = cpuid(1, 0);
        let base_family = (eax1 >> 8) & 0xF;
        let family = if base_family == 0xF { base_family + ((eax1 >> 20) & 0xFF) } else { base_family };
        let model = if base_family == 0x6 || base_family == 0xF {
            ((eax1 >> 4) & 0xF) | (((eax1 >> 16) & 0xF) << 4)
        } else {
            (eax1 >> 4) & 0xF
        };
        let ebx7 = if max_leaf >= 7 { cpuid(7, 0).1 } else { 0 };
        let (max_ext, ..) = cpuid(0x8000_0000, 0);
        let edx_ext = if max_ext >= 0x8000_0001 { cpuid(0x8000_0001, 0).3 } else { 0 };
        let mut brand = [0u8; 48];
        if max_ext >= 0x8000_0004 {
            for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
                let (a, b, c, d) = cpuid(leaf, 0);
                for (j, r) in [a, b, c, d].iter().enumerate() {
                    brand[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&r.to_le_bytes());
                }
            }
        }
        CpuInfo { vendor, brand, family, model, stepping: eax1 & 0xF, ecx1, edx1, ebx7, edx_ext }
    }

    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("?")
    }

    pub fn brand_str(&self) -> &str {
        let end = self.brand.iter().position(|&b| b == 0).unwrap_or(48);
        core::str::from_utf8(&self.brand[..end]).unwrap_or("?").trim()
    }

    pub fn has_nx(&self) -> bool {
        self.edx_ext & (1 << 20) != 0
    }

    pub fn has_1g_pages(&self) -> bool {
        self.edx_ext & (1 << 26) != 0
    }

    pub fn has_apic(&self) -> bool {
        self.edx1 & (1 << 9) != 0
    }
}

impl fmt::Display for CpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let feats: [(&str, bool); 12] = [
            ("sse2", self.edx1 & (1 << 26) != 0),
            ("sse4.2", self.ecx1 & (1 << 20) != 0),
            ("avx", self.ecx1 & (1 << 28) != 0),
            ("avx2", self.ebx7 & (1 << 5) != 0),
            ("apic", self.has_apic()),
            ("x2apic", self.ecx1 & (1 << 21) != 0),
            ("nx", self.has_nx()),
            ("1g-pages", self.has_1g_pages()),
            ("rdrand", self.ecx1 & (1 << 30) != 0),
            ("smep", self.ebx7 & (1 << 7) != 0),
            ("smap", self.ebx7 & (1 << 20) != 0),
            ("syscall", self.edx_ext & (1 << 11) != 0),
        ];
        write!(
            f,
            "{} \"{}\" family {:#x} model {:#x} stepping {} features:",
            self.vendor_str(),
            self.brand_str(),
            self.family,
            self.model,
            self.stepping
        )?;
        for (name, on) in feats {
            if on {
                write!(f, " {name}")?;
            }
        }
        Ok(())
    }
}
