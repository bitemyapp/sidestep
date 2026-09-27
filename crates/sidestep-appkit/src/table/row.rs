//! The views a table is made of: `NSTableRowView` (a row: its background,
//! selection and the cell views of its columns), `NSTableCellView` (a
//! cell with an object value and outlets for a text field and an image),
//! and `NSTableHeaderView`, a shell until scroll views place headers.
//!
//! **Background styles.** A row view gives its subviews its
//! `interiorBackgroundStyle` (emphasized while it's selected in the key
//! window's focused table, unless it's a group row or the table highlights
//! nothing), so that cells draw light on a strong selection: each subview
//! as it's added, and all of them before the row next draws (or the table
//! hands out one of its cell views) after a change, once however many
//! changes there were. A cell view takes the style and passes it down to
//! the cells of the controls under it, as the row view does for views
//! that aren't cell views; other views aren't told. All as measured on
//! macOS (`conformance/tests/cell_backgrounds.rs`).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackgroundStyle, NSBezierPath, NSColor, NSControl, NSResponder, NSTableCellView, NSTableView,
    NSTableViewRowSizeStyle, NSTableViewSelectionHighlightStyle, NSTableViewStyle, NSView,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::palette::System;
use crate::views;

// NSTableRowView

pub(crate) struct RowIvars {
    selected: Cell<bool>,
    previous_selected: Cell<bool>,
    next_selected: Cell<bool>,
    emphasized: Cell<bool>,
    group: Cell<bool>,
    floating: Cell<bool>,
    drop_target: Cell<bool>,
    highlight: Cell<NSTableViewSelectionHighlightStyle>,
    background: RefCell<Option<Retained<NSColor>>>,
    indentation: Cell<f64>,
    /// Something the interior style follows changed since the subviews
    /// were last given it.
    restyle: Cell<bool>,
}

impl Default for RowIvars {
    fn default() -> Self {
        RowIvars {
            selected: Cell::new(false),
            previous_selected: Cell::new(false),
            next_selected: Cell::new(false),
            emphasized: Cell::new(false),
            group: Cell::new(false),
            floating: Cell::new(false),
            drop_target: Cell::new(false),
            highlight: Cell::new(NSTableViewSelectionHighlightStyle::Regular),
            background: RefCell::new(None),
            indentation: Cell::new(0.0),
            restyle: Cell::new(false),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTableRowView"]
    #[ivars = RowIvars]
    pub(crate) struct NSTableRowViewImpl;

    impl NSTableRowViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(RowIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isSelected))]
        fn is_selected(&self) -> bool {
            self.ivars().selected.get()
        }

        #[unsafe(method(setSelected:))]
        fn set_selected(&self, flag: bool) {
            if self.ivars().selected.replace(flag) != flag {
                self.restyle();
            }
        }

        #[unsafe(method(isPreviousRowSelected))]
        fn is_previous_row_selected(&self) -> bool {
            self.ivars().previous_selected.get()
        }

        #[unsafe(method(setPreviousRowSelected:))]
        fn set_previous_row_selected(&self, flag: bool) {
            self.ivars().previous_selected.set(flag);
        }

        #[unsafe(method(isNextRowSelected))]
        fn is_next_row_selected(&self) -> bool {
            self.ivars().next_selected.get()
        }

        #[unsafe(method(setNextRowSelected:))]
        fn set_next_row_selected(&self, flag: bool) {
            self.ivars().next_selected.set(flag);
        }

        #[unsafe(method(isEmphasized))]
        fn is_emphasized(&self) -> bool {
            self.ivars().emphasized.get()
        }

        #[unsafe(method(setEmphasized:))]
        fn set_emphasized(&self, flag: bool) {
            if self.ivars().emphasized.replace(flag) != flag {
                self.restyle();
            }
        }

        #[unsafe(method(didAddSubview:))]
        fn did_add_subview(&self, subview: &NSView) {
            // SAFETY: NSView's didAddSubview: takes the subview.
            let _: () = unsafe { msg_send![super(self), didAddSubview: subview] };
            give_style(subview, self.as_row().interiorBackgroundStyle());
        }

        /// Before drawing, the subviews learn the style they draw on.
        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            self.give_style_if_changed();
            // SAFETY: NSView's viewWillDraw takes nothing.
            let _: () = unsafe { msg_send![super(self), viewWillDraw] };
        }

        #[unsafe(method(isGroupRowStyle))]
        fn is_group_row_style(&self) -> bool {
            self.ivars().group.get()
        }

        #[unsafe(method(setGroupRowStyle:))]
        fn set_group_row_style(&self, flag: bool) {
            if self.ivars().group.replace(flag) != flag {
                self.restyle();
            }
        }

        #[unsafe(method(isFloating))]
        fn is_floating(&self) -> bool {
            self.ivars().floating.get()
        }

        #[unsafe(method(setFloating:))]
        fn set_floating(&self, flag: bool) {
            self.ivars().floating.set(flag);
        }

        #[unsafe(method(isTargetForDropOperation))]
        fn is_target_for_drop_operation(&self) -> bool {
            self.ivars().drop_target.get()
        }

        #[unsafe(method(setTargetForDropOperation:))]
        fn set_target_for_drop_operation(&self, flag: bool) {
            self.ivars().drop_target.set(flag);
        }

        #[unsafe(method(indentationForDropOperation))]
        fn indentation_for_drop_operation(&self) -> f64 {
            self.ivars().indentation.get()
        }

        #[unsafe(method(setIndentationForDropOperation:))]
        fn set_indentation_for_drop_operation(&self, indentation: f64) {
            self.ivars().indentation.set(indentation);
        }

        #[unsafe(method(selectionHighlightStyle))]
        fn selection_highlight_style(&self) -> NSTableViewSelectionHighlightStyle {
            self.ivars().highlight.get()
        }

        #[unsafe(method(setSelectionHighlightStyle:))]
        fn set_selection_highlight_style(&self, style: NSTableViewSelectionHighlightStyle) {
            if self.ivars().highlight.replace(style) != style {
                self.restyle();
            }
        }

        /// Emphasized on a strong selection: selected and emphasized,
        /// highlighted (regularly or as a source list) and not a group
        /// row, as on macOS (`conformance/tests/cell_backgrounds.rs`,
        /// `row_interiors`).
        #[unsafe(method(interiorBackgroundStyle))]
        fn interior_background_style(&self) -> NSBackgroundStyle {
            let x = self.ivars();
            let highlighted = x.highlight.get() != NSTableViewSelectionHighlightStyle::None;
            if x.selected.get() && x.emphasized.get() && highlighted && !x.group.get() {
                NSBackgroundStyle::Emphasized
            } else {
                NSBackgroundStyle::Normal
            }
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            let color = self.ivars().background.borrow().clone();
            color.unwrap_or_else(NSColor::clearColor)
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: &NSColor) {
            self.ivars().background.replace(Some(color.retain()));
            self.redraw();
        }

        #[unsafe(method(drawBackgroundInRect:))]
        fn draw_background_in_rect(&self, dirty: NSRect) {
            if let Some(color) = self.ivars().background.borrow().clone() {
                color.setFill();
                NSBezierPath::fillRect(dirty);
            }
        }

        /// The accent while emphasized, a gray while not; rounded in the
        /// inset and source list styles, whose rows stand apart from the
        /// table's edges.
        #[unsafe(method(drawSelectionInRect:))]
        fn draw_selection_in_rect(&self, _dirty: NSRect) {
            if self.ivars().highlight.get() == NSTableViewSelectionHighlightStyle::None {
                return;
            }
            let color = if self.ivars().emphasized.get() {
                crate::color::catalog(System::SelectedContentBackground)
            } else {
                crate::color::catalog(System::UnemphasizedSelectedContentBackground)
            };
            color.setFill();
            let bounds = self.as_view().bounds();
            let table = views::superview_of(views::imp(self.as_view())).and_then(|t| {
                views::as_view(t).downcast_ref::<NSTableView>().map(|t| t.effectiveStyle())
            });
            if matches!(table, Some(NSTableViewStyle::Inset | NSTableViewStyle::SourceList)) {
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, 5.0, 5.0).fill();
            } else {
                NSBezierPath::fillRect(bounds);
            }
        }

        #[unsafe(method(drawSeparatorInRect:))]
        fn draw_separator_in_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(drawDraggingDestinationFeedbackInRect:))]
        fn draw_dragging_destination_feedback_in_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let row = self.as_row();
            row.drawBackgroundInRect(dirty);
            if self.ivars().selected.get() {
                row.drawSelectionInRect(dirty);
            }
            row.drawSeparatorInRect(dirty);
        }

        #[unsafe(method_id(viewAtColumn:))]
        fn view_at_column(&self, column: isize) -> Option<Retained<AnyObject>> {
            let subviews = views::subviews(views::imp(self.as_view()));
            let view = usize::try_from(column).ok().and_then(|c| subviews.get(c).cloned());
            view.map(|v| Retained::into_super(Retained::into_super(Retained::into_super(v))))
        }

        #[unsafe(method(numberOfColumns))]
        fn number_of_columns(&self) -> isize {
            views::subviews(views::imp(self.as_view())).len() as isize
        }
    }

    unsafe impl NSObjectProtocol for NSTableRowViewImpl {}
);

impl NSTableRowViewImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: a row view is an NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn as_row(&self) -> &objc2_app_kit::NSTableRowView {
        // SAFETY: NSTableRowView is this class.
        unsafe { &*(self as *const Self).cast::<objc2_app_kit::NSTableRowView>() }
    }

    fn redraw(&self) {
        self.as_view().setNeedsDisplay(true);
    }

    /// Something the look and the interior style follow changed: redraw,
    /// and give the subviews the style before then.
    fn restyle(&self) {
        self.ivars().restyle.set(true);
        self.redraw();
    }

    /// Give the subviews the row's style, if it may have changed since
    /// they were given it.
    fn give_style_if_changed(&self) {
        if self.ivars().restyle.replace(false) {
            let style = self.as_row().interiorBackgroundStyle();
            for sub in views::subviews(views::imp(self.as_view())) {
                give_style(&sub, style);
            }
        }
    }
}

/// Bring `row`'s subviews up to date with its style (the table does before
/// handing out one of them).
pub(crate) fn give_style_if_changed(row: &NSView) {
    if let Some(row) = row.downcast_ref::<objc2_app_kit::NSTableRowView>() {
        // SAFETY: NSTableRowView is NSTableRowViewImpl's class; subclasses
        // share its layout.
        let row = unsafe { &*(row as *const objc2_app_kit::NSTableRowView).cast::<NSTableRowViewImpl>() };
        row.give_style_if_changed();
    }
}

/// Give `view` a background style as AppKit's row and cell views do: a
/// cell view takes it (and passes it on), a control's cell takes it, and
/// any other view passes it to its subviews. Nothing else is told, even a
/// view that has a `setBackgroundStyle:` of its own.
fn give_style(view: &NSView, style: NSBackgroundStyle) {
    if view.isKindOfClass(NSTableCellView::class()) {
        // SAFETY: a cell view's setBackgroundStyle: takes the style.
        let _: () = unsafe { msg_send![view, setBackgroundStyle: style] };
    } else if let Some(control) = view.downcast_ref::<NSControl>() {
        if let Some(cell) = control.cell() {
            cell.setBackgroundStyle(style);
        }
    } else {
        for sub in views::subviews(views::imp(view)) {
            give_style(&sub, style);
        }
    }
}

// NSTableCellView

pub(crate) struct CellIvars {
    object: RefCell<Option<Retained<AnyObject>>>,
    /// Outlets, not retained beyond the view tree, as AppKit's are weak.
    text_field: RefCell<Option<objc2::rc::Weak<NSView>>>,
    image_view: RefCell<Option<objc2::rc::Weak<NSView>>>,
    background: Cell<NSBackgroundStyle>,
    size_style: Cell<NSTableViewRowSizeStyle>,
}

impl Default for CellIvars {
    fn default() -> Self {
        CellIvars {
            object: RefCell::new(None),
            text_field: RefCell::new(None),
            image_view: RefCell::new(None),
            background: Cell::new(NSBackgroundStyle::Normal),
            size_style: Cell::new(NSTableViewRowSizeStyle::Default),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTableCellView"]
    #[ivars = CellIvars]
    pub(crate) struct NSTableCellViewImpl;

    impl NSTableCellViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(CellIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            self.ivars().object.borrow().clone()
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, value: Option<&AnyObject>) {
            let old = self.ivars().object.replace(value.map(|v| v.retain()));
            drop(old);
        }

        #[unsafe(method_id(textField))]
        fn text_field(&self) -> Option<Retained<NSView>> {
            self.ivars().text_field.borrow().as_ref().and_then(objc2::rc::Weak::load)
        }

        #[unsafe(method(setTextField:))]
        fn set_text_field(&self, view: Option<&NSView>) {
            self.ivars().text_field.replace(view.map(objc2::rc::Weak::new));
        }

        #[unsafe(method_id(imageView))]
        fn image_view(&self) -> Option<Retained<NSView>> {
            self.ivars().image_view.borrow().as_ref().and_then(objc2::rc::Weak::load)
        }

        #[unsafe(method(setImageView:))]
        fn set_image_view(&self, view: Option<&NSView>) {
            self.ivars().image_view.replace(view.map(objc2::rc::Weak::new));
        }

        #[unsafe(method(backgroundStyle))]
        fn background_style(&self) -> NSBackgroundStyle {
            self.ivars().background.get()
        }

        /// Taken, and passed down to the controls' cells under the cell
        /// view (and any cell views there), every time, as on macOS.
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            self.ivars().background.set(style);
            // SAFETY: a cell view is an NSView.
            let view = unsafe { &*(self as *const Self).cast::<NSView>() };
            for sub in views::subviews(views::imp(view)) {
                give_style(&sub, style);
            }
        }

        #[unsafe(method(rowSizeStyle))]
        fn row_size_style(&self) -> NSTableViewRowSizeStyle {
            self.ivars().size_style.get()
        }

        #[unsafe(method(setRowSizeStyle:))]
        fn set_row_size_style(&self, style: NSTableViewRowSizeStyle) {
            self.ivars().size_style.set(style);
        }
    }

    unsafe impl NSObjectProtocol for NSTableCellViewImpl {}
);

// NSTableHeaderView

#[derive(Default)]
pub(crate) struct HeaderIvars {
    /// Weak, as AppKit's: a header that outlives its table has none.
    table: RefCell<Option<Weak<NSTableView>>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTableHeaderView"]
    #[ivars = HeaderIvars]
    pub(crate) struct NSTableHeaderViewImpl;

    impl NSTableHeaderViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(HeaderIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(tableView))]
        fn table_view(&self) -> Option<Retained<NSTableView>> {
            self.table()
        }

        #[unsafe(method(setTableView:))]
        fn set_table_view(&self, table: Option<&NSTableView>) {
            let old = self.ivars().table.replace(table.map(Weak::new));
            drop(old);
        }

        #[unsafe(method(draggedColumn))]
        fn dragged_column(&self) -> isize {
            -1
        }

        #[unsafe(method(draggedDistance))]
        fn dragged_distance(&self) -> f64 {
            0.0
        }

        #[unsafe(method(resizedColumn))]
        fn resized_column(&self) -> isize {
            -1
        }

        /// Its column's span, as tall as the header; without a table,
        /// nothing wide.
        #[unsafe(method(headerRectOfColumn:))]
        fn header_rect_of_column(&self, column: isize) -> NSRect {
            let h = self.as_view().bounds().size.height;
            let c = self.table().map_or(NSRect::ZERO, |table| table.rectOfColumn(column));
            NSRect::new(NSPoint::new(c.origin.x, 0.0), NSSize::new(c.size.width, h))
        }

        #[unsafe(method(columnAtPoint:))]
        fn column_at_point(&self, point: NSPoint) -> isize {
            match self.table() {
                Some(table) => table.columnAtPoint(NSPoint::new(point.x, 0.0)),
                None => -1,
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithWhite_alpha(0.95, 1.0).setFill();
            NSBezierPath::fillRect(self.as_view().bounds());
        }
    }

    unsafe impl NSObjectProtocol for NSTableHeaderViewImpl {}
);

impl NSTableHeaderViewImpl {
    fn table(&self) -> Option<Retained<NSTableView>> {
        self.ivars().table.borrow().as_ref().and_then(Weak::load)
    }

    fn as_view(&self) -> &NSView {
        // SAFETY: a header view is an NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }
}

/// Link a header view to its table.
pub(crate) fn set_header_table(header: &NSView, table: Option<&NSTableView>) {
    if let Some(h) = header.downcast_ref::<objc2_app_kit::NSTableHeaderView>() {
        // SAFETY: NSTableHeaderView is NSTableHeaderViewImpl's class.
        let h = unsafe { &*(h as *const objc2_app_kit::NSTableHeaderView).cast::<NSTableHeaderViewImpl>() };
        let old = h.ivars().table.replace(table.map(Weak::new));
        drop(old);
    }
}
