//! Drawing TextKit 2's fragments through a `CGContext`.
//!
//! `-[NSTextLayoutFragment drawAtPoint:inContext:]` and its line
//! fragments' take a `CGContext`. As on macOS, a text view hands each
//! fragment the point zero and a context whose origin is the fragment's
//! frame origin ([`with_cg_context_at`]): the current graphics context's
//! `CGContext`, which is the same graphics state (`coregraphics::context`).
//! [`with_context_state`] runs a fragment's default drawing, which records
//! through `graphics::with_recorder` into the current context's state:
//! directly when `cg` is the current context's, and otherwise (a program
//! drawing a fragment into a bitmap context of its own) with `cg` made the
//! current context for the drawing.

use kurbo::Affine;
use objc2::rc::Retained;
use objc2_app_kit::NSGraphicsContext;
use objc2_core_graphics::CGContext;
use objc2_foundation::NSPoint;

/// Run `f` with the current graphics context's `CGContext` (see the
/// module), its origin moved to `at` for `f`; nothing when there is no
/// current context.
pub(crate) fn with_cg_context_at<R>(at: NSPoint, f: impl FnOnce(&CGContext) -> R) -> Option<R> {
    /// Puts the state back, even when the drawing unwinds.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::context::with_state(|st| st.restore());
        }
    }
    let ctx: Retained<NSGraphicsContext> = NSGraphicsContext::currentContext()?;
    let cg = ctx.CGContext();
    crate::context::with_state(|st| {
        st.save();
        st.set_ctm(st.gs.ctm * Affine::translate((at.x, at.y)));
    })?;
    let _restore = Restore;
    Some(f(&cg))
}

/// Run the default drawing `f` for `cg` (see the module). `f` records
/// through `graphics::with_recorder`.
pub(crate) fn with_context_state(cg: &CGContext, f: impl FnOnce()) {
    let current = NSGraphicsContext::currentContext();
    if current.is_some_and(|c| std::ptr::eq(&*c.CGContext(), cg)) {
        f();
    } else {
        crate::coregraphics::context::drawing_into(cg, f);
    }
}
