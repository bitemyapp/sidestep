//! What AppKit's event machinery costs a program, in nanoseconds per
//! operation, the median of seven runs. Run in release mode on macOS
//! (AppKit) and on Linux (Sidestep) to compare:
//! `cargo run --release -p eventbench`.
//!
//! - a view frame change nobody observes, while the program observes other
//!   notifications (the frame notification is posted on every change);
//! - the same change with one observer of it;
//! - posting an event and taking it back from the queue in a tracking loop;
//! - sending a key event through a window to its first responder.
//!
//! No window is shown.

use std::cell::Cell;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventTrackingRunLoopMode, NSEventType, NSResponder, NSView, NSViewFrameDidChangeNotification, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

/// Nanoseconds per operation for `n` runs of `f`, the median of seven.
fn median(n: u32, mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..n {
                f();
            }
            start.elapsed().as_secs_f64() * 1e9 / n as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

define_class!(
    /// Takes keys.
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EventBenchKeys"]
    #[ivars = Cell<u64>]
    struct Keys;

    impl Keys {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: &NSEvent) {
            self.ivars().set(self.ivars().get() + 1);
        }
    }

    unsafe impl NSObjectProtocol for Keys {}
);

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let keys: Retained<Keys> =
        unsafe { msg_send![super(Keys::alloc(mtm).set_ivars(Cell::new(0))), initWithFrame: frame] };
    window.setContentView(Some(&keys));
    let view =
        NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(10.0, 10.0), NSSize::new(50.0, 50.0)));
    keys.addSubview(&view);

    // Someone observes something, so posting can't skip everything.
    let center = NSNotificationCenter::defaultCenter();
    let other = NSString::from_str("EventBenchOther");
    let seen = RcBlock::new(|_: NonNull<NSNotification>| {});
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&other), None, None, &seen) };

    let mut x = 0.0;
    let mut move_view = || {
        x = if x == 10.0 { 11.0 } else { 10.0 };
        view.setFrameOrigin(NSPoint::new(x, 10.0));
    };
    let unobserved = median(100_000, &mut move_view);
    let counted = std::rc::Rc::new(Cell::new(0u64));
    let c = counted.clone();
    let heard = RcBlock::new(move |_: NonNull<NSNotification>| c.set(c.get() + 1));
    let frame_name = unsafe { NSViewFrameDidChangeNotification };
    let observer =
        unsafe { center.addObserverForName_object_queue_usingBlock(Some(frame_name), Some(&view), None, &heard) };
    let observed = median(100_000, &mut move_view);
    assert!(counted.get() > 0);

    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    )
    .unwrap();
    let mode = unsafe { NSEventTrackingRunLoopMode };
    let queue = median(20_000, || {
        app.postEvent_atStart(&event, false);
        let taken =
            app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, None, mode, true);
        assert!(taken.is_some());
    });

    window.makeFirstResponder(Some(&keys));
    let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        window.windowNumber(),
        None,
        &NSString::from_str("a"),
        &NSString::from_str("a"),
        false,
        0,
    )
    .unwrap();
    let dispatch = median(100_000, || window.sendEvent(&key));
    assert!(keys.ivars().get() > 0);

    println!("view frame change, unobserved      {unobserved:8.1} ns");
    println!("view frame change, observed        {observed:8.1} ns");
    println!("post and take an event (tracking)  {queue:8.1} ns");
    println!("key event to the first responder   {dispatch:8.1} ns");
    unsafe {
        center.removeObserver(observer.as_ref());
        center.removeObserver(token.as_ref());
    }
}
