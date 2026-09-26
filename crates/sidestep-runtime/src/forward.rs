//! Message forwarding through `-forwardingTargetForSelector:`.
//!
//! With `objc_msg_lookup` the caller calls the implementation itself, with
//! the receiver it already has, so forwarding a message to another object
//! means changing the receiver on the way. When a class overrides
//! `-forwardingTargetForSelector:` (or `+forwardingTargetForSelector:` for
//! class messages), a selector it doesn't implement resolves to
//! [`trampoline`]: a few instructions of assembly (the frame in
//! `trampoline`, shared with `objc_msgSend`) that save the argument
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
use crate::object::Object;
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

/// Where the trampoline saved the integer argument registers, in order:
/// x0 to x8 on aarch64 (x8 holds the address for a returned struct), and
/// rdi, rsi, rdx, rcx, r8, r9, rax on x86_64 (followed by two words of
/// the frame, never read).
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
        crate::message::does_not_recognize(receiver, sel)
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

// The trampoline: `resolve` gets the saved integer registers, in the
// frame `trampoline::saving_entry!` lays out.
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
    setup: ["lea rdi, [rsp + 128]"],
    call: resolve,
    after: [],
);
