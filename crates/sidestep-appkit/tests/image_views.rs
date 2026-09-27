//! Image views on Linux, driven without a compositor: image files dropped
//! on an editable image view (through the drag testing hooks), images cut,
//! copied and pasted, and an animated GIF stepping through its frames, in
//! a window and out of one, on the null render thread.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSBitmapImageRep, NSImage, NSImageView, NSMenuItem, NSPasteboard, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};
    use sidestep_appkit::drag::testing::{self, DND_COPY, Reply};
    use sidestep_appkit::testing as app_testing;

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxImageViewTarget"]
        struct Target;

        impl Target {
            #[unsafe(method(changed:))]
            fn changed(&self, sender: &NSImageView) {
                let size = sender.image().map(|i| i.size());
                LOG.with(|l| l.borrow_mut().push(format!("changed: {size:?}")));
            }
        }

        unsafe impl NSObjectProtocol for Target {}
    );

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn fixture(name: &str) -> String {
        format!("{}/../../conformance/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn image(name: &str) -> Retained<NSImage> {
        NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&fixture(name))).expect("an image")
    }

    struct Scene {
        window: Retained<NSWindow>,
        view: Retained<NSImageView>,
        _target: Retained<Target>,
    }

    /// A 200 by 100 window, never shown, with an image view on its left
    /// half that sends `changed:` to a target.
    fn scene(mtm: MainThreadMarker) -> Scene {
        // SAFETY: a titled window, never shown.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 200.0, 100.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        // SAFETY: the test keeps its reference.
        unsafe { window.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
        window.setContentView(Some(&content));
        let view = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
        content.addSubview(&view);
        // SAFETY: NSObject's initializer.
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        // SAFETY: the scene keeps the target as long as the view.
        unsafe {
            view.setTarget(Some(&target));
            view.setAction(Some(sel!(changed:)));
        }
        Scene { window, view, _target: target }
    }

    fn file_url(name: &str) -> String {
        format!("file://{}\r\n", fixture(name))
    }

    /// Drop the files `urls` over the image view.
    fn drop_files(s: &Scene, urls: &str) -> Vec<Reply> {
        testing::enter_with_urls(&s.window, 50.0, 50.0, &["text/uri-list"], DND_COPY, Some(urls));
        testing::drop();
        testing::take_replies()
    }

    fn drops(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // Not editable: it takes no drop.
        let replies = drop_files(&s, &file_url("halves.png"));
        assert_eq!(replies.last(), Some(&Reply::Finish { performed: false }));
        assert!(s.view.image().is_none());
        assert!(take_log().is_empty());
        // Editable: an image file dropped on it becomes its image, and it
        // sends its action.
        s.view.setEditable(true);
        let replies = drop_files(&s, &file_url("halves.png"));
        assert_eq!(replies.last(), Some(&Reply::Finish { performed: true }), "{replies:?}");
        assert_eq!(s.view.image().map(|i| i.size()), Some(NSSize::new(4.0, 4.0)));
        assert_eq!(take_log(), ["changed: Some(CGSize { width: 4.0, height: 4.0 })"]);
        // A file that isn't an image is refused.
        let replies = drop_files(&s, "file:///etc/hostname\r\n");
        assert_eq!(replies.last(), Some(&Reply::Finish { performed: false }));
        assert_eq!(s.view.image().map(|i| i.size()), Some(NSSize::new(4.0, 4.0)));
        assert!(take_log().is_empty());
        // Disabled, it still takes one: being editable is what counts, as
        // on macOS.
        s.view.setEnabled(false);
        let replies = drop_files(&s, &file_url("small.gif"));
        assert_eq!(replies.last(), Some(&Reply::Finish { performed: true }), "{replies:?}");
        assert_eq!(s.view.image().map(|i| i.size()), Some(NSSize::new(3.0, 2.0)));
        assert_eq!(take_log(), ["changed: Some(CGSize { width: 3.0, height: 2.0 })"]);
        s.window.close();
    }

    fn validates(view: &NSImageView, action: objc2::runtime::Sel) -> bool {
        let item = NSMenuItem::new(MainThreadMarker::from(view));
        // SAFETY: any selector may be an action.
        unsafe { item.setAction(Some(action)) };
        // SAFETY: validateMenuItem: takes a menu item and returns BOOL.
        unsafe { msg_send![view, validateMenuItem: &*item] }
    }

    fn cut_copy_paste(mtm: MainThreadMarker) {
        let s = scene(mtm);
        let other = scene(mtm);
        let none: Option<&AnyObject> = None;
        s.view.setImage(Some(&image("halves.png")));
        // Copying puts the image on the general pasteboard, as a TIFF.
        assert!(validates(&s.view, sel!(copy:)));
        // SAFETY: copy: takes a sender.
        let _: () = unsafe { msg_send![&*s.view, copy: none] };
        let board = NSPasteboard::generalPasteboard();
        let types: Vec<String> = board.types().map(|t| t.iter().map(|t| t.to_string()).collect()).unwrap_or_default();
        assert!(types.iter().any(|t| t == "public.tiff"), "{types:?}");
        // Pasting takes it, when editable.
        assert!(!validates(&other.view, sel!(paste:)));
        // SAFETY: paste: takes a sender.
        let _: () = unsafe { msg_send![&*other.view, paste: none] };
        assert!(other.view.image().is_none());
        other.view.setEditable(true);
        assert!(validates(&other.view, sel!(paste:)));
        // SAFETY: as above.
        let _: () = unsafe { msg_send![&*other.view, paste: none] };
        assert_eq!(other.view.image().map(|i| i.size()), Some(NSSize::new(4.0, 4.0)));
        assert_eq!(take_log(), ["changed: Some(CGSize { width: 4.0, height: 4.0 })"]);
        // Cutting copies and clears; a view that doesn't allow cut, copy
        // and paste leaves the image, and doesn't delete it either.
        other.view.setAllowsCutCopyPaste(false);
        assert!(!validates(&other.view, sel!(cut:)) && !validates(&other.view, sel!(delete:)));
        // SAFETY: cut: and delete: take a sender.
        let _: () = unsafe { msg_send![&*other.view, cut: none] };
        // SAFETY: as above.
        let _: () = unsafe { msg_send![&*other.view, delete: none] };
        assert!(other.view.image().is_some());
        assert!(take_log().is_empty());
        other.view.setAllowsCutCopyPaste(true);
        // SAFETY: as above.
        let _: () = unsafe { msg_send![&*other.view, cut: none] };
        assert!(other.view.image().is_none());
        assert_eq!(take_log(), ["changed: None"]);
        assert!(!validates(&other.view, sel!(cut:)) && !validates(&other.view, sel!(copy:)));
        s.window.close();
        other.window.close();
    }

    fn current_frame(image: &NSImage) -> isize {
        let rep = image.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
        // SAFETY: the key is a property name.
        let value = rep.valueForProperty(&NSString::from_str("NSImageCurrentFrame")).expect("a frame");
        // SAFETY: the frame is a number.
        unsafe { msg_send![&*value, integerValue] }
    }

    /// Run the main loop until `done`, or fail after `seconds`.
    fn run_until(seconds: u64, mut done: impl FnMut() -> bool) {
        let give_up = Instant::now() + Duration::from_secs(seconds);
        while !done() {
            assert!(Instant::now() < give_up, "timed out");
            app_testing::run_for(10);
        }
    }

    fn animation(mtm: MainThreadMarker) {
        let s = scene(mtm);
        let gif = image("frames.gif");
        s.view.setImage(Some(&gif));
        assert_eq!(current_frame(&gif), 0);
        // In a window, it steps through the frames and around again.
        let mut seen = vec![0];
        run_until(10, || {
            let f = current_frame(&gif);
            if seen.last() != Some(&f) {
                seen.push(f);
            }
            seen.len() >= 4
        });
        assert_eq!(seen, [0, 1, 2, 0]);
        // Not animating, it stays on its frame.
        s.view.setAnimates(false);
        let now = current_frame(&gif);
        app_testing::run_for(500);
        assert_eq!(current_frame(&gif), now);
        // Animating again, it moves on; out of the window too, as on macOS,
        // until it's without the image.
        s.view.setAnimates(true);
        run_until(10, || current_frame(&gif) != now);
        s.view.removeFromSuperview();
        let now = current_frame(&gif);
        run_until(10, || current_frame(&gif) != now);
        s.view.setImage(None);
        let now = current_frame(&gif);
        app_testing::run_for(500);
        assert_eq!(current_frame(&gif), now);
        // A bitmap told a frame it doesn't have keeps its own.
        let rep = gif.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
        let n = NSNumber::new_isize(7);
        // SAFETY: the key is a property name and the value a number.
        unsafe { rep.setProperty_withValue(&NSString::from_str("NSImageCurrentFrame"), Some(&n)) };
        assert_eq!(current_frame(&gif), now);
        s.window.close();
    }

    type Test = (&'static str, fn(MainThreadMarker));

    pub(crate) fn run() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        app_testing::use_null_backend();
        let _app = NSApplication::sharedApplication(mtm);
        testing::capture_replies();
        let tests: &[Test] = &[("drops", drops), ("cut_copy_paste", cut_copy_paste), ("animation", animation)];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
