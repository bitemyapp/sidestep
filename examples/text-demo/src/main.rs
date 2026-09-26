//! Text on show: weights and styles, kerning and ligatures, CJK, emoji,
//! right-to-left scripts and mixed text, paragraph alignment, wrapping and
//! truncation. Written only against objc2-app-kit: on macOS it runs on
//! AppKit, on Linux on Sidestep, which makes it the reference for comparing
//! the two by eye.
//!
//! SCENARIO=attributes shows a second page: underline styles and patterns,
//! strikethrough, outlined and slanted text, baseline offsets, kerning, tab
//! stops of every kind, and paragraphs mixing directions.
//!
//! SLICE_QUIT_AFTER: seconds until the app terminates itself.

use std::cell::OnceCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSFontDescriptorSymbolicTraits, NSFontWeight,
    NSFontWeightBlack, NSFontWeightBold, NSFontWeightHeavy, NSFontWeightLight, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSFontWeightThin, NSFontWeightUltraLight, NSForegroundColorAttributeName, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSStringDrawing, NSTextAlignment, NSTextTab,
    NSTextTabType, NSView, NSWindow, NSWindowStyleMask,
};
#[allow(deprecated)] // NSObliqueness, which TextKit 2 leaves out and string drawing draws.
use objc2_app_kit::{
    NSBaselineOffsetAttributeName, NSKernAttributeName, NSObliquenessAttributeName, NSStrikethroughStyleAttributeName,
    NSStrokeColorAttributeName, NSStrokeWidthAttributeName, NSUnderlineColorAttributeName,
    NSUnderlineStyleAttributeName,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSTimer, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

type Attributes = Retained<NSDictionary<NSString, AnyObject>>;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn attrs(font: &NSFont, color: &NSColor) -> Attributes {
    // SAFETY: the keys are constant strings.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[font as &AnyObject, color as &AnyObject])
}

fn attrs_with_style(font: &NSFont, color: &NSColor, style: &NSMutableParagraphStyle) -> Attributes {
    // SAFETY: the keys are constant strings.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
    NSDictionary::from_slices(&keys, &[font as &AnyObject, color as &AnyObject, style as &AnyObject])
}

fn ink() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.12, 0.12, 0.16, 1.0)
}

fn quiet() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.45, 0.45, 0.52, 1.0)
}

/// Draw `text` at (x, y) and return where the next piece on the line goes.
fn put(text: &str, x: f64, y: f64, attrs: &Attributes) -> f64 {
    let s = NSString::from_str(text);
    // SAFETY: the dictionary holds valid attributes.
    unsafe {
        s.drawAtPoint_withAttributes(NSPoint::new(x, y), Some(attrs));
        x + s.sizeWithAttributes(Some(attrs)).width
    }
}

fn boxed(text: &str, r: NSRect, attrs: &Attributes) {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.93, 0.92, 0.96, 1.0).setFill();
    NSBezierPath::fillRect(r);
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(text).drawInRect_withAttributes(r, Some(attrs)) };
}

fn label(text: &str, x: f64, y: f64) {
    put(text, x, y, &attrs(&NSFont::systemFontOfSize(11.0), &quiet()));
}

fn draw_page() {
    let ink = ink();
    let (left, right) = (24.0, 660.0);

    put("Sidestep text", left, 16.0, &attrs(&NSFont::boldSystemFontOfSize(28.0), &ink));

    label("Weights of the system font", left, 62.0);
    let weights: [(&str, NSFontWeight); 9] = unsafe {
        [
            ("UltraLight", NSFontWeightUltraLight),
            ("Thin", NSFontWeightThin),
            ("Light", NSFontWeightLight),
            ("Regular", NSFontWeightRegular),
            ("Medium", NSFontWeightMedium),
            ("Semibold", NSFontWeightSemibold),
            ("Bold", NSFontWeightBold),
            ("Heavy", NSFontWeightHeavy),
            ("Black", NSFontWeightBlack),
        ]
    };
    let mut x = left;
    for (name, weight) in weights {
        x = put(&format!("{name} "), x, 78.0, &attrs(&NSFont::systemFontOfSize_weight(17.0, weight), &ink));
    }
    let italic = NSFont::systemFontOfSize(17.0)
        .fontDescriptor()
        .fontDescriptorWithSymbolicTraits(NSFontDescriptorSymbolicTraits::TraitItalic);
    let italic = NSFont::fontWithDescriptor_size(&italic, 0.0).unwrap();
    let x = put("Italic, ", left, 104.0, &attrs(&italic, &ink));
    let bold_italic = italic.fontDescriptor().fontDescriptorWithSymbolicTraits(
        NSFontDescriptorSymbolicTraits::TraitItalic | NSFontDescriptorSymbolicTraits::TraitBold,
    );
    put("bold italic", x, 104.0, &attrs(&NSFont::fontWithDescriptor_size(&bold_italic, 0.0).unwrap(), &ink));

    label("Kerning and ligatures (serif)", left, 140.0);
    let serif = NSFont::fontWithName_size(ns_string!("Times New Roman"), 30.0).unwrap();
    put("AVATAR WAVE Type · office affluent fjord", left, 156.0, &attrs(&serif, &ink));

    label("Monospaced", left, 206.0);
    let mono = unsafe { NSFont::monospacedSystemFontOfSize_weight(15.0, NSFontWeightRegular) };
    put("fn main() { println!(\"0O 1lI\"); } // == != -> =>", left, 222.0, &attrs(&mono, &ink));

    label("CJK", left, 256.0);
    put(
        "中文：你好，世界。日本語：こんにちは。한국어: 안녕하세요",
        left,
        272.0,
        &attrs(&NSFont::systemFontOfSize(20.0), &ink),
    );

    label("Emoji", left, 312.0);
    put("😀 🎉 👍🏽 ❤️ 🇯🇵 👩‍💻 1️⃣ 🦀 with text", left, 328.0, &attrs(&NSFont::systemFontOfSize(24.0), &ink));

    label("Right to left, in a box, natural alignment", left, 374.0);
    let rtl = attrs(&NSFont::systemFontOfSize(20.0), &ink);
    boxed("العربية: مرحبا بالعالم", rect(left, 390.0, 580.0, 30.0), &rtl);
    boxed("עברית: שלום עולם", rect(left, 426.0, 580.0, 30.0), &rtl);
    label("Mixed directions", left, 470.0);
    put(
        "English, עברית 123 and العربية 456, then English.",
        left,
        486.0,
        &attrs(&NSFont::systemFontOfSize(18.0), &ink),
    );

    label("Sizes", left, 526.0);
    let mut x = left;
    for size in [9.0, 11.0, 13.0, 16.0, 20.0, 26.0, 34.0] {
        x = put("Ag ", x, 542.0, &attrs(&NSFont::systemFontOfSize(size), &ink)) + 4.0;
    }

    label("Colors", left, 598.0);
    let mut x = left;
    for (name, r, g, b) in
        [("red ", 0.85, 0.2, 0.2), ("green ", 0.15, 0.6, 0.25), ("blue ", 0.2, 0.35, 0.85), ("gold", 0.8, 0.6, 0.1)]
    {
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
        x = put(name, x, 614.0, &attrs(&NSFont::boldSystemFontOfSize(18.0), &color));
    }

    let body = NSFont::systemFontOfSize(13.0);
    let paragraph = "Text wraps at word boundaries inside the rectangle it is drawn in, and a word \
                     too long for a line, like Supercalifragilisticexpialidocious, breaks inside.";
    for (i, (name, alignment)) in [
        ("Left", NSTextAlignment::Left),
        ("Center", NSTextAlignment::Center),
        ("Right", NSTextAlignment::Right),
        ("Justified", NSTextAlignment::Justified),
    ]
    .into_iter()
    .enumerate()
    {
        let y = 16.0 + i as f64 * 110.0;
        label(&format!("{name}, line spacing 3"), right, y);
        let style = NSMutableParagraphStyle::new();
        style.setAlignment(alignment);
        style.setLineSpacing(3.0);
        boxed(paragraph, rect(right, y + 16.0, 300.0, 84.0), &attrs_with_style(&body, &ink, &style));
    }

    let long = "Truncation keeps what fits and marks what it leaves out with an ellipsis";
    for (i, (name, mode)) in [
        ("Truncating head", NSLineBreakMode::ByTruncatingHead),
        ("Truncating middle", NSLineBreakMode::ByTruncatingMiddle),
        ("Truncating tail", NSLineBreakMode::ByTruncatingTail),
        ("Clipping", NSLineBreakMode::ByClipping),
    ]
    .into_iter()
    .enumerate()
    {
        let y = 460.0 + i as f64 * 48.0;
        label(name, right, y);
        let style = NSMutableParagraphStyle::new();
        style.setLineBreakMode(mode);
        boxed(long, rect(right, y + 16.0, 260.0, 20.0), &attrs_with_style(&body, &ink, &style));
    }

    label("Tab stops: left, right and decimal", 990.0, 170.0);
    let tabs = NSMutableParagraphStyle::new();
    let options = NSDictionary::new();
    let tab = |alignment, location| unsafe {
        NSTextTab::initWithTextAlignment_location_options(NSTextTab::alloc(), alignment, location, &options)
    };
    // Replace the default stops, every 28 points, with three of our own.
    (1..=12).for_each(|i| tabs.removeTabStop(&tab(NSTextAlignment::Left, 28.0 * f64::from(i))));
    tabs.addTabStop(&tab(NSTextAlignment::Left, 12.0));
    tabs.addTabStop(&tab(NSTextAlignment::Right, 150.0));
    tabs.addTabStop(&NSTextTab::initWithType_location(NSTextTab::alloc(), NSTextTabType::DecimalTabStopType, 210.0));
    boxed(
        "\tApples\t3\t1.25\n\tPears\t12\t10.5\n\tFigs\t144\t0.125",
        rect(990.0, 186.0, 260.0, 56.0),
        &attrs_with_style(&body, &ink, &tabs),
    );

    let style = NSMutableParagraphStyle::new();
    style.setFirstLineHeadIndent(24.0);
    style.setHeadIndent(8.0);
    style.setParagraphSpacing(6.0);
    label("Indents and paragraph spacing", 990.0, 16.0);
    boxed(
        "A first line indented further than the rest.\nA second paragraph, after some space.",
        rect(990.0, 32.0, 260.0, 120.0),
        &attrs_with_style(&body, &ink, &style),
    );
}

/// A font and color with more attributes, as (key, value) pairs.
fn with(font: &NSFont, color: &NSColor, more: &[(&NSString, &AnyObject)]) -> Attributes {
    // SAFETY: the keys are constant strings.
    let mut keys = unsafe { vec![NSFontAttributeName, NSForegroundColorAttributeName] };
    let mut values: Vec<&AnyObject> = vec![font, color];
    for &(key, value) in more {
        keys.push(key);
        values.push(value);
    }
    NSDictionary::from_slices(&keys, &values)
}

fn number(value: f64) -> Retained<NSNumber> {
    NSNumber::new_f64(value)
}

#[allow(deprecated)]
fn draw_attributes_page() {
    let ink = ink();
    let (left, right) = (24.0, 660.0);
    let font = NSFont::systemFontOfSize(17.0);
    let body = attrs(&font, &ink);
    let plain = |more: &[(&NSString, &AnyObject)]| with(&font, &ink, more);
    put("Attributes", left, 16.0, &attrs(&NSFont::boldSystemFontOfSize(28.0), &ink));

    // SAFETY: the keys are constant strings, the values numbers and colors.
    let (underline, strike, stroke, stroke_color, oblique, baseline, kern, underline_color) = unsafe {
        (
            NSUnderlineStyleAttributeName,
            NSStrikethroughStyleAttributeName,
            NSStrokeWidthAttributeName,
            NSStrokeColorAttributeName,
            NSObliquenessAttributeName,
            NSBaselineOffsetAttributeName,
            NSKernAttributeName,
            NSUnderlineColorAttributeName,
        )
    };
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(0.85, 0.2, 0.2, 1.0);
    let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.35, 0.85, 1.0);

    label("Underlines: single, thick, double; dotted, dashed, dash-dot, dash-dot-dot; by word", left, 62.0);
    let mut x = left;
    for style in [0x01, 0x02, 0x09] {
        x = put("Underlined", x, 78.0, &plain(&[(underline, &number(f64::from(style)))])) + 16.0;
    }
    let mut x = left;
    for pattern in [0x100, 0x200, 0x300, 0x400] {
        let styled = plain(&[(underline, &number(f64::from(0x01 | pattern))), (underline_color, &blue)]);
        x = put("Patterned", x, 106.0, &styled) + 16.0;
    }
    put("only under the words", x, 106.0, &plain(&[(underline, &number(f64::from(0x01 | 0x8000)))]));

    label("Strikethrough, single and thick", left, 142.0);
    let x = put("Struck through", left, 158.0, &plain(&[(strike, &number(1.0))])) + 16.0;
    put("and thick", x, 158.0, &plain(&[(strike, &number(2.0))]));

    label("Outlined (stroke 3), filled and outlined in red (stroke -3), slanted (obliqueness 0.25)", left, 194.0);
    let big = NSFont::boldSystemFontOfSize(30.0);
    let x = put("Outline", left, 210.0, &with(&big, &ink, &[(stroke, &number(3.0))])) + 20.0;
    let x = put("Filled", x, 210.0, &with(&big, &ink, &[(stroke, &number(-3.0)), (stroke_color, &red)])) + 20.0;
    put("Slanted", x, 210.0, &with(&big, &ink, &[(oblique, &number(0.25))]));

    label("Baseline offsets and kerning", left, 262.0);
    let x = put("H", left, 290.0, &body);
    let x = put("2", x, 290.0, &plain(&[(baseline, &number(-4.0))]));
    let x = put("O and x", x, 290.0, &body);
    let x = put("2", x, 290.0, &plain(&[(baseline, &number(7.0))]));
    let x = put(";  ", x, 290.0, &body);
    let x = put("tracked out", x, 290.0, &plain(&[(kern, &number(4.0))])) + 12.0;
    put("AVATAR (kerning off)", x, 290.0, &plain(&[(kern, &number(0.0))]));

    label("Tab stops: left 20, center 150, right 260, decimal 340; then every 60 from the last", left, 338.0);
    let tab = |kind, location| NSTextTab::initWithType_location(NSTextTab::alloc(), kind, location);
    let tabs = NSMutableParagraphStyle::new();
    tabs.setTabStops(Some(&NSArray::from_retained_slice(&[
        tab(NSTextTabType::LeftTabStopType, 20.0),
        tab(NSTextTabType::CenterTabStopType, 150.0),
        tab(NSTextTabType::RightTabStopType, 260.0),
        tab(NSTextTabType::DecimalTabStopType, 340.0),
    ])));
    tabs.setDefaultTabInterval(60.0);
    let table = "\tItem\tCentered\tRight\tPrice\n\tApples\tred\t3\t1.25\n\tPears\tgreen and ripe\t12\t10.5\n\
                 \tFigs\tpurple\t144\t0.125\n\tTotal\t\t159\t11.875\tthen\tevery\t60";
    let columns = rect(left, 354.0, 600.0, 110.0);
    NSColor::colorWithSRGBRed_green_blue_alpha(0.93, 0.92, 0.96, 1.0).setFill();
    NSBezierPath::fillRect(columns);
    NSColor::colorWithSRGBRed_green_blue_alpha(0.8, 0.5, 0.5, 1.0).setFill();
    for stop in [20.0, 150.0, 260.0, 340.0, 400.0, 460.0, 520.0] {
        NSBezierPath::fillRect(rect(left + stop, 354.0, 1.0, 110.0));
    }
    let table_attrs = attrs_with_style(&NSFont::systemFontOfSize(15.0), &ink, &tabs);
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(table).drawInRect_withAttributes(columns, Some(&table_attrs)) };

    label("Mixed directions, wrapped, in both paragraph directions", left, 488.0);
    let mixed = "The word שלום means peace; مرحبا بالعالم means hello world, and 123 stays in order. \
                 עברית עם English באמצע ומספרים 42 ו-7.";
    let wrapped = attrs(&NSFont::systemFontOfSize(16.0), &ink);
    boxed(mixed, rect(left, 504.0, 290.0, 130.0), &wrapped);
    let rtl = NSMutableParagraphStyle::new();
    rtl.setBaseWritingDirection(objc2_app_kit::NSWritingDirection::RightToLeft);
    boxed(
        mixed,
        rect(left + 310.0, 504.0, 290.0, 130.0),
        &attrs_with_style(&NSFont::systemFontOfSize(16.0), &ink, &rtl),
    );

    label("Decorations on mixed text, and on emoji", right, 62.0);
    let decorated = plain(&[(underline, &number(1.0)), (strike, &number(1.0)), (underline_color, &red)]);
    boxed("Hello שלום مرحبا 😀 🎉", rect(right, 78.0, 300.0, 30.0), &decorated);
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextDemoPage"]
    struct Page;

    impl Page {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.99, 0.99, 0.985, 1.0).setFill();
            NSBezierPath::fillRect(dirty);
            if std::env::var("SCENARIO").as_deref() == Ok("attributes") {
                draw_attributes_page();
            } else {
                draw_page();
            }
        }
    }
);

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    timer: OnceCell<Retained<NSTimer>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextDemoDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let frame = rect(0.0, 0.0, 1280.0, 800.0);
            let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable;
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
            window.setTitle(ns_string!("Sidestep text"));
            let this = Page::alloc(mtm).set_ivars(());
            let page: Retained<Page> = unsafe { msg_send![super(this), initWithFrame: frame] };
            page.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            window.setContentView(Some(&page));
            window.makeKeyAndOrderFront(None);
            let _ = self.ivars().window.set(window);
            if let Some(secs) = std::env::var("SLICE_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
                let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                    NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
                });
                let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
                let _ = self.ivars().timer.set(timer);
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _sender: &NSApplication) -> bool {
            true
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let this = Delegate::alloc(mtm).set_ivars(DelegateIvars::default());
    let delegate: Retained<Delegate> = unsafe { msg_send![super(this), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
