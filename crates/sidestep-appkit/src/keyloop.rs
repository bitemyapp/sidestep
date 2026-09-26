//! The key view loop: the order Tab and Shift-Tab move the keyboard focus
//! through a window's views in.
//!
//! Each view links to its next key view and remembers the view that links
//! to it (its previous key view), both weakly, so a view going away leaves
//! no dangling link. As on macOS (`conformance/tests/appkit_events.rs`
//! pins all of this): linking A to B makes A B's previous key view, even if
//! B had another; unlinking A (nil) also clears its old next view's link
//! back, if that was A; taking a view out of its window changes no links.
//! A view can become the key view when it accepts first responder, is in a
//! window and isn't hidden. The next valid key view is the first view along
//! the links that can, stopping short of coming back to the start.
//!
//! A window's `selectNextKeyView:` moves the first responder to the next
//! valid key view after it, or, when no view is first responder, to the
//! window's initial first responder; at the end of an open chain nothing
//! moves. While it runs, `keyViewSelectionDirection` says which way.
//! `recalculateKeyViewLoop` links the content view and every view in it,
//! each followed by its subviews ordered top to bottom and then left to
//! right, back round to the content view; with `autorecalculatesKeyViewLoop`
//! a window does so before each selection.

use std::cell::{Cell, RefCell};

use objc2::msg_send;
use objc2::rc::{Retained, Weak};
use objc2_app_kit::{NSSelectionDirection, NSView, NSWindow};
use objc2_foundation::NSRect;

use crate::views::{self, NSViewImpl};
use crate::window::NSWindowImpl;

/// A view's links.
#[derive(Default)]
pub(crate) struct KeyLinks {
    next: RefCell<Option<Weak<NSView>>>,
    previous: RefCell<Option<Weak<NSView>>>,
}

fn load(link: &RefCell<Option<Weak<NSView>>>) -> Option<Retained<NSView>> {
    link.borrow().as_ref().and_then(Weak::load)
}

fn store(link: &RefCell<Option<Weak<NSView>>>, view: Option<&NSView>) {
    // Dropped after the borrow ends.
    let old = link.replace(view.map(Weak::new));
    drop(old);
}

/// `nextKeyView`.
pub(crate) fn next(view: &NSViewImpl) -> Option<Retained<NSView>> {
    load(&views::key_links(view).next)
}

/// `previousKeyView`.
pub(crate) fn previous(view: &NSViewImpl) -> Option<Retained<NSView>> {
    load(&views::key_links(view).previous)
}

/// `setNextKeyView:`.
pub(crate) fn set_next(view: &NSViewImpl, next: Option<&NSView>) {
    let links = views::key_links(view);
    let old = load(&links.next);
    store(&links.next, next);
    match next {
        Some(next) => store(&views::key_links(views::imp(next)).previous, Some(views::as_view(view))),
        None => {
            if let Some(old) = old {
                let back = &views::key_links(views::imp(&old)).previous;
                if load(back).is_some_and(|p| std::ptr::eq(views::imp(&p), view)) {
                    store(back, None);
                }
            }
        }
    }
}

/// `canBecomeKeyView`.
pub(crate) fn can_become_key_view(view: &NSViewImpl) -> bool {
    views::window_of(view).is_some()
        && !views::is_hidden_or_has_hidden_ancestor(view)
        && views::as_view(view).acceptsFirstResponder()
}

/// The first view along `step` from `view` that can become the key view,
/// short of coming back to `view`. A loop that doesn't pass through `view`
/// ends when it would visit a view twice.
fn valid(view: &NSViewImpl, step: fn(&NSView) -> Option<Retained<NSView>>) -> Option<Retained<NSView>> {
    let mut seen: Vec<*const NSView> = Vec::new();
    let mut at = step(views::as_view(view));
    while let Some(candidate) = at {
        let ptr = Retained::as_ptr(&candidate);
        if std::ptr::eq(views::imp(&candidate), view) || seen.contains(&ptr) {
            return None;
        }
        if candidate.canBecomeKeyView() {
            return Some(candidate);
        }
        seen.push(ptr);
        at = step(&candidate);
    }
    None
}

/// `nextValidKeyView`.
pub(crate) fn next_valid(view: &NSViewImpl) -> Option<Retained<NSView>> {
    // SAFETY: nextKeyView takes nothing and returns a view or nil.
    valid(view, |v| unsafe { v.nextKeyView() })
}

/// `previousValidKeyView`.
pub(crate) fn previous_valid(view: &NSViewImpl) -> Option<Retained<NSView>> {
    // SAFETY: previousKeyView takes nothing and returns a view or nil.
    valid(view, |v| unsafe { v.previousKeyView() })
}

thread_local! {
    /// The window selecting a key view, and which way, while it does.
    static SELECTING: Cell<(usize, NSSelectionDirection)> =
        const { Cell::new((0, NSSelectionDirection::DirectSelection)) };
}

/// `keyViewSelectionDirection`.
pub(crate) fn direction(window: &NSWindowImpl) -> NSSelectionDirection {
    let (selecting, direction) = SELECTING.with(Cell::get);
    if selecting == window as *const NSWindowImpl as usize { direction } else { NSSelectionDirection::DirectSelection }
}

/// Make `target` the first responder, selecting it in `direction`.
fn select(window: &NSWindowImpl, target: &NSView, direction: NSSelectionDirection) {
    let previous = SELECTING.with(|s| s.replace((window as *const NSWindowImpl as usize, direction)));
    struct Restore((usize, NSSelectionDirection));
    impl Drop for Restore {
        fn drop(&mut self) {
            SELECTING.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(previous);
    window.as_window().makeFirstResponder(Some(target));
}

/// `selectNextKeyView:` (`forward`) and `selectPreviousKeyView:`.
pub(crate) fn select_next(window: &NSWindowImpl, forward: bool) {
    let this = window.as_window();
    if this.autorecalculatesKeyViewLoop() {
        this.recalculateKeyViewLoop();
    }
    let first = this.firstResponder().and_then(|f| f.downcast::<NSView>().ok());
    let first = first.filter(|v| views::window_of(views::imp(v)).is_some_and(|w| std::ptr::eq(w, window)));
    let target = match &first {
        Some(view) => valid_from(view, forward),
        None => this.initialFirstResponder(),
    };
    if let Some(target) = target {
        select(window, &target, direction_of(forward));
    }
}

/// `view`'s next or previous valid key view, by message, as subclasses may
/// override them.
fn valid_from(view: &NSView, forward: bool) -> Option<Retained<NSView>> {
    // SAFETY: both take nothing and return a view or nil.
    unsafe { if forward { view.nextValidKeyView() } else { view.previousValidKeyView() } }
}

fn direction_of(forward: bool) -> NSSelectionDirection {
    if forward { NSSelectionDirection::SelectingNext } else { NSSelectionDirection::SelectingPrevious }
}

/// `selectKeyViewFollowingView:` (`forward`) and
/// `selectKeyViewPrecedingView:`.
pub(crate) fn select_around(window: &NSWindowImpl, view: &NSView, forward: bool) {
    let target = valid_from(view, forward);
    if let Some(target) = target {
        select(window, &target, direction_of(forward));
    }
}

/// `recalculateKeyViewLoop`.
pub(crate) fn recalculate(window: &NSWindowImpl) {
    let Some(content) = window.content() else { return };
    let mut order = Vec::new();
    collect(&content, &mut order);
    for pair in order.windows(2) {
        // SAFETY: setNextKeyView: takes a view or nil.
        unsafe { pair[0].setNextKeyView(Some(&pair[1])) };
    }
    if let Some(last) = order.last() {
        // SAFETY: as above.
        unsafe { last.setNextKeyView(Some(&content)) };
    }
}

/// `view`, then each subview in reading order (top to bottom, then left to
/// right, as they sit in the window) followed by its own subviews.
fn collect(view: &NSView, order: &mut Vec<Retained<NSView>>) {
    order.push(objc2::Message::retain(view));
    let mut subviews: Vec<(NSRect, Retained<NSView>)> = views::subviews(views::imp(view))
        .into_iter()
        .map(|sub| (sub.convertRect_toView(sub.bounds(), None), sub))
        .collect();
    // Window coordinates run up: a higher top comes first.
    subviews.sort_by(|(a, _), (b, _)| {
        let (top_a, top_b) = (a.origin.y + a.size.height, b.origin.y + b.size.height);
        top_b.total_cmp(&top_a).then(a.origin.x.total_cmp(&b.origin.x))
    });
    for (_, sub) in subviews {
        collect(&sub, order);
    }
}

/// Tab, Shift-Tab and Escape reaching the end of a window's responder
/// chain (`-[NSWindow keyDown:]`): after the window's views had the key as
/// a key equivalent, Tab selects the next key view, Shift-Tab the previous
/// one, and Escape sends `cancelOperation:` up the chain from the first
/// responder. Other keys are nobody's.
pub(crate) fn window_key_down(window: &NSWindowImpl, event: &objc2_app_kit::NSEvent) {
    let this: &NSWindow = window.as_window();
    if this.performKeyEquivalent(event) {
        return;
    }
    let characters = event.characters().map(|c| c.to_string()).unwrap_or_default();
    let shift = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
    match characters.as_str() {
        "\t" if !shift => this.selectNextKeyView(None),
        "\t" | "\u{19}" => this.selectPreviousKeyView(None),
        "\u{1b}" => cancel(window),
        _ => {
            // SAFETY: noResponderFor: takes a selector.
            let _: () = unsafe { msg_send![this, noResponderFor: objc2::sel!(keyDown:)] };
        }
    }
}

/// Send `cancelOperation:` to the first responder that has it, up the
/// chain.
pub(crate) fn cancel(window: &NSWindowImpl) {
    let this = window.as_window();
    let Some(first) = this.firstResponder() else { return };
    // SAFETY: tryToPerform:with: takes a selector and an object.
    let _ = unsafe { first.tryToPerform_with(objc2::sel!(cancelOperation:), Some(this)) };
}
