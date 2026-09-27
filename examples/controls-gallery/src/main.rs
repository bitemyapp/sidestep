//! The controls gallery: every control Sidestep draws, in its normal,
//! pressed, disabled, on and default states, in one window. Written only
//! against objc2-app-kit: on macOS it shows AppKit's controls, on Linux
//! Sidestep's.
//!
//! SCENARIO: buttons, text, indicators, segmented, images, or all (the
//! default).
//! GALLERY_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::RefCell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezelStyle,
    NSBezierPath, NSBox, NSBoxType, NSButton, NSButtonType, NSCellImagePosition, NSColor, NSControlSize, NSFont,
    NSImage, NSImageFrameStyle, NSImageScaling, NSImageView, NSLineBreakMode, NSProgressIndicator,
    NSProgressIndicatorStyle, NSResponder, NSSearchField, NSSecureTextField, NSSegmentStyle, NSSegmentSwitchTracking,
    NSSegmentedControl, NSSlider, NSStepper, NSSwitch, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

define_class!(
    // A flipped view, so the gallery lays out from the top.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GalleryPage"]
    struct Page;

    impl Page {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// Lays controls out in rows, left to right.
struct Layout {
    page: Retained<NSView>,
    x: f64,
    y: f64,
    row: f64,
    /// The view to give the keyboard to when the window opens.
    focus: Option<Retained<NSView>>,
}

impl Layout {
    const MARGIN: f64 = 20.0;
    const GAP: f64 = 12.0;

    fn add(&mut self, view: &NSView) {
        let size = view.frame().size;
        view.setFrameOrigin(NSPoint::new(self.x, self.y));
        self.page.addSubview(view);
        self.x += size.width + Self::GAP;
        self.row = self.row.max(size.height);
    }

    fn next_row(&mut self) {
        self.x = Self::MARGIN;
        self.y += self.row + Self::GAP;
        self.row = 0.0;
    }
}

fn push(title: &str, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: no target or action.
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm) }
}

fn buttons(l: &mut Layout, mtm: MainThreadMarker) {
    l.add(&push("Push", mtm));
    let default = push("Default", mtm);
    default.setKeyEquivalent(ns_string!("\r"));
    l.add(&default);
    let destructive = push("Delete", mtm);
    destructive.setHasDestructiveAction(true);
    l.add(&destructive);
    let pressed = push("Pressed", mtm);
    pressed.highlight(true);
    l.add(&pressed);
    let disabled = push("Disabled", mtm);
    disabled.setEnabled(false);
    l.add(&disabled);
    let small = push("Small", mtm);
    small.setControlSize(NSControlSize::Small);
    small.sizeToFit();
    l.add(&small);
    l.next_row();

    for bezel in
        [NSBezelStyle::Circular, NSBezelStyle::HelpButton, NSBezelStyle::Disclosure, NSBezelStyle::PushDisclosure]
    {
        let b = push("", mtm);
        b.setBezelStyle(bezel);
        b.sizeToFit();
        l.add(&b);
    }
    for bezel in [NSBezelStyle::SmallSquare, NSBezelStyle::Toolbar, NSBezelStyle::Badge] {
        let b = push("Square", mtm);
        b.setBezelStyle(bezel);
        b.sizeToFit();
        l.add(&b);
    }
    let borderless = push("Borderless", mtm);
    borderless.setBordered(false);
    borderless.sizeToFit();
    l.add(&borderless);
    l.next_row();

    for (title, state, enabled) in [("Off", 0, true), ("On", 1, true), ("Mixed", -1, true), ("Disabled", 1, false)] {
        // SAFETY: no target or action.
        let b = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm) };
        b.setAllowsMixedState(true);
        b.setState(state);
        b.setEnabled(enabled);
        l.add(&b);
    }
    l.next_row();
    // Radio buttons in one superview with one action are a group: two
    // groups, each with one on.
    for (title, state, enabled, action) in [
        ("One", 1, true, sel!(first:)),
        ("Two", 0, true, sel!(first:)),
        ("Off", 0, false, sel!(second:)),
        ("On", 1, false, sel!(second:)),
    ] {
        // SAFETY: no target; the actions only name the groups.
        let b = unsafe {
            NSButton::radioButtonWithTitle_target_action(&NSString::from_str(title), None, Some(action), mtm)
        };
        l.add(&b);
        b.setState(state);
        b.setEnabled(enabled);
    }
    l.next_row();
}

fn text(l: &mut Layout, mtm: MainThreadMarker) {
    l.add(&NSTextField::labelWithString(ns_string!("A label"), mtm));
    let disabled = NSTextField::labelWithString(ns_string!("A disabled label"), mtm);
    disabled.setEnabled(false);
    l.add(&disabled);
    let bold = NSTextField::labelWithString(ns_string!("Bold, 16 points"), mtm);
    bold.setFont(Some(&NSFont::boldSystemFontOfSize(16.0)));
    bold.sizeToFit();
    l.add(&bold);
    let truncated = NSTextField::labelWithString(ns_string!("A label too long for its frame, truncated"), mtm);
    truncated.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    truncated.setFrameSize(NSSize::new(160.0, truncated.frame().size.height));
    l.add(&truncated);
    l.next_row();
    let wrapping = NSTextField::wrappingLabelWithString(
        ns_string!("A wrapping label: its text breaks between words onto as many lines as its width needs."),
        mtm,
    );
    wrapping.setFrameSize(NSSize::new(240.0, 60.0));
    l.add(&wrapping);
    l.next_row();
    let field = NSTextField::textFieldWithString(ns_string!("Some text"), mtm);
    field.setFrameSize(NSSize::new(160.0, field.frame().size.height));
    l.add(&field);
    l.focus = Some(Retained::into_super(Retained::into_super(field)));
    let placeholder = NSTextField::textFieldWithString(ns_string!(""), mtm);
    placeholder.setPlaceholderString(Some(ns_string!("Placeholder")));
    placeholder.setFrameSize(NSSize::new(160.0, placeholder.frame().size.height));
    l.add(&placeholder);
    let off = NSTextField::textFieldWithString(ns_string!("Disabled"), mtm);
    off.setEnabled(false);
    off.setFrameSize(NSSize::new(120.0, off.frame().size.height));
    l.add(&off);
    l.next_row();
    let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(0.0, 0.0, 160.0, 24.0));
    secure.setStringValue(ns_string!("hunter2"));
    l.add(&secure);
    let search = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 180.0, 24.0));
    search.setPlaceholderString(Some(ns_string!("Search")));
    l.add(&search);
    let query = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 180.0, 24.0));
    query.setStringValue(ns_string!("query"));
    l.add(&query);
    let bordered = NSTextField::textFieldWithString(ns_string!("Bordered"), mtm);
    bordered.setBordered(true);
    bordered.sizeToFit();
    bordered.setFrameSize(NSSize::new(120.0, bordered.frame().size.height));
    l.add(&bordered);
    l.next_row();
}

fn indicators(l: &mut Layout, mtm: MainThreadMarker) {
    for (value, indeterminate) in [(0.0, false), (35.0, false), (100.0, false), (0.0, true)] {
        let bar = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 140.0, 20.0));
        bar.setIndeterminate(indeterminate);
        bar.setDoubleValue(value);
        l.add(&bar);
        if indeterminate {
            unsafe { bar.startAnimation(None) };
        }
    }
    l.next_row();
    for (size, animate) in [
        (NSControlSize::Regular, true),
        (NSControlSize::Small, true),
        (NSControlSize::Mini, true),
        (NSControlSize::Regular, false),
    ] {
        let spinner = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 32.0, 32.0));
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(size);
        spinner.sizeToFit();
        l.add(&spinner);
        if animate {
            unsafe { spinner.startAnimation(None) };
        }
    }
    let determinate = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 32.0, 32.0));
    determinate.setStyle(NSProgressIndicatorStyle::Spinning);
    determinate.setIndeterminate(false);
    determinate.setDoubleValue(65.0);
    l.add(&determinate);
    l.next_row();
    let titled = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 200.0, 80.0));
    titled.setTitle(ns_string!("A box"));
    let inside = NSTextField::labelWithString(ns_string!("Content"), mtm);
    titled.contentView().expect("content").addSubview(&inside);
    l.add(&titled);
    let custom = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 120.0, 80.0));
    custom.setBoxType(NSBoxType::Custom);
    custom.setCornerRadius(8.0);
    custom.setBorderWidth(2.0);
    custom.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(0.21, 0.52, 0.89, 1.0));
    custom.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(0.21, 0.52, 0.89, 0.15));
    l.add(&custom);
    let separator = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 200.0, 1.0));
    separator.setBoxType(NSBoxType::Separator);
    let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 80.0));
    separator.setFrameOrigin(NSPoint::new(0.0, 40.0));
    holder.addSubview(&separator);
    l.add(&holder);
    l.next_row();
}

fn segmented(l: &mut Layout, mtm: MainThreadMarker) {
    let labels =
        |ls: &[&str]| NSArray::from_retained_slice(&ls.iter().map(|s| NSString::from_str(s)).collect::<Vec<_>>());
    let one = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &labels(&["Day", "Week", "Month", "Year"]),
            NSSegmentSwitchTracking::SelectOne,
            None,
            None,
            mtm,
        )
    };
    one.setSelectedSegment(1);
    l.add(&one);
    let any = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &labels(&["Bold", "Italic", "Underline"]),
            NSSegmentSwitchTracking::SelectAny,
            None,
            None,
            mtm,
        )
    };
    any.setSelected_forSegment(true, 0);
    any.setSelected_forSegment(true, 2);
    any.setSegmentStyle(NSSegmentStyle::Separated);
    l.add(&any);
    let off = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &labels(&["On", "Off"]),
            NSSegmentSwitchTracking::SelectOne,
            None,
            None,
            mtm,
        )
    };
    off.setSelectedSegment(0);
    off.setEnabled(false);
    l.add(&off);
    l.next_row();
    let stepper = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 20.0, 26.0));
    l.add(&stepper);
    for (value, ticks) in [(0.3, 0), (0.75, 5)] {
        let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 160.0, 21.0));
        slider.setNumberOfTickMarks(ticks);
        slider.setDoubleValue(value);
        l.add(&slider);
    }
    let vertical = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 21.0, 80.0));
    vertical.setDoubleValue(0.6);
    l.add(&vertical);
    for state in [0, 1] {
        let switch = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 54.0, 24.0));
        switch.setState(state);
        l.add(&switch);
    }
    let disabled = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 54.0, 24.0));
    disabled.setState(1);
    disabled.setEnabled(false);
    l.add(&disabled);
    l.next_row();
}

/// A symbol image, or the landscape picture as a template where the
/// system has no such symbol.
fn symbol(name: &str) -> Retained<NSImage> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None).unwrap_or_else(|| {
        let image = picture(16.0, 16.0);
        image.setTemplate(true);
        image
    })
}

/// A picture in color, drawn by a handler: a sky, a sun and a hill.
fn picture(w: f64, h: f64) -> Retained<NSImage> {
    let draw = RcBlock::new(move |r: NSRect| -> Bool {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.55, 0.78, 0.96, 1.0).set();
        NSBezierPath::fillRect(r);
        NSColor::colorWithSRGBRed_green_blue_alpha(0.98, 0.8, 0.2, 1.0).set();
        NSBezierPath::bezierPathWithOvalInRect(rect(w * 0.6, h * 0.55, w * 0.25, h * 0.25)).fill();
        NSColor::colorWithSRGBRed_green_blue_alpha(0.3, 0.62, 0.3, 1.0).set();
        NSBezierPath::bezierPathWithOvalInRect(rect(-w * 0.2, -h * 0.6, w * 1.1, h)).fill();
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(w, h), false, &draw)
}

fn image_view(image: &NSImage, frame: NSImageFrameStyle, size: f64, mtm: MainThreadMarker) -> Retained<NSImageView> {
    let v = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, size, size));
    v.setImage(Some(image));
    v.setImageFrameStyle(frame);
    v.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    v
}

// The square bezel names are deprecated, and programs still use them.
#[allow(deprecated)]
fn images(l: &mut Layout, mtm: MainThreadMarker) {
    // Image views: a picture in every frame style, a tinted symbol, a
    // disabled one, and an editable well that takes dropped images.
    let pic = picture(64.0, 48.0);
    for frame in [
        NSImageFrameStyle::None,
        NSImageFrameStyle::Photo,
        NSImageFrameStyle::GrayBezel,
        NSImageFrameStyle::Groove,
        NSImageFrameStyle::Button,
    ] {
        l.add(&image_view(&pic, frame, 64.0, mtm));
    }
    let tinted = image_view(&symbol("star.fill"), NSImageFrameStyle::None, 40.0, mtm);
    tinted.setContentTintColor(Some(&NSColor::systemOrangeColor()));
    l.add(&tinted);
    let plain = image_view(&symbol("gearshape"), NSImageFrameStyle::None, 40.0, mtm);
    l.add(&plain);
    let off = image_view(&symbol("gearshape"), NSImageFrameStyle::None, 40.0, mtm);
    off.setEnabled(false);
    l.add(&off);
    let well = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 64.0, 64.0));
    well.setImageFrameStyle(NSImageFrameStyle::GrayBezel);
    well.setEditable(true);
    l.add(&well);
    l.next_row();

    // Image-only buttons at every bezel.
    let star = symbol("star");
    for bezel in [
        NSBezelStyle::Push,
        NSBezelStyle::FlexiblePush,
        NSBezelStyle::Circular,
        NSBezelStyle::SmallSquare,
        NSBezelStyle::ShadowlessSquare,
        NSBezelStyle::TexturedSquare,
        NSBezelStyle::Toolbar,
        NSBezelStyle::Badge,
        NSBezelStyle::Disclosure,
    ] {
        // SAFETY: no target or action.
        let b = unsafe { NSButton::buttonWithImage_target_action(&star, None, None, mtm) };
        b.setBezelStyle(bezel);
        b.sizeToFit();
        l.add(&b);
    }
    // SAFETY: as above.
    let borderless = unsafe { NSButton::buttonWithImage_target_action(&star, None, None, mtm) };
    borderless.setBordered(false);
    borderless.sizeToFit();
    l.add(&borderless);
    l.next_row();

    // An image beside, above, below and under a title.
    let trash = symbol("trash");
    for (title, position, bezel) in [
        ("Leading", NSCellImagePosition::ImageLeading, NSBezelStyle::Push),
        ("Trailing", NSCellImagePosition::ImageTrailing, NSBezelStyle::Push),
        ("Above", NSCellImagePosition::ImageAbove, NSBezelStyle::FlexiblePush),
        ("Below", NSCellImagePosition::ImageBelow, NSBezelStyle::ShadowlessSquare),
        ("Overlaps", NSCellImagePosition::ImageOverlaps, NSBezelStyle::FlexiblePush),
    ] {
        // SAFETY: no target or action.
        let b = unsafe {
            NSButton::buttonWithTitle_image_target_action(&NSString::from_str(title), &trash, None, None, mtm)
        };
        b.setBezelStyle(bezel);
        b.setImagePosition(position);
        b.sizeToFit();
        l.add(&b);
    }
    // SAFETY: as above.
    let hugging =
        unsafe { NSButton::buttonWithTitle_image_target_action(ns_string!("Hugging"), &trash, None, None, mtm) };
    hugging.setImageHugsTitle(true);
    hugging.setFrameSize(NSSize::new(140.0, hugging.frame().size.height));
    l.add(&hugging);
    l.next_row();

    // Template images by state: pressed, disabled, the default button, a
    // toggle on (showing its alternate image), tinted borderless buttons.
    let gear = symbol("gearshape");
    type Setup<'a> = &'a dyn Fn(&NSButton);
    let states: [(&str, Setup); 6] = [
        ("Pressed", &|b| b.highlight(true)),
        ("Disabled", &|b| b.setEnabled(false)),
        ("Default", &|b| b.setKeyEquivalent(ns_string!("\r"))),
        ("Toggle", &|b| {
            b.setButtonType(NSButtonType::Toggle);
            b.setAlternateImage(Some(&symbol("gearshape.fill")));
            b.setState(1);
        }),
        ("Tinted", &|b| {
            b.setBordered(false);
            b.setContentTintColor(Some(&NSColor::systemPinkColor()));
        }),
        ("Plain", &|b| b.setBordered(false)),
    ];
    for (title, setup) in states {
        // SAFETY: no target or action.
        let b = unsafe {
            NSButton::buttonWithTitle_image_target_action(&NSString::from_str(title), &gear, None, None, mtm)
        };
        setup(&b);
        b.sizeToFit();
        l.add(&b);
    }
    l.next_row();
}

#[derive(Default)]
struct DelegateIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.open_window();
            if let Some(seconds) = std::env::var("GALLERY_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
                    std::process::exit(0);
                });
            }
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

    fn open_window(&self) {
        let mtm = self.mtm();
        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "all".into());
        let height = if matches!(scenario.as_str(), "all") { 920.0 } else { 620.0 };
        let size = NSSize::new(800.0, height);
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, size),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Controls"));
        let page: Retained<Page> =
            unsafe { msg_send![Page::alloc(mtm), initWithFrame: NSRect::new(NSPoint::ZERO, size)] };
        let page: Retained<NSView> = Retained::into_super(page);
        let mut l = Layout { page: page.clone(), x: Layout::MARGIN, y: Layout::MARGIN, row: 0.0, focus: None };
        let all = scenario == "all";
        if all || scenario == "buttons" {
            buttons(&mut l, mtm);
        }
        if all || scenario == "text" {
            text(&mut l, mtm);
        }
        if all || scenario == "indicators" {
            indicators(&mut l, mtm);
        }
        if all || scenario == "segmented" {
            segmented(&mut l, mtm);
        }
        if all || scenario == "images" {
            images(&mut l, mtm);
        }
        window.setContentView(Some(&page));
        if let Some(focus) = &l.focus {
            window.setInitialFirstResponder(Some(focus));
        }
        window.makeKeyAndOrderFront(None);
        self.ivars().window.replace(Some(window));
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
