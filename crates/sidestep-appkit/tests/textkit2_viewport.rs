//! A TextKit 2 text view's viewport on Linux: scrolling up into text laid
//! out only as estimates keeps what showed where it was on screen (the
//! view scrolls by what laying out the text above moved it), an edit
//! moves the text below it as it should (the view doesn't hold it in
//! place), and undoing a delete of the whole of a long text lays out what
//! shows, not all of it.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::{Cell, RefCell};
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSFont, NSScrollView, NSTextDelegate, NSTextElementProvider, NSTextInputClient, NSTextLayoutFragment,
        NSTextLayoutFragmentEnumerationOptions, NSTextView, NSTextViewDelegate,
    };
    use objc2_foundation::{
        NSDate, NSObjectProtocol, NSPoint, NSRange, NSRect, NSRunLoop, NSSize, NSString, NSUndoManager,
    };
    use sidestep_appkit as _;
    use sidestep_foundation as _;

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "TextKit2ViewportUndoing"]
        #[ivars = RefCell<Option<Retained<NSUndoManager>>>]
        struct Undoing;

        unsafe impl NSObjectProtocol for Undoing {}
        unsafe impl NSTextDelegate for Undoing {}
        unsafe impl NSTextViewDelegate for Undoing {
            #[unsafe(method_id(undoManagerForTextView:))]
            fn undo_manager_for(&self, _tv: &NSTextView) -> Option<Retained<NSUndoManager>> {
                self.ivars().borrow().clone()
            }
        }
    );

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// Paragraphs of wide letters, of narrow ones and short ones: the
    /// estimates (half an em a letter) are off, and off by different
    /// amounts, however they are scaled.
    fn uneven(n: usize) -> String {
        (0..n)
            .map(|i| {
                let body = match i % 3 {
                    0 => "MMMM ".repeat(20),
                    1 => "iiii ".repeat(20),
                    _ => "short".to_string(),
                };
                format!("{i} {body}\n")
            })
            .collect()
    }

    struct View {
        scroll: Retained<NSScrollView>,
        tv: Retained<NSTextView>,
    }

    fn view(mtm: MainThreadMarker, text: &str) -> View {
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        tv.setVerticallyResizable(true);
        tv.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        scroll.setDocumentView(Some(&tv));
        tv.setString(&NSString::from_str(text));
        tv.viewWillDraw();
        View { scroll, tv }
    }

    impl View {
        fn top(&self) -> f64 {
            self.scroll.contentView().bounds().origin.y
        }

        fn scroll_to(&self, y: f64) {
            let clip = self.scroll.contentView();
            clip.scrollToPoint(NSPoint::new(0.0, y));
            self.scroll.reflectScrolledClipView(&clip);
            self.tv.viewWillDraw();
        }

        /// Where the fragment at `offset` shows, from the top of what
        /// shows.
        fn on_screen(&self, offset: isize) -> f64 {
            let tlm = self.tv.textLayoutManager().expect("TextKit 2");
            let cm = tlm.textContentManager().expect("content");
            let at = cm.locationFromLocation_withOffset(&cm.documentRange().location(), offset).expect("a location");
            let f = tlm.textLayoutFragmentForLocation(&at).expect("a fragment");
            f.layoutFragmentFrame().origin.y - self.top()
        }

        /// The offset of the fragment showing `dy` below the top.
        fn showing(&self, dy: f64) -> isize {
            let tlm = self.tv.textLayoutManager().expect("TextKit 2");
            let cm = tlm.textContentManager().expect("content");
            let f = tlm.textLayoutFragmentForPosition(NSPoint::new(10.0, self.top() + dy)).expect("a fragment");
            cm.offsetFromLocation_toLocation(&cm.documentRange().location(), &f.rangeInElement().location())
        }
    }

    /// Scrolling up a wheel step at a time from the middle of a long text:
    /// the text that showed moves down by the step, exactly, though what
    /// comes into view above it is laid out for the first time.
    fn scrolling_up_keeps_text_in_place(mtm: MainThreadMarker) {
        let v = view(mtm, &uneven(3000));
        let middle = v.tv.frame().size.height / 2.0;
        v.scroll_to(middle);
        for _ in 0..20 {
            let o = v.showing(40.0);
            let before = v.on_screen(o);
            let top = v.top();
            v.scroll_to(top - 40.0);
            let after = v.on_screen(o);
            assert!((after - before - 40.0).abs() < 0.5, "moved {} for a 40-point step", after - before);
        }
    }

    /// An edit above what shows moves it: typing a line break in the first
    /// visible paragraph moves what follows down a line, on screen.
    fn edits_move_text(mtm: MainThreadMarker) {
        let v = view(mtm, &uneven(400));
        v.scroll_to(1000.0);
        let o = v.showing(5.0);
        let below = v.showing(120.0);
        let before = v.on_screen(below);
        v.tv.setSelectedRange(NSRange::new(o as usize + 1, 0));
        let nl = NSString::from_str("\n");
        unsafe { v.tv.insertText_replacementRange(&nl, NSRange::new(isize::MAX as usize, 0)) };
        v.tv.viewWillDraw();
        let after = v.on_screen(below + 1);
        assert!(after > before + 5.0, "{before} -> {after}");
    }

    /// Fragments laid out, of all of them.
    fn laid(tv: &NSTextView) -> (usize, usize) {
        let tlm = tv.textLayoutManager().expect("TextKit 2");
        let (laid, all) = (Cell::new(0), Cell::new(0));
        let block = block2::RcBlock::new(|f: NonNull<NSTextLayoutFragment>| -> Bool {
            all.set(all.get() + 1);
            if unsafe { f.as_ref() }.state().0 == 3 {
                laid.set(laid.get() + 1);
            }
            Bool::YES
        });
        tlm.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
            None,
            NSTextLayoutFragmentEnumerationOptions::None,
            &block,
        );
        (laid.get(), all.get())
    }

    /// Selecting all of a long text, deleting it and undoing: the text
    /// comes back selected and the view scrolls to it, laying out the
    /// fragments that show and not the rest.
    fn undoing_a_long_delete_lays_out_what_shows(mtm: MainThreadMarker) {
        let text = uneven(3000);
        let v = view(mtm, &text);
        let um = NSUndoManager::new(mtm);
        let d: Retained<Undoing> =
            unsafe { msg_send![super(Undoing::alloc(mtm).set_ivars(RefCell::new(Some(um.clone())))), init] };
        v.tv.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        v.tv.setAllowsUndo(true);
        unsafe { v.tv.selectAll(None) };
        unsafe { NSTextInputClient::doCommandBySelector(&*v.tv, sel!(deleteBackward:)) };
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
        assert_eq!(v.tv.string().length(), 0);
        um.undo();
        assert_eq!(v.tv.string().to_string(), text);
        v.tv.viewWillDraw();
        let (laid, all) = laid(&v.tv);
        assert_eq!(all, 3000);
        assert!(laid < 100, "{laid} of {all} laid out");
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
        type Test = (&'static str, fn(MainThreadMarker));
        let tests: &[Test] = &[
            ("scrolling_up_keeps_text_in_place", scrolling_up_keeps_text_in_place),
            ("edits_move_text", edits_move_text),
            ("undoing_a_long_delete_lays_out_what_shows", undoing_a_long_delete_lays_out_what_shows),
        ];
        for (name, test) in tests {
            test(mtm);
            println!("test {name} ... ok");
        }
    }
}
