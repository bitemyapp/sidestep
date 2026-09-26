//! The one frame shared by the assembly entry points that run Rust code
//! before jumping to a method: the forwarding trampoline (`forward`) and
//! `objc_msgSend` (`msgsend`). Each saves every register a message's
//! arguments may be in, calls a Rust function, restores them and jumps to
//! the implementation the function returned, so arguments, stack arguments
//! and struct returns pass through untouched. The frame carries unwind
//! information, so a panic in the Rust function unwinds through it into
//! the sender.
//!
//! [`saving_entry!`] takes the entry's name and extra symbol directives,
//! code to run on entry before the frame (which may branch to the local
//! label `1`), code setting up the call's arguments (the message's are
//! still in their registers), the function to call, and code placed after
//! the jump (the code at label `1`).

/// aarch64. The frame: x29/x30, then x0-x8 at sp+16 (x8 holds the address
/// of a struct returned in memory), then q0-q7 at sp+96. `bti c`
/// (`hint #34`) lets the entry be called indirectly under branch target
/// identification, and is a no-op elsewhere.
#[cfg(target_arch = "aarch64")]
macro_rules! saving_entry {
    (
        name: $name:literal,
        directives: [$($directive:literal),* $(,)?],
        before: [$($before:literal),* $(,)?],
        setup: [$($setup:literal),* $(,)?],
        call: $callee:path,
        after: [$($after:literal),* $(,)?] $(,)?
    ) => {
        std::arch::global_asm!(
            ".text",
            ".p2align 2",
            concat!(".globl ", $name),
            $($directive,)*
            concat!(".type ", $name, ", %function"),
            concat!($name, ":"),
            ".cfi_startproc",
            "hint #34",
            $($before,)*
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
            $($setup,)*
            "bl {callee}",
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
            $($after,)*
            ".cfi_endproc",
            concat!(".size ", $name, ", . - ", $name),
            callee = sym $callee,
        );
    };
}

/// x86_64. The frame: rbp, then xmm0-xmm7 at rsp, then rdi, rsi, rdx,
/// rcx, r8, r9 and rax (the vector register count of a variadic call) at
/// rsp+128, with room after them to read the block as nine words. rsp
/// stays 16-byte aligned for the call.
#[cfg(target_arch = "x86_64")]
macro_rules! saving_entry {
    (
        name: $name:literal,
        directives: [$($directive:literal),* $(,)?],
        before: [$($before:literal),* $(,)?],
        setup: [$($setup:literal),* $(,)?],
        call: $callee:path,
        after: [$($after:literal),* $(,)?] $(,)?
    ) => {
        std::arch::global_asm!(
            ".text",
            ".p2align 4",
            concat!(".globl ", $name),
            $($directive,)*
            concat!(".type ", $name, ", @function"),
            concat!($name, ":"),
            ".cfi_startproc",
            $($before,)*
            "push rbp",
            ".cfi_def_cfa_offset 16",
            ".cfi_offset rbp, -16",
            "mov rbp, rsp",
            ".cfi_def_cfa_register rbp",
            "sub rsp, 208",
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
            $($setup,)*
            "call {callee}",
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
            $($after,)*
            ".cfi_endproc",
            concat!(".size ", $name, ", . - ", $name),
            callee = sym $callee,
        );
    };
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) use saving_entry;
