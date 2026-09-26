//! Message forwarding through `-forwardingTargetForSelector:`.
//!
//! With `objc_msg_lookup` the caller calls the implementation itself, with
//! the receiver it already has, so forwarding a message to another object
//! means changing the receiver on the way. When a class overrides
//! `-forwardingTargetForSelector:` (or `+forwardingTargetForSelector:` for
//! class messages), a selector it doesn't implement resolves to
//! [`trampoline`]: a few instructions of assembly that save the argument
//! registers, ask [`resolve`] for the target and its implementation, put
//! the target where the receiver was, restore the rest and jump. The
//! arguments, stack arguments and return value pass through untouched, so
//! any signature forwards.
//!
//! The trampoline is cached like any implementation, but the target is not:
//! `-forwardingTargetForSelector:` is asked on every message, since its
//! answer may change. A nil target (or the receiver itself) ends in
//! `-doesNotRecognizeSelector:`. `-forwardInvocation:`, which needs
//! `NSInvocation`, is not supported.
//!
//! The trampolines carry unwind information, so a panic in
//! `-forwardingTargetForSelector:` or `-doesNotRecognizeSelector:` unwinds
//! through them into the sender.

use std::mem::transmute;

use crate::Imp;
use crate::class::{Class, lookup_imp};
use crate::message::objc_msg_lookup;
use crate::object::{Object, isa_relaxed};
use crate::selector::{self, Sel, known};

type Id = *mut Object;

/// The trampoline, on architectures that have one.
pub(crate) fn trampoline() -> Option<Imp> {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        unsafe extern "C" {
            fn sidestep_forward_trampoline();
        }
        // SAFETY: the trampoline is only ever called as a method, and
        // passes the call on to one.
        Some(unsafe { transmute::<unsafe extern "C" fn(), Imp>(sidestep_forward_trampoline) })
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    None
}

/// Whether `imp` is the trampoline.
#[inline]
pub(crate) fn is_trampoline(imp: Imp) -> bool {
    trampoline().is_some_and(|t| t as usize == imp as usize)
}

/// Whether messages that `cls` (a class, or a metaclass for class
/// messages) doesn't implement should go through the trampoline: whether
/// it overrides the root class's `forwardingTargetForSelector:`, which
/// answers nil.
pub(crate) fn forwards(cls: &'static Class) -> bool {
    lookup_imp(cls, known().forwarding_target).is_some_and(|imp| !crate::nsobject::is_default_forwarding_target(imp))
}

/// Where a trampoline saved the integer argument registers, in order:
/// x0 to x8 on aarch64 (x8 holds the address for a returned struct), and
/// rdi, rsi, rdx, rcx, r8, r9, rax on x86_64.
type Saved = [usize; 9];

/// Finds the target and implementation for a forwarded message, writes
/// the target into the saved receiver register and returns the
/// implementation to jump to.
///
/// # Safety
/// Called only by the trampolines, with the registers of a message send.
unsafe extern "C-unwind" fn resolve(saved: &mut Saved) -> Imp {
    // On x86_64, a method returning a large struct takes the address to
    // write it to first, moving the receiver and selector along by one.
    // The second argument is either the selector or, in that case, the
    // receiver, which is never a selector's address.
    let at = if cfg!(target_arch = "x86_64") && !selector::is_selector(saved[1]) { 1 } else { 0 };
    let (receiver, sel) = (saved[at] as Id, saved[at + 1] as Sel);
    // SAFETY: a message's receiver is live, and its selector valid.
    unsafe {
        let target = forwarding_target(receiver, sel);
        if !target.is_null() && target != receiver {
            saved[at] = target as usize;
            return objc_msg_lookup(target, sel).expect("lookup never fails");
        }
        does_not_recognize(receiver, sel)
    }
}

/// `[receiver forwardingTargetForSelector:sel]`.
unsafe fn forwarding_target(receiver: Id, sel: Sel) -> Id {
    let ask = known().forwarding_target;
    // SAFETY: the caller passes a live receiver; the method takes a
    // selector and returns an object.
    unsafe {
        let imp = objc_msg_lookup(receiver, ask).expect("lookup never fails");
        let imp: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = transmute(imp);
        imp(receiver, ask, sel)
    }
}

/// `[receiver doesNotRecognizeSelector:sel]`, which raises (panics). A
/// class that overrides it to return has nothing to return to.
unsafe fn does_not_recognize(receiver: Id, sel: Sel) -> ! {
    let report = known().does_not_recognize;
    // SAFETY: the caller passes a live receiver; the method takes a
    // selector.
    unsafe {
        let imp = objc_msg_lookup(receiver, report).expect("lookup never fails");
        let imp: unsafe extern "C-unwind" fn(Id, Sel, Sel) = transmute(imp);
        imp(receiver, report, sel);
    }
    // SAFETY: as above.
    let cls = unsafe { isa_relaxed(receiver) };
    // SAFETY: the selector came from the runtime.
    let name = unsafe { selector::name(sel) };
    panic!(
        "{}[{} {}]: message could not be forwarded, and -doesNotRecognizeSelector: returned",
        if cls.is_meta() { '+' } else { '-' },
        cls.instance_class().name().to_string_lossy(),
        name.to_string_lossy()
    );
}

// The frame: x29/x30, then x0-x8 at sp+16 (the `Saved` array), then q0-q7
// at sp+96. `bti c` lets the trampoline be called indirectly under branch
// target identification, and is a no-op elsewhere.
#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl sidestep_forward_trampoline",
    ".hidden sidestep_forward_trampoline",
    ".type sidestep_forward_trampoline, %function",
    "sidestep_forward_trampoline:",
    ".cfi_startproc",
    "hint #34",
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
    "add x0, sp, #16",
    "bl {resolve}",
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
    ".cfi_endproc",
    ".size sidestep_forward_trampoline, . - sidestep_forward_trampoline",
    resolve = sym resolve,
);

// The frame: rbp, then xmm0-xmm7 at rsp, then rdi, rsi, rdx, rcx, r8, r9
// and rax (the vector register count for variadic calls) at rsp+128, the
// `Saved` array. rsp stays 16-byte aligned for the call.
#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl sidestep_forward_trampoline",
    ".hidden sidestep_forward_trampoline",
    ".type sidestep_forward_trampoline, @function",
    "sidestep_forward_trampoline:",
    ".cfi_startproc",
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
    "lea rdi, [rsp + 128]",
    "call {resolve}",
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
    ".cfi_endproc",
    ".size sidestep_forward_trampoline, . - sidestep_forward_trampoline",
    resolve = sym resolve,
);
