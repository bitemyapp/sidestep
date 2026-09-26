//! A view-based `NSTableView`, with `NSTableColumn`, `NSTableRowView`,
//! `NSTableCellView` and a shell `NSTableHeaderView`.
//!
//! **Geometry** follows the style, as measured on macOS (`Metrics`).
//! Columns sit side by side, each its width plus half the horizontal
//! intercell spacing on either side; rows one under another, each its
//! height plus the vertical spacing; a cell sits in its column past that
//! half spacing, and in its row by half the vertical spacing. The plain
//! style is just that. The full-width style pads the outer edges of the
//! first and last columns by 6 points instead; the inset style (what the
//! default, automatic style comes to) does too, and insets the columns by
//! 10 points and the rows by 5 at the top and bottom; the source list
//! style insets the rows by 10 at the top only. The table is at least as
//! big as its clip view. Row heights come from `rowHeight`, or row by row
//! from the delegate's `tableView:heightOfRow:`, asked when first needed
//! and kept in a Fenwick tree (`heights`), so finding a row's place, or the
//! row at a place, costs a logarithm of the row count; rows inserted later
//! are the only ones asked about then.
//!
//! **Realization.** Only rows near the visible rectangle have views: a
//! row view (`tableView:rowViewForRow:` or an NSTableRowView) holding a
//! cell view per column from `tableView:viewForTableColumn:row:`. The
//! table's `layout` makes the rows that came into view and recycles those
//! that went far out of it, whose cell views wait in pools by identifier
//! for `makeViewWithIdentifier:owner:` (which sends them
//! `prepareForReuse`). The table follows its clip view: scrolling or
//! resizing the clip view asks for the table's layout through
//! `view_layout`, not the notification center. Row work therefore scales
//! with the rows on screen, not the rows in the table.
//!
//! **Selection** is an index set. Selecting in code isn't checked with the
//! delegate; clicks and the arrow keys are (`shouldSelectRow`,
//! `selectionIndexesForProposedSelection:`). Either way a change posts
//! `NSTableViewSelectionDidChangeNotification`, which the delegate hears as
//! `tableViewSelectionDidChange:`. Rows inserted, removed or moved carry
//! the selection (and `selectedRow`) with them, and only removing a
//! selected row changes it. Clicks set `clickedRow` and send the action,
//! or the double action on a second click, to the target. The table's own
//! column changes post `NSTableViewColumnDidResizeNotification` and
//! `NSTableViewColumnDidMoveNotification` the same way; nothing posts
//! `NSTableViewSelectionIsChangingNotification` yet (no drag selection).
//!
//! NSTableView is an NSControl in AppKit. Until Sidestep's NSControl lands
//! it subclasses NSView here and keeps its own target and action.

mod column;
mod heights;
mod row;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBezierPath, NSColor, NSEvent, NSEventModifierFlags, NSResponder, NSTableColumn, NSTableHeaderView,
    NSTableRowView, NSTableView, NSTableViewAnimationOptions, NSTableViewColumnAutoresizingStyle,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewGridLineStyle, NSTableViewRowSizeStyle,
    NSTableViewSelectionHighlightStyle, NSTableViewStyle, NSUserInterfaceItemIdentification, NSView, NSWindow,
};
use objc2_foundation::{
    NSArray, NSCopying, NSDictionary, NSIndexSet, NSMutableCopying, NSMutableIndexSet, NSNotification, NSNumber,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use heights::{Fenwick, Heights};

use crate::view_layout;
use crate::views;

sidestep_runtime::static_class!(pub NSTABLEVIEW, NSTABLEVIEW_META = "NSTableView", || {
    let _ = NSTableViewImpl::class();
});
sidestep_runtime::static_class!(pub NSTABLECOLUMN, NSTABLECOLUMN_META = "NSTableColumn", || {
    let _ = column::NSTableColumnImpl::class();
});
sidestep_runtime::static_class!(pub NSTABLEROWVIEW, NSTABLEROWVIEW_META = "NSTableRowView", || {
    let _ = row::NSTableRowViewImpl::class();
});
sidestep_runtime::static_class!(pub NSTABLECELLVIEW, NSTABLECELLVIEW_META = "NSTableCellView", || {
    let _ = row::NSTableCellViewImpl::class();
});
sidestep_runtime::static_class!(pub NSTABLEHEADERVIEW, NSTABLEHEADERVIEW_META = "NSTableHeaderView", || {
    let _ = row::NSTableHeaderViewImpl::class();
});

sidestep_foundation::constant_string!(
    NSTableViewSelectionDidChangeNotification = "NSTableViewSelectionDidChangeNotification"
);
sidestep_foundation::constant_string!(
    NSTableViewSelectionIsChangingNotification = "NSTableViewSelectionIsChangingNotification"
);
sidestep_foundation::constant_string!(NSTableViewColumnDidMoveNotification = "NSTableViewColumnDidMoveNotification");
sidestep_foundation::constant_string!(
    NSTableViewColumnDidResizeNotification = "NSTableViewColumnDidResizeNotification"
);
sidestep_foundation::constant_string!(NSTableViewRowViewKey = "NSTableViewRowViewKey");

/// Rows kept beyond each edge of the visible ones, so scrolling back and
/// forth doesn't remake them.
const MARGIN_ROWS: usize = 2;

/// How far past the visible rows a table in a shown window makes rows: as
/// far as its scroll view draws ahead, a tile each way, so those tiles
/// aren't drawn once empty and again with the rows.
const PREPARED: f64 = crate::protocol::TILE_HEIGHT as f64;

/// Where a style puts rows and columns: insets around them, and the
/// padding at the outer edges of the first and last columns (half the
/// spacing when none).
struct Metrics {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    edge: Option<f64>,
}

impl Metrics {
    fn of(style: NSTableViewStyle) -> Metrics {
        let m = |left, top, bottom, edge| Metrics { left, right: left, top, bottom, edge };
        match style {
            NSTableViewStyle::FullWidth => m(0.0, 0.0, 0.0, Some(6.0)),
            NSTableViewStyle::Inset => m(10.0, 5.0, 5.0, Some(6.0)),
            NSTableViewStyle::SourceList => m(10.0, 10.0, 0.0, Some(6.0)),
            _ => m(0.0, 0.0, 0.0, None),
        }
    }
}

/// A column laid out: where it starts (not yet rounded), the room it
/// takes, and how far into it its cells start.
struct Placed {
    start: f64,
    span: f64,
    lead: f64,
}

/// A row with views: its row view and its cell views by column.
#[derive(Clone)]
struct Realized {
    row: Retained<NSTableRowView>,
    cells: Vec<Option<Retained<NSView>>>,
    /// The table made the row view, so it may reuse it.
    ours: bool,
}

pub(crate) struct TableIvars {
    columns: RefCell<Vec<Retained<NSTableColumn>>>,
    data_source: RefCell<Option<Weak<AnyObject>>>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    header: RefCell<Option<Retained<NSView>>>,
    corner: RefCell<Option<Retained<NSView>>>,
    row_height: Cell<f64>,
    spacing: Cell<NSSize>,
    grid: Cell<NSTableViewGridLineStyle>,
    alternating: Cell<bool>,
    background: RefCell<Option<Retained<NSColor>>>,
    grid_color: RefCell<Option<Retained<NSColor>>>,
    style: Cell<NSTableViewStyle>,
    row_size_style: Cell<NSTableViewRowSizeStyle>,
    highlight: Cell<NSTableViewSelectionHighlightStyle>,
    autoresizing: Cell<NSTableViewColumnAutoresizingStyle>,
    multiple: Cell<bool>,
    empty: Cell<bool>,
    column_selection: Cell<bool>,
    type_select: Cell<bool>,
    reordering: Cell<bool>,
    resizing: Cell<bool>,
    floats_group_rows: Cell<bool>,
    automatic_heights: Cell<bool>,
    /// The row count, once asked for.
    rows: Cell<Option<usize>>,
    /// The rows' heights, once asked for.
    heights: RefCell<Option<Heights>>,
    realized: RefCell<BTreeMap<usize, Realized>>,
    /// Cell views waiting for reuse, by identifier.
    pool: RefCell<HashMap<String, Vec<Retained<NSView>>>>,
    row_pool: RefCell<Vec<Retained<NSTableRowView>>>,
    selection: RefCell<Retained<NSMutableIndexSet>>,
    selected_columns: RefCell<Retained<NSMutableIndexSet>>,
    /// The row most recently added to the selection.
    last_selected: Cell<isize>,
    /// Where a shift-click extends the selection from.
    anchor: Cell<isize>,
    clicked: Cell<(isize, isize)>,
    target: RefCell<Option<Weak<AnyObject>>>,
    action: Cell<Option<Sel>>,
    double_action: Cell<Option<Sel>>,
    updates: Cell<usize>,
    sort_descriptors: RefCell<Option<Retained<NSArray<AnyObject>>>>,
    autosave: RefCell<Option<Retained<NSString>>>,
    autosave_columns: Cell<bool>,
}

impl Default for TableIvars {
    fn default() -> Self {
        TableIvars {
            columns: RefCell::default(),
            data_source: RefCell::new(None),
            delegate: RefCell::new(None),
            header: RefCell::new(None),
            corner: RefCell::new(None),
            row_height: Cell::new(24.0),
            spacing: Cell::new(NSSize::new(17.0, 0.0)),
            grid: Cell::new(NSTableViewGridLineStyle::empty()),
            alternating: Cell::new(false),
            background: RefCell::new(None),
            grid_color: RefCell::new(None),
            style: Cell::new(NSTableViewStyle::Automatic),
            row_size_style: Cell::new(NSTableViewRowSizeStyle::Custom),
            highlight: Cell::new(NSTableViewSelectionHighlightStyle::Regular),
            autoresizing: Cell::new(NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle),
            multiple: Cell::new(false),
            empty: Cell::new(true),
            column_selection: Cell::new(false),
            type_select: Cell::new(true),
            reordering: Cell::new(true),
            resizing: Cell::new(true),
            floats_group_rows: Cell::new(true),
            automatic_heights: Cell::new(false),
            rows: Cell::new(None),
            heights: RefCell::new(None),
            realized: RefCell::default(),
            pool: RefCell::default(),
            row_pool: RefCell::default(),
            selection: RefCell::new(NSMutableIndexSet::new()),
            selected_columns: RefCell::new(NSMutableIndexSet::new()),
            last_selected: Cell::new(-1),
            anchor: Cell::new(-1),
            clicked: Cell::new((-1, -1)),
            target: RefCell::new(None),
            action: Cell::new(None),
            double_action: Cell::new(None),
            updates: Cell::new(0),
            sort_descriptors: RefCell::new(None),
            autosave: RefCell::new(None),
            autosave_columns: Cell::new(false),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTableView"]
    #[ivars = TableIvars]
    pub(crate) struct NSTableViewImpl;

    impl NSTableViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(TableIvars::default());
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            view_layout::follow_clip_view(views::imp(this.as_view()));
            let mtm = MainThreadMarker::from(&*this);
            let header = NSTableHeaderView::initWithFrame(
                NSTableHeaderView::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(frame.size.width, 28.0)),
            );
            row::set_header_table(&header, Some(this.as_table()));
            this.ivars().header.replace(Some(Retained::into_super(header)));
            this
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        // Data source and delegate, weak as in AppKit.

        #[unsafe(method_id(dataSource))]
        fn data_source(&self) -> Option<Retained<ProtocolObject<dyn NSTableViewDataSource>>> {
            let d = self.ivars().data_source.borrow().as_ref().and_then(Weak::load);
            // SAFETY: only data sources are stored.
            d.map(|d| unsafe { Retained::cast_unchecked(d) })
        }

        #[unsafe(method(setDataSource:))]
        fn set_data_source(&self, source: Option<&ProtocolObject<dyn NSTableViewDataSource>>) {
            self.ivars().data_source.replace(source.map(|s| Weak::new(s.as_ref())));
            self.forget_rows();
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<ProtocolObject<dyn NSTableViewDelegate>>> {
            let d = self.ivars().delegate.borrow().as_ref().and_then(Weak::load);
            // SAFETY: only delegates are stored.
            d.map(|d| unsafe { Retained::cast_unchecked(d) })
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSTableViewDelegate>>) {
            self.ivars().delegate.replace(delegate.map(|d| Weak::new(d.as_ref())));
            self.forget_rows();
        }

        #[unsafe(method_id(headerView))]
        fn header_view(&self) -> Option<Retained<NSView>> {
            self.ivars().header.borrow().clone()
        }

        #[unsafe(method(setHeaderView:))]
        fn set_header_view(&self, header: Option<&NSView>) {
            if let Some(header) = header {
                row::set_header_table(header, Some(self.as_table()));
            }
            let old = self.ivars().header.replace(header.map(|h| h.retain()));
            if let Some(old) = &old {
                row::set_header_table(old, None);
            }
            drop(old);
        }

        #[unsafe(method_id(cornerView))]
        fn corner_view(&self) -> Option<Retained<NSView>> {
            self.ivars().corner.borrow().clone()
        }

        #[unsafe(method(setCornerView:))]
        fn set_corner_view(&self, corner: Option<&NSView>) {
            let old = self.ivars().corner.replace(corner.map(|c| c.retain()));
            drop(old);
        }

        // Settings.

        #[unsafe(method(allowsColumnReordering))]
        fn allows_column_reordering(&self) -> bool {
            self.ivars().reordering.get()
        }

        #[unsafe(method(setAllowsColumnReordering:))]
        fn set_allows_column_reordering(&self, flag: bool) {
            self.ivars().reordering.set(flag);
        }

        #[unsafe(method(allowsColumnResizing))]
        fn allows_column_resizing(&self) -> bool {
            self.ivars().resizing.get()
        }

        #[unsafe(method(setAllowsColumnResizing:))]
        fn set_allows_column_resizing(&self, flag: bool) {
            self.ivars().resizing.set(flag);
        }

        #[unsafe(method(columnAutoresizingStyle))]
        fn column_autoresizing_style(&self) -> NSTableViewColumnAutoresizingStyle {
            self.ivars().autoresizing.get()
        }

        #[unsafe(method(setColumnAutoresizingStyle:))]
        fn set_column_autoresizing_style(&self, style: NSTableViewColumnAutoresizingStyle) {
            self.ivars().autoresizing.set(style);
        }

        #[unsafe(method(gridStyleMask))]
        fn grid_style_mask(&self) -> NSTableViewGridLineStyle {
            self.ivars().grid.get()
        }

        #[unsafe(method(setGridStyleMask:))]
        fn set_grid_style_mask(&self, grid: NSTableViewGridLineStyle) {
            self.ivars().grid.set(grid);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(intercellSpacing))]
        fn intercell_spacing(&self) -> NSSize {
            self.ivars().spacing.get()
        }

        #[unsafe(method(setIntercellSpacing:))]
        fn set_intercell_spacing(&self, spacing: NSSize) {
            if self.ivars().spacing.replace(spacing) != spacing {
                self.heights_changed();
            }
        }

        #[unsafe(method(usesAlternatingRowBackgroundColors))]
        fn uses_alternating_row_background_colors(&self) -> bool {
            self.ivars().alternating.get()
        }

        #[unsafe(method(setUsesAlternatingRowBackgroundColors:))]
        fn set_uses_alternating_row_background_colors(&self, flag: bool) {
            self.ivars().alternating.set(flag);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            let color = self.ivars().background.borrow().clone();
            color.unwrap_or_else(|| NSColor::colorWithWhite_alpha(1.0, 1.0))
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: &NSColor) {
            self.ivars().background.replace(Some(color.retain()));
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method_id(gridColor))]
        fn grid_color(&self) -> Retained<NSColor> {
            let color = self.ivars().grid_color.borrow().clone();
            color.unwrap_or_else(|| NSColor::colorWithWhite_alpha(0.85, 1.0))
        }

        #[unsafe(method(setGridColor:))]
        fn set_grid_color(&self, color: &NSColor) {
            self.ivars().grid_color.replace(Some(color.retain()));
        }

        #[unsafe(method(rowSizeStyle))]
        fn row_size_style(&self) -> NSTableViewRowSizeStyle {
            self.ivars().row_size_style.get()
        }

        #[unsafe(method(setRowSizeStyle:))]
        fn set_row_size_style(&self, style: NSTableViewRowSizeStyle) {
            self.ivars().row_size_style.set(style);
        }

        #[unsafe(method(effectiveRowSizeStyle))]
        fn effective_row_size_style(&self) -> NSTableViewRowSizeStyle {
            self.ivars().row_size_style.get()
        }

        #[unsafe(method(style))]
        fn style(&self) -> NSTableViewStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setStyle:))]
        fn set_style(&self, style: NSTableViewStyle) {
            if self.ivars().style.replace(style) != style {
                self.heights_changed();
            }
        }

        /// The automatic style is the inset one, or the source list for a
        /// source list's highlight, as on macOS.
        #[unsafe(method(effectiveStyle))]
        fn effective_style(&self) -> NSTableViewStyle {
            self.resolved_style()
        }

        #[unsafe(method(selectionHighlightStyle))]
        fn selection_highlight_style(&self) -> NSTableViewSelectionHighlightStyle {
            self.ivars().highlight.get()
        }

        #[unsafe(method(setSelectionHighlightStyle:))]
        fn set_selection_highlight_style(&self, style: NSTableViewSelectionHighlightStyle) {
            let before = self.resolved_style();
            self.ivars().highlight.set(style);
            for row in self.row_views() {
                row.setSelectionHighlightStyle(style);
            }
            if self.resolved_style() != before {
                self.heights_changed();
            }
        }

        #[unsafe(method(rowHeight))]
        fn row_height(&self) -> f64 {
            self.ivars().row_height.get()
        }

        #[unsafe(method(setRowHeight:))]
        fn set_row_height(&self, height: f64) {
            if self.ivars().row_height.replace(height) != height {
                self.heights_changed();
            }
        }

        #[unsafe(method(usesAutomaticRowHeights))]
        fn uses_automatic_row_heights(&self) -> bool {
            self.ivars().automatic_heights.get()
        }

        #[unsafe(method(setUsesAutomaticRowHeights:))]
        fn set_uses_automatic_row_heights(&self, flag: bool) {
            self.ivars().automatic_heights.set(flag);
        }

        #[unsafe(method(floatsGroupRows))]
        fn floats_group_rows(&self) -> bool {
            self.ivars().floats_group_rows.get()
        }

        #[unsafe(method(setFloatsGroupRows:))]
        fn set_floats_group_rows(&self, flag: bool) {
            self.ivars().floats_group_rows.set(flag);
        }

        #[unsafe(method(noteHeightOfRowsWithIndexesChanged:))]
        fn note_height_of_rows_with_indexes_changed(&self, rows: &NSIndexSet) {
            self.update_heights(rows);
        }

        // Columns.

        #[unsafe(method_id(tableColumns))]
        fn table_columns(&self) -> Retained<NSArray<NSTableColumn>> {
            NSArray::from_retained_slice(&self.ivars().columns.borrow())
        }

        #[unsafe(method(numberOfColumns))]
        fn number_of_columns(&self) -> isize {
            self.ivars().columns.borrow().len() as isize
        }

        #[unsafe(method(addTableColumn:))]
        fn add_table_column(&self, column: &NSTableColumn) {
            self.ivars().columns.borrow_mut().push(column.retain());
            column::link(column, Some(self.as_table()));
            self.columns_rebuilt();
        }

        #[unsafe(method(removeTableColumn:))]
        fn remove_table_column(&self, column: &NSTableColumn) {
            let removed = {
                let mut columns = self.ivars().columns.borrow_mut();
                columns.iter().position(|c| std::ptr::eq(&**c, column)).map(|i| columns.remove(i))
            };
            if removed.is_some() {
                column::link(column, None);
                self.columns_rebuilt();
            }
            drop(removed);
        }

        #[unsafe(method(moveColumn:toColumn:))]
        fn move_column(&self, from: isize, to: isize) {
            {
                let mut columns = self.ivars().columns.borrow_mut();
                let len = columns.len();
                let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else { return };
                if from >= len || to >= len || from == to {
                    return;
                }
                let c = columns.remove(from);
                columns.insert(to, c);
            }
            self.columns_rebuilt();
            let (old, new) = (NSNumber::new_isize(from), NSNumber::new_isize(to));
            // SAFETY: the name is a constant this module exports.
            let name = unsafe { objc2_app_kit::NSTableViewColumnDidMoveNotification };
            self.announce(name, sel!(tableViewColumnDidMove:), &[("NSOldColumn", &old), ("NSNewColumn", &new)]);
        }

        #[unsafe(method(columnWithIdentifier:))]
        fn column_with_identifier(&self, identifier: &NSString) -> isize {
            self.column_index(identifier).map_or(-1, |i| i as isize)
        }

        #[unsafe(method_id(tableColumnWithIdentifier:))]
        fn table_column_with_identifier(&self, identifier: &NSString) -> Option<Retained<NSTableColumn>> {
            let i = self.column_index(identifier);
            i.and_then(|i| self.ivars().columns.borrow().get(i).cloned())
        }

        #[unsafe(method(tile))]
        fn tile(&self) {
            self.tile_frame();
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            self.size_last_column();
        }

        #[unsafe(method(sizeLastColumnToFit))]
        fn size_last_column_to_fit(&self) {
            self.size_last_column();
        }

        #[unsafe(method(scrollRowToVisible:))]
        fn scroll_row_to_visible(&self, row: isize) {
            if row >= 0 && (row as usize) < self.row_count() {
                self.as_view().scrollRectToVisible(self.rect_of_row(row as usize));
            }
        }

        #[unsafe(method(scrollColumnToVisible:))]
        fn scroll_column_to_visible(&self, column: isize) {
            let r = self.as_table().rectOfColumn(column);
            if r.size.width > 0.0 {
                let visible = self.as_view().visibleRect();
                self.as_view().scrollRectToVisible(NSRect::new(
                    NSPoint::new(r.origin.x, visible.origin.y),
                    NSSize::new(r.size.width, visible.size.height.min(1.0)),
                ));
            }
        }

        // Rows.

        #[unsafe(method(numberOfRows))]
        fn number_of_rows(&self) -> isize {
            self.row_count() as isize
        }

        #[unsafe(method(reloadData))]
        fn reload_data(&self) {
            self.reload();
        }

        #[unsafe(method(noteNumberOfRowsChanged))]
        fn note_number_of_rows_changed(&self) {
            self.ivars().rows.set(None);
            self.ivars().heights.replace(None);
            let count = self.row_count();
            self.trim_selection(count);
            self.drop_rows(|r| r >= count);
            self.tile_frame();
            self.as_view().setNeedsLayout(true);
        }

        #[unsafe(method(reloadDataForRowIndexes:columnIndexes:))]
        fn reload_data_for_rows(&self, rows: &NSIndexSet, columns: &NSIndexSet) {
            self.reload_cells(rows, columns);
        }

        #[unsafe(method(beginUpdates))]
        fn begin_updates(&self) {
            self.ivars().updates.set(self.ivars().updates.get() + 1);
        }

        #[unsafe(method(endUpdates))]
        fn end_updates(&self) {
            let n = self.ivars().updates.get().saturating_sub(1);
            self.ivars().updates.set(n);
            if n == 0 {
                self.realize();
            }
        }

        #[unsafe(method(insertRowsAtIndexes:withAnimation:))]
        fn insert_rows(&self, indexes: &NSIndexSet, _animation: NSTableViewAnimationOptions) {
            self.insert_rows_at(indexes);
        }

        #[unsafe(method(removeRowsAtIndexes:withAnimation:))]
        fn remove_rows(&self, indexes: &NSIndexSet, _animation: NSTableViewAnimationOptions) {
            self.remove_rows_at(indexes);
        }

        #[unsafe(method(moveRowAtIndex:toIndex:))]
        fn move_row(&self, from: isize, to: isize) {
            self.move_row_at(from, to);
        }

        #[unsafe(method(editedColumn))]
        fn edited_column(&self) -> isize {
            -1
        }

        #[unsafe(method(editedRow))]
        fn edited_row(&self) -> isize {
            -1
        }

        #[unsafe(method(clickedColumn))]
        fn clicked_column(&self) -> isize {
            self.ivars().clicked.get().1
        }

        #[unsafe(method(clickedRow))]
        fn clicked_row(&self) -> isize {
            self.ivars().clicked.get().0
        }

        // Target and action (NSControl's, until NSControl is the superclass).

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            self.ivars().target.replace(target.map(Weak::new));
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            self.ivars().action.get()
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            self.ivars().action.set(action);
        }

        #[unsafe(method(doubleAction))]
        fn double_action(&self) -> Option<Sel> {
            self.ivars().double_action.get()
        }

        #[unsafe(method(setDoubleAction:))]
        fn set_double_action(&self, action: Option<Sel>) {
            self.ivars().double_action.set(action);
        }

        #[unsafe(method_id(sortDescriptors))]
        fn sort_descriptors(&self) -> Retained<NSArray<AnyObject>> {
            self.ivars().sort_descriptors.borrow().clone().unwrap_or_default()
        }

        /// A change is told to the data source, with the descriptors before
        /// (nil the first time).
        #[unsafe(method(setSortDescriptors:))]
        fn set_sort_descriptors(&self, descriptors: &NSArray<AnyObject>) {
            let old = self.ivars().sort_descriptors.borrow().clone();
            let same = match &old {
                Some(old) => old.isEqualToArray(descriptors),
                None => descriptors.count() == 0,
            };
            if same {
                return;
            }
            self.ivars().sort_descriptors.replace(Some(descriptors.copy()));
            if let Some(source) = self.source_for(sel!(tableView:sortDescriptorsDidChange:)) {
                // SAFETY: the data source method takes the table and the old
                // descriptors, or nil.
                let _: () =
                    unsafe { msg_send![&*source, tableView: self.as_table(), sortDescriptorsDidChange: old.as_deref()] };
            }
        }

        #[unsafe(method_id(autosaveName))]
        fn autosave_name(&self) -> Option<Retained<NSString>> {
            self.ivars().autosave.borrow().clone()
        }

        #[unsafe(method(setAutosaveName:))]
        fn set_autosave_name(&self, name: Option<&NSString>) {
            let old = self.ivars().autosave.replace(name.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        #[unsafe(method(autosaveTableColumns))]
        fn autosave_table_columns(&self) -> bool {
            self.ivars().autosave_columns.get()
        }

        #[unsafe(method(setAutosaveTableColumns:))]
        fn set_autosave_table_columns(&self, flag: bool) {
            self.ivars().autosave_columns.set(flag);
        }

        // Selection.

        #[unsafe(method(allowsMultipleSelection))]
        fn allows_multiple_selection(&self) -> bool {
            self.ivars().multiple.get()
        }

        #[unsafe(method(setAllowsMultipleSelection:))]
        fn set_allows_multiple_selection(&self, flag: bool) {
            self.ivars().multiple.set(flag);
        }

        #[unsafe(method(allowsEmptySelection))]
        fn allows_empty_selection(&self) -> bool {
            self.ivars().empty.get()
        }

        #[unsafe(method(setAllowsEmptySelection:))]
        fn set_allows_empty_selection(&self, flag: bool) {
            self.ivars().empty.set(flag);
        }

        #[unsafe(method(allowsColumnSelection))]
        fn allows_column_selection(&self) -> bool {
            self.ivars().column_selection.get()
        }

        #[unsafe(method(setAllowsColumnSelection:))]
        fn set_allows_column_selection(&self, flag: bool) {
            self.ivars().column_selection.set(flag);
        }

        #[unsafe(method(allowsTypeSelect))]
        fn allows_type_select(&self) -> bool {
            self.ivars().type_select.get()
        }

        #[unsafe(method(setAllowsTypeSelect:))]
        fn set_allows_type_select(&self, flag: bool) {
            self.ivars().type_select.set(flag);
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            if !self.ivars().multiple.get() {
                return;
            }
            let all = NSMutableIndexSet::new();
            for r in 0..self.row_count() {
                if self.should_select(r) {
                    all.addIndex(r);
                }
            }
            let chosen = self.proposed(&all);
            self.set_selection(&chosen);
        }

        #[unsafe(method(deselectAll:))]
        fn deselect_all(&self, _sender: Option<&AnyObject>) {
            if self.ivars().empty.get() {
                self.set_selection(&NSIndexSet::new());
            }
        }

        #[unsafe(method(selectRowIndexes:byExtendingSelection:))]
        fn select_row_indexes(&self, indexes: &NSIndexSet, extend: bool) {
            self.select_rows(indexes, extend);
        }

        #[unsafe(method_id(selectedRowIndexes))]
        fn selected_row_indexes(&self) -> Retained<NSIndexSet> {
            Retained::into_super(self.ivars().selection.borrow().mutableCopy())
        }

        #[unsafe(method(deselectRow:))]
        fn deselect_row(&self, row: isize) {
            let Ok(row) = usize::try_from(row) else { return };
            let now = self.selection_copy();
            if now.containsIndex(row) {
                now.removeIndex(row);
                self.set_selection(&now);
            }
        }

        #[unsafe(method(selectedRow))]
        fn selected_row(&self) -> isize {
            self.ivars().last_selected.get()
        }

        #[unsafe(method(isRowSelected:))]
        fn is_row_selected(&self, row: isize) -> bool {
            usize::try_from(row).is_ok_and(|r| self.ivars().selection.borrow().containsIndex(r))
        }

        #[unsafe(method(numberOfSelectedRows))]
        fn number_of_selected_rows(&self) -> isize {
            self.ivars().selection.borrow().count() as isize
        }

        #[unsafe(method(selectColumnIndexes:byExtendingSelection:))]
        fn select_column_indexes(&self, indexes: &NSIndexSet, extend: bool) {
            let columns = self.ivars().columns.borrow().len();
            let set = self.ivars().selected_columns.borrow().clone();
            if !extend {
                set.removeAllIndexes();
            }
            for i in index_list(indexes) {
                if i < columns {
                    set.addIndex(i);
                }
            }
        }

        #[unsafe(method_id(selectedColumnIndexes))]
        fn selected_column_indexes(&self) -> Retained<NSIndexSet> {
            Retained::into_super(self.ivars().selected_columns.borrow().mutableCopy())
        }

        #[unsafe(method(deselectColumn:))]
        fn deselect_column(&self, column: isize) {
            if let Ok(c) = usize::try_from(column) {
                self.ivars().selected_columns.borrow().removeIndex(c);
            }
        }

        #[unsafe(method(selectedColumn))]
        fn selected_column(&self) -> isize {
            let last = self.ivars().selected_columns.borrow().lastIndex();
            if last == usize::MAX || last as isize == isize::MAX { -1 } else { last as isize }
        }

        #[unsafe(method(isColumnSelected:))]
        fn is_column_selected(&self, column: isize) -> bool {
            usize::try_from(column).is_ok_and(|c| self.ivars().selected_columns.borrow().containsIndex(c))
        }

        #[unsafe(method(numberOfSelectedColumns))]
        fn number_of_selected_columns(&self) -> isize {
            self.ivars().selected_columns.borrow().count() as isize
        }

        // Geometry.

        #[unsafe(method(rectOfColumn:))]
        fn rect_of_column(&self, column: isize) -> NSRect {
            let Some((x, w)) = self.column_span(column) else { return NSRect::ZERO };
            let h = self.as_view().frame().size.height;
            NSRect::new(NSPoint::new(x, 0.0), NSSize::new(w, h))
        }

        #[unsafe(method(rectOfRow:))]
        fn rect_of_row_method(&self, row: isize) -> NSRect {
            match usize::try_from(row) {
                Ok(r) if r < self.row_count() => self.rect_of_row(r),
                _ => NSRect::ZERO,
            }
        }

        #[unsafe(method_id(columnIndexesInRect:))]
        fn column_indexes_in_rect(&self, rect: NSRect) -> Retained<NSIndexSet> {
            let set = NSMutableIndexSet::new();
            let count = self.ivars().columns.borrow().len();
            for c in 0..count {
                if let Some((x, w)) = self.column_span(c as isize)
                    && w > 0.0
                    && x < rect.origin.x + rect.size.width
                    && x + w > rect.origin.x
                {
                    set.addIndex(c);
                }
            }
            Retained::into_super(set)
        }

        #[unsafe(method(rowsInRect:))]
        fn rows_in_rect(&self, rect: NSRect) -> NSRange {
            match self.rows_in(rect.origin.y, rect.origin.y + rect.size.height) {
                Some((first, last)) => NSRange::new(first, last - first + 1),
                None => NSRange::new(0, 0),
            }
        }

        #[unsafe(method(columnAtPoint:))]
        fn column_at_point(&self, point: NSPoint) -> isize {
            let count = self.ivars().columns.borrow().len();
            (0..count as isize)
                .find(|&c| {
                    self.column_span(c).is_some_and(|(x, w)| w > 0.0 && point.x >= x && point.x < x + w)
                })
                .unwrap_or(-1)
        }

        #[unsafe(method(rowAtPoint:))]
        fn row_at_point(&self, point: NSPoint) -> isize {
            let top = self.metrics().top;
            self.with_heights(|h| h.row_at(point.y - top)).map_or(-1, |r| r as isize)
        }

        #[unsafe(method(frameOfCellAtColumn:row:))]
        fn frame_of_cell(&self, column: isize, row: isize) -> NSRect {
            let (Ok(c), Ok(r)) = (usize::try_from(column), usize::try_from(row)) else { return NSRect::ZERO };
            if c >= self.ivars().columns.borrow().len() || r >= self.row_count() {
                return NSRect::ZERO;
            }
            let row = self.rect_of_row(r);
            let cell = self.cell_frame(c, row.size.height);
            NSRect::new(NSPoint::new(cell.origin.x, row.origin.y + cell.origin.y), cell.size)
        }

        // Views.

        #[unsafe(method_id(viewAtColumn:row:makeIfNecessary:))]
        fn view_at_column(&self, column: isize, row: isize, make: bool) -> Option<Retained<NSView>> {
            self.cell_at(column, row, make)
        }

        #[unsafe(method_id(rowViewAtRow:makeIfNecessary:))]
        fn row_view_at_row(&self, row: isize, make: bool) -> Option<Retained<NSTableRowView>> {
            self.row_view_at(row, make)
        }

        #[unsafe(method(rowForView:))]
        fn row_for_view(&self, view: &NSView) -> isize {
            self.locate(view).map_or(-1, |(r, _)| r as isize)
        }

        #[unsafe(method(columnForView:))]
        fn column_for_view(&self, view: &NSView) -> isize {
            self.locate(view).and_then(|(_, c)| c).map_or(-1, |c| c as isize)
        }

        #[unsafe(method_id(makeViewWithIdentifier:owner:))]
        fn make_view_with_identifier(&self, identifier: &NSString, _owner: Option<&AnyObject>) -> Option<Retained<NSView>> {
            self.dequeue(identifier)
        }

        #[unsafe(method(enumerateAvailableRowViewsUsingBlock:))]
        fn enumerate_available_row_views(
            &self,
            handler: &block2::DynBlock<dyn Fn(NonNull<NSTableRowView>, isize) + '_>,
        ) {
            let rows: Vec<(usize, Retained<NSTableRowView>)> =
                self.ivars().realized.borrow().iter().map(|(r, x)| (*r, x.row.clone())).collect();
            for (r, view) in rows {
                handler.call((NonNull::from(&*view), r as isize));
            }
        }

        // The view.

        #[unsafe(method(layout))]
        fn layout(&self) {
            // SAFETY: NSView's layout takes nothing.
            let _: () = unsafe { msg_send![super(self), layout] };
            self.tile_frame();
            self.realize();
        }

        #[unsafe(method(viewDidMoveToSuperview))]
        fn view_did_move_to_superview(&self) {
            self.tile_frame();
            self.as_view().setNeedsLayout(true);
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: NSResponder's becomeFirstResponder returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            self.emphasize(ok);
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: NSResponder's resignFirstResponder returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.emphasize(false);
            }
            ok
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            self.draw(dirty);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.click(event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.send_click_action(event);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let events = NSArray::from_retained_slice(&[event.retain()]);
            self.as_view().interpretKeyEvents(&events);
        }

        #[unsafe(method(moveUp:))]
        fn move_up(&self, _sender: Option<&AnyObject>) {
            self.step_selection(-1);
        }

        #[unsafe(method(moveDown:))]
        fn move_down(&self, _sender: Option<&AnyObject>) {
            self.step_selection(1);
        }
    }

    unsafe impl NSObjectProtocol for NSTableViewImpl {}
);

fn table_imp(table: &NSTableView) -> &NSTableViewImpl {
    // SAFETY: NSTableView is NSTableViewImpl's class; subclasses share its
    // layout.
    unsafe { &*(table as *const NSTableView).cast::<NSTableViewImpl>() }
}

/// A column's width or visibility changed.
pub(crate) fn columns_changed(table: &NSTableView) {
    table_imp(table).columns_moved();
}

/// `window` became the key window or stopped being it: a table that is its
/// first responder shows its selection strongly only while it's key.
pub(crate) fn key_changed(window: &NSWindow) {
    if let Some(responder) = window.firstResponder()
        && let Some(table) = responder.downcast_ref::<NSTableView>()
    {
        table_imp(table).emphasize(true);
    }
}

/// A column of `table` changed its width from `old`: say so.
pub(crate) fn column_resized(table: &NSTableView, column: &NSTableColumn, old: f64) {
    let old = NSNumber::new_f64(old);
    // SAFETY: the name is a constant this module exports.
    let name = unsafe { objc2_app_kit::NSTableViewColumnDidResizeNotification };
    table_imp(table).announce(
        name,
        sel!(tableViewColumnDidResize:),
        &[("NSTableColumn", column), ("NSOldWidth", &old)],
    );
}

/// The indexes of a set, in order.
fn index_list(set: &NSIndexSet) -> Vec<usize> {
    let mut out = Vec::with_capacity(set.count());
    let mut i = set.firstIndex();
    while i != usize::MAX && (i as isize) != isize::MAX {
        out.push(i);
        i = set.indexGreaterThanIndex(i);
    }
    out
}

impl NSTableViewImpl {
    /// The effective style: the automatic one resolved.
    fn resolved_style(&self) -> NSTableViewStyle {
        // A source list's highlight still asks for the source list style.
        #[allow(deprecated)]
        let source_list = NSTableViewSelectionHighlightStyle::SourceList;
        match self.ivars().style.get() {
            NSTableViewStyle::Automatic if self.ivars().highlight.get() == source_list => NSTableViewStyle::SourceList,
            NSTableViewStyle::Automatic => NSTableViewStyle::Inset,
            other => other,
        }
    }

    fn metrics(&self) -> Metrics {
        Metrics::of(self.resolved_style())
    }

    /// The realized rows' views.
    fn row_views(&self) -> Vec<Retained<NSTableRowView>> {
        self.ivars().realized.borrow().values().map(|x| x.row.clone()).collect()
    }

    /// Whether selected rows show the strong selection: the table has the
    /// focus of the key window.
    fn emphasized(&self) -> bool {
        self.as_view().window().is_some_and(|w| {
            w.isKeyWindow()
                && w.firstResponder()
                    .is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<u8>(), (self as *const Self).cast()))
        })
    }

    /// The focus came or went: rows show it.
    fn emphasize(&self, focused: bool) {
        let on = focused && self.as_view().window().is_some_and(|w| w.isKeyWindow());
        for row in self.row_views() {
            row.setEmphasized(on);
        }
    }

    /// Tell the delegate, then everyone, of a change, with user info.
    fn announce(&self, name: &NSString, selector: Sel, info: &[(&str, &AnyObject)]) {
        let delegate = self.delegate_for(selector);
        if delegate.is_none() && !sidestep_foundation::notification_center::has_observers(name) {
            return;
        }
        let keys: Vec<Retained<NSString>> = info.iter().map(|(k, _)| NSString::from_str(k)).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&AnyObject> = info.iter().map(|(_, v)| *v).collect();
        let info = NSDictionary::from_slices(&keys, &values);
        let this: &AnyObject = self.as_view();
        // SAFETY: a dictionary of strings to objects is a dictionary of
        // objects, as user info is.
        let info = unsafe { &*Retained::as_ptr(&info).cast::<NSDictionary>() };
        // SAFETY: as above.
        let note = unsafe { NSNotification::notificationWithName_object_userInfo(name, Some(this), Some(info)) };
        if let Some(d) = delegate {
            // SAFETY: the delegate's notification methods take the
            // notification.
            let _: () = unsafe { objc2::runtime::MessageReceiver::send_message(&*d, selector, (&*note,)) };
        }
        sidestep_foundation::notification_center::default_center().postNotification(&note);
    }

    fn cell_at(&self, column: isize, row: isize, make: bool) -> Option<Retained<NSView>> {
        let (Ok(c), Ok(r)) = (usize::try_from(column), usize::try_from(row)) else { return None };
        if r >= self.row_count() {
            return None;
        }
        if make && !self.ivars().realized.borrow().contains_key(&r) {
            self.make_row(r);
        }
        let realized = self.ivars().realized.borrow();
        realized.get(&r).and_then(|x| x.cells.get(c).cloned().flatten())
    }

    fn row_view_at(&self, row: isize, make: bool) -> Option<Retained<NSTableRowView>> {
        let r = usize::try_from(row).ok().filter(|&r| r < self.row_count())?;
        if make && !self.ivars().realized.borrow().contains_key(&r) {
            self.make_row(r);
        }
        self.ivars().realized.borrow().get(&r).map(|x| x.row.clone())
    }

    /// A cell view waiting for reuse, told so.
    fn dequeue(&self, identifier: &NSString) -> Option<Retained<NSView>> {
        let view = self.ivars().pool.borrow_mut().get_mut(&identifier.to_string()).and_then(Vec::pop)?;
        view.prepareForReuse();
        Some(view)
    }

    fn as_view(&self) -> &NSView {
        // SAFETY: a table is an NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn as_table(&self) -> &NSTableView {
        // SAFETY: NSTableView is this class.
        unsafe { &*(self as *const Self).cast::<NSTableView>() }
    }

    fn delegate_for(&self, selector: Sel) -> Option<Retained<AnyObject>> {
        let d = self.ivars().delegate.borrow().as_ref().and_then(Weak::load)?;
        d.class().responds_to(selector).then_some(d)
    }

    fn source_for(&self, selector: Sel) -> Option<Retained<AnyObject>> {
        let d = self.ivars().data_source.borrow().as_ref().and_then(Weak::load)?;
        d.class().responds_to(selector).then_some(d)
    }

    /// The number of rows, asked of the data source when not known.
    fn row_count(&self) -> usize {
        if let Some(n) = self.ivars().rows.get() {
            return n;
        }
        let n = match self.source_for(sel!(numberOfRowsInTableView:)) {
            // SAFETY: the data source method takes the table and returns an
            // integer.
            Some(d) => unsafe { msg_send![&*d, numberOfRowsInTableView: self.as_table()] },
            None => 0isize,
        };
        let n = n.max(0) as usize;
        self.ivars().rows.set(Some(n));
        n
    }

    /// Work with the rows' heights, asking the delegate for them when not
    /// known.
    fn with_heights<R>(&self, f: impl FnOnce(&Heights) -> R) -> R {
        if self.ivars().heights.borrow().is_none() {
            let heights = self.measure();
            self.ivars().heights.replace(Some(heights));
        }
        let heights = self.ivars().heights.borrow();
        f(heights.as_ref().expect("heights"))
    }

    fn measure(&self) -> Heights {
        let count = self.row_count();
        let spacing = self.ivars().spacing.get().height;
        match self.delegate_for(sel!(tableView:heightOfRow:)) {
            Some(d) => Heights::Varied(Fenwick::new((0..count).map(|r| self.ask_height(&d, r)).collect())),
            None => Heights::Uniform { count, height: self.ivars().row_height.get() + spacing },
        }
    }

    /// A row's height as the delegate gives it, with the spacing.
    fn ask_height(&self, delegate: &AnyObject, row: usize) -> f64 {
        // SAFETY: the delegate method takes the table and a row and returns
        // a CGFloat.
        let h: f64 = unsafe { msg_send![delegate, tableView: self.as_table(), heightOfRow: row as isize] };
        h + self.ivars().spacing.get().height
    }

    fn rect_of_row(&self, row: usize) -> NSRect {
        let top = self.metrics().top;
        let (y, h) = self.with_heights(|hs| (hs.start(row), hs.height(row)));
        let w = self.as_view().frame().size.width;
        NSRect::new(NSPoint::new(0.0, top + y), NSSize::new(w, h))
    }

    /// The first and last rows with any part between `y0` and `y1`.
    fn rows_in(&self, y0: f64, y1: f64) -> Option<(usize, usize)> {
        let top = self.metrics().top;
        let (y0, y1) = (y0 - top, y1 - top);
        self.with_heights(|h| {
            let total = h.total();
            if y1 <= 0.0 || y0 >= total || y1 <= y0 {
                return None;
            }
            let first = h.row_at(y0.max(0.0))?;
            let last = h.row_at((y1.min(total) - 1e-9).max(0.0))?;
            Some((first, last))
        })
    }

    /// Each column laid out, hidden ones taking no room, and where the
    /// columns end.
    fn place_columns(&self) -> (Vec<Placed>, f64) {
        let m = self.metrics();
        let half = self.ivars().spacing.get().width / 2.0;
        let columns = self.ivars().columns.borrow();
        let first = columns.iter().position(|c| !column::is_hidden(c));
        let last = columns.iter().rposition(|c| !column::is_hidden(c));
        let mut x = m.left;
        let placed = columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                if column::is_hidden(col) {
                    return Placed { start: x, span: 0.0, lead: 0.0 };
                }
                let lead = if Some(i) == first { m.edge.unwrap_or(half) } else { half };
                let trail = if Some(i) == last { m.edge.unwrap_or(half) } else { half };
                let span = lead + column::span(col) + trail;
                let p = Placed { start: x, span, lead };
                x += span;
                p
            })
            .collect();
        (placed, x)
    }

    /// A column's rectangle across the table, its edges on whole points,
    /// or none.
    fn column_span(&self, column: isize) -> Option<(f64, f64)> {
        let c = usize::try_from(column).ok()?;
        let (placed, _) = self.place_columns();
        let p = placed.get(c)?;
        let (x0, x1) = (p.start.round(), (p.start + p.span).round());
        Some((x0, x1 - x0))
    }

    fn content_width(&self) -> f64 {
        self.place_columns().1 + self.metrics().right
    }

    fn column_index(&self, identifier: &NSString) -> Option<usize> {
        let columns = self.ivars().columns.borrow().clone();
        columns.iter().position(|c| c.identifier().isEqualToString(identifier))
    }

    /// The table's frame for its columns and rows: as big as its clip view
    /// at least, when it's in one.
    fn tile_frame(&self) {
        let m = self.metrics();
        let rows = m.top + self.with_heights(Heights::total) + m.bottom;
        let mut size = NSSize::new(self.content_width(), rows);
        if let Some(sup) = views::superview_of(views::imp(self.as_view()))
            && views::is_clip(sup)
        {
            let clip = views::frame(sup).size;
            size = NSSize::new(size.width.max(clip.width), size.height.max(clip.height));
        }
        let old = self.as_view().frame().size;
        if old != size {
            self.as_view().setFrameSize(size);
            // Rows are as wide as the table.
            if old.width != size.width {
                self.place_rows();
            }
        }
    }

    /// Give the last visible column the room left of the table's width.
    fn size_last_column(&self) {
        let columns = self.ivars().columns.borrow().clone();
        let Some(last) = columns.iter().rposition(|c| !column::is_hidden(c)) else { return };
        let (_, end) = self.place_columns();
        let room = self.as_view().frame().size.width - self.metrics().right - end;
        columns[last].setWidth(column::span(&columns[last]) + room);
    }

    fn heights_changed(&self) {
        self.ivars().heights.replace(None);
        self.tile_frame();
        self.place_rows();
        self.as_view().setNeedsLayout(true);
    }

    fn update_heights(&self, rows: &NSIndexSet) {
        let Some(d) = self.delegate_for(sel!(tableView:heightOfRow:)) else { return };
        let count = self.ivars().heights.borrow().as_ref().map_or(0, Heights::count);
        // Asked before the heights are borrowed: the delegate may ask the
        // table things.
        let new: Vec<(usize, f64)> =
            index_list(rows).into_iter().filter(|&r| r < count).map(|r| (r, self.ask_height(&d, r))).collect();
        if let Some(Heights::Varied(f)) = self.ivars().heights.borrow_mut().as_mut() {
            for (r, h) in new {
                if r < f.len() {
                    f.set(r, h);
                }
            }
        }
        self.tile_frame();
        self.place_rows();
        self.as_view().setNeedsLayout(true);
    }

    fn columns_moved(&self) {
        self.tile_frame();
        self.place_rows();
        self.as_view().setNeedsDisplay(true);
    }

    /// Columns were added, removed or reordered: rows are made again.
    fn columns_rebuilt(&self) {
        self.drop_rows(|_| true);
        self.tile_frame();
        self.as_view().setNeedsLayout(true);
    }

    /// Forget what the data source and delegate said.
    fn forget_rows(&self) {
        self.ivars().rows.set(None);
        self.ivars().heights.replace(None);
        self.drop_rows(|_| true);
        self.as_view().setNeedsLayout(true);
    }

    fn reload(&self) {
        self.forget_rows();
        let count = self.row_count();
        self.trim_selection(count);
        self.tile_frame();
    }

    /// The rows near the visible rectangle, with views; the others without.
    fn realize(&self) {
        if self.ivars().updates.get() > 0 {
            return;
        }
        let visible = view_layout::visible_rect(views::imp(self.as_view()));
        let shown = self.as_view().window().is_some_and(|w| w.isVisible());
        let ahead = if shown { PREPARED } else { 0.0 };
        let range = if visible.size.height > 0.0 {
            self.rows_in(visible.origin.y - ahead, visible.origin.y + visible.size.height + ahead)
        } else {
            None
        };
        let (keep_from, keep_to) = match range {
            Some((a, b)) => (a.saturating_sub(MARGIN_ROWS), b + MARGIN_ROWS),
            None => (1, 0),
        };
        self.drop_rows(|r| r < keep_from || r > keep_to);
        if let Some((first, last)) = range {
            for r in first..=last {
                if !self.ivars().realized.borrow().contains_key(&r) {
                    self.make_row(r);
                }
            }
        }
    }

    /// Give row `r` its views.
    fn make_row(&self, r: usize) {
        let table = self.as_table();
        let frame = self.rect_of_row(r);
        let delegate_row = self.delegate_for(sel!(tableView:rowViewForRow:)).and_then(|d| {
            // SAFETY: the delegate method takes the table and a row and
            // returns a row view or nil.
            let v: Option<Retained<NSTableRowView>> =
                unsafe { msg_send![&*d, tableView: table, rowViewForRow: r as isize] };
            v
        });
        let ours = delegate_row.is_none();
        let row_view = delegate_row.unwrap_or_else(|| {
            let pooled = self.ivars().row_pool.borrow_mut().pop();
            pooled.unwrap_or_else(|| {
                crate::load_shell::<NSTableRowView>();
                let mtm = MainThreadMarker::from(self);
                NSTableRowView::initWithFrame(NSTableRowView::alloc(mtm), frame)
            })
        });
        row_view.setFrame(frame);
        let selected = self.ivars().selection.borrow().containsIndex(r);
        row_view.setSelected(selected);
        row_view.setEmphasized(self.emphasized());
        row_view.setSelectionHighlightStyle(self.ivars().highlight.get());
        let group = self.delegate_for(sel!(tableView:isGroupRow:)).is_some_and(|d| {
            // SAFETY: the delegate method takes the table and a row and
            // returns BOOL.
            unsafe { msg_send![&*d, tableView: table, isGroupRow: r as isize] }
        });
        row_view.setGroupRowStyle(group);
        let columns = self.ivars().columns.borrow().clone();
        let mut cells: Vec<Option<Retained<NSView>>> = vec![None; columns.len()];
        if group {
            if let Some(cell) = self.cell_for(None, r) {
                cell.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
                row_view.addSubview(&cell);
                if let Some(first) = cells.first_mut() {
                    *first = Some(cell);
                }
            }
        } else {
            for (c, col) in columns.iter().enumerate() {
                if column::is_hidden(col) {
                    continue;
                }
                if let Some(cell) = self.cell_for(Some(col), r) {
                    cell.setFrame(self.cell_frame(c, frame.size.height));
                    row_view.addSubview(&cell);
                    cells[c] = Some(cell);
                }
            }
        }
        self.as_view().addSubview(&row_view);
        self.ivars().realized.borrow_mut().insert(r, Realized { row: row_view.clone(), cells, ours });
        if let Some(d) = self.delegate_for(sel!(tableView:didAddRowView:forRow:)) {
            // SAFETY: the delegate method takes the table, a row view and a
            // row.
            let _: () = unsafe { msg_send![&*d, tableView: table, didAddRowView: &*row_view, forRow: r as isize] };
        }
    }

    /// A cell's frame in its row view: its column's width, past the
    /// column's lead.
    fn cell_frame(&self, column: usize, row_height: f64) -> NSRect {
        let (placed, _) = self.place_columns();
        let s = self.ivars().spacing.get();
        let (x, w) = match (placed.get(column), self.ivars().columns.borrow().get(column)) {
            (Some(p), Some(col)) => ((p.start + p.lead).floor(), column::span(col)),
            _ => (0.0, 0.0),
        };
        NSRect::new(NSPoint::new(x, (s.height / 2.0).floor()), NSSize::new(w, (row_height - s.height).max(0.0)))
    }

    /// The delegate's view for a cell, with the data source's object value.
    fn cell_for(&self, column: Option<&NSTableColumn>, r: usize) -> Option<Retained<NSView>> {
        let table = self.as_table();
        let d = self.delegate_for(sel!(tableView:viewForTableColumn:row:))?;
        // SAFETY: the delegate method takes the table, a column or nil and
        // a row, and returns a view or nil.
        let cell: Option<Retained<NSView>> =
            unsafe { msg_send![&*d, tableView: table, viewForTableColumn: column, row: r as isize] };
        let cell = cell?;
        if let (Some(col), Some(source)) = (column, self.source_for(sel!(tableView:objectValueForTableColumn:row:))) {
            // SAFETY: the data source method takes the table, a column and a
            // row, and returns an object or nil.
            let value: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*source, tableView: table, objectValueForTableColumn: col, row: r as isize] };
            if cell.respondsToSelector(sel!(setObjectValue:)) {
                // SAFETY: setObjectValue: takes an object.
                let _: () = unsafe { msg_send![&*cell, setObjectValue: value.as_deref()] };
            }
        }
        Some(cell)
    }

    /// Put realized rows and their cells where they belong now.
    fn place_rows(&self) {
        let rows: Vec<(usize, Realized)> =
            self.ivars().realized.borrow().iter().map(|(r, x)| (*r, x.clone())).collect();
        for (r, Realized { row: row_view, cells, .. }) in rows {
            let frame = self.rect_of_row(r);
            row_view.setFrame(frame);
            if row_view.isGroupRowStyle() {
                if let Some(Some(cell)) = cells.first() {
                    cell.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
                }
                continue;
            }
            for (c, cell) in cells.iter().enumerate() {
                if let Some(cell) = cell {
                    cell.setFrame(self.cell_frame(c, frame.size.height));
                }
            }
        }
    }

    /// Take the views of the rows `gone` picks, keeping cells and our row
    /// views for reuse.
    fn drop_rows(&self, gone: impl Fn(usize) -> bool) {
        let dropped: Vec<(usize, Realized)> = {
            let mut realized = self.ivars().realized.borrow_mut();
            let keys: Vec<usize> = realized.keys().copied().filter(|r| gone(*r)).collect();
            keys.into_iter().filter_map(|r| realized.remove(&r).map(|x| (r, x))).collect()
        };
        for (r, x) in dropped {
            self.recycle(r, x);
        }
    }

    fn recycle(&self, r: usize, x: Realized) {
        for cell in x.cells.into_iter().flatten() {
            cell.removeFromSuperview();
            if let Some(id) = cell.identifier() {
                self.ivars().pool.borrow_mut().entry(id.to_string()).or_default().push(cell);
            }
        }
        x.row.removeFromSuperview();
        if let Some(d) = self.delegate_for(sel!(tableView:didRemoveRowView:forRow:)) {
            // SAFETY: as for didAddRowView.
            let _: () =
                unsafe { msg_send![&*d, tableView: self.as_table(), didRemoveRowView: &*x.row, forRow: r as isize] };
        }
        if x.ours {
            for sub in views::subviews(views::imp(&x.row)) {
                sub.removeFromSuperview();
            }
            x.row.setSelected(false);
            self.ivars().row_pool.borrow_mut().push(x.row);
        }
    }

    fn reload_cells(&self, rows: &NSIndexSet, columns: &NSIndexSet) {
        let cols = self.ivars().columns.borrow().clone();
        for r in index_list(rows) {
            let Some(row_view) = self.ivars().realized.borrow().get(&r).map(|x| x.row.clone()) else { continue };
            let height = row_view.frame().size.height;
            for c in index_list(columns) {
                let Some(col) = cols.get(c) else { continue };
                let old = self.ivars().realized.borrow_mut().get_mut(&r).and_then(|x| x.cells.get_mut(c)?.take());
                if let Some(old) = old {
                    old.removeFromSuperview();
                    if let Some(id) = old.identifier() {
                        self.ivars().pool.borrow_mut().entry(id.to_string()).or_default().push(old);
                    }
                }
                if let Some(cell) = self.cell_for(Some(col), r) {
                    cell.setFrame(self.cell_frame(c, height));
                    row_view.addSubview(&cell);
                    if let Some(x) = self.ivars().realized.borrow_mut().get_mut(&r)
                        && let Some(slot) = x.cells.get_mut(c)
                    {
                        *slot = Some(cell);
                    }
                }
            }
        }
    }

    /// Shift realized rows from `from` on by `by`.
    fn shift_rows(&self, from: usize, by: isize) {
        let mut realized = self.ivars().realized.borrow_mut();
        let moved: Vec<usize> = realized.keys().copied().filter(|&r| r >= from).collect();
        let entries: Vec<(usize, Realized)> =
            moved.iter().filter_map(|r| realized.remove(r).map(|x| (*r, x))).collect();
        for (r, x) in entries {
            realized.insert((r as isize + by) as usize, x);
        }
    }

    /// Carry `selectedRow` and the anchor along a change of row numbers.
    fn renumber(&self, map: impl Fn(isize) -> Option<isize>) {
        for cell in [&self.ivars().last_selected, &self.ivars().anchor] {
            let r = cell.get();
            if r >= 0 {
                cell.set(map(r).unwrap_or(-1));
            }
        }
    }

    fn insert_rows_at(&self, indexes: &NSIndexSet) {
        let list = index_list(indexes);
        if list.is_empty() {
            return;
        }
        let count = self.row_count() + list.len();
        for &i in &list {
            self.shift_rows(i, 1);
            self.ivars().selection.borrow().shiftIndexesStartingAtIndex_by(i, 1);
            self.renumber(|r| Some(if r >= i as isize { r + 1 } else { r }));
        }
        self.ivars().rows.set(Some(count));
        // Only the new rows' heights are asked for.
        if self.ivars().heights.borrow().is_some() {
            let delegate = self.delegate_for(sel!(tableView:heightOfRow:));
            let new: Vec<f64> =
                list.iter().map(|&i| delegate.as_ref().map_or(0.0, |d| self.ask_height(d, i))).collect();
            if let Some(h) = self.ivars().heights.borrow_mut().as_mut() {
                for (&i, &height) in list.iter().zip(&new) {
                    h.insert(i, &[height]);
                }
            }
        }
        self.tile_frame();
        self.place_rows();
        self.realize();
    }

    fn remove_rows_at(&self, indexes: &NSIndexSet) {
        let list = index_list(indexes);
        if list.is_empty() {
            return;
        }
        let removes_selected = list.iter().any(|&i| self.ivars().selection.borrow().containsIndex(i));
        for &i in list.iter().rev() {
            let removed = self.ivars().realized.borrow_mut().remove(&i);
            if let Some(x) = removed {
                self.recycle(i, x);
            }
            self.shift_rows(i + 1, -1);
            let selection = self.ivars().selection.borrow().clone();
            selection.removeIndex(i);
            selection.shiftIndexesStartingAtIndex_by(i + 1, -1);
            let i = i as isize;
            self.renumber(|r| match r.cmp(&i) {
                std::cmp::Ordering::Less => Some(r),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(r - 1),
            });
            if let Some(h) = self.ivars().heights.borrow_mut().as_mut() {
                h.remove(i as usize, 1);
            }
        }
        // A removed selected row hands `selectedRow` to the first left.
        if self.ivars().last_selected.get() < 0 {
            let selection = self.ivars().selection.borrow().clone();
            self.ivars().last_selected.set(if selection.count() == 0 { -1 } else { selection.firstIndex() as isize });
        }
        let count = self.row_count().saturating_sub(list.len());
        self.ivars().rows.set(Some(count));
        self.tile_frame();
        self.place_rows();
        self.realize();
        if removes_selected {
            self.selection_changed();
        }
    }

    fn move_row_at(&self, from: isize, to: isize) {
        let count = self.row_count();
        let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else { return };
        if from >= count || to >= count || from == to {
            return;
        }
        {
            let mut realized = self.ivars().realized.borrow_mut();
            let moving = realized.remove(&from);
            let mut rest: Vec<(usize, Realized)> = std::mem::take(&mut *realized).into_iter().collect();
            for (r, _) in rest.iter_mut() {
                if from < to && *r > from && *r <= to {
                    *r -= 1;
                } else if to < from && *r >= to && *r < from {
                    *r += 1;
                }
            }
            realized.extend(rest);
            if let Some(m) = moving {
                realized.insert(to, m);
            }
        }
        let selected = self.ivars().selection.borrow().containsIndex(from);
        let selection = self.ivars().selection.borrow().clone();
        selection.removeIndex(from);
        if from < to {
            selection.shiftIndexesStartingAtIndex_by(from + 1, -1);
            selection.shiftIndexesStartingAtIndex_by(to, 1);
        } else {
            selection.shiftIndexesStartingAtIndex_by(to, 1);
            selection.shiftIndexesStartingAtIndex_by(from + 1, -1);
        }
        if selected {
            selection.addIndex(to);
        }
        let (from, to) = (from as isize, to as isize);
        self.renumber(|r| {
            Some(if r == from {
                to
            } else if from < to && r > from && r <= to {
                r - 1
            } else if to < from && r >= to && r < from {
                r + 1
            } else {
                r
            })
        });
        if let Some(h) = self.ivars().heights.borrow_mut().as_mut() {
            h.move_row(from as usize, to as usize);
        }
        self.place_rows();
    }

    // Selection.

    fn selection_copy(&self) -> Retained<NSMutableIndexSet> {
        self.ivars().selection.borrow().mutableCopy()
    }

    fn should_select(&self, r: usize) -> bool {
        self.delegate_for(sel!(tableView:shouldSelectRow:)).is_none_or(|d| {
            // SAFETY: the delegate method takes the table and a row and
            // returns BOOL.
            unsafe { msg_send![&*d, tableView: self.as_table(), shouldSelectRow: r as isize] }
        })
    }

    /// What the delegate makes of a proposed selection.
    fn proposed(&self, proposed: &NSIndexSet) -> Retained<NSIndexSet> {
        match self.delegate_for(sel!(tableView:selectionIndexesForProposedSelection:)) {
            // SAFETY: the delegate method takes the table and an index set
            // and returns an index set.
            Some(d) => unsafe {
                msg_send![&*d, tableView: self.as_table(), selectionIndexesForProposedSelection: proposed]
            },
            None => proposed.copy(),
        }
    }

    fn select_rows(&self, indexes: &NSIndexSet, extend: bool) {
        let count = self.row_count();
        let now = if extend { self.selection_copy() } else { NSMutableIndexSet::new() };
        let mut added = None;
        for i in index_list(indexes) {
            if i < count {
                now.addIndex(i);
                added = Some(i);
            }
        }
        if index_list(indexes).iter().any(|&i| i >= count) && added.is_none() {
            return;
        }
        if let Some(a) = added {
            self.ivars().last_selected.set(a as isize);
            self.ivars().anchor.set(a as isize);
        }
        self.set_selection(&now);
    }

    /// Take `new` as the selection; tell everyone if it changed.
    fn set_selection(&self, new: &NSIndexSet) {
        if new.isEqualToIndexSet(&self.ivars().selection.borrow()) {
            return;
        }
        let old = self.selection_copy();
        let copy = new.mutableCopy();
        self.ivars().selection.replace(copy);
        let last = self.ivars().last_selected.get();
        if last < 0 || !new.containsIndex(last as usize) {
            let l = new.lastIndex();
            self.ivars().last_selected.set(if new.count() == 0 { -1 } else { l as isize });
        }
        // Row views show it.
        let realized: Vec<(usize, Retained<NSTableRowView>)> =
            self.ivars().realized.borrow().iter().map(|(r, x)| (*r, x.row.clone())).collect();
        for (r, row) in realized {
            let on = new.containsIndex(r);
            if on != old.containsIndex(r) {
                row.setSelected(on);
            }
        }
        self.selection_changed();
    }

    fn selection_changed(&self) {
        // SAFETY: the name is a constant this module exports.
        let name = unsafe { objc2_app_kit::NSTableViewSelectionDidChangeNotification };
        let delegate = self.delegate_for(sel!(tableViewSelectionDidChange:));
        if delegate.is_none() && !sidestep_foundation::notification_center::has_observers(name) {
            return;
        }
        let note = self.notification(name);
        if let Some(d) = delegate {
            // SAFETY: the delegate method takes the notification.
            let _: () = unsafe { msg_send![&*d, tableViewSelectionDidChange: &*note] };
        }
        sidestep_foundation::notification_center::default_center().postNotification(&note);
    }

    fn notification(&self, name: &NSString) -> Retained<objc2_foundation::NSNotification> {
        sidestep_foundation::notification(name, Some(self.as_view()))
    }

    fn trim_selection(&self, count: usize) {
        let now = self.selection_copy();
        let beyond = now.countOfIndexesInRange(NSRange::new(count, usize::MAX / 2));
        if beyond > 0 {
            now.removeIndexesInRange(NSRange::new(count, usize::MAX / 2));
            self.set_selection(&now);
        }
    }

    /// Where a view is: its row, and its column if it's a cell.
    fn locate(&self, view: &NSView) -> Option<(usize, Option<usize>)> {
        let realized = self.ivars().realized.borrow();
        let mut cur = Some(views::imp(view));
        while let Some(v) = cur {
            for (r, x) in realized.iter() {
                if std::ptr::eq(views::imp(&x.row), v) {
                    return Some((*r, None));
                }
                if let Some(c) = x
                    .cells
                    .iter()
                    .position(|cell| cell.as_deref().is_some_and(|cell| std::ptr::eq(views::imp(cell), v)))
                {
                    return Some((*r, Some(c)));
                }
            }
            cur = views::superview_of(v);
        }
        None
    }

    // Input.

    fn click(&self, event: &NSEvent) {
        let p = self.as_view().convertPoint_fromView(event.locationInWindow(), None);
        let (row, column) = (self.as_table().rowAtPoint(p), self.as_table().columnAtPoint(p));
        self.ivars().clicked.set((row, column));
        if let Some(window) = self.as_view().window() {
            window.makeFirstResponder(Some(self.as_view()));
        }
        let flags = event.modifierFlags();
        let multiple = self.ivars().multiple.get();
        if row < 0 {
            if self.ivars().empty.get() && !flags.contains(NSEventModifierFlags::Command) {
                self.set_selection(&NSIndexSet::new());
            }
            return;
        }
        let r = row as usize;
        let proposed = if multiple && flags.contains(NSEventModifierFlags::Command) {
            let now = self.selection_copy();
            if now.containsIndex(r) {
                if now.count() > 1 || self.ivars().empty.get() {
                    now.removeIndex(r);
                }
            } else if self.should_select(r) {
                now.addIndex(r);
            }
            now
        } else if multiple && flags.contains(NSEventModifierFlags::Shift) && self.ivars().anchor.get() >= 0 {
            let a = self.ivars().anchor.get() as usize;
            let (lo, hi) = (a.min(r), a.max(r));
            let now = NSMutableIndexSet::new();
            for i in lo..=hi {
                if self.should_select(i) {
                    now.addIndex(i);
                }
            }
            now
        } else {
            if !self.should_select(r) {
                return;
            }
            self.ivars().anchor.set(row);
            NSMutableIndexSet::indexSetWithIndex(r)
        };
        let chosen = self.proposed(&proposed);
        if chosen.containsIndex(r) {
            self.ivars().last_selected.set(row);
        }
        self.set_selection(&chosen);
    }

    fn send_click_action(&self, event: &NSEvent) {
        let action = if event.clickCount() >= 2 { self.ivars().double_action.get() } else { self.ivars().action.get() };
        if let Some(action) = action {
            let target = self.ivars().target.borrow().as_ref().and_then(Weak::load);
            let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
            // SAFETY: the action is sent with the table as its argument.
            let _ = unsafe { app.sendAction_to_from(action, target.as_deref(), Some(self.as_view())) };
        }
        self.ivars().clicked.set((-1, -1));
    }

    /// Move the selection one row up (-1) or down (1), as the arrow keys do.
    fn step_selection(&self, by: isize) {
        let count = self.row_count() as isize;
        if count == 0 {
            return;
        }
        let from = self.ivars().last_selected.get();
        let mut to = if from < 0 { if by > 0 { 0 } else { count - 1 } } else { from + by };
        while (0..count).contains(&to) && !self.should_select(to as usize) {
            to += by;
        }
        if !(0..count).contains(&to) {
            return;
        }
        let chosen = self.proposed(&NSMutableIndexSet::indexSetWithIndex(to as usize));
        self.ivars().last_selected.set(to);
        self.ivars().anchor.set(to);
        self.set_selection(&chosen);
        self.as_table().scrollRowToVisible(to);
    }

    // Drawing.

    fn draw(&self, dirty: NSRect) {
        self.as_table().backgroundColor().setFill();
        NSBezierPath::fillRect(dirty);
        if self.ivars().alternating.get()
            && let Some((first, last)) = self.rows_in(dirty.origin.y, dirty.origin.y + dirty.size.height)
        {
            NSColor::colorWithWhite_alpha(0.96, 1.0).setFill();
            for r in (first..=last).filter(|r| r % 2 == 1) {
                NSBezierPath::fillRect(self.rect_of_row(r));
            }
        }
        let grid = self.ivars().grid.get();
        if grid.0 == 0 {
            return;
        }
        self.as_table().gridColor().setFill();
        if grid.contains(NSTableViewGridLineStyle::SolidVerticalGridLineMask) {
            let count = self.ivars().columns.borrow().len();
            for c in 0..count {
                if let Some((x, w)) = self.column_span(c as isize) {
                    let line =
                        NSRect::new(NSPoint::new(x + w - 1.0, dirty.origin.y), NSSize::new(1.0, dirty.size.height));
                    NSBezierPath::fillRect(line);
                }
            }
        }
        if grid.contains(NSTableViewGridLineStyle::SolidHorizontalGridLineMask)
            && let Some((first, last)) = self.rows_in(dirty.origin.y, dirty.origin.y + dirty.size.height)
        {
            for r in first..=last {
                let row = self.rect_of_row(r);
                let line = NSRect::new(
                    NSPoint::new(dirty.origin.x, row.origin.y + row.size.height - 1.0),
                    NSSize::new(dirty.size.width, 1.0),
                );
                NSBezierPath::fillRect(line);
            }
        }
    }
}
