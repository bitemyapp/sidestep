//! Input, window state and the pasteboard, logged: a window that prints
//! every event it gets, one line each, and shows the line typed so far.
//! Written only against objc2-app-kit: on macOS it runs on AppKit, on
//! Linux on Sidestep.
//!
//! Typing adds to the line; Backspace deletes. With Command (the Super key
//! on Linux): C copies the line, V pastes, M miniaturizes, Z zooms, F
//! toggles full screen, W closes the window, N opens another window, and T
//! shows or hides a borderless child window at the last click, I switches
//! between the I-beam and arrow cursors, and H hides the pointer until it
//! moves.
//!
//! INPUT_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::{OnceCell, RefCell};
use std::io::Write;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezierPath, NSColor,
    NSCursor, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSPasteboard,
    NSPasteboardTypeString, NSResponder, NSStringDrawing, NSView, NSWindow, NSWindowDelegate, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
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
            let typed = NSString::from_str(&format!("Typed: {}|", self.ivars().text.borrow()));
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
            let chars = e.characters().map(|c| c.to_string()).unwrap_or_default();
            if e.modifierFlags().contains(NSEventModifierFlags::Command) {
                let key = e.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
                self.command(&key);
            } else if chars == "\u{7f}" {
                self.ivars().text.borrow_mut().pop();
            } else if chars.chars().all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)) {
                self.ivars().text.borrow_mut().push_str(&chars);
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
                "scrollWheel dx={:.2} dy={:.2} scrolling={:.2},{:.2} precise={}",
                e.deltaX(),
                e.deltaY(),
                e.scrollingDeltaX(),
                e.scrollingDeltaY(),
                e.hasPreciseScrollingDeltas()
            ));
        }
    }
);

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
