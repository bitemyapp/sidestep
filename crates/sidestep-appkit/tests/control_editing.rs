//! Live field editing and transparent popup drawing through the public
//! AppKit methods. Runs on the main thread without a desktop connection.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSBackingStoreType, NSBitmapFormat, NSBitmapImageRep, NSCell, NSDeviceRGBColorSpace, NSGraphicsContext,
        NSPopUpButton, NSTextField, NSTextInputClient, NSTextView, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString};

    fn rect(w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::ZERO, NSSize::new(w, h))
    }

    fn field(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<NSTextField>) {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(300.0, 100.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(300.0, 100.0));
        window.setContentView(Some(&content));
        let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(240.0, 30.0));
        field.setBezeled(false);
        field.setDrawsBackground(false);
        field.setEditable(true);
        field.setPlaceholderString(Some(&NSString::from_str("Placeholder")));
        content.addSubview(&field);
        (window, field)
    }

    fn start(field: &NSTextField) -> Retained<NSTextView> {
        unsafe { field.selectText(None) };
        field.currentEditor().expect("active editor").downcast().expect("text view")
    }

    fn type_text(editor: &NSTextView, text: &str) {
        unsafe {
            NSTextInputClient::insertText_replacementRange(
                editor,
                &NSString::from_str(text),
                NSRange::new(NSNotFound as usize, 0),
            )
        };
    }

    /// The getter validates the active editor; the setter aborts it.
    fn live_values(mtm: MainThreadMarker) {
        let (_window, field) = field(mtm);
        let editor = start(&field);
        type_text(&editor, "Typed prompt");
        assert_eq!(editor.string().to_string(), "Typed prompt");
        assert_eq!(field.stringValue().to_string(), "Typed prompt", "read the active editor");
        assert_eq!(field.cell().unwrap().stringValue().to_string(), "Typed prompt", "validate the cell");
        type_text(&editor, " uncommitted");
        field.setStringValue(&NSString::from_str("Replacement"));
        assert!(field.currentEditor().is_none(), "setting a value ends editing");
        assert_eq!(field.stringValue().to_string(), "Replacement");
        let editor = start(&field);
        assert_eq!(editor.string().to_string(), "Replacement");
        type_text(&editor, "Discard this");
        assert!(field.abortEditing());
        assert_eq!(field.stringValue().to_string(), "Replacement", "abort keeps the last validated value");
    }

    /// Draw a cell alone, so any ink is its own, not the editor's.
    fn ink(cell: &NSCell, view: &NSView, interior: bool) -> usize {
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), 240, 30, 8, 4,
                true, false, NSDeviceRGBColorSpace, NSBitmapFormat::empty(), 0, 32,
            )
        }.expect("bitmap");
        let len = rep.bytesPerRow() as usize * rep.pixelsHigh() as usize;
        unsafe { std::ptr::write_bytes(rep.bitmapData(), 0, len) };
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).unwrap();
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        if interior {
            cell.drawInteriorWithFrame_inView(view.bounds(), view);
        } else {
            cell.drawWithFrame_inView(view.bounds(), view);
        }
        context.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let pixels = unsafe { std::slice::from_raw_parts(rep.bitmapData(), len) };
        pixels.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count()
    }

    fn editor_owns_text(mtm: MainThreadMarker) {
        for initial in ["", "Old text"] {
            let (_window, field) = field(mtm);
            field.setStringValue(&NSString::from_str(initial));
            let cell = field.cell().unwrap();
            assert!(ink(&cell, &field, true) > 0, "idle field draws its value or placeholder");
            let editor = start(&field);
            type_text(&editor, "New text");
            assert_eq!(ink(&cell, &field, true), 0, "the active editor owns text drawing");
            field.setBezeled(true);
            assert!(ink(&cell, &field, false) > 0, "editing still draws the field frame");
            field.setBezeled(false);
            assert!(field.abortEditing());
            assert!(ink(&cell, &field, true) > 0, "ending editing restores cell drawing");
        }
    }

    fn transparent_popup(mtm: MainThreadMarker) {
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(240.0, 30.0), false);
        popup.addItemWithTitle(&NSString::from_str("Selected item"));
        let cell = popup.cell().unwrap();
        assert!(ink(&cell, &popup, false) > 0);
        popup.setTransparent(true);
        assert_eq!(ink(&cell, &popup, false), 0, "transparent popup draws no title, arrows or bezel");
        assert_eq!(popup.titleOfSelectedItem().unwrap().to_string(), "Selected item");
        popup.setTransparent(false);
        assert!(ink(&cell, &popup, false) > 0);
    }

    pub fn run() {
        sidestep_appkit::testing::use_null_backend();
        let mtm = MainThreadMarker::new().expect("main thread");
        let mut failed = false;
        for (name, test) in [
            ("live_values", live_values as fn(MainThreadMarker)),
            ("editor_owns_text", editor_owns_text),
            ("transparent_popup", transparent_popup),
        ] {
            if std::panic::catch_unwind(|| test(mtm)).is_ok() {
                println!("test {name} ... ok");
            } else {
                failed = true;
                println!("test {name} ... FAILED");
            }
        }
        assert!(!failed, "control editing regressions");
    }
}
