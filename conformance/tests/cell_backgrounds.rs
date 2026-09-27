//! Background styles, checked on macOS and on Linux alike: who is given a
//! style (cell views, and the cells of the controls under a row or cell
//! view; nothing else) and when (as a subview is added; after a row
//! changes, only once it's about to draw or its table hands out one of its
//! cell views); what a row view's interior style follows; which cells draw
//! on an emphasized background (their interior style) and how their text
//! changes there: the label colors turn light, in a text field's text
//! color and its attributed runs alike, and so does a text color with the
//! label color's value, while other colors (those made from label colors
//! too), the placeholder and whatever a program draws itself stay.
//!
//! Colors are compared by their relations (dark or light, stronger or
//! weaker, changed or not), never by value: Sidestep's palette is its own.
//! Appearances are always set explicitly, since the Mac running the tests
//! may be in dark mode. Pictures are drawn into bitmaps, without windows;
//! windows are never shown.
//!
//! What macOS does with images there, for the image cells to follow (see
//! `cell::template_ink` in Sidestep): a template image, tinted or not, in
//! an image view or a borderless button, is drawn opaque white on an
//! emphasized background; an image that isn't a template keeps its colors.
//!
//! Key windows: a table emphasizes its selection when it gets the focus of
//! a window that is key. Without showing a window, macOS can only be told
//! so by calling `becomeKeyWindow` (which it remembers privately: it
//! doesn't read an overridden `isKeyWindow`, and `resignKeyWindow` doesn't
//! undo it, nor does it change a table that already has the focus), so the
//! tests here move the focus in a window that has been told it's key and
//! answers `isKeyWindow` so, as Sidestep reads it. The window gaining and
//! losing key status is covered on Linux (`linux_table_rows`).
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::RefCell;

use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSBackgroundStyle, NSBackingStoreType, NSBezelStyle, NSBitmapImageRep, NSButton, NSButtonType, NSCell, NSColor,
    NSColorSpace, NSControl, NSControlTextEditingDelegate, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSRectFill, NSResponder, NSScrollView, NSTableCellView, NSTableColumn, NSTableRowView, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewSelectionHighlightStyle, NSTableViewStyle, NSTextField,
    NSTextFieldCell, NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSIndexSet, NSObjectProtocol, NSRect, NSString};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn aqua() -> Retained<NSAppearance> {
    // SAFETY: the name is a constant string.
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).expect("Aqua")
}

fn dark() -> Retained<NSAppearance> {
    // SAFETY: the name is a constant string.
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).expect("Dark Aqua")
}

const NORMAL: NSBackgroundStyle = NSBackgroundStyle::Normal;
const EMPHASIZED: NSBackgroundStyle = NSBackgroundStyle::Emphasized;

define_class!(
    /// A cell view that writes down the styles it's given, by identifier.
    #[unsafe(super(NSTableCellView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsCellView"]
    struct LoggingCellView;

    impl LoggingCellView {
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            let view: &NSView = self;
            let id = view.identifier().map(|s| s.to_string()).unwrap_or_default();
            log(format!("cellview {id} {}", style.0));
            // SAFETY: the superclass's method, with the argument it was given.
            unsafe { msg_send![super(self), setBackgroundStyle: style] }
        }
    }
);

define_class!(
    /// A plain view with a setBackgroundStyle: of its own, which no one
    /// calls.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsStyledView"]
    struct StyledView;

    impl StyledView {
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            log(format!("styled {}", style.0));
        }
    }
);

define_class!(
    /// A text field cell that writes down the styles it's given, by its
    /// text.
    #[unsafe(super(NSTextFieldCell, NSActionCell, NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsFieldCell"]
    struct LoggingFieldCell;

    impl LoggingFieldCell {
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            log(format!("fieldcell {} {}", self.stringValue(), style.0));
            // SAFETY: the superclass's method, with the argument it was given.
            unsafe { msg_send![super(self), setBackgroundStyle: style] }
        }
    }
);

define_class!(
    /// A cell that draws its own interior: a square of the label color.
    #[unsafe(super(NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsFillCell"]
    struct FillCell;

    impl FillCell {
        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior(&self, frame: NSRect, _view: &NSView) {
            NSColor::labelColor().setFill();
            NSRectFill(rect(frame.origin.x, frame.origin.y, 10.0, 10.0));
        }
    }
);

define_class!(
    /// A window that says it's key when told to, without being shown.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsKeyWindow"]
    #[ivars = std::cell::Cell<bool>]
    struct KeyWindow;

    impl KeyWindow {
        #[unsafe(method(isKeyWindow))]
        fn is_key_window(&self) -> bool {
            self.ivars().get()
        }
    }
);

define_class!(
    /// A view that writes down its layout, `viewWillDraw` and drawing.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsDrawLog"]
    struct DrawLog;

    impl DrawLog {
        #[unsafe(method(layout))]
        fn layout(&self) {
            log("layout".into());
            // SAFETY: the superclass's method.
            unsafe { msg_send![super(self), layout] }
        }

        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            log("viewWillDraw".into());
            // SAFETY: the superclass's method.
            unsafe { msg_send![super(self), viewWillDraw] }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            log("drawRect".into());
        }
    }
);

/// A label whose cell writes down the styles it's given.
fn logging_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let f = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    // SAFETY: the class's designated initializer.
    let cell: Retained<LoggingFieldCell> =
        unsafe { msg_send![LoggingFieldCell::alloc(mtm), initTextCell: &*NSString::from_str(text)] };
    cell.setEditable(false);
    cell.setSelectable(false);
    cell.setBezeled(false);
    cell.setBordered(false);
    cell.setDrawsBackground(false);
    f.setCell(Some(&cell));
    f
}

fn logging_cell_view(mtm: MainThreadMarker, id: &str) -> Retained<LoggingCellView> {
    // SAFETY: NSView's designated initializer.
    let view: Retained<LoggingCellView> =
        unsafe { msg_send![LoggingCellView::alloc(mtm), initWithFrame: rect(0.0, 0.0, 100.0, 20.0)] };
    let v: &NSView = &view;
    v.setIdentifier(Some(&NSString::from_str(id)));
    view
}

fn styled_view(mtm: MainThreadMarker) -> Retained<StyledView> {
    // SAFETY: NSView's designated initializer.
    unsafe { msg_send![StyledView::alloc(mtm), initWithFrame: NSRect::ZERO] }
}

fn responds(o: &AnyObject, s: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector.
    unsafe { msg_send![o, respondsToSelector: s] }
}

fn style_of(control: &NSControl) -> NSBackgroundStyle {
    control.cell().expect("a cell").backgroundStyle()
}

fn who_takes_styles(mtm: MainThreadMarker) {
    // Only cell views take a style; controls answer their cell's.
    let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    let label = NSTextField::labelWithString(&NSString::from_str("x"), mtm);
    // SAFETY: no target or action.
    let button = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("x"), None, None, mtm) };
    let cell_view = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), NSRect::ZERO);
    let row = NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), NSRect::ZERO);
    let set = sel!(setBackgroundStyle:);
    assert!(responds(&cell_view, set));
    assert!(!responds(&view, set) && !responds(&label, set) && !responds(&button, set) && !responds(&row, set));
    assert!(responds(&label, sel!(backgroundStyle)) && responds(&button, sel!(backgroundStyle)));
    label.cell().expect("a cell").setBackgroundStyle(EMPHASIZED);
    // SAFETY: a control's backgroundStyle returns the style.
    let got: NSBackgroundStyle = unsafe { msg_send![&*label, backgroundStyle] };
    assert_eq!(got, EMPHASIZED);

    // A cell view passes its style to the controls' cells under it, at any
    // depth, every time; views that aren't controls aren't told, even if
    // they could take it.
    let cv = logging_cell_view(mtm, "cv");
    let direct = logging_label(mtm, "direct");
    let nested = logging_label(mtm, "nested");
    let outlet = logging_label(mtm, "outlet");
    let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    holder.addSubview(&nested);
    holder.addSubview(&outlet);
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("c"), None, None, mtm) };
    let styled = styled_view(mtm);
    let cvv: &NSView = &cv;
    cvv.addSubview(&direct);
    cvv.addSubview(&holder);
    cvv.addSubview(&styled);
    cvv.addSubview(&check);
    let cvt: &NSTableCellView = &cv;
    // SAFETY: the outlet is a subview, which outlives its use here.
    unsafe { cvt.setTextField(Some(&outlet)) };
    assert_eq!(take(), Vec::<String>::new(), "adding subviews tells no one");
    cvt.setBackgroundStyle(EMPHASIZED);
    let told = ["cellview cv 1", "fieldcell direct 1", "fieldcell nested 1", "fieldcell outlet 1"];
    assert_eq!(take(), told);
    assert_eq!(style_of(&check), EMPHASIZED);
    cvt.setBackgroundStyle(EMPHASIZED);
    assert_eq!(take(), told, "told again");
    let late = logging_label(mtm, "late");
    cvv.addSubview(&late);
    assert_eq!(take(), Vec::<String>::new());
    assert_eq!(style_of(&late), NORMAL, "a subview added later isn't told");
    cvt.setBackgroundStyle(NORMAL);
    assert_eq!(
        take(),
        ["cellview cv 0", "fieldcell direct 0", "fieldcell nested 0", "fieldcell outlet 0", "fieldcell late 0"]
    );

    // A row view gives its style to each subview as it's added, in the
    // same way: to cell views, which pass it on, and to controls' cells.
    let r = NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    r.setSelected(true);
    r.setEmphasized(true);
    let bare = logging_label(mtm, "bare");
    let deep = logging_label(mtm, "deep");
    let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    holder.addSubview(&deep);
    let outer = logging_cell_view(mtm, "outer");
    let inner = logging_cell_view(mtm, "inner");
    let outer_view: &NSView = &outer;
    outer_view.addSubview(&inner);
    take();
    r.addSubview(&styled_view(mtm));
    r.addSubview(&bare);
    r.addSubview(&holder);
    r.addSubview(&outer);
    assert_eq!(take(), ["fieldcell bare 1", "fieldcell deep 1", "cellview outer 1", "cellview inner 1"]);
}

fn row_interiors(mtm: MainThreadMarker) {
    let r = NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), NSRect::ZERO);
    assert!(!r.isEmphasized() && !r.isSelected());
    #[allow(deprecated)]
    let source_list = NSTableViewSelectionHighlightStyle::SourceList;
    let regular = NSTableViewSelectionHighlightStyle::Regular;
    let none = NSTableViewSelectionHighlightStyle::None;
    for (selected, emphasized, highlight, group, want) in [
        (true, true, regular, false, EMPHASIZED),
        (true, false, regular, false, NORMAL),
        (false, true, regular, false, NORMAL),
        (true, true, none, false, NORMAL),
        (true, true, source_list, false, EMPHASIZED),
        (true, true, regular, true, NORMAL),
        (false, false, regular, true, NORMAL),
    ] {
        r.setSelected(selected);
        r.setEmphasized(emphasized);
        r.setSelectionHighlightStyle(highlight);
        r.setGroupRowStyle(group);
        let got = r.interiorBackgroundStyle();
        assert_eq!(got, want, "selected {selected} emphasized {emphasized} highlight {highlight:?} group {group}");
    }
}

// Tables.

define_class!(
    /// Six rows of a logging cell view holding a logging label.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BackgroundsTableSource"]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSTableViewDataSource for Source {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            6
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
            log(format!("viewFor {row}"));
            let cv = logging_cell_view(mtm, &format!("r{row}"));
            let field = logging_label(mtm, &format!("f{row}"));
            let cvv: &NSView = &cv;
            cvv.addSubview(&field);
            let cvt: &NSTableCellView = &cv;
            // SAFETY: the outlet is a subview, which outlives its use here.
            unsafe { cvt.setTextField(Some(&field)) };
            Some(Retained::into_super(Retained::into_super(cv)))
        }

        #[unsafe(method(tableView:didAddRowView:forRow:))]
        fn did_add(&self, _table: &NSTableView, _row_view: &NSTableRowView, row: isize) {
            log(format!("didAdd {row}"));
        }
    }
);

/// A plain table of six rows in a scroll view in a window that is never
/// shown.
fn table(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<NSTableView>, Retained<Source>) {
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
    table_in(mtm, w)
}

/// A window that says it's key when `KeyWindow`'s ivar says so, never
/// shown.
fn key_window(mtm: MainThreadMarker) -> Retained<KeyWindow> {
    let w = KeyWindow::alloc(mtm).set_ivars(std::cell::Cell::new(false));
    // SAFETY: NSWindow's designated initializer; the window is never shown.
    unsafe {
        msg_send![
            super(w),
            initWithContentRect: rect(0.0, 0.0, 300.0, 200.0),
            styleMask: NSWindowStyleMask::Titled,
            backing: NSBackingStoreType::Buffered,
            defer: true
        ]
    }
}

/// The table of `table`, in `w`.
fn table_in(
    mtm: MainThreadMarker,
    w: Retained<NSWindow>,
) -> (Retained<NSWindow>, Retained<NSTableView>, Retained<Source>) {
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    t.setStyle(NSTableViewStyle::Plain);
    t.setHeaderView(None);
    let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("a"));
    c.setWidth(200.0);
    t.addTableColumn(&c);
    // SAFETY: the superclass's designated initializer.
    let src: Retained<Source> = unsafe { msg_send![Source::alloc(mtm), init] };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDataSource(Some(ProtocolObject::from_ref(&*src))) };
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(Some(ProtocolObject::from_ref(&*src))) };
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    scroll.setDocumentView(Some(&t));
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    w.setContentView(Some(&scroll));
    t.reloadData();
    (w, t, src)
}

fn close(w: &NSWindow, t: &NSTableView) {
    // SAFETY: the table doesn't retain these; they outlive its use of them.
    unsafe { t.setDelegate(None) };
    // SAFETY: as above.
    unsafe { t.setDataSource(None) };
    w.setContentView(None);
}

fn when_rows_restyle(mtm: MainThreadMarker) {
    // A row alone: a change reaches the subviews once, as the row is about
    // to draw, whatever changed in between.
    let r = NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), rect(0.0, 0.0, 100.0, 20.0));
    let cv = logging_cell_view(mtm, "c");
    r.addSubview(&cv);
    assert_eq!(take(), ["cellview c 0"]);
    r.setSelected(true);
    r.setEmphasized(true);
    assert_eq!(take(), Vec::<String>::new());
    let cvt: &NSTableCellView = &cv;
    assert_eq!(cvt.backgroundStyle(), NORMAL, "not told yet");
    r.layoutSubtreeIfNeeded();
    assert_eq!(take(), Vec::<String>::new(), "layout doesn't tell");
    r.viewWillDraw();
    assert_eq!(take(), ["cellview c 1"]);
    assert_eq!(cvt.backgroundStyle(), EMPHASIZED);
    r.setEmphasized(false);
    r.setEmphasized(true);
    r.setEmphasized(false);
    assert_eq!(take(), Vec::<String>::new());
    let _ = snapshot(&r, 1.0);
    assert_eq!(take(), ["cellview c 0"], "a snapshot prepares the row to draw");

    // In a table: each cell view is told as its row is made, between the
    // delegate making it and hearing of the row.
    let (w, t, _src) = table(mtm);
    w.layoutIfNeeded();
    let made: Vec<String> = (0..6)
        .flat_map(|r| {
            [format!("viewFor {r}"), format!("cellview r{r} 0"), format!("fieldcell f{r} 0"), format!("didAdd {r}")]
        })
        .collect();
    assert_eq!(take(), made);
    // The focus of a window that isn't key doesn't emphasize.
    assert!(w.makeFirstResponder(Some(&t)));
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
    let row = t.rowViewAtRow_makeIfNecessary(1, false).expect("row 1");
    assert!(row.isSelected() && !row.isEmphasized());
    take();
    // A change reaches the cells when the table hands one out.
    row.setEmphasized(true);
    let _ = t.rowViewAtRow_makeIfNecessary(1, false);
    assert_eq!(take(), Vec::<String>::new());
    let cell = t.viewAtColumn_row_makeIfNecessary(0, 1, false).expect("a cell view");
    assert_eq!(take(), ["cellview r1 1", "fieldcell f1 1"]);
    let cell: &NSTableCellView = cell.downcast_ref().expect("a cell view");
    // SAFETY: the outlet is set.
    let field = unsafe { cell.textField() }.expect("a text field");
    assert_eq!(style_of(&field), EMPHASIZED);
    w.makeFirstResponder(None);
    close(&w, &t);
    take();
}

fn key_focus(mtm: MainThreadMarker) {
    // The focus of a window that is key emphasizes the rows, and the cells
    // of the selected ones hear of it once, when the table hands one out.
    let kw = key_window(mtm);
    kw.ivars().set(true);
    let (w, t, _src) = table_in(mtm, Retained::into_super(kw.clone()));
    w.layoutIfNeeded();
    w.becomeKeyWindow();
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
    let row = t.rowViewAtRow_makeIfNecessary(1, false).expect("row 1");
    assert!(row.isSelected() && !row.isEmphasized());
    take();
    assert!(w.makeFirstResponder(Some(&t)));
    assert!(row.isEmphasized() && row.interiorBackgroundStyle() == EMPHASIZED);
    let other = t.rowViewAtRow_makeIfNecessary(0, false).expect("row 0");
    assert!(other.isEmphasized() && other.interiorBackgroundStyle() == NORMAL, "not selected");
    assert_eq!(take(), Vec::<String>::new());
    let _ = t.viewAtColumn_row_makeIfNecessary(0, 1, false);
    assert_eq!(take(), ["cellview r1 1", "fieldcell f1 1"]);
    let _ = t.viewAtColumn_row_makeIfNecessary(0, 1, false);
    assert_eq!(take(), Vec::<String>::new(), "told once");

    // Losing the focus takes it away.
    assert!(w.makeFirstResponder(None));
    assert!(!row.isEmphasized() && row.interiorBackgroundStyle() == NORMAL);
    let _ = t.viewAtColumn_row_makeIfNecessary(0, 1, false);
    assert_eq!(take(), ["cellview r1 0", "fieldcell f1 0"]);
    close(&w, &t);
    take();
}

fn snapshots_prepare(mtm: MainThreadMarker) {
    // Drawing a view into a bitmap lays out what needs it, hidden views
    // too, then sends viewWillDraw, before drawing.
    // SAFETY: NSView's designated initializer.
    let v: Retained<DrawLog> = unsafe { msg_send![DrawLog::alloc(mtm), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
    let parent = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    parent.addSubview(&v);
    // (macOS may draw a view twice the first time.)
    let _ = snapshot(&parent, 1.0);
    take();
    let _ = snapshot(&parent, 1.0);
    assert_eq!(take(), ["viewWillDraw", "drawRect"]);
    v.setNeedsLayout(true);
    let _ = snapshot(&parent, 1.0);
    assert_eq!(take(), ["layout", "viewWillDraw", "drawRect"]);
    v.setHidden(true);
    v.setNeedsLayout(true);
    let _ = snapshot(&parent, 1.0);
    assert_eq!(take(), ["layout"], "a hidden view is laid out, but not prepared or drawn");
}

fn source_lists(mtm: MainThreadMarker) {
    // The source list style brings its highlight; the other styles but the
    // automatic one take it away.
    #[allow(deprecated)]
    let source_list = NSTableViewSelectionHighlightStyle::SourceList;
    let regular = NSTableViewSelectionHighlightStyle::Regular;
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    assert_eq!(t.selectionHighlightStyle(), regular);
    t.setStyle(NSTableViewStyle::SourceList);
    assert_eq!((t.selectionHighlightStyle(), t.effectiveStyle()), (source_list, NSTableViewStyle::SourceList));
    t.setStyle(NSTableViewStyle::Automatic);
    assert_eq!((t.selectionHighlightStyle(), t.effectiveStyle()), (source_list, NSTableViewStyle::SourceList));
    t.setStyle(NSTableViewStyle::Plain);
    assert_eq!((t.selectionHighlightStyle(), t.effectiveStyle()), (regular, NSTableViewStyle::Plain));
    t.setStyle(NSTableViewStyle::SourceList);
    t.setSelectionHighlightStyle(regular);
    assert_eq!((t.style(), t.effectiveStyle()), (NSTableViewStyle::SourceList, NSTableViewStyle::SourceList));
    let t = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
    t.setSelectionHighlightStyle(NSTableViewSelectionHighlightStyle::None);
    t.setStyle(NSTableViewStyle::SourceList);
    assert_eq!(t.selectionHighlightStyle(), source_list);

    // A source list's selected rows are emphasized as others are, but not
    // its group rows.
    let (w, t, _src) = table(mtm);
    t.setStyle(NSTableViewStyle::SourceList);
    w.layoutIfNeeded();
    t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
    let row = t.rowViewAtRow_makeIfNecessary(1, false).expect("row 1");
    assert_eq!(row.selectionHighlightStyle(), source_list);
    assert_eq!(row.interiorBackgroundStyle(), NORMAL);
    row.setEmphasized(true);
    assert_eq!(row.interiorBackgroundStyle(), EMPHASIZED);
    row.setGroupRowStyle(true);
    assert_eq!(row.interiorBackgroundStyle(), NORMAL);
    close(&w, &t);
    take();
}

// Colors.

/// The strongest pixel of `rep` within `area` (in pixels from the top
/// left; all of it if none), unpremultiplied: the color text was drawn in.
fn ink_in(rep: &NSBitmapImageRep, area: Option<NSRect>) -> [u8; 4] {
    let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
    let (x0, y0, x1, y1) = match area {
        Some(a) => (
            (a.origin.x as isize).max(0),
            (a.origin.y as isize).max(0),
            ((a.origin.x + a.size.width) as isize).min(w),
            ((a.origin.y + a.size.height) as isize).min(h),
        ),
        None => (0, 0, w, h),
    };
    let mut best = [0u8; 4];
    for y in y0..y1 {
        for x in x0..x1 {
            let p = pixel(rep, x, y);
            if p[3] > best[3] {
                best = p;
            }
        }
    }
    if best[3] == 0 {
        return best;
    }
    let a = f64::from(best[3]) / 255.0;
    let un = |c: u8| (f64::from(c) / a).round().min(255.0) as u8;
    [un(best[0]), un(best[1]), un(best[2]), best[3]]
}

fn ink(rep: &NSBitmapImageRep) -> [u8; 4] {
    ink_in(rep, None)
}

/// Near white (and so light on the selection), and seen.
fn light(c: [u8; 4]) -> bool {
    c[..3].iter().all(|&v| v >= 200) && c[3] > 20
}

/// Near black, and seen.
fn dark_ink(c: [u8; 4]) -> bool {
    c[..3].iter().all(|&v| v <= 60) && c[3] > 20
}

fn same(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 3)
}

fn big() -> Retained<NSFont> {
    NSFont::boldSystemFontOfSize(48.0)
}

/// `control` (framed 80 by 60) drawn in `appearance` with its cell on
/// `style`: the ink of its text.
fn drawn(mtm: MainThreadMarker, appearance: &NSAppearance, control: &NSControl, style: NSBackgroundStyle) -> [u8; 4] {
    let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 80.0, 60.0));
    holder.setAppearance(Some(appearance));
    control.setFrame(rect(0.0, 0.0, 80.0, 60.0));
    holder.addSubview(control);
    control.cell().expect("a cell").setBackgroundStyle(style);
    let rep = snapshot(&holder, 1.0);
    control.removeFromSuperview();
    ink(&rep)
}

fn label_in(mtm: MainThreadMarker, color: Option<&NSColor>) -> Retained<NSTextField> {
    let f = NSTextField::labelWithString(&NSString::from_str("M"), mtm);
    f.setFont(Some(&big()));
    if let Some(c) = color {
        f.setTextColor(Some(c));
    }
    f
}

fn attributed(text: &str, color: &NSColor) -> Retained<NSAttributedString> {
    let font = big();
    // SAFETY: the keys are constant strings.
    let keys = unsafe { [NSForegroundColorAttributeName, NSFontAttributeName] };
    let attrs = NSDictionary::from_slices(&keys, &[color as &AnyObject, &*font as &AnyObject]);
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), &attrs) }
}

/// A color whose provider answers `answer`'s color, whatever the
/// appearance.
fn dynamic(answer: fn() -> Retained<NSColor>) -> Retained<NSColor> {
    let provider = block2::RcBlock::new(move |_: std::ptr::NonNull<NSAppearance>| {
        std::ptr::NonNull::new(Retained::autorelease_return(answer())).expect("a color")
    });
    // SAFETY: the provider returns an autoreleased color.
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

/// What `make` makes with `appearance` the drawing appearance: how a
/// system color is fixed to an appearance.
fn made_in(appearance: &NSAppearance, make: impl Fn() -> Retained<NSColor>) -> Retained<NSColor> {
    let made = RefCell::new(None);
    appearance.performAsCurrentDrawingAppearance(&block2::RcBlock::new(|| *made.borrow_mut() = Some(make())));
    made.into_inner().expect("made")
}

fn emphasized_text(mtm: MainThreadMarker) {
    let labels = [
        ("label", NSColor::labelColor()),
        ("secondary", NSColor::secondaryLabelColor()),
        ("tertiary", NSColor::tertiaryLabelColor()),
        ("quaternary", NSColor::quaternaryLabelColor()),
    ];
    let stand_ins = [
        ("controlText", NSColor::controlTextColor()),
        ("headerText", NSColor::headerTextColor()),
        ("selectedControlText", NSColor::selectedControlTextColor()),
    ];
    let staying = [
        ("text", NSColor::textColor()),
        ("link", NSColor::linkColor()),
        ("systemRed", NSColor::systemRedColor()),
        ("red", NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0)),
        ("gray", NSColor::colorWithSRGBRed_green_blue_alpha(0.5, 0.5, 0.5, 1.0)),
    ];
    for (name, appearance) in [("aqua", aqua()), ("dark", dark())] {
        // The label colors turn light, and keep their order.
        let mut strengths = Vec::new();
        for (color, c) in &labels {
            let f = label_in(mtm, Some(c));
            let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
            assert!(light(on), "{name} {color}: {on:?} on the selection");
            if name == "aqua" {
                assert!(dark_ink(normal), "{name} {color}: {normal:?}");
            }
            strengths.push(on[3]);
        }
        assert!(strengths.windows(2).all(|w| w[0] > w[1]), "{name}: {strengths:?}");
        // The label's own color too, and the text colors that stand in for
        // the label's.
        let f = label_in(mtm, None);
        let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
        assert!(light(on) && on[3] >= normal[3], "{name} default: {normal:?} {on:?}");
        for (color, c) in &stand_ins {
            let on = drawn(mtm, &appearance, &label_in(mtm, Some(c)), EMPHASIZED);
            assert!(light(on) && on[3] > 200, "{name} {color}: {on:?}");
        }
        // Other colors stay, and so do the lowered and raised styles.
        for (color, c) in &staying {
            let f = label_in(mtm, Some(c));
            let normal = drawn(mtm, &appearance, &f, NORMAL);
            for style in [EMPHASIZED, NSBackgroundStyle::Raised, NSBackgroundStyle::Lowered] {
                let got = drawn(mtm, &appearance, &f, style);
                assert!(same(got, normal), "{name} {color} on {style:?}: {got:?}, not {normal:?}");
            }
        }
        let f = label_in(mtm, Some(&NSColor::labelColor()));
        let normal = drawn(mtm, &appearance, &f, NORMAL);
        for style in [NSBackgroundStyle::Raised, NSBackgroundStyle::Lowered] {
            assert!(same(drawn(mtm, &appearance, &f, style), normal), "{name} label on {style:?}");
        }
        // Attributed runs are mapped alike.
        let f = NSTextField::labelWithAttributedString(&attributed("M", &NSColor::secondaryLabelColor()), mtm);
        let on = drawn(mtm, &appearance, &f, EMPHASIZED);
        assert!(light(on), "{name} attributed secondary: {on:?}");
        let f = NSTextField::labelWithAttributedString(&attributed("M", &NSColor::systemRedColor()), mtm);
        let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
        assert!(same(on, normal), "{name} attributed red: {on:?}, not {normal:?}");
        // The label color's value turns light, whatever made it: a provider
        // answering it, its components. Colors made from the other label
        // colors, or from the label color with another alpha, stay; and in
        // attributed runs only the label colors themselves change.
        let components = |of: fn() -> Retained<NSColor>| {
            made_in(&appearance, || of().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).expect("sRGB"))
        };
        let provided = dynamic(NSColor::labelColor);
        for (color, c) in [("provided label", provided), ("label's components", components(NSColor::labelColor))] {
            let on = drawn(mtm, &appearance, &label_in(mtm, Some(&c)), EMPHASIZED);
            assert!(light(on) && on[3] > 200, "{name} {color}: {on:?}");
        }
        let half = || NSColor::labelColor().colorWithAlphaComponent(0.5);
        let made_from = [
            ("provided secondary", dynamic(NSColor::secondaryLabelColor)),
            ("provided tertiary", dynamic(NSColor::tertiaryLabelColor)),
            ("provided label, half", dynamic(NSColor::labelColor).colorWithAlphaComponent(0.5)),
            ("label, half", made_in(&appearance, half)),
            ("secondary's components", components(NSColor::secondaryLabelColor)),
        ];
        for (color, c) in &made_from {
            let f = label_in(mtm, Some(c));
            let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
            assert!(same(on, normal), "{name} {color}: {on:?}, not {normal:?}");
            if name == "aqua" {
                assert!(dark_ink(normal), "{name} {color}: {normal:?}");
            }
        }
        let f = NSTextField::labelWithAttributedString(&attributed("M", &dynamic(NSColor::labelColor)), mtm);
        let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
        assert!(same(on, normal), "{name} attributed provided label: {on:?}, not {normal:?}");
        // Disabled text turns an opaque light gray, weaker than a label.
        let f = label_in(mtm, Some(&NSColor::disabledControlTextColor()));
        let on = drawn(mtm, &appearance, &f, EMPHASIZED);
        let label = drawn(mtm, &appearance, &label_in(mtm, None), EMPHASIZED);
        assert!(light(on) && on[3] > 240 && on[0] < label[0], "{name} disabled text: {on:?}, the label {label:?}");
        // The placeholder stays.
        let f = NSTextField::textFieldWithString(&NSString::new(), mtm);
        f.setFont(Some(&big()));
        f.setBezeled(false);
        f.setDrawsBackground(false);
        f.setPlaceholderString(Some(&NSString::from_str("M")));
        let (normal, on) = (drawn(mtm, &appearance, &f, NORMAL), drawn(mtm, &appearance, &f, EMPHASIZED));
        assert!(same(on, normal), "{name} placeholder: {on:?}, not {normal:?}");
        // The text color property doesn't change.
        let f = label_in(mtm, None);
        f.cell().expect("a cell").setBackgroundStyle(EMPHASIZED);
        assert!(f.textColor().expect("a color").isEqual(Some(&NSColor::labelColor())));
    }
}

/// Buttons titled "M" in a big font, with the interior styles macOS gives
/// them on a normal and an emphasized background.
fn buttons(mtm: MainThreadMarker) -> Vec<(&'static str, Retained<NSButton>, NSBackgroundStyle, NSBackgroundStyle)> {
    let m = NSString::from_str("M");
    // SAFETY: no target or action.
    let push = || unsafe { NSButton::buttonWithTitle_target_action(&m, None, None, mtm) };
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&m, None, None, mtm) };
    // SAFETY: no target or action.
    let radio = unsafe { NSButton::radioButtonWithTitle_target_action(&m, None, None, mtm) };
    let borderless = push();
    borderless.setBordered(false);
    let accessory = push();
    accessory.setBezelStyle(NSBezelStyle::AccessoryBar);
    let badge = push();
    badge.setBezelStyle(NSBezelStyle::Badge);
    let toggle = push();
    toggle.setButtonType(NSButtonType::PushOnPushOff);
    toggle.setBordered(false);
    toggle.setState(1);
    let all = vec![
        ("check box", check, NORMAL, NORMAL),
        ("radio button", radio, NORMAL, NORMAL),
        ("push button", push(), NORMAL, NORMAL),
        ("accessory bar", accessory, NORMAL, NORMAL),
        ("borderless", borderless, NORMAL, EMPHASIZED),
        ("borderless toggle, on", toggle, NORMAL, EMPHASIZED),
        ("badge", badge, EMPHASIZED, EMPHASIZED),
    ];
    for (_, b, _, _) in &all {
        b.setFont(Some(&big()));
    }
    all
}

fn interior_styles(mtm: MainThreadMarker) {
    // A text field's text is on the background only when the field draws
    // none of its own.
    let field = |set: &dyn Fn(&NSTextField)| {
        let f = NSTextField::labelWithString(&NSString::from_str("x"), mtm);
        set(&f);
        let c = f.cell().expect("a cell");
        c.setBackgroundStyle(EMPHASIZED);
        c.interiorBackgroundStyle()
    };
    assert_eq!(field(&|_| {}), EMPHASIZED);
    assert_eq!(field(&|f| f.setBordered(true)), EMPHASIZED);
    assert_eq!(field(&|f| f.setEditable(true)), EMPHASIZED);
    assert_eq!(field(&|f| f.setSelectable(true)), EMPHASIZED);
    assert_eq!(field(&|f| f.setBezeled(true)), NORMAL);
    assert_eq!(field(&|f| f.setDrawsBackground(true)), NORMAL);
    let f = NSTextField::textFieldWithString(&NSString::from_str("x"), mtm);
    let c = f.cell().expect("a cell");
    c.setBackgroundStyle(EMPHASIZED);
    assert_eq!(c.interiorBackgroundStyle(), NORMAL);

    // A button's content is on its own bezel or box, but a borderless
    // button's is on the background, and a badge is always dark.
    for (name, button, want_normal, want_emphasized) in buttons(mtm) {
        let c = button.cell().expect("a cell");
        c.setBackgroundStyle(NORMAL);
        assert_eq!(c.interiorBackgroundStyle(), want_normal, "{name}");
        c.setBackgroundStyle(EMPHASIZED);
        assert_eq!(c.interiorBackgroundStyle(), want_emphasized, "{name}");
    }
}

/// `cell`'s interior (not its bezel) drawn 120 by 80 in `appearance` on
/// `style`: the ink within its title rect.
fn interior(mtm: MainThreadMarker, appearance: &NSAppearance, cell: &NSCell, style: NSBackgroundStyle) -> [u8; 4] {
    cell.setBackgroundStyle(style);
    let c = cell.retain();
    let v = draw_view(mtm, rect(0.0, 0.0, 120.0, 80.0), true, move |view, _| {
        c.drawInteriorWithFrame_inView(view.bounds(), view);
    });
    v.setAppearance(Some(appearance));
    let title = cell.titleRectForBounds(rect(0.0, 0.0, 120.0, 80.0));
    ink_in(&snapshot(&v, 1.0), Some(title))
}

fn button_titles(mtm: MainThreadMarker) {
    let a = aqua();
    // Every title but a badge's turns light on the selection,
    // whatever the interior style.
    for (name, button, _, _) in buttons(mtm) {
        if name == "badge" {
            continue;
        }
        let cell = button.cell().expect("a cell");
        let (normal, on) = (interior(mtm, &a, &cell, NORMAL), interior(mtm, &a, &cell, EMPHASIZED));
        assert!(dark_ink(normal), "{name}: {normal:?}");
        assert!(light(on), "{name}: {on:?} on the selection");
    }
    // A content tint gives way to it; a disabled title is weaker there.
    let (_, borderless, _, _) = buttons(mtm).into_iter().find(|b| b.0 == "borderless").expect("a borderless button");
    borderless.setContentTintColor(Some(&NSColor::systemRedColor()));
    let cell = borderless.cell().expect("a cell");
    let tinted = interior(mtm, &a, &cell, NORMAL);
    assert!(tinted[0] > 200 && tinted[1] < 120 && tinted[2] < 120, "red: {tinted:?}");
    let on = interior(mtm, &a, &cell, EMPHASIZED);
    assert!(light(on), "tinted on the selection: {on:?}");
    borderless.setContentTintColor(None);
    let enabled = interior(mtm, &a, &cell, EMPHASIZED);
    borderless.setEnabled(false);
    let disabled = interior(mtm, &a, &cell, EMPHASIZED);
    assert!(light(disabled) && disabled[3] < enabled[3], "{disabled:?} against {enabled:?}");

    // A push button's bezel is washed light there, more so pressed; a
    // default button's stays the accent.
    let bezel = |key: bool, pressed: bool, style| {
        // SAFETY: no target or action.
        let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("M"), None, None, mtm) };
        if key {
            b.setKeyEquivalent(&NSString::from_str("\r"));
        }
        b.setFrame(rect(0.0, 0.0, 80.0, 32.0));
        let cell = b.cell().expect("a cell");
        cell.setHighlighted(pressed);
        cell.setBackgroundStyle(style);
        let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 80.0, 32.0));
        holder.setAppearance(Some(&a));
        holder.addSubview(&b);
        // Beside the title, halfway down.
        pixel(&snapshot(&holder, 1.0), 16, 16)
    };
    // How light a premultiplied pixel looks over mid gray.
    let over_gray = |p: [u8; 4]| u32::from(p[0]) + (255 - u32::from(p[3])) * 128 / 255;
    let (normal, on) = (bezel(false, false, NORMAL), bezel(false, false, EMPHASIZED));
    assert!(over_gray(on) > over_gray(normal) + 10, "{normal:?} {on:?}");
    let pressed = bezel(false, true, EMPHASIZED);
    assert!(over_gray(pressed) > over_gray(on) + 10, "{on:?} {pressed:?}");
    let (normal, on) = (bezel(true, false, NORMAL), bezel(true, false, EMPHASIZED));
    assert!(same(normal, on) && normal[3] == 255, "{normal:?} {on:?}");
}

fn generic_cells(mtm: MainThreadMarker) {
    // A plain text cell's text turns light on the selection.
    for (name, appearance) in [("aqua", aqua()), ("dark", dark())] {
        let cell = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::from_str("M"));
        cell.setFont(Some(&big()));
        let mut inks = Vec::new();
        for style in [NORMAL, EMPHASIZED] {
            cell.setBackgroundStyle(style);
            let c = cell.clone();
            let v = draw_view(mtm, rect(0.0, 0.0, 80.0, 60.0), false, move |view, _| {
                c.drawWithFrame_inView(view.bounds(), view);
            });
            v.setAppearance(Some(&appearance));
            inks.push(ink(&snapshot(&v, 1.0)));
        }
        assert!(light(inks[1]), "{name}: {:?}", inks[1]);
        if name == "aqua" {
            assert!(dark_ink(inks[0]), "{name}: {:?}", inks[0]);
        }
        // Disabled, weaker there as anywhere else.
        cell.setEnabled(false);
        cell.setBackgroundStyle(EMPHASIZED);
        let c = cell.clone();
        let v = draw_view(mtm, rect(0.0, 0.0, 80.0, 60.0), false, move |view, _| {
            c.drawWithFrame_inView(view.bounds(), view);
        });
        v.setAppearance(Some(&appearance));
        let disabled = ink(&snapshot(&v, 1.0));
        assert!(light(disabled) && disabled[3] < inks[1][3], "{name}: {disabled:?} against {:?}", inks[1]);
    }

    // What a cell or a view draws itself doesn't change.
    let a = aqua();
    let fill_ink = |style| {
        // SAFETY: NSCell's initializer.
        let c: Retained<FillCell> = unsafe { msg_send![FillCell::alloc(mtm), initTextCell: &*NSString::from_str("x")] };
        let c: Retained<NSCell> = Retained::into_super(c);
        c.setBackgroundStyle(style);
        let v = draw_view(mtm, rect(0.0, 0.0, 20.0, 20.0), true, move |view, _| {
            c.drawWithFrame_inView(view.bounds(), view);
        });
        v.setAppearance(Some(&a));
        pixel(&snapshot(&v, 1.0), 5, 5)
    };
    let (normal, on) = (fill_ink(NORMAL), fill_ink(EMPHASIZED));
    assert!(same(normal, on) && normal[3] > 0, "{normal:?} {on:?}");
    let cv = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), rect(0.0, 0.0, 20.0, 20.0));
    cv.setAppearance(Some(&a));
    let own = draw_view(mtm, rect(0.0, 0.0, 20.0, 20.0), true, |_, _| {
        NSColor::labelColor().setFill();
        NSRectFill(rect(0.0, 0.0, 10.0, 10.0));
    });
    cv.addSubview(&own);
    let before = pixel(&snapshot(&cv, 1.0), 5, 5);
    cv.setBackgroundStyle(EMPHASIZED);
    assert!(same(pixel(&snapshot(&cv, 1.0), 5, 5), before));
    // Nor do the label colors in a plain cell's attributed value.
    let value = attributed("M", &NSColor::labelColor());
    let inks: Vec<[u8; 4]> = [NORMAL, EMPHASIZED]
        .into_iter()
        .map(|style| {
            let c = NSCell::initTextCell(NSCell::alloc(mtm), &NSString::new());
            c.setAttributedStringValue(&value);
            c.setBackgroundStyle(style);
            let v = draw_view(mtm, rect(0.0, 0.0, 80.0, 60.0), false, move |view, _| {
                c.drawWithFrame_inView(view.bounds(), view);
            });
            v.setAppearance(Some(&a));
            ink(&snapshot(&v, 1.0))
        })
        .collect();
    assert!(same(inks[0], inks[1]) && dark_ink(inks[1]), "{inks:?}");
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("who_takes_styles", who_takes_styles),
        ("row_interiors", row_interiors),
        ("when_rows_restyle", when_rows_restyle),
        ("key_focus", key_focus),
        ("snapshots_prepare", snapshots_prepare),
        ("source_lists", source_lists),
        ("emphasized_text", emphasized_text),
        ("interior_styles", interior_styles),
        ("button_titles", button_titles),
        ("generic_cells", generic_cells),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
