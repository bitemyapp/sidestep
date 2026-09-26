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
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{
    NSAttributedString, NSCopying, NSDictionary, NSNotification, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

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

/// How `number` reads in the current locale, as `NSNumber` describes it:
/// what cells show for numbers, whatever the locale.
fn localized(number: &NSNumber) -> String {
    let locale = objc2_foundation::NSLocale::currentLocale();
    // SAFETY: the locale is an NSLocale.
    unsafe { number.descriptionWithLocale(Some(&locale)) }.to_string()
}

/// A mouse-down at `(x, y)` in the window, for hit tests.
fn press(x: f64, y: f64) -> Retained<NSEvent> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown,
        NSPoint::new(x, y),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        1,
        1.0,
    )
    .expect("a mouse event")
}

/// A cell's `sendActionOn:` mask, read by setting it back.
fn mask_of(c: &NSCell) -> u64 {
    let mask = c.sendActionOn(NSEventMask::LeftMouseUp) as u64;
    c.sendActionOn(NSEventMask(mask));
    mask
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
    assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_f64(3.75)));
    assert_eq!(v.intValue(), 3);
    v.setFloatValue(1.5);
    assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_f64(1.5)));
    assert_eq!(v.intValue(), 1);
    v.setIntegerValue(-9);
    assert_eq!(v.stringValue().to_string(), "-9");

    // Numbers read as NSNumber describes them in the current locale (in
    // English, "1,234.5"); floats are widened to doubles first.
    for i in [1234, -1_234_567, 0, 999, 1000] {
        v.setIntValue(i);
        assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_i32(i)), "{i}");
    }
    v.setIntegerValue(1_234_567);
    assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_isize(1_234_567)));
    let doubles = [0.1, 2.0, 1.0 / 3.0, -0.5, 1234.5, 1e16, 12_345_678.9, 1e20, 1e-5, f64::NAN, f64::INFINITY];
    for d in doubles {
        v.setDoubleValue(d);
        assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_f64(d)), "{d}");
    }
    for f in [0.1f32, 0.3, 1e20] {
        v.setFloatValue(f);
        assert_eq!(v.stringValue().to_string(), localized(&NSNumber::new_f64(f64::from(f))), "{f}");
    }
    // Ints of large doubles: through a 64-bit integer, which saturates;
    // then its low 32 bits.
    for (d, int, integer) in
        [(1e16, 1_874_919_424, 10_000_000_000_000_000), (1e20, -1, isize::MAX), (f64::NEG_INFINITY, 0, isize::MIN)]
    {
        v.setDoubleValue(d);
        assert_eq!((v.intValue(), v.integerValue()), (int, integer), "{d}");
    }
    v.setFloatValue(1e20);
    assert_eq!(v.intValue(), -1);

    // Only text cells take numbers; a string makes any cell a text cell.
    let n = NSCell::new(mtm);
    n.setDoubleValue(3.5);
    n.setIntValue(3);
    assert_eq!(n.r#type(), NSCellType::NullCellType);
    assert!(n.font().is_none() && n.objectValue().is_none());
    assert_eq!((n.stringValue().to_string(), n.doubleValue()), (String::new(), 0.0));
    // Made a text cell, an empty cell reads "Cell".
    n.setType(NSCellType::TextCellType);
    assert_eq!(n.stringValue().to_string(), "Cell");
    let a = NSActionCell::new(mtm);
    a.setIntValue(4);
    assert_eq!((a.r#type(), a.intValue()), (NSCellType::NullCellType, 0));
    // An object is kept, whatever the cell's type.
    let o = NSCell::new(mtm);
    unsafe { o.setObjectValue(Some(&NSString::from_str("s"))) };
    assert_eq!((o.r#type(), o.stringValue().to_string()), (NSCellType::NullCellType, "s".into()));
    let t = NSCell::new(mtm);
    t.setStringValue(&NSString::from_str("s"));
    assert_eq!(t.r#type(), NSCellType::TextCellType);
    let t = NSCell::new(mtm);
    t.setAttributedStringValue(&NSAttributedString::from_nsstring(&NSString::from_str("a")));
    assert_eq!(t.r#type(), NSCellType::TextCellType);

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

    // Continuous is the periodic bit of the mask, one setting.
    let (up, down, drag, periodic) = (
        NSEventMask::LeftMouseUp.0,
        NSEventMask::LeftMouseDown.0,
        NSEventMask::LeftMouseDragged.0,
        NSEventMask::Periodic.0,
    );
    let a = NSActionCell::new(mtm);
    assert_eq!((mask_of(&a), a.isContinuous()), (up, false));
    a.setContinuous(true);
    assert_eq!(mask_of(&a), periodic | up);
    a.setContinuous(false);
    assert_eq!(mask_of(&a), up);
    a.sendActionOn(NSEventMask(periodic | up));
    assert!(a.isContinuous());
    a.sendActionOn(NSEventMask(up));
    assert!(!a.isContinuous());
    a.sendActionOn(NSEventMask(drag));
    assert!(!a.isContinuous());
    a.sendActionOn(NSEventMask(down | periodic));
    assert!(a.isContinuous());
    let plain = NSCell::new(mtm);
    plain.setContinuous(true);
    assert_eq!(mask_of(&plain), periodic | up);
    let b = NSButtonCell::new(mtm);
    assert_eq!((mask_of(&b), b.isContinuous()), (up, false));
    b.setContinuous(true);
    assert_eq!(mask_of(&b), periodic | up);
    // A slider acts on the press, the drag and the release, and is
    // continuous by the drag bit.
    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    let sc = slider.cell().expect("a cell");
    assert_eq!((mask_of(&sc), sc.isContinuous()), (down | up | drag, true));
    sc.setContinuous(false);
    assert_eq!(mask_of(&sc), up);
    sc.setContinuous(true);
    assert_eq!(mask_of(&sc), down | up | drag);
    slider.sendActionOn(NSEventMask::LeftMouseUp);
    assert!(!sc.isContinuous() && !slider.isContinuous());
    slider.sendActionOn(NSEventMask(periodic | up));
    assert!(!sc.isContinuous());
    // A stepper acts on the press, and periodically.
    let stepper = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 19.0, 28.0));
    let stc = stepper.cell().expect("a cell");
    assert_eq!((mask_of(&stc), stc.isContinuous()), (down | periodic, true));
    stc.setContinuous(false);
    assert_eq!(mask_of(&stc), down);
    let label = NSTextField::labelWithString(&NSString::from_str("x"), mtm);
    assert_eq!(mask_of(&label.cell().expect("a cell")), up);
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
    assert_eq!(c.titleRectForBounds(bounds), rect(half_up((100.0 - tw) / 2.0), half_up((40.0 - th) / 2.0), tw, th));
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
    assert_eq!(c.titleRectForBounds(bounds), rect(22.0, half_up((30.0 - t.height) / 2.0), t.width.ceil(), t.height));
    // A check box's title is 20-point text: taller than the box.
    b.setFont(Some(&system(20.0)));
    let t20 = text_size("A", &system(20.0));
    assert_eq!(b.intrinsicContentSize(), NSSize::new(22.0 + t20.width.ceil(), t20.height));
}

/// Half-way positions round up, as AppKit places titles and boxes.
fn half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

fn check_box_rects(mtm: MainThreadMarker) {
    // The box and the title each centered across, the title after the box
    // and its gap; a mini box sits a point lower than centered.
    let sizes = [
        (NSControlSize::Regular, 16.0, 6.0, 13.0, 16.0),
        (NSControlSize::Small, 14.0, 4.0, 11.0, 14.0),
        (NSControlSize::Mini, 12.0, 4.0, 9.0, 11.0),
        (NSControlSize::Large, 18.0, 6.0, 13.0, 18.0),
    ];
    for radio in [false, true] {
        for (size, side, gap, font, center) in sizes {
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
            let c = b.cell().expect("a cell");
            let t = text_size(title, &system(font));
            for h in [40.0, 17.0] {
                let bounds = rect(0.0, 0.0, 120.0, h);
                let what = format!("radio {radio} {size:?} height {h}");
                assert_eq!(c.imageRectForBounds(bounds), rect(0.0, half_up((h - center) / 2.0), side, side), "{what}");
                let title_rect = rect(side + gap, half_up((h - t.height) / 2.0), t.width.ceil(), t.height);
                assert_eq!(c.titleRectForBounds(bounds), title_rect, "{what}");
                // The drawing rect is the bounds, at least the box tall.
                assert_eq!(c.drawingRectForBounds(bounds), rect(0.0, 0.0, 120.0, h.max(side)), "{what}");
            }
        }
    }
}

/// What a bezel measures, at one control size, as formulas over the
/// title's size: the intrinsic size, how much more `cellSize` is, and the
/// title and drawing rects in a given bounds.
struct Bezel {
    intrinsic: fn(tw: f64, th: f64) -> (f64, f64),
    empty: (f64, f64),
    frame_extra: (f64, f64),
    title: fn(w: f64, h: f64, tw: f64, th: f64) -> NSRect,
    drawing: fn(w: f64, h: f64, titled: bool) -> NSRect,
}

fn buttons_of_every_bezel(mtm: MainThreadMarker) {
    use NSBezelStyle as B;
    // Push, per size: its height, which the bezel is 12, 10, 8 or 14 in
    // from each end; a titled mini bezel sits a point lower.
    macro_rules! push {
        ($h:expr, $lower:expr) => {
            Bezel {
                intrinsic: |tw, _| (tw + $h, $h),
                empty: (10.0 + $h, $h),
                frame_extra: (0.0, 0.0),
                title: centered_fn,
                drawing: |w, h, titled| {
                    let pad = $h / 2.0;
                    let lower = if titled { $lower } else { 0.0 };
                    rect(pad, ((h - $h) / 2.0).floor() + lower, w - 2.0 * pad, $h)
                },
            }
        };
    }
    fn centered_fn(w: f64, h: f64, tw: f64, th: f64) -> NSRect {
        rect(half_up((w - tw) / 2.0), half_up((h - th) / 2.0), tw, th)
    }
    // Flexible push (and glass): a push button's width, its title's height
    // plus an inset of 4, 3, 1 or 6 top and bottom.
    macro_rules! flexible {
        ($pad:expr, $d:expr, $empty:expr) => {
            Bezel {
                intrinsic: |tw, th| (tw + 2.0 * $pad, th + 2.0 * $d),
                empty: (10.0 + 2.0 * $pad, $empty),
                frame_extra: (0.0, 0.0),
                title: centered_fn,
                drawing: |w, h, _| rect($pad, $d, w - 2.0 * $pad, h - 2.0 * $d),
            }
        };
    }
    macro_rules! circular {
        ($d:expr, $empty:expr) => {
            Bezel {
                intrinsic: |tw, th| (tw + 2.0 * $d, th + 2.0 * $d),
                empty: ($empty, $empty),
                frame_extra: (0.0, 0.0),
                title: centered_fn,
                drawing: |w, h, _| rect($d, $d, w - 2.0 * $d, h - 2.0 * $d),
            }
        };
    }
    macro_rules! square {
        ($side:expr) => {
            Bezel {
                intrinsic: |_, _| ($side, $side),
                empty: ($side, $side),
                frame_extra: (0.0, 0.0),
                title: zero_fn,
                drawing: none_fn,
            }
        };
    }
    fn zero_fn(_: f64, _: f64, _: f64, _: f64) -> NSRect {
        NSRect::ZERO
    }
    fn none_fn(_: f64, _: f64, _: bool) -> NSRect {
        NSRect::ZERO
    }
    let disclosure = Bezel {
        intrinsic: |_, _| (13.0, 13.0),
        empty: (13.0, 13.0),
        frame_extra: (0.0, 0.0),
        title: |w, h, _, th| rect((w - 13.0) / 2.0, (h - th) / 2.0, 13.0, th),
        drawing: |w, h, _| rect((w - 13.0) / 2.0, (h - 13.0) / 2.0, 13.0, 13.0),
    };
    let small_square = Bezel {
        intrinsic: |tw, th| (tw + 6.0, th + 4.0),
        empty: (2.0, 4.0),
        frame_extra: (0.0, 2.0),
        title: |w, h, _, th| rect(1.0, (h - th) / 2.0, w - 2.0, th),
        drawing: |w, h, _| rect(1.0, 3.0, w - 2.0, h - 6.0),
    };
    let shadowless = Bezel {
        intrinsic: |tw, th| (tw + 10.0, th + 6.0),
        empty: (6.0, 6.0),
        frame_extra: (0.0, 0.0),
        title: |w, h, _, th| rect(3.0, (h - th) / 2.0, w - 6.0, th),
        drawing: |w, h, _| rect(3.0, 3.0, w - 6.0, h - 6.0),
    };
    macro_rules! textured {
        ($pad:expr, $height:expr) => {
            Bezel {
                intrinsic: |tw, _| (tw + $pad, $height),
                empty: ($pad - 4.0, $height),
                frame_extra: (4.0, 5.0),
                title: |w, h, _, th| rect($pad / 2.0, (h - th) / 2.0, w - $pad, th),
                drawing: |w, h, _| rect($pad / 2.0, 2.0, w - $pad, h - 4.0),
            }
        };
    }
    macro_rules! toolbar {
        ($pad:expr, $height:expr) => {
            Bezel {
                intrinsic: |tw, _| (tw + $pad, $height),
                empty: ($pad - 4.0, $height),
                frame_extra: (2.0, 3.0),
                title: |w, h, _, th| rect($pad / 2.0 - 1.0, (h - th) / 2.0 - 1.5, w - $pad + 2.0, th),
                drawing: |w, h, _| rect(3.0, (h - $height) / 2.0 - 0.5, w - 6.0, $height),
            }
        };
    }
    // Badges measure their title at 11 points, whatever the control size.
    let badge = Bezel {
        intrinsic: |tw, _| (tw + 10.0, 18.0),
        empty: (18.0, 14.0),
        frame_extra: (0.0, 0.0),
        title: |w, h, tw, th| rect(half_up((w - tw - 2.0) / 2.0), half_up((h - th) / 2.0), tw + 2.0, th),
        drawing: |w, h, _| rect(4.0, 2.0, w - 8.0, h - 4.0),
    };
    let sizes = [
        (NSControlSize::Regular, 13.0),
        (NSControlSize::Small, 11.0),
        (NSControlSize::Mini, 9.0),
        (NSControlSize::Large, 13.0),
    ];
    for (i, (size, font)) in sizes.into_iter().enumerate() {
        let pushes = [push!(24.0, 0.0), push!(20.0, 0.0), push!(16.0, 1.0), push!(28.0, 0.0)];
        let flexibles = [
            flexible!(12.0, 4.0, 18.0),
            flexible!(10.0, 3.0, 16.0),
            flexible!(8.0, 1.0, 12.0),
            flexible!(14.0, 6.0, 22.0),
        ];
        let circulars = [circular!(4.0, 18.0), circular!(3.0, 16.0), circular!(1.0, 12.0), circular!(6.0, 22.0)];
        let helps = [square!(24.0), square!(20.0), square!(16.0), square!(28.0)];
        let textureds = [textured!(8.0, 20.0), textured!(6.0, 14.0), textured!(6.0, 11.0), textured!(8.0, 20.0)];
        let toolbars = [toolbar!(14.0, 20.0), toolbar!(12.0, 16.0), toolbar!(10.0, 13.0), toolbar!(14.0, 20.0)];
        #[allow(deprecated)]
        let cases: [(NSBezelStyle, &Bezel); 15] = [
            (B::Automatic, &pushes[i]),
            (B::Push, &pushes[i]),
            (B::AccessoryBar, &pushes[i]),
            (B::AccessoryBarAction, &pushes[i]),
            (B::FlexiblePush, &flexibles[i]),
            (B::Glass, &flexibles[i]),
            (B::Circular, &circulars[i]),
            (B::HelpButton, &helps[i]),
            (B::PushDisclosure, &helps[i]),
            (B::Disclosure, &disclosure),
            (B::SmallSquare, &small_square),
            (B::ShadowlessSquare, &shadowless),
            (B::TexturedSquare, &textureds[i]),
            (B::Toolbar, &toolbars[i]),
            (B::Badge, &badge),
        ];
        for (bezel, m) in cases {
            let what = format!("{bezel:?} {size:?}");
            let b = push("Cancel", mtm);
            b.setBezelStyle(bezel);
            b.setControlSize(size);
            let c = b.cell().expect("a cell");
            let t = text_size("Cancel", &system(if bezel == B::Badge { 11.0 } else { font }));
            let (tw, th) = (t.width.ceil(), t.height);
            let (w, h) = (m.intrinsic)(tw, th);
            assert_eq!(b.intrinsicContentSize(), NSSize::new(w, h), "{what}");
            let (dw, dh) = m.frame_extra;
            assert_eq!(c.cellSize(), NSSize::new(w + dw, h + dh), "{what}");
            for (bw, bh) in [(120.0, 40.0), (w, h)] {
                let bounds = rect(0.0, 0.0, bw, bh);
                assert_eq!(c.titleRectForBounds(bounds), (m.title)(bw, bh, tw, th), "title {what} in {bw}x{bh}");
                assert_eq!(c.drawingRectForBounds(bounds), (m.drawing)(bw, bh, true), "drawing {what} in {bw}x{bh}");
                assert_eq!(c.imageRectForBounds(bounds), NSRect::ZERO, "image {what}");
            }
            b.setTitle(&NSString::from_str(""));
            assert_eq!(b.intrinsicContentSize(), NSSize::new(m.empty.0, m.empty.1), "empty {what}");
            let bounds = rect(0.0, 0.0, 120.0, 40.0);
            assert_eq!(c.titleRectForBounds(bounds), NSRect::ZERO, "empty {what}");
            assert_eq!(c.drawingRectForBounds(bounds), (m.drawing)(120.0, 40.0, false), "empty drawing {what}");
        }
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

    assert!(!c.sendsActionOnEndEditing());

    let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    let sc = secure.cell().expect("a cell");
    assert!(!sc.sendsActionOnEndEditing());
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
    assert!(!c.wraps() && !c.isScrollable() && !c.sendsActionOnEndEditing());
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
    assert!(!c.sendsActionOnEndEditing());
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
    // Leaving the field sends its action, however editing ends.
    assert!(c.sendsActionOnEndEditing());
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
    assert!(is_kind(&c, NSSearchFieldCell::class()) && !c.sendsActionOnEndEditing());
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

// What text fields tell their delegates and the notification center.

thread_local!(static NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn take_notes() -> Vec<String> {
    NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

/// Note what `who` was told: the notification's name, whether it came
/// from a text field, its user info's keys and the movement it holds.
fn note(who: &str, n: &NSNotification) {
    let from_field = n.object().is_some_and(|o| is_kind(&o, NSTextField::class()));
    let (mut keys, mut movement) = (Vec::new(), None);
    if let Some(info) = n.userInfo() {
        for key in info.allKeys() {
            // SAFETY: the keys here are strings.
            let key: Retained<NSString> = unsafe { Retained::cast_unchecked(key) };
            keys.push(key.to_string());
        }
        let key = NSString::from_str("NSTextMovement");
        movement = info.objectForKey(&key).map(|m| {
            // SAFETY: the movement is a number.
            let m: isize = unsafe { msg_send![&*m, integerValue] };
            m
        });
    }
    keys.sort();
    let name = n.name().to_string();
    NOTES.with(|v| v.borrow_mut().push(format!("{who} {name} field={from_field} {keys:?} {movement:?}")));
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceFieldDelegate"]
    #[ivars = std::cell::Cell<bool>]
    struct FieldDelegate;

    impl FieldDelegate {
        #[unsafe(method(controlTextDidBeginEditing:))]
        fn began(&self, n: &NSNotification) {
            note("delegate", n);
        }

        #[unsafe(method(controlTextDidChange:))]
        fn changed(&self, n: &NSNotification) {
            note("delegate", n);
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn ended(&self, n: &NSNotification) {
            note("delegate", n);
            // Some delegates let go of the field when editing ends.
            if self.ivars().get()
                && let Some(field) = n.object()
            {
                // SAFETY: the notification comes from a text field, which
                // takes nil as its delegate.
                let _: () = unsafe { msg_send![&*field, setDelegate: None::<&AnyObject>] };
            }
        }
    }

    unsafe impl NSObjectProtocol for FieldDelegate {}
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTextObserver"]
    struct TextObserver;

    impl TextObserver {
        #[unsafe(method(noted:))]
        fn noted(&self, n: &NSNotification) {
            note("observer", n);
        }
    }

    unsafe impl NSObjectProtocol for TextObserver {}
);

fn field_delegate(mtm: MainThreadMarker, lets_go: bool) -> Retained<FieldDelegate> {
    let this = FieldDelegate::alloc(mtm).set_ivars(std::cell::Cell::new(lets_go));
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![super(this), init] }
}

fn text_notifications(mtm: MainThreadMarker) {
    let t = target(mtm);
    let field = NSTextField::textFieldWithString(&NSString::from_str("x"), mtm);
    // SAFETY: the target outlives the field's use of it.
    unsafe {
        field.setTarget(Some(&t));
        field.setAction(Some(sel!(first:)));
    }
    let delegate = field_delegate(mtm, false);
    // SAFETY: the delegate outlives the field's use of it.
    let _: () = unsafe { msg_send![&*field, setDelegate: Some(&*delegate as &AnyObject)] };
    // SAFETY: NSObject's initializer.
    let observer: Retained<TextObserver> = unsafe { msg_send![TextObserver::alloc(mtm), init] };
    let center = objc2_foundation::NSNotificationCenter::defaultCenter();
    // SAFETY: the names are constant strings.
    let names = unsafe {
        [
            NSControlTextDidBeginEditingNotification,
            NSControlTextDidChangeNotification,
            NSControlTextDidEndEditingNotification,
        ]
    };
    let field_object: &AnyObject = &field;
    for name in names {
        // SAFETY: the observer answers noted:, and is removed below.
        unsafe { center.addObserver_selector_name_object(&observer, sel!(noted:), Some(name), Some(field_object)) };
    }
    // The field editor's notifications, as it would post them. AppKit
    // wants its field editor to be text; Sidestep's is to come.
    #[cfg(target_vendor = "apple")]
    // SAFETY: +new makes a text view, on the main thread.
    let editor: Retained<AnyObject> = unsafe { msg_send![objc2::class!(NSTextView), new] };
    #[cfg(not(target_vendor = "apple"))]
    let editor: Retained<AnyObject> = Retained::into_super(NSObject::new());
    let text_note = |name: &str, movement: Option<isize>| -> Retained<NSNotification> {
        let info = movement.map(|m| {
            let key = NSString::from_str("NSTextMovement");
            NSDictionary::from_slices(&[&*key], &[&*NSNumber::new_isize(m) as &AnyObject])
        });
        // SAFETY: the class method takes a name, an object and a
        // dictionary or nil.
        unsafe {
            msg_send![
                NSNotification::class(),
                notificationWithName: &*NSString::from_str(name),
                object: Some(&*editor as &AnyObject),
                userInfo: info.as_deref()
            ]
        }
    };
    take_notes();
    take_actions();
    // One notification from the field, with the field editor in its user
    // info, to the delegate first and then to observers.
    // SAFETY: the text notifications take a notification.
    unsafe {
        let _: () = msg_send![&*field, textDidBeginEditing: &*text_note("NSTextDidBeginEditingNotification", None)];
        let _: () = msg_send![&*field, textDidChange: &*text_note("NSTextDidChangeNotification", None)];
    }
    let editor_only = r#"["NSFieldEditor"] None"#;
    assert_eq!(
        take_notes(),
        [
            format!("delegate NSControlTextDidBeginEditingNotification field=true {editor_only}"),
            format!("observer NSControlTextDidBeginEditingNotification field=true {editor_only}"),
            format!("delegate NSControlTextDidChangeNotification field=true {editor_only}"),
            format!("observer NSControlTextDidChangeNotification field=true {editor_only}"),
        ]
    );
    assert!(take_actions().is_empty());
    // The end also says how editing ended; a textFieldWithString: field
    // sends its action however it ended (here, a tab).
    // SAFETY: as above.
    unsafe { msg_send![&*field, textDidEndEditing: &*text_note("NSTextDidEndEditingNotification", Some(0x11))] }
    let ended = r#"["NSFieldEditor", "NSTextMovement"] Some(17)"#;
    assert_eq!(
        take_notes(),
        [
            format!("delegate NSControlTextDidEndEditingNotification field=true {ended}"),
            format!("observer NSControlTextDidEndEditingNotification field=true {ended}"),
        ]
    );
    assert_eq!(take_actions().len(), 1);
    // Other fields send it only when Return ends editing.
    field.cell().expect("a cell").setSendsActionOnEndEditing(false);
    unsafe { msg_send![&*field, textDidEndEditing: &*text_note("NSTextDidEndEditingNotification", Some(0x11))] }
    assert!(take_actions().is_empty());
    unsafe { msg_send![&*field, textDidEndEditing: &*text_note("NSTextDidEndEditingNotification", Some(0x10))] }
    assert_eq!(take_actions().len(), 1);
    // A delegate may let go of the field while it's told.
    let leaving = field_delegate(mtm, true);
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*field, setDelegate: Some(&*leaving as &AnyObject)] };
    take_notes();
    unsafe { msg_send![&*field, textDidEndEditing: &*text_note("NSTextDidEndEditingNotification", Some(0x10))] }
    assert!(field.delegate().is_none());
    assert_eq!(take_notes().len(), 2);
    // SAFETY: the observer was added above.
    unsafe { center.removeObserver(&observer) };
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
    // Steppers aren't flipped.
    assert!(!s.isFlipped());
    // Moving a limit past the value brings the value along.
    s.setMaxValue(59.0);
    s.setDoubleValue(30.0);
    s.setMaxValue(10.0);
    assert_eq!((s.doubleValue(), s.stringValue().to_string()), (10.0, "10".to_string()));
    s.setMaxValue(59.0);
    s.setDoubleValue(5.0);
    s.setMinValue(8.0);
    assert_eq!(s.doubleValue(), 8.0);
    s.setMinValue(0.0);
    // The arrow keys' methods step, wrapping; performClick: steps the way
    // the last step went, down at first.
    let step = |sel: objc2::runtime::Sel| {
        // SAFETY: the move methods take a sender.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*s, sel, (None::<&AnyObject>,)) }
    };
    let fresh = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 19.0, 28.0));
    fresh.setDoubleValue(5.0);
    unsafe { fresh.performClick(None) };
    assert_eq!(fresh.doubleValue(), 4.0);
    s.setDoubleValue(5.0);
    step(sel!(moveUp:));
    assert_eq!(s.doubleValue(), 6.0);
    step(sel!(moveDown:));
    assert_eq!(s.doubleValue(), 5.0);
    s.setDoubleValue(0.0);
    step(sel!(moveDown:));
    assert_eq!(s.doubleValue(), 59.0);
    step(sel!(moveUp:));
    assert_eq!(s.doubleValue(), 0.0);
    unsafe { s.performClick(None) };
    assert_eq!(s.doubleValue(), 1.0);
    step(sel!(moveDown:));
    s.setDoubleValue(0.0);
    unsafe { s.performClick(None) };
    assert_eq!(s.doubleValue(), 59.0);
    s.setDoubleValue(5.0);
    unsafe { s.performClick(None) };
    assert_eq!(s.doubleValue(), 4.0);
    s.setValueWraps(false);
    s.setDoubleValue(0.0);
    unsafe { s.performClick(None) };
    assert_eq!(s.doubleValue(), 0.0);
    s.setDoubleValue(59.0);
    step(sel!(moveUp:));
    assert_eq!(s.doubleValue(), 59.0);
    s.setValueWraps(true);
    s.setIncrement(2.5);
    s.setDoubleValue(1.0);
    step(sel!(moveDown:));
    assert_eq!(s.doubleValue(), 59.0);
    s.setEnabled(false);
    s.setDoubleValue(3.0);
    unsafe { s.performClick(None) };
    assert_eq!(s.doubleValue(), 3.0);

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
    // A point finds a tick within a point of its rect (the far edges out),
    // else none.
    let not_found = isize::MAX;
    let ticks = [
        ((100.0, 10.0), 2),
        ((50.0, 10.0), 1),
        ((51.0, 10.0), 1),
        ((48.5, 10.0), 1),
        ((52.0, 10.0), not_found),
        ((47.0, 10.0), not_found),
        ((30.0, 10.0), not_found),
        ((49.0, 5.0), not_found),
        ((49.0, 20.0), not_found),
        ((-1.0, 10.0), 0),
        ((201.0, 10.0), 4),
    ];
    for ((x, y), index) in ticks {
        assert_eq!(sl.indexOfTickMarkAtPoint(NSPoint::new(x, y)), index, "({x}, {y})");
    }
    assert_eq!(sl.rectOfTickMarkAtIndex(1), rect(49.0, 10.0, 2.0, 2.0));
    // A lone tick is in the middle, worth the middle value.
    sl.setNumberOfTickMarks(1);
    assert_eq!((sl.tickMarkValueAtIndex(0), sl.rectOfTickMarkAtIndex(0)), (0.5, rect(99.0, 10.0, 2.0, 2.0)));
    sl.setNumberOfTickMarks(0);
    assert_eq!(sl.indexOfTickMarkAtPoint(NSPoint::new(100.0, 10.0)), not_found);
    sl.setNumberOfTickMarks(5);
    sl.setAllowsTickMarkValuesOnly(true);
    sl.setDoubleValue(0.3);
    assert_eq!(sl.doubleValue(), 0.25);
    // Moving a limit past the value brings the value along.
    let sl = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 200.0, 21.0));
    sl.setDoubleValue(0.8);
    sl.setMaxValue(0.5);
    assert_eq!(sl.doubleValue(), 0.5);
    sl.setMaxValue(1.0);
    assert_eq!(sl.doubleValue(), 0.5);
    sl.setMinValue(0.9);
    assert_eq!(sl.doubleValue(), 0.9);
    sl.setMinValue(0.0);
    // The neutral value is kept.
    assert_eq!(sl.neutralValue(), 0.0);
    sl.setNeutralValue(0.3);
    assert_eq!(sl.neutralValue(), 0.3);
    // The track rect: the bounds, moved across by half what the slider's
    // thickness leaves.
    let track = |sl: &NSSlider| -> NSRect {
        // SAFETY: trackRect takes nothing and returns a rect.
        unsafe { msg_send![&*sl.cell().expect("a cell"), trackRect] }
    };
    assert_eq!(track(&sl), rect(0.0, 2.5, 200.0, 21.0));
    let tall = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 200.0, 40.0));
    assert_eq!(track(&tall), rect(0.0, 12.0, 200.0, 40.0));
    tall.setFrameSize(NSSize::new(200.0, 21.0));
    tall.setControlSize(NSControlSize::Small);
    assert_eq!(track(&tall), rect(0.0, 3.5, 200.0, 21.0));
    let upright = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 21.0, 200.0));
    assert_eq!(track(&upright), rect(2.5, 0.0, 21.0, 200.0));
    // The keys' methods: a twentieth of the range, the alternate increment
    // if set, the next tick when only ticks are allowed; the page keys go
    // to the ends.
    let key = |sl: &NSSlider, sel: objc2::runtime::Sel| {
        // SAFETY: the move methods take a sender.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(sl, sel, (None::<&AnyObject>,)) }
    };
    sl.setDoubleValue(0.5);
    let steps = [
        (sel!(moveRight:), 0.55),
        (sel!(moveLeft:), 0.5),
        (sel!(moveUp:), 0.55),
        (sel!(moveDown:), 0.5),
        (sel!(pageUp:), 1.0),
        (sel!(pageDown:), 0.0),
    ];
    for (sel, value) in steps {
        key(&sl, sel);
        assert!((sl.doubleValue() - value).abs() < 1e-12, "{sel:?}: {}", sl.doubleValue());
    }
    sl.setDoubleValue(0.5);
    sl.setAltIncrementValue(0.2);
    key(&sl, sel!(moveRight:));
    assert!((sl.doubleValue() - 0.7).abs() < 1e-12);
    sl.setAltIncrementValue(0.0);
    sl.setNumberOfTickMarks(5);
    sl.setDoubleValue(0.3);
    key(&sl, sel!(moveRight:));
    assert!((sl.doubleValue() - 0.35).abs() < 1e-12);
    sl.setAllowsTickMarkValuesOnly(true);
    sl.setDoubleValue(0.25);
    key(&sl, sel!(moveRight:));
    assert_eq!(sl.doubleValue(), 0.5);
    sl.setAllowsTickMarkValuesOnly(false);
    sl.setNumberOfTickMarks(0);
    sl.setMaxValue(10.0);
    sl.setDoubleValue(5.0);
    key(&sl, sel!(moveRight:));
    assert_eq!(sl.doubleValue(), 5.5);

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
    assert!(sw.isEnabled() && sw.acceptsFirstResponder() && !sw.isFlipped());
    assert_eq!(sw.intrinsicContentSize(), NSSize::new(54.0, 24.0));
    assert_eq!((sw.stringValue().to_string(), sw.intValue()), (String::new(), 0));
    assert!(sw.objectValue().is_none());
    sw.setState(1);
    assert_eq!((sw.state(), sw.intValue()), (1, 1));
    assert_eq!((sw.stringValue().to_string(), sw.doubleValue()), ("1".to_string(), 1.0));
    sw.setState(0);
    assert_eq!(sw.stringValue().to_string(), "0");
    sw.setControlSize(NSControlSize::Small);
    assert_eq!(sw.intrinsicContentSize(), NSSize::new(54.0, 24.0));
    // The value setters set the state from the value's integer, keeping
    // the value; the state setter keeps on, off and mixed.
    sw.setIntValue(1);
    assert_eq!((sw.state(), sw.stringValue().to_string()), (1, "1".to_string()));
    sw.setState(0);
    // SAFETY: an NSNumber is an object value.
    unsafe { sw.setObjectValue(Some(&NSNumber::new_i32(1))) };
    assert_eq!(sw.state(), 1);
    sw.setState(0);
    sw.setStringValue(&NSString::from_str("1"));
    assert_eq!(sw.state(), 1);
    sw.setState(0);
    sw.setDoubleValue(0.6);
    assert_eq!(sw.state(), 0);
    sw.setFloatValue(1.0);
    assert_eq!(sw.state(), 1);
    sw.setIntegerValue(5);
    assert_eq!((sw.state(), sw.intValue()), (1, 5));
    sw.setIntegerValue(-1);
    assert_eq!((sw.state(), sw.intValue()), (-1, -1));
    sw.setState(7);
    assert_eq!(sw.state(), 1);
    sw.setState(-1);
    assert_eq!(sw.state(), -1);
}

// A value whose description changes the cell holding it, as a program's
// object may.

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMeddlingValue"]
    #[ivars = RefCell<Option<Retained<NSCell>>>]
    struct MeddlingValue;

    impl MeddlingValue {
        // Cells copy the objects they're given.
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut objc2::runtime::NSZone) -> Retained<Self> {
            NOTES.with(|n| n.borrow_mut().push("copied".into()));
            objc2::Message::retain(self)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let cell = self.ivars().borrow_mut().take();
            if let Some(cell) = cell {
                cell.setStringValue(&NSString::from_str("changed"));
            }
            NSString::from_str("meddled")
        }
    }

    unsafe impl NSObjectProtocol for MeddlingValue {}
);

fn values_that_change_their_cell(mtm: MainThreadMarker) {
    let cell = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str(""));
    let value = MeddlingValue::alloc(mtm).set_ivars(RefCell::new(Some(cell.clone())));
    // SAFETY: NSObject's initializer.
    let value: Retained<MeddlingValue> = unsafe { msg_send![super(value), init] };
    take_notes();
    // SAFETY: any object that copies is an object value.
    unsafe { cell.setObjectValue(Some(&value)) };
    assert_eq!(take_notes(), ["copied"]);
    // Reading the value asks the object for its description, which changes
    // the cell: the read answers the description, the next the new value.
    assert_eq!(cell.stringValue().to_string(), "meddled");
    assert_eq!(cell.stringValue().to_string(), "changed");
}

// Copying

fn copying_cells(mtm: MainThreadMarker) {
    let t = target(mtm);
    // SAFETY: the target outlives the button's use of it.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("Copy me"), Some(&t), Some(sel!(first:)), mtm)
    };
    b.setState(1);
    let cell = b.cell().expect("a cell");
    cell.setTag(5);
    let copy = cell.copy();
    // The copy is a new cell of the same class, with the settings and the
    // target and action, shown by no view.
    assert!(!std::ptr::eq(&*copy, &*cell));
    assert!(std::ptr::eq(copy.class(), NSButtonCell::class()));
    assert_eq!((copy.title().to_string(), copy.state(), copy.tag()), ("Copy me".to_string(), 1, 5));
    assert_eq!(copy.action(), Some(sel!(first:)));
    assert!(copy.target().is_some_and(|x| std::ptr::eq(&*x, &**t as &AnyObject)));
    assert!(unsafe { copy.controlView() }.is_none());
    assert_eq!(copy.lineBreakMode(), NSLineBreakMode::ByTruncatingTail);
    // SAFETY: the copy is a button cell.
    let bc: Retained<NSButtonCell> = unsafe { Retained::cast_unchecked(copy) };
    assert_eq!(bc.bezelStyle(), NSBezelStyle::Automatic);
    // Changing the copy leaves the original alone.
    bc.setTitle(Some(&NSString::from_str("Other")));
    assert_eq!(b.title().to_string(), "Copy me");

    let text = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("text"));
    text.setBordered(true);
    let tc = text.copy();
    assert!(std::ptr::eq(tc.class(), NSCell::class()) && !std::ptr::eq(&*tc, &*text));
    assert_eq!((tc.stringValue().to_string(), tc.isBordered()), ("text".to_string(), true));
    assert_eq!(tc.r#type(), NSCellType::TextCellType);

    let label = NSTextField::labelWithString(&NSString::from_str("lbl"), mtm);
    let lc = label.cell().expect("a cell").copy();
    assert!(std::ptr::eq(lc.class(), NSTextFieldCell::class()));
    assert_eq!((lc.stringValue().to_string(), lc.isEditable()), ("lbl".to_string(), false));

    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    slider.setMaxValue(10.0);
    slider.setDoubleValue(3.0);
    // SAFETY: the copy is a slider cell.
    let sc: Retained<NSSliderCell> = unsafe { Retained::cast_unchecked(slider.cell().expect("a cell").copy()) };
    assert_eq!((sc.doubleValue(), sc.maxValue()), (3.0, 10.0));

    let seg = NSSegmentedControl::initWithFrame(NSSegmentedControl::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    seg.setSegmentCount(2);
    seg.setLabel_forSegment(&NSString::from_str("A"), 0);
    seg.setSelectedSegment(1);
    // SAFETY: the copy is a segmented cell.
    let gc: Retained<NSSegmentedCell> = unsafe { Retained::cast_unchecked(seg.cell().expect("a cell").copy()) };
    assert_eq!((gc.segmentCount(), gc.selectedSegment()), (2, 1));
    assert_eq!(gc.labelForSegment(0).map(|l| l.to_string()), Some("A".into()));
}

// Sizing to fit

fn sizes_that_fit(mtm: MainThreadMarker) {
    let th = text_size("", &system(13.0)).height;
    // A label lays its text out in the width offered, and is no wider
    // than its text; offered no width, it takes its own.
    let label = NSTextField::labelWithString(&NSString::from_str("Hello"), mtm);
    let natural = label.cell().expect("a cell").cellSize();
    assert_eq!(label.sizeThatFits(NSSize::new(10.0, 10.0)), NSSize::new(10.0, th));
    assert_eq!(label.sizeThatFits(NSSize::new(10.0, 0.0)), NSSize::new(10.0, th));
    for offered in [NSSize::new(1000.0, 1000.0), NSSize::new(0.0, 5.0), NSSize::new(-5.0, 10.0)] {
        assert_eq!(label.sizeThatFits(offered), natural, "{offered:?}");
    }
    // A wrapping label wraps at the width offered.
    let text = "This is a long wrapping label that should wrap onto several lines when narrow";
    let wrapping = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    let fit = wrapping.sizeThatFits(NSSize::new(100.0, 10000.0));
    assert!(fit.width <= 100.0 && fit.height > th && fit.height % th == 0.0, "{fit:?}");
    assert_eq!(fit, wrapping.cell().expect("a cell").cellSizeForBounds(rect(0.0, 0.0, 100.0, 10000.0)));
    assert_eq!(wrapping.sizeThatFits(NSSize::new(0.0, 0.0)), wrapping.cell().expect("a cell").cellSize());
    wrapping.setMaximumNumberOfLines(2);
    assert_eq!(wrapping.sizeThatFits(NSSize::new(100.0, 10000.0)).height, 2.0 * th);
    // A text cell's size in bounds is its text's, no larger than the
    // bounds, laid out in their width.
    let cell = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("Hello world"));
    assert_eq!(cell.cellSizeForBounds(rect(0.0, 0.0, 10.0, 10.0)), NSSize::new(10.0, 10.0));
    let narrow = cell.cellSizeForBounds(rect(0.0, 0.0, 30.0, 100.0));
    assert!(narrow.width <= 30.0 && narrow.height > th && narrow.height % th == 0.0, "{narrow:?}");
    let lc = label.cell().expect("a cell");
    assert_eq!(lc.cellSizeForBounds(rect(0.0, 0.0, 10.0, 10.0)), NSSize::new(10.0, 10.0));
    // A bezeled field takes the width of its bounds (at least its bezel's)
    // and its text's height.
    let field = NSTextField::textFieldWithString(&NSString::from_str("Hello"), mtm);
    let fc = field.cell().expect("a cell");
    assert_eq!(fc.cellSizeForBounds(rect(0.0, 0.0, 10.0, 5.0)), NSSize::new(12.0, th + 8.0));
    assert_eq!(fc.cellSizeForBounds(rect(0.0, 0.0, 100.0, 5.0)), NSSize::new(100.0, th + 8.0));
    assert_eq!(field.sizeThatFits(NSSize::new(10.0, 10.0)), NSSize::new(12.0, th + 8.0));
    assert_eq!(field.sizeThatFits(NSSize::new(1000.0, 1000.0)), fc.cellSize());
    field.setBordered(true);
    assert_eq!(fc.cellSizeForBounds(rect(0.0, 0.0, 10.0, 10.0)), NSSize::new(10.0, 10.0));
    assert_eq!(field.sizeThatFits(NSSize::new(10.0, 10.0)), NSSize::new(10.0, th + 4.0));
    // A control without a cell fits what it's offered.
    let control = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 30.0, 40.0));
    assert_eq!(control.sizeThatFits(NSSize::new(10.0, 10.0)), NSSize::new(10.0, 10.0));
    assert_eq!(control.sizeThatFits(NSSize::new(100.0, 200.0)), NSSize::new(100.0, 200.0));
    // A push button's cell gives up width, not height; the button fits its
    // cell whatever it's offered.
    let b = push("Cancel", mtm);
    let bc = b.cell().expect("a cell");
    let size = bc.cellSize();
    assert_eq!(bc.cellSizeForBounds(rect(0.0, 0.0, 10.0, 10.0)), NSSize::new(10.0, 24.0));
    assert_eq!(bc.cellSizeForBounds(rect(0.0, 0.0, 40.0, 10.0)), NSSize::new(40.0, 24.0));
    assert_eq!(bc.cellSizeForBounds(rect(0.0, 0.0, 1000.0, 1000.0)), size);
    assert_eq!(b.sizeThatFits(NSSize::new(10.0, 10.0)), size);
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("Check"), None, None, mtm) };
    assert_eq!(check.sizeThatFits(NSSize::new(10.0, 10.0)), check.cell().expect("a cell").cellSize());
    // Along an axis a cell has no size of its own, the offer.
    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 200.0, 21.0));
    assert_eq!(slider.sizeThatFits(NSSize::new(10.0, 10.0)), NSSize::new(10.0, 16.0));
}

// Hit testing

fn hit_tests(mtm: MainThreadMarker) {
    const CONTENT: usize = 1;
    const EDITABLE: usize = 2;
    const TRACKABLE: usize = 4;
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 200.0));
    // A click at (10, 10), in and out of the cell's frame, enabled and not.
    let e = press(10.0, 10.0);
    let hits = |c: &NSCell| {
        let at = |r: NSRect| c.hitTestForEvent_inRect_ofView(&e, r, &view).0;
        let (inside, outside) = (rect(0.0, 0.0, 50.0, 50.0), rect(20.0, 20.0, 50.0, 50.0));
        let enabled = (at(inside), at(outside));
        c.setEnabled(false);
        let disabled = (at(inside), at(outside));
        c.setEnabled(true);
        [enabled.0, enabled.1, disabled.0, disabled.1]
    };
    // Cells with no content track anywhere while enabled.
    let anywhere = [CONTENT | TRACKABLE, CONTENT | TRACKABLE, CONTENT, CONTENT];
    assert_eq!(hits(&NSCell::new(mtm)), anywhere);
    assert_eq!(hits(&NSActionCell::new(mtm)), anywhere);
    // Text is content inside the frame, editable text when it's editable
    // or selectable and enabled.
    let text = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("x"));
    assert_eq!(hits(&text), [CONTENT, 0, CONTENT, 0]);
    text.setSelectable(true);
    assert_eq!(hits(&text), [CONTENT | EDITABLE, 0, CONTENT, 0]);
    let label = NSTextField::labelWithString(&NSString::from_str("x"), mtm);
    assert_eq!(hits(&label.cell().expect("a cell")), [CONTENT, 0, CONTENT, 0]);
    let field = NSTextField::textFieldWithString(&NSString::from_str("x"), mtm);
    assert_eq!(hits(&field.cell().expect("a cell")), [CONTENT | EDITABLE, 0, CONTENT, 0]);
    // A search field is editable text wherever the click is.
    let search = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    let all = CONTENT | EDITABLE;
    assert_eq!(hits(&search.cell().expect("a cell")), [all, all, all, all]);
    // A button tracks in its frame, enabled or not.
    assert_eq!(hits(&NSButtonCell::new(mtm)), [CONTENT | TRACKABLE, 0, CONTENT | TRACKABLE, 0]);
    // A check box only over its box and title.
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("Title"), None, None, mtm) };
    check.setFrame(rect(0.0, 0.0, 100.0, 30.0));
    let cc = check.cell().expect("a cell");
    for ((x, y), hit) in
        [((5.0, 15.0), CONTENT | TRACKABLE), ((30.0, 15.0), CONTENT | TRACKABLE), ((90.0, 15.0), 0), ((5.0, 2.0), 0)]
    {
        assert_eq!(cc.hitTestForEvent_inRect_ofView(&press(x, y), check.bounds(), &check).0, hit, "({x}, {y})");
    }
    // Sliders and steppers track in their own frames; segments take their
    // clicks themselves.
    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 100.0, 21.0));
    let stepper = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 19.0, 28.0));
    for control in [&*slider as &NSControl, &stepper] {
        let c = control.cell().expect("a cell");
        assert_eq!(
            c.hitTestForEvent_inRect_ofView(&press(5.0, 10.0), control.bounds(), control).0,
            CONTENT | TRACKABLE
        );
        assert_eq!(c.hitTestForEvent_inRect_ofView(&press(150.0, 10.0), control.bounds(), control).0, 0);
    }
    let seg = NSSegmentedControl::initWithFrame(NSSegmentedControl::alloc(mtm), rect(0.0, 0.0, 100.0, 24.0));
    seg.setSegmentCount(2);
    let sc = seg.cell().expect("a cell");
    assert_eq!(sc.hitTestForEvent_inRect_ofView(&press(50.0, 10.0), seg.bounds(), &seg).0, 0);
}

// Attributed values

fn attributes_of(a: &NSAttributedString) -> Retained<NSDictionary<NSString, AnyObject>> {
    let mut range = objc2_foundation::NSRange::new(0, 0);
    // SAFETY: index 0 is in the string; the range is written.
    unsafe { a.attributesAtIndex_effectiveRange(0, &mut range) }
}

fn paragraph_of(attrs: &NSDictionary<NSString, AnyObject>) -> (NSTextAlignment, NSLineBreakMode) {
    // SAFETY: the key is a constant string.
    let style = attrs.objectForKey(unsafe { NSParagraphStyleAttributeName }).expect("a paragraph style");
    // SAFETY: the value for this key is a paragraph style.
    let style: Retained<NSParagraphStyle> = unsafe { Retained::cast_unchecked(style) };
    (style.alignment(), style.lineBreakMode())
}

fn font_size_of(attrs: &NSDictionary<NSString, AnyObject>) -> f64 {
    // SAFETY: the key is a constant string.
    let font = attrs.objectForKey(unsafe { NSFontAttributeName }).expect("a font");
    // SAFETY: the value for this key is a font.
    let font: Retained<NSFont> = unsafe { Retained::cast_unchecked(font) };
    font.pointSize()
}

/// A plain value read as an attributed string carries the cell's font,
/// text color and paragraph settings, as it's drawn.
fn attributed_values(mtm: MainThreadMarker) {
    // SAFETY: the key is a constant string.
    let color_key = unsafe { NSForegroundColorAttributeName };
    let l = label("Hello", mtm);
    let a = l.attributedStringValue();
    assert_eq!(a.string().to_string(), "Hello");
    let attrs = attributes_of(&a);
    assert_eq!(font_size_of(&attrs), 13.0);
    assert!(attrs.objectForKey(color_key).is_some());
    assert_eq!(paragraph_of(&attrs), (NSTextAlignment::Natural, NSLineBreakMode::ByClipping));
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    l.setAlignment(NSTextAlignment::Center);
    l.setTextColor(Some(&red));
    l.setFont(Some(&system(20.0)));
    let attrs = attributes_of(&l.attributedStringValue());
    assert_eq!(font_size_of(&attrs), 20.0);
    assert!(attrs.objectForKey(color_key).is_some_and(|c| std::ptr::eq(&*c, &**red as &AnyObject)));
    assert_eq!(paragraph_of(&attrs).0, NSTextAlignment::Center);
    // Empty text has no attributes.
    assert_eq!(label("", mtm).attributedStringValue().length(), 0);

    let b = push("OK", mtm);
    let attrs = attributes_of(&b.attributedTitle());
    assert_eq!(font_size_of(&attrs), 13.0);
    assert!(attrs.objectForKey(color_key).is_some());
    assert_eq!(paragraph_of(&attrs), (NSTextAlignment::Center, NSLineBreakMode::ByTruncatingTail));
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("Check"), None, None, mtm) };
    assert_eq!(
        paragraph_of(&attributes_of(&check.attributedTitle())),
        (NSTextAlignment::Natural, NSLineBreakMode::ByTruncatingTail)
    );
    let cell = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("t"));
    let attrs = attributes_of(&cell.attributedStringValue());
    assert_eq!(paragraph_of(&attrs), (NSTextAlignment::Left, NSLineBreakMode::ByWordWrapping));
    assert_eq!(font_size_of(&attrs), 13.0);
    cell.setIntValue(5);
    assert_eq!(font_size_of(&attributes_of(&cell.attributedStringValue())), 13.0);
    let control = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    control.setStringValue(&NSString::from_str("x"));
    let attrs = attributes_of(&control.attributedStringValue());
    assert_eq!(font_size_of(&attrs), 13.0);
    assert!(attrs.objectForKey(color_key).is_some());
}

// Accessibility

/// A string-valued accessibility getter's answer.
macro_rules! ax {
    ($object:expr, $getter:ident) => {{
        // SAFETY: the getter takes nothing and returns a string or nil.
        let s: Option<Retained<NSString>> = unsafe { msg_send![$object, $getter] };
        s.map(|s| s.to_string())
    }};
}

/// A BOOL-valued accessibility getter's answer.
macro_rules! ax_is {
    ($object:expr, $getter:ident) => {{
        // SAFETY: the getter takes nothing and returns BOOL.
        let b: bool = unsafe { msg_send![$object, $getter] };
        b
    }};
}

/// Set an accessibility property.
macro_rules! ax_set {
    ($object:expr, $setter:ident, $value:expr) => {{
        // SAFETY: the setter takes an object (or BOOL) and returns nothing.
        let _: () = unsafe { msg_send![$object, $setter: $value] };
    }};
}

fn ax_value(object: &AnyObject) -> Option<Retained<AnyObject>> {
    // SAFETY: accessibilityValue takes nothing and returns an object or nil.
    unsafe { msg_send![object, accessibilityValue] }
}

fn ax_number(object: &AnyObject) -> Option<f64> {
    let value = ax_value(object)?;
    assert!(is_kind(&value, NSNumber::class()), "a number");
    // SAFETY: numbers answer doubleValue.
    Some(unsafe { msg_send![&*value, doubleValue] })
}

fn ax_text(object: &AnyObject) -> Option<String> {
    let value = ax_value(object)?;
    assert!(is_kind(&value, NSString::class()), "a string");
    // SAFETY: just checked that it is a string.
    Some(unsafe { &*(Retained::as_ptr(&value).cast::<NSString>()) }.to_string())
}

/// Accessibility properties are kept and answered back, nil included, and
/// until a program sets one each class answers its default. A control
/// isn't an element itself: its cell is, with the role and (for buttons)
/// the title as its label. Views without cells are elements of their own.
fn accessibility(mtm: MainThreadMarker) {
    let s = NSString::from_str;
    // SAFETY: the roles are immutable constant strings.
    let roles: [(&NSString, &str); 20] = unsafe {
        [
            (NSAccessibilityButtonRole, "AXButton"),
            (NSAccessibilityCheckBoxRole, "AXCheckBox"),
            (NSAccessibilityRadioButtonRole, "AXRadioButton"),
            (NSAccessibilityRadioGroupRole, "AXRadioGroup"),
            (NSAccessibilityGroupRole, "AXGroup"),
            (NSAccessibilityStaticTextRole, "AXStaticText"),
            (NSAccessibilityTextFieldRole, "AXTextField"),
            (NSAccessibilityTextAreaRole, "AXTextArea"),
            (NSAccessibilitySliderRole, "AXSlider"),
            (NSAccessibilityIncrementorRole, "AXIncrementor"),
            (NSAccessibilityProgressIndicatorRole, "AXProgressIndicator"),
            (NSAccessibilityBusyIndicatorRole, "AXBusyIndicator"),
            (NSAccessibilityPopUpButtonRole, "AXPopUpButton"),
            (NSAccessibilityImageRole, "AXImage"),
            (NSAccessibilityListRole, "AXList"),
            (NSAccessibilityUnknownRole, "AXUnknown"),
            (NSAccessibilityLayoutAreaRole, "AXLayoutArea"),
            (NSAccessibilitySwitchSubrole, "AXSwitch"),
            (NSAccessibilitySearchFieldSubrole, "AXSearchField"),
            (NSAccessibilitySecureTextFieldSubrole, "AXSecureTextField"),
        ]
    };
    for (constant, value) in roles {
        assert_eq!(constant.to_string(), value);
    }

    // A plain view is nothing in particular until told.
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(ax!(&*view, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert_eq!(ax!(&*view, accessibilitySubrole), None);
    assert_eq!(ax!(&*view, accessibilityLabel), None);
    assert_eq!(ax!(&*view, accessibilityTitle), None);
    assert_eq!(ax!(&*view, accessibilityHelp), None);
    assert_eq!(ax!(&*view, accessibilityIdentifier), None);
    assert!(ax_value(&view).is_none());
    assert!(!ax_is!(&*view, isAccessibilityElement));
    assert!(!ax_is!(&*view, isAccessibilityHidden));
    assert!(!ax_is!(&*view, isAccessibilityEnabled));
    ax_set!(&*view, setAccessibilityLabel, &*s("Area"));
    ax_set!(&*view, setAccessibilityElement, true);
    ax_set!(&*view, setAccessibilityHidden, true);
    ax_set!(&*view, setAccessibilityTitle, &*s("T"));
    ax_set!(&*view, setAccessibilityValue, &*s("V"));
    // SAFETY: the roles are immutable constant strings.
    let (button_role, switch_subrole, group_role) =
        unsafe { (NSAccessibilityButtonRole, NSAccessibilitySwitchSubrole, NSAccessibilityGroupRole) };
    ax_set!(&*view, setAccessibilityRole, button_role);
    ax_set!(&*view, setAccessibilitySubrole, switch_subrole);
    ax_set!(&*view, setAccessibilityRoleDescription, &*s("thing"));
    ax_set!(&*view, setAccessibilityPlaceholderValue, &*s("ph"));
    assert_eq!(ax!(&*view, accessibilityLabel).as_deref(), Some("Area"));
    assert!(ax_is!(&*view, isAccessibilityElement));
    assert!(ax_is!(&*view, isAccessibilityHidden));
    assert_eq!(ax!(&*view, accessibilityTitle).as_deref(), Some("T"));
    assert_eq!(ax_text(&view).as_deref(), Some("V"));
    assert_eq!(ax!(&*view, accessibilityRole).as_deref(), Some("AXButton"));
    assert_eq!(ax!(&*view, accessibilitySubrole).as_deref(), Some("AXSwitch"));
    assert_eq!(ax!(&*view, accessibilityRoleDescription).as_deref(), Some("thing"));
    assert_eq!(ax!(&*view, accessibilityPlaceholderValue).as_deref(), Some("ph"));

    // Controls are enabled as they are; buttons are labeled and titled by
    // their title until told otherwise, and nil is kept as nil.
    let control = NSControl::initWithFrame(NSControl::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(ax!(&*control, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert!(!ax_is!(&*control, isAccessibilityElement));
    assert!(ax_is!(&*control, isAccessibilityEnabled));
    let button = unsafe { NSButton::buttonWithTitle_target_action(&s("OK"), None, None, mtm) };
    assert_eq!(ax!(&*button, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert!(!ax_is!(&*button, isAccessibilityElement));
    assert_eq!(ax!(&*button, accessibilityLabel).as_deref(), Some("OK"));
    assert_eq!(ax!(&*button, accessibilityTitle).as_deref(), Some("OK"));
    button.setTitle(&s("Changed"));
    assert_eq!(ax!(&*button, accessibilityLabel).as_deref(), Some("Changed"));
    ax_set!(&*button, setAccessibilityTitle, &*s("T"));
    assert_eq!(ax!(&*button, accessibilityTitle).as_deref(), Some("T"));
    assert_eq!(ax!(&*button, accessibilityLabel).as_deref(), Some("Changed"));
    ax_set!(&*button, setAccessibilityLabel, &*s("Save"));
    ax_set!(&*button, setAccessibilityHelp, &*s("Saves"));
    ax_set!(&*button, setAccessibilityIdentifier, &*s("save"));
    ax_set!(&*button, setAccessibilityElement, false);
    ax_set!(&*button, setAccessibilityRole, group_role);
    assert_eq!(ax!(&*button, accessibilityLabel).as_deref(), Some("Save"));
    assert_eq!(ax!(&*button, accessibilityHelp).as_deref(), Some("Saves"));
    assert_eq!(ax!(&*button, accessibilityIdentifier).as_deref(), Some("save"));
    assert_eq!(ax!(&*button, accessibilityRole).as_deref(), Some("AXGroup"));
    assert!(!ax_is!(&*button, isAccessibilityElement));
    ax_set!(&*button, setAccessibilityRole, None::<&NSString>);
    ax_set!(&*button, setAccessibilityLabel, None::<&NSString>);
    assert_eq!(ax!(&*button, accessibilityRole), None);
    assert_eq!(ax!(&*button, accessibilityLabel), None);
    ax_set!(&*button, setAccessibilityEnabled, false);
    assert!(!ax_is!(&*button, isAccessibilityEnabled));
    assert!(button.isEnabled(), "accessibility doesn't disable the button");

    // What's set on a control reaches its cell as macOS has it: help is
    // the cell's too; a label set on a button leaves its cell's empty; the
    // title and the rest stay the control's.
    let forwarded = unsafe { NSButton::buttonWithTitle_target_action(&s("OK"), None, None, mtm) };
    let fc = forwarded.cell().expect("a cell");
    ax_set!(&*forwarded, setAccessibilityHelp, &*s("Saves"));
    assert_eq!(ax!(&*fc, accessibilityHelp).as_deref(), Some("Saves"));
    ax_set!(&*forwarded, setAccessibilityLabel, &*s("Save"));
    assert_eq!(ax!(&*forwarded, accessibilityLabel).as_deref(), Some("Save"));
    assert_eq!(ax!(&*fc, accessibilityLabel).as_deref(), Some(""));
    ax_set!(&*forwarded, setAccessibilityTitle, &*s("T"));
    assert_eq!(ax!(&*fc, accessibilityTitle).as_deref(), Some("OK"));
    ax_set!(&*forwarded, setAccessibilityIdentifier, &*s("id"));
    assert_eq!(ax!(&*fc, accessibilityIdentifier), None);
    ax_set!(&*forwarded, setAccessibilityLabel, None::<&NSString>);
    assert_eq!(ax!(&*fc, accessibilityLabel).as_deref(), Some(""));
    // A control's label, until set, is its cell's.
    let fresh = unsafe { NSButton::buttonWithTitle_target_action(&s("OK"), None, None, mtm) };
    ax_set!(&*fresh.cell().expect("a cell"), setAccessibilityLabel, &*s("CellLabel"));
    assert_eq!(ax!(&*fresh, accessibilityLabel).as_deref(), Some("CellLabel"));
    ax_set!(&*fresh.cell().expect("a cell"), setAccessibilityHelp, &*s("CellHelp"));
    assert_eq!(ax!(&*fresh, accessibilityHelp), None);
    let field_label = NSTextField::labelWithString(&s("Lab"), mtm);
    ax_set!(&*field_label, setAccessibilityLabel, &*s("LL"));
    ax_set!(&*field_label, setAccessibilityHelp, &*s("LH"));
    let flc = field_label.cell().expect("a cell");
    assert_eq!((ax!(&*flc, accessibilityLabel), ax!(&*flc, accessibilityHelp).as_deref()), (None, Some("LH")));

    // Cells stand for their controls.
    let cell = |c: &NSControl| c.cell().expect("a cell");
    let push = unsafe { NSButton::buttonWithTitle_target_action(&s("OK"), None, None, mtm) };
    let push_cell = cell(&push);
    assert_eq!(ax!(&*push_cell, accessibilityRole).as_deref(), Some("AXButton"));
    assert_eq!(ax!(&*push_cell, accessibilityLabel).as_deref(), Some("OK"));
    assert!(ax_is!(&*push_cell, isAccessibilityElement));
    assert!(ax_is!(&*push_cell, isAccessibilityEnabled));
    let lone = NSButtonCell::new(mtm);
    assert_eq!(ax!(&*lone, accessibilityLabel).as_deref(), Some("Button"));
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&s("Check"), None, None, mtm) };
    check.setState(1);
    assert_eq!(ax!(&*cell(&check), accessibilityRole).as_deref(), Some("AXCheckBox"));
    assert_eq!(ax_number(&cell(&check)), Some(1.0));
    let radio = unsafe { NSButton::radioButtonWithTitle_target_action(&s("Radio"), None, None, mtm) };
    assert_eq!(ax!(&*cell(&radio), accessibilityRole).as_deref(), Some("AXRadioButton"));
    assert_eq!(ax_number(&cell(&radio)), Some(0.0));

    let plain = NSCell::new(mtm);
    assert_eq!(ax!(&*plain, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert!(ax_is!(&*plain, isAccessibilityElement));
    ax_set!(&*plain, setAccessibilityLabel, &*s("C"));
    assert_eq!(ax!(&*plain, accessibilityLabel).as_deref(), Some("C"));
    let text = NSCell::initTextCell(NSCell::alloc(mtm), &s("x"));
    assert_eq!(ax!(&*text, accessibilityRole).as_deref(), Some("AXStaticText"));
    assert_eq!(ax_text(&text).as_deref(), Some("x"));

    let label = NSTextField::labelWithString(&s("Label"), mtm);
    assert_eq!(ax!(&*label, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert!(!ax_is!(&*label, isAccessibilityElement));
    assert_eq!(ax_text(&label).as_deref(), Some("Label"));
    assert_eq!(ax!(&*cell(&label), accessibilityRole).as_deref(), Some("AXStaticText"));
    assert_eq!(ax_text(&cell(&label)).as_deref(), Some("Label"));
    let field = NSTextField::textFieldWithString(&s("Field"), mtm);
    assert_eq!(ax!(&*cell(&field), accessibilityRole).as_deref(), Some("AXTextField"));
    assert_eq!(ax!(&*cell(&field), accessibilitySubrole), None);
    let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    assert_eq!(ax!(&*cell(&secure), accessibilityRole).as_deref(), Some("AXTextField"));
    assert_eq!(ax!(&*cell(&secure), accessibilitySubrole).as_deref(), Some("AXSecureTextField"));
    let search = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 100.0, 22.0));
    assert_eq!(ax!(&*cell(&search), accessibilityRole).as_deref(), Some("AXTextField"));
    assert_eq!(ax!(&*cell(&search), accessibilitySubrole).as_deref(), Some("AXSearchField"));

    let segments = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &objc2_foundation::NSArray::from_retained_slice(&[s("A"), s("B")]),
            NSSegmentSwitchTracking::SelectOne,
            None,
            None,
            mtm,
        )
    };
    assert_eq!(ax!(&*segments, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert_eq!(ax!(&*cell(&segments), accessibilityRole).as_deref(), Some("AXRadioGroup"));
    let stepper = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 20.0, 26.0));
    assert_eq!(ax!(&*cell(&stepper), accessibilityRole).as_deref(), Some("AXIncrementor"));
    assert_eq!(ax_number(&cell(&stepper)), Some(0.0));
    let slider = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(0.5, 0.0, 1.0, None, None, mtm) };
    assert_eq!(ax!(&*slider, accessibilityRole).as_deref(), Some("AXUnknown"));
    assert_eq!(ax!(&*cell(&slider), accessibilityRole).as_deref(), Some("AXSlider"));
    assert_eq!(ax_number(&cell(&slider)), Some(0.5));

    // Views without cells.
    let group = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    assert_eq!(ax!(&*group, accessibilityRole).as_deref(), Some("AXGroup"));
    assert!(ax_is!(&*group, isAccessibilityElement));
    assert!(!ax_is!(&*group, isAccessibilityEnabled));
    group.setBoxType(NSBoxType::Separator);
    assert!(!ax_is!(&*group, isAccessibilityElement));
    let progress = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    assert_eq!(ax!(&*progress, accessibilityRole).as_deref(), Some("AXBusyIndicator"));
    assert!(ax_is!(&*progress, isAccessibilityElement));
    progress.setIndeterminate(false);
    progress.setDoubleValue(30.0);
    assert_eq!(ax!(&*progress, accessibilityRole).as_deref(), Some("AXProgressIndicator"));
    // The value is the fraction done.
    assert_eq!(ax_number(&progress), Some(0.3));
    let switch = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 54.0, 24.0));
    assert_eq!(ax!(&*switch, accessibilityRole).as_deref(), Some("AXButton"));
    assert_eq!(ax!(&*switch, accessibilitySubrole).as_deref(), Some("AXSwitch"));
    assert!(ax_is!(&*switch, isAccessibilityElement));
    assert_eq!(ax_number(&switch), Some(0.0));
    switch.setState(1);
    assert_eq!(ax_number(&switch), Some(1.0));
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
        ("check_box_rects", check_box_rects),
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
        ("accessibility", accessibility),
        ("values_that_change_their_cell", values_that_change_their_cell),
        ("copying_cells", copying_cells),
        ("sizes_that_fit", sizes_that_fit),
        ("hit_tests", hit_tests),
        ("attributed_values", attributed_values),
        ("text_notifications", text_notifications),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
