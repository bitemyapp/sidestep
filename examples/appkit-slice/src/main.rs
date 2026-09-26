//! The AppKit slice: one window holding either a page of text with a blinking
//! caret (and optionally a 60 fps spinner), or a long list in a scroll view.
//! Written only against objc2-app-kit: on macOS it runs on AppKit, on Linux
//! on Sidestep.
//!
//! SCENARIO: the page alone (idle), with a caret blinking twice a second
//! (caret, the default), with a spinner turning at 60 fps (anim), or the list
//! scrolling at 120 px/s (scroll).
//! SLICE_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::{Cell, OnceCell, RefCell};
use std::f64::consts::{FRAC_PI_2, TAU};
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSResponder, NSScrollView, NSStringDrawing, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

const LINE_H: f64 = 20.0;
const MARGIN: f64 = 16.0;
const PAGE_LINES: usize = 60;
const LIST_LINES: usize = 2000;

fn line(i: usize) -> String {
    format!("{i:04}  fn example_{i}(value: usize) -> Result<Vec<String>, Error> {{ todo!() }}")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn intersects(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

/// Fonts, colors and text attributes, made once on the main thread.
struct Style {
    attrs: Retained<NSDictionary<NSString, AnyObject>>,
    bg: Retained<NSColor>,
    accent: Retained<NSColor>,
}

fn style() -> &'static Style {
    thread_local!(static STYLE: OnceCell<&'static Style> = const { OnceCell::new() });
    STYLE.with(|s| {
        *s.get_or_init(|| {
            let font = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
            let fg = NSColor::colorWithSRGBRed_green_blue_alpha(0.80, 0.84, 0.96, 1.0);
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let values: [&AnyObject; 2] = [&font, &fg];
            Box::leak(Box::new(Style {
                attrs: NSDictionary::from_slices(&keys, &values),
                bg: NSColor::colorWithSRGBRed_green_blue_alpha(0.118, 0.118, 0.180, 1.0),
                accent: NSColor::colorWithSRGBRed_green_blue_alpha(0.961, 0.663, 0.498, 1.0),
            }))
        })
    })
}

fn draw_line(i: usize, y: f64) {
    let text = NSString::from_str(&line(i));
    unsafe { text.drawAtPoint_withAttributes(NSPoint::new(MARGIN, y), Some(&style().attrs)) };
}

fn fill(r: NSRect) {
    NSBezierPath::fillRect(r);
}

#[derive(Default)]
struct PageIvars {
    caret_on: Cell<bool>,
    caret_line: Cell<usize>,
    spin: Cell<f64>,
    spinner: Cell<bool>,
}

define_class!(
    /// A page of text with a caret, and optionally a spinner.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SlicePage"]
    #[ivars = PageIvars]
    struct Page;

    impl Page {
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
            let s = style();
            s.bg.setFill();
            fill(dirty);
            for i in 0..PAGE_LINES {
                let y = MARGIN + i as f64 * LINE_H;
                if intersects(dirty, rect(0.0, y, 1e9, LINE_H)) {
                    draw_line(i, y);
                }
            }
            if self.ivars().caret_on.get() {
                s.accent.setFill();
                fill(self.caret_rect());
            }
            if self.ivars().spinner.get() {
                s.accent.setFill();
                let r = self.spinner_rect();
                let (cx, cy) = (r.origin.x + 12.0, r.origin.y + 12.0);
                let path = NSBezierPath::bezierPath();
                for k in 0..4 {
                    let a = self.ivars().spin.get() * TAU + k as f64 * FRAC_PI_2;
                    let p = NSPoint::new(cx + 8.0 * a.cos(), cy + 8.0 * a.sin());
                    if k == 0 { path.moveToPoint(p) } else { path.lineToPoint(p) }
                }
                path.closePath();
                path.fill();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let line = (((p.y - MARGIN) / LINE_H).max(0.0) as usize).min(PAGE_LINES - 1);
            self.setNeedsDisplayInRect(self.caret_rect());
            self.ivars().caret_line.set(line);
            self.ivars().caret_on.set(true);
            self.setNeedsDisplayInRect(self.caret_rect());
        }
    }
);

impl Page {
    fn new(mtm: MainThreadMarker, frame: NSRect, spinner: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PageIvars {
            caret_on: Cell::new(true),
            spinner: Cell::new(spinner),
            ..Default::default()
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn caret_rect(&self) -> NSRect {
        rect(MARGIN - 3.0, MARGIN + self.ivars().caret_line.get() as f64 * LINE_H + 1.0, 2.0, LINE_H - 2.0)
    }

    fn spinner_rect(&self) -> NSRect {
        rect(self.bounds().size.width - 40.0, 12.0, 24.0, 24.0)
    }

    fn blink(&self) {
        self.ivars().caret_on.set(!self.ivars().caret_on.get());
        self.setNeedsDisplayInRect(self.caret_rect());
    }

    fn step_spinner(&self, t: f64) {
        self.ivars().spin.set(t);
        self.setNeedsDisplayInRect(self.spinner_rect());
    }
}

define_class!(
    /// A tall list of lines, the document of a scroll view.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SliceList"]
    struct List;

    impl List {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            style().bg.setFill();
            fill(dirty);
            let first = (dirty.origin.y / LINE_H).floor().max(0.0) as usize;
            let last = (((dirty.origin.y + dirty.size.height) / LINE_H).ceil() as usize).min(LIST_LINES);
            for i in first..last {
                draw_line(i, i as f64 * LINE_H);
            }
        }
    }
);

impl List {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    timers: RefCell<Vec<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SliceDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.open_window();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _sender: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn every(&self, seconds: f64, f: impl Fn() + 'static) {
        let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, true, &block) };
        self.ivars().timers.borrow_mut().push(timer);
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "caret".into());
        let frame = rect(0.0, 0.0, 1280.0, 800.0);
        let style_mask = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style_mask,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Sidestep AppKit slice"));
        let sizable = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
        let start = Instant::now();

        if scenario == "scroll" {
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
            scroll.setHasVerticalScroller(true);
            scroll.setAutoresizingMask(sizable);
            let list = List::new(mtm, rect(0.0, 0.0, frame.size.width, LIST_LINES as f64 * LINE_H));
            list.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            scroll.setDocumentView(Some(&list));
            window.setContentView(Some(&scroll));
            self.every(1.0 / 60.0, move || {
                let clip = scroll.contentView();
                let visible = clip.bounds().size.height;
                let y = (start.elapsed().as_secs_f64() * 120.0) % (LIST_LINES as f64 * LINE_H - visible);
                clip.scrollToPoint(NSPoint::new(0.0, y));
                scroll.reflectScrolledClipView(&clip);
            });
        } else {
            let page = Page::new(mtm, frame, scenario == "anim");
            page.setAutoresizingMask(sizable);
            window.setContentView(Some(&page));
            window.makeFirstResponder(Some(&page));
            if scenario == "caret" {
                let page = page.clone();
                self.every(0.5, move || page.blink());
            }
            if scenario == "anim" {
                self.every(1.0 / 60.0, move || page.step_spinner(start.elapsed().as_secs_f64()));
            }
        }

        if let Some(secs) = std::env::var("SLICE_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
            });
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
            self.ivars().timers.borrow_mut().push(timer);
        }

        window.makeKeyAndOrderFront(None);
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
