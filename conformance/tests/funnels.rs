//! The funnels: AppKit methods that other AppKit methods reach by message,
//! so a subclass's override sees each call, with what AppKit passes.
//! Each test overrides one in a subclass, logs the calls, and checks which
//! operations go through it, in what order and with which arguments, as
//! measured on macOS:
//!
//! - a text field's `textView:doCommandBySelector:`, which the field
//!   editor offers each command to, asks the field's delegate
//!   (`control:textView:doCommandBySelector:`), whose YES keeps the
//!   editor from performing it;
//! - a text view's selection changes all go through
//!   `setSelectedRanges:affinity:stillSelecting:`, its own included;
//! - `textContainerOrigin` places the text for hit testing and drawing;
//! - `copy:` and `cut:` write through `writeSelectionToPasteboard:types:`,
//!   which writes each type through `writeSelectionToPasteboard:type:`;
//! - dragging asks `characterIndexForInsertionAtPoint:` where a drop goes;
//! - a click ends a composition with `unmarkText`;
//! - `setNeedsDisplay:` goes through `setNeedsDisplayInRect:`;
//! - `orderFront:`, `orderBack:`, `orderOut:`, `makeKeyAndOrderFront:`
//!   and `close` go through `orderWindow:relativeTo:`, and frame changes
//!   through `constrainFrameRect:toScreen:`.
//!
//! No window is shown: the overriding window never calls super to order
//! itself in. AppKit belongs to the main thread, so this file has its own
//! `main`.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSControl, NSDragOperation, NSEvent,
    NSEventModifierFlags, NSEventType, NSFont, NSFontWeightRegular, NSLayoutManager, NSPasteboard,
    NSPasteboardTypeString, NSResponder, NSScreen, NSSelectionAffinity, NSText, NSTextContainer, NSTextField,
    NSTextInputClient, NSTextStorage, NSTextView, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSMutableAttributedString, NSNotFound, NSNotification, NSNumber, NSPoint, NSRange, NSRect, NSSize,
    NSString, NSValue,
};

use sidestep as _;

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// What the text field delegate answers.
    static ANSWER: Cell<bool> = const { Cell::new(false) };
    /// The origin the text view reports, when set.
    static ORIGIN: Cell<Option<NSPoint>> = const { Cell::new(None) };
}

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelFieldDelegate"]
    struct FieldDelegate;

    unsafe impl NSObjectProtocol for FieldDelegate {}

    impl FieldDelegate {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _control: &NSControl, text_view: &NSTextView, command: Sel) -> bool {
            log(format!("delegate {} editor {}", command.name().to_str().unwrap(), text_view.isFieldEditor()));
            ANSWER.with(Cell::get)
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn did_end(&self, n: &NSNotification) {
            let movement = n.userInfo().and_then(|i| i.objectForKey(&s("NSTextMovement")));
            let movement = movement.and_then(|m| m.downcast::<NSNumber>().ok()).map(|m| m.integerValue());
            log(format!("didEnd {movement:?}"));
        }
    }
);

define_class!(
    #[unsafe(super(NSTextField, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelField"]
    struct Field;

    impl Field {
        #[unsafe(method(textView:doCommandBySelector:))]
        fn command(&self, text_view: &NSTextView, command: Sel) -> bool {
            log(format!("field {}", command.name().to_str().unwrap()));
            let answer: bool = unsafe { msg_send![super(self), textView: text_view, doCommandBySelector: command] };
            log(format!("super {answer}"));
            answer
        }
    }
);

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelTextView"]
    struct TextView;

    impl TextView {
        #[unsafe(method(setSelectedRanges:affinity:stillSelecting:))]
        fn set_ranges(&self, ranges: &NSArray<NSValue>, affinity: NSSelectionAffinity, still: bool) {
            let list: Vec<String> = ranges.iter().map(|v| {
                let r = unsafe { v.rangeValue() };
                format!("{},{}", r.location, r.length)
            }).collect();
            log(format!("select {} {} {still}", list.join(" "), affinity.0));
            let _: () = unsafe { msg_send![super(self), setSelectedRanges: ranges, affinity: affinity, stillSelecting: still] };
        }

        #[unsafe(method(textContainerOrigin))]
        fn origin(&self) -> NSPoint {
            let own: NSPoint = unsafe { msg_send![super(self), textContainerOrigin] };
            match ORIGIN.with(Cell::get) {
                Some(p) => {
                    log("origin".into());
                    p
                }
                None => own,
            }
        }

        #[unsafe(method(writeSelectionToPasteboard:types:))]
        fn write_types(&self, pb: &NSPasteboard, types: &NSArray<NSString>) -> bool {
            let names: Vec<String> = types.iter().map(|t| t.to_string()).collect();
            log(format!("types {}", names.join(",")));
            unsafe { msg_send![super(self), writeSelectionToPasteboard: pb, types: types] }
        }

        #[unsafe(method(writeSelectionToPasteboard:type:))]
        fn write_type(&self, pb: &NSPasteboard, t: &NSString) -> bool {
            log(format!("type {t}"));
            unsafe { msg_send![super(self), writeSelectionToPasteboard: pb, type: t] }
        }

        #[unsafe(method(characterIndexForInsertionAtPoint:))]
        fn insertion_index(&self, p: NSPoint) -> usize {
            let index: usize = unsafe { msg_send![super(self), characterIndexForInsertionAtPoint: p] };
            log(format!("index {},{} {index}", p.x, p.y));
            index
        }

        #[unsafe(method(unmarkText))]
        fn unmark(&self) {
            log("unmark".into());
            let _: () = unsafe { msg_send![super(self), unmarkText] };
        }
    }
);

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelView"]
    #[ivars = u32]
    struct View;

    impl View {
        #[unsafe(method(setNeedsDisplayInRect:))]
        fn needs_display(&self, r: NSRect) {
            log(format!("rect {} {} {} {}", r.origin.x, r.origin.y, r.size.width, r.size.height));
            let _: () = unsafe { msg_send![super(self), setNeedsDisplayInRect: r] };
        }

        #[unsafe(method(viewWillStartLiveResize))]
        fn will_start(&self) {
            log(format!("will {}", self.ivars()));
            let _: () = unsafe { msg_send![super(self), viewWillStartLiveResize] };
        }

        #[unsafe(method(viewDidEndLiveResize))]
        fn did_end(&self) {
            log(format!("did {}", self.ivars()));
            let _: () = unsafe { msg_send![super(self), viewDidEndLiveResize] };
        }
    }
);

define_class!(
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelWindow"]
    struct Window;

    impl Window {
        /// Logged, never ordered in: the window doesn't show.
        #[unsafe(method(orderWindow:relativeTo:))]
        fn order(&self, place: NSWindowOrderingMode, other: isize) {
            log(format!("order {} {other}", place.0));
        }

        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain(&self, frame: NSRect, screen: Option<&NSScreen>) -> NSRect {
            let out: NSRect = unsafe { msg_send![super(self), constrainFrameRect: frame, toScreen: screen] };
            log(format!(
                "constrain {} {} {} {} {}",
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
                if screen.is_some() { "screen" } else { "nil" }
            ));
            out
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelDragInfo"]
    #[ivars = (Retained<NSPasteboard>, NSPoint)]
    struct DragInfo;

    unsafe impl NSObjectProtocol for DragInfo {}

    impl DragInfo {
        #[unsafe(method_id(draggingPasteboard))]
        fn pasteboard(&self) -> Retained<NSPasteboard> {
            self.ivars().0.clone()
        }

        #[unsafe(method(draggingLocation))]
        fn location(&self) -> NSPoint {
            self.ivars().1
        }

        #[unsafe(method(draggingSourceOperationMask))]
        fn mask(&self) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Generic
        }

        #[unsafe(method(draggingSource))]
        fn source(&self) -> *mut AnyObject {
            std::ptr::null_mut()
        }

        #[unsafe(method(draggingSequenceNumber))]
        fn sequence(&self) -> isize {
            1
        }

        #[unsafe(method(draggingDestinationWindow))]
        fn destination(&self) -> *mut AnyObject {
            std::ptr::null_mut()
        }

        #[unsafe(method(draggedImageLocation))]
        fn image_location(&self) -> NSPoint {
            self.ivars().1
        }

        #[unsafe(method(numberOfValidItemsForDrop))]
        fn valid_items(&self) -> isize {
            1
        }

        #[unsafe(method(setNumberOfValidItemsForDrop:))]
        fn set_valid_items(&self, _n: isize) {}

        #[unsafe(method(animatesToDestination))]
        fn animates(&self) -> bool {
            false
        }

        #[unsafe(method(setAnimatesToDestination:))]
        fn set_animates(&self, _flag: bool) {}

        #[unsafe(method(draggingFormation))]
        fn formation(&self) -> isize {
            0
        }
    }
);

fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn text_view(mtm: MainThreadMarker) -> Retained<NSTextView> {
    let frame = rect(0.0, 0.0, 300.0, 200.0);
    let ts = NSTextStorage::new();
    let lm = NSLayoutManager::new();
    ts.addLayoutManager(&lm);
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(300.0, 1.0e7));
    lm.addTextContainer(&tc);
    let tv: Retained<TextView> = unsafe { msg_send![TextView::alloc(mtm), initWithFrame: frame, textContainer: &*tc] };
    let tv: Retained<NSTextView> = unsafe { Retained::cast_unchecked(tv) };
    tv.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })));
    tv
}

fn selected(tv: &NSTextView) -> (usize, usize) {
    let r = NSTextInputClient::selectedRange(tv);
    (r.location, r.length)
}

/// A click at `at` (in the view): the release is posted first, so the
/// view's tracking loop ends at once.
fn click(app: &NSApplication, w: &NSWindow, view: &NSView, at: NSPoint) {
    let loc = view.convertPoint_toView(at, None);
    let event = |kind, pressure| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            loc,
            NSEventModifierFlags::empty(),
            0.0,
            w.windowNumber(),
            None,
            0,
            1,
            pressure,
        )
        .expect("a mouse event")
    };
    app.postEvent_atStart(&event(NSEventType::LeftMouseUp, 0.0), true);
    view.mouseDown(&event(NSEventType::LeftMouseDown, 1.0));
}

/// The field editor offers each command to the field; the field asks its
/// delegate, whose YES means the command was handled, so the editor does
/// nothing more (Return doesn't end editing).
fn text_field_commands(mtm: MainThreadMarker) {
    let w = window(mtm);
    let d: Retained<FieldDelegate> = unsafe { msg_send![FieldDelegate::alloc(mtm), init] };
    for subclass in [false, true] {
        let frame = rect(10.0, 10.0, 200.0, 22.0);
        let field: Retained<NSTextField> = if subclass {
            let f: Retained<Field> = unsafe { msg_send![Field::alloc(mtm), initWithFrame: frame] };
            unsafe { Retained::cast_unchecked(f) }
        } else {
            NSTextField::initWithFrame(NSTextField::alloc(mtm), frame)
        };
        field.setStringValue(&s("hello"));
        w.contentView().unwrap().addSubview(&field);
        let _: () = unsafe { msg_send![&*field, setDelegate: &*d] };
        for answer in [false, true] {
            ANSWER.with(|a| a.set(answer));
            assert!(w.makeFirstResponder(Some(&field)));
            let editor = unsafe { w.fieldEditor_forObject(true, Some(&field)) }.expect("the field editor");
            take();
            for command in ["moveUp:", "cancelOperation:", "insertNewline:"] {
                let selector = Sel::register(&std::ffi::CString::new(command).unwrap());
                let _: () = unsafe { msg_send![&*editor, doCommandBySelector: selector] };
                let mut expected = Vec::new();
                if subclass {
                    expected.push(format!("field {command}"));
                }
                expected.push(format!("delegate {command} editor true"));
                if subclass {
                    expected.push(format!("super {answer}"));
                }
                if command == "insertNewline:" && !answer {
                    // NSReturnTextMovement.
                    expected.push("didEnd Some(16)".into());
                }
                assert_eq!(take(), expected, "{command} (subclass {subclass}, answer {answer})");
                w.makeFirstResponder(Some(&field));
                take();
            }
            w.makeFirstResponder(None);
            take();
        }
        field.removeFromSuperview();
    }
    // Without a delegate that answers, a field doesn't take the command.
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::ZERO);
    let editor = NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::ZERO);
    let taken: bool = unsafe { msg_send![&*field, textView: &*editor, doCommandBySelector: sel!(insertNewline:)] };
    assert!(!taken);
    // With one, it asks whether or not it is editing.
    let _: () = unsafe { msg_send![&*field, setDelegate: &*d] };
    ANSWER.with(|a| a.set(true));
    let taken: bool = unsafe { msg_send![&*field, textView: &*editor, doCommandBySelector: sel!(insertNewline:)] };
    assert!(taken);
    assert_eq!(take(), ["delegate insertNewline: editor false"]);
}

/// Every change of the selection goes through
/// `setSelectedRanges:affinity:stillSelecting:`, with the affinity (0
/// upstream, 1 downstream) and still-selecting flag AppKit gives each.
fn selection(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let w = window(mtm);
    let tv = text_view(mtm);
    w.contentView().unwrap().addSubview(&tv);
    tv.setString(&s("hello world, again"));
    assert_eq!(take(), ["select 18,0 0 false"]);
    tv.setSelectedRange(NSRange::new(1, 2));
    assert_eq!(take(), ["select 1,2 0 false"]);
    tv.setSelectedRange_affinity_stillSelecting(NSRange::new(2, 0), NSSelectionAffinity::Upstream, true);
    assert_eq!(take(), ["select 2,0 0 true"]);
    tv.setSelectedRanges(&NSArray::from_retained_slice(&[unsafe { NSValue::valueWithRange(NSRange::new(3, 1)) }]));
    assert_eq!(take(), ["select 3,1 0 false"]);
    let _: () = unsafe { msg_send![&*tv, moveRight: None::<&AnyObject>] };
    assert_eq!(take(), ["select 4,0 1 false"]);
    let _: () = unsafe { msg_send![&*tv, moveWordRightAndModifySelection: None::<&AnyObject>] };
    assert_eq!(take(), ["select 4,1 1 false"]);
    let _: () = unsafe { msg_send![&*tv, selectAll: None::<&AnyObject>] };
    assert_eq!(take(), ["select 0,18 0 false"]);
    tv.setSelectedRange(NSRange::new(5, 0));
    take();
    unsafe { tv.insertText_replacementRange(&s("X"), NSRange::new(NSNotFound as usize, 0)) };
    assert_eq!(take(), ["select 6,0 0 false"]);
    let _: () = unsafe { msg_send![&*tv, deleteBackward: None::<&AnyObject>] };
    assert_eq!(take(), ["select 5,0 0 false"]);
    let _: () = unsafe { msg_send![&*tv, selectWord: None::<&AnyObject>] };
    assert_eq!(take(), ["select 0,5 0 false"]);
    // A program editing the storage moves the selection through it too.
    let storage = unsafe { tv.textStorage() }.unwrap();
    let m: &NSMutableAttributedString = &storage;
    m.replaceCharactersInRange_withString(NSRange::new(0, 5), &s("HI"));
    assert_eq!(take(), ["select 2,0 0 false"]);
    // A click: still selecting while the button is down, then not.
    tv.setSelectedRange(NSRange::new(0, 0));
    take();
    click(&app, &w, &tv, NSPoint::new(1.0, 5.0));
    let log = take();
    // Still selecting while the button is down (once, or more), then not.
    let still: Vec<bool> = log.iter().map(|l| l.ends_with("true")).collect();
    let (last, down) = still.split_last().expect("selection changes");
    assert!(!last && !down.is_empty() && down.iter().all(|&s| s), "{log:?}");
    assert!(log.iter().all(|l| l.starts_with("select 0,0 ")), "{log:?}");
    assert_eq!(selected(&tv), (0, 0));
}

/// Hit testing and drawing place the text at `textContainerOrigin`.
fn container_origin(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("abc"));
    take();
    let at_start = |p: NSPoint| tv.characterIndexForInsertionAtPoint(p);
    ORIGIN.with(|o| o.set(Some(NSPoint::new(40.0, 30.0))));
    let index = at_start(NSPoint::new(41.0, 33.0));
    assert!(take().iter().any(|l| l == "origin"), "hit testing asks for the origin");
    assert_eq!(index, 0);
    let far = at_start(NSPoint::new(250.0, 33.0));
    take();
    assert_eq!(far, 3);
    let _ = unsafe { tv.firstRectForCharacterRange_actualRange(NSRange::new(0, 1), std::ptr::null_mut()) };
    assert!(take().iter().any(|l| l == "origin"), "firstRectForCharacterRange asks for the origin");
    let rep = tv.bitmapImageRepForCachingDisplayInRect(tv.bounds()).expect("a bitmap");
    tv.cacheDisplayInRect_toBitmapImageRep(tv.bounds(), &rep);
    assert!(take().iter().any(|l| l == "origin"), "drawing asks for the origin");
    ORIGIN.with(|o| o.set(None));
}

/// `copy:` and `cut:` write the writable types through the funnel, each
/// type through the one-type method, in order.
/// The general pasteboard's items as they were, put back when dropped:
/// `copy:` and `cut:` only write to the general pasteboard, which on macOS
/// is the user's clipboard.
struct KeptPasteboard(Vec<Retained<objc2_app_kit::NSPasteboardItem>>);

impl KeptPasteboard {
    fn keep() -> Self {
        let pb = NSPasteboard::generalPasteboard();
        let items = pb.pasteboardItems().map(|items| items.to_vec()).unwrap_or_default();
        KeptPasteboard(
            items
                .iter()
                .map(|item| {
                    let copy = objc2_app_kit::NSPasteboardItem::new();
                    for t in item.types().iter() {
                        if let Some(data) = item.dataForType(&t) {
                            copy.setData_forType(&data, &t);
                        }
                    }
                    copy
                })
                .collect(),
        )
    }
}

impl Drop for KeptPasteboard {
    fn drop(&mut self) {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if !self.0.is_empty() {
            let writers: Vec<Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>> =
                self.0.drain(..).map(ProtocolObject::from_retained).collect();
            pb.writeObjects(&NSArray::from_retained_slice(&writers));
        }
    }
}

fn pasteboard(mtm: MainThreadMarker) {
    let _kept = KeptPasteboard::keep();
    let tv = text_view(mtm);
    tv.setRichText(false);
    tv.setString(&s("copy me"));
    tv.setSelectedRange(NSRange::new(0, 4));
    take();
    let types: Retained<NSArray<NSString>> = unsafe { msg_send![&*tv, writablePasteboardTypes] };
    let names: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let expected: Vec<String> = std::iter::once(format!("types {}", names.join(",")))
        .chain(names.iter().map(|n| format!("type {n}")))
        .collect();
    let _: () = unsafe { msg_send![&*tv, copy: None::<&AnyObject>] };
    assert_eq!(take(), expected);
    let pb = NSPasteboard::generalPasteboard();
    assert_eq!(pb.stringForType(unsafe { NSPasteboardTypeString }).map(|t| t.to_string()).as_deref(), Some("copy"));
    let _: () = unsafe { msg_send![&*tv, cut: None::<&AnyObject>] };
    let log = take();
    assert_eq!(log[..expected.len()], expected[..]);
    assert_eq!(log[expected.len()..], ["select 0,0 0 false"]);
    assert_eq!(tv.string().to_string(), " me");
}

/// A drop asks the view where it goes, at the drag's place in the view,
/// and leaves what it put in selected.
fn drop_index(mtm: MainThreadMarker) {
    let w = window(mtm);
    let tv = text_view(mtm);
    w.contentView().unwrap().addSubview(&tv);
    tv.setString(&s("drop here please"));
    take();
    let pb = NSPasteboard::pasteboardWithUniqueName();
    pb.clearContents();
    pb.setString_forType(&s("X"), unsafe { NSPasteboardTypeString });
    let at = NSPoint::new(20.0, 5.0);
    let loc = tv.convertPoint_toView(at, None);
    let info: Retained<DragInfo> = unsafe { msg_send![super(DragInfo::alloc(mtm).set_ivars((pb.clone(), loc))), init] };
    let op: NSDragOperation = unsafe { msg_send![&*tv, draggingEntered: &*info] };
    assert_eq!(op, NSDragOperation::Copy);
    let log = take();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("index 20,5 "), "{log:?}");
    let index: usize = log[0].rsplit(' ').next().unwrap().parse().unwrap();
    let _: NSDragOperation = unsafe { msg_send![&*tv, draggingUpdated: &*info] };
    assert_eq!(take(), [format!("index 20,5 {index}")]);
    let ok: bool = unsafe { msg_send![&*tv, performDragOperation: &*info] };
    assert!(ok);
    let log = take();
    assert!(log.iter().any(|l| l.starts_with("index 20,5")), "{log:?}");
    assert_eq!(log.last().map(String::as_str), Some(format!("select {index},1 0 false").as_str()), "{log:?}");
    let mut expected = String::from("drop here please");
    expected.insert(index, 'X');
    assert_eq!(tv.string().to_string(), expected);
    assert_eq!(selected(&tv), (index, 1));
}

/// A click while an input method composes commits the marked text through
/// `unmarkText`.
fn unmark_on_click(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let w = window(mtm);
    let tv = text_view(mtm);
    w.contentView().unwrap().addSubview(&tv);
    assert!(w.makeFirstResponder(Some(&tv)));
    tv.setString(&s("ab"));
    tv.setSelectedRange(NSRange::new(1, 0));
    unsafe {
        tv.setMarkedText_selectedRange_replacementRange(
            &s("k"),
            NSRange::new(1, 0),
            NSRange::new(NSNotFound as usize, 0),
        )
    };
    assert!(NSTextInputClient::hasMarkedText(&*tv));
    take();
    click(&app, &w, &tv, NSPoint::new(1.0, 5.0));
    assert_eq!(take().iter().filter(|l| *l == "unmark").count(), 1);
    assert!(!NSTextInputClient::hasMarkedText(&*tv));
    assert_eq!(tv.string().to_string(), "akb");
}

/// `setNeedsDisplay:YES` marks the whole plane through
/// `setNeedsDisplayInRect:`; NO does nothing.
fn needs_display(mtm: MainThreadMarker) {
    let v: Retained<View> =
        unsafe { msg_send![super(View::alloc(mtm).set_ivars(0)), initWithFrame: rect(5.0, 6.0, 30.0, 40.0)] };
    v.setNeedsDisplay(true);
    let half = -f64::MAX / 2.0;
    assert_eq!(take(), [format!("rect {half} {half} {} {}", f64::MAX, f64::MAX)]);
    v.setNeedsDisplay(false);
    assert_eq!(take(), Vec::<String>::new());
    let w = window(mtm);
    w.contentView().unwrap().addSubview(&v);
    take();
    v.setNeedsDisplay(true);
    assert_eq!(take(), [format!("rect {half} {half} {} {}", f64::MAX, f64::MAX)]);
}

/// A view's own live resize methods don't pass the message on: a window
/// sends it to each view.
fn live_resize_hooks(mtm: MainThreadMarker) {
    let make = |n: u32| -> Retained<View> {
        unsafe { msg_send![super(View::alloc(mtm).set_ivars(n)), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] }
    };
    let (top, child) = (make(1), make(2));
    top.addSubview(&child);
    take();
    let _: () = unsafe { msg_send![&*top, viewWillStartLiveResize] };
    let log: Vec<String> = take().into_iter().filter(|l| !l.starts_with("rect")).collect();
    assert_eq!(log, ["will 1"]);
    let _: () = unsafe { msg_send![&*top, viewDidEndLiveResize] };
    let log: Vec<String> = take().into_iter().filter(|l| !l.starts_with("rect")).collect();
    assert_eq!(log, ["did 1"]);
    let live: bool = unsafe { msg_send![&*child, inLiveResize] };
    assert!(!live);
}

/// Ordering goes through `orderWindow:relativeTo:` (relative to no
/// window). Frame changes of a window that isn't on screen aren't
/// constrained (`constrainFrameRect:toScreen:` isn't asked).
fn window_funnels(mtm: MainThreadMarker) {
    let w: Retained<Window> = unsafe {
        msg_send![Window::alloc(mtm), initWithContentRect: rect(100.0, 100.0, 400.0, 300.0), styleMask: NSWindowStyleMask::Titled, backing: NSBackingStoreType::Buffered, defer: true]
    };
    let w: Retained<NSWindow> = unsafe { Retained::cast_unchecked(w) };
    unsafe { w.setReleasedWhenClosed(false) };
    assert_eq!(take(), Vec::<String>::new(), "made without either");
    w.orderFront(None);
    assert_eq!(take(), ["order 1 0"]);
    w.orderBack(None);
    assert_eq!(take(), ["order -1 0"]);
    w.orderOut(None);
    assert_eq!(take(), ["order 0 0"]);
    w.makeKeyAndOrderFront(None);
    assert_eq!(take(), ["order 1 0"]);
    w.orderWindow_relativeTo(NSWindowOrderingMode::Above, 5);
    assert_eq!(take(), ["order 1 5"]);
    assert!(!w.isVisible());
    // A window that isn't on screen isn't constrained: nothing asks.
    let frame = rect(50.0, 60.0, 300.0, 200.0);
    w.setFrame_display(frame, true);
    assert_eq!(w.frame(), frame);
    w.setFrameOrigin(NSPoint::new(70.0, 80.0));
    w.setFrameTopLeftPoint(NSPoint::new(10.0, 500.0));
    assert_eq!(w.frame(), rect(10.0, 300.0, 300.0, 200.0));
    // Its content grows down from its top left corner.
    let before = w.frame();
    w.setContentSize(NSSize::new(250.0, 150.0));
    let after = w.frame();
    assert_eq!(after.origin.x, before.origin.x);
    assert_eq!(after.origin.y + after.size.height, before.origin.y + before.size.height);
    w.center();
    assert_eq!(take(), Vec::<String>::new());
    // Asked with no screen, it constrains to the main screen's visible
    // frame (a titled window: inside it).
    let asked = rect(10.0, 30000.0, 400.0, 300.0);
    let got: NSRect = unsafe { msg_send![&*w, constrainFrameRect: asked, toScreen: None::<&NSScreen>] };
    match NSScreen::mainScreen(mtm) {
        Some(main) => {
            let v = main.visibleFrame();
            assert_eq!(got, rect(10.0, v.origin.y + v.size.height - 300.0, 400.0, 300.0));
            let low: NSRect = unsafe {
                msg_send![&*w, constrainFrameRect: rect(-900.0, -900.0, 400.0, 300.0), toScreen: None::<&NSScreen>]
            };
            assert_eq!(low.origin, v.origin);
        }
        None => assert_eq!(got, asked, "no screen at all"),
    }
    take();
    w.close();
    assert_eq!(take(), ["order 0 0"]);
}

define_class!(
    /// Orders itself as AppKit does, logging what it's asked to constrain.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FunnelShownWindow"]
    struct ShownWindow;

    impl ShownWindow {
        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain(&self, frame: NSRect, screen: Option<&NSScreen>) -> NSRect {
            let out: NSRect = unsafe { msg_send![super(self), constrainFrameRect: frame, toScreen: screen] };
            log(format!("constrain {} {}", frame.origin.y, out.origin.y));
            out
        }
    }
);

/// Opt-in on macOS (`SIDESTEP_CONFORMANCE_WINDOWS=1`), as it shows a
/// window (never activating the application); `sidestep-appkit`'s
/// `linux_sweep` counts the same on Linux. Ordering in constrains once,
/// on screen or not; moving the window asks twice, the second time with
/// the first answer; a frame or a content size once, the content keeping
/// its top left corner.
fn window_constraints_on_screen(mtm: MainThreadMarker) {
    if std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_none() || !cfg!(target_vendor = "apple") {
        println!("funnels: window_constraints_on_screen skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs it on macOS)");
        return;
    }
    let spin = || {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
    };
    let w: Retained<ShownWindow> = unsafe {
        msg_send![ShownWindow::alloc(mtm), initWithContentRect: rect(100.0, 100.0, 400.0, 300.0), styleMask: NSWindowStyleMask::Titled, backing: NSBackingStoreType::Buffered, defer: true]
    };
    let w: Retained<NSWindow> = unsafe { Retained::cast_unchecked(w) };
    unsafe { w.setReleasedWhenClosed(false) };
    let count = |what: &str, n: usize| {
        let log = take();
        assert_eq!(log.iter().filter(|l| l.starts_with("constrain")).count(), n, "{what}: {log:?}");
    };
    w.setFrameOrigin(NSPoint::new(10.0, 30000.0));
    count("moved off screen", 0);
    w.orderFront(None);
    spin();
    count("ordered in", 1);
    assert!(w.frame().origin.y < 30000.0, "constrained: {:?}", w.frame());
    w.orderFront(None);
    spin();
    count("ordered front again", 1);
    w.setFrame_display(rect(100.0, 100.0, 400.0, 300.0), false);
    count("setFrame:display:", 1);
    w.setContentSize(NSSize::new(200.0, 100.0));
    count("setContentSize:", 1);
    let f = w.frame();
    assert_eq!((f.origin.x, f.origin.y + f.size.height), (100.0, 400.0), "the top left corner stays");
    w.setFrameOrigin(NSPoint::new(10.0, 200.0));
    count("setFrameOrigin:", 2);
    w.setFrameTopLeftPoint(NSPoint::new(10.0, 500.0));
    count("setFrameTopLeftPoint:", 2);
    w.center();
    count("center", 2);
    w.close();
    take();
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let tests: &[Test] = &[
        ("text_field_commands", text_field_commands),
        ("selection", selection),
        ("container_origin", container_origin),
        ("pasteboard", pasteboard),
        ("drop_index", drop_index),
        ("unmark_on_click", unmark_on_click),
        ("needs_display", needs_display),
        ("live_resize_hooks", live_resize_hooks),
        ("window_funnels", window_funnels),
        ("window_constraints_on_screen", window_constraints_on_screen),
    ];
    for (name, test) in tests {
        take();
        test(mtm);
        println!("test {name} ... ok");
    }
}
