//! Text blocks and tables: `NSTextBlock`, `NSTextTableBlock` and
//! `NSTextTable`, and where the layout manager puts the paragraphs a
//! paragraph style's `textBlocks` enclose.
//!
//! A block's settings are widths for each of its layers (padding, border,
//! margin; innermost first) and edges, and its dimensions, each absolute
//! or a percentage of the width of the rect it is laid out in (vertical
//! edges too). A block is laid out in its enclosing rect (the container's
//! width less its line fragment padding at each end, or the enclosing
//! block's layout rect): its layout rect starts inside its left edge's
//! layers, as wide as its content width (none set is zero wide), never
//! more than fits. Its bounds are its content (from the top of its first
//! paragraph's first line fragment to the bottom of its last paragraph's
//! last line, before that paragraph's trailing spacing) with its layers
//! around it. Consecutive paragraphs that share a block (the same object,
//! enclosed the same way) are in it together; its top layers come before
//! the first, and after the last the text goes on below the lowest bottom
//! of the blocks that end there.
//!
//! A table's cells are table blocks. The paragraphs of a row (consecutive
//! paragraphs whose cells are in the same row of the same table) are laid
//! out side by side: the table is laid out like a block (none set is as
//! wide as fits; its top layers come before its first row, and nothing
//! after its last), and split into as many columns as the row's cells
//! reach, a cell with a content width taking that much and the others
//! sharing the rest evenly (whole points). A cell's bounds run the row's
//! height, the tallest of its cells. With `collapsesBorders` each cell
//! keeps half of each border (rounded up to a half point) and the table
//! moves half a point right and down. Height dimensions, vertical
//! alignment, row spans and `hidesEmptyCells` are kept but not used in
//! layout.
//!
//! Drawing fills a block's bounds with its background color, margins
//! included, then draws each border edge in its color: the left and right
//! ones the border box's height, the top and bottom ones between them. A
//! cell's collapsed borders are drawn whole, centered on its edges, so
//! neighbours' fall on one line; borders thinner than a point are shapes,
//! antialiased, so hairlines show whatever the pixels' size.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSLayoutManager, NSParagraphStyle, NSParagraphStyleAttributeName, NSTextBlock, NSTextBlockDimension,
    NSTextBlockLayer, NSTextBlockValueType, NSTextBlockVerticalAlignment, NSTextContainer, NSTextTable,
    NSTextTableBlock, NSTextTableLayoutAlgorithm, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSInteger, NSNotFound, NSPoint, NSRange, NSRect, NSRectEdge, NSSize, NSString, NSUInteger,
    NSZone,
};
use smallvec::SmallVec;

use crate::protocol::Op;

sidestep_runtime::static_class!(pub(crate) NSTEXTBLOCK, NSTEXTBLOCK_META = "NSTextBlock", || {
    let _ = NSTextBlockImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTTABLEBLOCK, NSTEXTTABLEBLOCK_META = "NSTextTableBlock", || {
    let _ = NSTextTableBlockImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTTABLE, NSTEXTTABLE_META = "NSTextTable", || {
    let _ = NSTextTableImpl::class();
});

/// A paragraph's text blocks, outermost first.
pub(crate) type Chain = Arc<[Retained<NSTextBlock>]>;

/// Edges, as `NSRectEdge` numbers them.
pub(crate) const MIN_X: usize = 0;
pub(crate) const MIN_Y: usize = 1;
pub(crate) const MAX_X: usize = 2;
pub(crate) const MAX_Y: usize = 3;

/// The border layer's index in [`Values::widths`].
const BORDER: usize = 1;

/// A width or dimension: points, or a percentage of a width.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Val {
    v: f64,
    pct: bool,
}

impl Val {
    fn new(v: f64, kind: NSTextBlockValueType) -> Val {
        Val { v, pct: kind == NSTextBlockValueType::PercentageValueType }
    }

    fn kind(self) -> NSTextBlockValueType {
        if self.pct { NSTextBlockValueType::PercentageValueType } else { NSTextBlockValueType::AbsoluteValueType }
    }

    /// In points, percentages of `basis`.
    fn at(self, basis: f64) -> f64 {
        if self.pct { self.v * basis / 100.0 } else { self.v }
    }
}

/// A block's numbers.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Values {
    /// By layer (padding, border, margin) and edge.
    widths: [[Val; 4]; 3],
    /// Width, minimum and maximum width, height, minimum and maximum height.
    dims: [Val; 6],
    valign: NSUInteger,
}

impl Values {
    /// The layers' total on `edge`, percentages of `basis`; with
    /// `collapse`, half the border.
    pub fn decor(&self, edge: usize, basis: f64, collapse: bool) -> f64 {
        let mut sum = 0.0;
        for (layer, widths) in self.widths.iter().enumerate() {
            let w = widths[edge].at(basis);
            sum += if collapse && layer == BORDER { half_border(w) } else { w };
        }
        sum
    }

    /// The content width set, in points (0 when none is).
    pub fn content_width(&self, basis: f64) -> f64 {
        self.dims[0].at(basis)
    }

    /// The layout rect across (x, width) in an enclosing one.
    pub fn across(&self, x: f64, w: f64) -> (f64, f64) {
        let (l, r) = (self.decor(MIN_X, w, false), self.decor(MAX_X, w, false));
        let fits = (w - l - r).max(0.0);
        (x + l, self.content_width(w).min(fits).max(0.0))
    }
}

/// Half of a border, as a cell keeps it with collapsed borders: rounded
/// up to a half point.
fn half_border(w: f64) -> f64 {
    w.max(0.0).ceil() / 2.0
}

fn layer_index(layer: NSTextBlockLayer) -> Option<usize> {
    match layer {
        NSTextBlockLayer::Padding => Some(0),
        NSTextBlockLayer::Border => Some(1),
        NSTextBlockLayer::Margin => Some(2),
        _ => None,
    }
}

fn edge_index(edge: NSRectEdge) -> Option<usize> {
    Some(edge.0).filter(|&e| e < 4)
}

fn dim_index(d: NSTextBlockDimension) -> Option<usize> {
    match d.0 {
        0..=2 => Some(d.0),
        4..=6 => Some(d.0 - 1),
        _ => None,
    }
}

pub(crate) struct BlockIvars {
    values: Cell<Values>,
    background: RefCell<Option<Retained<NSColor>>>,
    borders: RefCell<[Option<Retained<NSColor>>; 4]>,
}

impl BlockIvars {
    fn new(values: Values) -> BlockIvars {
        BlockIvars { values: Cell::new(values), background: RefCell::default(), borders: RefCell::default() }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextBlock"]
    #[ivars = BlockIvars]
    pub(crate) struct NSTextBlockImpl;

    impl NSTextBlockImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(BlockIvars::new(Values::default()));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(setValue:type:forDimension:))]
        fn set_value(&self, value: f64, kind: NSTextBlockValueType, dimension: NSTextBlockDimension) {
            if let Some(d) = dim_index(dimension) {
                self.update(|v| v.dims[d] = Val::new(value, kind));
            }
        }

        #[unsafe(method(valueForDimension:))]
        fn value_for_dimension(&self, dimension: NSTextBlockDimension) -> f64 {
            dim_index(dimension).map_or(0.0, |d| self.values().dims[d].v)
        }

        #[unsafe(method(valueTypeForDimension:))]
        fn value_type_for_dimension(&self, dimension: NSTextBlockDimension) -> NSTextBlockValueType {
            dim_index(dimension).map_or(NSTextBlockValueType::AbsoluteValueType, |d| self.values().dims[d].kind())
        }

        #[unsafe(method(setContentWidth:type:))]
        fn set_content_width(&self, value: f64, kind: NSTextBlockValueType) {
            self.update(|v| v.dims[0] = Val::new(value, kind));
        }

        #[unsafe(method(contentWidth))]
        fn content_width(&self) -> f64 {
            self.values().dims[0].v
        }

        #[unsafe(method(contentWidthValueType))]
        fn content_width_value_type(&self) -> NSTextBlockValueType {
            self.values().dims[0].kind()
        }

        #[unsafe(method(setWidth:type:forLayer:edge:))]
        fn set_width_for_edge(&self, value: f64, kind: NSTextBlockValueType, layer: NSTextBlockLayer, edge: NSRectEdge) {
            if let (Some(l), Some(e)) = (layer_index(layer), edge_index(edge)) {
                self.update(|v| v.widths[l][e] = Val::new(value, kind));
            }
        }

        #[unsafe(method(setWidth:type:forLayer:))]
        fn set_width(&self, value: f64, kind: NSTextBlockValueType, layer: NSTextBlockLayer) {
            if let Some(l) = layer_index(layer) {
                self.update(|v| v.widths[l] = [Val::new(value, kind); 4]);
            }
        }

        #[unsafe(method(widthForLayer:edge:))]
        fn width_for_layer(&self, layer: NSTextBlockLayer, edge: NSRectEdge) -> f64 {
            match (layer_index(layer), edge_index(edge)) {
                (Some(l), Some(e)) => self.values().widths[l][e].v,
                _ => 0.0,
            }
        }

        #[unsafe(method(widthValueTypeForLayer:edge:))]
        fn width_value_type(&self, layer: NSTextBlockLayer, edge: NSRectEdge) -> NSTextBlockValueType {
            match (layer_index(layer), edge_index(edge)) {
                (Some(l), Some(e)) => self.values().widths[l][e].kind(),
                _ => NSTextBlockValueType::AbsoluteValueType,
            }
        }

        #[unsafe(method(verticalAlignment))]
        fn vertical_alignment(&self) -> NSTextBlockVerticalAlignment {
            NSTextBlockVerticalAlignment(self.values().valign)
        }

        #[unsafe(method(setVerticalAlignment:))]
        fn set_vertical_alignment(&self, alignment: NSTextBlockVerticalAlignment) {
            self.update(|v| v.valign = alignment.0);
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Option<Retained<NSColor>> {
            self.ivars().background.borrow().clone()
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            let old = self.ivars().background.replace(color.map(|c| c.retain()));
            drop(old);
        }

        #[unsafe(method(setBorderColor:forEdge:))]
        fn set_border_color_for_edge(&self, color: Option<&NSColor>, edge: NSRectEdge) {
            if let Some(e) = edge_index(edge) {
                let old = std::mem::replace(&mut self.ivars().borders.borrow_mut()[e], color.map(|c| c.retain()));
                drop(old);
            }
        }

        #[unsafe(method(setBorderColor:))]
        fn set_border_color(&self, color: Option<&NSColor>) {
            let new = [(); 4].map(|_| color.map(|c| c.retain()));
            let old = self.ivars().borders.replace(new);
            drop(old);
        }

        #[unsafe(method_id(borderColorForEdge:))]
        fn border_color_for_edge(&self, edge: NSRectEdge) -> Option<Retained<NSColor>> {
            edge_index(edge).and_then(|e| self.ivars().borders.borrow()[e].clone())
        }

        #[unsafe(method(rectForLayoutAtPoint:inRect:textContainer:characterRange:))]
        fn rect_for_layout(&self, point: NSPoint, rect: NSRect, _c: &NSTextContainer, _r: NSRange) -> NSRect {
            layout_rect(&self.values(), point, rect)
        }

        #[unsafe(method(boundsRectForContentRect:inRect:textContainer:characterRange:))]
        fn bounds_rect(&self, content: NSRect, rect: NSRect, _c: &NSTextContainer, _r: NSRange) -> NSRect {
            bounds_rect(&self.values(), content, rect.size.width, false)
        }

        #[unsafe(method(drawBackgroundWithFrame:inView:characterRange:layoutManager:))]
        fn draw_background(&self, frame: NSRect, _view: &NSView, _r: NSRange, _lm: &NSLayoutManager) {
            draw_box(self.as_block(), frame, frame.size.width, true, true, false);
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSTextBlockImpl {
            let copy = new_block(self.values());
            copy_colors(self, &copy);
            Retained::into_raw(copy)
        }
    }

    unsafe impl NSObjectProtocol for NSTextBlockImpl {}
);

impl NSTextBlockImpl {
    fn values(&self) -> Values {
        self.ivars().values.get()
    }

    fn update(&self, f: impl FnOnce(&mut Values)) {
        let mut v = self.values();
        f(&mut v);
        self.ivars().values.set(v);
    }

    fn as_block(&self) -> &NSTextBlock {
        // SAFETY: this class is NSTextBlock.
        unsafe { &*(self as *const Self).cast::<NSTextBlock>() }
    }
}

fn new_block(values: Values) -> Retained<NSTextBlockImpl> {
    crate::load_shell::<NSTextBlock>();
    let this = NSTextBlockImpl::alloc().set_ivars(BlockIvars::new(values));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn copy_colors(from: &NSTextBlockImpl, to: &NSTextBlockImpl) {
    let background = from.ivars().background.borrow().clone();
    let borders = from.ivars().borders.borrow().clone();
    drop(to.ivars().background.replace(background));
    drop(to.ivars().borders.replace(borders));
}

/// Any block's own settings: every `NSTextBlock` is (or inherits from)
/// Sidestep's.
pub(crate) fn base(b: &NSTextBlock) -> &NSTextBlockImpl {
    // SAFETY: NSTextBlock is NSTextBlockImpl's class; its subclasses
    // inherit its instance variables.
    unsafe { &*(b as *const NSTextBlock).cast::<NSTextBlockImpl>() }
}

/// A block's layout rect in `rect`, from `point` down: inside its left,
/// right and top layers, as wide as its content width fits, down to the
/// rect's bottom.
fn layout_rect(v: &Values, point: NSPoint, rect: NSRect) -> NSRect {
    let w = rect.size.width;
    let (x, width) = v.across(rect.origin.x, w);
    let y = point.y + v.decor(MIN_Y, w, false);
    let bottom = rect.origin.y + rect.size.height;
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, (bottom - y).max(0.0)))
}

/// A block's bounds around its content: the content with its layers.
fn bounds_rect(v: &Values, content: NSRect, basis: f64, collapse: bool) -> NSRect {
    let d = |e| v.decor(e, basis, collapse);
    NSRect::new(
        NSPoint::new(content.origin.x - d(MIN_X), content.origin.y - d(MIN_Y)),
        NSSize::new(content.size.width + d(MIN_X) + d(MAX_X), content.size.height + d(MIN_Y) + d(MAX_Y)),
    )
}

// NSTextTableBlock

pub(crate) struct CellIvars {
    table: RefCell<Option<Retained<NSTextTable>>>,
    row: Cell<NSInteger>,
    row_span: Cell<NSInteger>,
    column: Cell<NSInteger>,
    column_span: Cell<NSInteger>,
}

define_class!(
    #[unsafe(super(NSTextBlock, NSObject))]
    #[name = "NSTextTableBlock"]
    #[ivars = CellIvars]
    pub(crate) struct NSTextTableBlockImpl;

    impl NSTextTableBlockImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(cell_ivars(None, 0, 1, 0, 1));
            // SAFETY: NSTextBlock's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithTable:startingRow:rowSpan:startingColumn:columnSpan:))]
        fn init_with_table(
            this: Allocated<Self>,
            table: &NSTextTable,
            row: NSInteger,
            row_span: NSInteger,
            column: NSInteger,
            column_span: NSInteger,
        ) -> Retained<Self> {
            let this = this.set_ivars(cell_ivars(Some(table.retain()), row, row_span, column, column_span));
            // SAFETY: NSTextBlock's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(table))]
        fn table(&self) -> Option<Retained<NSTextTable>> {
            self.ivars().table.borrow().clone()
        }

        #[unsafe(method(startingRow))]
        fn starting_row(&self) -> NSInteger {
            self.ivars().row.get()
        }

        #[unsafe(method(rowSpan))]
        fn row_span(&self) -> NSInteger {
            self.ivars().row_span.get()
        }

        #[unsafe(method(startingColumn))]
        fn starting_column(&self) -> NSInteger {
            self.ivars().column.get()
        }

        #[unsafe(method(columnSpan))]
        fn column_span(&self) -> NSInteger {
            self.ivars().column_span.get()
        }

        #[unsafe(method(rectForLayoutAtPoint:inRect:textContainer:characterRange:))]
        fn rect_for_layout(&self, point: NSPoint, rect: NSRect, _c: &NSTextContainer, _r: NSRange) -> NSRect {
            cell_layout_rect(self.as_cell(), point, rect)
        }

        #[unsafe(method(boundsRectForContentRect:inRect:textContainer:characterRange:))]
        fn bounds_rect(&self, content: NSRect, rect: NSRect, _c: &NSTextContainer, _r: NSRange) -> NSRect {
            let link = link(self.as_cell());
            let collapse = link.cell.is_some_and(|c| c.collapses);
            bounds_rect(&link.values, content, rect.size.width, collapse)
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSTextTableBlockImpl {
            let iv = self.ivars();
            let table = iv.table.borrow().clone();
            crate::load_shell::<NSTextTableBlock>();
            let copy = NSTextTableBlockImpl::alloc().set_ivars(cell_ivars(
                table,
                iv.row.get(),
                iv.row_span.get(),
                iv.column.get(),
                iv.column_span.get(),
            ));
            // SAFETY: NSTextBlock's designated initializer.
            let copy: Retained<NSTextTableBlockImpl> = unsafe { msg_send![super(copy), init] };
            let (from, to) = (base(self.as_cell()), base(copy.as_cell()));
            to.ivars().values.set(from.values());
            copy_colors(from, to);
            Retained::into_raw(copy)
        }
    }
);

impl NSTextTableBlockImpl {
    fn as_cell(&self) -> &NSTextBlock {
        // SAFETY: this class is NSTextTableBlock, an NSTextBlock.
        unsafe { &*(self as *const Self).cast::<NSTextBlock>() }
    }
}

fn cell_ivars(table: Option<Retained<NSTextTable>>, row: isize, rs: isize, col: isize, cs: isize) -> CellIvars {
    CellIvars {
        table: RefCell::new(table),
        row: Cell::new(row),
        row_span: Cell::new(rs),
        column: Cell::new(col),
        column_span: Cell::new(cs),
    }
}

// NSTextTable

pub(crate) struct TableIvars {
    columns: Cell<NSUInteger>,
    algorithm: Cell<NSTextTableLayoutAlgorithm>,
    collapses: Cell<bool>,
    hides_empty: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSTextBlock, NSObject))]
    #[name = "NSTextTable"]
    #[ivars = TableIvars]
    pub(crate) struct NSTextTableImpl;

    impl NSTextTableImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(TableIvars {
                columns: Cell::new(0),
                algorithm: Cell::new(NSTextTableLayoutAlgorithm::AutomaticLayoutAlgorithm),
                collapses: Cell::new(false),
                hides_empty: Cell::new(false),
            });
            // SAFETY: NSTextBlock's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(numberOfColumns))]
        fn number_of_columns(&self) -> NSUInteger {
            self.ivars().columns.get()
        }

        #[unsafe(method(setNumberOfColumns:))]
        fn set_number_of_columns(&self, n: NSUInteger) {
            self.ivars().columns.set(n);
        }

        #[unsafe(method(layoutAlgorithm))]
        fn layout_algorithm(&self) -> NSTextTableLayoutAlgorithm {
            self.ivars().algorithm.get()
        }

        #[unsafe(method(setLayoutAlgorithm:))]
        fn set_layout_algorithm(&self, algorithm: NSTextTableLayoutAlgorithm) {
            self.ivars().algorithm.set(algorithm);
        }

        #[unsafe(method(collapsesBorders))]
        fn collapses_borders(&self) -> bool {
            self.ivars().collapses.get()
        }

        #[unsafe(method(setCollapsesBorders:))]
        fn set_collapses_borders(&self, flag: bool) {
            self.ivars().collapses.set(flag);
        }

        #[unsafe(method(hidesEmptyCells))]
        fn hides_empty_cells(&self) -> bool {
            self.ivars().hides_empty.get()
        }

        #[unsafe(method(setHidesEmptyCells:))]
        fn set_hides_empty_cells(&self, flag: bool) {
            self.ivars().hides_empty.set(flag);
        }

        #[unsafe(method(rectForBlock:layoutAtPoint:inRect:textContainer:characterRange:))]
        fn rect_for_block(
            &self,
            block: &NSTextTableBlock,
            point: NSPoint,
            rect: NSRect,
            _c: &NSTextContainer,
            _r: NSRange,
        ) -> NSRect {
            cell_layout_rect(block, point, rect)
        }

        #[unsafe(method(boundsRectForBlock:contentRect:inRect:textContainer:characterRange:))]
        fn bounds_rect_for_block(
            &self,
            block: &NSTextTableBlock,
            content: NSRect,
            rect: NSRect,
            _c: &NSTextContainer,
            _r: NSRange,
        ) -> NSRect {
            bounds_rect(&base(block).values(), content, rect.size.width, self.ivars().collapses.get())
        }

        #[unsafe(method(drawBackgroundForBlock:withFrame:inView:characterRange:layoutManager:))]
        fn draw_background_for_block(
            &self,
            block: &NSTextTableBlock,
            frame: NSRect,
            _view: &NSView,
            _r: NSRange,
            _lm: &NSLayoutManager,
        ) {
            draw_box(block, frame, frame.size.width, true, true, self.ivars().collapses.get());
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSTextTableImpl {
            let iv = self.ivars();
            crate::load_shell::<NSTextTable>();
            let copy = NSTextTableImpl::alloc().set_ivars(TableIvars {
                columns: Cell::new(iv.columns.get()),
                algorithm: Cell::new(iv.algorithm.get()),
                collapses: Cell::new(iv.collapses.get()),
                hides_empty: Cell::new(iv.hides_empty.get()),
            });
            // SAFETY: NSTextBlock's designated initializer.
            let copy: Retained<NSTextTableImpl> = unsafe { msg_send![super(copy), init] };
            let (from, to) = (base(self.as_block()), base(copy.as_block()));
            to.ivars().values.set(from.values());
            copy_colors(from, to);
            Retained::into_raw(copy)
        }
    }
);

impl NSTextTableImpl {
    fn as_block(&self) -> &NSTextBlock {
        // SAFETY: this class is NSTextTable, an NSTextBlock.
        unsafe { &*(self as *const Self).cast::<NSTextBlock>() }
    }
}

/// A cell's layout rect on its own: its table laid out in `rect`, split
/// evenly into the table's columns, and the cell's columns inside its
/// layers. (Layout managers work out a row's columns from all its cells.)
fn cell_layout_rect(cell: &NSTextBlock, point: NSPoint, rect: NSRect) -> NSRect {
    let link = link(cell);
    let Some(c) = link.cell else { return layout_rect(&link.values, point, rect) };
    let columns = base_table(c.table).ivars().columns.get().max(c.reach());
    let spec = [CellSpec { column: c.column, span: c.span, width: None }];
    let (tx, widths) = columns_of(&c, rect.origin.x, rect.size.width, columns.max(1), &spec);
    let (x, w) = cell_across(&c, &widths, tx);
    let (lx, lw) = inset_cell(&link.values, x, w, c.collapses);
    let y = point.y + c.table_values.decor(MIN_Y, rect.size.width, false) + link.values.decor(MIN_Y, w, c.collapses);
    let bottom = rect.origin.y + rect.size.height;
    NSRect::new(NSPoint::new(lx, y), NSSize::new(lw, (bottom - y).max(0.0)))
}

fn base_table(t: *const NSTextTable) -> &'static NSTextTableImpl {
    // SAFETY: callers pass a table a live cell holds; NSTextTable is
    // NSTextTableImpl's class.
    unsafe { &*t.cast::<NSTextTableImpl>() }
}

// Layout.

/// A block as layout reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Link {
    pub values: Values,
    pub cell: Option<CellLink>,
}

/// A table cell's place: its table and settings, row and columns.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellLink {
    pub table: *const NSTextTable,
    pub table_values: Values,
    pub collapses: bool,
    pub row: isize,
    pub column: usize,
    pub span: usize,
}

impl CellLink {
    /// The columns up to the cell's last.
    fn reach(&self) -> usize {
        self.column + self.span
    }
}

/// How `b` lays out.
pub(crate) fn link(b: &NSTextBlock) -> Link {
    let values = base(b).values();
    let cell = super::is_kind(b.class(), NSTextTableBlockImpl::class())
        .then(|| {
            // SAFETY: an NSTextTableBlock is an NSTextTableBlockImpl.
            let c = unsafe { &*(b as *const NSTextBlock).cast::<NSTextTableBlockImpl>() };
            let iv = c.ivars();
            let table = iv.table.borrow().as_ref().map(Retained::as_ptr)?;
            let t = base_table(table);
            // SAFETY: a table is an NSTextBlock.
            let table_values = base(unsafe { &*table.cast::<NSTextBlock>() }).values();
            Some(CellLink {
                table,
                table_values,
                collapses: t.ivars().collapses.get(),
                row: iv.row.get(),
                column: usize::try_from(iv.column.get()).unwrap_or(0),
                span: usize::try_from(iv.column_span.get()).unwrap_or(1).max(1),
            })
        })
        .flatten();
    Link { values, cell }
}

/// Whether `a` and `b` enclose the same way through level `k`: the same
/// blocks, outermost first.
pub(crate) fn same_through(a: &[Retained<NSTextBlock>], b: &[Retained<NSTextBlock>], k: usize) -> bool {
    a.len() > k && b.len() > k && (0..=k).all(|j| std::ptr::eq(&*a[j], &*b[j]))
}

/// Where a chain's first table cell is, and its table and row.
pub(crate) fn row_key(chain: &[Retained<NSTextBlock>]) -> Option<(usize, *const NSTextTable, isize)> {
    chain.iter().enumerate().find_map(|(k, b)| link(b).cell.map(|c| (k, c.table, c.row)))
}

/// Whether two paragraphs' chains put them in the same table, enclosed
/// alike; and in the same row.
pub(crate) fn same_table(a: &[Retained<NSTextBlock>], b: &[Retained<NSTextBlock>]) -> (bool, bool) {
    match (row_key(a), row_key(b)) {
        (Some((k, t, r)), Some((k2, t2, r2))) if k == k2 && std::ptr::eq(t, t2) => {
            let outer = k == 0 || same_through(a, b, k - 1);
            (outer, outer && r == r2)
        }
        _ => (false, false),
    }
}

/// A cell of a row, for working out the row's columns.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellSpec {
    pub column: usize,
    pub span: usize,
    /// The width set for its column: content width and layers.
    pub width: Option<f64>,
}

/// A table's columns in an enclosing rect across: the table's left and
/// each column's width.
fn columns_of(c: &CellLink, x: f64, w: f64, n: usize, cells: &[CellSpec]) -> (f64, Vec<f64>) {
    let tv = &c.table_values;
    let (l, r) = (tv.decor(MIN_X, w, false), tv.decor(MAX_X, w, false));
    let fits = (w - l - r).max(0.0);
    let set = tv.content_width(w);
    let tw = if set > 0.0 { set.min(fits) } else { fits };
    let tx = x + l + if c.collapses { 0.5 } else { 0.0 };
    let mut widths: Vec<Option<f64>> = vec![None; n];
    for s in cells {
        if s.span == 1
            && let (Some(slot), Some(width)) = (widths.get_mut(s.column), s.width)
        {
            *slot = Some(width);
        }
    }
    let taken: f64 = widths.iter().flatten().sum();
    let open = widths.iter().filter(|w| w.is_none()).count();
    let share = if open > 0 { ((tw - taken).max(0.0) / open as f64).floor() } else { 0.0 };
    (tx, widths.into_iter().map(|w| w.unwrap_or(share)).collect())
}

/// A cell's rect across (its columns) from the table's left.
fn cell_across(c: &CellLink, widths: &[f64], tx: f64) -> (f64, f64) {
    let before: f64 = widths.iter().take(c.column).sum();
    let w: f64 = widths.iter().skip(c.column).take(c.span).sum();
    (tx + before, w)
}

/// A cell's layout rect across inside its rect.
fn inset_cell(v: &Values, x: f64, w: f64, collapse: bool) -> (f64, f64) {
    let (l, r) = (v.decor(MIN_X, w, collapse), v.decor(MAX_X, w, collapse));
    (x + l, (w - l - r).max(0.0))
}

/// A row's cells' rects across: for each cell of `cells` (by the
/// position of its block in `blocks`), its rect's x and width, in an
/// enclosing rect across.
pub(crate) fn row_columns(blocks: &[&NSTextBlock], x: f64, w: f64) -> Vec<(f64, f64)> {
    let links: Vec<Link> = blocks.iter().map(|b| link(b)).collect();
    let Some(first) = links.iter().find_map(|l| l.cell) else { return vec![(x, w); blocks.len()] };
    let tw = {
        let tv = &first.table_values;
        let fits = (w - tv.decor(MIN_X, w, false) - tv.decor(MAX_X, w, false)).max(0.0);
        let set = tv.content_width(w);
        if set > 0.0 { set.min(fits) } else { fits }
    };
    let specs: Vec<CellSpec> = links
        .iter()
        .filter_map(|l| {
            let c = l.cell?;
            let set = l.values.content_width(tw);
            let width = (set > 0.0)
                .then(|| set + l.values.decor(MIN_X, tw, c.collapses) + l.values.decor(MAX_X, tw, c.collapses));
            Some(CellSpec { column: c.column, span: c.span, width })
        })
        .collect();
    let n = specs.iter().map(|s| s.column + s.span).max().unwrap_or(1).max(1);
    let (tx, widths) = columns_of(&first, x, w, n, &specs);
    links.iter().map(|l| l.cell.map_or((x, w), |c| cell_across(&c, &widths, tx))).collect()
}

/// One block's part in a paragraph's layout.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    pub block: Retained<NSTextBlock>,
    pub link: Link,
    /// The layout rect across (container points).
    pub lx: f64,
    pub lw: f64,
    /// The bounds across.
    pub x0: f64,
    pub x1: f64,
    /// The width the block's percentages are of, and the enclosing
    /// rect's (the same but for a cell, whose table's is the enclosing).
    pub basis: f64,
    pub outer: f64,
}

/// Where a paragraph's blocks put it across: each block's layout and
/// bounds, and the paragraph's content x and width.
#[derive(Clone, Debug, Default)]
pub(crate) struct Across {
    pub x: f64,
    pub w: f64,
    pub levels: SmallVec<[Level; 2]>,
}

/// Lay a chain out across from an enclosing rect (`x`, `w`): a table
/// cell takes its rect from `cell` (its level and the table's enclosing
/// rect across).
pub(crate) fn across(
    chain: &[Retained<NSTextBlock>],
    mut x: f64,
    mut w: f64,
    cell: &dyn Fn(usize, f64, f64) -> (f64, f64),
) -> Across {
    let mut levels = SmallVec::new();
    for (k, b) in chain.iter().enumerate() {
        let l = link(b);
        let level = match l.cell {
            None => {
                let (lx, lw) = l.values.across(x, w);
                let (dl, dr) = (l.values.decor(MIN_X, w, false), l.values.decor(MAX_X, w, false));
                Level { block: b.clone(), link: l, lx, lw, x0: lx - dl, x1: lx + lw + dr, basis: w, outer: w }
            }
            Some(c) => {
                let (cx, cw) = cell(k, x, w);
                let (lx, lw) = inset_cell(&l.values, cx, cw, c.collapses);
                Level { block: b.clone(), link: l, lx, lw, x0: cx, x1: cx + cw, basis: cw, outer: w }
            }
        };
        (x, w) = (level.lx, level.lw);
        levels.push(level);
    }
    Across { x, w, levels }
}

/// A block's part of a paragraph's height, for drawing it and answering
/// for its rects: across, and down from the paragraph's flow top.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    pub block: Retained<NSTextBlock>,
    /// The layout rect across, and its top where the block starts.
    pub lx: f64,
    pub lw: f64,
    pub ltop: Option<f64>,
    /// The bounds across, and the part down this paragraph covers.
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    /// Whether the bounds start (end) here, and with collapsed borders.
    pub first: bool,
    pub last: bool,
    pub collapse: bool,
}

/// Where a paragraph among text blocks goes.
#[derive(Clone, Debug, Default)]
pub(crate) struct Place {
    /// The content's left from the container's padding edge, and its width.
    pub left: f32,
    pub width: f32,
    /// From the paragraph's flow top to its top (before its leading
    /// spacing).
    pub pre: f32,
    /// How far it moves the text below down.
    pub advance: f32,
    /// What the text's height leaves out when it is the last paragraph.
    pub tail: f32,
    /// In a table row (whose paragraphs share their flow top), and the
    /// row's last paragraph (the one that moves the text below down).
    pub row: bool,
    pub row_end: bool,
    /// The widest the paragraph's blocks reach, from the padding edge.
    pub reach: f32,
    pub slices: Vec<Slice>,
}

/// A paragraph's lines, as placing it needs them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub lead: f64,
    pub height: f64,
    pub trail: f64,
}

/// Place a paragraph outside tables with chain `chain` (its levels laid
/// out across), after a paragraph with `prev` and before one with `next`.
pub(crate) fn place_flow(
    chain: &[Retained<NSTextBlock>],
    a: &Across,
    prev: &[Retained<NSTextBlock>],
    next: &[Retained<NSTextBlock>],
    m: Metrics,
    padding: f64,
) -> Place {
    let starts: SmallVec<[bool; 4]> = (0..chain.len()).map(|k| !same_through(chain, prev, k)).collect();
    let ends: SmallVec<[bool; 4]> = (0..chain.len()).map(|k| !same_through(chain, next, k)).collect();
    let tops: SmallVec<[f64; 4]> = a.levels.iter().map(|l| l.link.values.decor(MIN_Y, l.basis, false)).collect();
    let bottoms: SmallVec<[f64; 4]> = a.levels.iter().map(|l| l.link.values.decor(MAX_Y, l.basis, false)).collect();
    let pre: f64 = (0..chain.len()).filter(|&k| starts[k]).map(|k| tops[k]).sum();
    let content_bottom = pre + m.lead + m.height;
    let lowest = (0..chain.len())
        .filter(|&k| ends[k])
        .map(|k| bottoms[k])
        .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
    let post = lowest.map_or(0.0, |b| (b - m.trail).max(0.0));
    let advance = content_bottom + m.trail + post;
    let tail = match lowest {
        Some(b) => advance - (content_bottom + b).max(content_bottom),
        None => m.trail,
    };
    let mut ltop = 0.0;
    let slices = a
        .levels
        .iter()
        .enumerate()
        .map(|(k, l)| {
            if starts[k] {
                ltop += tops[k];
            }
            Slice {
                block: l.block.clone(),
                lx: l.lx,
                lw: l.lw,
                ltop: starts[k].then_some(ltop),
                x0: l.x0,
                x1: l.x1,
                y0: if starts[k] { pre - tops[k] } else { 0.0 },
                y1: if ends[k] { content_bottom + bottoms[k] } else { advance },
                first: starts[k],
                last: ends[k],
                collapse: false,
            }
        })
        .collect();
    Place {
        left: (a.x - padding) as f32,
        width: a.w as f32,
        pre: pre as f32,
        advance: advance as f32,
        tail: tail as f32,
        row: false,
        row_end: false,
        reach: reach(a, padding),
        slices,
    }
}

/// The widest a paragraph's blocks reach, as a line's reach counts: from
/// the padding edge, less the padding after it.
fn reach(a: &Across, padding: f64) -> f32 {
    a.levels.iter().map(|l| l.x1 - 2.0 * padding).fold(0.0, f64::max) as f32
}

/// A paragraph of a table row, for placing the row.
pub(crate) struct RowMember<'a> {
    pub chain: &'a [Retained<NSTextBlock>],
    pub across: &'a Across,
    pub metrics: Metrics,
}

/// Place a table row's paragraphs: `members` in text order, after a
/// paragraph with `prev` and before one with `next`. The cells are at
/// level `k` of each chain.
pub(crate) fn place_row(
    members: &[RowMember<'_>],
    k: usize,
    prev: &[Retained<NSTextBlock>],
    next: &[Retained<NSTextBlock>],
    padding: f64,
) -> Vec<Place> {
    let Some(first) = members.first() else { return Vec::new() };
    let last = &members[members.len() - 1];
    let cell0 = first.across.levels[k].link.cell;
    let collapse = cell0.is_some_and(|c| c.collapses);
    // Before the row: the enclosing blocks that start with it, and the
    // table's top when the row is its first.
    let outer_tops: SmallVec<[f64; 4]> =
        first.across.levels[..k].iter().map(|l| l.link.values.decor(MIN_Y, l.basis, false)).collect();
    let starts: SmallVec<[bool; 4]> = (0..k).map(|j| !same_through(first.chain, prev, j)).collect();
    let mut row_pre: f64 = (0..k).filter(|&j| starts[j]).map(|j| outer_tops[j]).sum();
    let table_starts = !same_table(first.chain, prev).0;
    if table_starts && let Some(c) = cell0 {
        let outer = first.across.levels[k].outer;
        row_pre += c.table_values.decor(MIN_Y, outer, false) + if collapse { 0.5 } else { 0.0 };
    }
    // Each cell's paragraphs, stacked in it.
    let mut places: Vec<Place> = Vec::with_capacity(members.len());
    let mut row_bottom = row_pre;
    let mut cell_first: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < members.len() {
        let cell = &members[i].chain[k];
        let mut j = i;
        while j + 1 < members.len() && std::ptr::eq(&*members[j + 1].chain[k], &**cell) {
            j += 1;
        }
        cell_first.push(i);
        let cl = &members[i].across.levels[k];
        let mut cursor = row_pre + cl.link.values.decor(MIN_Y, cl.basis, collapse);
        let mut content_bottom = cursor;
        for n in i..=j {
            let m = &members[n];
            let inner = &m.across.levels[k + 1..];
            let before = if n > i { members[n - 1].chain } else { &[][..] };
            let after = if n < j { members[n + 1].chain } else { &[][..] };
            let starts: SmallVec<[bool; 4]> =
                (k + 1..m.chain.len()).map(|q| n == i || !same_through(m.chain, before, q)).collect();
            let ends: SmallVec<[bool; 4]> =
                (k + 1..m.chain.len()).map(|q| n == j || !same_through(m.chain, after, q)).collect();
            let tops: SmallVec<[f64; 4]> = inner.iter().map(|l| l.link.values.decor(MIN_Y, l.basis, false)).collect();
            let bottoms: SmallVec<[f64; 4]> =
                inner.iter().map(|l| l.link.values.decor(MAX_Y, l.basis, false)).collect();
            let pre = cursor + (0..inner.len()).filter(|&q| starts[q]).map(|q| tops[q]).sum::<f64>();
            let c_bottom = pre + m.metrics.lead + m.metrics.height;
            let lowest = (0..inner.len()).filter(|&q| ends[q]).map(|q| bottoms[q]).fold(0.0, f64::max);
            let post = (lowest - m.metrics.trail).max(0.0);
            let top_of = cursor;
            cursor = c_bottom + m.metrics.trail + post;
            content_bottom = c_bottom;
            let mut ltop = pre - (0..inner.len()).filter(|&q| starts[q]).map(|q| tops[q]).sum::<f64>();
            let slices = inner
                .iter()
                .enumerate()
                .map(|(q, l)| {
                    if starts[q] {
                        ltop += tops[q];
                    }
                    Slice {
                        block: l.block.clone(),
                        lx: l.lx,
                        lw: l.lw,
                        ltop: starts[q].then_some(ltop),
                        x0: l.x0,
                        x1: l.x1,
                        y0: if starts[q] { pre - tops[q] } else { top_of },
                        y1: if ends[q] { c_bottom + bottoms[q] } else { cursor },
                        first: starts[q],
                        last: ends[q],
                        collapse: false,
                    }
                })
                .collect();
            places.push(Place {
                left: (m.across.x - padding) as f32,
                width: m.across.w as f32,
                pre: pre as f32,
                advance: 0.0,
                tail: 0.0,
                row: true,
                row_end: false,
                reach: reach(m.across, padding),
                slices,
            });
        }
        let bottom = (content_bottom + cl.link.values.decor(MAX_Y, cl.basis, collapse)).max(cursor);
        row_bottom = row_bottom.max(bottom);
        i = j + 1;
    }
    // The cells' bounds run the row's height; the enclosing blocks'
    // slices go with the row's last paragraph.
    for &f in &cell_first {
        let l = &members[f].across.levels[k];
        let mut ltop = row_pre;
        ltop += l.link.values.decor(MIN_Y, l.basis, collapse);
        places[f].slices.insert(
            0,
            Slice {
                block: l.block.clone(),
                lx: l.lx,
                lw: l.lw,
                ltop: Some(ltop),
                x0: l.x0,
                x1: l.x1,
                y0: row_pre,
                y1: row_bottom,
                first: true,
                last: true,
                collapse,
            },
        );
    }
    let ends: SmallVec<[bool; 4]> = (0..k).map(|j| !same_through(last.chain, next, j)).collect();
    let outer_bottoms: SmallVec<[f64; 4]> =
        last.across.levels[..k].iter().map(|l| l.link.values.decor(MAX_Y, l.basis, false)).collect();
    let post = (0..k).filter(|&j| ends[j]).map(|j| outer_bottoms[j]).fold(0.0, f64::max);
    let advance = row_bottom + post;
    let first_pre = f64::from(places[0].pre);
    let mut ltop = 0.0;
    let outer: Vec<Slice> = first.across.levels[..k]
        .iter()
        .enumerate()
        .map(|(j, l)| {
            if starts[j] {
                ltop += outer_tops[j];
            }
            Slice {
                block: l.block.clone(),
                lx: l.lx,
                lw: l.lw,
                ltop: starts[j].then_some(ltop),
                x0: l.x0,
                x1: l.x1,
                y0: if starts[j] { first_pre - outer_tops[j] } else { 0.0 },
                y1: if ends[j] { row_bottom + outer_bottoms[j] } else { advance },
                first: starts[j],
                last: ends[j],
                collapse: false,
            }
        })
        .collect();
    let end = places.len() - 1;
    let p = &mut places[end];
    p.row_end = true;
    p.advance = advance as f32;
    p.tail = 0.0;
    // Enclosing blocks first: they're drawn under the row.
    let own = std::mem::take(&mut p.slices);
    p.slices = outer;
    p.slices.extend(own);
    places
}

// Drawing.

/// Draw a block's box: its background over `frame` (its bounds) and its
/// borders inside its margins; the top and bottom only where the box
/// starts and ends (`top`, `bottom`). A table cell's collapsed borders are
/// centered on its edges, so its neighbours' fall on the same line.
pub(crate) fn draw_box(b: &NSTextBlock, frame: NSRect, basis: f64, top: bool, bottom: bool, collapse: bool) {
    let imp = base(b);
    let v = imp.values();
    let background = imp.ivars().background.borrow().clone();
    let borders = imp.ivars().borders.borrow().clone();
    if let Some(bg) = &background {
        fill(frame, bg);
    }
    let margin = |e: usize| v.widths[2][e].at(basis);
    let border = |e: usize| v.widths[BORDER][e].at(basis).max(0.0);
    let (x0, x1) = (frame.origin.x + margin(MIN_X), frame.origin.x + frame.size.width - margin(MAX_X));
    let y0 = frame.origin.y + if top { margin(MIN_Y) } else { 0.0 };
    let y1 = frame.origin.y + frame.size.height - if bottom { margin(MAX_Y) } else { 0.0 };
    let (l, r, t, bm) = (border(MIN_X), border(MAX_X), border(MIN_Y), border(MAX_Y));
    let rect = |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    let edges = if collapse {
        [
            rect(x0 - l / 2.0, y0, l, y1 - y0),
            rect(x0, y0 - t / 2.0, x1 - x0, t),
            rect(x1 - r / 2.0, y0, r, y1 - y0),
            rect(x0, y1 - bm / 2.0, x1 - x0, bm),
        ]
    } else {
        [
            rect(x0, y0, l, y1 - y0),
            rect(x0 + l, y0, x1 - x0 - l - r, t),
            rect(x1 - r, y0, r, y1 - y0),
            rect(x0 + l, y1 - bm, x1 - x0 - l - r, bm),
        ]
    };
    for (e, rect) in edges.into_iter().enumerate() {
        let shown = match e {
            MIN_Y => top,
            MAX_Y => bottom,
            _ => true,
        };
        if let Some(c) = &borders[e]
            && shown
            && rect.size.width > 0.0
            && rect.size.height > 0.0
        {
            fill(rect, c);
        }
    }
}

/// Fill `r`: thinner than a point, as a shape, so a hairline shows
/// (antialiased) whatever the pixels' size.
fn fill(r: NSRect, color: &NSColor) {
    let color = crate::color::resolve(color);
    crate::graphics::with_recorder(|rec| {
        let rect = rec.xf.rect(r);
        if rect.intersect(&rec.clip).is_empty() {
            return;
        }
        if r.size.width < 1.0 || r.size.height < 1.0 {
            let Some(shape) = tiny_skia::Rect::from_ltrb(rect.x0, rect.y0, rect.x1, rect.y1) else { return };
            rec.ops.push(Op::FillPath {
                path: std::sync::Arc::new(tiny_skia::PathBuilder::from_rect(shape)),
                even_odd: false,
                paint: crate::protocol::Paint::Solid(color),
                // Layer points already.
                draw: crate::protocol::Draw {
                    xf: tiny_skia::Transform::identity(),
                    blend: crate::protocol::Blend::SourceOver,
                    aa: true,
                    clip: rec.clip,
                    mask: None,
                    shadow: None,
                },
            });
        } else {
            rec.ops.push(Op::Fill { rect: rect.intersect(&rec.clip), color });
        }
    });
}

/// A block's box being drawn: slices of consecutive paragraphs joined.
pub(crate) struct Open {
    block: Retained<NSTextBlock>,
    rect: NSRect,
    basis: f64,
    top: bool,
    bottom: bool,
    collapse: bool,
}

/// Join paragraphs' slices (with each paragraph's flow top, in text
/// order) into boxes, and draw them, enclosing blocks first; `origin` is
/// the container's in the view.
pub(crate) fn draw_slices(paras: &[(f64, &[Slice])], origin: NSPoint) {
    let mut open: Vec<Open> = Vec::new();
    for &(top, slices) in paras {
        for s in slices {
            let (y0, y1) = (top + s.y0, top + s.y1);
            let joined = open.iter_mut().rev().find(|o| std::ptr::eq(&*o.block, &*s.block) && !o.bottom && !s.first);
            match joined {
                Some(o) => {
                    let bottom = (o.rect.origin.y + o.rect.size.height).max(y1);
                    o.rect.size.height = bottom - o.rect.origin.y;
                    o.bottom = s.last;
                }
                None => open.push(Open {
                    block: s.block.clone(),
                    rect: NSRect::new(NSPoint::new(s.x0, y0), NSSize::new(s.x1 - s.x0, (y1 - y0).max(0.0))),
                    basis: (s.x1 - s.x0).max(0.0),
                    top: s.first,
                    bottom: s.last,
                    collapse: s.collapse,
                }),
            }
        }
    }
    for o in open {
        let r = NSRect::new(NSPoint::new(o.rect.origin.x + origin.x, o.rect.origin.y + origin.y), o.rect.size);
        draw_box(&o.block, r, o.basis, o.top, o.bottom, o.collapse);
    }
}

// NSAttributedString's ranges of blocks and tables.

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepAttributedStringBlocks"]
    struct AttributedStringBlocks;

    impl AttributedStringBlocks {
        #[unsafe(method(rangeOfTextBlock:atIndex:))]
        fn range_of_text_block(&self, block: &NSTextBlock, location: NSUInteger) -> NSRange {
            range_where(self.as_string(), location, |chain| chain.iter().any(|b| std::ptr::eq(&**b, block)))
        }

        #[unsafe(method(rangeOfTextTable:atIndex:))]
        fn range_of_text_table(&self, table: &NSTextTable, location: NSUInteger) -> NSRange {
            range_where(self.as_string(), location, |chain| {
                chain.iter().any(|b| link(b).cell.is_some_and(|c| std::ptr::eq(c.table, table)))
            })
        }
    }
);

impl AttributedStringBlocks {
    fn as_string(&self) -> &NSAttributedString {
        // SAFETY: installed on NSAttributedString, so the receiver is one.
        unsafe { &*(self as *const Self).cast::<NSAttributedString>() }
    }
}

sidestep_runtime::category!("NSAttributedString"(SidestepTextBlocks), |category| {
    // SAFETY: the helper's methods treat their receiver as an attributed
    // string.
    unsafe { category.add_methods_of(AttributedStringBlocks::class()) };
});

/// The text blocks of the paragraph starting at `at`.
fn chain_at(s: &NSAttributedString, at: usize) -> Option<Chain> {
    // SAFETY: a valid index, and the key is a constant string AppKit
    // exports.
    let value: Option<Retained<AnyObject>> = unsafe {
        msg_send![s, attribute: NSParagraphStyleAttributeName, atIndex: at, effectiveRange: std::ptr::null_mut::<NSRange>()]
    };
    let style = value?.downcast::<NSParagraphStyle>().ok()?;
    crate::paragraph::blocks_of(&style)
}

/// The paragraphs around `location` whose blocks `has`, joined.
fn range_where(s: &NSAttributedString, location: usize, has: impl Fn(&[Retained<NSTextBlock>]) -> bool) -> NSRange {
    let text: Retained<NSString> = s.string();
    let len = text.length();
    let none = NSRange::new(NSNotFound as usize, 0);
    if location >= len {
        return none;
    }
    let para = |i: usize| text.paragraphRangeForRange(NSRange::new(i, 0));
    let holds = |r: NSRange| chain_at(s, r.location).is_some_and(|c| has(&c));
    let here = para(location);
    if !holds(here) {
        return none;
    }
    let (mut start, mut end) = (here.location, here.location + here.length);
    while start > 0 {
        let p = para(start - 1);
        if !holds(p) {
            break;
        }
        start = p.location;
    }
    while end < len {
        let p = para(end);
        if !holds(p) {
            break;
        }
        end = p.location + p.length;
    }
    NSRange::new(start, end - start)
}
