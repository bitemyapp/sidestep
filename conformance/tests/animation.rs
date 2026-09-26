//! NSAnimationContext and `animator`, checked on macOS and on Linux alike:
//! the context's settings and how groups save and restore them, when
//! completion handlers run, and changes made through `animator` in a group
//! that lasts no time. (How AppKit animates over time isn't asserted:
//! Sidestep applies changes at once.)
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use common::rect;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{MainThreadMarker, MainThreadOnly, msg_send, sel};
use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSView};

use sidestep as _;

/// Run the thread's run loop for `seconds`, where Foundation's run loop
/// can (else say so and return false). Classes are looked up by name, as
/// Sidestep's Foundation doesn't have `NSDate` yet.
fn run_loop_for(seconds: f64) -> bool {
    let classes = AnyClass::get(c"NSRunLoop").zip(AnyClass::get(c"NSDate"));
    let Some((run_loop_class, date_class)) = classes.filter(|(r, _)| r.instance_method(sel!(runUntilDate:)).is_some())
    else {
        println!("    (skipping the parts that run the run loop, which Foundation can't yet)");
        return false;
    };
    // SAFETY: +currentRunLoop takes nothing and returns the run loop;
    // +dateWithTimeIntervalSinceNow: takes seconds and returns a date.
    unsafe {
        let run_loop: Retained<AnyObject> = msg_send![run_loop_class, currentRunLoop];
        let until: Retained<AnyObject> = msg_send![date_class, dateWithTimeIntervalSinceNow: seconds];
        let _: () = msg_send![&*run_loop, runUntilDate: &*until];
    }
    true
}

fn contexts(_: MainThreadMarker) {
    let context = NSAnimationContext::currentContext();
    assert_eq!(context.duration(), 0.25);
    assert!(!context.allowsImplicitAnimation());
    assert!(context.completionHandler().is_null());
    assert!(std::ptr::eq(&*context, &*NSAnimationContext::currentContext()), "one per thread");

    // A group runs at once, with the current context, starting from the
    // enclosing settings; what it sets ends with it.
    let seen: Rc<Cell<Option<(bool, f64, f64)>>> = Rc::default();
    let s = seen.clone();
    let group = RcBlock::new(move |c: NonNull<NSAnimationContext>| {
        // SAFETY: AppKit passes the context.
        let c = unsafe { c.as_ref() };
        let started = c.duration();
        c.setDuration(0.5);
        let current = NSAnimationContext::currentContext();
        s.set(Some((std::ptr::eq(c, &*current), started, current.duration())));
    });
    NSAnimationContext::runAnimationGroup(&group);
    assert_eq!(seen.get(), Some((true, 0.25, 0.5)));
    assert_eq!(NSAnimationContext::currentContext().duration(), 0.25);

    // Nested groups inherit and restore.
    let inner_started = Rc::new(Cell::new(0.0));
    let after_inner = Rc::new(Cell::new(0.0));
    let (i, a) = (inner_started.clone(), after_inner.clone());
    let outer = RcBlock::new(move |c: NonNull<NSAnimationContext>| {
        // SAFETY: AppKit passes the context.
        unsafe { c.as_ref() }.setDuration(3.0);
        let i = i.clone();
        let inner = RcBlock::new(move |c: NonNull<NSAnimationContext>| {
            // SAFETY: AppKit passes the context.
            let c = unsafe { c.as_ref() };
            i.set(c.duration());
            c.setDuration(7.0);
        });
        NSAnimationContext::runAnimationGroup(&inner);
        a.set(NSAnimationContext::currentContext().duration());
    });
    NSAnimationContext::runAnimationGroup(&outer);
    assert_eq!((inner_started.get(), after_inner.get()), (3.0, 3.0));

    // beginGrouping and endGrouping do the same.
    NSAnimationContext::beginGrouping();
    let inside = NSAnimationContext::currentContext();
    assert!(std::ptr::eq(&*inside, &*context), "the same context");
    inside.setDuration(2.0);
    inside.setAllowsImplicitAnimation(true);
    NSAnimationContext::endGrouping();
    assert_eq!(context.duration(), 0.25);
    assert!(!context.allowsImplicitAnimation());

    // A duration is kept as given, even one below zero.
    NSAnimationContext::beginGrouping();
    context.setDuration(-1.0);
    assert_eq!(context.duration(), -1.0);
    NSAnimationContext::endGrouping();
}

fn completions(mtm: MainThreadMarker) {
    // Changes in a group lasting no time apply at once; the completion
    // handler runs later, from the run loop.
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    let done = Rc::new(Cell::new(0));
    let d = done.clone();
    let target = view.clone();
    let changes = RcBlock::new(move |c: NonNull<NSAnimationContext>| {
        // SAFETY: AppKit passes the context.
        unsafe { c.as_ref() }.setDuration(0.0);
        target.animator().setAlphaValue(0.25);
        target.animator().setFrame(rect(1.0, 2.0, 3.0, 4.0));
        target.animator().setHidden(true);
    });
    let completion = RcBlock::new(move || d.set(d.get() + 1));
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
    assert_eq!((view.alphaValue(), view.frame(), view.isHidden()), (0.25, rect(1.0, 2.0, 3.0, 4.0), true));
    assert_eq!(done.get(), 0, "not called within the group");

    // A handler set on the context runs when its group ends, and leaves
    // the context.
    let later = Rc::new(Cell::new(0));
    let l = later.clone();
    NSAnimationContext::beginGrouping();
    let handler = RcBlock::new(move || l.set(l.get() + 1));
    let context = NSAnimationContext::currentContext();
    context.setDuration(0.0);
    context.setCompletionHandler(Some(&handler));
    assert!(!context.completionHandler().is_null());
    NSAnimationContext::endGrouping();
    assert!(context.completionHandler().is_null());
    assert_eq!(later.get(), 0);

    if run_loop_for(0.05) {
        assert_eq!((done.get(), later.get()), (1, 1), "called once each");
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[("contexts", contexts), ("completions", completions)];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
