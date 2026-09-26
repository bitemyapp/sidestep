//! Field editor sessions on Linux, started through the testing hooks the
//! controls' methods will call (`-[NSTextField selectText:]` and the
//! rest): a control that leaves its window, or goes, mid-edit leaves
//! nothing behind that the next session trips on, and a control put back
//! edits afresh.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::RefCell;

    use objc2::rc::{Retained, Weak, autoreleasepool};
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSBackingStoreType, NSResponder, NSTextInputClient, NSTextView, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSNotFound, NSNotification, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString};
    use sidestep_appkit::textkit::testing;

    #[derive(Default)]
    struct Log {
        events: RefCell<Vec<String>>,
    }

    define_class!(
        /// A control of its own: the text it starts with, and what the
        /// field editor tells it, as its delegate.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "FieldEditorTestControl"]
        #[ivars = Log]
        struct Control;

        unsafe impl NSObjectProtocol for Control {}

        impl Control {
            #[unsafe(method_id(stringValue))]
            fn string_value(&self) -> Retained<NSString> {
                NSString::from_str("start")
            }

            #[unsafe(method(textShouldBeginEditing:))]
            fn should_begin(&self, _t: &AnyObject) -> bool {
                self.ivars().events.borrow_mut().push("shouldBegin".into());
                true
            }

            #[unsafe(method(textDidBeginEditing:))]
            fn did_begin(&self, _n: &NSNotification) {
                self.ivars().events.borrow_mut().push("didBegin".into());
            }

            #[unsafe(method(textDidEndEditing:))]
            fn did_end(&self, _n: &NSNotification) {
                self.ivars().events.borrow_mut().push("didEnd".into());
            }
        }
    );

    fn control(mtm: MainThreadMarker) -> Retained<Control> {
        let frame = NSRect::new(NSPoint::new(10.0, 10.0), NSSize::new(200.0, 22.0));
        unsafe { msg_send![super(Control::alloc(mtm).set_ivars(Log::default())), initWithFrame: frame] }
    }

    fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
        let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(300.0, 200.0));
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, frame.size));
        w.setContentView(Some(&content));
        w
    }

    fn editor_of(c: &Control) -> Retained<NSTextView> {
        testing::current_editor(c).expect("editing").downcast().expect("a text view")
    }

    fn type_text(tv: &NSTextView, text: &str) {
        let none = NSRange::new(NSNotFound as usize, 0);
        unsafe { NSTextInputClient::insertText_replacementRange(tv, &NSString::from_str(text), none) };
    }

    fn is_first_responder(w: &NSWindow, v: &NSView) -> bool {
        w.firstResponder().is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<u8>(), (v as *const NSView).cast()))
    }

    /// The editor goes into the control, first responder, the control's
    /// text selected; the first edit begins editing.
    fn a_session(mtm: MainThreadMarker) {
        let w = window(mtm);
        let c = control(mtm);
        w.contentView().unwrap().addSubview(&c);
        testing::select_text(&c);
        let editor = editor_of(&c);
        assert!(is_first_responder(&w, &editor));
        assert_eq!(editor.string().to_string(), "start");
        assert_eq!(editor.selectedRange(), NSRange::new(0, 5));
        type_text(&editor, "x");
        assert_eq!(c.ivars().events.take(), ["shouldBegin", "didBegin"]);
        assert!(testing::abort_editing(&c));
        assert!(testing::current_editor(&c).is_none());
    }

    /// A control freed mid-edit: the next session, in another control,
    /// starts cleanly, and hears editing begin.
    fn a_control_going_mid_edit(mtm: MainThreadMarker) {
        let w = window(mtm);
        let gone = autoreleasepool(|_| {
            let c = control(mtm);
            w.contentView().unwrap().addSubview(&c);
            testing::select_text(&c);
            type_text(&editor_of(&c), "x");
            c.removeFromSuperview();
            Weak::from_retained(&c)
        });
        assert!(gone.load().is_none(), "the control is freed");
        // Views allocated now may take its memory.
        let filler: Vec<Retained<NSView>> =
            (0..8).map(|_| NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO)).collect();
        let next = control(mtm);
        w.contentView().unwrap().addSubview(&next);
        testing::select_text(&next);
        let editor = editor_of(&next);
        assert!(is_first_responder(&w, &editor));
        let clip = unsafe { editor.superview() }.expect("in the control's clip view");
        let inside = unsafe { clip.superview() }.expect("in the control");
        assert!(std::ptr::eq(&*inside, &**next as &NSView));
        type_text(&editor, "y");
        assert_eq!(next.ivars().events.take(), ["shouldBegin", "didBegin"], "editing begins afresh");
        drop(filler);
        testing::abort_editing(&next);
    }

    /// A control taken out of its window mid-edit and put back: selecting
    /// it again starts a new session.
    fn a_control_put_back(mtm: MainThreadMarker) {
        let w = window(mtm);
        let c = control(mtm);
        let content = w.contentView().unwrap();
        content.addSubview(&c);
        testing::select_text(&c);
        type_text(&editor_of(&c), "x");
        c.ivars().events.take();
        c.removeFromSuperview();
        content.addSubview(&c);
        testing::select_text(&c);
        let editor = editor_of(&c);
        assert!(is_first_responder(&w, &editor), "a new session");
        type_text(&editor, "y");
        assert_eq!(c.ivars().events.take(), ["shouldBegin", "didBegin"]);
        testing::abort_editing(&c);
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
        a_session(mtm);
        println!("test a_session ... ok");
        a_control_going_mid_edit(mtm);
        println!("test a_control_going_mid_edit ... ok");
        a_control_put_back(mtm);
        println!("test a_control_put_back ... ok");
    }
}
