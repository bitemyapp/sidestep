//! `objc_msgSend` and its x86_64 variants, for code that calls them
//! directly through objc2's FFI declarations. objc2 itself sends every
//! message with `objc_msg_lookup` on this ABI, which is faster: these
//! entry points save the argument registers, call `objc_msg_lookup` and
//! jump to what it returns, so they cost a few nanoseconds more.
//!
//! A message to nil returns zero in the integer and floating-point return
//! registers (and, for `objc_msgSend_fpret`, on the x87 stack) and writes
//! nothing through a struct return address, as on Apple's runtime.
//!
//! Like the forwarding trampolines, these carry unwind tables, so a panic
//! from `+initialize` or `+resolveInstanceMethod:` during the lookup
//! unwinds through them.

use crate::Imp;
use crate::message::objc_msg_lookup;
use crate::object::Object;
use crate::selector::Sel;

/// Looks up the method for the receiver and selector the entry point
/// saved.
///
/// # Safety
/// Called only by the entry points below, with a message's registers.
unsafe extern "C-unwind" fn lookup(receiver: *mut Object, sel: Sel) -> Imp {
    // SAFETY: a message's receiver is live, and its selector valid.
    unsafe { objc_msg_lookup(receiver, sel) }.expect("lookup never fails")
}

#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl objc_msgSend",
    ".type objc_msgSend, %function",
    "objc_msgSend:",
    ".cfi_startproc",
    "hint #34",
    "cbz x0, 1f",
    "stp x29, x30, [sp, #-224]!",
    ".cfi_def_cfa_offset 224",
    ".cfi_offset x29, -224",
    ".cfi_offset x30, -216",
    "mov x29, sp",
    ".cfi_def_cfa_register x29",
    "stp x0, x1, [sp, #16]",
    "stp x2, x3, [sp, #32]",
    "stp x4, x5, [sp, #48]",
    "stp x6, x7, [sp, #64]",
    "str x8, [sp, #80]",
    "stp q0, q1, [sp, #96]",
    "stp q2, q3, [sp, #128]",
    "stp q4, q5, [sp, #160]",
    "stp q6, q7, [sp, #192]",
    "bl {lookup}",
    "mov x16, x0",
    "ldp q6, q7, [sp, #192]",
    "ldp q4, q5, [sp, #160]",
    "ldp q2, q3, [sp, #128]",
    "ldp q0, q1, [sp, #96]",
    "ldr x8, [sp, #80]",
    "ldp x6, x7, [sp, #64]",
    "ldp x4, x5, [sp, #48]",
    "ldp x2, x3, [sp, #32]",
    "ldp x0, x1, [sp, #16]",
    "ldp x29, x30, [sp], #224",
    ".cfi_def_cfa sp, 0",
    ".cfi_restore x29",
    ".cfi_restore x30",
    "br x16",
    // nil: zero the return registers.
    "1:",
    "mov x1, xzr",
    "movi d0, #0",
    "movi d1, #0",
    "movi d2, #0",
    "movi d3, #0",
    "ret",
    ".cfi_endproc",
    ".size objc_msgSend, . - objc_msgSend",
    lookup = sym lookup,
);

// Each saves rdi, rsi, rdx, rcx, r8, r9, rax (the vector register count of
// a variadic call) and xmm0-xmm7, calls `lookup` with the receiver and
// selector, restores them and jumps. `objc_msgSend_stret` finds the
// receiver and selector one register later, after the address of the
// struct to return.
#[cfg(target_arch = "x86_64")]
macro_rules! x86_64_entry {
    ($name:literal, $receiver:literal, $selector:literal, $nil:literal) => {
        std::arch::global_asm!(
            ".text",
            ".p2align 4",
            concat!(".globl ", $name),
            concat!(".type ", $name, ", @function"),
            concat!($name, ":"),
            ".cfi_startproc",
            concat!("test ", $receiver, ", ", $receiver),
            "jz 1f",
            "push rbp",
            ".cfi_def_cfa_offset 16",
            ".cfi_offset rbp, -16",
            "mov rbp, rsp",
            ".cfi_def_cfa_register rbp",
            "sub rsp, 192",
            "movdqu xmmword ptr [rsp], xmm0",
            "movdqu xmmword ptr [rsp + 16], xmm1",
            "movdqu xmmword ptr [rsp + 32], xmm2",
            "movdqu xmmword ptr [rsp + 48], xmm3",
            "movdqu xmmword ptr [rsp + 64], xmm4",
            "movdqu xmmword ptr [rsp + 80], xmm5",
            "movdqu xmmword ptr [rsp + 96], xmm6",
            "movdqu xmmword ptr [rsp + 112], xmm7",
            "mov qword ptr [rsp + 128], rdi",
            "mov qword ptr [rsp + 136], rsi",
            "mov qword ptr [rsp + 144], rdx",
            "mov qword ptr [rsp + 152], rcx",
            "mov qword ptr [rsp + 160], r8",
            "mov qword ptr [rsp + 168], r9",
            "mov qword ptr [rsp + 176], rax",
            concat!("mov rdi, ", $receiver),
            concat!("mov rsi, ", $selector),
            "call {lookup}",
            "mov r11, rax",
            "movdqu xmm0, xmmword ptr [rsp]",
            "movdqu xmm1, xmmword ptr [rsp + 16]",
            "movdqu xmm2, xmmword ptr [rsp + 32]",
            "movdqu xmm3, xmmword ptr [rsp + 48]",
            "movdqu xmm4, xmmword ptr [rsp + 64]",
            "movdqu xmm5, xmmword ptr [rsp + 80]",
            "movdqu xmm6, xmmword ptr [rsp + 96]",
            "movdqu xmm7, xmmword ptr [rsp + 112]",
            "mov rdi, qword ptr [rsp + 128]",
            "mov rsi, qword ptr [rsp + 136]",
            "mov rdx, qword ptr [rsp + 144]",
            "mov rcx, qword ptr [rsp + 152]",
            "mov r8, qword ptr [rsp + 160]",
            "mov r9, qword ptr [rsp + 168]",
            "mov rax, qword ptr [rsp + 176]",
            "leave",
            ".cfi_def_cfa rsp, 8",
            ".cfi_restore rbp",
            "jmp r11",
            "1:",
            $nil,
            "ret",
            ".cfi_endproc",
            concat!(".size ", $name, ", . - ", $name),
            lookup = sym lookup,
        );
    };
}

#[cfg(target_arch = "x86_64")]
x86_64_entry!("objc_msgSend", "rdi", "rsi", "xor eax, eax\nxor edx, edx\nxorps xmm0, xmm0\nxorps xmm1, xmm1");
#[cfg(target_arch = "x86_64")]
x86_64_entry!("objc_msgSend_fpret", "rdi", "rsi", "xor eax, eax\nxor edx, edx\nfldz");
#[cfg(target_arch = "x86_64")]
x86_64_entry!("objc_msgSend_stret", "rsi", "rdx", "");

/// On x86_64, the implementation for messages returning a struct in
/// memory. The forwarding trampoline handles both conventions, so this is
/// `class_getMethodImplementation`.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn class_getMethodImplementation_stret(
    cls: *const crate::class::Class,
    sel: Sel,
) -> Option<Imp> {
    // SAFETY: forwarded contract.
    unsafe { crate::message::class_getMethodImplementation(cls, sel) }
}
