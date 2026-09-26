//! `objc_msgSend` and its x86_64 variants, for code that calls them
//! directly through objc2's FFI declarations. objc2 itself sends every
//! message with `objc_msg_lookup` on this ABI, which its callers can
//! inline; these serve C code and code written against the C API.
//!
//! Each probes the receiver's method cache in assembly, exactly as
//! `cache::probe` does: the class's cache word, the slot the selector's
//! address picks (masked by the word's top 16 bits), a hit when the slot's
//! tag XOR its implementation is the selector, the next slot on a
//! collision, and a miss at an empty slot. A hit jumps straight to the
//! implementation with every argument register untouched. The probe uses
//! only scratch registers: x9-x17 on aarch64; on x86_64, r10 and r11 and
//! two more kept below the stack pointer (the red zone, which a function
//! that calls nothing may use), since rax carries the number of vector
//! registers a variadic method's arguments use.
//!
//! A miss (a class not yet initialized, or a selector not yet cached)
//! takes the slow path: save the argument registers, call
//! `objc_msg_lookup`, restore them and jump to what it returns.
//!
//! A message to nil returns zero in the integer and floating-point return
//! registers (and, for `objc_msgSend_fpret`, on the x87 stack) and writes
//! nothing through a struct return address, as on Apple's runtime.
//!
//! The slow path shares its frame, and its unwind tables, with the
//! forwarding trampoline (see `trampoline`), so a panic from `+initialize`
//! or `+resolveInstanceMethod:` during the lookup unwinds through it.

use crate::Imp;
use crate::class::Class;
use crate::message::objc_msg_lookup;
use crate::object::Object;
use crate::selector::Sel;

/// Where a class keeps its cache word, for the probes below.
const CACHE: usize = std::mem::offset_of!(Class, cache);

/// Looks up the method for the receiver and selector the entry point
/// saved.
///
/// # Safety
/// Called only by the entry points below, with a message's registers.
unsafe extern "C-unwind" fn lookup(receiver: *mut Object, sel: Sel) -> Imp {
    // SAFETY: a message's receiver is live, and its selector valid.
    unsafe { objc_msg_lookup(receiver, sel) }.expect("lookup never fails")
}

// Each probes the cache before the frame (falling through into it on a
// miss), and on a miss saves the argument registers in the frame
// `trampoline::saving_entry!` lays out, calls `lookup` with the receiver
// and selector, restores them and jumps; a nil receiver branches to label
// 1 first.
#[cfg(target_arch = "aarch64")]
crate::trampoline::saving_entry!(
    name: "objc_msgSend",
    directives: [],
    before: [
        "cbz x0, 1f",
        "ldr x9, [x0]",
        "ldr x10, [x9, #{cache}]",
        // The mask, in bytes of slots (16 each), and the slots.
        "lsr x11, x10, #48",
        "lsl x11, x11, #4",
        "and x10, x10, #0xffffffffffff",
        // Selectors are 16-byte aligned, so this is (sel >> 4 & mask) * 16.
        "and x12, x1, x11",
        "2:",
        "add x13, x10, x12",
        "ldp x14, x15, [x13]",
        "eor x16, x14, x15",
        "cmp x16, x1",
        "b.ne 3f",
        "mov x16, x15",
        "br x16",
        "3:",
        "cbz x14, 4f",
        "add x12, x12, #16",
        "and x12, x12, x11",
        "b 2b",
        "4:",
    ],
    setup: [],
    call: lookup,
    after: ["1:", "mov x1, xzr", "movi d0, #0", "movi d1, #0", "movi d2, #0", "movi d3, #0", "ret"],
    consts: [cache = CACHE],
);

/// An x86_64 entry point probing the cache for a receiver in `$receiver`
/// and a selector in `$sel`. rax and rbx are kept in the red zone while
/// the probe runs.
#[cfg(target_arch = "x86_64")]
macro_rules! probing_entry_x86_64 {
    (
        name: $name:literal,
        receiver: $receiver:literal,
        sel: $sel:literal,
        setup: [$($setup:literal),* $(,)?],
        after: [$($after:literal),* $(,)?] $(,)?
    ) => {
        crate::trampoline::saving_entry!(
            name: $name,
            directives: [],
            before: [
                concat!("test ", $receiver, ", ", $receiver),
                "jz 1f",
                concat!("mov r10, qword ptr [", $receiver, "]"),
                "mov r10, qword ptr [r10 + {cache}]",
                "mov qword ptr [rsp - 8], rax",
                "mov qword ptr [rsp - 16], rbx",
                // The mask, in bytes of slots (16 each), and the slots.
                "mov r11, r10",
                "shr r11, 48",
                "shl r11, 4",
                "shl r10, 16",
                "shr r10, 16",
                // Selectors are 16-byte aligned, so this is
                // (sel >> 4 & mask) * 16.
                concat!("mov rax, ", $sel),
                "and rax, r11",
                "2:",
                "mov rbx, qword ptr [r10 + rax]",
                "xor rbx, qword ptr [r10 + rax + 8]",
                concat!("cmp rbx, ", $sel),
                "jne 3f",
                // A slot is written once, so its implementation reads the
                // same again.
                "mov r11, qword ptr [r10 + rax + 8]",
                "mov rbx, qword ptr [rsp - 16]",
                "mov rax, qword ptr [rsp - 8]",
                "jmp r11",
                "3:",
                "cmp qword ptr [r10 + rax], 0",
                "je 4f",
                "add rax, 16",
                "and rax, r11",
                "jmp 2b",
                "4:",
                "mov rbx, qword ptr [rsp - 16]",
                "mov rax, qword ptr [rsp - 8]",
            ],
            setup: [$($setup),*],
            call: lookup,
            after: [$($after),*],
            consts: [cache = CACHE],
        );
    };
}

#[cfg(target_arch = "x86_64")]
probing_entry_x86_64!(
    name: "objc_msgSend",
    receiver: "rdi",
    sel: "rsi",
    setup: [],
    after: ["1:", "xor eax, eax", "xor edx, edx", "xorps xmm0, xmm0", "xorps xmm1, xmm1", "ret"],
);

#[cfg(target_arch = "x86_64")]
probing_entry_x86_64!(
    name: "objc_msgSend_fpret",
    receiver: "rdi",
    sel: "rsi",
    setup: [],
    after: ["1:", "xor eax, eax", "xor edx, edx", "fldz", "ret"],
);

// The receiver and selector come one register later, after the address of
// the struct to return, which a nil message leaves unwritten and returns
// in rax, as the calling convention asks of any function returning a
// struct in memory.
#[cfg(target_arch = "x86_64")]
probing_entry_x86_64!(
    name: "objc_msgSend_stret",
    receiver: "rsi",
    sel: "rdx",
    setup: ["mov rdi, rsi", "mov rsi, rdx"],
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
