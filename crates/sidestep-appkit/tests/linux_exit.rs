//! A program that returns from `main` with windows on screen exits cleanly:
//! its windows aren't torn down as the main thread's thread-locals go, which
//! would run their views' code against the ones already gone (a scroll
//! layer telling the render thread it left, after the connection to it
//! went). On macOS, a program that exits doesn't deallocate its windows
//! either.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSBackingStoreType, NSScrollView, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use sidestep_appkit::testing;

    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    testing::use_null_backend();
    let size = NSSize::new(400.0, 300.0);
    // SAFETY: a plain window, shown by the null render thread.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, size),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // A scroll view big enough for a layer of its own.
    let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::ZERO, size));
    let document = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(400.0, 5000.0)));
    sv.setDocumentView(Some(&document));
    w.setContentView(Some(&sv));
    w.makeKeyAndOrderFront(None);
    testing::settle();
    // The first frame waits a moment for the desktop's light or dark, which
    // the null render thread never tells.
    testing::run_for(200);
    testing::settle();
    assert_eq!(testing::scroll_layers(&w).len(), 1, "a scroll layer, which tells the render thread when it goes");
    println!("test exit_with_windows_on_screen ... ok");
}
