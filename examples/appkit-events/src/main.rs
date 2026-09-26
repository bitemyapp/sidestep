//! Sheets, modal windows and tooltips, written only against objc2-app-kit:
//! on macOS it runs on AppKit, on Linux on Sidestep. `SCENARIO` chooses
//! what it shows:
//!
//! - `hover` (default): a window whose left box has a tooltip; on Linux the
//!   pointer is put over it (a headless compositor has no pointer to move),
//!   so the tooltip comes up.
//! - `sheet`: the window with a sheet attached, top-centre under its title
//!   bar.
//! - `modal`: a modal window over the main one; its box ends it when
//!   clicked, or Return does.
//!
//! EVENTS_QUIT_AFTER: seconds until the program terminates itself.

use std::cell::OnceCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezierPath, NSColor,
    NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSResponder, NSStringDrawing, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn after(seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block) }
}

struct BoxIvars {
    label: &'static str,
    color: (f64, f64, f64),
    /// Clicking ends the modal session.
    ends_modal: bool,
}

define_class!(
    /// A coloured box with a label.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EventsBox"]
    #[ivars = BoxIvars]
    struct LabelBox;

    impl LabelBox {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let (r, g, b) = self.ivars().color;
            NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
            NSBezierPath::fillRect(self.bounds());
            let font = NSFont::systemFontOfSize(14.0);
            let white = NSColor::whiteColor();
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let values: [&AnyObject; 2] = [&font, &white];
            let attributes = NSDictionary::from_slices(&keys, &values);
            let label = NSString::from_str(self.ivars().label);
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(12.0, 12.0), Some(&attributes)) };
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if self.ivars().ends_modal {
                NSApplication::sharedApplication(self.mtm()).stopModalWithCode(1);
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().ends_modal
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.characters().is_some_and(|c| c.to_string() == "\r") {
                NSApplication::sharedApplication(self.mtm()).stopModalWithCode(1);
            }
        }
    }
);

fn label_box(
    mtm: MainThreadMarker,
    frame: NSRect,
    label: &'static str,
    color: (f64, f64, f64),
    ends_modal: bool,
) -> Retained<LabelBox> {
    let this = LabelBox::alloc(mtm).set_ivars(BoxIvars { label, color, ends_modal });
    unsafe { msg_send![super(this), initWithFrame: frame] }
}

fn window(mtm: MainThreadMarker, title: &str, size: NSSize) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, size.width, size.height),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w.setTitle(&NSString::from_str(title));
    w
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    kept: OnceCell<Vec<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EventsDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _note: &NSNotification) {
            self.launched();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn launched(&self) {
        let mtm = self.mtm();
        let main = window(mtm, "Sidestep events", NSSize::new(480.0, 300.0));
        let content = label_box(mtm, rect(0.0, 0.0, 480.0, 300.0), "", (0.93, 0.93, 0.92), false);
        let tipped = label_box(mtm, rect(30.0, 150.0, 190.0, 100.0), "Hover here", (0.2, 0.45, 0.7), false);
        tipped.setToolTip(Some(&NSString::from_str("Tooltips show after the pointer rests")));
        let other = label_box(mtm, rect(260.0, 150.0, 190.0, 100.0), "A box", (0.55, 0.3, 0.5), false);
        content.addSubview(&tipped);
        content.addSubview(&other);
        main.setContentView(Some(&content));
        main.makeKeyAndOrderFront(None);
        let _ = self.ivars().window.set(main.clone());

        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "hover".into());
        let mut kept: Vec<Retained<AnyObject>> = Vec::new();
        match scenario.as_str() {
            "sheet" => {
                let sheet = window(mtm, "A sheet", NSSize::new(280.0, 120.0));
                let body = label_box(mtm, rect(0.0, 0.0, 280.0, 120.0), "A sheet, attached", (0.3, 0.55, 0.35), false);
                sheet.setContentView(Some(&body));
                main.beginSheet_completionHandler(&sheet, None);
                kept.push(Retained::into_super(Retained::into_super(Retained::into_super(sheet))));
            }
            "modal" => {
                let timer = after(0.3, move || {
                    let mtm = MainThreadMarker::new().unwrap();
                    let modal = window(mtm, "A modal window", NSSize::new(300.0, 120.0));
                    let body =
                        label_box(mtm, rect(0.0, 0.0, 300.0, 120.0), "Click or press Return", (0.7, 0.4, 0.2), true);
                    modal.setContentView(Some(&body));
                    modal.makeFirstResponder(Some(&body));
                    let response = NSApplication::sharedApplication(mtm).runModalForWindow(&modal);
                    modal.orderOut(None);
                    println!("modal ended with {response}");
                });
                kept.push(Retained::into_super(Retained::into_super(timer)));
            }
            _ => {
                let defaults = objc2_foundation::NSUserDefaults::standardUserDefaults();
                defaults.setInteger_forKey(200, &NSString::from_str("NSInitialToolTipDelay"));
                #[cfg(not(target_vendor = "apple"))]
                {
                    let main = main.clone();
                    let timer = after(0.3, move || {
                        let id = sidestep_appkit::testing::showing_id(&main);
                        sidestep_appkit::testing::inject_enter(id, 60.0, 90.0);
                    });
                    kept.push(Retained::into_super(Retained::into_super(timer)));
                }
            }
        }
        if let Some(secs) = std::env::var("EVENTS_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            let timer =
                after(secs, || NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None));
            kept.push(Retained::into_super(Retained::into_super(timer)));
        }
        let _ = self.ivars().kept.set(kept);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<Delegate> =
        unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(DelegateIvars::default())), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
