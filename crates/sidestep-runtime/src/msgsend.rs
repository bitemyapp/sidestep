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
//! only scratch registers: x9-x17 on aarch64; on x86_64, r10 and r11, and
//! past a collision two more kept below the stack pointer (the red zone,
//! which a function that calls nothing may use), since rax carries the
//! number of vector registers a variadic method's arguments use.
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
#[cfg(target_arch = "aarch64")]
use crate::cache::ADDRESS;
#[cfg(target_arch = "x86_64")]
use crate::cache::IMP_OFFSET;
use crate::cache::{MASK_SHIFT, SLOT_SHIFT};
use crate::class::Class;
use crate::message::objc_msg_lookup;
use crate::object::Object;
use crate::selector::Sel;

/// Where a class keeps its cache word, for the probes below.
const CACHE: usize = std::mem::offset_of!(Class, cache);
/// `sel & word >> HOME_SHIFT` is the offset of a selector's home slot in
/// the table (`cache::home` times the size of a slot): the shift leaves
/// the mask, the word's top bits, scaled by a slot's size, over a few
/// address bits that meet a selector's low bits, which are zero.
const HOME_SHIFT: u32 = MASK_SHIFT - SLOT_SHIFT;
/// The size of a slot, the step from one to the next.
const SLOT: usize = 1 << SLOT_SHIFT;
/// The bits above a cache word's address, the mask's.
#[cfg(target_arch = "x86_64")]
const MASK_BITS: u32 = usize::BITS - MASK_SHIFT;

/// Looks up the method for the receiver and selector the entry point
/// saved.
///
/// # Safety
/// Called only by the entry points below, with a message's registers.
unsafe extern "C-unwind" fn lookup(receiver: *mut Object, sel: Sel) -> Imp {
    #[cfg(test)]
    tests::MISSES.with(|misses| misses.set(misses.get() + 1));
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
        // The home slot's offset (see HOME_SHIFT), then the slots.
        "and x12, x1, x10, lsr #{home_shift}",
        "and x11, x10, #{address}",
        "2:",
        "add x13, x11, x12",
        // The tag in x17, the implementation in x16, which branch target
        // identification lets `br` use.
        "ldp x17, x16, [x13]",
        "eor x9, x17, x16",
        "cmp x9, x1",
        "b.ne 3f",
        "br x16",
        "3:",
        "cbz x17, 4f",
        // The next slot, wrapping around the same way.
        "add x12, x12, #{slot}",
        "and x12, x12, x10, lsr #{home_shift}",
        "b 2b",
        "4:",
    ],
    setup: [],
    call: lookup,
    after: ["1:", "mov x1, xzr", "movi d0, #0", "movi d1, #0", "movi d2, #0", "movi d3, #0", "ret"],
    consts: [cache = CACHE, home_shift = HOME_SHIFT, address = ADDRESS, slot = SLOT],
);

/// An x86_64 entry point probing the cache for a receiver in `$receiver`
/// and a selector in `$sel`. The first slot is probed with r10 and r11
/// alone; after a collision, the probe starts again with rax and rbx too,
/// kept in the red zone meanwhile.
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
                // The home slot's offset (see HOME_SHIFT), then its
                // address: the word without the mask's bits.
                "mov r11, r10",
                "shr r11, {home_shift}",
                concat!("and r11, ", $sel),
                "shl r10, {mask_bits}",
                "shr r10, {mask_bits}",
                "add r11, r10",
                "mov r10, qword ptr [r11]",
                "xor r10, qword ptr [r11 + {imp}]",
                concat!("cmp r10, ", $sel),
                "jne 3f",
                // A slot is written once, so its implementation reads the
                // same again.
                "jmp qword ptr [r11 + {imp}]",
                "3:",
                "cmp qword ptr [r11], 0",
                "je 4f",
                // Another selector's slot: probe again from the home slot,
                // wrapping around the table, with two more registers.
                "mov qword ptr [rsp - 8], rax",
                "mov qword ptr [rsp - 16], rbx",
                concat!("mov r10, qword ptr [", $receiver, "]"),
                "mov r10, qword ptr [r10 + {cache}]",
                "mov rax, r10",
                "shr rax, {home_shift}",
                "mov r11, rax",
                concat!("and r11, ", $sel),
                "shl r10, {mask_bits}",
                "shr r10, {mask_bits}",
                "5:",
                "mov rbx, qword ptr [r10 + r11]",
                "xor rbx, qword ptr [r10 + r11 + {imp}]",
                concat!("cmp rbx, ", $sel),
                "je 6f",
                "cmp qword ptr [r10 + r11], 0",
                "je 7f",
                // The next slot's offset is a multiple of a slot's size,
                // so the address bits below the mask in `word >>
                // HOME_SHIFT` don't matter.
                "add r11, {slot}",
                "and r11, rax",
                "jmp 5b",
                "6:",
                "mov r11, qword ptr [r10 + r11 + {imp}]",
                "mov rbx, qword ptr [rsp - 16]",
                "mov rax, qword ptr [rsp - 8]",
                "jmp r11",
                "7:",
                "mov rbx, qword ptr [rsp - 16]",
                "mov rax, qword ptr [rsp - 8]",
                "4:",
            ],
            setup: [$($setup),*],
            call: lookup,
            after: [$($after),*],
            consts: [
                cache = CACHE,
                home_shift = HOME_SHIFT,
                mask_bits = MASK_BITS,
                imp = IMP_OFFSET,
                slot = SLOT,
            ],
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use objc2::encode::{Encode, Encoding};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{ClassType, msg_send, sel};

    thread_local! {
        /// Messages sent on this thread that took the slow path.
        pub(super) static MISSES: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "C-unwind" {
        fn objc_msgSend();
        #[cfg(target_arch = "x86_64")]
        fn objc_msgSend_stret();
        #[cfg(target_arch = "x86_64")]
        fn objc_msgSend_fpret();
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Wide([u64; 5]);

    // SAFETY: five integers.
    unsafe impl Encode for Wide {
        const ENCODING: Encoding = Encoding::Struct("SidestepUnitWide", &[<[u64; 5]>::ENCODING]);
    }

    /// Every selector's method: its own selector.
    extern "C-unwind" fn echo(_: &AnyObject, cmd: Sel) -> Sel {
        cmd
    }

    extern "C-unwind" fn wide(_: &AnyObject, _: Sel) -> Wide {
        Wide([1, 2, 3, 4, 5])
    }

    extern "C-unwind" fn half(_: &AnyObject, _: Sel) -> f64 {
        0.5
    }

    /// How many of the messages `send` sends take the slow path, once
    /// they have all been sent before. A method change elsewhere (another
    /// test's) empties every cache, so that measurement is taken again.
    fn misses(send: impl Fn()) -> usize {
        for _ in 0..10 {
            send();
            let (epoch, before) = (crate::cache::epoch(), MISSES.get());
            send();
            if crate::cache::epoch() == epoch {
                return MISSES.get() - before;
            }
        }
        panic!("method tables kept changing");
    }

    /// Cached messages, including ones past a collision in the cache,
    /// never leave the assembly probe.
    #[test]
    fn cached_messages_take_the_fast_path() {
        let sels: Vec<Sel> =
            (0..200).map(|i| Sel::register(&std::ffi::CString::new(format!("sidestepFast{i}")).unwrap())).collect();
        let mut builder = ClassBuilder::new(c"SidestepUnitFastPath", NSObject::class()).unwrap();
        for &sel in &sels {
            // SAFETY: the signature matches the selector.
            unsafe { builder.add_method(sel, echo as extern "C-unwind" fn(_, _) -> _) };
        }
        // SAFETY: as above.
        unsafe {
            builder.add_method(sel!(sidestepWide), wide as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(sidestepHalf), half as extern "C-unwind" fn(_, _) -> _);
        }
        let cls: &AnyClass = builder.register();
        let obj: Retained<NSObject> = unsafe { msg_send![cls, new] };
        let this: *const AnyObject = Retained::as_ptr(&obj).cast();

        // SAFETY: objc_msgSend called as each method's implementation.
        let send: unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> Sel =
            unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C-unwind" fn()) };
        let all = || {
            for &sel in &sels {
                assert_eq!(unsafe { send(this, sel) }, sel);
            }
        };
        assert_eq!(misses(all), 0);

        // x86_64 returns the struct in memory, through its own entry point.
        #[cfg(target_arch = "aarch64")]
        let entry = objc_msgSend as unsafe extern "C-unwind" fn();
        #[cfg(target_arch = "x86_64")]
        let entry = objc_msgSend_stret as unsafe extern "C-unwind" fn();
        // SAFETY: as above.
        let send: unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> Wide = unsafe { std::mem::transmute(entry) };
        assert_eq!(misses(|| assert_eq!(unsafe { send(this, sel!(sidestepWide)) }, Wide([1, 2, 3, 4, 5]))), 0);

        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: as above.
            let send: unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> f64 =
                unsafe { std::mem::transmute(objc_msgSend_fpret as unsafe extern "C-unwind" fn()) };
            assert_eq!(misses(|| assert_eq!(unsafe { send(this, sel!(sidestepHalf)) }, 0.5)), 0);
        }
    }
}
