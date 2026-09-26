//! `NSTableColumn`: a column's identifier, width and limits, title and
//! flags. Its table keeps it; the column points back weakly, as AppKit's
//! does, so a column that outlives its table has none.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSTableColumn, NSTableColumnResizingOptions, NSTableView};
use objc2_foundation::{NSObjectProtocol, NSString};

pub(crate) struct ColumnIvars {
    identifier: RefCell<Retained<NSString>>,
    table: RefCell<Option<Weak<NSTableView>>>,
    width: Cell<f64>,
    min: Cell<f64>,
    max: Cell<f64>,
    title: RefCell<Retained<NSString>>,
    editable: Cell<bool>,
    hidden: Cell<bool>,
    resizing: Cell<NSTableColumnResizingOptions>,
    tool_tip: RefCell<Option<Retained<NSString>>>,
    sort_prototype: RefCell<Option<Retained<AnyObject>>>,
}

impl ColumnIvars {
    fn new(identifier: Retained<NSString>) -> Self {
        ColumnIvars {
            identifier: RefCell::new(identifier),
            table: RefCell::new(None),
            width: Cell::new(100.0),
            min: Cell::new(10.0),
            max: Cell::new(f32::MAX as f64),
            title: RefCell::new(NSString::from_str("Field")),
            editable: Cell::new(true),
            hidden: Cell::new(false),
            resizing: Cell::new(
                NSTableColumnResizingOptions::AutoresizingMask | NSTableColumnResizingOptions::UserResizingMask,
            ),
            tool_tip: RefCell::new(None),
            sort_prototype: RefCell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTableColumn"]
    #[ivars = ColumnIvars]
    pub(crate) struct NSTableColumnImpl;

    impl NSTableColumnImpl {
        #[unsafe(method_id(initWithIdentifier:))]
        fn init_with_identifier(this: Allocated<Self>, identifier: &NSString) -> Retained<Self> {
            let this = this.set_ivars(ColumnIvars::new(objc2_foundation::NSCopying::copy(identifier)));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ColumnIvars::new(NSString::new()));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Retained<NSString> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: &NSString) {
            let old = self.ivars().identifier.replace(objc2_foundation::NSCopying::copy(identifier));
            drop(old);
        }

        #[unsafe(method_id(tableView))]
        fn table_view(&self) -> Option<Retained<NSTableView>> {
            self.table()
        }

        #[unsafe(method(setTableView:))]
        fn set_table_view(&self, table: Option<&NSTableView>) {
            link(as_column(self), table);
        }

        #[unsafe(method(width))]
        fn width(&self) -> f64 {
            self.ivars().width.get()
        }

        #[unsafe(method(setWidth:))]
        fn set_width(&self, width: f64) {
            let width = width.max(self.ivars().min.get()).min(self.ivars().max.get());
            let old = self.ivars().width.replace(width);
            if old != width {
                self.resized(old);
            }
        }

        #[unsafe(method(minWidth))]
        fn min_width(&self) -> f64 {
            self.ivars().min.get()
        }

        #[unsafe(method(setMinWidth:))]
        fn set_min_width(&self, min: f64) {
            self.ivars().min.set(min);
            let old = self.ivars().width.get();
            if old < min {
                self.ivars().width.set(min);
                self.resized(old);
            }
        }

        #[unsafe(method(maxWidth))]
        fn max_width(&self) -> f64 {
            self.ivars().max.get()
        }

        #[unsafe(method(setMaxWidth:))]
        fn set_max_width(&self, max: f64) {
            self.ivars().max.set(max);
            let old = self.ivars().width.get();
            if old > max {
                self.ivars().width.set(max);
                self.resized(old);
            }
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            let old = self.ivars().title.replace(objc2_foundation::NSCopying::copy(title));
            drop(old);
        }

        #[unsafe(method(isEditable))]
        fn is_editable(&self) -> bool {
            self.ivars().editable.get()
        }

        #[unsafe(method(setEditable:))]
        fn set_editable(&self, flag: bool) {
            self.ivars().editable.set(flag);
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            self.ivars().hidden.get()
        }

        #[unsafe(method(setHidden:))]
        fn set_hidden(&self, flag: bool) {
            if self.ivars().hidden.replace(flag) != flag {
                self.changed();
            }
        }

        #[unsafe(method(resizingMask))]
        fn resizing_mask(&self) -> NSTableColumnResizingOptions {
            self.ivars().resizing.get()
        }

        #[unsafe(method(setResizingMask:))]
        fn set_resizing_mask(&self, mask: NSTableColumnResizingOptions) {
            self.ivars().resizing.set(mask);
        }

        #[unsafe(method(isResizable))]
        fn is_resizable(&self) -> bool {
            self.ivars().resizing.get().contains(NSTableColumnResizingOptions::UserResizingMask)
        }

        #[unsafe(method(setResizable:))]
        fn set_resizable(&self, flag: bool) {
            let mask = if flag {
                NSTableColumnResizingOptions::AutoresizingMask | NSTableColumnResizingOptions::UserResizingMask
            } else {
                NSTableColumnResizingOptions::NoResizing
            };
            self.ivars().resizing.set(mask);
        }

        #[unsafe(method_id(headerToolTip))]
        fn header_tool_tip(&self) -> Option<Retained<NSString>> {
            self.ivars().tool_tip.borrow().clone()
        }

        #[unsafe(method(setHeaderToolTip:))]
        fn set_header_tool_tip(&self, tip: Option<&NSString>) {
            let old = self.ivars().tool_tip.replace(tip.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        #[unsafe(method_id(sortDescriptorPrototype))]
        fn sort_descriptor_prototype(&self) -> Option<Retained<AnyObject>> {
            self.ivars().sort_prototype.borrow().clone()
        }

        #[unsafe(method(setSortDescriptorPrototype:))]
        fn set_sort_descriptor_prototype(&self, prototype: Option<&AnyObject>) {
            let old = self.ivars().sort_prototype.replace(prototype.map(|p| p.retain()));
            drop(old);
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {}
    }

    unsafe impl NSObjectProtocol for NSTableColumnImpl {}
);

impl NSTableColumnImpl {
    fn table(&self) -> Option<Retained<NSTableView>> {
        self.ivars().table.borrow().as_ref().and_then(Weak::load)
    }

    /// Visibility changed: the table lays out again.
    fn changed(&self) {
        if let Some(table) = self.table() {
            super::columns_changed(&table);
        }
    }

    /// The width changed from `old`: the table lays out again and says so.
    fn resized(&self, old: f64) {
        if let Some(table) = self.table() {
            super::columns_changed(&table);
            super::column_resized(&table, as_column(self), old);
        }
    }
}

fn as_column(column: &NSTableColumnImpl) -> &NSTableColumn {
    // SAFETY: NSTableColumn is NSTableColumnImpl's class.
    unsafe { &*(column as *const NSTableColumnImpl).cast::<NSTableColumn>() }
}

pub(crate) fn imp(column: &NSTableColumn) -> &NSTableColumnImpl {
    // SAFETY: NSTableColumn is NSTableColumnImpl's class; subclasses share
    // its layout.
    unsafe { &*(column as *const NSTableColumn).cast::<NSTableColumnImpl>() }
}

/// The room a column takes: none when hidden.
pub(crate) fn span(column: &NSTableColumn) -> f64 {
    let ivars = imp(column).ivars();
    if ivars.hidden.get() { 0.0 } else { ivars.width.get() }
}

pub(crate) fn is_hidden(column: &NSTableColumn) -> bool {
    imp(column).ivars().hidden.get()
}

pub(crate) fn link(column: &NSTableColumn, table: Option<&NSTableView>) {
    let old = imp(column).ivars().table.replace(table.map(Weak::new));
    drop(old);
}
