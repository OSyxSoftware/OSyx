//! Shared helper: proves that the kernel preserves SSE registers per thread.

/// Loads xmm0-xmm15 with `seed + i`, runs `work`, then checks that every
/// register still holds its value.
///
/// The programs are compiled soft-float, so no compiler-generated code (in
/// `work` or OSyx Std) touches XMM registers; any change would come from the
/// kernel failing to save and restore this thread's state while other
/// threads that use XMM run in between.
pub fn preserved_across(seed: u64, work: impl FnOnce()) -> bool {
    let mut vals = [0u64; 16];
    for (i, v) in vals.iter_mut().enumerate() {
        *v = seed + i as u64;
    }
    // SAFETY: SSE is enabled by the kernel for user code; reads 16 u64.
    unsafe {
        core::arch::asm!(
            "movq xmm0, [{p}]", "movq xmm1, [{p} + 8]", "movq xmm2, [{p} + 16]", "movq xmm3, [{p} + 24]",
            "movq xmm4, [{p} + 32]", "movq xmm5, [{p} + 40]", "movq xmm6, [{p} + 48]", "movq xmm7, [{p} + 56]",
            "movq xmm8, [{p} + 64]", "movq xmm9, [{p} + 72]", "movq xmm10, [{p} + 80]", "movq xmm11, [{p} + 88]",
            "movq xmm12, [{p} + 96]", "movq xmm13, [{p} + 104]", "movq xmm14, [{p} + 112]", "movq xmm15, [{p} + 120]",
            p = in(reg) vals.as_ptr(),
        );
    }
    work();
    let mut out = [0u64; 16];
    // SAFETY: as above; stores 16 u64 values into `out`.
    unsafe {
        core::arch::asm!(
            "movq [{p}], xmm0", "movq [{p} + 8], xmm1", "movq [{p} + 16], xmm2", "movq [{p} + 24], xmm3",
            "movq [{p} + 32], xmm4", "movq [{p} + 40], xmm5", "movq [{p} + 48], xmm6", "movq [{p} + 56], xmm7",
            "movq [{p} + 64], xmm8", "movq [{p} + 72], xmm9", "movq [{p} + 80], xmm10", "movq [{p} + 88], xmm11",
            "movq [{p} + 96], xmm12", "movq [{p} + 104], xmm13", "movq [{p} + 112], xmm14", "movq [{p} + 120], xmm15",
            p = in(reg) out.as_mut_ptr(),
        );
    }
    out == vals
}
