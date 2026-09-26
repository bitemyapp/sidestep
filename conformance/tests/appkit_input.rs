//! Input through AppKit, checked on macOS and on Linux alike without showing
//! a window: key and mouse events and what they carry, how a window routes
//! them to its first responder and up the responder chain, first responder
//! changes, key window state, size limits, the backing scale, pasteboards,
//! cursors and tracking areas.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString, NSResponder,
    NSView, NSWindow, NSWindowDelegate, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

/// What a recording view saw, as (view name, selector name, detail).
type Log = RefCell<Vec<(String, &'static str, String)>>;

thread_local!(static LOG: Log = const { RefCell::new(Vec::new()) });

fn log(view: &Recorder, what: &'static str, detail: String) {
    LOG.with(|l| l.borrow_mut().push((view.ivars().name.clone(), what, detail)));
}

fn take_log() -> Vec<(String, &'static str, String)> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

struct RecorderIvars {
    name: String,
    /// Handle events itself, or pass them up the chain.
    handles: bool,
    accepts: bool,
    refuses_resign: std::cell::Cell<bool>,
    /// The key equivalent this view performs, if any.
    equivalent: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceInputRecorder"]
    #[ivars = RecorderIvars]
    struct Recorder;

    impl Recorder {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().accepts
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            log(self, "becomeFirstResponder", String::new());
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            log(self, "resignFirstResponder", String::new());
            !self.ivars().refuses_resign.get()
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            log(self, "keyDown:", event.characters().map(|c| c.to_string()).unwrap_or_default());
            if !self.ivars().handles {
                // SAFETY: NSResponder's keyDown: takes an event.
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            log(self, "keyUp:", event.characters().map(|c| c.to_string()).unwrap_or_default());
            if !self.ivars().handles {
                // SAFETY: NSResponder's keyUp: takes an event.
                unsafe { msg_send![super(self), keyUp: event] }
            }
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            log(self, "flagsChanged:", format!("{:#x}", event.modifierFlags().0));
            if !self.ivars().handles {
                // SAFETY: NSResponder's flagsChanged: takes an event.
                unsafe { msg_send![super(self), flagsChanged: event] }
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            log(self, "mouseDown:", format!("{}", event.clickCount()));
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let key = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
            log(self, "performKeyEquivalent:", key.clone());
            let mine = self.ivars().equivalent.borrow().as_deref() == Some(key.as_str());
            // SAFETY: NSView's performKeyEquivalent: takes an event and
            // returns BOOL.
            mine || unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            log(self, "rightMouseDown:", format!("{}", event.buttonNumber()));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            log(self, "otherMouseDown:", format!("{}", event.buttonNumber()));
        }
    }

    unsafe impl NSObjectProtocol for Recorder {}
);

impl Recorder {
    fn new(mtm: MainThreadMarker, name: &str, frame: NSRect, handles: bool, accepts: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RecorderIvars {
            name: name.into(),
            handles,
            accepts,
            refuses_resign: std::cell::Cell::new(false),
            equivalent: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn window(mtm: MainThreadMarker, style: NSWindowStyleMask) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            style,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn titled() -> NSWindowStyleMask {
    NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
}

fn key_event(
    kind: NSEventType,
    window: &NSWindow,
    flags: NSEventModifierFlags,
    chars: &str,
    unmodified: &str,
    repeat: bool,
    code: u16,
) -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::new(0.0, 0.0),
        flags,
        12.5,
        window.windowNumber(),
        None,
        &NSString::from_str(chars),
        &NSString::from_str(unmodified),
        repeat,
        code,
    )
    .expect("a key event")
}

fn mouse_event(kind: NSEventType, window: &NSWindow, at: NSPoint, clicks: isize) -> Retained<NSEvent> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        at,
        NSEventModifierFlags::Shift,
        3.0,
        window.windowNumber(),
        None,
        0,
        clicks,
        1.0,
    )
    .expect("a mouse event")
}

fn key_events_carry_their_fields(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let flags = NSEventModifierFlags::Shift | NSEventModifierFlags::Control;
    let e = key_event(NSEventType::KeyDown, &w, flags, "\u{1}", "A", true, 38);
    assert_eq!(e.r#type(), NSEventType::KeyDown);
    assert_eq!(e.characters().unwrap().to_string(), "\u{1}");
    assert_eq!(e.charactersIgnoringModifiers().unwrap().to_string(), "A");
    assert!(e.isARepeat());
    assert_eq!(e.keyCode(), 38);
    assert!(e.modifierFlags().contains(flags));
    assert_eq!(e.timestamp(), 12.5);
    assert_eq!(e.windowNumber(), w.windowNumber());

    let up = key_event(NSEventType::KeyUp, &w, NSEventModifierFlags(0), "a", "a", false, 38);
    assert_eq!(up.r#type(), NSEventType::KeyUp);
    assert!(!up.isARepeat());
    assert!(!up.modifierFlags().contains(NSEventModifierFlags::Shift));

    let flags_changed = key_event(NSEventType::FlagsChanged, &w, NSEventModifierFlags::Option, "", "", false, 58);
    assert_eq!(flags_changed.r#type(), NSEventType::FlagsChanged);
    assert!(flags_changed.modifierFlags().contains(NSEventModifierFlags::Option));
    assert_eq!(flags_changed.keyCode(), 58);
}

fn mouse_events_carry_their_fields(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let e = mouse_event(NSEventType::LeftMouseDown, &w, NSPoint::new(10.0, 20.0), 2);
    assert_eq!(e.r#type(), NSEventType::LeftMouseDown);
    assert_eq!(e.clickCount(), 2);
    assert_eq!(e.locationInWindow(), NSPoint::new(10.0, 20.0));
    assert!(e.modifierFlags().contains(NSEventModifierFlags::Shift));
    assert_eq!(e.buttonNumber(), 0);
    // Which button a made-up right or other mouse event names is up to the
    // platform (Apple's say 0), so it isn't checked.
}

fn event_timing_settings(_: MainThreadMarker) {
    assert!(NSEvent::doubleClickInterval() > 0.0);
    assert!(NSEvent::keyRepeatDelay() > 0.0);
    assert!(NSEvent::keyRepeatInterval() > 0.0);
}

/// A window with a content view holding two subviews, `inner` inside
/// `middle`: content > middle > inner.
struct Views {
    window: Retained<NSWindow>,
    content: Retained<Recorder>,
    middle: Retained<Recorder>,
    inner: Retained<Recorder>,
}

fn views(mtm: MainThreadMarker, middle_handles: bool, inner_handles: bool) -> Views {
    let window = window(mtm, titled());
    let content = Recorder::new(mtm, "content", rect(0.0, 0.0, 400.0, 300.0), true, false);
    let middle = Recorder::new(mtm, "middle", rect(50.0, 50.0, 200.0, 200.0), middle_handles, true);
    let inner = Recorder::new(mtm, "inner", rect(10.0, 10.0, 100.0, 100.0), inner_handles, true);
    window.setContentView(Some(&content));
    content.addSubview(&middle);
    middle.addSubview(&inner);
    take_log();
    Views { window, content, middle, inner }
}

fn is(r: Option<Retained<NSResponder>>, view: &NSView) -> bool {
    r.is_some_and(|r| std::ptr::eq(&*r, view as &NSResponder))
}

fn key_events_go_to_the_first_responder(mtm: MainThreadMarker) {
    let v = views(mtm, true, true);
    assert!(v.window.makeFirstResponder(Some(&v.inner)));
    assert!(is(v.window.firstResponder(), &v.inner));
    assert_eq!(take_log(), vec![("inner".into(), "becomeFirstResponder", String::new())]);

    let w = &v.window;
    w.sendEvent(&key_event(NSEventType::KeyDown, w, NSEventModifierFlags(0), "x", "x", false, 7));
    w.sendEvent(&key_event(NSEventType::KeyUp, w, NSEventModifierFlags(0), "x", "x", false, 7));
    w.sendEvent(&key_event(NSEventType::FlagsChanged, w, NSEventModifierFlags::Shift, "", "", false, 56));
    let log = take_log();
    assert_eq!(
        log,
        vec![
            ("inner".into(), "keyDown:", "x".into()),
            ("inner".into(), "keyUp:", "x".into()),
            ("inner".into(), "flagsChanged:", format!("{:#x}", NSEventModifierFlags::Shift.0)),
        ]
    );
    drop(v.content);
}

fn unhandled_key_events_go_up_the_chain(mtm: MainThreadMarker) {
    let v = views(mtm, true, false);
    v.window.makeFirstResponder(Some(&v.inner));
    take_log();
    let w = &v.window;
    w.sendEvent(&key_event(NSEventType::KeyDown, w, NSEventModifierFlags(0), "y", "y", false, 16));
    w.sendEvent(&key_event(NSEventType::FlagsChanged, w, NSEventModifierFlags::Command, "", "", false, 55));
    let log = take_log();
    assert_eq!(
        log,
        vec![
            ("inner".into(), "keyDown:", "y".into()),
            ("middle".into(), "keyDown:", "y".into()),
            ("inner".into(), "flagsChanged:", format!("{:#x}", NSEventModifierFlags::Command.0)),
            ("middle".into(), "flagsChanged:", format!("{:#x}", NSEventModifierFlags::Command.0)),
        ]
    );
}

fn key_equivalents_search_the_view_tree(mtm: MainThreadMarker) {
    let v = views(mtm, true, true);
    let side = Recorder::new(mtm, "side", rect(300.0, 10.0, 50.0, 50.0), true, false);
    v.content.addSubview(&side);
    v.inner.ivars().equivalent.replace(Some("k".into()));
    take_log();
    let w = &v.window;
    let command = NSEventModifierFlags::Command;
    let asked = |key: &str, names: &[&str]| -> Vec<(String, &'static str, String)> {
        names.iter().map(|n| (n.to_string(), "performKeyEquivalent:", key.to_string())).collect()
    };
    // Depth first, subviews in order, until a view performs it.
    let k = key_event(NSEventType::KeyDown, w, command, "k", "k", false, 40);
    assert!(w.performKeyEquivalent(&k));
    assert_eq!(take_log(), asked("k", &["content", "middle", "inner"]));
    let j = key_event(NSEventType::KeyDown, w, command, "j", "j", false, 38);
    assert!(!w.performKeyEquivalent(&j));
    assert_eq!(take_log(), asked("j", &["content", "middle", "inner", "side"]));
}

fn first_responder_changes(mtm: MainThreadMarker) {
    let v = views(mtm, true, true);
    let w = &v.window;
    assert!(w.makeFirstResponder(Some(&v.inner)));
    assert!(w.makeFirstResponder(Some(&v.middle)));
    assert_eq!(
        take_log(),
        vec![
            ("inner".into(), "becomeFirstResponder", String::new()),
            ("inner".into(), "resignFirstResponder", String::new()),
            ("middle".into(), "becomeFirstResponder", String::new()),
        ]
    );
    // Making the first responder first again asks nobody.
    assert!(w.makeFirstResponder(Some(&v.middle)));
    assert_eq!(take_log(), vec![]);

    // A responder that won't resign keeps its place.
    v.middle.ivars().refuses_resign.set(true);
    assert!(!w.makeFirstResponder(Some(&v.inner)));
    assert!(is(w.firstResponder(), &v.middle));
    assert_eq!(take_log(), vec![("middle".into(), "resignFirstResponder", String::new())]);
    v.middle.ivars().refuses_resign.set(false);

    // Nil makes the window itself the first responder.
    assert!(w.makeFirstResponder(None));
    let first = w.firstResponder().expect("the window");
    assert!(std::ptr::eq(&*first, &**w as &NSResponder));
    assert_eq!(take_log(), vec![("middle".into(), "resignFirstResponder", String::new())]);

    // A first responder taken out of the window leaves the window as first
    // responder, and so does one inside a view taken out.
    assert!(w.makeFirstResponder(Some(&v.inner)));
    v.inner.removeFromSuperview();
    assert!(std::ptr::eq(&*w.firstResponder().unwrap(), &**w as &NSResponder));
    v.middle.addSubview(&v.inner);
    assert!(w.makeFirstResponder(Some(&v.inner)));
    v.middle.removeFromSuperview();
    assert!(std::ptr::eq(&*w.firstResponder().unwrap(), &**w as &NSResponder));
    // Without being asked to resign.
    let became = ("inner".to_string(), "becomeFirstResponder", String::new());
    assert_eq!(take_log(), vec![became.clone(), became]);
}

fn first_responders_belong_to_their_window(mtm: MainThreadMarker) {
    let v = views(mtm, true, true);
    let w = &v.window;
    assert!(w.makeFirstResponder(Some(&v.inner)));
    take_log();
    // A view outside the window doesn't become its first responder: the
    // current one resigns and the window takes over, without the view
    // being asked.
    let stray = Recorder::new(mtm, "stray", rect(0.0, 0.0, 10.0, 10.0), true, true);
    assert!(w.makeFirstResponder(Some(&stray)));
    assert!(std::ptr::eq(&*w.firstResponder().unwrap(), &**w as &NSResponder));
    assert_eq!(take_log(), vec![("inner".into(), "resignFirstResponder", String::new())]);
    // Nor does a view in another window.
    let other = views(mtm, true, true);
    assert!(w.makeFirstResponder(Some(&other.inner)));
    assert!(std::ptr::eq(&*w.firstResponder().unwrap(), &**w as &NSResponder));
    assert_eq!(take_log(), vec![]);
}

fn a_new_window_is_its_own_first_responder(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let first = w.firstResponder().expect("the window");
    assert!(std::ptr::eq(&*first, &*w as &NSResponder));
}

fn key_window_state(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    assert!(!w.isKeyWindow());
    assert!(!w.isMainWindow());
    assert!(w.canBecomeKeyWindow());
    let borderless = window(mtm, NSWindowStyleMask::Borderless);
    assert!(!borderless.canBecomeKeyWindow());
    assert!(!borderless.isKeyWindow());
    // Only a title bar makes a window able to become key, not a resize
    // border.
    let resizable = window(mtm, NSWindowStyleMask::Borderless | NSWindowStyleMask::Resizable);
    assert!(!resizable.canBecomeKeyWindow());
    // A window that isn't on screen can't become main.
    assert!(!w.canBecomeMainWindow());
    assert!(!borderless.canBecomeMainWindow());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceWindowDelegate"]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}
    unsafe impl NSWindowDelegate for WindowDelegate {}
);

fn window_delegates_are_weak(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    objc2::rc::autoreleasepool(|_| {
        let delegate: Retained<WindowDelegate> =
            unsafe { msg_send![super(WindowDelegate::alloc(mtm).set_ivars(())), init] };
        w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let held = w.delegate().expect("the delegate");
        assert!(std::ptr::eq(Retained::as_ptr(&held).cast::<u8>(), Retained::as_ptr(&delegate).cast::<u8>()));
    });
    // The window didn't keep its delegate alive.
    assert!(w.delegate().is_none());
}

fn style_masks(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    assert_eq!(w.styleMask(), titled());
    w.setStyleMask(NSWindowStyleMask::Titled | NSWindowStyleMask::Closable);
    assert_eq!(w.styleMask(), NSWindowStyleMask::Titled | NSWindowStyleMask::Closable);
    assert!(!w.isMiniaturized());
}

fn window_settings(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSColor, NSWindowCollectionBehavior, NSWindowTabbingMode, NSWindowTitleVisibility};
    let w = window(mtm, titled());
    // What a new window starts with.
    assert!(w.isOpaque());
    assert!(w.hasShadow());
    assert_eq!(w.alphaValue(), 1.0);
    assert_eq!(w.titleVisibility(), NSWindowTitleVisibility::Visible);
    assert!(!w.titlebarAppearsTransparent());
    assert!(!w.isMovableByWindowBackground());
    assert_eq!(w.level(), 0);
    assert!(!w.ignoresMouseEvents());
    assert_eq!(w.frameAutosaveName().to_string(), "");
    assert!(w.isMovable());
    assert!(!w.isDocumentEdited());
    assert_eq!(w.subtitle().to_string(), "");
    assert!(!w.hidesOnDeactivate());
    assert!(w.canHide());
    assert!(w.initialFirstResponder().is_none());

    // What it's given, it keeps.
    w.setOpaque(false);
    w.setHasShadow(false);
    w.setAlphaValue(0.5);
    w.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    w.setTitlebarAppearsTransparent(true);
    w.setMovableByWindowBackground(true);
    w.setLevel(3);
    w.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    w.setTabbingMode(NSWindowTabbingMode::Disallowed);
    w.setIgnoresMouseEvents(true);
    w.setMovable(false);
    w.setDocumentEdited(true);
    w.setSubtitle(&NSString::from_str("sub"));
    w.setHidesOnDeactivate(true);
    w.setCanHide(false);
    assert!(!w.isOpaque());
    assert!(!w.hasShadow());
    assert_eq!(w.alphaValue(), 0.5);
    assert_eq!(w.titleVisibility(), NSWindowTitleVisibility::Hidden);
    assert!(w.titlebarAppearsTransparent());
    assert!(w.isMovableByWindowBackground());
    assert_eq!(w.level(), 3);
    assert!(w.collectionBehavior().contains(NSWindowCollectionBehavior::FullScreenPrimary));
    assert_eq!(w.tabbingMode(), NSWindowTabbingMode::Disallowed);
    assert!(w.ignoresMouseEvents());
    assert!(!w.isMovable());
    assert!(w.isDocumentEdited());
    assert_eq!(w.subtitle().to_string(), "sub");
    assert!(w.hidesOnDeactivate());
    assert!(!w.canHide());

    let color = NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.4, 0.6, 1.0);
    w.setBackgroundColor(Some(&color));
    assert!(w.backgroundColor().isEqual(Some(&color)));

    objc2::rc::autoreleasepool(|_| {
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
        w.setInitialFirstResponder(Some(&view));
        assert!(w.initialFirstResponder().is_some_and(|v| std::ptr::eq(&*v, &*view)));
    });
    // The window doesn't keep it alive.
    assert!(w.initialFirstResponder().is_none());
}

fn size_limits(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    // Frame sizes are content sizes plus the title bar, whose height is up
    // to the platform.
    let frame_size = |content: NSSize| w.frameRectForContentRect(NSRect::new(NSPoint::ZERO, content)).size;
    let content_size = |frame: NSSize| w.contentRectForFrameRect(NSRect::new(NSPoint::ZERO, frame)).size;
    let unlimited = NSSize::new(f32::MAX as f64, f32::MAX as f64);
    assert_eq!(w.contentMinSize(), NSSize::ZERO);
    assert_eq!(w.minSize(), frame_size(NSSize::ZERO));
    assert_eq!(w.contentMaxSize(), unlimited);
    assert_eq!(w.maxSize(), unlimited);

    // Setting either the content or the frame limit sets the other.
    w.setContentMinSize(NSSize::new(200.0, 100.0));
    assert_eq!(w.contentMinSize(), NSSize::new(200.0, 100.0));
    assert_eq!(w.minSize(), frame_size(NSSize::new(200.0, 100.0)));
    w.setContentMaxSize(NSSize::new(800.0, 600.0));
    assert_eq!(w.contentMaxSize(), NSSize::new(800.0, 600.0));
    assert_eq!(w.maxSize(), frame_size(NSSize::new(800.0, 600.0)));
    w.setMinSize(NSSize::new(300.0, 250.0));
    assert_eq!(w.minSize(), NSSize::new(300.0, 250.0));
    assert_eq!(w.contentMinSize(), content_size(NSSize::new(300.0, 250.0)));
    w.setMaxSize(NSSize::new(900.0, 700.0));
    assert_eq!(w.maxSize(), NSSize::new(900.0, 700.0));
    assert_eq!(w.contentMaxSize(), content_size(NSSize::new(900.0, 700.0)));

    // The frame holds the content rectangle given at creation.
    let frame = w.frame();
    let content = w.contentRectForFrameRect(frame);
    assert_eq!(content, rect(100.0, 100.0, 400.0, 300.0));
    assert!(frame.size.width >= content.size.width && frame.size.height >= content.size.height);
    assert_eq!(w.frameRectForContentRect(content), frame);
    let class_frame = NSWindow::frameRectForContentRect_styleMask(content, titled(), mtm);
    assert_eq!(class_frame, frame);
    assert_eq!(NSWindow::frameRectForContentRect_styleMask(content, NSWindowStyleMask::Borderless, mtm), content);
}

fn child_windows_and_screen_coordinates(mtm: MainThreadMarker) {
    let parent = window(mtm, titled());
    let child = window(mtm, NSWindowStyleMask::Borderless);
    assert!(child.parentWindow().is_none());
    unsafe { parent.addChildWindow_ordered(&child, NSWindowOrderingMode::Above) };
    assert!(child.parentWindow().is_some_and(|p| std::ptr::eq(&*p, &*parent)));
    parent.removeChildWindow(&child);
    assert!(child.parentWindow().is_none());
    unsafe { child.setParentWindow(Some(&parent)) };
    assert!(child.parentWindow().is_some_and(|p| std::ptr::eq(&*p, &*parent)));
    unsafe { child.setParentWindow(None) };
    assert!(child.parentWindow().is_none());

    // Screen coordinates are the window's moved by its frame's origin.
    let o = parent.frame().origin;
    let r = rect(10.0, 20.0, 30.0, 40.0);
    let on_screen = rect(o.x + 10.0, o.y + 20.0, 30.0, 40.0);
    assert_eq!(parent.convertRectToScreen(r), on_screen);
    assert_eq!(parent.convertRectFromScreen(on_screen), r);
    assert_eq!(parent.convertPointToScreen(NSPoint::new(1.0, 2.0)), NSPoint::new(o.x + 1.0, o.y + 2.0));
    assert_eq!(parent.convertPointFromScreen(NSPoint::new(o.x + 1.0, o.y + 2.0)), NSPoint::new(1.0, 2.0));
}

fn backing_scale(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let s = w.backingScaleFactor();
    assert!(s >= 1.0);
    let r = rect(1.0, 2.0, 30.0, 40.0);
    let b = w.convertRectToBacking(r);
    assert_eq!(b, rect(s, 2.0 * s, 30.0 * s, 40.0 * s));
    assert_eq!(w.convertRectFromBacking(b), r);
    assert_eq!(w.convertPointToBacking(NSPoint::new(3.0, 4.0)), NSPoint::new(3.0 * s, 4.0 * s));
    assert_eq!(w.convertPointFromBacking(NSPoint::new(3.0 * s, 4.0 * s)), NSPoint::new(3.0, 4.0));
}

fn pasteboard_names(_: MainThreadMarker) {
    use objc2_app_kit::*;
    let names = unsafe {
        [
            (NSPasteboardTypeString, "public.utf8-plain-text"),
            (NSPasteboardTypePDF, "com.adobe.pdf"),
            (NSPasteboardTypeTIFF, "public.tiff"),
            (NSPasteboardTypePNG, "public.png"),
            (NSPasteboardTypeRTF, "public.rtf"),
            (NSPasteboardTypeRTFD, "com.apple.flat-rtfd"),
            (NSPasteboardTypeHTML, "public.html"),
            (NSPasteboardTypeTabularText, "public.utf8-tab-separated-values-text"),
            (NSPasteboardTypeURL, "public.url"),
            (NSPasteboardTypeFileURL, "public.file-url"),
            (NSPasteboardNameGeneral, "Apple CFPasteboard general"),
            (NSPasteboardNameFind, "Apple CFPasteboard find"),
        ]
    };
    for (name, value) in names {
        assert_eq!(name.to_string(), value);
    }
    let general = NSPasteboard::generalPasteboard();
    assert_eq!(general.name().to_string(), "Apple CFPasteboard general");
    assert!(std::ptr::eq(&*general, &*NSPasteboard::generalPasteboard()));
    let named = NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameGeneral });
    assert!(std::ptr::eq(&*general, &*named));
}

fn cursors(_: MainThreadMarker) {
    use objc2_app_kit::NSCursor;
    let arrow = NSCursor::arrowCursor();
    let beam = NSCursor::IBeamCursor();
    let hand = NSCursor::pointingHandCursor();
    assert!(!arrow.isEqual(Some(&beam)));
    assert!(!beam.isEqual(Some(&hand)));
    assert!(arrow.isEqual(Some(&NSCursor::arrowCursor())));
    let current = || NSCursor::currentCursor();
    assert!(current().isEqual(Some(&arrow)));
    // Cursors stack.
    beam.push();
    assert!(current().isEqual(Some(&beam)));
    hand.push();
    assert!(current().isEqual(Some(&hand)));
    hand.pop();
    assert!(current().isEqual(Some(&beam)));
    NSCursor::pop_class();
    assert!(current().isEqual(Some(&arrow)));
    hand.set();
    assert!(current().isEqual(Some(&hand)));
    arrow.set();
}

fn pasteboards(_: MainThreadMarker) {
    // A pasteboard of our own, so the test leaves the user's clipboard alone.
    let pb = NSPasteboard::pasteboardWithUniqueName();
    let string_type = unsafe { NSPasteboardTypeString };
    assert_eq!(string_type.to_string(), "public.utf8-plain-text");
    let start = pb.changeCount();
    let cleared = pb.clearContents();
    assert!(cleared > start);
    assert_eq!(pb.changeCount(), cleared);
    assert!(pb.stringForType(string_type).is_none());
    assert!(pb.setString_forType(&NSString::from_str("héllo, pasteboard"), string_type));
    assert_eq!(pb.stringForType(string_type).unwrap().to_string(), "héllo, pasteboard");
    assert_eq!(pb.changeCount(), cleared);
    let again = pb.clearContents();
    assert!(again > cleared);
    assert!(pb.stringForType(string_type).is_none());
    let other = NSPasteboard::pasteboardWithUniqueName();
    assert!(!other.name().isEqual(Some(&pb.name())));

    // The old type names are the new ones.
    #[allow(deprecated)]
    let legacy = unsafe {
        [
            (objc2_app_kit::NSStringPboardType, NSPasteboardTypeString),
            (objc2_app_kit::NSHTMLPboardType, objc2_app_kit::NSPasteboardTypeHTML),
            (objc2_app_kit::NSRTFPboardType, objc2_app_kit::NSPasteboardTypeRTF),
            (objc2_app_kit::NSTabularTextPboardType, objc2_app_kit::NSPasteboardTypeTabularText),
            (objc2_app_kit::NSPDFPboardType, objc2_app_kit::NSPasteboardTypePDF),
        ]
    };
    for (old, new) in legacy {
        pb.clearContents();
        assert!(pb.setString_forType(&NSString::from_str("new"), new));
        assert_eq!(pb.stringForType(old).map(|s| s.to_string()).as_deref(), Some("new"), "{old}");
        pb.clearContents();
        assert!(pb.setString_forType(&NSString::from_str("old"), old));
        assert_eq!(pb.stringForType(new).map(|s| s.to_string()).as_deref(), Some("old"), "{old}");
    }
    #[allow(deprecated)]
    let string = unsafe { objc2_app_kit::NSStringPboardType };
    assert_eq!(string.to_string(), "NSStringPboardType");
}

thread_local!(static NOTES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) });

fn note(what: &'static str) {
    NOTES.with(|n| n.borrow_mut().push(what));
}

fn take_notes() -> Vec<&'static str> {
    NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

define_class!(
    /// Records what a window tells its delegate.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceRecordingWindowDelegate"]
    struct RecordingDelegate;

    unsafe impl NSObjectProtocol for RecordingDelegate {}

    unsafe impl NSWindowDelegate for RecordingDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            note("windowShouldClose:");
            true
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _note: &NSNotification) {
            note("windowWillClose:");
        }

        #[unsafe(method(windowWillMiniaturize:))]
        fn window_will_miniaturize(&self, _note: &NSNotification) {
            note("windowWillMiniaturize:");
        }

        #[unsafe(method(windowDidMiniaturize:))]
        fn window_did_miniaturize(&self, _note: &NSNotification) {
            note("windowDidMiniaturize:");
        }
    }
);

fn closing_and_miniaturizing(mtm: MainThreadMarker) {
    let delegate: Retained<RecordingDelegate> =
        unsafe { msg_send![super(RecordingDelegate::alloc(mtm).set_ivars(())), init] };
    let delegate = ProtocolObject::from_ref(&*delegate);
    // performClose: and performMiniaturize: need the window's buttons.
    let plain = window(mtm, NSWindowStyleMask::Titled);
    plain.setDelegate(Some(delegate));
    plain.performClose(None);
    plain.performMiniaturize(None);
    assert!(!plain.isMiniaturized());
    assert_eq!(take_notes(), Vec::<&str>::new());
    // A window that isn't on screen isn't miniaturized.
    let w = window(mtm, titled());
    w.setDelegate(Some(delegate));
    w.miniaturize(None);
    assert!(!w.isMiniaturized());
    assert_eq!(take_notes(), Vec::<&str>::new());
    // Closing it tells the delegate all the same.
    w.close();
    assert_eq!(take_notes(), ["windowWillClose:"]);
    plain.setDelegate(None);
    w.setDelegate(None);
}

fn windows_by_number(mtm: MainThreadMarker) {
    use objc2_app_kit::NSApplication;
    let app = NSApplication::sharedApplication(mtm);
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            titled(),
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let found = |n: isize| app.windowWithWindowNumber(n).is_some_and(|f| std::ptr::eq(&*f, &*w));
    // A window is the application's whether it's on screen or not.
    assert!(found(w.windowNumber()));
    w.orderOut(None);
    assert!(found(w.windowNumber()));
    // Even closed, while the program keeps it.
    w.close();
    assert!(found(w.windowNumber()));
}

fn content_views_outlive_their_windows(mtm: MainThreadMarker) {
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    objc2::rc::autoreleasepool(|_| {
        let w = window(mtm, titled());
        w.setContentView(Some(&view));
        let next = unsafe { view.nextResponder() }.expect("the window");
        assert!(std::ptr::eq(Retained::as_ptr(&next).cast::<AnyObject>(), Retained::as_ptr(&w).cast::<AnyObject>()));
    });
    // The window is gone, and the view doesn't point at it.
    assert!(unsafe { view.nextResponder() }.is_none());
}

type Test = (&'static str, fn(MainThreadMarker));

fn more_window_settings(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSWindowAnimationBehavior, NSWindowOcclusionState, NSWindowSharingType};
    let w = window(mtm, titled());
    assert!(w.preservesContentDuringLiveResize());
    assert!(!w.inLiveResize());
    assert_eq!(w.animationBehavior(), NSWindowAnimationBehavior::Default);
    assert_eq!(w.sharingType(), NSWindowSharingType::ReadOnly);
    assert!(!w.autorecalculatesKeyViewLoop());
    assert_eq!(w.representedFilename().to_string(), "");
    assert_eq!(w.resizeIncrements(), NSSize::new(1.0, 1.0));
    assert_eq!(w.contentResizeIncrements(), NSSize::new(1.0, 1.0));
    assert!(w.isZoomable() && w.isResizable() && w.isMiniaturizable());
    assert!(!w.worksWhenModal());
    assert!(!w.isSheet());
    assert!(w.attachedSheet().is_none() && w.sheetParent().is_none());
    // A window that hasn't been shown isn't visible.
    assert!(!w.occlusionState().contains(NSWindowOcclusionState::Visible));

    assert!(w.hasTitleBar() && w.hasCloseBox());
    assert!(!w.isFloatingPanel() && !w.isModalPanel());
    let fixed = window(mtm, NSWindowStyleMask::Titled);
    assert!(!fixed.isResizable() && !fixed.isMiniaturizable() && !fixed.hasCloseBox());

    w.setPreservesContentDuringLiveResize(false);
    w.setAnimationBehavior(NSWindowAnimationBehavior::None);
    w.setSharingType(NSWindowSharingType::None);
    unsafe { w.setAllowsConcurrentViewDrawing(false) };
    w.setDisplaysWhenScreenProfileChanges(true);
    w.setAutorecalculatesKeyViewLoop(true);
    w.setRepresentedFilename(&NSString::from_str("/tmp/file.txt"));
    w.setMiniwindowTitle(Some(&NSString::from_str("mini")));
    w.setTabbingIdentifier(&NSString::from_str("group"));
    w.setResizeIncrements(NSSize::new(2.0, 3.0));
    assert_eq!(w.resizeIncrements(), NSSize::new(2.0, 3.0));
    w.setContentResizeIncrements(NSSize::new(4.0, 5.0));
    assert_eq!(w.contentResizeIncrements(), NSSize::new(4.0, 5.0));
    w.setContentAspectRatio(NSSize::new(16.0, 9.0));
    assert_eq!(w.contentAspectRatio(), NSSize::new(16.0, 9.0));
    assert!(!w.preservesContentDuringLiveResize());
    assert_eq!(w.animationBehavior(), NSWindowAnimationBehavior::None);
    assert_eq!(w.sharingType(), NSWindowSharingType::None);
    assert!(!w.allowsConcurrentViewDrawing());
    assert!(w.displaysWhenScreenProfileChanges());
    assert!(w.autorecalculatesKeyViewLoop());
    assert_eq!(w.representedFilename().to_string(), "/tmp/file.txt");
    assert_eq!(w.miniwindowTitle().to_string(), "mini");
    assert_eq!(w.tabbingIdentifier().to_string(), "group");
    // Nil goes back to a title of the platform's choosing.
    w.setMiniwindowTitle(None);
    assert_ne!(w.miniwindowTitle().to_string(), "mini");

    let tabbing = NSWindow::allowsAutomaticWindowTabbing(mtm);
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    assert!(!NSWindow::allowsAutomaticWindowTabbing(mtm));
    NSWindow::setAllowsAutomaticWindowTabbing(tabbing, mtm);
}

fn posted_events(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSApplication, NSEventMask, NSEventTrackingRunLoopMode};
    let app = NSApplication::sharedApplication(mtm);
    let made = |data1: isize| {
        NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(1.0, 2.0),
            NSEventModifierFlags::empty(),
            3.0,
            0,
            None,
            4,
            data1,
            5,
        )
        .expect("an application-defined event")
    };
    let a = made(10);
    assert_eq!(a.r#type(), NSEventType::ApplicationDefined);
    assert_eq!((a.subtype().0, a.data1(), a.data2()), (4, 10, 5));
    assert_eq!(a.locationInWindow(), NSPoint::new(1.0, 2.0));

    let mode = unsafe { NSEventTrackingRunLoopMode };
    let next = |dequeue: bool| {
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, None, mode, dequeue)
    };
    // Posted events come back in order; one posted at the start comes first.
    app.postEvent_atStart(&a, false);
    app.postEvent_atStart(&made(20), true);
    assert_eq!(next(false).map(|e| e.data1()), Some(20));
    assert_eq!(next(true).map(|e| e.data1()), Some(20));
    assert_eq!(next(true).map(|e| e.data1()), Some(10));
    assert!(next(true).is_none());
    // Discarded ones don't.
    app.postEvent_atStart(&made(30), false);
    app.discardEventsMatchingMask_beforeEvent(NSEventMask::ApplicationDefined, None);
    assert!(next(true).is_none());
    assert!(app.modalWindow().is_none());
}

fn event_monitors(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSApplication, NSEventMask};
    let app = NSApplication::sharedApplication(mtm);
    let seen = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = seen.clone();
    let handler = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
        counter.set(counter.get() + 1);
        event.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) }
        .expect("a monitor");
    let w = window(mtm, titled());
    let event = key_event(NSEventType::KeyDown, &w, NSEventModifierFlags::empty(), "a", "a", false, 0);
    // Local monitors see what goes through sendEvent:, until removed.
    app.sendEvent(&event);
    assert_eq!(seen.get(), 1);
    unsafe { NSEvent::removeMonitor(&monitor) };
    app.sendEvent(&event);
    assert_eq!(seen.get(), 1);
}

fn tracking_areas(mtm: MainThreadMarker) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSCursor, NSTrackingArea, NSTrackingAreaOptions as O};
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    let options = O::MouseEnteredAndExited | O::MouseMoved | O::ActiveInKeyWindow | O::InVisibleRect;
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            rect(10.0, 20.0, 30.0, 40.0),
            options,
            Some(&view),
            None,
        )
    };
    assert_eq!(area.rect(), rect(10.0, 20.0, 30.0, 40.0));
    assert_eq!(area.options(), options);
    assert!(area.owner().is_some_and(|o| std::ptr::eq(&*o as *const _ as *const NSView, &*view)));
    assert!(area.userInfo().is_none());
    view.addTrackingArea(&area);
    view.removeTrackingArea(&area);
    let tag = unsafe {
        view.addTrackingRect_owner_userData_assumeInside(rect(0.0, 0.0, 5.0, 5.0), &view, std::ptr::null_mut(), false)
    };
    view.removeTrackingRect(tag);

    // Cursor rectangles belong to views; windows turn them on and off.
    view.addCursorRect_cursor(rect(0.0, 0.0, 10.0, 10.0), &NSCursor::IBeamCursor());
    view.removeCursorRect_cursor(rect(0.0, 0.0, 10.0, 10.0), &NSCursor::IBeamCursor());
    view.discardCursorRects();
    let w = window(mtm, titled());
    w.setContentView(Some(&view));
    assert!(w.areCursorRectsEnabled());
    w.disableCursorRects();
    assert!(!w.areCursorRectsEnabled());
    w.enableCursorRects();
    assert!(w.areCursorRectsEnabled());
    w.invalidateCursorRectsForView(&view);

    // Entered and exited events carry what they're made with.
    let data = 0x1234 as *mut std::ffi::c_void;
    let event = unsafe {
        NSEvent::enterExitEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_trackingNumber_userData(
            NSEventType::MouseEntered,
            NSPoint::new(3.0, 4.0),
            NSEventModifierFlags::Shift,
            7.0,
            0,
            None,
            5,
            6,
            data,
        )
    }
    .expect("an entered event");
    assert_eq!(event.r#type(), NSEventType::MouseEntered);
    assert_eq!(event.locationInWindow(), NSPoint::new(3.0, 4.0));
    assert_eq!(event.modifierFlags(), NSEventModifierFlags::Shift);
    assert_eq!(event.timestamp(), 7.0);
    assert_eq!(event.eventNumber(), 5);
    assert_eq!(event.trackingNumber(), 6);
    assert_eq!(event.userData(), data);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("key_events_carry_their_fields", key_events_carry_their_fields),
        ("mouse_events_carry_their_fields", mouse_events_carry_their_fields),
        ("event_timing_settings", event_timing_settings),
        ("key_events_go_to_the_first_responder", key_events_go_to_the_first_responder),
        ("unhandled_key_events_go_up_the_chain", unhandled_key_events_go_up_the_chain),
        ("first_responder_changes", first_responder_changes),
        ("key_equivalents_search_the_view_tree", key_equivalents_search_the_view_tree),
        ("first_responders_belong_to_their_window", first_responders_belong_to_their_window),
        ("a_new_window_is_its_own_first_responder", a_new_window_is_its_own_first_responder),
        ("key_window_state", key_window_state),
        ("window_delegates_are_weak", window_delegates_are_weak),
        ("style_masks", style_masks),
        ("window_settings", window_settings),
        ("more_window_settings", more_window_settings),
        ("size_limits", size_limits),
        ("child_windows_and_screen_coordinates", child_windows_and_screen_coordinates),
        ("backing_scale", backing_scale),
        ("pasteboards", pasteboards),
        ("pasteboard_names", pasteboard_names),
        ("cursors", cursors),
        ("tracking_areas", tracking_areas),
        ("posted_events", posted_events),
        ("event_monitors", event_monitors),
        ("closing_and_miniaturizing", closing_and_miniaturizing),
        ("windows_by_number", windows_by_number),
        ("content_views_outlive_their_windows", content_views_outlive_their_windows),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
