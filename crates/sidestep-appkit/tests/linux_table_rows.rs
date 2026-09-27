//! Table rows on Linux, through the null render thread: a selected row's
//! labels are painted light while the table has the key window's focus,
//! and in the label color again once the window loses the keyboard or the
//! table the focus. What Apple's AppKit does is pinned by
//! `conformance/tests/cell_backgrounds.rs` (which cells are told what, and
//! how they draw on each style); this covers what only a shown window's
//! display pass does with it.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSApplication, NSBackgroundStyle, NSBackingStoreType,
        NSControlTextEditingDelegate, NSScrollView, NSTableCellView, NSTableColumn, NSTableView, NSTableViewDataSource,
        NSTableViewDelegate, NSTableViewStyle, NSTextField, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSIndexSet, NSPoint, NSRect, NSSize, NSString};
    use sidestep_appkit::testing::{self, PaintedText};

    const ROWS: isize = 5;
    const ROW_HEIGHT: f64 = 24.0;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    define_class!(
        /// Rows of a cell view holding a label, the usual way.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxTableRowsSource"]
        struct Source;

        unsafe impl NSObjectProtocol for Source {}

        unsafe impl NSTableViewDataSource for Source {
            #[unsafe(method(numberOfRowsInTableView:))]
            fn number_of_rows(&self, _table: &NSTableView) -> isize {
                ROWS
            }
        }

        unsafe impl NSControlTextEditingDelegate for Source {}

        unsafe impl NSTableViewDelegate for Source {
            #[unsafe(method_id(tableView:viewForTableColumn:row:))]
            fn view_for(
                &self,
                table: &NSTableView,
                _column: Option<&NSTableColumn>,
                row: isize,
            ) -> Option<Retained<NSView>> {
                let mtm = MainThreadMarker::from(table);
                let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), rect(0.0, 0.0, 200.0, 24.0));
                let label = NSTextField::labelWithString(&NSString::from_str(&format!("Row {row}")), mtm);
                label.setFrame(rect(2.0, 4.0, 190.0, 16.0));
                cell.addSubview(&label);
                // SAFETY: the outlet is a subview, which outlives its use.
                unsafe { cell.setTextField(Some(&label)) };
                Some(Retained::into_super(cell))
            }
        }
    );

    /// A plain table of five labeled rows in a key window, settled, its
    /// text noted from the first frame on.
    fn shown_table(mtm: MainThreadMarker) -> (Retained<NSWindow>, u32, Retained<NSTableView>, Retained<Source>) {
        let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        t.setStyle(NSTableViewStyle::Plain);
        t.setHeaderView(None);
        t.setRowHeight(ROW_HEIGHT);
        t.setIntercellSpacing(NSSize::new(0.0, 0.0));
        let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("a"));
        c.setWidth(200.0);
        t.addTableColumn(&c);
        // SAFETY: the superclass's designated initializer.
        let src: Retained<Source> = unsafe { msg_send![Source::alloc(mtm), init] };
        // SAFETY: the table doesn't retain these; they outlive its use of them.
        unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*src))) };
        // SAFETY: as above.
        unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*src))) };
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        scroll.setDocumentView(Some(&t));
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        // SAFETY: a plain window.
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 300.0, 200.0),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { w.setReleasedWhenClosed(false) };
        w.setContentView(Some(&scroll));
        t.reloadData();
        t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
        w.makeFirstResponder(Some(&t));
        testing::note_painted_text(true);
        w.makeKeyAndOrderFront(None);
        testing::settle_first_frames();
        assert!(w.isKeyWindow());
        // However many frames showing it took, the tests read one frame:
        // everything drawn again once.
        testing::take_painted_text();
        t.setNeedsDisplayInRect(t.bounds());
        testing::settle();
        let id = testing::showing_id(&w);
        (w, id, t, src)
    }

    /// Light: the text color for selections (white on the accent).
    fn light(c: [f32; 4]) -> bool {
        c[..3].iter().all(|&v| v > 0.9) && c[3] > 0.9
    }

    /// The label color, in the light appearance.
    fn dark(c: [f32; 4]) -> bool {
        c[..3].iter().all(|&v| v < 0.2) && c[3] > 0.5
    }

    /// The row the text painted at `y` (in the table's points) is on.
    fn row_of(t: &PaintedText) -> isize {
        (f64::from(t.y) / ROW_HEIGHT).floor() as isize
    }

    /// The colors of the text painted since the last call, by row.
    fn painted() -> Vec<(isize, [f32; 4])> {
        let mut out: Vec<(isize, [f32; 4])> =
            testing::take_painted_text().iter().map(|t| (row_of(t), t.color)).collect();
        out.sort_by_key(|(r, _)| *r);
        out
    }

    fn cell_style(t: &NSTableView, row: isize) -> NSBackgroundStyle {
        let cell = t.viewAtColumn_row_makeIfNecessary(0, row, false).expect("a cell view");
        let cell: &NSTableCellView = cell.downcast_ref().expect("a cell view");
        // SAFETY: the outlet is set.
        let label = unsafe { cell.textField() }.expect("a label");
        label.cell().expect("a cell").backgroundStyle()
    }

    fn selected_labels_turn_light_while_key(mtm: MainThreadMarker) {
        let (w, id, t, _src) = shown_table(mtm);
        // The first frame: row 1's label light, the others in the label
        // color.
        let first = painted();
        let rows: Vec<isize> = first.iter().map(|(r, _)| *r).collect();
        assert_eq!(rows, (0..ROWS).collect::<Vec<_>>(), "{first:?}");
        for (r, color) in &first {
            if *r == 1 {
                assert!(light(*color), "row 1 on the selection: {color:?}");
            } else {
                assert!(dark(*color), "row {r}: {color:?}");
            }
        }
        assert_eq!(cell_style(&t, 1), NSBackgroundStyle::Emphasized);
        assert_eq!(cell_style(&t, 2), NSBackgroundStyle::Normal);

        // The window loses the keyboard: the selection stays, weakly, and
        // its label is painted in the label color again.
        testing::inject_focus(id, false);
        testing::settle();
        assert!(!w.isKeyWindow());
        let row = t.rowViewAtRow_makeIfNecessary(1, false).expect("row 1");
        assert!(row.isSelected() && !row.isEmphasized());
        let again = painted();
        let on_row: Vec<[f32; 4]> = again.iter().filter(|(r, _)| *r == 1).map(|(_, c)| *c).collect();
        assert!(!on_row.is_empty() && on_row.iter().all(|c| dark(*c)), "{again:?}");
        assert_eq!(cell_style(&t, 1), NSBackgroundStyle::Normal);

        // And back.
        testing::inject_focus(id, true);
        testing::settle();
        assert!(row.isEmphasized());
        let back = painted();
        let on_row: Vec<[f32; 4]> = back.iter().filter(|(r, _)| *r == 1).map(|(_, c)| *c).collect();
        assert!(!on_row.is_empty() && on_row.iter().all(|c| light(*c)), "{back:?}");

        // The table gives up the focus: the same.
        w.makeFirstResponder(None);
        testing::settle();
        assert!(!row.isEmphasized());
        let unfocused = painted();
        let on_row: Vec<[f32; 4]> = unfocused.iter().filter(|(r, _)| *r == 1).map(|(_, c)| *c).collect();
        assert!(!on_row.is_empty() && on_row.iter().all(|c| dark(*c)), "{unfocused:?}");

        // Another row selected while focused: the light moves with it.
        w.makeFirstResponder(Some(&t));
        t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(3), false);
        testing::settle();
        let moved = painted();
        for (r, color) in &moved {
            match r {
                3 => assert!(light(*color), "row 3: {color:?}"),
                1 => assert!(dark(*color), "row 1: {color:?}"),
                _ => {}
            }
        }
        assert!(moved.iter().any(|(r, _)| *r == 3) && moved.iter().any(|(r, _)| *r == 1), "{moved:?}");

        testing::note_painted_text(false);
        w.close();
        // SAFETY: the table doesn't retain these; they outlive its use of them.
        unsafe { t.setDelegate(None) };
        // SAFETY: as above.
        unsafe { t.setDataSource(None) };
        testing::settle();
    }

    type Test = (&'static str, fn(MainThreadMarker));

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        // Light, whatever the desktop prefers.
        // SAFETY: the name is a constant string.
        let aqua = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).expect("Aqua");
        NSApplication::sharedApplication(mtm).setAppearance(Some(&aqua));
        let tests: &[Test] = &[("selected_labels_turn_light_while_key", selected_labels_turn_light_while_key)];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
