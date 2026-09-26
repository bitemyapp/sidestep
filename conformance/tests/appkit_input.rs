//! Input through AppKit, checked on macOS and on Linux alike without showing
//! a window: key and mouse events and what they carry, how a window routes
//! them to its first responder and up the responder chain, first responder
//! changes, key window state, size limits, the backing scale, and
//! pasteboards.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString, NSResponder,
    NSView, NSWindow, NSWindowDelegate, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

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

    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    w.setInitialFirstResponder(Some(&view));
    assert!(w.initialFirstResponder().is_some_and(|v| std::ptr::eq(&*v, &*view)));
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
}

type Test = (&'static str, fn(MainThreadMarker));

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
        ("a_new_window_is_its_own_first_responder", a_new_window_is_its_own_first_responder),
        ("key_window_state", key_window_state),
        ("window_delegates_are_weak", window_delegates_are_weak),
        ("style_masks", style_masks),
        ("window_settings", window_settings),
        ("size_limits", size_limits),
        ("child_windows_and_screen_coordinates", child_windows_and_screen_coordinates),
        ("backing_scale", backing_scale),
        ("pasteboards", pasteboards),
        ("pasteboard_names", pasteboard_names),
        ("cursors", cursors),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
