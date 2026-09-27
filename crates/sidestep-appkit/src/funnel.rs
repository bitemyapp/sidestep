//! Funnel points: AppKit methods that other AppKit methods reach by
//! message, so that a subclass's override (or a program's swizzle) sees
//! every change it is documented to see: a text view's selection through
//! `setSelectedRanges:affinity:stillSelecting:`, a window's ordering
//! through `orderWindow:relativeTo:`, a view's invalidation through
//! `setNeedsDisplayInRect:`, and the rest (docs/architecture.md, "Funnel
//! points").
//!
//! Sidestep's own methods mostly call each other in Rust. At a funnel the
//! caller asks [`Funnel::overridden`] whether the receiver's class still
//! answers the selector with Sidestep's own implementation; if it does, the
//! caller goes on in Rust, and otherwise it sends the message. The answer
//! is one method-cache probe, the same lookup a message send starts with,
//! so a program that overrides nothing pays next to nothing, much as the
//! runtime's retain fast path skips `-retain` for classes that don't
//! override it.
//!
//! Each funnel keeps the implementation its class had when it loaded,
//! captured by the class's loader before any program code could see the
//! class, so a swizzle made at any time later counts as an override.

use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};

unsafe extern "C-unwind" {
    /// The runtime's: the implementation a message would call.
    fn class_getMethodImplementation(cls: *const AnyClass, sel: Sel) -> Option<Imp>;
}

/// One funnel: a selector's own implementation on the class that
/// defines it.
pub(crate) struct Funnel {
    own: AtomicUsize,
}

impl Funnel {
    pub(crate) const fn new() -> Self {
        Funnel { own: AtomicUsize::new(0) }
    }

    /// Remember `class`'s implementation of `sel` as the own one. Called by
    /// the class's loader, once it is defined.
    pub(crate) fn capture(&self, class: &AnyClass, sel: Sel) {
        let imp = implementation(class, sel);
        self.own.store(imp, Ordering::Relaxed);
    }

    /// Whether a message of `sel` to `receiver` would run something other
    /// than Sidestep's own implementation: a subclass's override, or a
    /// replacement a program put in.
    #[inline]
    pub(crate) fn overridden(&self, receiver: &AnyObject, sel: Sel) -> bool {
        let own = self.own.load(Ordering::Relaxed);
        // Not captured (the class hasn't loaded, which a receiver of it
        // can't be): send.
        own == 0 || implementation(receiver.class(), sel) != own
    }
}

#[inline]
fn implementation(class: &AnyClass, sel: Sel) -> usize {
    // SAFETY: a class and a selector; the runtime answers with the
    // implementation a message would call (the forwarding one if none).
    unsafe { class_getMethodImplementation(class, sel) }.map_or(0, |imp| imp as usize)
}
