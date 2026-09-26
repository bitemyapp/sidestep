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
//! They share their frame, and its unwind tables, with the forwarding
//! trampoline (see `trampoline`), so a panic from `+initialize` or
//! `+resolveInstanceMethod:` during the lookup unwinds through them.

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

// Each saves the argument registers in the frame
// `trampoline::saving_entry!` lays out, calls `lookup` with the receiver
// and selector, restores them and jumps; a nil receiver branches to label
// 1 before the frame is made.
#[cfg(target_arch = "aarch64")]
crate::trampoline::saving_entry!(
    name: "objc_msgSend",
    directives: [],
    before: ["cbz x0, 1f"],
    setup: [],
    call: lookup,
    after: ["1:", "mov x1, xzr", "movi d0, #0", "movi d1, #0", "movi d2, #0", "movi d3, #0", "ret"],
);

#[cfg(target_arch = "x86_64")]
crate::trampoline::saving_entry!(
    name: "objc_msgSend",
    directives: [],
    before: ["test rdi, rdi", "jz 1f"],
    setup: [],
    call: lookup,
    after: ["1:", "xor eax, eax", "xor edx, edx", "xorps xmm0, xmm0", "xorps xmm1, xmm1", "ret"],
);

#[cfg(target_arch = "x86_64")]
crate::trampoline::saving_entry!(
    name: "objc_msgSend_fpret",
    directives: [],
    before: ["test rdi, rdi", "jz 1f"],
    setup: [],
    call: lookup,
    after: ["1:", "xor eax, eax", "xor edx, edx", "fldz", "ret"],
);

// The receiver and selector come one register later, after the address of
// the struct to return, which a nil message leaves unwritten and returns
// in rax, as the calling convention asks of any function returning a
// struct in memory.
#[cfg(target_arch = "x86_64")]
crate::trampoline::saving_entry!(
    name: "objc_msgSend_stret",
    directives: [],
    before: ["test rsi, rsi", "jz 1f"],
    setup: ["mov rdi, rsi", "mov rsi, rdx"],
    call: lookup,
    after: ["1:", "mov rax, rdi", "ret"],
);

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
