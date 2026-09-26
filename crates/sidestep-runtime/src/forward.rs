//! Message forwarding: through `-forwardingTargetForSelector:`, and then
//! through `-methodSignatureForSelector:` and `-forwardInvocation:`.
//!
//! With `objc_msg_lookup` the caller calls the implementation itself, with
//! the receiver it already has, so forwarding a message to another object
//! means changing the receiver on the way. When a class overrides
//! `-forwardingTargetForSelector:` (or `+forwardingTargetForSelector:` for
//! class messages), or Foundation has installed its handler for
//! `-forwardInvocation:` (see `call::set_forward_handler`), a selector the
//! class doesn't implement resolves to [`trampoline`]: a few instructions
//! of assembly (the frame in `trampoline`, shared with `objc_msgSend`)
//! that save the argument registers, ask [`resolve`] what to do, restore
//! the registers and jump.
//!
//! [`resolve`] first asks the receiver's `-forwardingTargetForSelector:`,
//! if its class has one. A target other than nil and the receiver itself
//! takes the receiver's place, and the trampoline jumps to the target's
//! implementation: the arguments, stack arguments and return value pass
//! through untouched, so any signature forwards. Otherwise the handler
//! gets the saved registers and the sender's stack arguments as a
//! [`Frame`], builds an `NSInvocation` from them, sends
//! `-forwardInvocation:` and writes the invocation's return value into the
//! saved registers; the trampoline then restores them and jumps to
//! [`forward_return`], a lone `ret`, which hands them to the sender. With
//! no handler, the message is unrecognized.
//!
//! The trampoline is cached like any implementation, but nothing it
//! decides is: `-forwardingTargetForSelector:` is asked on every message,
//! since its answer may change, and so is the signature. Like Apple's
//! `_objc_msgForward`, it is what `class_getMethodImplementation` gives
//! for a selector the class doesn't implement, and a class doesn't respond
//! to the selectors it forwards.
//!
//! The trampolines carry unwind information, so a panic in
//! `-forwardingTargetForSelector:`, `-forwardInvocation:` or
//! `-doesNotRecognizeSelector:` unwinds through them into the sender.

use std::mem::transmute;

use crate::Imp;
use crate::call::{Frame, Registers, forward_handler};
use crate::class::{Class, lookup_imp};
use crate::message::objc_msg_lookup;
use crate::object::{Object, isa};
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
/// Foundation forwards through `-forwardInvocation:`, or the class
/// overrides the root class's `forwardingTargetForSelector:`, which
/// answers nil.
pub(crate) fn forwards(cls: &'static Class) -> bool {
    forward_handler().is_some() || own_forwarding_target(cls).is_some()
}

/// The class's `-forwardingTargetForSelector:`, unless it is the root
/// class's, which forwards nothing.
fn own_forwarding_target(cls: &'static Class) -> Option<Imp> {
    lookup_imp(cls, known().forwarding_target).filter(|&imp| !crate::nsobject::is_default_forwarding_target(imp))
}

/// Where the sender's stack arguments start, from the saved registers:
/// past the trampoline's frame (`trampoline::saving_entry!`), at the stack
/// pointer the sender called with.
#[cfg(target_arch = "aarch64")]
const STACK_ARGUMENTS: usize = 208;
#[cfg(target_arch = "x86_64")]
const STACK_ARGUMENTS: usize = 224;

/// Decides what a forwarded message does. Returns the implementation to
/// jump to: the target's, with the target written into the saved receiver
/// register, or [`forward_return`] once the handler has written the return
/// value into the saved registers.
///
/// # Safety
/// Called only by the trampolines, with the registers of a message send.
unsafe extern "C-unwind" fn resolve(regs: &mut Registers) -> Imp {
    let saved = regs.integer_mut();
    // On x86_64, a method returning a large struct takes the address to
    // write it to first, moving the receiver and selector along by one.
    // The second argument is either the selector or, in that case, the
    // receiver, which is never a selector's address.
    let at = if cfg!(target_arch = "x86_64") && !selector::is_selector(saved[1]) { 1 } else { 0 };
    let (receiver, sel) = (saved[at] as Id, saved[at + 1] as Sel);
    // SAFETY: a message's receiver is live, and its selector valid.
    unsafe {
        if let Some(ask) = own_forwarding_target(isa(receiver)) {
            let ask: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = transmute(ask);
            let target = ask(receiver, known().forwarding_target, sel);
            if !target.is_null() && target != receiver {
                regs.integer_mut()[at] = target as usize;
                return objc_msg_lookup(target, sel).expect("lookup never fails");
            }
        }
        if let Some(handler) = forward_handler() {
            let stack = (regs as *mut Registers).cast::<u8>().add(STACK_ARGUMENTS);
            let mut frame = Frame::new(regs, stack);
            let objc2_sel = transmute::<Sel, objc2::runtime::Sel>(sel);
            handler(&*receiver.cast::<objc2::runtime::AnyObject>(), objc2_sel, &mut frame);
            return forward_return();
        }
        crate::message::does_not_recognize(receiver, sel)
    }
}

/// What the trampoline jumps to after a message was forwarded through
/// `-forwardInvocation:`: a return to the sender, with the registers the
/// handler set.
fn forward_return() -> Imp {
    unsafe extern "C" {
        fn sidestep_forward_return();
    }
    // SAFETY: only ever jumped to by the trampoline.
    unsafe { transmute::<unsafe extern "C" fn(), Imp>(sidestep_forward_return) }
}

#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl sidestep_forward_return",
    ".hidden sidestep_forward_return",
    ".type sidestep_forward_return, %function",
    "sidestep_forward_return:",
    ".cfi_startproc",
    "hint #34",
    "ret",
    ".cfi_endproc",
    ".size sidestep_forward_return, . - sidestep_forward_return",
);

#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl sidestep_forward_return",
    ".hidden sidestep_forward_return",
    ".type sidestep_forward_return, @function",
    "sidestep_forward_return:",
    ".cfi_startproc",
    "ret",
    ".cfi_endproc",
    ".size sidestep_forward_return, . - sidestep_forward_return",
);

// The trampoline: `resolve` gets the saved registers, laid out as
// `call::Registers`, in the frame `trampoline::saving_entry!` lays out.
#[cfg(target_arch = "aarch64")]
crate::trampoline::saving_entry!(
    name: "sidestep_forward_trampoline",
    directives: [".hidden sidestep_forward_trampoline"],
    before: [],
    setup: ["add x0, sp, #16"],
    call: resolve,
    after: [],
);

#[cfg(target_arch = "x86_64")]
crate::trampoline::saving_entry!(
    name: "sidestep_forward_trampoline",
    directives: [".hidden sidestep_forward_trampoline"],
    before: [],
    setup: ["mov rdi, rsp"],
    call: resolve,
    after: [],
);
