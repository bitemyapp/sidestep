//! Input, window state and the pasteboard, logged: a window that prints
//! every event it gets, one line each, and shows the line typed so far.
//! Written only against objc2-app-kit: on macOS it runs on AppKit, on
//! Linux on Sidestep.
//!
//! Typing adds to the line and Backspace deletes, through the view's input
//! context, so input methods work too: text being composed shows in
//! brackets. With Command (the Super key
//! on Linux): C copies the line, V pastes, M miniaturizes, Z zooms, F
//! toggles full screen, W closes the window, N opens another window, and T
//! shows or hides a borderless child window at the last click, I switches
//! between the I-beam and arrow cursors, H hides the pointer until it
//! moves, P lets clicks through the window for five seconds, L locks the
//! window in place (or unlocks it), E hides or shows the title, and G makes
//! it longer.
//!
//! On the right, one box lights up while the pointer is over it and shows
//! the pointing hand (a tracking area), and another shows the I-beam (a
//! cursor rectangle).
//!
//! Shift-dragging anywhere in the window moves it. Dragging from the hover
//! box is followed by a nested event loop. Command-O runs a modal window,
//! which Return or Escape ends.
//!
//! INPUT_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::{Cell, OnceCell, RefCell};
use std::io::Write;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezierPath, NSColor,
    NSCursor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSPasteboard, NSPasteboardTypeString, NSResponder, NSStringDrawing,
    NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowDelegate,
    NSWindowOcclusionState, NSWindowOrderingMode, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    NSString, NSTimer,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn log(line: String) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn flags(e: &NSEvent) -> String {
    let f = e.modifierFlags();
    let names = [
        (NSEventModifierFlags::Shift, "shift"),
        (NSEventModifierFlags::Control, "control"),
        (NSEventModifierFlags::Option, "option"),
        (NSEventModifierFlags::Command, "command"),
        (NSEventModifierFlags::CapsLock, "capslock"),
        (NSEventModifierFlags::NumericPad, "numpad"),
        (NSEventModifierFlags::Function, "function"),
    ];
    let set: Vec<&str> = names.iter().filter(|(m, _)| f.contains(*m)).map(|(_, n)| *n).collect();
    if set.is_empty() { "-".into() } else { set.join("+") }
}

fn escaped(s: Option<Retained<NSString>>) -> String {
    s.map(|s| s.to_string().escape_default().to_string()).unwrap_or_else(|| "nil".into())
}

#[derive(Default)]
struct PadIvars {
    text: RefCell<String>,
    /// Text an input method is composing, shown after the typed text.
    marked: RefCell<String>,
    last: RefCell<String>,
    click: RefCell<Option<NSPoint>>,
    tip: RefCell<Option<Retained<NSWindow>>>,
    others: RefCell<Vec<Retained<NSWindow>>>,
}

define_class!(
    /// Shows what was typed and the last event.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "InputPad"]
    #[ivars = PadIvars]
    struct Pad;

    impl Pad {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.98, 0.98, 0.97, 1.0).setFill();
            NSBezierPath::fillRect(dirty);
            let attrs = attributes(18.0);
            let marked = self.ivars().marked.borrow();
            let marked = if marked.is_empty() { String::new() } else { format!("[{marked}]") };
            let typed = NSString::from_str(&format!("Typed: {}{marked}|", self.ivars().text.borrow()));
            unsafe { typed.drawAtPoint_withAttributes(NSPoint::new(16.0, 16.0), Some(&attrs)) };
            let last = NSString::from_str(&self.ivars().last.borrow());
            unsafe { last.drawAtPoint_withAttributes(NSPoint::new(16.0, 48.0), Some(&attributes(13.0))) };
            if let Some(p) = *self.ivars().click.borrow() {
                NSColor::colorWithSRGBRed_green_blue_alpha(0.85, 0.35, 0.2, 1.0).setFill();
                NSBezierPath::fillRect(rect(p.x - 6.0, p.y - 6.0, 12.0, 12.0));
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            self.note(format!(
                "keyDown chars=\"{}\" ignoring=\"{}\" flags={} code={} repeat={}",
                escaped(e.characters()),
                escaped(e.charactersIgnoringModifiers()),
                flags(e),
                e.keyCode(),
                e.isARepeat()
            ));
            if e.modifierFlags().contains(NSEventModifierFlags::Command) {
                let key = e.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
                self.command(&key);
            } else if let Some(context) = self.inputContext() {
                // Typed text and editing keys come back through
                // NSTextInputClient, as input methods' text does.
                context.handleEvent(e);
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, e: &NSEvent) {
            self.note(format!("keyUp chars=\"{}\" flags={} code={}", escaped(e.characters()), flags(e), e.keyCode()));
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, e: &NSEvent) {
            self.note(format!("flagsChanged flags={} code={}", flags(e), e.keyCode()));
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            if e.modifierFlags().contains(NSEventModifierFlags::Shift)
                && let Some(window) = self.window()
            {
                log("moving the window".into());
                window.performWindowDragWithEvent(e);
                return;
            }
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            self.ivars().click.replace(Some(p));
            self.note(format!("mouseDown at={:.0},{:.0} clicks={} flags={}", p.x, p.y, e.clickCount(), flags(e)));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            self.note(format!("mouseUp at={:.0},{:.0} clicks={}", p.x, p.y, e.clickCount()));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            self.note(format!("mouseDragged at={:.0},{:.0}", p.x, p.y));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, e: &NSEvent) {
            self.note(format!("rightMouseDown button={} clicks={}", e.buttonNumber(), e.clickCount()));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, e: &NSEvent) {
            self.note(format!("otherMouseDown button={} clicks={}", e.buttonNumber(), e.clickCount()));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            self.note(format!(
                "scrollWheel dx={:.2} dy={:.2} scrolling={:.2},{:.2} precise={} phase={} momentum={} inverted={}",
                e.deltaX(),
                e.deltaY(),
                e.scrollingDeltaX(),
                e.scrollingDeltaY(),
                e.hasPreciseScrollingDeltas(),
                e.phase().0,
                e.momentumPhase().0,
                e.isDirectionInvertedFromDevice()
            ));
        }
    }

    unsafe impl NSTextInputClient for Pad {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, text: &AnyObject, _range: NSRange) {
            let text = plain(text);
            self.ivars().marked.borrow_mut().clear();
            self.ivars().text.borrow_mut().push_str(&text);
            self.note(format!("insertText \"{}\"", text.escape_default()));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            if selector == objc2::sel!(deleteBackward:) {
                self.ivars().text.borrow_mut().pop();
            }
            log(format!("command {}", selector.name().to_str().unwrap_or("?")));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, text: &AnyObject, selected: NSRange, _replaced: NSRange) {
            let text = plain(text);
            self.note(format!("markedText \"{}\" selected={},{}", text.escape_default(), selected.location, selected.length));
            self.ivars().marked.replace(text);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let marked = self.ivars().marked.take();
            self.ivars().text.borrow_mut().push_str(&marked);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(utf16(&self.ivars().text.borrow()) + utf16(&self.ivars().marked.borrow()), 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let marked = utf16(&self.ivars().marked.borrow());
            if marked == 0 {
                NSRange::new(isize::MAX as usize, 0)
            } else {
                NSRange::new(utf16(&self.ivars().text.borrow()), marked)
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            None
        }

        // An empty array, found at run time: Sidestep has no NSArray yet.
        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Option<Retained<AnyObject>> {
            match objc2::runtime::AnyClass::get(c"NSArray") {
                Some(class) => unsafe { msg_send![class, array] },
                None => None,
            }
        }

        /// The caret, after the typed and composed text, on screen.
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            let marked = self.ivars().marked.borrow();
            let marked = if marked.is_empty() { String::new() } else { format!("[{marked}]") };
            let before = NSString::from_str(&format!("Typed: {}{marked}", self.ivars().text.borrow()));
            let width = unsafe { before.sizeWithAttributes(Some(&attributes(18.0))) }.width;
            let caret = self.convertRect_toView(rect(16.0 + width, 16.0, 2.0, 22.0), None);
            self.window().map_or(NSRect::ZERO, |w| w.convertRectToScreen(caret))
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> usize {
            0
        }
    }
);

#[derive(Default)]
struct HoverIvars {
    inside: Cell<bool>,
    area: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    /// Lights up while the pointer is over it, and shows the pointing hand.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "InputHover"]
    #[ivars = HoverIvars]
    struct Hover;

    impl Hover {
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().area.take() {
                self.removeTrackingArea(&old);
            }
            let options = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::CursorUpdate
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::ZERO,
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            self.ivars().area.replace(Some(area));
            unsafe { msg_send![super(self), updateTrackingAreas] }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _e: &NSEvent) {
            log("mouseEntered hover".into());
            self.ivars().inside.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            log("mouseExited hover".into());
            self.ivars().inside.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, _e: &NSEvent) {
            log("cursorUpdate hover".into());
            NSCursor::pointingHandCursor().set();
        }

        /// Follows a drag in a loop of its own, as controls track the mouse.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            let Some(window) = self.window() else { return };
            log("tracking a drag in a nested loop".into());
            let mask = NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp;
            while let Some(event) = window.nextEventMatchingMask(mask) {
                let p = self.convertPoint_fromView(event.locationInWindow(), None);
                if event.r#type() == NSEventType::LeftMouseUp {
                    log(format!("tracking done at={:.0},{:.0}", p.x, p.y));
                    break;
                }
                log(format!("tracking drag at={:.0},{:.0}", p.x, p.y));
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let (r, g, b) = if self.ivars().inside.get() { (0.93, 0.75, 0.3) } else { (0.9, 0.9, 0.88) };
            NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
            NSBezierPath::fillRect(self.bounds());
            let label = NSString::from_str("Hover here");
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(12.0, 12.0), Some(&attributes(14.0))) };
        }
    }
);

define_class!(
    /// The content of a modal window: Return ends the session with 1,
    /// Escape aborts it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "InputModal"]
    struct ModalPad;

    impl ModalPad {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            let app = NSApplication::sharedApplication(self.mtm());
            match e.characters().map(|c| c.to_string()).as_deref() {
                Some("\r") => app.stopModalWithCode(1),
                Some("\u{1b}") => app.abortModal(),
                _ => log("modal: key ignored".into()),
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            log("modal: click".into());
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.97, 0.95, 0.9, 1.0).setFill();
            NSBezierPath::fillRect(self.bounds());
            let label = NSString::from_str("Return: done. Escape: cancel.");
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(16.0, 40.0), Some(&attributes(14.0))) };
        }
    }
);

define_class!(
    /// Shows the I-beam, through a cursor rectangle.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "InputBeam"]
    struct Beam;

    impl Beam {
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::IBeamCursor());
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0).setFill();
            NSBezierPath::fillRect(self.bounds());
            let label = NSString::from_str("I-beam here");
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(12.0, 12.0), Some(&attributes(14.0))) };
        }
    }
);

/// The text of an NSString or NSAttributedString.
fn plain(text: &AnyObject) -> String {
    let responds: bool = unsafe { msg_send![text, respondsToSelector: objc2::sel!(string)] };
    let string: Retained<NSString> =
        if responds { unsafe { msg_send![text, string] } } else { unsafe { msg_send![text, description] } };
    string.to_string()
}

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

fn attributes(size: f64) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::systemFontOfSize(size);
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.1, 0.12, 1.0);
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, &color];
    NSDictionary::from_slices(&keys, &values)
}

impl Pad {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PadIvars::default());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn note(&self, line: String) {
        log(line.clone());
        self.ivars().last.replace(line);
    }

    fn command(&self, key: &str) {
        let Some(window) = self.window() else { return };
        let pasteboard = NSPasteboard::generalPasteboard();
        let kind = unsafe { NSPasteboardTypeString };
        match key {
            "c" => {
                pasteboard.clearContents();
                let text = NSString::from_str(&self.ivars().text.borrow());
                let ok = pasteboard.setString_forType(&text, kind);
                log(format!("copied ok={ok} count={}", pasteboard.changeCount()));
            }
            "v" => {
                let pasted = pasteboard.stringForType(kind).map(|s| s.to_string());
                log(format!("pasted {:?} count={}", pasted, pasteboard.changeCount()));
                if let Some(text) = pasted {
                    self.ivars().text.borrow_mut().push_str(&text);
                }
            }
            "m" => window.miniaturize(None),
            "z" => window.zoom(None),
            "f" => window.toggleFullScreen(None),
            "w" => window.performClose(None),
            "n" => {
                let second = open_window(self.mtm(), "Another window", rect(40.0, 40.0, 420.0, 260.0));
                second.makeKeyAndOrderFront(None);
                log(format!("opened another {}", describe(&second)));
                self.ivars().others.borrow_mut().push(second);
            }
            "t" => self.toggle_tip(&window),
            "i" => {
                let beam = NSCursor::IBeamCursor();
                let arrow = NSCursor::arrowCursor();
                let next = if NSCursor::currentCursor().isEqual(Some(&beam)) { arrow } else { beam };
                next.set();
                log(format!(
                    "cursor {}",
                    if next.isEqual(Some(&NSCursor::IBeamCursor())) { "I-beam" } else { "arrow" }
                ));
            }
            "h" => {
                NSCursor::setHiddenUntilMouseMoves(true);
                log("cursor hidden until the mouse moves".into());
            }
            "p" => {
                window.setIgnoresMouseEvents(true);
                log("clicks go through for five seconds".into());
                let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                    window.setIgnoresMouseEvents(false);
                    log("clicks come back".into());
                });
                let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(5.0, false, &block) };
            }
            "l" => {
                window.setMovable(!window.isMovable());
                log(format!("movable={}", window.isMovable()));
            }
            "o" => {
                let mtm = self.mtm();
                let modal = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        rect(0.0, 0.0, 320.0, 120.0),
                        NSWindowStyleMask::Titled,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                unsafe { modal.setReleasedWhenClosed(false) };
                modal.setTitle(&NSString::from_str("A modal window"));
                let content: Retained<ModalPad> = unsafe {
                    msg_send![super(ModalPad::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 320.0, 120.0)]
                };
                modal.setContentView(Some(&content));
                modal.makeFirstResponder(Some(&content));
                log("modal: running".into());
                let response = NSApplication::sharedApplication(mtm).runModalForWindow(&modal);
                modal.orderOut(None);
                log(format!("modal: ended with {response}"));
            }
            "g" => {
                let title = format!("{} and a longer title", window.title());
                window.setTitle(&NSString::from_str(&title));
                log(format!("title {title:?}"));
            }
            "e" => {
                let hidden = window.titleVisibility() == NSWindowTitleVisibility::Hidden;
                let next = if hidden { NSWindowTitleVisibility::Visible } else { NSWindowTitleVisibility::Hidden };
                window.setTitleVisibility(next);
                log(format!("title hidden={}", !hidden));
            }
            _ => {}
        }
    }

    /// Show or hide a small borderless child window at the last click, as
    /// tooltips and completion lists are made.
    fn toggle_tip(&self, window: &NSWindow) {
        if let Some(tip) = self.ivars().tip.take() {
            window.removeChildWindow(&tip);
            tip.orderOut(None);
            log("tip hidden".into());
            return;
        }
        let at = self.ivars().click.borrow().unwrap_or(NSPoint::new(100.0, 100.0));
        let in_window = self.convertPoint_toView(at, None);
        let on_screen = window.convertRectToScreen(rect(in_window.x, in_window.y - 60.0, 160.0, 60.0));
        let tip = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm()),
                on_screen,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { tip.setReleasedWhenClosed(false) };
        let view = Pad::new(self.mtm(), rect(0.0, 0.0, 160.0, 60.0));
        view.ivars().text.replace("tip".into());
        tip.setContentView(Some(&view));
        unsafe { window.addChildWindow_ordered(&tip, NSWindowOrderingMode::Above) };
        tip.orderFront(None);
        log(format!("tip shown at {:.0},{:.0}", at.x, at.y));
        self.ivars().tip.replace(Some(tip));
    }
}

fn open_window(mtm: MainThreadMarker, title: &str, frame: NSRect) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    let pad = Pad::new(mtm, NSRect::new(NSPoint::ZERO, frame.size));
    let hover: Retained<Hover> = unsafe {
        msg_send![super(Hover::alloc(mtm).set_ivars(HoverIvars::default())), initWithFrame: rect(480.0, 90.0, 200.0, 80.0)]
    };
    let beam: Retained<Beam> =
        unsafe { msg_send![super(Beam::alloc(mtm).set_ivars(())), initWithFrame: rect(480.0, 190.0, 200.0, 80.0)] };
    pad.addSubview(&hover);
    pad.addSubview(&beam);
    window.setContentView(Some(&pad));
    window.makeFirstResponder(Some(&pad));
    window
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    timers: RefCell<Vec<Retained<NSTimer>>>,
}

fn describe(window: &NSWindow) -> String {
    let f = window.frame();
    let c = window.contentRectForFrameRect(f);
    format!(
        "frame={:.0}x{:.0} content={:.0}x{:.0} scale={} key={} zoomed={}",
        f.size.width,
        f.size.height,
        c.size.width,
        c.size.height,
        window.backingScaleFactor(),
        window.isKeyWindow(),
        window.isZoomed()
    )
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "InputDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _: &NSNotification) {
            self.open_window();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationDidBecomeActive:))]
        fn did_become_active(&self, _: &NSNotification) {
            log("applicationDidBecomeActive".into());
        }

        #[unsafe(method(applicationDidResignActive:))]
        fn did_resign_active(&self, _: &NSNotification) {
            log("applicationDidResignActive".into());
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowDidBecomeKey:))]
        fn did_become_key(&self, _: &NSNotification) {
            log(format!("windowDidBecomeKey {}", describe(self.window())));
        }

        #[unsafe(method(windowDidResignKey:))]
        fn did_resign_key(&self, _: &NSNotification) {
            log("windowDidResignKey".into());
        }

        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _: &NSNotification) {
            log(format!("windowDidResize {}", describe(self.window())));
        }

        #[unsafe(method(windowDidMiniaturize:))]
        fn did_miniaturize(&self, _: &NSNotification) {
            log("windowDidMiniaturize".into());
        }

        #[unsafe(method(windowDidChangeOcclusionState:))]
        fn did_change_occlusion_state(&self, _: &NSNotification) {
            let visible = self.window().occlusionState().contains(NSWindowOcclusionState::Visible);
            log(format!("windowDidChangeOcclusionState visible={visible}"));
        }

        #[unsafe(method(windowDidEnterFullScreen:))]
        fn did_enter_full_screen(&self, _: &NSNotification) {
            log(format!("windowDidEnterFullScreen {}", describe(self.window())));
        }

        #[unsafe(method(windowDidExitFullScreen:))]
        fn did_exit_full_screen(&self, _: &NSNotification) {
            log(format!("windowDidExitFullScreen {}", describe(self.window())));
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn did_change_backing_properties(&self, _: &NSNotification) {
            log(format!("windowDidChangeBackingProperties {}", describe(self.window())));
        }

        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _: &NSWindow) -> bool {
            log("windowShouldClose".into());
            true
        }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _: &NSNotification) {
            log("windowWillClose".into());
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("window")
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let window = open_window(mtm, "Sidestep input", rect(0.0, 0.0, 720.0, 420.0));
        window.setContentMinSize(NSSize::new(320.0, 200.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));

        if let Some(secs) = std::env::var("INPUT_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
            });
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
            self.ivars().timers.borrow_mut().push(timer);
        }
        window.makeKeyAndOrderFront(None);
        log(format!("opened {}", describe(&window)));
        let _ = self.ivars().window.set(window);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
