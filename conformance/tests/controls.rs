//! Controls and cells, checked on macOS and on Linux alike without a
//! window: defaults, values and their conversions, state cycling, target
//! and action, and every size and rectangle a program can observe.
//! Sizes that depend on text are checked as formulas over the text's own
//! measured size, so they hold whatever fonts a system has.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker));

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn text_size(text: &str, font: &NSFont) -> NSSize {
    // SAFETY: the key is a constant string.
    let key = unsafe { NSFontAttributeName };
    let attrs = NSDictionary::from_slices(&[key], &[font as &AnyObject]);
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attrs)) }
}

fn system(size: f64) -> Retained<NSFont> {
    NSFont::systemFontOfSize(size)
}

fn is_kind(object: &AnyObject, class: &AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

// What the target saw, as (action, sender state, sender highlighted).
thread_local!(static ACTIONS: RefCell<Vec<(&'static str, isize, bool)>> = const { RefCell::new(Vec::new()) });

fn take_actions() -> Vec<(&'static str, isize, bool)> {
    ACTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

fn sender_state(sender: &AnyObject) -> (isize, bool) {
    // SAFETY: senders here are controls.
    let control = unsafe { &*(sender as *const AnyObject).cast::<NSControl>() };
    let state = control.cell().map_or(0, |c| c.state());
    (state, control.isHighlighted())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceControlTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(first:))]
        fn first(&self, sender: &AnyObject) {
            let (state, highlighted) = sender_state(sender);
            ACTIONS.with(|a| a.borrow_mut().push(("first:", state, highlighted)));
        }

        #[unsafe(method(second:))]
        fn second(&self, sender: &AnyObject) {
            let (state, highlighted) = sender_state(sender);
            ACTIONS.with(|a| a.borrow_mut().push(("second:", state, highlighted)));
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![Target::alloc(mtm), init] }
}

// Cells

fn cell_defaults(mtm: MainThreadMarker) {
    let c = NSCell::new(mtm);
    assert_eq!(c.r#type(), NSCellType::NullCellType);
    assert_eq!(c.state(), NSControlStateValueOff);
    assert!(c.isEnabled());
    assert!(!c.isBordered() && !c.isBezeled() && !c.isEditable() && !c.isSelectable() && !c.isScrollable());
    assert!(!c.isHighlighted() && !c.isContinuous());
    assert!(c.wraps());
    assert_eq!(c.alignment(), NSTextAlignment::Left);
    assert_eq!(c.lineBreakMode(), NSLineBreakMode::ByWordWrapping);
    assert!(c.font().is_none());
    assert_eq!(c.title().to_string(), "");
    assert_eq!(c.stringValue().to_string(), "");
    assert!(c.objectValue().is_none());
    assert_eq!(c.intValue(), 0);
    assert_eq!(c.controlSize(), NSControlSize::Regular);
    assert_eq!(c.focusRingType(), NSFocusRingType::Default);
    assert!(!c.refusesFirstResponder() && c.acceptsFirstResponder() && !c.showsFirstResponder());
    assert!(!c.sendsActionOnEndEditing() && !c.truncatesLastVisibleLine() && !c.usesSingleLineMode());
    assert!(!c.allowsMixedState());
    assert_eq!(c.nextState(), 1);
    // A plain cell has no tag, target or action.
    assert_eq!(c.tag(), -1);
    assert!(c.action().is_none() && c.target().is_none());
    assert_eq!(c.backgroundStyle(), NSBackgroundStyle::Normal);
    assert!(!c.isOpaque());
    assert!(!NSCell::prefersTrackingUntilMouseUp(mtm));
    assert_eq!(NSCell::defaultFocusRingType(mtm), NSFocusRingType::Exterior);
    // A cell with no content has no size of its own: it takes whatever it's
    // given.
    assert_eq!(c.cellSize(), NSSize::new(40000.0, 40000.0));
    assert_eq!(c.cellSizeForBounds(rect(0.0, 0.0, 100.0, 100.0)), NSSize::new(100.0, 100.0));
    let b = rect(0.0, 0.0, 100.0, 50.0);
    assert_eq!((c.titleRectForBounds(b), c.drawingRectForBounds(b), c.imageRectForBounds(b)), (b, b, b));
    // Disabled cells don't take the keyboard.
    c.setEnabled(false);
    assert!(!c.isEnabled() && !c.acceptsFirstResponder());

    let a = NSActionCell::new(mtm);
    assert_eq!(a.r#type(), NSCellType::NullCellType);
    assert!(a.action().is_none() && a.target().is_none());
    assert_eq!(a.tag(), 0);
}

fn text_cells(mtm: MainThreadMarker) {
    let c = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("Hello"));
    assert_eq!(c.r#type(), NSCellType::TextCellType);
    assert_eq!(c.title().to_string(), "Hello");
    assert_eq!(c.stringValue().to_string(), "Hello");
    let font = c.font().expect("text cells have a font");
    assert_eq!(font.pointSize(), 13.0);
    let object = c.objectValue().expect("the string");
    assert!(is_kind(&object, NSString::class()));
    // The text plus two points each side; a border adds two points all
    // round, a bezel three.
    let t = text_size("Hello", &font);
    assert_eq!(c.cellSize(), NSSize::new(t.width + 4.0, t.height));
    assert_eq!(c.cellSizeForBounds(rect(0.0, 0.0, 100.0, 50.0)), NSSize::new(t.width + 4.0, t.height));
    let b = rect(0.0, 0.0, 100.0, 50.0);
    assert_eq!(c.titleRectForBounds(b), b);
    c.setBordered(true);
    assert_eq!(c.cellSize(), NSSize::new(t.width + 8.0, t.height + 4.0));
    assert_eq!(c.titleRectForBounds(b), rect(2.0, 2.0, 96.0, 46.0));
    assert_eq!(c.drawingRectForBounds(b), rect(2.0, 2.0, 96.0, 46.0));
    c.setBordered(false);
    c.setBezeled(true);
    assert_eq!(c.cellSize(), NSSize::new(t.width + 10.0, t.height + 6.0));
    assert_eq!(c.titleRectForBounds(b), rect(3.0, 3.0, 94.0, 44.0));

    // The title is the string value.
    c.setTitle(&NSString::from_str("title"));
    assert_eq!(c.stringValue().to_string(), "title");
    // A cell given a string becomes a text cell.
    let n = NSCell::new(mtm);
    n.setStringValue(&NSString::from_str("x"));
    assert_eq!(n.r#type(), NSCellType::TextCellType);
    assert_eq!(n.stringValue().to_string(), "x");

    let a = NSActionCell::initTextCell(NSActionCell::alloc(mtm), &NSString::from_str("Hi"));
    assert_eq!(a.r#type(), NSCellType::TextCellType);
    assert_eq!(a.font().map(|f| f.pointSize()), Some(13.0));
}

fn cell_values(mtm: MainThreadMarker) {
    let v = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str(""));
    v.setIntValue(42);
    assert_eq!(v.stringValue().to_string(), "42");
    assert_eq!((v.doubleValue(), v.floatValue(), v.integerValue()), (42.0, 42.0, 42));
    let object = v.objectValue().expect("a number");
    assert!(is_kind(&object, NSNumber::class()));
    v.setDoubleValue(3.75);
    assert_eq!(v.stringValue().to_string(), "3.75");
    assert_eq!(v.intValue(), 3);
    v.setFloatValue(1.5);
    assert_eq!(v.stringValue().to_string(), "1.5");
    assert_eq!(v.intValue(), 1);
    for (value, text) in [(0.1, "0.1"), (2.0, "2"), (1.0 / 3.0, "0.3333333333333333"), (-0.5, "-0.5")] {
        v.setDoubleValue(value);
        assert_eq!(v.stringValue().to_string(), text);
    }
    v.setIntegerValue(-9);
    assert_eq!(v.stringValue().to_string(), "-9");

    // Strings convert as NSString's own accessors read them.
    v.setStringValue(&NSString::from_str("  12abc"));
    assert_eq!((v.intValue(), v.doubleValue(), v.integerValue()), (12, 12.0, 12));
    v.setStringValue(&NSString::from_str("7.9"));
    assert_eq!((v.intValue(), v.integerValue(), v.doubleValue()), (7, 7, 7.9));
    v.setStringValue(&NSString::from_str("-3.5e2"));
    assert_eq!((v.intValue(), v.doubleValue()), (-3, -350.0));

    // SAFETY: nil and an NSNumber are valid object values.
    unsafe { v.setObjectValue(None) };
    assert_eq!(v.stringValue().to_string(), "");
    assert!(v.objectValue().is_none());
    assert_eq!(v.intValue(), 0);
    unsafe { v.setObjectValue(Some(&NSNumber::new_i32(5))) };
    assert_eq!(v.stringValue().to_string(), "5");
    assert_eq!(v.intValue(), 5);

    // An attributed string is the object value itself.
    let attributed = NSAttributedString::from_nsstring(&NSString::from_str("att"));
    v.setAttributedStringValue(&attributed);
    assert_eq!(v.stringValue().to_string(), "att");
    assert!(is_kind(&v.objectValue().expect("the string"), NSAttributedString::class()));
    assert_eq!(v.attributedStringValue().string().to_string(), "att");
    // A plain string comes back as an attributed one.
    v.setStringValue(&NSString::from_str("plain"));
    assert_eq!(v.attributedStringValue().string().to_string(), "plain");
}

fn cell_state(mtm: MainThreadMarker) {
    let s = NSCell::new(mtm);
    // Any other value is on, and mixed is on without mixed state.
    s.setState(5);
    assert_eq!(s.state(), NSControlStateValueOn);
    s.setState(NSControlStateValueMixed);
    assert_eq!(s.state(), NSControlStateValueOn);
    s.setAllowsMixedState(true);
    s.setState(NSControlStateValueMixed);
    assert_eq!(s.state(), NSControlStateValueMixed);
    assert_eq!(s.nextState(), NSControlStateValueOn);
    // Mixed, on, off, mixed.
    let mut seen = Vec::new();
    for _ in 0..3 {
        s.setNextState();
        seen.push(s.state());
    }
    assert_eq!(seen, [1, 0, -1]);
    s.setAllowsMixedState(false);
    s.setState(NSControlStateValueOff);
    s.setNextState();
    assert_eq!(s.state(), NSControlStateValueOn);
    s.setNextState();
    assert_eq!(s.state(), NSControlStateValueOff);

    // Actions go out on the mouse-up by default; sendActionOn: answers
    // the previous mask.
    let previous = s.sendActionOn(NSEventMask::LeftMouseDown);
    assert_eq!(previous as u64, NSEventMask::LeftMouseUp.0);
    assert_eq!(s.sendActionOn(NSEventMask::LeftMouseUp) as u64, NSEventMask::LeftMouseDown.0);
    s.setContinuous(true);
    assert!(s.isContinuous());
}

// Controls

fn control_defaults(mtm: MainThreadMarker) {
    let c = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
    assert!(NSControl::cellClass(mtm).is_none());
    assert!(c.cell().is_none());
    assert!(c.isEnabled());
    assert_eq!(c.stringValue().to_string(), "");
    assert!(c.font().is_none());
    assert_eq!(c.alignment(), NSTextAlignment::Left);
    assert_eq!(c.controlSize(), NSControlSize::Regular);
    assert!(!c.isContinuous() && !c.refusesFirstResponder() && !c.ignoresMultiClick());
    assert_eq!(c.tag(), 0);
    // SAFETY: the constant is a CGFloat.
    let none = unsafe { NSViewNoIntrinsicMetric };
    assert_eq!(none, -1.0);
    assert_eq!(c.intrinsicContentSize(), NSSize::new(none, none));
    // A control without a cell still keeps a value.
    c.setStringValue(&NSString::from_str("x"));
    assert_eq!(c.stringValue().to_string(), "x");
    c.setIntValue(3);
    assert_eq!(c.stringValue().to_string(), "3");

    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(v.intrinsicContentSize(), NSSize::new(none, none));
    assert_eq!(v.fittingSize(), NSSize::new(0.0, 0.0));
}

fn control_forwards_to_its_cell(mtm: MainThreadMarker) {
    let c = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
    let cell = NSActionCell::initTextCell(NSActionCell::alloc(mtm), &NSString::from_str("cellvalue"));
    c.setCell(Some(&cell));
    assert_eq!(c.stringValue().to_string(), "cellvalue");
    let view = unsafe { cell.controlView() }.expect("the control");
    assert!(std::ptr::eq(&*view, &**c));
    assert!(c.selectedCell().is_some_and(|s| std::ptr::eq(&*s, &**cell)));
    // The control's tag is its own; the selected tag is the cell's.
    cell.setTag(7);
    assert_eq!((c.tag(), c.selectedTag()), (0, 7));
    c.setTag(9);
    assert_eq!((c.tag(), cell.tag()), (9, 7));
    // Target, action, enabled and the value are the cell's.
    let t = target(mtm);
    // SAFETY: the target outlives the control's use of it.
    unsafe {
        c.setTarget(Some(&t));
        c.setAction(Some(sel!(first:)));
    }
    assert_eq!(cell.action(), Some(sel!(first:)));
    assert!(cell.target().is_some_and(|x| std::ptr::eq(&*x, &**t as &AnyObject)));
    c.setEnabled(false);
    assert!(!cell.isEnabled());
    c.setEnabled(true);
    c.setDoubleValue(2.5);
    assert_eq!(cell.stringValue().to_string(), "2.5");
    c.setAlignment(NSTextAlignment::Center);
    assert_eq!(cell.alignment(), NSTextAlignment::Center);
    c.setFont(Some(&system(20.0)));
    assert_eq!(cell.font().map(|f| f.pointSize()), Some(20.0));
    c.setFont(Some(&system(13.0)));
    c.setStringValue(&NSString::from_str("cellvalue"));

    // Sizing to fit takes the cell's size, rounded up.
    let size = cell.cellSize();
    let fit = NSSize::new(size.width.ceil(), size.height.ceil());
    c.sizeToFit();
    assert_eq!(c.frame().size, fit);
    assert_eq!(c.sizeThatFits(NSSize::new(10.0, 10.0)), fit);

    // The target is weak.
    objc2::rc::autoreleasepool(|_| {
        let gone = target(mtm);
        unsafe { c.setTarget(Some(&gone)) };
    });
    assert!(c.target().is_none());
}

fn actions(mtm: MainThreadMarker) {
    let _app = NSApplication::sharedApplication(mtm);
    let t = target(mtm);
    let c = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
    let cell = NSActionCell::initTextCell(NSActionCell::alloc(mtm), &NSString::from_str("x"));
    c.setCell(Some(&cell));
    // SAFETY: the target outlives the control's use of it.
    unsafe {
        c.setTarget(Some(&t));
        c.setAction(Some(sel!(first:)));
    }
    take_actions();
    // sendAction:to: sends whatever the control's state.
    assert!(unsafe { c.sendAction_to(Some(sel!(second:)), Some(&t)) });
    c.setEnabled(false);
    assert!(unsafe { c.sendAction_to(Some(sel!(second:)), Some(&t)) });
    assert_eq!(take_actions().len(), 2);
    c.setEnabled(true);
    // No action, or nobody to take it: nothing is sent.
    assert!(!unsafe { c.sendAction_to(None, Some(&t)) });
    assert!(!unsafe { c.sendAction_to(Some(sel!(nobodyHasThis:)), None) });
    assert!(take_actions().is_empty());

    // performClick: advances the state, then sends the action while the
    // cell shows highlighted.
    unsafe { c.performClick(None) };
    assert_eq!(take_actions(), [("first:", 1, true)]);
    assert!(!c.isHighlighted());
    unsafe { c.performClick(None) };
    assert_eq!(take_actions(), [("first:", 0, true)]);
    // Not when disabled.
    c.setEnabled(false);
    unsafe { c.performClick(None) };
    assert!(take_actions().is_empty());
    assert_eq!(cell.state(), 0);
}

// Buttons

fn push(title: &str, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: no target or action.
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm) }
}

fn button_defaults(mtm: MainThreadMarker) {
    let b = NSButton::initWithFrame(NSButton::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
    let cell = b.cell().expect("a cell");
    assert!(is_kind(&cell, NSButtonCell::class()));
    assert_eq!(b.title().to_string(), "Button");
    assert_eq!(b.stringValue().to_string(), "0");
    assert_eq!(b.state(), 0);
    assert_eq!(b.bezelStyle(), NSBezelStyle::Automatic);
    assert!(b.isBordered() && !b.isTransparent());
    assert_eq!(b.imagePosition(), NSCellImagePosition::NoImage);
    assert_eq!(b.font().map(|f| f.pointSize()), Some(13.0));
    assert_eq!(b.alignment(), NSTextAlignment::Center);
    assert_eq!(b.keyEquivalent().to_string(), "");
    assert_eq!(b.keyEquivalentModifierMask(), NSEventModifierFlags::empty());
    assert_eq!(cell.r#type(), NSCellType::TextCellType);
    assert!(b.isFlipped());
    assert!(b.contentTintColor().is_none() && b.bezelColor().is_none());
    assert!(!b.hasDestructiveAction() && !b.showsBorderOnlyWhileMouseInside() && !b.imageHugsTitle());
    assert_eq!(b.alternateTitle().to_string(), "");
    assert_eq!(b.attributedTitle().string().to_string(), "Button");
    let (mut delay, mut interval) = (0.0f32, 0.0f32);
    // SAFETY: two floats to write.
    unsafe { b.getPeriodicDelay_interval(std::ptr::NonNull::from(&mut delay), std::ptr::NonNull::from(&mut interval)) };
    assert_eq!((delay, interval), (0.4, 0.075));
    assert_eq!(cell.sendActionOn(NSEventMask::LeftMouseUp) as u64, NSEventMask::LeftMouseUp.0);
    // SAFETY: +prefersTrackingUntilMouseUp takes nothing.
    let prefers: bool = unsafe { msg_send![NSButtonCell::class(), prefersTrackingUntilMouseUp] };
    assert!(!prefers);
    // SAFETY: +cellClass takes nothing and returns a class.
    let cell_class: Option<&AnyClass> = unsafe { msg_send![NSButton::class(), cellClass] };
    assert!(cell_class.is_some_and(|c| std::ptr::eq(c, NSButtonCell::class())));

    let p = push("OK", mtm);
    assert_eq!(p.bezelStyle(), NSBezelStyle::Automatic);
    assert_eq!(p.lineBreakMode(), NSLineBreakMode::ByTruncatingTail);
    assert_eq!(p.alignment(), NSTextAlignment::Center);
    assert_eq!(p.imageScaling(), NSImageScaling::ScaleProportionallyDown);
}

fn button_values(mtm: MainThreadMarker) {
    // A button's value is its state; its title is apart.
    let t = push("T", mtm);
    t.setState(1);
    assert_eq!((t.stringValue().to_string(), t.intValue()), ("1".to_string(), 1));
    assert!(is_kind(&t.objectValue().expect("a number"), NSNumber::class()));
    assert_eq!(t.title().to_string(), "T");
    t.setStringValue(&NSString::from_str("0"));
    assert_eq!(t.state(), 0);
    assert_eq!(t.title().to_string(), "T");
    t.setIntValue(5);
    assert_eq!(t.state(), 1);
    t.setAllowsMixedState(true);
    t.setIntValue(-1);
    assert_eq!(t.state(), -1);
    t.setIntValue(-5);
    assert_eq!(t.state(), -1);
    // The key equivalent changes nothing else.
    t.setKeyEquivalent(&NSString::from_str("\r"));
    assert_eq!(t.keyEquivalentModifierMask(), NSEventModifierFlags::empty());
    assert_eq!(t.bezelStyle(), NSBezelStyle::Automatic);
}

fn button_types(mtm: MainThreadMarker) {
    use NSButtonType as T;
    let cases = [
        (T::MomentaryLight, 12, 0),
        (T::PushOnPushOff, 14, 12),
        (T::Toggle, 3, 1),
        (T::Switch, 1, 1),
        (T::Radio, 1, 1),
        (T::MomentaryChange, 1, 0),
        (T::OnOff, 12, 12),
        (T::MomentaryPushIn, 14, 0),
        (T::Accelerator, 12, 0),
    ];
    for (kind, highlights, shows) in cases {
        let b = NSButton::initWithFrame(NSButton::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
        b.setButtonType(kind);
        let cell = b.cell().expect("a cell");
        // SAFETY: the cell is an NSButtonCell.
        let cell: Retained<NSButtonCell> = unsafe { Retained::cast_unchecked(cell) };
        assert_eq!((cell.highlightsBy().0, cell.showsStateBy().0), (highlights, shows), "{kind:?}");
        let boxed = matches!(kind, T::Switch | T::Radio);
        assert_eq!(b.isBordered(), !boxed, "{kind:?}");
        let position = if boxed { NSCellImagePosition::ImageLeading } else { NSCellImagePosition::NoImage };
        assert_eq!(b.imagePosition(), position, "{kind:?}");
        // Every type toggles, except that a radio button stays on.
        b.setNextState();
        let first = b.state();
        b.setNextState();
        assert_eq!((first, b.state()), (1, if kind == T::Radio { 1 } else { 0 }), "{kind:?}");
    }
}

fn push_buttons(mtm: MainThreadMarker) {
    // As tall as the control size says; as wide as the title, in the
    // system font at the size's size, plus that height again.
    let sizes = [
        (NSControlSize::Regular, 24.0, 13.0),
        (NSControlSize::Small, 20.0, 11.0),
        (NSControlSize::Mini, 16.0, 9.0),
        (NSControlSize::Large, 28.0, 13.0),
    ];
    for (size, height, font) in sizes {
        for title in ["A", "OK", "Cancel", "Some long title"] {
            let b = push(title, mtm);
            b.setControlSize(size);
            // The font stays; the title is measured at the size's.
            assert_eq!(b.font().map(|f| f.pointSize()), Some(13.0));
            let w = text_size(title, &system(font)).width.ceil();
            assert_eq!(b.intrinsicContentSize(), NSSize::new(w + height, height), "{title:?} {size:?}");
            assert_eq!(b.cell().expect("a cell").cellSize(), NSSize::new(w + height, height));
        }
        let b = push("", mtm);
        b.setControlSize(size);
        assert_eq!(b.intrinsicContentSize(), NSSize::new(10.0 + height, height), "empty {size:?}");
    }
    // The factory sizes the button to fit.
    let b = push("Cancel", mtm);
    let w = text_size("Cancel", &system(13.0)).width.ceil();
    assert_eq!(b.frame(), rect(0.0, 0.0, w + 24.0, 24.0));
    // A font set by the program is used, at any control size; the height
    // stays the control size's.
    b.setFont(Some(&system(20.0)));
    let w20 = text_size("Cancel", &system(20.0)).width.ceil();
    assert_eq!(b.intrinsicContentSize(), NSSize::new(w20 + 24.0, 24.0));
    b.setFont(Some(&system(13.0)));
    // The title rect is the title's size, centered; the drawing rect is the
    // bezel, the control size's height centered, 12 in from each end.
    let c = b.cell().expect("a cell");
    let t = text_size("Cancel", &system(13.0));
    let (tw, th) = (t.width.ceil(), t.height);
    let bounds = rect(0.0, 0.0, 100.0, 40.0);
    assert_eq!(c.titleRectForBounds(bounds), rect(((100.0 - tw) / 2.0).floor(), (40.0 - th) / 2.0, tw, th));
    assert_eq!(c.drawingRectForBounds(bounds), rect(12.0, 8.0, 76.0, 24.0));
    let fit = rect(0.0, 0.0, tw + 24.0, 24.0);
    assert_eq!(c.drawingRectForBounds(fit), rect(12.0, 0.0, tw, 24.0));
    // Borderless: the title alone.
    b.setBordered(false);
    assert_eq!(b.intrinsicContentSize(), NSSize::new(tw + 4.0, th));
}

fn check_boxes(mtm: MainThreadMarker) {
    // A box, a gap, then the title in the size's font; as tall as the
    // taller of the box and the title.
    let sizes = [
        (NSControlSize::Regular, 16.0, 6.0, 13.0),
        (NSControlSize::Small, 14.0, 4.0, 11.0),
        (NSControlSize::Mini, 12.0, 4.0, 9.0),
        (NSControlSize::Large, 18.0, 6.0, 13.0),
    ];
    for radio in [false, true] {
        for (size, side, gap, font) in sizes {
            let title = "Check this";
            // SAFETY: no target or action.
            let b = unsafe {
                if radio {
                    NSButton::radioButtonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
                } else {
                    NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
                }
            };
            b.setControlSize(size);
            let t = text_size(title, &system(font));
            let expected = NSSize::new(side + gap + t.width.ceil(), side.max(t.height));
            assert_eq!(b.intrinsicContentSize(), expected, "radio {radio} {size:?}");
            b.setTitle(&NSString::from_str(""));
            assert_eq!(b.intrinsicContentSize(), NSSize::new(side, side), "empty, radio {radio} {size:?}");
        }
    }
    // SAFETY: no target or action.
    let b = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("A"), None, None, mtm) };
    assert!(!b.isBordered());
    assert_eq!(b.imagePosition(), NSCellImagePosition::ImageLeading);
    assert_eq!(b.lineBreakMode(), NSLineBreakMode::ByTruncatingTail);
    assert_eq!(b.alignment(), NSTextAlignment::Natural);
    assert_eq!((b.state(), b.allowsMixedState()), (0, false));
    let t = text_size("A", &system(13.0));
    assert_eq!(b.frame(), rect(0.0, 0.0, 22.0 + t.width.ceil(), t.height.max(16.0)));
    let c = b.cell().expect("a cell");
    let bounds = rect(0.0, 0.0, 120.0, 30.0);
    assert_eq!(c.imageRectForBounds(bounds), rect(0.0, 7.0, 16.0, 16.0));
    assert_eq!(c.titleRectForBounds(bounds), rect(22.0, (30.0 - t.height) / 2.0, t.width.ceil(), t.height));
    // A check box's title is 20-point text: taller than the box.
    b.setFont(Some(&system(20.0)));
    let t20 = text_size("A", &system(20.0));
    assert_eq!(b.intrinsicContentSize(), NSSize::new(22.0 + t20.width.ceil(), t20.height));
}

fn buttons_of_every_bezel(mtm: MainThreadMarker) {
    let t = text_size("Cancel", &system(13.0));
    let (tw, th) = (t.width.ceil(), t.height);
    let small = text_size("Cancel", &system(11.0)).width.ceil();
    let cases = [
        (NSBezelStyle::Circular, (tw + 8.0, 24.0), (18.0, 18.0)),
        (NSBezelStyle::HelpButton, (24.0, 24.0), (24.0, 24.0)),
        (NSBezelStyle::Disclosure, (13.0, 13.0), (13.0, 13.0)),
        (NSBezelStyle::PushDisclosure, (24.0, 24.0), (24.0, 24.0)),
        (NSBezelStyle::SmallSquare, (tw + 6.0, th + 4.0), (2.0, 4.0)),
        (NSBezelStyle::FlexiblePush, (tw + 24.0, 24.0), (34.0, 18.0)),
        (NSBezelStyle::AccessoryBar, (tw + 24.0, 24.0), (34.0, 24.0)),
        (NSBezelStyle::AccessoryBarAction, (tw + 24.0, 24.0), (34.0, 24.0)),
        (NSBezelStyle::Toolbar, (tw + 14.0, 20.0), (10.0, 20.0)),
        (NSBezelStyle::Badge, (small + 10.0, 18.0), (18.0, 14.0)),
        #[allow(deprecated)]
        (NSBezelStyle::TexturedSquare, (tw + 8.0, 20.0), (4.0, 20.0)),
    ];
    for (bezel, (w, h), (ew, eh)) in cases {
        let b = push("Cancel", mtm);
        b.setBezelStyle(bezel);
        assert_eq!(b.intrinsicContentSize(), NSSize::new(w, h), "{bezel:?}");
        b.setTitle(&NSString::from_str(""));
        assert_eq!(b.intrinsicContentSize(), NSSize::new(ew, eh), "empty {bezel:?}");
    }
}

// Text fields

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(text), mtm)
}

fn text_field_defaults(mtm: MainThreadMarker) {
    let f = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    let c = f.cell().expect("a cell");
    assert!(is_kind(&c, NSTextFieldCell::class()));
    assert!(f.isEditable() && f.isSelectable() && f.isBezeled() && !f.isBordered() && f.drawsBackground());
    assert_eq!(f.bezelStyle(), NSTextFieldBezelStyle::SquareBezel);
    assert_eq!(f.font().map(|f| f.pointSize()), Some(13.0));
    assert_eq!(f.lineBreakMode(), NSLineBreakMode::ByWordWrapping);
    assert_eq!(f.alignment(), NSTextAlignment::Left);
    assert!(c.wraps() && !c.isScrollable() && !c.usesSingleLineMode() && !c.truncatesLastVisibleLine());
    assert_eq!((f.maximumNumberOfLines(), f.preferredMaxLayoutWidth()), (0, 0.0));
    assert!(f.placeholderString().is_none());
    assert!(f.textColor().is_some() && f.backgroundColor().is_some());
    assert!(f.acceptsFirstResponder() && !f.refusesFirstResponder() && f.isEnabled() && !f.isContinuous());
    assert_eq!(f.stringValue().to_string(), "");
    assert!(f.isFlipped());
    let th = text_size("", &system(13.0)).height;
    assert_eq!(f.intrinsicContentSize(), NSSize::new(-1.0, th + 8.0));
    // The title rect is 4 in from the bezel, and at least a line tall,
    // centered when the field is shorter.
    let expect = |w: f64, h: f64| {
        if h - 8.0 >= th { rect(4.0, 4.0, w - 8.0, h - 8.0) } else { rect(4.0, ((h - th) / 2.0).floor(), w - 8.0, th) }
    };
    assert_eq!(c.titleRectForBounds(f.bounds()), expect(100.0, 22.0));
    assert_eq!(c.titleRectForBounds(rect(0.0, 0.0, 100.0, 40.0)), expect(100.0, 40.0));
    assert_eq!(c.drawingRectForBounds(rect(0.0, 0.0, 100.0, 40.0)), expect(100.0, 40.0));

    let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    let sc = secure.cell().expect("a cell");
    assert!(is_kind(&sc, NSSecureTextFieldCell::class()));
    // SAFETY: the cell is a secure text field cell.
    let sc: Retained<NSSecureTextFieldCell> = unsafe { Retained::cast_unchecked(sc) };
    assert!(sc.echosBullets());
}

fn labels(mtm: MainThreadMarker) {
    let l = label("Hello", mtm);
    assert!(!l.isEditable() && !l.isSelectable() && !l.isBezeled() && !l.isBordered() && !l.drawsBackground());
    assert_eq!(l.lineBreakMode(), NSLineBreakMode::ByClipping);
    assert_eq!(l.alignment(), NSTextAlignment::Natural);
    let c = l.cell().expect("a cell");
    assert!(!c.wraps() && !c.isScrollable());
    assert!(!l.acceptsFirstResponder());
    assert!(l.textColor().is_some());
    let font = system(13.0);
    for text in ["", "A", "Hello", "Hello world", "Two\nlines", "gjpqy"] {
        let l = label(text, mtm);
        let t = text_size(text, &font);
        // The text plus two points each side, sized to fit; the intrinsic
        // size leaves the padding out.
        assert_eq!(l.cell().expect("a cell").cellSize(), NSSize::new(t.width + 4.0, t.height), "{text:?}");
        assert_eq!(l.frame().size, NSSize::new((t.width + 4.0).ceil(), t.height), "{text:?}");
        assert_eq!(l.intrinsicContentSize(), NSSize::new(t.width.ceil(), t.height), "{text:?}");
        assert_eq!(l.cell().expect("a cell").titleRectForBounds(l.bounds()), l.bounds());
    }
    // Other fonts measure the same way.
    let l = label("Hello", mtm);
    l.setFont(Some(&system(20.0)));
    let t = text_size("Hello", &system(20.0));
    assert_eq!(l.intrinsicContentSize(), NSSize::new(t.width.ceil(), t.height));
    // The control size doesn't change a label's font.
    l.setControlSize(NSControlSize::Small);
    assert_eq!(l.font().map(|f| f.pointSize()), Some(20.0));
    // Sizing to fit takes the padding.
    let l = label("Hi there", mtm);
    l.setFrameSize(NSSize::new(200.0, 50.0));
    l.sizeToFit();
    let t = text_size("Hi there", &font);
    assert_eq!(l.frame().size, NSSize::new((t.width + 4.0).ceil(), t.height));
    assert_eq!(l.fittingSize(), l.frame().size);
}

fn wrapping_labels(mtm: MainThreadMarker) {
    let text = "This is a long wrapping label that should wrap onto several lines when narrow";
    let w = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    let c = w.cell().expect("a cell");
    assert_eq!(w.lineBreakMode(), NSLineBreakMode::ByWordWrapping);
    assert!(c.wraps() && !c.isScrollable() && w.isSelectable() && !w.isEditable());
    assert_eq!((w.maximumNumberOfLines(), w.preferredMaxLayoutWidth()), (0, 0.0));
    let th = text_size("", &system(13.0)).height;
    // Unconstrained: one line.
    assert_eq!(w.intrinsicContentSize().height, th);
    // With a preferred width: several lines, no wider.
    w.setPreferredMaxLayoutWidth(100.0);
    let s = w.intrinsicContentSize();
    assert!(s.width <= 100.0, "{s:?}");
    assert!(s.height > th && s.height % th == 0.0, "{s:?}");
    // At most so many lines.
    w.setMaximumNumberOfLines(2);
    assert_eq!(w.intrinsicContentSize().height, 2.0 * th);
}

fn text_fields(mtm: MainThreadMarker) {
    let font = system(13.0);
    let t = text_size("Edit me", &font);
    let th = t.height;
    let f = NSTextField::textFieldWithString(&NSString::from_str("Edit me"), mtm);
    let c = f.cell().expect("a cell");
    assert!(f.isEditable() && f.isBezeled() && !f.isBordered() && f.drawsBackground());
    assert_eq!(f.lineBreakMode(), NSLineBreakMode::ByClipping);
    assert!(!c.wraps() && c.isScrollable() && !c.usesSingleLineMode());
    // A bezel adds 4 all round, plus the text's own 2 each side.
    assert_eq!(c.cellSize(), NSSize::new((t.width + 12.0).ceil(), th + 8.0));
    assert_eq!(f.frame().size, NSSize::new((t.width + 12.0).ceil(), th + 8.0));
    assert_eq!(f.intrinsicContentSize(), NSSize::new(-1.0, th + 8.0));
    // A border: 2 each side, and odd insets for the title.
    f.setBordered(true);
    assert!(!f.isBezeled());
    assert_eq!(c.cellSize(), NSSize::new(t.width + 8.0, th + 4.0));
    assert_eq!(f.intrinsicContentSize(), NSSize::new(-1.0, th + 4.0));
    assert_eq!(c.titleRectForBounds(rect(0.0, 0.0, 100.0, 40.0)), rect(2.0, 3.0, 96.0, 35.0));
    // Neither: the text alone.
    f.setBordered(false);
    assert_eq!(c.cellSize(), NSSize::new(t.width + 4.0, th));
    assert_eq!(f.intrinsicContentSize(), NSSize::new(-1.0, th));
    assert_eq!(c.titleRectForBounds(rect(0.0, 0.0, 100.0, 40.0)), rect(0.0, 0.0, 100.0, 40.0));
    // Other fonts.
    let big = NSTextField::textFieldWithString(&NSString::from_str("Hello"), mtm);
    big.setFont(Some(&system(20.0)));
    let t20 = text_size("Hello", &system(20.0));
    assert_eq!(big.cell().expect("a cell").cellSize(), NSSize::new((t20.width + 12.0).ceil(), t20.height + 8.0));
    // An empty field; its placeholder counts while it's empty.
    let empty = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
    assert_eq!(empty.frame().size, NSSize::new(12.0, th + 8.0));
    empty.setPlaceholderString(Some(&NSString::from_str("Placeholder text")));
    assert_eq!(empty.placeholderString().map(|p| p.to_string()), Some("Placeholder text".into()));
    let p = text_size("Placeholder text", &font);
    assert_eq!(empty.cell().expect("a cell").cellSize(), NSSize::new((p.width + 12.0).ceil(), th + 8.0));
}

fn search_fields(mtm: MainThreadMarker) {
    let th = text_size("", &system(13.0)).height;
    let f = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 200.0, 22.0));
    let c = f.cell().expect("a cell");
    assert!(is_kind(&c, NSSearchFieldCell::class()));
    assert_eq!(f.bezelStyle(), NSTextFieldBezelStyle::RoundedBezel);
    assert!(f.isBezeled() && !f.isBordered() && !f.drawsBackground());
    assert!(c.usesSingleLineMode() && !c.wraps());
    assert!(!f.sendsSearchStringImmediately() && !f.sendsWholeSearchString());
    assert_eq!(f.intrinsicContentSize(), NSSize::new(-1.0, th + 8.0));
    // The text between the magnifier and the clear button; the buttons'
    // rects are 9 points tall, centered.
    let mid = |h: f64, part: f64| ((h - part) / 2.0).round();
    assert_eq!(f.searchTextBounds(), rect(22.0, ((22.0 - th) / 2.0).floor(), 153.0, th));
    assert_eq!(f.searchButtonBounds(), rect(6.0, mid(22.0, 9.0), 16.0, 9.0));
    assert_eq!(f.cancelButtonBounds(), rect(181.0, mid(22.0, 9.0), 15.0, 9.0));
    // SAFETY: the cell is a search field cell.
    let sc: Retained<NSSearchFieldCell> = unsafe { Retained::cast_unchecked(c) };
    let b = rect(0.0, 0.0, 300.0, 40.0);
    assert_eq!(sc.searchTextRectForBounds(b), rect(22.0, ((40.0 - th) / 2.0).floor(), 253.0, th));
    assert_eq!(sc.searchButtonRectForBounds(b), rect(6.0, mid(40.0, 9.0), 16.0, 9.0));
    assert_eq!(sc.cancelButtonRectForBounds(b), rect(281.0, mid(40.0, 9.0), 15.0, 9.0));
    f.setStringValue(&NSString::from_str("query"));
    assert_eq!(f.cancelButtonBounds(), rect(181.0, mid(22.0, 9.0), 15.0, 9.0));
}

fn notification_names(_mtm: MainThreadMarker) {
    // SAFETY: the names are constant strings.
    unsafe {
        assert_eq!(NSControlTextDidBeginEditingNotification.to_string(), "NSControlTextDidBeginEditingNotification");
        assert_eq!(NSControlTextDidChangeNotification.to_string(), "NSControlTextDidChangeNotification");
        assert_eq!(NSControlTextDidEndEditingNotification.to_string(), "NSControlTextDidEndEditingNotification");
    }
}

// Boxes

fn new_box(mtm: MainThreadMarker) -> Retained<NSBox> {
    NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0))
}

fn content_frame(b: &NSBox) -> NSRect {
    b.contentView().expect("a content view").frame()
}

fn boxes(mtm: MainThreadMarker) {
    let b = new_box(mtm);
    assert_eq!(b.boxType(), NSBoxType::Primary);
    assert_eq!(b.titlePosition(), NSTitlePosition::AtTop);
    assert_eq!(b.title().to_string(), "Title");
    assert_eq!(b.titleFont().pointSize(), 11.0);
    assert_eq!(b.contentViewMargins(), NSSize::new(5.0, 5.0));
    assert!(!b.isTransparent() && !b.isFlipped());
    assert_eq!((b.borderWidth(), b.cornerRadius()), (1.0, 0.0));
    let content = b.contentView().expect("a content view");
    assert!(!content.isFlipped());
    assert!(unsafe { content.superview() }.is_some_and(|s| std::ptr::eq(&*s, &**b as &NSView)));
    // The title is its text plus 4 each side, 7 in from the left, at the
    // top; the border comes down to 2 points under the title's top edge
    // less its height; the content sits inside the border by the margins.
    let t = text_size("Title", &b.titleFont());
    let (tw, th) = (t.width + 8.0, t.height);
    let top = 100.0 - (th - 2.0);
    for pos in [NSTitlePosition::AboveTop, NSTitlePosition::AtTop] {
        b.setTitlePosition(pos);
        assert_eq!(b.borderRect(), rect(0.0, 0.0, 200.0, top), "{pos:?}");
        assert_eq!(b.titleRect(), rect(7.0, 100.0 - th, tw, th), "{pos:?}");
        assert_eq!(content_frame(&b), rect(5.0, 5.0, 190.0, top - 10.0), "{pos:?}");
    }
    b.setTitlePosition(NSTitlePosition::NoTitle);
    assert_eq!(b.borderRect(), rect(0.0, 0.0, 200.0, 100.0));
    assert_eq!(b.titleRect(), NSRect::ZERO);
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 190.0, 90.0));
    b.setTitlePosition(NSTitlePosition::BelowTop);
    assert_eq!(b.borderRect(), rect(0.0, 0.0, 200.0, 100.0));
    assert_eq!(b.titleRect(), rect(7.0, 100.0 - th + 2.0, tw, th));
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 190.0, 100.0 - th + 4.0 - 5.0));
    b.setTitlePosition(NSTitlePosition::AboveBottom);
    assert_eq!(b.borderRect(), rect(0.0, 0.0, 200.0, 100.0));
    assert_eq!(b.titleRect(), rect(7.0, -2.0, tw, th));
    assert_eq!(content_frame(&b), rect(5.0, th - 4.0, 190.0, 95.0 - (th - 4.0)));
    for pos in [NSTitlePosition::AtBottom, NSTitlePosition::BelowBottom] {
        b.setTitlePosition(pos);
        assert_eq!(b.borderRect(), rect(0.0, th - 2.0, 200.0, 100.0 - (th - 2.0)), "{pos:?}");
        assert_eq!(b.titleRect(), rect(7.0, 0.0, tw, th), "{pos:?}");
        assert_eq!(content_frame(&b), rect(5.0, th + 3.0, 190.0, 95.0 - (th + 3.0)), "{pos:?}");
    }
    b.setTitlePosition(NSTitlePosition::AtTop);
    b.setTitle(&NSString::from_str(""));
    assert_eq!(b.titleRect(), rect(7.0, 100.0 - th, 8.0, th));
    b.setTitle(&NSString::from_str("Title"));
    b.setContentViewMargins(NSSize::new(10.0, 20.0));
    assert_eq!(content_frame(&b), rect(10.0, 20.0, 180.0, top - 40.0));
    b.setContentViewMargins(NSSize::new(5.0, 5.0));
    // The content follows the box.
    b.setFrameSize(NSSize::new(300.0, 150.0));
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 290.0, 150.0 - (th - 2.0) - 10.0));
}

// Border types are deprecated, but programs set them.
#[allow(deprecated)]
fn custom_and_separator_boxes(mtm: MainThreadMarker) {
    let b = new_box(mtm);
    b.setBoxType(NSBoxType::Custom);
    // A custom box has no title; its line insets the content by one more
    // point, whatever its width, unless it has no border. (AppKit moves
    // the content when the title position is next set.)
    for pos in [NSTitlePosition::NoTitle, NSTitlePosition::AtTop] {
        b.setTitlePosition(pos);
        assert_eq!(b.borderRect(), rect(0.0, 0.0, 200.0, 100.0));
        assert_eq!(b.titleRect(), NSRect::ZERO);
        assert_eq!(content_frame(&b), rect(6.0, 6.0, 188.0, 88.0));
    }
    for width in [0.0, 3.0, 10.0] {
        b.setBorderWidth(width);
        assert_eq!(content_frame(&b), rect(6.0, 6.0, 188.0, 88.0));
    }
    b.setBorderWidth(1.0);
    b.setBorderType(NSBorderType::NoBorder);
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 190.0, 90.0));
    b.setBorderType(NSBorderType::LineBorder);
    assert_eq!(content_frame(&b), rect(6.0, 6.0, 188.0, 88.0));
    b.setCornerRadius(8.0);
    b.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0));
    assert_eq!(b.cornerRadius(), 8.0);
    // A primary box's border type changes nothing it measures.
    let p = new_box(mtm);
    p.setTitlePosition(NSTitlePosition::NoTitle);
    for kind in [NSBorderType::NoBorder, NSBorderType::LineBorder, NSBorderType::BezelBorder] {
        p.setBorderType(kind);
        assert_eq!(content_frame(&p), rect(5.0, 5.0, 190.0, 90.0));
    }

    let sep = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 200.0, 1.0));
    sep.setBoxType(NSBoxType::Separator);
    assert!(sep.contentView().is_none());
    assert_eq!(sep.borderRect(), rect(0.0, 0.0, 200.0, 1.0));
    assert_eq!(sep.intrinsicContentSize(), NSSize::new(-1.0, 1.0));
}

fn box_sizing(mtm: MainThreadMarker) {
    // The frame for a content frame: out by the margins, and the title.
    let b = new_box(mtm);
    let th = text_size("Title", &b.titleFont()).height;
    b.setFrameFromContentFrame(rect(10.0, 10.0, 50.0, 50.0));
    assert_eq!(b.frame(), rect(5.0, 5.0, 60.0, 60.0 + th - 2.0));
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 50.0, 50.0));
    // Sizing to fit wraps the content round its subviews, which move to its
    // origin while the box moves by as much.
    let b = new_box(mtm);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    let inner = NSView::initWithFrame(NSView::alloc(mtm), rect(3.0, 4.0, 20.0, 30.0));
    b.contentView().expect("a content view").addSubview(&inner);
    b.sizeToFit();
    assert_eq!(b.frame(), rect(3.0, 4.0, 30.0, 40.0));
    assert_eq!(content_frame(&b), rect(5.0, 5.0, 20.0, 30.0));
    assert_eq!(inner.frame(), rect(0.0, 0.0, 20.0, 30.0));
    // A titled box is at least as wide as its title and 20 points more.
    let b = new_box(mtm);
    let t = text_size("Title", &b.titleFont());
    let inner = NSView::initWithFrame(NSView::alloc(mtm), rect(3.0, 4.0, 20.0, 30.0));
    b.contentView().expect("a content view").addSubview(&inner);
    b.sizeToFit();
    assert_eq!(b.frame(), rect(3.0, 4.0, (t.width + 8.0 + 20.0).max(30.0), 40.0 + th - 2.0));
    // Replacing the content view puts the new one in the content rect.
    let b = new_box(mtm);
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 5.0, 5.0));
    b.setContentView(Some(&v));
    assert_eq!(v.frame(), rect(5.0, 5.0, 190.0, 100.0 - (th - 2.0) - 10.0));
    assert!(unsafe { v.superview() }.is_some_and(|s| std::ptr::eq(&*s, &**b as &NSView)));
}

// Progress indicators

// `isBezeled` is deprecated, but programs read it.
#[allow(deprecated)]
fn progress_indicators(mtm: MainThreadMarker) {
    let p = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 100.0, 30.0));
    assert_eq!(p.style(), NSProgressIndicatorStyle::Bar);
    assert!(p.isIndeterminate() && p.isBezeled() && p.isDisplayedWhenStopped() && p.usesThreadedAnimation());
    assert_eq!((p.minValue(), p.maxValue(), p.doubleValue()), (0.0, 100.0, 0.0));
    assert_eq!(p.controlSize(), NSControlSize::Regular);
    // Values stay between the limits as they're set; moving a limit leaves
    // the value alone.
    p.setIndeterminate(false);
    p.setDoubleValue(150.0);
    assert_eq!(p.doubleValue(), 100.0);
    p.setDoubleValue(-5.0);
    assert_eq!(p.doubleValue(), 0.0);
    p.setDoubleValue(50.0);
    p.incrementBy(10.0);
    assert_eq!(p.doubleValue(), 60.0);
    p.incrementBy(100.0);
    assert_eq!(p.doubleValue(), 100.0);
    p.setDoubleValue(60.0);
    p.setMaxValue(40.0);
    assert_eq!(p.doubleValue(), 60.0);
    // Starting and stopping change nothing a program reads.
    unsafe {
        p.startAnimation(None);
        p.stopAnimation(None);
    }
    // Sizes per control size: a bar's height, a spinner's square.
    let sizes = [
        (NSControlSize::Regular, 20.0, 32.0),
        (NSControlSize::Small, 12.0, 16.0),
        (NSControlSize::Mini, 12.0, 10.0),
        (NSControlSize::Large, 20.0, 32.0),
    ];
    for (size, bar, spinner) in sizes {
        let q = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
        q.setControlSize(size);
        assert_eq!(q.intrinsicContentSize(), NSSize::new(-1.0, bar), "bar {size:?}");
        q.sizeToFit();
        assert_eq!(q.frame().size, NSSize::new(100.0, bar), "bar {size:?}");
        q.setStyle(NSProgressIndicatorStyle::Spinning);
        assert_eq!(q.intrinsicContentSize(), NSSize::new(spinner, spinner), "spinner {size:?}");
        q.sizeToFit();
        assert_eq!(q.frame().size, NSSize::new(spinner, spinner), "spinner {size:?}");
        assert!(q.isDisplayedWhenStopped() && q.isIndeterminate());
    }
}

// Segmented controls

fn segmented(labels: &[&str], mtm: MainThreadMarker) -> Retained<NSSegmentedControl> {
    let labels: Vec<Retained<NSString>> = labels.iter().map(|l| NSString::from_str(l)).collect();
    let labels = objc2_foundation::NSArray::from_retained_slice(&labels);
    // SAFETY: no target or action.
    unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &labels,
            NSSegmentSwitchTracking::SelectOne,
            None,
            None,
            mtm,
        )
    }
}

fn segmented_controls(mtm: MainThreadMarker) {
    let s = NSSegmentedControl::initWithFrame(NSSegmentedControl::alloc(mtm), rect(0.0, 0.0, 200.0, 24.0));
    assert_eq!((s.segmentCount(), s.selectedSegment()), (0, -1));
    assert_eq!(s.segmentStyle(), NSSegmentStyle::Automatic);
    assert_eq!(s.trackingMode(), NSSegmentSwitchTracking::SelectOne);
    assert_eq!(s.segmentDistribution(), NSSegmentDistribution::Fill);
    assert!(is_kind(&s.cell().expect("a cell"), NSSegmentedCell::class()));
    assert_eq!(s.intrinsicContentSize(), NSSize::new(0.0, 0.0));
    s.setSegmentCount(3);
    assert_eq!((0..3).map(|i| s.tagForSegment(i)).collect::<Vec<_>>(), [0, 0, 0]);
    assert!((0..3).all(|i| s.labelForSegment(i).is_none() && s.widthForSegment(i) == 0.0));

    let s = segmented(&["One", "Two", "Three"], mtm);
    let font = system(13.0);
    let w = |l: &str| text_size(l, &font).width.ceil() + 20.0;
    // Each segment is its label and 20 points, with a point between.
    let natural = w("One") + w("Two") + w("Three") + 2.0;
    assert_eq!(s.intrinsicContentSize(), NSSize::new(natural, 24.0));
    assert_eq!(s.frame(), rect(0.0, 0.0, natural, 24.0));
    assert_eq!(s.segmentCount(), 3);
    assert_eq!(s.selectedSegment(), -1);
    // The factory tags segments by index.
    assert_eq!((0..3).map(|i| s.tagForSegment(i)).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(s.labelForSegment(2).map(|l| l.to_string()), Some("Three".into()));
    assert!((0..3).all(|i| s.isEnabledForSegment(i) && s.widthForSegment(i) == 0.0));
    // Selecting one deselects the others; deselecting leaves the selected
    // segment's index as it was.
    s.setSelectedSegment(1);
    assert_eq!(s.selectedSegment(), 1);
    assert_eq!((0..3).map(|i| s.isSelectedForSegment(i)).collect::<Vec<_>>(), [false, true, false]);
    s.setSelected_forSegment(true, 2);
    assert_eq!(s.selectedSegment(), 2);
    assert_eq!((0..3).map(|i| s.isSelectedForSegment(i)).collect::<Vec<_>>(), [false, false, true]);
    s.setSelected_forSegment(false, 2);
    assert_eq!(s.selectedSegment(), 2);
    assert!((0..3).all(|i| !s.isSelectedForSegment(i)));
    // Any number may be selected; the selected segment is the last chosen.
    s.setTrackingMode(NSSegmentSwitchTracking::SelectAny);
    s.setSelected_forSegment(true, 0);
    s.setSelected_forSegment(true, 2);
    assert_eq!(s.selectedSegment(), 2);
    assert_eq!((0..3).map(|i| s.isSelectedForSegment(i)).collect::<Vec<_>>(), [true, false, true]);
    s.setSelectedSegment(-1);
    assert_eq!(s.selectedSegment(), -1);
    assert!((0..3).all(|i| !s.isSelectedForSegment(i)));
    // New segments are empty: 24 points each.
    s.setSegmentCount(5);
    assert_eq!(s.labelForSegment(3), None);
    assert_eq!(s.intrinsicContentSize(), NSSize::new(natural + 2.0 * 25.0, 24.0));
    s.setSegmentCount(2);
    assert_eq!(s.selectedSegment(), -1);
    // A width set is the segment's width.
    s.setWidth_forSegment(50.0, 0);
    assert_eq!(s.widthForSegment(0), 50.0);
    assert_eq!(s.intrinsicContentSize(), NSSize::new(50.0 + w("Two") + 1.0, 24.0));
    s.setTag_forSegment(7, 1);
    assert!(s.selectSegmentWithTag(7));
    assert_eq!((s.selectedSegment(), s.indexOfSelectedItem()), (1, 1));
    assert!(!s.selectSegmentWithTag(99));
    s.setEnabled_forSegment(false, 0);
    assert!(!s.isEnabledForSegment(0));
    s.setToolTip_forSegment(Some(&NSString::from_str("tip")), 1);
    assert_eq!(s.toolTipForSegment(1).map(|t| t.to_string()), Some("tip".into()));
    // Styles don't change sizes.
    for style in [NSSegmentStyle::Rounded, NSSegmentStyle::Separated, NSSegmentStyle::SmallSquare] {
        let s = segmented(&["One", "Two", "Three"], mtm);
        s.setSegmentStyle(style);
        assert_eq!(s.intrinsicContentSize(), NSSize::new(natural, 24.0), "{style:?}");
    }
    // Control sizes change the height and the room round the label.
    for (size, pad, height) in
        [(NSControlSize::Small, 18.0, 20.0), (NSControlSize::Mini, 14.0, 16.0), (NSControlSize::Large, 24.0, 28.0)]
    {
        let s = segmented(&["One"], mtm);
        s.setControlSize(size);
        assert_eq!(
            s.intrinsicContentSize(),
            NSSize::new(text_size("One", &font).width.ceil() + pad, height),
            "{size:?}"
        );
    }
    let s = segmented(&[""], mtm);
    assert_eq!(s.intrinsicContentSize(), NSSize::new(24.0, 24.0));
}

// Steppers, sliders and switches

fn steppers_sliders_switches(mtm: MainThreadMarker) {
    let s = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 19.0, 28.0));
    assert_eq!((s.minValue(), s.maxValue(), s.increment(), s.doubleValue()), (0.0, 59.0, 1.0, 0.0));
    assert!(s.valueWraps() && s.autorepeat() && s.isContinuous());
    assert!(is_kind(&s.cell().expect("a cell"), NSStepperCell::class()));
    for size in [NSControlSize::Regular, NSControlSize::Small, NSControlSize::Mini, NSControlSize::Large] {
        s.setControlSize(size);
        assert_eq!(s.intrinsicContentSize(), NSSize::new(20.0, 26.0));
    }
    s.setDoubleValue(200.0);
    assert_eq!(s.doubleValue(), 59.0);
    s.setDoubleValue(-3.0);
    assert_eq!(s.doubleValue(), 0.0);
    s.setMaxValue(10.0);
    s.setDoubleValue(50.0);
    assert_eq!(s.doubleValue(), 10.0);

    let sl = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 200.0, 21.0));
    assert_eq!((sl.minValue(), sl.maxValue(), sl.doubleValue(), sl.altIncrementValue()), (0.0, 1.0, 0.0, 0.0));
    assert_eq!(sl.numberOfTickMarks(), 0);
    assert_eq!(sl.tickMarkPosition(), NSTickMarkPosition::Below);
    assert!(!sl.allowsTickMarkValuesOnly() && !sl.isVertical() && sl.isContinuous());
    assert_eq!(sl.sliderType(), NSSliderType::Linear);
    assert!(NSSliderCell::prefersTrackingUntilMouseUp(mtm));
    let knobs = [
        (NSControlSize::Regular, 16.0, 20.0),
        (NSControlSize::Small, 14.0, 18.0),
        (NSControlSize::Mini, 12.0, 16.0),
        (NSControlSize::Large, 20.0, 24.0),
    ];
    for (size, height, knob) in knobs {
        sl.setControlSize(size);
        assert_eq!(sl.intrinsicContentSize(), NSSize::new(-1.0, height), "{size:?}");
        assert_eq!(sl.knobThickness(), knob, "{size:?}");
    }
    sl.setControlSize(NSControlSize::Regular);
    sl.setDoubleValue(200.0);
    assert_eq!(sl.doubleValue(), 1.0);
    sl.setDoubleValue(-3.0);
    assert_eq!(sl.doubleValue(), 0.0);
    // Tick marks spread over the whole width, 2 points wide.
    sl.setNumberOfTickMarks(5);
    assert_eq!((0..5).map(|i| sl.tickMarkValueAtIndex(i)).collect::<Vec<_>>(), [0.0, 0.25, 0.5, 0.75, 1.0]);
    for i in 0..5 {
        let r = sl.rectOfTickMarkAtIndex(i);
        assert_eq!((r.origin.x, r.size.width, r.size.height), (-1.0 + 50.0 * i as f64, 2.0, 2.0), "tick {i}");
    }
    assert_eq!(sl.closestTickMarkValueToValue(0.3), 0.25);
    assert_eq!(sl.closestTickMarkValueToValue(0.4), 0.5);
    assert_eq!(sl.indexOfTickMarkAtPoint(NSPoint::new(100.0, 10.0)), 2);
    sl.setAllowsTickMarkValuesOnly(true);
    sl.setDoubleValue(0.3);
    assert_eq!(sl.doubleValue(), 0.25);
    // SAFETY: no target or action.
    let v = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(5.0, 0.0, 10.0, None, None, mtm) };
    assert_eq!((v.doubleValue(), v.minValue(), v.maxValue()), (5.0, 0.0, 10.0));
    assert_eq!(v.frame(), rect(0.0, 0.0, 100.0, 16.0));
    // Taller than wide: vertical.
    let tall = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 21.0, 200.0));
    assert!(tall.isVertical());
    assert_eq!(tall.intrinsicContentSize(), NSSize::new(16.0, -1.0));

    let sw = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 50.0, 30.0));
    assert_eq!(sw.state(), 0);
    assert!(sw.cell().is_none());
    assert!(sw.isEnabled() && sw.acceptsFirstResponder());
    assert_eq!(sw.intrinsicContentSize(), NSSize::new(54.0, 24.0));
    sw.setState(1);
    assert_eq!((sw.state(), sw.intValue()), (1, 1));
    sw.setControlSize(NSControlSize::Small);
    assert_eq!(sw.intrinsicContentSize(), NSSize::new(54.0, 24.0));
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("cell_defaults", cell_defaults),
        ("text_cells", text_cells),
        ("cell_values", cell_values),
        ("cell_state", cell_state),
        ("control_defaults", control_defaults),
        ("control_forwards_to_its_cell", control_forwards_to_its_cell),
        ("actions", actions),
        ("button_defaults", button_defaults),
        ("button_values", button_values),
        ("button_types", button_types),
        ("push_buttons", push_buttons),
        ("check_boxes", check_boxes),
        ("buttons_of_every_bezel", buttons_of_every_bezel),
        ("text_field_defaults", text_field_defaults),
        ("labels", labels),
        ("wrapping_labels", wrapping_labels),
        ("text_fields", text_fields),
        ("search_fields", search_fields),
        ("notification_names", notification_names),
        ("boxes", boxes),
        ("custom_and_separator_boxes", custom_and_separator_boxes),
        ("box_sizing", box_sizing),
        ("progress_indicators", progress_indicators),
        ("segmented_controls", segmented_controls),
        ("steppers_sliders_switches", steppers_sliders_switches),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
