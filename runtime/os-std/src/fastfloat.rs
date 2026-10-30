//! Hardware double and single precision arithmetic for the soft-float user
//! target.
//!
//! User programs are built for `x86_64-unknown-none`, whose compiler lowers
//! every floating-point operation to a `compiler_builtins` routine written
//! in integer code (`__adddf3`, `__muldf3`...). A Zen profile showed those
//! routines using about a third of all user-mode CPU time: the Windows
//! compatibility layer implements the C math library in Rust, and a browser
//! calls it constantly. The kernel enables SSE and saves the SSE state of
//! every thread, so the same operations can simply run on the hardware.
//!
//! The soft-float ABI passes a `f64` as its bit pattern in a general
//! register (`rdi`, `rsi`; result in `rax`) and an `f32` in the low half of
//! `edi`/`esi`/`eax`. These definitions take over from the compiler's own
//! (an object that defines a symbol is used before an archive member that
//! would define it again). They use only `xmm0` and `xmm1`, which are
//! caller-saved in both the System V and the Windows x64 calling
//! conventions, and leave MXCSR alone (round to nearest, exceptions masked:
//! the state every thread starts with, and what the soft routines compute).
//! Comparisons stay in `compiler_builtins`: they are cheap integer code.
use core::arch::global_asm;

macro_rules! binary_f64 {
    ($($name:literal => $op:literal),*) => {
        global_asm!($(concat!(
            ".section .text.", $name, ",\"ax\",@progbits\n",
            ".global ", $name, "\n",
            ".type ", $name, ",@function\n",
            $name, ":\n",
            "    movq xmm0, rdi\n",
            "    movq xmm1, rsi\n",
            "    ", $op, " xmm0, xmm1\n",
            "    movq rax, xmm0\n",
            "    ret\n",
            ".size ", $name, ", . - ", $name, "\n"
        )),*);
    };
}

macro_rules! binary_f32 {
    ($($name:literal => $op:literal),*) => {
        global_asm!($(concat!(
            ".section .text.", $name, ",\"ax\",@progbits\n",
            ".global ", $name, "\n",
            ".type ", $name, ",@function\n",
            $name, ":\n",
            "    movd xmm0, edi\n",
            "    movd xmm1, esi\n",
            "    ", $op, " xmm0, xmm1\n",
            "    movd eax, xmm0\n",
            "    ret\n",
            ".size ", $name, ", . - ", $name, "\n"
        )),*);
    };
}

binary_f64!("__adddf3" => "addsd", "__subdf3" => "subsd", "__muldf3" => "mulsd", "__divdf3" => "divsd");
binary_f32!("__addsf3" => "addss", "__subsf3" => "subss", "__mulsf3" => "mulss", "__divsf3" => "divss");

// Conversions that are exact on the hardware: widening, narrowing with the
// current rounding mode, and signed integers. Out-of-range float to integer
// results are unspecified for these routines (the compiler's saturating
// casts clamp around them).
global_asm!(
    ".section .text.__extendsfdf2,\"ax\",@progbits",
    ".global __extendsfdf2",
    ".type __extendsfdf2,@function",
    "__extendsfdf2:",
    "    movd xmm0, edi",
    "    cvtss2sd xmm0, xmm0",
    "    movq rax, xmm0",
    "    ret",
    ".section .text.__truncdfsf2,\"ax\",@progbits",
    ".global __truncdfsf2",
    ".type __truncdfsf2,@function",
    "__truncdfsf2:",
    "    movq xmm0, rdi",
    "    cvtsd2ss xmm0, xmm0",
    "    movd eax, xmm0",
    "    ret",
    ".section .text.__floatsidf,\"ax\",@progbits",
    ".global __floatsidf",
    ".type __floatsidf,@function",
    "__floatsidf:",
    "    cvtsi2sd xmm0, edi",
    "    movq rax, xmm0",
    "    ret",
    ".section .text.__floatdidf,\"ax\",@progbits",
    ".global __floatdidf",
    ".type __floatdidf,@function",
    "__floatdidf:",
    "    cvtsi2sd xmm0, rdi",
    "    movq rax, xmm0",
    "    ret",
    ".section .text.__floatsisf,\"ax\",@progbits",
    ".global __floatsisf",
    ".type __floatsisf,@function",
    "__floatsisf:",
    "    cvtsi2ss xmm0, edi",
    "    movd eax, xmm0",
    "    ret",
    ".section .text.__floatdisf,\"ax\",@progbits",
    ".global __floatdisf",
    ".type __floatdisf,@function",
    "__floatdisf:",
    "    cvtsi2ss xmm0, rdi",
    "    movd eax, xmm0",
    "    ret",
    ".section .text.__fixdfsi,\"ax\",@progbits",
    ".global __fixdfsi",
    ".type __fixdfsi,@function",
    "__fixdfsi:",
    "    movq xmm0, rdi",
    "    cvttsd2si eax, xmm0",
    "    ret",
    ".section .text.__fixdfdi,\"ax\",@progbits",
    ".global __fixdfdi",
    ".type __fixdfdi,@function",
    "__fixdfdi:",
    "    movq xmm0, rdi",
    "    cvttsd2si rax, xmm0",
    "    ret",
    ".section .text.__fixsfsi,\"ax\",@progbits",
    ".global __fixsfsi",
    ".type __fixsfsi,@function",
    "__fixsfsi:",
    "    movd xmm0, edi",
    "    cvttss2si eax, xmm0",
    "    ret",
    ".section .text.__fixsfdi,\"ax\",@progbits",
    ".global __fixsfdi",
    ".type __fixsfdi,@function",
    "__fixsfdi:",
    "    movd xmm0, edi",
    "    cvttss2si rax, xmm0",
    "    ret",
);
