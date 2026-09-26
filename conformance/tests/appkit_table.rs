//! A view-based NSTableView, checked on macOS and on Linux alike: defaults,
//! the plain style's geometry (rows, columns, cells, hit testing) and the
//! other styles', views made only for rows near the visible ones and
//! reused by identifier, scrolling to rows, selection and its
//! notification, the selection following rows that come and go, row
//! updates, heights from the delegate (asked only for new rows), column
//! notifications, sort descriptors, cells' background styles, delegates
//! asking the table things as rows go, and columns and headers that
//! outlive their table. On Linux, clicking rows too.
//!
//! Windows are never shown. AppKit belongs to the main thread, so this
//! file has its own `main`.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackgroundStyle, NSBackingStoreType, NSControlTextEditingDelegate, NSLayoutConstraint, NSResponder, NSScrollView,
    NSTableCellView, NSTableColumn, NSTableColumnResizingOptions, NSTableRowView, NSTableView,
    NSTableViewAnimationOptions, NSTableViewColumnAutoresizingStyle, NSTableViewDataSource, NSTableViewDelegate,
    NSTableViewGridLineStyle, NSTableViewRowSizeStyle, NSTableViewSelectionHighlightStyle, NSTableViewStyle,
    NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSNotification, NSNumber, NSPoint, NSRange, NSRect, NSSize, NSSortDescriptor, NSString,
};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

define_class!(
    /// A cell view that notes its reuse.
    #[unsafe(super(NSTableCellView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTableCell"]
    struct Cellv;

    impl Cellv {
        #[unsafe(method(prepareForReuse))]
        fn prepare_for_reuse(&self) {
            log("prepareForReuse".into());
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), prepareForReuse] }
        }
    }

    unsafe impl NSObjectProtocol for Cellv {}
);

struct SourceIvars {
    rows: Cell<isize>,
    made: Cell<usize>,
    heights_of: RefCell<Vec<f64>>,
    /// Ask the table where a row view is as it goes.
    ask_on_remove: Cell<bool>,
}

define_class!(
    /// The data source and delegate: rows of cell views, one kind per
    /// column, logged.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTableSource"]
    #[ivars = SourceIvars]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSTableViewDataSource for Source {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.get()
        }

        #[unsafe(method(tableView:sortDescriptorsDidChange:))]
        fn sort_descriptors_did_change(&self, _table: &NSTableView, old: Option<&NSArray<NSSortDescriptor>>) {
            match old {
                Some(old) => log(format!("sortDescriptorsDidChange {}", old.count())),
                None => log("sortDescriptorsDidChange nil".into()),
            }
        }
    }

    unsafe impl NSControlTextEditingDelegate for Source {}

    unsafe impl NSTableViewDelegate for Source {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<NSView>> {
            let id = column.map(|c| c.identifier()).unwrap_or_else(|| NSString::from_str("group"));
            // SAFETY: there is no owner to connect outlets to.
            let reused = unsafe { table.makeViewWithIdentifier_owner(&id, None) };
            let view = reused.unwrap_or_else(|| {
                self.ivars().made.set(self.ivars().made.get() + 1);
                let mtm = MainThreadMarker::from(table);
                // SAFETY: the class's initializer.
                let cell: Retained<Cellv> = unsafe { msg_send![Cellv::alloc(mtm), initWithFrame: NSRect::ZERO] };
                let view: Retained<NSView> = Retained::into_super(Retained::into_super(cell));
                view.setIdentifier(Some(&id));
                view
            });
            log(format!("viewFor {id} {row}"));
            Some(view)
        }

        #[unsafe(method(tableView:didAddRowView:forRow:))]
        fn did_add(&self, _table: &NSTableView, _row_view: &NSTableRowView, row: isize) {
            log(format!("didAdd {row}"));
        }

        #[unsafe(method(tableView:didRemoveRowView:forRow:))]
        fn did_remove(&self, table: &NSTableView, row_view: &NSTableRowView, row: isize) {
            log(format!("didRemove {row}"));
            if self.ivars().ask_on_remove.get() {
                let _ = (table.rowForView(row_view), table.rowViewAtRow_makeIfNecessary(0, false));
            }
        }

        #[unsafe(method(tableViewColumnDidResize:))]
        fn column_did_resize(&self, note: &NSNotification) {
            let info = note.userInfo().expect("user info");
            let old = info.objectForKey(ns("NSOldWidth")).expect("the old width");
            let old = old.downcast_ref::<NSNumber>().expect("a number").doubleValue();
            let column = info.objectForKey(ns("NSTableColumn")).expect("the column");
            let column = column.downcast_ref::<NSTableColumn>().expect("a column").identifier();
            log(format!("columnDidResize {column} {old}"));
        }

        #[unsafe(method(tableViewColumnDidMove:))]
        fn column_did_move(&self, note: &NSNotification) {
            let info = note.userInfo().expect("user info");
            let get =
                |key| info.objectForKey(ns(key)).and_then(|n| n.downcast_ref::<NSNumber>().map(|n| n.integerValue()));
            log(format!("columnDidMove {:?} {:?}", get("NSOldColumn"), get("NSNewColumn")));
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select(&self, _table: &NSTableView, row: isize) -> bool {
            log(format!("shouldSelect {row}"));
            row != 7
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, note: &NSNotification) {
            log(format!("selectionDidChange {}", note.name()));
        }
    }
);

define_class!(
    /// Rows 10, 11, 12… points tall.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTableHeights"]
    #[ivars = SourceIvars]
    struct Heights;

    unsafe impl NSObjectProtocol for Heights {}

    unsafe impl NSTableViewDataSource for Heights {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.get()
        }
    }

    unsafe impl NSControlTextEditingDelegate for Heights {}

    unsafe impl NSTableViewDelegate for Heights {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            _row: isize,
        ) -> Option<Retained<NSView>> {
            Some(NSView::initWithFrame(NSView::alloc(MainThreadMarker::from(table)), NSRect::ZERO))
        }

        #[unsafe(method(tableView:heightOfRow:))]
        fn height_of_row(&self, _table: &NSTableView, row: isize) -> f64 {
            log(format!("heightOf {row}"));
            self.ivars().heights_of.borrow()[row as usize]
        }
    }
);

fn ns(s: &str) -> &AnyObject {
    let s: &'static NSString = Box::leak(Box::new(NSString::from_str(s)));
    s
}

fn source(mtm: MainThreadMarker, rows: isize) -> Retained<Source> {
    let this = Source::alloc(mtm).set_ivars(SourceIvars {
        rows: Cell::new(rows),
        made: Cell::new(0),
        heights_of: RefCell::default(),
        ask_on_remove: Cell::new(false),
    });
    // SAFETY: the superclass's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn column(mtm: MainThreadMarker, id: &str, width: f64) -> Retained<NSTableColumn> {
    let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str(id));
    c.setWidth(width);
    c
}

/// A plain-style table with columns "a" (100) and "b" (150).
fn table(mtm: MainThreadMarker) -> Retained<NSTableView> {
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    t.setStyle(NSTableViewStyle::Plain);
    t.addTableColumn(&column(mtm, "a", 100.0));
    t.addTableColumn(&column(mtm, "b", 150.0));
    t
}

fn realized(t: &NSTableView, rows: isize) -> Vec<isize> {
    (0..rows).filter(|&r| t.rowViewAtRow_makeIfNecessary(r, false).is_some()).collect()
}

fn indexes(set: &NSIndexSet) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = set.firstIndex();
    while i != usize::MAX && i as isize != isize::MAX {
        out.push(i);
        i = set.indexGreaterThanIndex(i);
    }
    out
}

/// A table of 100 rows 22 points apart in a scroll view 300 by 200, in a
/// window.
fn scrolling_table(
    mtm: MainThreadMarker,
) -> (Retained<NSWindow>, Retained<NSScrollView>, Retained<NSTableView>, Retained<Source>) {
    let t = table(mtm);
    t.setHeaderView(None);
    t.setIntercellSpacing(NSSize::new(4.0, 2.0));
    t.setRowHeight(20.0);
    let src = source(mtm, 100);
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*src))) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*src))) };
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    scroll.setDocumentView(Some(&t));
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 300.0, 200.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    w.setContentView(Some(&scroll));
    t.reloadData();
    (w, scroll, t, src)
}

fn close(w: &NSWindow, t: &NSTableView) {
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(None) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(None) };
    w.setContentView(None);
}

fn defaults(mtm: MainThreadMarker) {
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    assert_eq!(t.rowHeight(), 24.0);
    assert_eq!(t.intercellSpacing(), NSSize::new(17.0, 0.0));
    assert_eq!(t.gridStyleMask(), NSTableViewGridLineStyle::empty());
    assert!(!t.usesAlternatingRowBackgroundColors() && !t.allowsMultipleSelection() && !t.allowsColumnSelection());
    assert!(t.allowsEmptySelection() && t.allowsTypeSelect() && t.isFlipped());
    assert!(t.headerView().is_some());
    assert_eq!((t.numberOfRows(), t.numberOfColumns(), t.selectedRow()), (0, 0, -1));
    assert_eq!(t.style(), NSTableViewStyle::Automatic);
    assert_eq!(t.effectiveStyle(), NSTableViewStyle::Inset);
    assert_eq!(t.rowSizeStyle(), NSTableViewRowSizeStyle::Custom);
    assert_eq!(t.columnAutoresizingStyle(), NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle);
    assert_eq!(t.selectionHighlightStyle(), NSTableViewSelectionHighlightStyle::Regular);
    assert_eq!((t.clickedRow(), t.clickedColumn(), t.editedRow()), (-1, -1, -1));
    assert!(t.doubleAction().is_none());

    let c = column(mtm, "a", 100.0);
    assert_eq!((c.minWidth(), c.maxWidth()), (10.0, f32::MAX as f64));
    assert_eq!(c.title().to_string(), "Field");
    assert_eq!(
        c.resizingMask(),
        NSTableColumnResizingOptions::AutoresizingMask | NSTableColumnResizingOptions::UserResizingMask
    );
    assert!(!c.isHidden() && c.isEditable() && c.tableView().is_none());
    t.addTableColumn(&c);
    assert!(c.tableView().is_some_and(|x| std::ptr::eq(&*x, &*t)));
    assert_eq!(t.columnWithIdentifier(&NSString::from_str("a")), 0);
    assert_eq!(t.columnWithIdentifier(&NSString::from_str("zz")), -1);
    assert!(t.tableColumnWithIdentifier(&NSString::from_str("a")).is_some_and(|x| std::ptr::eq(&*x, &*c)));
    t.removeTableColumn(&c);
    assert!(c.tableView().is_none() && t.numberOfColumns() == 0);
}

fn geometry(mtm: MainThreadMarker) {
    let t = table(mtm);
    let src = source(mtm, 10);
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*src))) };
    t.reloadData();
    // Columns as wide as their width and the spacing; rows as tall as
    // their height and the spacing; cells in by half the spacing.
    assert_eq!(t.numberOfRows(), 10);
    assert_eq!(t.frame(), rect(0.0, 0.0, 284.0, 240.0));
    assert_eq!((t.rectOfRow(0), t.rectOfRow(1)), (rect(0.0, 0.0, 284.0, 24.0), rect(0.0, 24.0, 284.0, 24.0)));
    assert_eq!((t.rectOfColumn(0), t.rectOfColumn(1)), (rect(0.0, 0.0, 117.0, 240.0), rect(117.0, 0.0, 167.0, 240.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 1), rect(8.0, 24.0, 100.0, 24.0));
    assert_eq!(t.frameOfCellAtColumn_row(1, 1), rect(125.0, 24.0, 150.0, 24.0));
    assert_eq!((t.rowAtPoint(NSPoint::new(5.0, 23.9)), t.rowAtPoint(NSPoint::new(5.0, 24.0))), (0, 1));
    assert_eq!((t.rowAtPoint(NSPoint::new(5.0, -1.0)), t.rowAtPoint(NSPoint::new(5.0, 5000.0))), (-1, -1));
    assert_eq!((t.columnAtPoint(NSPoint::new(150.0, 5.0)), t.columnAtPoint(NSPoint::new(400.0, 5.0))), (1, -1));
    let rows = t.rowsInRect(rect(0.0, 30.0, 10.0, 40.0));
    assert_eq!((rows.location, rows.length), (1, 2));
    assert_eq!(indexes(&t.columnIndexesInRect(rect(90.0, 0.0, 40.0, 5.0))), [0, 1]);
    // New spacing and height take effect at once.
    t.setIntercellSpacing(NSSize::new(4.0, 2.0));
    t.setRowHeight(20.0);
    assert_eq!(t.frame(), rect(0.0, 0.0, 258.0, 220.0));
    assert_eq!((t.rectOfRow(1), t.rectOfColumn(1)), (rect(0.0, 22.0, 258.0, 22.0), rect(104.0, 0.0, 154.0, 220.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 1), rect(2.0, 23.0, 100.0, 20.0));
    assert_eq!(t.frameOfCellAtColumn_row(1, 1), rect(106.0, 23.0, 150.0, 20.0));
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(None) };
    let _ = NSRange::new(0, 0);
}

fn visible_rows(mtm: MainThreadMarker) {
    let (w, scroll, t, src) = scrolling_table(mtm);
    // As wide as the clip view; as tall as its rows.
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 2200.0));
    take();
    w.layoutIfNeeded();
    let log = take();
    assert_eq!(log[..3], ["viewFor a 0", "viewFor b 0", "didAdd 0"]);
    // The visible rows have views (and perhaps a few more), others don't.
    let shown = realized(&t, 100);
    assert!((0..10).all(|r| shown.contains(&r)), "{shown:?}");
    assert!(shown.iter().all(|&r| r < 14), "{shown:?}");
    let row = t.rowViewAtRow_makeIfNecessary(1, false).expect("row 1");
    assert_eq!(row.frame(), rect(0.0, 22.0, 300.0, 22.0));
    let cells: Vec<NSRect> = row.subviews().iter().map(|v| v.frame()).collect();
    assert_eq!(cells, [rect(2.0, 1.0, 100.0, 20.0), rect(106.0, 1.0, 150.0, 20.0)]);
    let cell = t.viewAtColumn_row_makeIfNecessary(1, 1, false).expect("a cell");
    assert_eq!((t.rowForView(&cell), t.columnForView(&cell)), (1, 1));
    assert_eq!((t.rowForView(&row), t.columnForView(&row)), (1, -1));
    // Views for a row far away only when asked to make them.
    assert!(t.viewAtColumn_row_makeIfNecessary(0, 50, false).is_none());
    take();
    let far = t.viewAtColumn_row_makeIfNecessary(0, 50, true).expect("made");
    assert_eq!(take(), ["viewFor a 50", "viewFor b 50", "didAdd 50"]);
    assert_eq!(far.frame(), rect(2.0, 1.0, 100.0, 20.0));

    // Scrolling: rows that go far away give their views to the new ones.
    scroll.contentView().scrollToPoint(NSPoint::new(0.0, 110.0));
    w.layoutIfNeeded();
    let log = take();
    let shown = realized(&t, 100);
    assert!((5..15).all(|r| shown.contains(&r)), "{shown:?}");
    assert!(!shown.contains(&0) && !shown.contains(&1), "{shown:?}");
    assert!(log.contains(&"didRemove 0".to_owned()) && log.contains(&"prepareForReuse".to_owned()), "{log:?}");
    assert!(src.ivars().made.get() < 2 * shown.len() + 6);
    // To a row, as little as it takes.
    t.scrollRowToVisible(40);
    assert_eq!(scroll.contentView().bounds().origin, NSPoint::new(0.0, 702.0));
    t.scrollRowToVisible(2);
    assert_eq!(scroll.contentView().bounds().origin, NSPoint::new(0.0, 44.0));
    // Wider: the table follows the clip view; the last column can fill it.
    scroll.setFrameSize(NSSize::new(500.0, 200.0));
    w.layoutIfNeeded();
    assert_eq!(t.frame().size.width, 500.0);
    assert_eq!(t.tableColumns().objectAtIndex(1).width(), 150.0);
    t.sizeLastColumnToFit();
    assert_eq!(t.tableColumns().objectAtIndex(1).width(), 392.0);
    close(&w, &t);
    take();
}

fn selecting(mtm: MainThreadMarker) {
    let (w, _scroll, t, _src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    take();
    let one = |i| NSIndexSet::indexSetWithIndex(i);
    // In code, the delegate isn't asked, and hears of the change.
    t.selectRowIndexes_byExtendingSelection(&one(3), false);
    assert_eq!(take(), ["selectionDidChange NSTableViewSelectionDidChangeNotification"]);
    assert_eq!((t.selectedRow(), t.numberOfSelectedRows()), (3, 1));
    assert!(t.isRowSelected(3) && !t.isRowSelected(4));
    assert!(t.rowViewAtRow_makeIfNecessary(3, false).unwrap().isSelected());
    assert!(!t.rowViewAtRow_makeIfNecessary(4, false).unwrap().isSelected());
    // Extending, even without multiple selection.
    t.selectRowIndexes_byExtendingSelection(&one(5), true);
    assert_eq!((t.selectedRow(), t.numberOfSelectedRows()), (5, 2));
    t.setAllowsMultipleSelection(true);
    t.selectRowIndexes_byExtendingSelection(&one(1), true);
    assert_eq!((t.selectedRow(), indexes(&t.selectedRowIndexes())), (1, vec![1, 3, 5]));
    take();
    t.selectRowIndexes_byExtendingSelection(&one(1), true);
    assert_eq!(take(), Vec::<String>::new());
    t.deselectRow(5);
    assert_eq!((t.selectedRow(), t.numberOfSelectedRows()), (1, 2));
    take();
    // Selecting all asks about each row.
    // SAFETY: the sender may be nil.
    unsafe { t.selectAll(None) };
    let log = take();
    assert_eq!(log.iter().filter(|l| l.starts_with("shouldSelect")).count(), 100);
    assert_eq!(log.last().unwrap(), "selectionDidChange NSTableViewSelectionDidChangeNotification");
    assert_eq!(t.numberOfSelectedRows(), 99);
    assert!(!t.isRowSelected(7));
    // SAFETY: the sender may be nil.
    unsafe { t.deselectAll(None) };
    assert_eq!((t.numberOfSelectedRows(), t.selectedRow()), (0, -1));
    // Rows that don't exist aren't selected.
    take();
    t.selectRowIndexes_byExtendingSelection(&one(200), false);
    assert_eq!((t.numberOfSelectedRows(), take()), (0, vec![]));
    // An empty selection may be refused.
    t.selectRowIndexes_byExtendingSelection(&one(2), false);
    t.setAllowsEmptySelection(false);
    // SAFETY: the sender may be nil.
    unsafe { t.deselectAll(None) };
    assert_eq!(t.numberOfSelectedRows(), 1);
    close(&w, &t);
    take();
}

fn updating_rows(mtm: MainThreadMarker) {
    let (w, _scroll, t, src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    take();
    // The data source is asked again.
    src.ivars().rows.set(10);
    t.noteNumberOfRowsChanged();
    assert_eq!(t.numberOfRows(), 10);
    // Without asking.
    t.insertRowsAtIndexes_withAnimation(&NSIndexSet::indexSetWithIndex(0), NSTableViewAnimationOptions::empty());
    assert_eq!(t.numberOfRows(), 11);
    t.removeRowsAtIndexes_withAnimation(&NSIndexSet::indexSetWithIndex(0), NSTableViewAnimationOptions::empty());
    assert_eq!(t.numberOfRows(), 10);
    w.layoutIfNeeded();
    take();
    // A cell made again gives up the old view first.
    t.reloadDataForRowIndexes_columnIndexes(&NSIndexSet::indexSetWithIndex(1), &NSIndexSet::indexSetWithIndex(0));
    assert_eq!(take(), ["prepareForReuse", "viewFor a 1"]);
    close(&w, &t);
    take();
}

fn row_heights(mtm: MainThreadMarker) {
    let this = Heights::alloc(mtm).set_ivars(SourceIvars {
        rows: Cell::new(5),
        made: Cell::new(0),
        heights_of: RefCell::new(vec![10.0, 11.0, 12.0, 13.0, 14.0]),
        ask_on_remove: Cell::new(false),
    });
    // SAFETY: the superclass's designated initializer.
    let h: Retained<Heights> = unsafe { msg_send![super(this), init] };
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    t.setStyle(NSTableViewStyle::Plain);
    t.setIntercellSpacing(NSSize::new(0.0, 0.0));
    t.addTableColumn(&column(mtm, "c", 100.0));
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*h))) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*h))) };
    take();
    t.reloadData();
    let rects: Vec<NSRect> = (0..5).map(|r| t.rectOfRow(r)).collect();
    assert_eq!(
        rects,
        [
            rect(0.0, 0.0, 100.0, 10.0),
            rect(0.0, 10.0, 100.0, 11.0),
            rect(0.0, 21.0, 100.0, 12.0),
            rect(0.0, 33.0, 100.0, 13.0),
            rect(0.0, 46.0, 100.0, 14.0),
        ]
    );
    assert_eq!(t.frame().size.height, 60.0);
    assert_eq!((t.rowAtPoint(NSPoint::new(1.0, 32.9)), t.rowAtPoint(NSPoint::new(1.0, 33.0))), (2, 3));
    take();
    h.ivars().heights_of.borrow_mut()[2] = 30.0;
    t.noteHeightOfRowsWithIndexesChanged(&NSIndexSet::indexSetWithIndex(2));
    assert!(take().contains(&"heightOf 2".to_owned()));
    assert_eq!((t.rectOfRow(2), t.rectOfRow(3).origin.y), (rect(0.0, 21.0, 100.0, 30.0), 51.0));
    assert_eq!(t.frame().size.height, 78.0);
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(None) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(None) };
    let _: Option<&AnyObject> = None;
}

/// A table of `rows` rows 24 points apart, styled, in a scroll view 300 by
/// 200 (with no header), in a window.
fn styled_table(
    mtm: MainThreadMarker,
    style: Option<NSTableViewStyle>,
    rows: isize,
) -> (Retained<NSWindow>, Retained<NSTableView>, Retained<Source>) {
    let (w, _scroll, t, src) = scrolling_table(mtm);
    t.setIntercellSpacing(NSSize::new(17.0, 0.0));
    t.setRowHeight(24.0);
    match style {
        Some(style) => t.setStyle(style),
        None => t.setStyle(NSTableViewStyle::Automatic),
    }
    src.ivars().rows.set(rows);
    t.reloadData();
    w.layoutIfNeeded();
    (w, t, src)
}

fn styles(mtm: MainThreadMarker) {
    let col = |t: &NSTableView, c| t.rectOfColumn(c);
    // The default: the inset style, padded at the outer columns' edges and
    // around the rows.
    let (w, t, _src) = styled_table(mtm, None, 20);
    assert_eq!(t.effectiveStyle(), NSTableViewStyle::Inset);
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 490.0));
    assert_eq!((t.rectOfRow(0), t.rectOfRow(1)), (rect(0.0, 5.0, 300.0, 24.0), rect(0.0, 29.0, 300.0, 24.0)));
    assert_eq!((col(&t, 0), col(&t, 1)), (rect(10.0, 0.0, 115.0, 490.0), rect(125.0, 0.0, 164.0, 490.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 0), rect(16.0, 5.0, 100.0, 24.0));
    assert_eq!(t.frameOfCellAtColumn_row(1, 1), rect(133.0, 29.0, 150.0, 24.0));
    let row = t.rowViewAtRow_makeIfNecessary(1, true).expect("row 1");
    assert_eq!(row.frame(), rect(0.0, 29.0, 300.0, 24.0));
    let cells: Vec<NSRect> = row.subviews().iter().map(|v| v.frame()).collect();
    assert_eq!(cells, [rect(16.0, 0.0, 100.0, 24.0), rect(133.0, 0.0, 150.0, 24.0)]);
    let rows_at: Vec<isize> =
        [4.9, 5.0, 28.9, 29.0, 484.9, 485.0].iter().map(|&y| t.rowAtPoint(NSPoint::new(20.0, y))).collect();
    assert_eq!(rows_at, [-1, 0, 0, 1, 19, -1]);
    let columns_at: Vec<isize> =
        [9.9, 10.0, 124.9, 125.0, 288.9, 289.0].iter().map(|&x| t.columnAtPoint(NSPoint::new(x, 20.0))).collect();
    assert_eq!(columns_at, [-1, 0, 0, 1, 1, -1]);
    let rows = t.rowsInRect(rect(0.0, 0.0, 300.0, 30.0));
    assert_eq!((rows.location, rows.length), (0, 2));
    // Spacing goes between the columns, not at their outer edges.
    t.setIntercellSpacing(NSSize::new(4.0, 2.0));
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 530.0));
    assert_eq!((col(&t, 0), col(&t, 1)), (rect(10.0, 0.0, 108.0, 530.0), rect(118.0, 0.0, 158.0, 530.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 0), rect(16.0, 6.0, 100.0, 24.0));
    assert_eq!(t.frameOfCellAtColumn_row(1, 1), rect(120.0, 32.0, 150.0, 24.0));
    close(&w, &t);

    // Few rows: the table fills its clip view.
    let (w, t, _src) = styled_table(mtm, Some(NSTableViewStyle::Plain), 3);
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 200.0));
    close(&w, &t);

    // Full width: the outer padding, without the insets.
    let (w, t, _src) = styled_table(mtm, Some(NSTableViewStyle::FullWidth), 20);
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 480.0));
    assert_eq!((col(&t, 0), col(&t, 1)), (rect(0.0, 0.0, 115.0, 480.0), rect(115.0, 0.0, 164.0, 480.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 0), rect(6.0, 0.0, 100.0, 24.0));
    assert_eq!(t.frameOfCellAtColumn_row(1, 1), rect(123.0, 24.0, 150.0, 24.0));
    close(&w, &t);

    // A source list: more room above the rows, none below.
    let (w, t, _src) = styled_table(mtm, Some(NSTableViewStyle::SourceList), 20);
    assert_eq!(t.frame(), rect(0.0, 0.0, 300.0, 490.0));
    assert_eq!((t.rectOfRow(0), t.rectOfRow(19)), (rect(0.0, 10.0, 300.0, 24.0), rect(0.0, 466.0, 300.0, 24.0)));
    assert_eq!(t.frameOfCellAtColumn_row(0, 0), rect(16.0, 10.0, 100.0, 24.0));
    close(&w, &t);

    // Outside a scroll view, as wide as its columns and their insets.
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    t.addTableColumn(&column(mtm, "a", 100.0));
    t.addTableColumn(&column(mtm, "b", 150.0));
    let src = source(mtm, 20);
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*src))) };
    t.reloadData();
    assert_eq!(t.frame().size.width, 299.0);
    // SAFETY: as above.
    unsafe { t.setDataSource(None) };
    take();
}

fn selection_follows_rows(mtm: MainThreadMarker) {
    let (w, _scroll, t, src) = scrolling_table(mtm);
    src.ivars().rows.set(20);
    t.reloadData();
    w.layoutIfNeeded();
    let one = |i| NSIndexSet::indexSetWithIndex(i);
    let none = NSTableViewAnimationOptions::empty();
    t.selectRowIndexes_byExtendingSelection(&one(5), false);
    take();
    // Rows coming and going around the selection carry it along, quietly.
    src.ivars().rows.set(19);
    t.removeRowsAtIndexes_withAnimation(&one(0), none);
    assert_eq!((only_selection(take()), t.selectedRow(), indexes(&t.selectedRowIndexes())), (vec![], 4, vec![4]));
    src.ivars().rows.set(20);
    t.insertRowsAtIndexes_withAnimation(&one(0), none);
    assert_eq!((only_selection(take()), t.selectedRow(), indexes(&t.selectedRowIndexes())), (vec![], 5, vec![5]));
    src.ivars().rows.set(21);
    t.insertRowsAtIndexes_withAnimation(&one(10), none);
    assert_eq!(t.selectedRow(), 5);
    t.moveRowAtIndex_toIndex(5, 8);
    assert_eq!((only_selection(take()), t.selectedRow(), indexes(&t.selectedRowIndexes())), (vec![], 8, vec![8]));
    t.moveRowAtIndex_toIndex(0, 12);
    assert_eq!((t.selectedRow(), indexes(&t.selectedRowIndexes())), (7, vec![7]));
    // Removing the selected row changes the selection.
    take();
    src.ivars().rows.set(20);
    t.removeRowsAtIndexes_withAnimation(&one(7), none);
    assert_eq!((only_selection(take()).len(), t.selectedRow(), t.numberOfSelectedRows()), (1, -1, 0));
    // The last row selected going leaves the first still selected as
    // `selectedRow`.
    t.setAllowsMultipleSelection(true);
    for (i, extend) in [(8, false), (3, true), (5, true)] {
        t.selectRowIndexes_byExtendingSelection(&one(i), extend);
    }
    assert_eq!(t.selectedRow(), 5);
    take();
    src.ivars().rows.set(19);
    t.removeRowsAtIndexes_withAnimation(&one(5), none);
    assert_eq!((only_selection(take()).len(), t.selectedRow(), indexes(&t.selectedRowIndexes())), (1, 3, vec![3, 7]));
    close(&w, &t);
    take();
}

fn only_selection(log: Vec<String>) -> Vec<String> {
    log.into_iter().filter(|l| l.starts_with("selectionDidChange")).collect()
}

fn column_changes(mtm: MainThreadMarker) {
    let (w, _scroll, t, _src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    take();
    let a = t.tableColumns().objectAtIndex(0);
    a.setWidth(120.0);
    assert_eq!(take(), ["columnDidResize a 100"]);
    a.setWidth(120.0);
    assert_eq!(take(), Vec::<String>::new());
    t.moveColumn_toColumn(0, 1);
    assert_eq!(
        take().into_iter().filter(|l| l.starts_with("column")).collect::<Vec<_>>(),
        ["columnDidMove Some(0) Some(1)"]
    );
    assert_eq!(t.tableColumns().objectAtIndex(1).identifier().to_string(), "a");
    // Sort descriptors: a change is told to the data source, with the old
    // ones (none the first time).
    let by = |key: &str| NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&NSString::from_str(key)), true);
    t.setSortDescriptors(&NSArray::from_retained_slice(&[by("a")]));
    assert_eq!(take(), ["sortDescriptorsDidChange nil"]);
    t.setSortDescriptors(&NSArray::from_retained_slice(&[by("a")]));
    assert_eq!(take(), Vec::<String>::new());
    t.setSortDescriptors(&NSArray::new());
    assert_eq!(take(), ["sortDescriptorsDidChange 1"]);
    close(&w, &t);
    take();
}

fn background_styles(mtm: MainThreadMarker) {
    // A row view gives a subview its style as it's added.
    let r = NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), NSRect::ZERO);
    assert!(!r.isEmphasized());
    r.setSelected(true);
    r.setEmphasized(true);
    assert_eq!(r.interiorBackgroundStyle(), NSBackgroundStyle::Emphasized);
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), NSRect::ZERO);
    r.addSubview(&cell);
    assert_eq!(cell.backgroundStyle(), NSBackgroundStyle::Emphasized);
    r.setSelected(false);
    assert_eq!(r.interiorBackgroundStyle(), NSBackgroundStyle::Normal);

    // In a table, every change is passed on.
    let (w, _scroll, t, _src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(2), false);
    let row = t.rowViewAtRow_makeIfNecessary(2, false).expect("row 2");
    let style = |r| {
        let cell = t.viewAtColumn_row_makeIfNecessary(0, r, false).expect("a cell");
        // SAFETY: the cells are NSTableCellViews, which have the property.
        let style: NSBackgroundStyle = unsafe { msg_send![&*cell, backgroundStyle] };
        style
    };
    // Not the key window's focus: a selected row isn't emphasized.
    assert!(!row.isEmphasized());
    assert_eq!((style(2), style(3)), (NSBackgroundStyle::Normal, NSBackgroundStyle::Normal));
    row.setEmphasized(true);
    assert_eq!((style(2), style(3)), (NSBackgroundStyle::Emphasized, NSBackgroundStyle::Normal));
    row.setEmphasized(false);
    assert_eq!(style(2), NSBackgroundStyle::Normal);
    row.setEmphasized(true);
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(3), false);
    assert_eq!((style(2), style(3)), (NSBackgroundStyle::Normal, NSBackgroundStyle::Normal));
    close(&w, &t);
    take();
}

fn heights_for_new_rows(mtm: MainThreadMarker) {
    let this = Heights::alloc(mtm).set_ivars(SourceIvars {
        rows: Cell::new(1000),
        made: Cell::new(0),
        heights_of: RefCell::new((0..1001).map(|r| 20.0 + (r % 3) as f64).collect()),
        ask_on_remove: Cell::new(false),
    });
    // SAFETY: the superclass's designated initializer.
    let h: Retained<Heights> = unsafe { msg_send![super(this), init] };
    let (w, _scroll, t, _src) = scrolling_table(mtm);
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*h))) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*h))) };
    t.reloadData();
    w.layoutIfNeeded();
    take();
    // Appending a row asks about that row only; removing it, about none.
    h.ivars().rows.set(1001);
    t.insertRowsAtIndexes_withAnimation(&NSIndexSet::indexSetWithIndex(1000), NSTableViewAnimationOptions::empty());
    assert_eq!(take(), ["heightOf 1000"]);
    h.ivars().rows.set(1000);
    t.removeRowsAtIndexes_withAnimation(&NSIndexSet::indexSetWithIndex(1000), NSTableViewAnimationOptions::empty());
    assert_eq!(take(), Vec::<String>::new());
    assert_eq!(t.numberOfRows(), 1000);
    close(&w, &t);
    take();
}

fn delegates_ask_as_rows_go(mtm: MainThreadMarker) {
    let (w, _scroll, t, src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    src.ivars().ask_on_remove.set(true);
    assert!(t.rowViewAtRow_makeIfNecessary(0, false).is_some());
    src.ivars().rows.set(99);
    t.removeRowsAtIndexes_withAnimation(&NSIndexSet::indexSetWithIndex(0), NSTableViewAnimationOptions::empty());
    assert_eq!(t.numberOfRows(), 99);
    src.ivars().ask_on_remove.set(false);
    close(&w, &t);
    take();
}

fn columns_outlive_their_table(mtm: MainThreadMarker) {
    let c = column(mtm, "a", 100.0);
    let header = objc2::rc::autoreleasepool(|_| {
        let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        t.addTableColumn(&c);
        t.headerView().expect("a header")
    });
    assert!(c.tableView().is_none() && header.tableView().is_none());
    c.setWidth(50.0);
    c.setHidden(true);
    assert_eq!(header.headerRectOfColumn(0), rect(0.0, 0.0, 0.0, 28.0));
}

/// Clicking rows, through the window.
#[cfg(not(target_vendor = "apple"))]
fn clicking(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "ConformanceTableTarget"]
        struct Target;

        impl Target {
            #[unsafe(method(clicked:))]
            fn clicked(&self, sender: &NSTableView) {
                log(format!("clicked {}", sender.clickedRow()));
            }

            #[unsafe(method(doubleClicked:))]
            fn double_clicked(&self, sender: &NSTableView) {
                log(format!("doubleClicked {}", sender.clickedRow()));
            }
        }
    );

    let (w, _scroll, t, _src) = scrolling_table(mtm);
    w.layoutIfNeeded();
    // SAFETY: the class's initializer.
    let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
    // SAFETY: the target outlives the table's use of it, and the actions take a sender.
    unsafe {
        let _: () = msg_send![&*t, setTarget: &*target];
        let _: () = msg_send![&*t, setAction: objc2::sel!(clicked:)];
        t.setDoubleAction(Some(objc2::sel!(doubleClicked:)));
    }
    take();
    // Row 3 is 66 to 88 down from the top of the 200-point window.
    let click = |kind, clicks, flags| {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            NSPoint::new(20.0, 200.0 - 70.0),
            flags,
            0.0,
            w.windowNumber(),
            None,
            0,
            clicks,
            1.0,
        )
        .expect("an event");
        w.sendEvent(&event);
    };
    click(NSEventType::LeftMouseDown, 1, NSEventModifierFlags::empty());
    assert_eq!(t.clickedRow(), 3);
    click(NSEventType::LeftMouseUp, 1, NSEventModifierFlags::empty());
    assert_eq!(t.selectedRow(), 3);
    assert_eq!(t.clickedRow(), -1);
    let log = take();
    assert!(log.contains(&"shouldSelect 3".to_owned()) && log.contains(&"clicked 3".to_owned()), "{log:?}");
    click(NSEventType::LeftMouseDown, 2, NSEventModifierFlags::empty());
    click(NSEventType::LeftMouseUp, 2, NSEventModifierFlags::empty());
    assert!(take().contains(&"doubleClicked 3".to_owned()));
    close(&w, &t);
    take();
}

define_class!(
    /// Rows of cells holding a view pinned inside by four constraints.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTableConstrained"]
    #[ivars = SourceIvars]
    struct Constrained;

    unsafe impl NSObjectProtocol for Constrained {}

    unsafe impl NSTableViewDataSource for Constrained {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.get()
        }
    }

    unsafe impl NSControlTextEditingDelegate for Constrained {}

    unsafe impl NSTableViewDelegate for Constrained {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            _row: isize,
        ) -> Option<Retained<NSView>> {
            let id = column.map(|c| c.identifier()).unwrap_or_else(|| NSString::from_str("group"));
            // SAFETY: there is no owner to connect outlets to.
            let reused = unsafe { table.makeViewWithIdentifier_owner(&id, None) };
            Some(reused.unwrap_or_else(|| self.make_cell(table, &id)))
        }
    }
);

impl Constrained {
    fn make_cell(&self, table: &NSTableView, id: &NSString) -> Retained<NSView> {
        self.ivars().made.set(self.ivars().made.get() + 1);
        let mtm = MainThreadMarker::from(table);
        let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), NSRect::ZERO);
        let label = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        label.setTranslatesAutoresizingMaskIntoConstraints(false);
        cell.addSubview(&label);
        let constraints = [
            label.leadingAnchor().constraintEqualToAnchor_constant(&cell.leadingAnchor(), 2.0),
            label.trailingAnchor().constraintEqualToAnchor_constant(&cell.trailingAnchor(), -2.0),
            label.topAnchor().constraintEqualToAnchor_constant(&cell.topAnchor(), 1.0),
            label.bottomAnchor().constraintEqualToAnchor_constant(&cell.bottomAnchor(), -1.0),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        let view: Retained<NSView> = Retained::into_super(cell);
        view.setIdentifier(Some(id));
        view
    }
}

/// Microseconds a run of `f` takes, the median of seven.
fn median_us(mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

/// What scrolling a long table costs, for comparing AppKit with Sidestep:
/// `cargo test --release -p sidestep-conformance --test appkit_table -- timing`.
fn timing(mtm: MainThreadMarker) {
    const ROWS: isize = 1_000_000;
    const PAGES: usize = 50;
    let (w, scroll, t, src) = scrolling_table(mtm);
    src.ivars().rows.set(ROWS);
    let reload = median_us(|| t.reloadData());
    w.layoutIfNeeded();
    take();
    let clip = scroll.contentView();
    let mut y = 0.0;
    // A page (the clip view's height) at a time, laid out after each.
    let page = median_us(|| {
        for _ in 0..PAGES {
            y += 200.0;
            clip.scrollToPoint(NSPoint::new(0.0, y));
            w.layoutIfNeeded();
        }
        take();
    }) / PAGES as f64;
    let step = median_us(|| {
        for _ in 0..PAGES {
            y += 10.0;
            clip.scrollToPoint(NSPoint::new(0.0, y));
            w.layoutIfNeeded();
        }
        take();
    }) / PAGES as f64;
    let made = src.ivars().made.get();
    // Appending rows one at a time to a table whose delegate sizes rows.
    let this = Heights::alloc(mtm).set_ivars(SourceIvars {
        rows: Cell::new(10_000),
        made: Cell::new(0),
        heights_of: RefCell::new(vec![22.0; 10_100]),
        ask_on_remove: Cell::new(false),
    });
    // SAFETY: the superclass's designated initializer.
    let h: Retained<Heights> = unsafe { msg_send![super(this), init] };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*h))) };
    // SAFETY: as above.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*h))) };
    t.reloadData();
    w.layoutIfNeeded();
    let append = median_us(|| {
        let n = h.ivars().rows.get();
        h.ivars().rows.set(n + 1);
        t.insertRowsAtIndexes_withAnimation(
            &NSIndexSet::indexSetWithIndex(n as usize),
            NSTableViewAnimationOptions::empty(),
        );
        w.layoutIfNeeded();
    });
    close(&w, &t);
    take();

    // Cells with constraints, in a window with 1,200 other constraints.
    let (w, scroll, t, _src) = scrolling_table(mtm);
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    w.setContentView(Some(&content));
    content.addSubview(&scroll);
    let other = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 1.0, 1.0));
    content.addSubview(&other);
    let mut constraints = Vec::new();
    for i in 0..300 {
        let v = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        v.setTranslatesAutoresizingMaskIntoConstraints(false);
        other.addSubview(&v);
        constraints.push(v.leadingAnchor().constraintEqualToAnchor_constant(&other.leadingAnchor(), i as f64));
        constraints.push(v.topAnchor().constraintEqualToAnchor(&other.topAnchor()));
        constraints.push(v.widthAnchor().constraintEqualToConstant(1.0));
        constraints.push(v.heightAnchor().constraintEqualToConstant(1.0));
    }
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    let this = Constrained::alloc(mtm).set_ivars(SourceIvars {
        rows: Cell::new(ROWS),
        made: Cell::new(0),
        heights_of: RefCell::default(),
        ask_on_remove: Cell::new(false),
    });
    // SAFETY: the superclass's designated initializer.
    let c: Retained<Constrained> = unsafe { msg_send![super(this), init] };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*c))) };
    // SAFETY: as above.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*c))) };
    t.reloadData();
    w.layoutIfNeeded();
    let clip = scroll.contentView();
    let mut y = 0.0;
    let constrained = median_us(|| {
        for _ in 0..PAGES {
            y += 200.0;
            clip.scrollToPoint(NSPoint::new(0.0, y));
            w.layoutIfNeeded();
        }
    }) / PAGES as f64;
    let cell = t.viewAtColumn_row_makeIfNecessary(0, t.rowAtPoint(NSPoint::new(5.0, y + 50.0)), false).expect("a cell");
    assert_eq!(cell.subviews().objectAtIndex(0).frame().size.width, 96.0);
    close(&w, &t);
    take();
    println!(
        "{ROWS} rows: reloadData {reload:.0} µs; scrolling a page and laying out {page:.1} µs; \
         10 points {step:.1} µs; {made} cell views made in all; appending a row to 10,000 sized by the \
         delegate {append:.1} µs; a page of cells with constraints, 1,200 others in the window: {constrained:.1} µs"
    );
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    if std::env::args().any(|a| a == "timing") {
        return objc2::rc::autoreleasepool(|_| timing(mtm));
    }
    #[allow(unused_mut)] // Linux adds a test.
    let mut tests: Vec<Test> = vec![
        ("defaults", defaults),
        ("geometry", geometry),
        ("visible_rows", visible_rows),
        ("selecting", selecting),
        ("updating_rows", updating_rows),
        ("row_heights", row_heights),
        ("styles", styles),
        ("selection_follows_rows", selection_follows_rows),
        ("column_changes", column_changes),
        ("background_styles", background_styles),
        ("heights_for_new_rows", heights_for_new_rows),
        ("delegates_ask_as_rows_go", delegates_ask_as_rows_go),
        ("columns_outlive_their_table", columns_outlive_their_table),
    ];
    #[cfg(not(target_vendor = "apple"))]
    tests.push(("clicking", clicking));
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        take();
        println!("test {name} ... ok");
    }
}
