//! `NSText` and `NSTextView`: showing and editing a text storage's text,
//! laid out by its layout manager in the view's text container.
//!
//! A text view is flipped; its text container sits at the container
//! origin (the inset from the top left), and container coordinates are the
//! layout manager's. `NSText` is the abstract superclass the bindings
//! declare the basic methods on; every text view is an `NSTextView`.
//!
//! - **The network.** `initWithFrame:textContainer:` takes an existing
//!   storage, layout manager and container; `initWithFrame:` builds one
//!   (TextKit 1: Sidestep has no TextKit 2 text view, and
//!   `initUsingTextLayoutManager:` builds TextKit 1 too). The view keeps
//!   its storage and container alive.
//! - **Edits** go through `edit`: one transaction per user edit, with the
//!   delegate calls, notifications and undo AppKit makes. `setString:` and
//!   `replaceCharactersInRange:withString:` are programmatic: no delegate
//!   questions, no undo.
//! - **Selection** is a range (or several), an affinity and a granularity.
//!   A change asks the delegate, posts
//!   `NSTextViewDidChangeSelectionNotification`, takes the typing
//!   attributes from the text before the insertion point, and redraws only
//!   what the old and new selections cover.
//! - **Size.** A vertically (or horizontally) resizable view grows to its
//!   text's height (width) plus the inset, within its minimum and maximum
//!   size, once per transaction; a container tracking the view's width
//!   follows the view's frame.
//! - **Drawing** records the background, the selection, the layout
//!   manager's backgrounds and glyphs for the lines in the dirty rect, and
//!   the caret. The caret blinks on a timer (GTK's rhythm: 1.2 s a cycle,
//!   solid after each edit or move and after 10 s without either), only
//!   while the view is first responder in the key window, and each blink
//!   redraws only the caret's rect.
//! - **Mouse**: a click places the insertion point, a double click selects
//!   a word and a triple click a paragraph, Shift extends, and dragging
//!   extends by the granularity the click started with, scrolling when the
//!   pointer leaves the visible rect. A click on a link goes to
//!   `clickedOnLink:atIndex:`.
//!
//! Editing commands (`commands`) and the input client methods
//! (`input_client`) are link-time categories on the class.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSClipView, NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSLayoutManager, NSParagraphStyle, NSResponder, NSScrollView,
    NSSelectionAffinity, NSSelectionGranularity, NSText, NSTextAlignment, NSTextContainer, NSTextStorage, NSTextView,
    NSView, NSWritingDirection,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSDate, NSDictionary, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize,
    NSString, NSTimer, NSUndoManager, NSValue,
};

use super::attrs::Dict;
use super::edit::{self, Kind, Typing, UndoAs};
use super::layout_manager::NSLayoutManagerImpl;
use super::notify::{self, Note};
use super::selection;
use crate::funnel::Funnel;

sidestep_runtime::static_class!(pub(crate) NSTEXT, NSTEXT_META = "NSText", || {
    let _ = NSTextImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTVIEW, NSTEXTVIEW_META = "NSTextView", || {
    let class = NSTextViewImpl::class();
    SELECT.capture(class, sel!(setSelectedRanges:affinity:stillSelecting:));
    ORIGIN.capture(class, sel!(textContainerOrigin));
    INSERTION_INDEX.capture(class, sel!(characterIndexForInsertionAtPoint:));
    WRITE_TYPES.capture(class, sel!(writeSelectionToPasteboard:types:));
    WRITE_TYPE.capture(class, sel!(writeSelectionToPasteboard:type:));
    UNMARK.capture(class, sel!(unmarkText));
});

// The text view's funnels (see `crate::funnel`): the methods AppKit's text
// view reaches by message, so a subclass's override sees every call.

/// Every selection change, the view's own included.
static SELECT: Funnel = Funnel::new();
/// Drawing and hit testing place the text container here.
static ORIGIN: Funnel = Funnel::new();
/// Where a drop would insert.
pub(crate) static INSERTION_INDEX: Funnel = Funnel::new();
/// `copy:` and `cut:` write the selection through these.
pub(crate) static WRITE_TYPES: Funnel = Funnel::new();
pub(crate) static WRITE_TYPE: Funnel = Funnel::new();
/// A click ends an input method's composition with it.
pub(crate) static UNMARK: Funnel = Funnel::new();

/// A container as tall (or wide) as text gets.
const HUGE: f64 = 10_000_000.0;

/// Background layout sizes a view to its text at most this often (a
/// resize draws the whole view again).
const IDLE_RESIZE: Duration = Duration::from_millis(250);

/// The caret's blink: on and off this long each, stopping (on) after this
/// long without an edit or a move.
const BLINK: Duration = Duration::from_millis(600);
const BLINK_IDLE: Duration = Duration::from_secs(10);

/// `NSNotFound`.
pub(crate) const NOT_FOUND: usize = isize::MAX as usize;

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSText"]
    pub(crate) struct NSTextImpl;

    impl NSTextImpl {}
);

/// Yes-or-no settings, as bits.
mod flag {
    pub const EDITABLE: u64 = 1 << 0;
    pub const SELECTABLE: u64 = 1 << 1;
    pub const RICH: u64 = 1 << 2;
    pub const FIELD_EDITOR: u64 = 1 << 3;
    pub const DRAWS_BACKGROUND: u64 = 1 << 4;
    pub const ALLOWS_UNDO: u64 = 1 << 5;
    pub const IMPORTS_GRAPHICS: u64 = 1 << 6;
    pub const USES_FONT_PANEL: u64 = 1 << 7;
    pub const USES_RULER: u64 = 1 << 8;
    pub const RULER_VISIBLE: u64 = 1 << 9;
    pub const H_RESIZABLE: u64 = 1 << 10;
    pub const V_RESIZABLE: u64 = 1 << 11;
    pub const SMART_INSERT: u64 = 1 << 12;
    pub const SPELLING: u64 = 1 << 13;
    pub const GRAMMAR: u64 = 1 << 14;
    pub const QUOTES: u64 = 1 << 15;
    pub const LINKS: u64 = 1 << 16;
    pub const DATA: u64 = 1 << 17;
    pub const DASHES: u64 = 1 << 18;
    pub const REPLACEMENT: u64 = 1 << 19;
    pub const CORRECTION: u64 = 1 << 20;
    pub const FIND_PANEL: u64 = 1 << 21;
    pub const FIND_BAR: u64 = 1 << 22;
    pub const INCREMENTAL_SEARCH: u64 = 1 << 23;
    pub const IMAGE_EDITING: u64 = 1 << 24;
    pub const NO_LINK_TOOLTIPS: u64 = 1 << 25;
    pub const GLYPH_INFO: u64 = 1 << 26;
    pub const INSPECTOR_BAR: u64 = 1 << 27;
    pub const BACKGROUND_COLOR_CHANGE: u64 = 1 << 28;
    pub const ROLLOVER: u64 = 1 << 29;
    pub const COMPLETION: u64 = 1 << 30;
    pub const NO_CHARACTER_PICKER: u64 = 1 << 31;
    pub const ADAPTIVE_COLORS: u64 = 1 << 32;
    pub const STILL_SELECTING: u64 = 1 << 33;
    /// A secure field's editor: bullets shown, nothing copied.
    pub const SECURE: u64 = 1 << 34;
    /// Takes dropped text: a new view does, and `updateDragTypeRegistration`
    /// keeps it with the editable flag (see `drop`).
    pub const DROP_TARGET: u64 = 1 << 35;
}

/// What kind of selection change is on its way through the funnel.
#[derive(Clone, Copy)]
enum Funneled {
    /// A program's, through a selection setter.
    Program,
    /// The view's own (an edit, a command, a click).
    Own { from_edit: bool },
}

/// The caret's blinking.
#[derive(Default)]
struct Caret {
    on: bool,
    timer: Option<Retained<NSTimer>>,
    last_activity: Option<Instant>,
}

pub(crate) struct Ivars {
    storage: RefCell<Option<Retained<NSTextStorage>>>,
    container: RefCell<Option<Retained<NSTextContainer>>>,
    delegate: RefCell<Weak<AnyObject>>,
    inset: Cell<NSSize>,
    selection: Cell<NSRange>,
    ranges: RefCell<Vec<NSRange>>,
    affinity: Cell<NSSelectionAffinity>,
    granularity: Cell<NSSelectionGranularity>,
    /// The end of the selection that stays put while Shift extends it.
    anchor: Cell<Option<usize>>,
    /// Where vertical moves aim, across lines of different lengths.
    goal_x: Cell<Option<f64>>,
    flags: Cell<u64>,
    min_size: Cell<NSSize>,
    max_size: Cell<NSSize>,
    background: RefCell<Option<Retained<NSColor>>>,
    insertion_color: RefCell<Option<Retained<NSColor>>>,
    typing: RefCell<Option<Retained<Dict>>>,
    selected_attrs: RefCell<Option<Retained<Dict>>>,
    marked_attrs: RefCell<Option<Retained<Dict>>>,
    link_attrs: RefCell<Option<Retained<Dict>>>,
    default_style: RefCell<Option<Retained<NSParagraphStyle>>>,
    /// Between textDidBeginEditing and textDidEndEditing.
    editing: Cell<bool>,
    /// Transactions open, and whether layout changed during them (what it
    /// changed is drawn again, and the frame sized, when the last ends).
    transactions: Cell<usize>,
    needs_size: Cell<bool>,
    /// When background layout last sized the view, and whether it owes a
    /// sizing it put off.
    sized_at: Cell<Option<Instant>>,
    size_owed: Cell<bool>,
    /// The marked text's range, while an input method composes.
    pub(crate) marked: Cell<Option<NSRange>>,
    coalescing: RefCell<Option<Rc<RefCell<Typing>>>>,
    /// How the next `shouldChangeTextInRange:…` registers undo.
    undo_as: Cell<UndoAs>,
    caret: RefCell<Caret>,
    /// Other settings kept for programs that read them back.
    checking_types: Cell<u64>,
    writing_tools: Cell<isize>,
    prediction: Cell<isize>,
    math_completion: Cell<isize>,
    orientation: Cell<isize>,
    locales: RefCell<Option<Retained<AnyObject>>>,
    highlight_attrs: RefCell<Option<Retained<Dict>>>,
    result_options: Cell<usize>,
    /// A selection change on its way through an overridden
    /// `setSelectedRanges:affinity:stillSelecting:` (see `funnel`).
    funneled: Cell<Option<Funneled>>,
}

impl Ivars {
    fn new(frame: NSRect) -> Ivars {
        use flag::*;
        Ivars {
            storage: RefCell::new(None),
            container: RefCell::new(None),
            delegate: RefCell::new(Weak::default()),
            inset: Cell::new(NSSize::ZERO),
            selection: Cell::new(NSRange::new(0, 0)),
            ranges: RefCell::new(vec![NSRange::new(0, 0)]),
            affinity: Cell::new(NSSelectionAffinity::Upstream),
            granularity: Cell::new(NSSelectionGranularity::SelectByCharacter),
            anchor: Cell::new(None),
            goal_x: Cell::new(None),
            flags: Cell::new(
                EDITABLE | SELECTABLE | RICH | DRAWS_BACKGROUND | USES_FONT_PANEL | FIND_PANEL | DROP_TARGET,
            ),
            min_size: Cell::new(frame.size),
            max_size: Cell::new(frame.size),
            background: RefCell::new(None),
            insertion_color: RefCell::new(None),
            typing: RefCell::new(None),
            selected_attrs: RefCell::new(None),
            marked_attrs: RefCell::new(None),
            link_attrs: RefCell::new(None),
            default_style: RefCell::new(None),
            editing: Cell::new(false),
            transactions: Cell::new(0),
            needs_size: Cell::new(false),
            sized_at: Cell::new(None),
            size_owed: Cell::new(false),
            marked: Cell::new(None),
            coalescing: RefCell::new(None),
            undo_as: Cell::new(UndoAs::Program),
            caret: RefCell::new(Caret::default()),
            checking_types: Cell::new(0),
            writing_tools: Cell::new(0),
            prediction: Cell::new(0),
            math_completion: Cell::new(0),
            orientation: Cell::new(0),
            locales: RefCell::new(None),
            highlight_attrs: RefCell::new(None),
            result_options: Cell::new(0),
            funneled: Cell::new(None),
        }
    }
}

impl Drop for Ivars {
    fn drop(&mut self) {
        // A view that goes while its caret blinks stops the timer, which
        // would otherwise fire for good.
        if let Some(t) = self.caret.get_mut().timer.take() {
            t.invalidate();
        }
    }
}

define_class!(
    #[unsafe(super(NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextView"]
    #[ivars = Ivars]
    pub(crate) struct NSTextViewImpl;

    impl NSTextViewImpl {
        // Making one.

        #[unsafe(method_id(initWithFrame:textContainer:))]
        fn init_with_frame_text_container(
            this: Allocated<Self>,
            frame: NSRect,
            container: Option<&NSTextContainer>,
        ) -> Retained<Self> {
            let this = this.set_ivars(Ivars::new(frame));
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            match container {
                Some(c) => this.attach(c),
                None => this.build_network(frame),
            }
            this
        }

        /// A view with a network of its own, which grows down with its
        /// text, as AppKit's does (a view given a container doesn't).
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            // SAFETY: the designated initializer, building a network.
            let this: Retained<Self> =
                unsafe { msg_send![this, initWithFrame: frame, textContainer: None::<&NSTextContainer>] };
            this.set(flag::V_RESIZABLE, true);
            this.ivars().max_size.set(NSSize::new(frame.size.width, HUGE));
            this
        }

        #[unsafe(method_id(initUsingTextLayoutManager:))]
        fn init_using_text_layout_manager(this: Allocated<Self>, _flag: bool) -> Retained<Self> {
            // SAFETY: the initializer building a network, as this one does.
            unsafe { msg_send![this, initWithFrame: NSRect::ZERO] }
        }

        #[unsafe(method_id(textViewUsingTextLayoutManager:))]
        fn text_view_using_text_layout_manager(_flag: bool) -> Retained<NSTextView> {
            new_text_view(main_thread(), NSRect::ZERO)
        }

        #[unsafe(method(stronglyReferencesTextStorage))]
        fn strongly_references_text_storage() -> bool {
            true
        }

        #[unsafe(method_id(fieldEditor))]
        fn field_editor() -> Retained<NSTextView> {
            let tv = new_text_view(main_thread(), NSRect::ZERO);
            tv.setFieldEditor(true);
            tv
        }

        #[unsafe(method_id(scrollableTextView))]
        fn scrollable_text_view() -> Retained<NSScrollView> {
            scrollable(main_thread(), false)
        }

        #[unsafe(method_id(scrollableDocumentContentTextView))]
        fn scrollable_document_content_text_view() -> Retained<NSScrollView> {
            scrollable(main_thread(), true)
        }

        #[unsafe(method_id(scrollablePlainDocumentContentTextView))]
        fn scrollable_plain_document_content_text_view() -> Retained<NSScrollView> {
            scrollable(main_thread(), false)
        }

        // The network.

        #[unsafe(method_id(textContainer))]
        fn text_container(&self) -> Option<Retained<NSTextContainer>> {
            self.ivars().container.borrow().clone()
        }

        #[unsafe(method(setTextContainer:))]
        fn set_text_container(&self, c: Option<&NSTextContainer>) {
            *self.ivars().container.borrow_mut() = c.map(|c| c.retain());
            // SAFETY: layoutManager and textStorage take nothing.
            let storage = c.and_then(|c| unsafe { c.layoutManager() }).and_then(|lm| unsafe { lm.textStorage() });
            *self.ivars().storage.borrow_mut() = storage;
            self.size_to_text();
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(replaceTextContainer:))]
        fn replace_text_container(&self, c: &NSTextContainer) {
            let old = self.ivars().container.borrow().clone();
            if let Some(old) = &old {
                // SAFETY: layoutManager takes nothing.
                if let Some(lm) = unsafe { old.layoutManager() } {
                    let containers = lm.textContainers();
                    if let Some(i) = containers.iter().position(|x| std::ptr::eq(&*x, &**old)) {
                        lm.removeTextContainerAtIndex(i);
                        lm.insertTextContainer_atIndex(c, i);
                    }
                }
                old.setTextView(None);
            }
            self.attach(c);
        }

        #[unsafe(method_id(layoutManager))]
        fn layout_manager(&self) -> Option<Retained<NSLayoutManager>> {
            self.manager()
        }

        #[unsafe(method_id(textStorage))]
        fn text_storage(&self) -> Option<Retained<NSTextStorage>> {
            self.storage()
        }

        #[unsafe(method_id(textLayoutManager))]
        fn text_layout_manager(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method_id(textContentStorage))]
        fn text_content_storage(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(textContainerInset))]
        fn text_container_inset(&self) -> NSSize {
            self.ivars().inset.get()
        }

        #[unsafe(method(setTextContainerInset:))]
        fn set_text_container_inset(&self, inset: NSSize) {
            self.ivars().inset.set(inset);
            self.track_frame(self.as_view().frame().size);
            self.size_to_text();
            self.as_view().setNeedsDisplay(true);
        }

        /// Where the text container sits in the view: the inset. Drawing
        /// and hit testing ask for it by message when a subclass overrides
        /// it (see `origin`).
        #[unsafe(method(textContainerOrigin))]
        fn text_container_origin(&self) -> NSPoint {
            self.own_origin()
        }

        /// Nothing to do: the origin isn't kept, it is worked out from the
        /// inset each time it is asked for.
        #[unsafe(method(invalidateTextContainerOrigin))]
        fn invalidate_text_container_origin(&self) {}

        // NSText.

        /// The text storage's own string, as AppKit's: live, and no copy.
        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            match self.storage() {
                Some(s) => s.string(),
                None => NSString::new(),
            }
        }

        #[unsafe(method(setString:))]
        fn set_string(&self, string: &NSString) {
            self.replace_all(string);
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            let Some(storage) = self.storage() else { return };
            self.discard_marked_text();
            self.begin_transaction();
            let m: &NSMutableAttributedString = &storage;
            m.replaceCharactersInRange_withString(range, string);
            let moved = moved_by_edit(self.selection(), range, string.length());
            self.set_selection_internal(moved, true);
            self.end_quiet_transaction();
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *self.ivars().delegate.borrow_mut() = delegate.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method(isEditable))]
        fn is_editable(&self) -> bool {
            self.has(flag::EDITABLE)
        }

        /// The view's drag types follow (`updateDragTypeRegistration`),
        /// when the flag changes, as on macOS.
        #[unsafe(method(setEditable:))]
        fn set_editable(&self, on: bool) {
            let was = self.has(flag::EDITABLE);
            self.set(flag::EDITABLE, on);
            if on {
                self.set(flag::SELECTABLE, true);
            }
            if was != on {
                super::drop::update_registration(self.as_text_view());
            }
        }

        #[unsafe(method(isSelectable))]
        fn is_selectable(&self) -> bool {
            self.has(flag::SELECTABLE)
        }

        /// Not selectable, it isn't editable either, and its drag types
        /// follow if that changed, as on macOS.
        #[unsafe(method(setSelectable:))]
        fn set_selectable(&self, on: bool) {
            self.set(flag::SELECTABLE, on);
            if !on && self.has(flag::EDITABLE) {
                self.set(flag::EDITABLE, false);
                super::drop::update_registration(self.as_text_view());
            }
        }

        #[unsafe(method(isRichText))]
        fn is_rich_text(&self) -> bool {
            self.has(flag::RICH)
        }

        #[unsafe(method(setRichText:))]
        fn set_rich_text(&self, on: bool) {
            self.set(flag::RICH, on);
            if !on {
                self.set(flag::IMPORTS_GRAPHICS, false);
            }
        }

        #[unsafe(method(importsGraphics))]
        fn imports_graphics(&self) -> bool {
            self.has(flag::IMPORTS_GRAPHICS)
        }

        #[unsafe(method(setImportsGraphics:))]
        fn set_imports_graphics(&self, on: bool) {
            self.set(flag::IMPORTS_GRAPHICS, on);
            if on {
                self.set(flag::RICH, true);
            }
        }

        #[unsafe(method(isFieldEditor))]
        fn is_field_editor(&self) -> bool {
            self.has(flag::FIELD_EDITOR)
        }

        #[unsafe(method(setFieldEditor:))]
        fn set_field_editor(&self, on: bool) {
            self.set(flag::FIELD_EDITOR, on);
        }

        #[unsafe(method(usesFontPanel))]
        fn uses_font_panel(&self) -> bool {
            self.has(flag::USES_FONT_PANEL)
        }

        #[unsafe(method(setUsesFontPanel:))]
        fn set_uses_font_panel(&self, on: bool) {
            self.set(flag::USES_FONT_PANEL, on);
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.has(flag::DRAWS_BACKGROUND)
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, on: bool) {
            self.set(flag::DRAWS_BACKGROUND, on);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            let set = self.ivars().background.borrow().clone();
            set.unwrap_or_else(|| semantic_color(sel!(textBackgroundColor), NSColor::whiteColor))
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            *self.ivars().background.borrow_mut() = color.map(|c| c.retain());
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(isRulerVisible))]
        fn is_ruler_visible(&self) -> bool {
            self.has(flag::RULER_VISIBLE)
        }

        #[unsafe(method(setRulerVisible:))]
        fn set_ruler_visible(&self, on: bool) {
            self.set(flag::RULER_VISIBLE, on);
        }

        #[unsafe(method(usesRuler))]
        fn uses_ruler(&self) -> bool {
            self.has(flag::USES_RULER)
        }

        #[unsafe(method(setUsesRuler:))]
        fn set_uses_ruler(&self, on: bool) {
            self.set(flag::USES_RULER, on);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            self.selection()
        }

        /// Through `setSelectedRanges:affinity:stillSelecting:`, upstream
        /// and not still selecting, as AppKit's.
        #[unsafe(method(setSelectedRange:))]
        fn set_selected_range(&self, range: NSRange) {
            self.select(range, NSSelectionAffinity::Upstream, false);
        }

        #[unsafe(method(scrollRangeToVisible:))]
        fn scroll_range_to_visible(&self, range: NSRange) {
            self.scroll_to(range);
        }

        #[unsafe(method_id(font))]
        fn font(&self) -> Option<Retained<NSFont>> {
            let dict = self.typing_attributes_dict();
            // SAFETY: the key is a constant string AppKit exports.
            dict.objectForKey(unsafe { NSFontAttributeName }).and_then(|f| f.downcast::<NSFont>().ok())
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: &NSFont) {
            // SAFETY: as above.
            let key = unsafe { NSFontAttributeName };
            self.apply_to_all(key, font);
        }

        #[unsafe(method(setFont:range:))]
        fn set_font_range(&self, font: &NSFont, range: NSRange) {
            // SAFETY: as above.
            self.apply_to_range(unsafe { NSFontAttributeName }, Some(font), range);
        }

        #[unsafe(method_id(textColor))]
        fn text_color(&self) -> Option<Retained<NSColor>> {
            let dict = self.typing_attributes_dict();
            // SAFETY: as above.
            dict.objectForKey(unsafe { NSForegroundColorAttributeName }).and_then(|c| c.downcast::<NSColor>().ok())
        }

        #[unsafe(method(setTextColor:))]
        fn set_text_color(&self, color: Option<&NSColor>) {
            // SAFETY: as above.
            let key = unsafe { NSForegroundColorAttributeName };
            match color {
                Some(c) => self.apply_to_all(key, c),
                None => {
                    let len = self.text_length();
                    self.apply_to_range(key, None, NSRange::new(0, len));
                }
            }
        }

        #[unsafe(method(setTextColor:range:))]
        fn set_text_color_range(&self, color: Option<&NSColor>, range: NSRange) {
            let value: Option<&AnyObject> = color.map(|c| c.as_ref());
            // SAFETY: as above.
            self.apply_to_range(unsafe { NSForegroundColorAttributeName }, value, range);
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            self.paragraph_style().map_or(NSTextAlignment::Natural, |s| s.alignment())
        }

        #[unsafe(method(setAlignment:))]
        fn set_alignment(&self, alignment: NSTextAlignment) {
            let len = self.text_length();
            self.set_alignment_in(alignment, NSRange::new(0, len));
        }

        #[unsafe(method(setAlignment:range:))]
        fn set_alignment_range(&self, alignment: NSTextAlignment, range: NSRange) {
            self.set_alignment_in(alignment, range);
        }

        #[unsafe(method(baseWritingDirection))]
        fn base_writing_direction(&self) -> NSWritingDirection {
            self.paragraph_style().map_or(NSWritingDirection::Natural, |s| s.baseWritingDirection())
        }

        #[unsafe(method(setBaseWritingDirection:))]
        fn set_base_writing_direction(&self, direction: NSWritingDirection) {
            let len = self.text_length();
            self.set_direction_in(direction, NSRange::new(0, len));
        }

        #[unsafe(method(setBaseWritingDirection:range:))]
        fn set_base_writing_direction_range(&self, direction: NSWritingDirection, range: NSRange) {
            self.set_direction_in(direction, range);
        }

        #[unsafe(method(maxSize))]
        fn max_size(&self) -> NSSize {
            self.ivars().max_size.get()
        }

        #[unsafe(method(setMaxSize:))]
        fn set_max_size(&self, size: NSSize) {
            self.ivars().max_size.set(size);
        }

        #[unsafe(method(minSize))]
        fn min_size(&self) -> NSSize {
            self.ivars().min_size.get()
        }

        #[unsafe(method(setMinSize:))]
        fn set_min_size(&self, size: NSSize) {
            self.ivars().min_size.set(size);
        }

        #[unsafe(method(isHorizontallyResizable))]
        fn is_horizontally_resizable(&self) -> bool {
            self.has(flag::H_RESIZABLE)
        }

        #[unsafe(method(setHorizontallyResizable:))]
        fn set_horizontally_resizable(&self, on: bool) {
            self.set(flag::H_RESIZABLE, on);
        }

        #[unsafe(method(isVerticallyResizable))]
        fn is_vertically_resizable(&self) -> bool {
            self.has(flag::V_RESIZABLE)
        }

        #[unsafe(method(setVerticallyResizable:))]
        fn set_vertically_resizable(&self, on: bool) {
            self.set(flag::V_RESIZABLE, on);
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            self.ivars().needs_size.set(false);
            self.size_to_text();
        }

        #[unsafe(method(setConstrainedFrameSize:))]
        fn set_constrained_frame_size(&self, size: NSSize) {
            let (min, max) = (self.ivars().min_size.get(), self.ivars().max_size.get());
            let current = self.as_view().frame().size;
            let w = if self.has(flag::H_RESIZABLE) { size.width.clamp(min.width, max.width.max(min.width)) } else { current.width };
            let h = if self.has(flag::V_RESIZABLE) { size.height.clamp(min.height, max.height.max(min.height)) } else { current.height };
            if (w, h) != (current.width, current.height) {
                self.as_view().setFrameSize(NSSize::new(w, h));
            }
        }

        // Selection.

        #[unsafe(method_id(selectedRanges))]
        fn selected_ranges(&self) -> Retained<NSArray<NSValue>> {
            let ranges = self.ivars().ranges.borrow().clone();
            let values: Vec<Retained<NSValue>> = ranges.iter().map(|r| value_of(*r)).collect();
            NSArray::from_retained_slice(&values)
        }

        #[unsafe(method(setSelectedRanges:))]
        fn set_selected_ranges(&self, ranges: &NSArray<NSValue>) {
            self.select_ranges(ranges, NSSelectionAffinity::Upstream, false);
        }

        /// The selection funnel: every change of the selection comes here,
        /// by message when a subclass overrides it (see `select` and
        /// `set_selection_internal`).
        #[unsafe(method(setSelectedRanges:affinity:stillSelecting:))]
        fn set_selected_ranges_affinity(&self, ranges: &NSArray<NSValue>, affinity: NSSelectionAffinity, still: bool) {
            let list: Vec<NSRange> = ranges.iter().map(|v| range_of(&v)).collect();
            self.select_now(list, affinity, still);
        }

        #[unsafe(method(setSelectedRange:affinity:stillSelecting:))]
        fn set_selected_range_affinity(&self, range: NSRange, affinity: NSSelectionAffinity, still: bool) {
            self.select(range, affinity, still);
        }

        #[unsafe(method(selectionAffinity))]
        fn selection_affinity(&self) -> NSSelectionAffinity {
            self.ivars().affinity.get()
        }

        #[unsafe(method(selectionGranularity))]
        fn selection_granularity(&self) -> NSSelectionGranularity {
            self.ivars().granularity.get()
        }

        #[unsafe(method(setSelectionGranularity:))]
        fn set_selection_granularity(&self, g: NSSelectionGranularity) {
            self.ivars().granularity.set(g);
        }

        #[unsafe(method(selectionRangeForProposedRange:granularity:))]
        fn selection_range_for_proposed_range(&self, proposed: NSRange, g: NSSelectionGranularity) -> NSRange {
            self.range_for_granularity(proposed, g)
        }

        #[unsafe(method(characterIndexForInsertionAtPoint:))]
        fn character_index_for_insertion_at_point(&self, p: NSPoint) -> usize {
            self.insertion_index_at(p)
        }

        // Typing attributes and styles.

        #[unsafe(method_id(typingAttributes))]
        fn typing_attributes(&self) -> Retained<Dict> {
            self.typing_attributes_dict()
        }

        #[unsafe(method(setTypingAttributes:))]
        fn set_typing_attributes(&self, attrs: &Dict) {
            // SAFETY: -copy of a dictionary is an immutable dictionary.
            let copy: Retained<Dict> = unsafe { msg_send![attrs, copy] };
            *self.ivars().typing.borrow_mut() = Some(copy);
            let delegate = self.delegate_object();
            notify::post(self.as_object(), delegate.as_deref(), Note::ChangeTypingAttributes, None);
        }

        #[unsafe(method_id(selectedTextAttributes))]
        fn selected_text_attributes(&self) -> Retained<Dict> {
            let set = self.ivars().selected_attrs.borrow().clone();
            set.unwrap_or_else(default_selected_attributes)
        }

        #[unsafe(method(setSelectedTextAttributes:))]
        fn set_selected_text_attributes(&self, attrs: &Dict) {
            *self.ivars().selected_attrs.borrow_mut() = Some(attrs.retain());
        }

        #[unsafe(method_id(markedTextAttributes))]
        fn marked_text_attributes(&self) -> Option<Retained<Dict>> {
            let set = self.ivars().marked_attrs.borrow().clone();
            set.or_else(|| Some(default_marked_attributes()))
        }

        #[unsafe(method(setMarkedTextAttributes:))]
        fn set_marked_text_attributes(&self, attrs: Option<&Dict>) {
            *self.ivars().marked_attrs.borrow_mut() = attrs.map(|a| a.retain());
        }

        #[unsafe(method_id(linkTextAttributes))]
        fn link_text_attributes(&self) -> Option<Retained<Dict>> {
            self.ivars().link_attrs.borrow().clone()
        }

        #[unsafe(method(setLinkTextAttributes:))]
        fn set_link_text_attributes(&self, attrs: Option<&Dict>) {
            *self.ivars().link_attrs.borrow_mut() = attrs.map(|a| a.retain());
        }

        #[unsafe(method_id(defaultParagraphStyle))]
        fn default_paragraph_style(&self) -> Option<Retained<NSParagraphStyle>> {
            self.ivars().default_style.borrow().clone()
        }

        #[unsafe(method(setDefaultParagraphStyle:))]
        fn set_default_paragraph_style(&self, style: Option<&NSParagraphStyle>) {
            *self.ivars().default_style.borrow_mut() = style.map(|s| s.retain());
        }

        #[unsafe(method_id(insertionPointColor))]
        fn insertion_point_color(&self) -> Retained<NSColor> {
            let set = self.ivars().insertion_color.borrow().clone();
            set.unwrap_or_else(NSColor::textColor)
        }

        #[unsafe(method(setInsertionPointColor:))]
        fn set_insertion_point_color(&self, color: Option<&NSColor>) {
            *self.ivars().insertion_color.borrow_mut() = color.map(|c| c.retain());
        }

        // The edit transaction's steps, which subclasses may override.

        #[unsafe(method(shouldChangeTextInRange:replacementString:))]
        fn should_change_text_in_range(&self, range: NSRange, string: Option<&NSString>) -> bool {
            self.should_change(range, string)
        }

        #[unsafe(method(shouldChangeTextInRanges:replacementStrings:))]
        fn should_change_text_in_ranges(&self, ranges: &NSArray<NSValue>, strings: Option<&NSArray<NSString>>) -> bool {
            ranges.iter().enumerate().all(|(i, r)| {
                let s = strings.and_then(|s| s.iter().nth(i));
                self.should_change(range_of(&r), s.as_deref())
            })
        }

        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            let delegate = self.delegate_object();
            notify::post(self.as_object(), delegate.as_deref(), Note::Change, None);
            if !edit::is_first_responder(self.as_text_view()) {
                self.end_editing(0);
            }
        }

        #[unsafe(method(rangeForUserTextChange))]
        fn range_for_user_text_change(&self) -> NSRange {
            if self.has(flag::EDITABLE) { self.selection() } else { NSRange::new(NOT_FOUND, 0) }
        }

        #[unsafe(method(rangeForUserCharacterAttributeChange))]
        fn range_for_user_character_attribute_change(&self) -> NSRange {
            if self.has(flag::EDITABLE) && self.has(flag::RICH) { self.selection() } else { NSRange::new(NOT_FOUND, 0) }
        }

        #[unsafe(method(rangeForUserParagraphAttributeChange))]
        fn range_for_user_paragraph_attribute_change(&self) -> NSRange {
            self.paragraph_change_range()
        }

        #[unsafe(method_id(rangesForUserTextChange))]
        fn ranges_for_user_text_change(&self) -> Option<Retained<NSArray<NSValue>>> {
            let r = if self.has(flag::EDITABLE) { Some(self.selection()) } else { None };
            r.map(|r| NSArray::from_retained_slice(&[value_of(r)]))
        }

        #[unsafe(method_id(rangesForUserCharacterAttributeChange))]
        fn ranges_for_user_character_attribute_change(&self) -> Option<Retained<NSArray<NSValue>>> {
            let r = if self.has(flag::EDITABLE) && self.has(flag::RICH) { Some(self.selection()) } else { None };
            r.map(|r| NSArray::from_retained_slice(&[value_of(r)]))
        }

        #[unsafe(method_id(rangesForUserParagraphAttributeChange))]
        fn ranges_for_user_paragraph_attribute_change(&self) -> Option<Retained<NSArray<NSValue>>> {
            let r = self.paragraph_change_range();
            (r.location != NOT_FOUND).then(|| NSArray::from_retained_slice(&[value_of(r)]))
        }

        // Undo.

        #[unsafe(method(allowsUndo))]
        fn allows_undo_value(&self) -> bool {
            self.has(flag::ALLOWS_UNDO)
        }

        #[unsafe(method(setAllowsUndo:))]
        fn set_allows_undo(&self, on: bool) {
            self.set(flag::ALLOWS_UNDO, on);
        }

        #[unsafe(method(breakUndoCoalescing))]
        fn break_undo_coalescing(&self) {
            self.set_coalescing(None);
        }

        #[unsafe(method(isCoalescingUndo))]
        fn is_coalescing_undo(&self) -> bool {
            self.ivars().coalescing.borrow().is_some()
        }

        #[unsafe(method_id(undoManager))]
        fn undo_manager(&self) -> Option<Retained<NSUndoManager>> {
            self.text_undo_manager()
        }

        // Links.

        #[unsafe(method(clickedOnLink:atIndex:))]
        fn clicked_on_link(&self, link: &AnyObject, index: usize) {
            let Some(d) = self.delegate_object() else { return };
            let sel = sel!(textView:clickedOnLink:atIndex:);
            if notify::responds(&d, sel) {
                // SAFETY: the delegate method takes the view, the link and
                // the index, and returns BOOL.
                let _handled: bool =
                    unsafe { msg_send![&*d, textView: self.as_text_view(), clickedOnLink: link, atIndex: index] };
            }
            // Nothing opens links otherwise: there is no workspace yet.
        }

        // Drawing.

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.has(flag::DRAWS_BACKGROUND)
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            self.draw(dirty);
        }

        #[unsafe(method(drawViewBackgroundInRect:))]
        fn draw_view_background_in_rect(&self, rect: NSRect) {
            if self.has(flag::DRAWS_BACKGROUND) {
                // SAFETY: backgroundColor takes nothing and returns a color.
                let color: Retained<NSColor> = unsafe { msg_send![self, backgroundColor] };
                fill(rect, &color);
            }
        }

        #[unsafe(method(shouldDrawInsertionPoint))]
        fn should_draw_insertion_point(&self) -> bool {
            self.caret_wanted()
        }

        #[unsafe(method(drawInsertionPointInRect:color:turnedOn:))]
        fn draw_insertion_point(&self, rect: NSRect, color: &NSColor, on: bool) {
            if on {
                fill(rect, color);
            } else {
                self.as_view().setNeedsDisplayInRect(rect);
            }
        }

        #[unsafe(method(updateInsertionPointStateAndRestartTimer:))]
        fn update_insertion_point_state(&self, restart: bool) {
            self.caret_activity(restart);
        }

        #[unsafe(method(setNeedsDisplayInRect:avoidAdditionalLayout:))]
        fn set_needs_display_avoiding_layout(&self, rect: NSRect, _avoid: bool) {
            self.as_view().setNeedsDisplayInRect(rect);
        }

        // Geometry.

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: NSView's setFrameSize:.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.track_frame(size);
        }

        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: NSRect) {
            // SAFETY: NSView's setFrame:.
            let _: () = unsafe { msg_send![super(self), setFrame: frame] };
            self.track_frame(frame.size);
        }

        /// In a clip view, the view fills what shows at least: its minimum
        /// size follows the clip view's, as AppKit's does.
        #[unsafe(method(resizeWithOldSuperviewSize:))]
        fn resize_with_old_superview_size(&self, old: NSSize) {
            // SAFETY: NSView's resizeWithOldSuperviewSize:.
            let _: () = unsafe { msg_send![super(self), resizeWithOldSuperviewSize: old] };
            self.follow_clip();
        }

        // Responder.

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.has(flag::SELECTABLE)
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            self.caret_activity(true);
            self.redraw_selection();
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let refused = self.ivars().editing.get() && !self.end_editing(0);
            if !refused {
                self.stop_caret();
                self.redraw_selection();
                if self.has(flag::FIELD_EDITOR) {
                    super::field_editor::resigned(self.as_text_view());
                }
            }
            !refused
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let context = self.as_view().inputContext();
            let handled = context.is_some_and(|c| c.handleEvent(event));
            if !handled {
                let events = NSArray::from_slice(&[event]);
                self.as_view().interpretKeyEvents(&events);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.track_mouse(event);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let cursor = objc2_app_kit::NSCursor::IBeamCursor();
            let view = self.as_view();
            view.addCursorRect_cursor(view.visibleRect(), &cursor);
        }

        // Settings kept for programs that read them back.

        #[unsafe(method(smartInsertDeleteEnabled))]
        fn smart_insert_delete_enabled(&self) -> bool {
            self.has(flag::SMART_INSERT)
        }

        #[unsafe(method(setSmartInsertDeleteEnabled:))]
        fn set_smart_insert_delete_enabled(&self, on: bool) {
            self.set(flag::SMART_INSERT, on);
        }

        #[unsafe(method(isContinuousSpellCheckingEnabled))]
        fn is_continuous_spell_checking_enabled(&self) -> bool {
            self.has(flag::SPELLING)
        }

        #[unsafe(method(setContinuousSpellCheckingEnabled:))]
        fn set_continuous_spell_checking_enabled(&self, on: bool) {
            self.set(flag::SPELLING, on);
        }

        #[unsafe(method(isGrammarCheckingEnabled))]
        fn is_grammar_checking_enabled(&self) -> bool {
            self.has(flag::GRAMMAR)
        }

        #[unsafe(method(setGrammarCheckingEnabled:))]
        fn set_grammar_checking_enabled(&self, on: bool) {
            self.set(flag::GRAMMAR, on);
        }

        #[unsafe(method(isAutomaticQuoteSubstitutionEnabled))]
        fn is_automatic_quote_substitution_enabled(&self) -> bool {
            self.has(flag::QUOTES)
        }

        #[unsafe(method(setAutomaticQuoteSubstitutionEnabled:))]
        fn set_automatic_quote_substitution_enabled(&self, on: bool) {
            self.set(flag::QUOTES, on);
        }

        #[unsafe(method(isAutomaticLinkDetectionEnabled))]
        fn is_automatic_link_detection_enabled(&self) -> bool {
            self.has(flag::LINKS)
        }

        #[unsafe(method(setAutomaticLinkDetectionEnabled:))]
        fn set_automatic_link_detection_enabled(&self, on: bool) {
            self.set(flag::LINKS, on);
        }

        #[unsafe(method(isAutomaticDataDetectionEnabled))]
        fn is_automatic_data_detection_enabled(&self) -> bool {
            self.has(flag::DATA)
        }

        #[unsafe(method(setAutomaticDataDetectionEnabled:))]
        fn set_automatic_data_detection_enabled(&self, on: bool) {
            self.set(flag::DATA, on);
        }

        #[unsafe(method(isAutomaticDashSubstitutionEnabled))]
        fn is_automatic_dash_substitution_enabled(&self) -> bool {
            self.has(flag::DASHES)
        }

        #[unsafe(method(setAutomaticDashSubstitutionEnabled:))]
        fn set_automatic_dash_substitution_enabled(&self, on: bool) {
            self.set(flag::DASHES, on);
        }

        #[unsafe(method(isAutomaticTextReplacementEnabled))]
        fn is_automatic_text_replacement_enabled(&self) -> bool {
            self.has(flag::REPLACEMENT)
        }

        #[unsafe(method(setAutomaticTextReplacementEnabled:))]
        fn set_automatic_text_replacement_enabled(&self, on: bool) {
            self.set(flag::REPLACEMENT, on);
        }

        #[unsafe(method(isAutomaticSpellingCorrectionEnabled))]
        fn is_automatic_spelling_correction_enabled(&self) -> bool {
            self.has(flag::CORRECTION)
        }

        #[unsafe(method(setAutomaticSpellingCorrectionEnabled:))]
        fn set_automatic_spelling_correction_enabled(&self, on: bool) {
            self.set(flag::CORRECTION, on);
        }

        #[unsafe(method(isAutomaticTextCompletionEnabled))]
        fn is_automatic_text_completion_enabled(&self) -> bool {
            self.has(flag::COMPLETION)
        }

        #[unsafe(method(setAutomaticTextCompletionEnabled:))]
        fn set_automatic_text_completion_enabled(&self, on: bool) {
            self.set(flag::COMPLETION, on);
        }

        #[unsafe(method(usesFindPanel))]
        fn uses_find_panel(&self) -> bool {
            self.has(flag::FIND_PANEL)
        }

        #[unsafe(method(setUsesFindPanel:))]
        fn set_uses_find_panel(&self, on: bool) {
            self.set(flag::FIND_PANEL, on);
        }

        #[unsafe(method(usesFindBar))]
        fn uses_find_bar(&self) -> bool {
            self.has(flag::FIND_BAR)
        }

        #[unsafe(method(setUsesFindBar:))]
        fn set_uses_find_bar(&self, on: bool) {
            self.set(flag::FIND_BAR, on);
        }

        #[unsafe(method(isIncrementalSearchingEnabled))]
        fn is_incremental_searching_enabled(&self) -> bool {
            self.has(flag::INCREMENTAL_SEARCH)
        }

        #[unsafe(method(setIncrementalSearchingEnabled:))]
        fn set_incremental_searching_enabled(&self, on: bool) {
            self.set(flag::INCREMENTAL_SEARCH, on);
        }

        #[unsafe(method(allowsImageEditing))]
        fn allows_image_editing(&self) -> bool {
            self.has(flag::IMAGE_EDITING)
        }

        #[unsafe(method(setAllowsImageEditing:))]
        fn set_allows_image_editing(&self, on: bool) {
            self.set(flag::IMAGE_EDITING, on);
        }

        #[unsafe(method(displaysLinkToolTips))]
        fn displays_link_tool_tips(&self) -> bool {
            !self.has(flag::NO_LINK_TOOLTIPS)
        }

        #[unsafe(method(setDisplaysLinkToolTips:))]
        fn set_displays_link_tool_tips(&self, on: bool) {
            self.set(flag::NO_LINK_TOOLTIPS, !on);
        }

        #[unsafe(method(acceptsGlyphInfo))]
        fn accepts_glyph_info(&self) -> bool {
            self.has(flag::GLYPH_INFO)
        }

        #[unsafe(method(setAcceptsGlyphInfo:))]
        fn set_accepts_glyph_info(&self, on: bool) {
            self.set(flag::GLYPH_INFO, on);
        }

        #[unsafe(method(usesInspectorBar))]
        fn uses_inspector_bar(&self) -> bool {
            self.has(flag::INSPECTOR_BAR)
        }

        #[unsafe(method(setUsesInspectorBar:))]
        fn set_uses_inspector_bar(&self, on: bool) {
            self.set(flag::INSPECTOR_BAR, on);
        }

        #[unsafe(method(allowsDocumentBackgroundColorChange))]
        fn allows_document_background_color_change(&self) -> bool {
            self.has(flag::BACKGROUND_COLOR_CHANGE)
        }

        #[unsafe(method(setAllowsDocumentBackgroundColorChange:))]
        fn set_allows_document_background_color_change(&self, on: bool) {
            self.set(flag::BACKGROUND_COLOR_CHANGE, on);
        }

        #[unsafe(method(usesRolloverButtonForSelection))]
        fn uses_rollover_button_for_selection(&self) -> bool {
            self.has(flag::ROLLOVER)
        }

        #[unsafe(method(setUsesRolloverButtonForSelection:))]
        fn set_uses_rollover_button_for_selection(&self, on: bool) {
            self.set(flag::ROLLOVER, on);
        }

        #[unsafe(method(allowsCharacterPickerTouchBarItem))]
        fn allows_character_picker_touch_bar_item(&self) -> bool {
            !self.has(flag::NO_CHARACTER_PICKER)
        }

        #[unsafe(method(setAllowsCharacterPickerTouchBarItem:))]
        fn set_allows_character_picker_touch_bar_item(&self, on: bool) {
            self.set(flag::NO_CHARACTER_PICKER, !on);
        }

        #[unsafe(method(usesAdaptiveColorMappingForDarkAppearance))]
        fn uses_adaptive_color_mapping(&self) -> bool {
            self.has(flag::ADAPTIVE_COLORS)
        }

        #[unsafe(method(setUsesAdaptiveColorMappingForDarkAppearance:))]
        fn set_uses_adaptive_color_mapping(&self, on: bool) {
            self.set(flag::ADAPTIVE_COLORS, on);
        }

        #[unsafe(method(enabledTextCheckingTypes))]
        fn enabled_text_checking_types(&self) -> u64 {
            self.ivars().checking_types.get()
        }

        #[unsafe(method(setEnabledTextCheckingTypes:))]
        fn set_enabled_text_checking_types(&self, types: u64) {
            self.ivars().checking_types.set(types);
        }

        #[unsafe(method(writingToolsBehavior))]
        fn writing_tools_behavior(&self) -> isize {
            self.ivars().writing_tools.get()
        }

        #[unsafe(method(setWritingToolsBehavior:))]
        fn set_writing_tools_behavior(&self, b: isize) {
            self.ivars().writing_tools.set(b);
        }

        #[unsafe(method(allowedWritingToolsResultOptions))]
        fn allowed_writing_tools_result_options(&self) -> usize {
            self.ivars().result_options.get()
        }

        #[unsafe(method(setAllowedWritingToolsResultOptions:))]
        fn set_allowed_writing_tools_result_options(&self, o: usize) {
            self.ivars().result_options.set(o);
        }

        #[unsafe(method(isWritingToolsActive))]
        fn is_writing_tools_active(&self) -> bool {
            false
        }

        #[unsafe(method(inlinePredictionType))]
        fn inline_prediction_type(&self) -> isize {
            self.ivars().prediction.get()
        }

        #[unsafe(method(setInlinePredictionType:))]
        fn set_inline_prediction_type(&self, t: isize) {
            self.ivars().prediction.set(t);
        }

        #[unsafe(method(mathExpressionCompletionType))]
        fn math_expression_completion_type(&self) -> isize {
            self.ivars().math_completion.get()
        }

        #[unsafe(method(setMathExpressionCompletionType:))]
        fn set_math_expression_completion_type(&self, t: isize) {
            self.ivars().math_completion.set(t);
        }

        #[unsafe(method(layoutOrientation))]
        fn layout_orientation(&self) -> isize {
            self.ivars().orientation.get()
        }

        #[unsafe(method(setLayoutOrientation:))]
        fn set_layout_orientation(&self, o: isize) {
            self.ivars().orientation.set(o);
        }

        #[unsafe(method_id(allowedInputSourceLocales))]
        fn allowed_input_source_locales(&self) -> Option<Retained<AnyObject>> {
            self.ivars().locales.borrow().clone()
        }

        #[unsafe(method(setAllowedInputSourceLocales:))]
        fn set_allowed_input_source_locales(&self, locales: Option<&AnyObject>) {
            *self.ivars().locales.borrow_mut() = locales.map(|l| l.retain());
        }

        #[unsafe(method_id(textHighlightAttributes))]
        fn text_highlight_attributes(&self) -> Retained<Dict> {
            let set = self.ivars().highlight_attrs.borrow().clone();
            set.unwrap_or_default()
        }

        #[unsafe(method(setTextHighlightAttributes:))]
        fn set_text_highlight_attributes(&self, attrs: &Dict) {
            *self.ivars().highlight_attrs.borrow_mut() = Some(attrs.retain());
        }

        #[unsafe(method(spellCheckerDocumentTag))]
        fn spell_checker_document_tag(&self) -> isize {
            0
        }
    }

    unsafe impl NSObjectProtocol for NSTextViewImpl {}
);

fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("sidestep: text views belong to the main thread")
}

/// A class method of NSColor if it has one (the semantic colors), else
/// `fallback`.
fn semantic_color(sel: Sel, fallback: impl FnOnce() -> Retained<NSColor>) -> Retained<NSColor> {
    let class: &AnyClass = NSColor::class();
    if class.class_method(sel).is_some() {
        // SAFETY: NSColor's semantic color methods take nothing and return a
        // color, autoreleased, which is retained here.
        let c: *mut NSColor = unsafe { objc2::runtime::MessageReceiver::send_message(class, sel, ()) };
        if let Some(c) = unsafe { Retained::retain(c) } {
            return c;
        }
    }
    fallback()
}

fn default_selected_attributes() -> Retained<Dict> {
    let color = semantic_color(sel!(selectedTextBackgroundColor), || {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.70, 0.84, 1.0, 1.0)
    });
    // SAFETY: the key is a constant string AppKit exports.
    let key = unsafe { objc2_app_kit::NSBackgroundColorAttributeName };
    NSDictionary::from_slices(&[key], &[&*color as &AnyObject])
}

fn default_marked_attributes() -> Retained<Dict> {
    let underline = objc2_foundation::NSNumber::new_isize(1);
    // SAFETY: the key is a constant string AppKit exports.
    let key = unsafe { objc2_app_kit::NSUnderlineStyleAttributeName };
    NSDictionary::from_slices(&[key], &[&*underline as &AnyObject])
}

/// Typing attributes a new text view starts with: 12-point Helvetica in
/// the text color, as AppKit's.
fn default_typing_attributes() -> Retained<Dict> {
    let font = NSFont::fontWithName_size(&NSString::from_str("Helvetica"), 12.0)
        .unwrap_or_else(|| NSFont::systemFontOfSize(12.0));
    let color = NSColor::textColor();
    // SAFETY: the keys are constant strings AppKit exports.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[&*font as &AnyObject, &*color as &AnyObject])
}

/// Fill `rect` (view points) with `color` in the view being drawn.
pub(crate) fn fill(rect: NSRect, color: &NSColor) {
    let color = crate::color::resolve(color);
    crate::graphics::with_recorder(|rec| {
        let r = rec.xf.rect(rect).intersect(&rec.clip);
        if !r.is_empty() {
            rec.ops.push(crate::protocol::Op::Fill { rect: r, color });
        }
    });
}

/// A new text view with a network of its own, as `initWithFrame:` makes
/// it.
pub(crate) fn new_text_view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSTextView> {
    crate::load_shell::<NSTextView>();
    // SAFETY: the initializer building a network.
    unsafe { msg_send![NSTextView::alloc(mtm), initWithFrame: frame] }
}

/// A scroll view with a text view as its document, set up as AppKit's
/// `scrollableTextView` is (both start empty): the view follows the
/// scroll view's size, filling what shows of it at least, and grows down
/// with its text.
fn scrollable(mtm: MainThreadMarker, rich: bool) -> Retained<NSScrollView> {
    crate::load_shell::<NSScrollView>();
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::ZERO);
    scroll.setHasVerticalScroller(true);
    let size = scroll.contentSize();
    let tv = new_text_view(mtm, NSRect::new(NSPoint::ZERO, size));
    tv.setMinSize(size);
    tv.setMaxSize(NSSize::new(size.width, HUGE));
    tv.setVerticallyResizable(true);
    tv.setHorizontallyResizable(false);
    tv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    tv.setRichText(rich);
    // SAFETY: textContainer takes nothing.
    if let Some(c) = unsafe { tv.textContainer() } {
        c.setSize(NSSize::new(size.width, HUGE));
        c.setWidthTracksTextView(true);
    }
    scroll.setDocumentView(Some(&tv));
    scroll
}

impl NSTextViewImpl {
    pub(crate) fn as_text_view(&self) -> &NSTextView {
        // SAFETY: NSTextView is this class.
        unsafe { &*(self as *const Self).cast::<NSTextView>() }
    }

    pub(crate) fn as_view(&self) -> &NSView {
        self.as_text_view()
    }

    pub(crate) fn as_object(&self) -> &AnyObject {
        self.as_text_view()
    }

    fn has(&self, bit: u64) -> bool {
        self.ivars().flags.get() & bit != 0
    }

    fn set(&self, bit: u64, on: bool) {
        let f = self.ivars().flags.get();
        self.ivars().flags.set(if on { f | bit } else { f & !bit });
    }

    pub(crate) fn storage(&self) -> Option<Retained<NSTextStorage>> {
        self.ivars().storage.borrow().clone()
    }

    pub(crate) fn container(&self) -> Option<Retained<NSTextContainer>> {
        self.ivars().container.borrow().clone()
    }

    pub(crate) fn manager(&self) -> Option<Retained<NSLayoutManager>> {
        let c = self.container()?;
        // SAFETY: layoutManager takes nothing.
        unsafe { c.layoutManager() }
    }

    /// The layout manager as Sidestep's own class (or a subclass), for the
    /// Rust fast paths.
    pub(crate) fn lm_impl(&self) -> Option<Retained<NSLayoutManagerImpl>> {
        let lm = self.manager()?;
        let ours = <NSLayoutManagerImpl as ClassType>::class();
        crate::textkit::is_kind(lm.class(), ours).then(|| {
            // SAFETY: an instance of the class or a subclass.
            unsafe { Retained::cast_unchecked(lm) }
        })
    }

    pub(crate) fn delegate_object(&self) -> Option<Retained<AnyObject>> {
        self.ivars().delegate.borrow().load()
    }

    pub(crate) fn text_length(&self) -> usize {
        self.storage().map_or(0, |s| selection::len(&s))
    }

    pub(crate) fn is_editable_now(&self) -> bool {
        self.has(flag::EDITABLE)
    }

    /// Whether it takes dropped text (see `drop`).
    pub(crate) fn takes_drops(&self) -> bool {
        self.has(flag::DROP_TARGET)
    }

    pub(crate) fn set_takes_drops(&self, on: bool) {
        self.set(flag::DROP_TARGET, on);
    }

    pub(crate) fn is_selectable_now(&self) -> bool {
        self.has(flag::SELECTABLE)
    }

    pub(crate) fn is_field_editor_now(&self) -> bool {
        self.has(flag::FIELD_EDITOR)
    }

    pub(crate) fn allows_undo(&self) -> bool {
        self.has(flag::ALLOWS_UNDO)
    }

    pub(crate) fn is_secure(&self) -> bool {
        self.has(flag::SECURE)
    }

    /// Make this a secure field's editor: its layout manager shows a bullet
    /// for each character, and the text can't be copied or cut.
    pub(crate) fn set_secure(&self, on: bool) {
        self.set(flag::SECURE, on);
        if let Some(lm) = self.lm_impl() {
            lm.set_masked(on);
        }
    }

    /// Attach to an existing network through its container.
    fn attach(&self, c: &NSTextContainer) {
        c.setTextView(Some(self.as_text_view()));
        *self.ivars().container.borrow_mut() = Some(c.retain());
        // SAFETY: layoutManager and textStorage take nothing.
        let storage = unsafe { c.layoutManager() }.and_then(|lm| unsafe { lm.textStorage() });
        *self.ivars().storage.borrow_mut() = storage;
    }

    /// Build a storage, layout manager and container of the view's own.
    fn build_network(&self, frame: NSRect) {
        crate::load_shell::<NSTextStorage>();
        crate::load_shell::<NSLayoutManager>();
        crate::load_shell::<NSTextContainer>();
        let storage = NSTextStorage::new();
        let lm = NSLayoutManager::new();
        storage.addLayoutManager(&lm);
        let c = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(frame.size.width, HUGE));
        c.setWidthTracksTextView(true);
        lm.addTextContainer(&c);
        self.attach(&c);
    }

    /// The text container's origin in the view, from `textContainerOrigin`
    /// when a subclass overrides it.
    pub(crate) fn origin(&self) -> NSPoint {
        if ORIGIN.overridden(self, sel!(textContainerOrigin)) {
            // SAFETY: the method takes nothing and returns a point.
            unsafe { msg_send![self, textContainerOrigin] }
        } else {
            self.own_origin()
        }
    }

    fn own_origin(&self) -> NSPoint {
        let i = self.ivars().inset.get();
        NSPoint::new(i.width, i.height)
    }

    /// `characterIndexForInsertionAtPoint:`'s answer, for a point in the
    /// view.
    fn insertion_index_at(&self, p: NSPoint) -> usize {
        let o = self.origin();
        self.lm_impl().map_or(0, |lm| lm.insertion_index(NSPoint::new(p.x - o.x, p.y - o.y)).0)
    }

    /// Where a drop at `p` (in the view) would go: from
    /// `characterIndexForInsertionAtPoint:`, by message when a subclass
    /// overrides it.
    pub(crate) fn drop_index(&self, p: NSPoint) -> usize {
        if INSERTION_INDEX.overridden(self, sel!(characterIndexForInsertionAtPoint:)) {
            // SAFETY: the method takes a point and returns an index.
            unsafe { msg_send![self, characterIndexForInsertionAtPoint: p] }
        } else {
            self.insertion_index_at(p)
        }
    }

    pub(crate) fn selection(&self) -> NSRange {
        self.ivars().selection.get()
    }

    pub(crate) fn typing_attributes_dict(&self) -> Retained<Dict> {
        let known = self.ivars().typing.borrow().clone();
        known.unwrap_or_else(|| {
            let d = default_typing_attributes();
            *self.ivars().typing.borrow_mut() = Some(d.clone());
            d
        })
    }

    pub(crate) fn coalescing(&self) -> Option<Rc<RefCell<Typing>>> {
        self.ivars().coalescing.borrow().clone()
    }

    pub(crate) fn set_coalescing(&self, run: Option<Rc<RefCell<Typing>>>) {
        // Dropped once the borrow ends.
        let old = std::mem::replace(&mut *self.ivars().coalescing.borrow_mut(), run);
        drop(old);
    }

    pub(crate) fn undo_as(&self) -> UndoAs {
        self.ivars().undo_as.get()
    }

    pub(crate) fn set_undo_as(&self, how: UndoAs) {
        self.ivars().undo_as.set(how);
    }

    /// Forget the text an input method is composing (it stays in the text,
    /// no longer marked), as a program's change to the text does.
    pub(crate) fn discard_marked_text(&self) {
        if self.ivars().marked.take().is_none() {
            return;
        }
        super::input_client::forget_composition(self);
        if let Some(c) = self.as_view().inputContext() {
            c.discardMarkedText();
        }
        self.as_view().setNeedsDisplay(true);
    }

    /// The marked range, if it is still inside the text (it is dropped when
    /// it isn't).
    pub(crate) fn marked_range(&self) -> Option<NSRange> {
        let m = self.ivars().marked.get()?;
        if m.location + m.length <= self.text_length() {
            return Some(m);
        }
        self.ivars().marked.set(None);
        super::input_client::forget_composition(self);
        None
    }

    /// A field editor's session ended without it resigning (its control
    /// left the window): forget editing, quietly, so the next session
    /// begins afresh.
    pub(crate) fn reset_editing(&self) {
        self.ivars().editing.set(false);
        self.set_coalescing(None);
        self.discard_marked_text();
    }

    /// The undo manager edits register with: the delegate's
    /// `undoManagerForTextView:`, else the responder chain's.
    pub(crate) fn text_undo_manager(&self) -> Option<Retained<NSUndoManager>> {
        if let Some(d) = self.delegate_object()
            && notify::responds(&d, sel!(undoManagerForTextView:))
        {
            // SAFETY: the delegate method takes the view and returns a
            // manager or nil.
            return unsafe { msg_send![&*d, undoManagerForTextView: self.as_text_view()] };
        }
        // SAFETY: NSResponder's undoManager, through the responder chain.
        unsafe { msg_send![super(self), undoManager] }
    }

    fn paragraph_style(&self) -> Option<Retained<NSParagraphStyle>> {
        let dict = self.typing_attributes_dict();
        // SAFETY: the key is a constant string AppKit exports.
        let style = dict.objectForKey(unsafe { objc2_app_kit::NSParagraphStyleAttributeName });
        style
            .and_then(|s| s.downcast::<NSParagraphStyle>().ok())
            .or_else(|| self.ivars().default_style.borrow().clone())
    }

    fn paragraph_change_range(&self) -> NSRange {
        if !(self.has(flag::EDITABLE) && self.has(flag::RICH)) {
            return NSRange::new(NOT_FOUND, 0);
        }
        let sel = self.selection();
        let Some(ts) = self.storage() else { return sel };
        let a = selection::paragraph(&ts, sel.location);
        let b = selection::paragraph(&ts, (sel.location + sel.length).saturating_sub(usize::from(sel.length > 0)));
        NSRange::new(a.start, b.end.max(a.end) - a.start)
    }

    /// Set one attribute on all the text and in the typing attributes.
    fn apply_to_all(&self, key: &NSString, value: &AnyObject) {
        let len = self.text_length();
        self.apply_to_range(key, Some(value), NSRange::new(0, len));
        let typing = self.typing_attributes_dict();
        let new = super::attrs::with_value(&typing, key, Some(value));
        *self.ivars().typing.borrow_mut() = Some(new);
    }

    /// Set (or, for none, remove) one attribute over `range`.
    fn apply_to_range(&self, key: &NSString, value: Option<&AnyObject>, range: NSRange) {
        let Some(storage) = self.storage() else { return };
        if range.length == 0 || range.location + range.length > storage.length() {
            return;
        }
        let m: &NSMutableAttributedString = &storage;
        match value {
            // SAFETY: the value is an object for the attribute.
            Some(v) => unsafe { m.addAttribute_value_range(key, v, range) },
            None => m.removeAttribute_range(key, range),
        }
    }

    fn set_alignment_in(&self, alignment: NSTextAlignment, range: NSRange) {
        let style = self.mutable_style();
        style.setAlignment(alignment);
        self.set_style_in(&style, range);
    }

    fn set_direction_in(&self, direction: NSWritingDirection, range: NSRange) {
        let style = self.mutable_style();
        style.setBaseWritingDirection(direction);
        self.set_style_in(&style, range);
    }

    fn mutable_style(&self) -> Retained<objc2_app_kit::NSMutableParagraphStyle> {
        match self.paragraph_style() {
            // SAFETY: -mutableCopy of a paragraph style is a mutable one.
            Some(s) => unsafe { msg_send![&*s, mutableCopy] },
            None => objc2_app_kit::NSMutableParagraphStyle::new(),
        }
    }

    fn set_style_in(&self, style: &objc2_app_kit::NSMutableParagraphStyle, range: NSRange) {
        // SAFETY: the key is a constant string AppKit exports.
        let key = unsafe { objc2_app_kit::NSParagraphStyleAttributeName };
        if let Some(storage) = self.storage() {
            let a = selection::paragraph(&storage, range.location);
            let b =
                selection::paragraph(&storage, (range.location + range.length).saturating_sub(1).max(range.location));
            self.apply_to_range(key, Some(style), NSRange::new(a.start, b.end.max(a.end) - a.start));
        }
        let typing = self.typing_attributes_dict();
        *self.ivars().typing.borrow_mut() = Some(super::attrs::with_value(&typing, key, Some(style)));
    }

    /// `setString:`: all the text replaced, in the first character's
    /// attributes (the typing attributes in an empty view), the insertion
    /// point after it. No delegate questions and no undo.
    fn replace_all(&self, string: &NSString) {
        let Some(storage) = self.storage() else { return };
        let attrs = if storage.length() > 0 {
            // SAFETY: an index inside the text.
            unsafe { storage.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) }
        } else {
            self.typing_attributes_dict()
        };
        // SAFETY: the attributes are an attribute dictionary.
        let new = unsafe { NSAttributedString::new_with_attributes(string, &attrs) };
        self.set_coalescing(None);
        self.discard_marked_text();
        self.begin_transaction();
        let m: &NSMutableAttributedString = &storage;
        m.replaceCharactersInRange_withAttributedString(NSRange::new(0, storage.length()), &new);
        self.set_selection_internal(NSRange::new(string.length(), 0), true);
        self.end_quiet_transaction();
    }

    // Transactions.

    pub(crate) fn begin_transaction(&self) {
        let t = &self.ivars().transactions;
        t.set(t.get() + 1);
    }

    /// End a transaction, the selection scrolled to as a user's edit is.
    pub(crate) fn end_transaction(&self) {
        if self.close_transaction() {
            self.scroll_to(self.selection());
        }
    }

    /// End a transaction of a program's edit (`setString:`,
    /// `replaceCharactersInRange:withString:`), which leaves the scroll
    /// position alone.
    fn end_quiet_transaction(&self) {
        self.close_transaction();
    }

    /// Whether the outermost transaction ended.
    fn close_transaction(&self) -> bool {
        let t = &self.ivars().transactions;
        t.set(t.get().saturating_sub(1));
        let done = t.get() == 0;
        if done {
            if self.ivars().needs_size.replace(false) {
                self.apply_layout_damage(false);
            }
            self.caret_activity(true);
        }
        done
    }

    /// Begin editing, if not already: the delegate may refuse.
    pub(crate) fn begin_editing(&self) -> bool {
        if self.ivars().editing.get() {
            return true;
        }
        let delegate = self.delegate_object();
        if !notify::ask(delegate.as_deref(), sel!(textShouldBeginEditing:), self.as_object()) {
            return false;
        }
        self.ivars().editing.set(true);
        notify::post(self.as_object(), delegate.as_deref(), Note::BeginEditing, None);
        true
    }

    /// End editing, unless the delegate refuses. `movement` is how it ended
    /// (an `NSTextMovement`), for the notification.
    pub(crate) fn end_editing(&self, movement: isize) -> bool {
        if !self.ivars().editing.get() {
            return true;
        }
        let delegate = self.delegate_object();
        if !notify::ask(delegate.as_deref(), sel!(textShouldEndEditing:), self.as_object()) {
            return false;
        }
        self.ivars().editing.set(false);
        let info = movement_info(movement);
        notify::post(self.as_object(), delegate.as_deref(), Note::EndEditing, Some(&info));
        true
    }

    /// End editing whether or not anything was edited (a field editor's
    /// Return, Tab or Backtab), unless the delegate refuses.
    /// Nothing edited since editing last ended: the delegate isn't asked,
    /// only told.
    pub(crate) fn end_editing_forced(&self, movement: isize) -> bool {
        if self.ivars().editing.get() {
            return self.end_editing(movement);
        }
        let delegate = self.delegate_object();
        let info = movement_info(movement);
        notify::post(self.as_object(), delegate.as_deref(), Note::EndEditing, Some(&info));
        true
    }

    /// Begin editing, ask the delegate, and register the change's undo.
    fn should_change(&self, range: NSRange, string: Option<&NSString>) -> bool {
        if !self.has(flag::EDITABLE) {
            return false;
        }
        if !self.begin_editing() {
            return false;
        }
        if let Some(d) = self.delegate_object()
            && notify::responds(&d, sel!(textView:shouldChangeTextInRange:replacementString:))
        {
            // SAFETY: the delegate method takes the view, a range and a
            // string or nil, and returns BOOL.
            let ok: bool = unsafe {
                msg_send![&*d, textView: self.as_text_view(), shouldChangeTextInRange: range, replacementString: string]
            };
            if !ok {
                return false;
            }
        }
        edit::register_change(self, range, string);
        true
    }

    // Selection.

    /// Select `range` as `setSelectedRange:affinity:stillSelecting:`
    /// does: through the funnel.
    fn select(&self, range: NSRange, affinity: NSSelectionAffinity, still: bool) {
        if !self.funnel(&[range], affinity, still, Funneled::Program) {
            self.select_now(vec![range], affinity, still);
        }
    }

    fn select_ranges(&self, ranges: &NSArray<NSValue>, affinity: NSSelectionAffinity, still: bool) {
        let list: Vec<NSRange> = ranges.iter().map(|v| range_of(&v)).collect();
        if !self.funnel(&list, affinity, still, Funneled::Program) {
            self.select_now(list, affinity, still);
        }
    }

    /// Send the change to an overriding `setSelectedRanges:affinity:
    /// stillSelecting:`, telling its call to super what kind of change it
    /// is. Whether it was sent: not when nothing overrides the method, nor
    /// for a change made while one is on its way (the override's own).
    fn funnel(&self, ranges: &[NSRange], affinity: NSSelectionAffinity, still: bool, kind: Funneled) -> bool {
        if self.ivars().funneled.get().is_some()
            || !SELECT.overridden(self, sel!(setSelectedRanges:affinity:stillSelecting:))
        {
            return false;
        }
        let values: Vec<Retained<NSValue>> = ranges.iter().map(|r| value_of(*r)).collect();
        let values = NSArray::from_retained_slice(&values);
        self.ivars().funneled.set(Some(kind));
        // SAFETY: the method takes an array of ranges, an affinity and a
        // BOOL.
        let _: () = unsafe {
            msg_send![self.as_text_view(), setSelectedRanges: &*values, affinity: affinity, stillSelecting: still]
        };
        self.ivars().funneled.set(None);
        true
    }

    /// `setSelectedRanges:affinity:stillSelecting:`'s work. A change of
    /// the view's own keeps what it would have kept (its edit's typing
    /// attributes, the anchor and goal its command sets); a program's
    /// resets the anchor and the vertical goal.
    fn select_now(&self, list: Vec<NSRange>, affinity: NSSelectionAffinity, still: bool) {
        let own = match self.ivars().funneled.take() {
            Some(Funneled::Own { from_edit }) => Some(from_edit),
            _ => None,
        };
        let list = normalize_ranges(list, self.text_length());
        let Some(&first) = list.first() else { return };
        self.ivars().affinity.set(affinity);
        self.set(flag::STILL_SELECTING, still);
        match own {
            Some(from_edit) => self.apply_selection(first, from_edit),
            None => {
                self.apply_selection(first, false);
                self.ivars().goal_x.set(None);
                self.ivars().anchor.set(None);
            }
        }
        if list.len() > 1 {
            *self.ivars().ranges.borrow_mut() = list;
        }
    }

    /// The text storage changed characters outside the view's own edits
    /// (a program editing the storage): the selection moves as AppKit's
    /// does, the marked text is forgotten, and a run of typing ends.
    /// `range` is the edited range as the text is now; `delta`, the change
    /// in length.
    fn storage_edited_here(&self, range: NSRange, delta: isize) {
        if self.ivars().transactions.get() > 0 {
            return;
        }
        self.discard_marked_text();
        self.set_coalescing(None);
        let old = NSRange::new(range.location, (range.length as isize - delta).max(0) as usize);
        let sel = self.selection();
        let moved = moved_by_edit(sel, old, range.length);
        if moved != sel || self.ivars().ranges.borrow().len() > 1 {
            self.set_selection_internal(moved, false);
        }
    }

    /// Change the selection for the view's own reasons (an edit, a
    /// command, a click): through an overriding `setSelectedRanges:
    /// affinity:stillSelecting:` if there is one, as AppKit's text view
    /// does, with the view's affinity (upstream after an edit).
    pub(crate) fn set_selection_internal(&self, range: NSRange, from_edit: bool) {
        let affinity = if from_edit { NSSelectionAffinity::Upstream } else { self.ivars().affinity.get() };
        let still = self.has(flag::STILL_SELECTING);
        if !self.funnel(&[range], affinity, still, Funneled::Own { from_edit }) {
            self.ivars().affinity.set(affinity);
            self.apply_selection(range, from_edit);
        }
    }

    /// Change the selection: ask the delegate, redraw what changed, take
    /// the typing attributes from the text (unless the change came with an
    /// edit, which keeps them), and tell everyone.
    fn apply_selection(&self, range: NSRange, from_edit: bool) {
        let len = self.text_length();
        let loc = range.location.min(len);
        let mut range = NSRange::new(loc, range.length.min(len - loc));
        let old = self.selection();
        let delegate = self.delegate_object();
        if let Some(d) = &delegate {
            let sel = sel!(textView:willChangeSelectionFromCharacterRange:toCharacterRange:);
            if notify::responds(d, sel) {
                // SAFETY: the delegate method takes the view and two ranges
                // and returns a range.
                range = unsafe {
                    msg_send![&**d, textView: self.as_text_view(), willChangeSelectionFromCharacterRange: old, toCharacterRange: range]
                };
                let loc = range.location.min(len);
                range = NSRange::new(loc, range.length.min(len - loc));
            }
        }
        self.ivars().selection.set(range);
        *self.ivars().ranges.borrow_mut() = vec![range];
        self.redraw_selection_change(old, range, false);
        if !from_edit {
            self.set_coalescing(None);
            self.take_typing_attributes();
        }
        let (key, keys) =
            (NSString::from_str("NSOldSelectedCharacterRange"), NSString::from_str("NSOldSelectedCharacterRanges"));
        let value = value_of(old);
        let values = NSArray::from_retained_slice(std::slice::from_ref(&value));
        let info: Retained<Dict> =
            NSDictionary::from_slices(&[&*key, &*keys], &[&*value as &AnyObject, &*values as &AnyObject]);
        notify::post(self.as_object(), delegate.as_deref(), Note::ChangeSelection, Some(&info));
        self.caret_activity(true);
        if let Some(c) = self.as_view().inputContext() {
            c.invalidateCharacterCoordinates();
        }
    }

    /// The typing attributes of the text at the insertion point: the
    /// character before it, or the first when it is at the start.
    fn take_typing_attributes(&self) {
        let Some(storage) = self.storage() else { return };
        let len = storage.length();
        if len == 0 {
            return;
        }
        let sel = self.selection();
        let at = if sel.length > 0 { sel.location } else { sel.location.saturating_sub(1) }.min(len - 1);
        // SAFETY: an index inside the text.
        let attrs = unsafe { storage.attributesAtIndex_effectiveRange(at, std::ptr::null_mut()) };
        *self.ivars().typing.borrow_mut() = Some(attrs);
    }

    /// The range a proposed selection becomes at a granularity: whole
    /// words, or whole paragraphs.
    pub(crate) fn range_for_granularity(&self, proposed: NSRange, g: NSSelectionGranularity) -> NSRange {
        let Some(ts) = self.storage() else { return proposed };
        let len = selection::len(&ts);
        let (a, b) = (proposed.location.min(len), (proposed.location + proposed.length).min(len));
        let r = match g {
            NSSelectionGranularity::SelectByWord => {
                let first = selection::word_at(&ts, a);
                let last = if b > a { selection::word_at(&ts, b - 1) } else { first.clone() };
                first.start.min(a)..last.end.max(b)
            }
            NSSelectionGranularity::SelectByParagraph => {
                let first = selection::paragraph(&ts, a);
                let last = if b > a { selection::paragraph(&ts, b - 1) } else { first.clone() };
                first.start..last.end
            }
            _ => a..b,
        };
        ns(r)
    }

    /// Redraw the selection as it is (its look changes with focus).
    fn redraw_selection(&self) {
        let sel = self.selection();
        self.redraw_selection_change(sel, sel, true);
    }

    /// Redraw what shows of the selection changing from `old` to `new`:
    /// the carets, and the lines that one covers and the other doesn't
    /// (all both cover, with `all`), one rect for each stretch, only where
    /// the view shows them. Nothing of a long selection outside that is
    /// measured or laid out.
    fn redraw_selection_change(&self, old: NSRange, new: NSRange, all: bool) {
        let view = self.as_view();
        if view.window().is_none() {
            return;
        }
        let Some(lm) = self.lm_impl() else { return };
        let o = self.origin();
        let upstream = self.ivars().affinity.get() == NSSelectionAffinity::Upstream;
        for r in [old, new] {
            // A caret in text not laid out yet is drawn when it is.
            if r.length == 0 && lm.is_laid(r.location..r.location) {
                let caret = lm.caret_rect(r.location, upstream);
                view.setNeedsDisplayInRect(offset(grow(caret, 1.0), o));
            }
        }
        let range = |r: NSRange| r.location..r.location + r.length;
        let changed: Vec<Range<usize>> =
            if all { vec![range(old), range(new)] } else { symmetric_difference(range(old), range(new)) };
        let changed: Vec<Range<usize>> = changed.into_iter().filter(|r| !r.is_empty()).collect();
        if changed.is_empty() {
            return;
        }
        let visible = view.visibleRect();
        if visible.size.width <= 0.0 || visible.size.height <= 0.0 {
            return;
        }
        let lines = lm.lines_in_y(visible.origin.y - o.y, visible.origin.y + visible.size.height - o.y);
        for c in changed {
            let span = lines
                .iter()
                .filter(|l| {
                    let r = l.range();
                    r.start < c.end && r.end > c.start
                })
                .map(|l| l.fragment_span())
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
            if let Some((a, b)) = span {
                let r = NSRect::new(NSPoint::new(visible.origin.x, a + o.y), NSSize::new(visible.size.width, b - a));
                view.setNeedsDisplayInRect(r);
            }
        }
    }

    // Size and scrolling.

    /// The layout manager laid text out again: once any transaction ends,
    /// draw again what changed, and size to the text if its extent may
    /// have (for background layout, `idle`, at most every so often).
    fn layout_changed_here(&self, idle: bool) {
        if self.ivars().transactions.get() > 0 {
            self.ivars().needs_size.set(true);
            return;
        }
        self.apply_layout_damage(idle);
    }

    fn apply_layout_damage(&self, idle: bool) {
        let Some(lm) = self.lm_impl() else { return };
        let damage = lm.take_damage();
        let resize = damage.is_some_and(|d| d.2) || self.ivars().size_owed.get();
        if resize {
            let now = Instant::now();
            let recent = self.ivars().sized_at.get().is_some_and(|t| now.duration_since(t) < IDLE_RESIZE);
            if idle && recent {
                self.ivars().size_owed.set(true);
            } else {
                self.ivars().size_owed.set(false);
                self.ivars().sized_at.set(Some(now));
                self.size_to_text();
            }
        }
        let Some((y0, y1, _)) = damage else { return };
        let view = self.as_view();
        if view.window().is_none() {
            return;
        }
        let o = self.origin();
        let b = view.bounds();
        let top = (y0 + o.y).max(b.origin.y);
        let bottom =
            if y1.is_finite() { (y1 + o.y).min(b.origin.y + b.size.height) } else { b.origin.y + b.size.height };
        if bottom > top {
            view.setNeedsDisplayInRect(NSRect::new(
                NSPoint::new(b.origin.x, top),
                NSSize::new(b.size.width, bottom - top),
            ));
        }
    }

    /// Grow or shrink to the text, as far as the view may: to its height
    /// as laid out so far, with estimates for the rest (background layout
    /// sizes it again as it goes), so a long text isn't laid out whole.
    fn size_to_text(&self) {
        if !self.has(flag::V_RESIZABLE) && !self.has(flag::H_RESIZABLE) {
            return;
        }
        let Some(lm) = self.lm_impl() else { return };
        let inset = self.ivars().inset.get();
        let height = lm.height() + 2.0 * inset.height;
        let width = lm.used_width() + 2.0 * inset.width;
        // SAFETY: the method's own types; a subclass may override it.
        let _: () = unsafe { msg_send![self, setConstrainedFrameSize: NSSize::new(width.ceil(), height.ceil())] };
    }

    /// As the document of a clip view: the minimum size is the clip view's,
    /// the maximum width follows it (for a view that doesn't grow across),
    /// and the view sizes to its text within them.
    fn follow_clip(&self) {
        // SAFETY: superview takes nothing.
        let Some(clip) = (unsafe { self.as_view().superview() }).and_then(|s| s.downcast::<NSClipView>().ok()) else {
            return;
        };
        let size = clip.bounds().size;
        self.ivars().min_size.set(size);
        if !self.has(flag::H_RESIZABLE) {
            let max = self.ivars().max_size.get();
            self.ivars().max_size.set(NSSize::new(size.width, max.height));
        }
        self.size_to_text();
    }

    /// A container tracking the view follows its size.
    fn track_frame(&self, size: NSSize) {
        let Some(c) = self.container() else { return };
        let inset = self.ivars().inset.get();
        let current = c.size();
        let w = if c.widthTracksTextView() { (size.width - 2.0 * inset.width).max(0.0) } else { current.width };
        let h = if c.heightTracksTextView() { (size.height - 2.0 * inset.height).max(0.0) } else { current.height };
        if (w, h) != (current.width, current.height) {
            c.setSize(NSSize::new(w, h));
        }
    }

    /// Scroll the enclosing clip view so `range` shows.
    pub(crate) fn scroll_to(&self, range: NSRange) {
        if unsafe { self.as_view().superview() }.is_none() {
            return;
        }
        let Some(lm) = self.lm_impl() else { return };
        let o = self.origin();
        let rect = if range.length == 0 {
            lm.caret_rect(range.location, false)
        } else {
            // The range's first line: nothing after it is measured.
            let (rects, _) = lm.first_line_rects(range.location..range.location + range.length);
            rects.into_iter().next().unwrap_or_else(|| lm.caret_rect(range.location, false))
        };
        scroll_rect_to_visible(self.as_view(), offset(grow(rect, 2.0), o));
    }

    // Drawing.

    fn draw(&self, dirty: NSRect) {
        let view = self.as_text_view();
        // SAFETY: the method's own types; a subclass may override it.
        let _: () = unsafe { msg_send![view, drawViewBackgroundInRect: dirty] };
        let Some(lm) = self.manager() else { return };
        let o = self.origin();
        let (y0, y1) = (dirty.origin.y - o.y, dirty.origin.y + dirty.size.height - o.y);
        let sel = self.selection();
        let ours = self.lm_impl();
        // Text blocks' boxes go under the selection.
        if let Some(ours) = &ours
            && keeps_drawing(&lm)
        {
            ours.draw_blocks_in_y(y0, y1, o);
        }
        if sel.length > 0
            && self.has(flag::SELECTABLE)
            && let Some(ours) = &ours
        {
            let attrs: Retained<Dict> = view.selectedTextAttributes();
            // SAFETY: the key is a constant string AppKit exports.
            let bg = attrs.objectForKey(unsafe { objc2_app_kit::NSBackgroundColorAttributeName });
            if let Some(color) = bg.and_then(|c| c.downcast::<NSColor>().ok()) {
                let color = if self.is_active() {
                    color
                } else {
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.86, 0.86, 0.86, 1.0)
                };
                // Only the lines in the dirty rect are measured.
                for r in ours.selection_rects_in(sel.location..sel.location + sel.length, y0, y1) {
                    fill(offset(r, o), &color);
                }
            }
        }
        match &ours {
            // Sidestep's own layout manager, drawing as it does: the lines in
            // the dirty rect, found by height.
            Some(ours) if keeps_drawing(&lm) => {
                ours.draw_rect(y0, y1, o, true);
                ours.draw_rect(y0, y1, o, false);
            }
            _ => {
                let Some(c) = self.container() else { return };
                let r = NSRect::new(NSPoint::new(dirty.origin.x - o.x, y0), dirty.size);
                let glyphs = lm.glyphRangeForBoundingRect_inTextContainer(r, &c);
                lm.drawBackgroundForGlyphRange_atPoint(glyphs, o);
                lm.drawGlyphsForGlyphRange_atPoint(glyphs, o);
            }
        }
        if let (Some(marked), Some(ours)) = (self.marked_range(), &ours) {
            let color = NSColor::textColor();
            for r in ours.selection_rects_in(marked.location..marked.location + marked.length, y0, y1) {
                let r = offset(r, o);
                let line = NSRect::new(
                    NSPoint::new(r.origin.x, r.origin.y + r.size.height - 1.0),
                    NSSize::new(r.size.width, 1.0),
                );
                fill(line, &color);
            }
        }
        let on = self.ivars().caret.borrow().on;
        if sel.length == 0
            && on
            && self.caret_wanted()
            && let Some(ours) = &ours
        {
            let upstream = self.ivars().affinity.get() == NSSelectionAffinity::Upstream;
            let rect = offset(ours.caret_rect(sel.location, upstream), o);
            let color: Retained<NSColor> = view.insertionPointColor();
            // SAFETY: the method's own types; a subclass may override it.
            let _: () = unsafe { msg_send![view, drawInsertionPointInRect: rect, color: &*color, turnedOn: true] };
        }
    }

    /// First responder in the key window.
    fn is_active(&self) -> bool {
        let view = self.as_text_view();
        view.window().is_some_and(|w| w.isKeyWindow()) && edit::is_first_responder(view)
    }

    /// Whether the caret shows: an insertion point in an active view.
    fn caret_wanted(&self) -> bool {
        self.selection().length == 0 && self.has(flag::SELECTABLE) && self.is_active()
    }

    // The caret's blinking.

    /// An edit or move: show the caret solid and blink again from now.
    fn caret_activity(&self, restart: bool) {
        if !self.caret_wanted() {
            self.stop_caret();
            return;
        }
        let running = {
            let mut caret = self.ivars().caret.borrow_mut();
            caret.on = true;
            caret.last_activity = Some(Instant::now());
            caret.timer.is_some()
        };
        self.redraw_caret();
        if running && !restart {
            return;
        }
        if running {
            // Blink from now: a fresh timer.
            if let Some(t) = self.ivars().caret.borrow_mut().timer.take() {
                t.invalidate();
            }
        }
        let weak: Weak<NSTextViewImpl> = Weak::from_retained(&self.retain());
        let block = block2::RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| match weak.load() {
            Some(view) => view.blink(),
            // SAFETY: the timer firing is alive while its block runs.
            None => unsafe { timer.as_ref() }.invalidate(),
        });
        // SAFETY: the block takes the timer.
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(BLINK.as_secs_f64(), true, &block) };
        self.ivars().caret.borrow_mut().timer = Some(timer);
    }

    fn blink(&self) {
        if !self.caret_wanted() {
            self.stop_caret();
            return;
        }
        let stop = {
            let mut caret = self.ivars().caret.borrow_mut();
            let idle = caret.last_activity.is_none_or(|t| t.elapsed() >= BLINK_IDLE);
            if idle {
                // Solid from now on, until the next edit or move.
                caret.on = true;
                caret.timer.take()
            } else {
                caret.on = !caret.on;
                None
            }
        };
        if let Some(t) = stop {
            t.invalidate();
        }
        self.redraw_caret();
    }

    fn stop_caret(&self) {
        let timer = {
            let mut caret = self.ivars().caret.borrow_mut();
            caret.on = false;
            caret.timer.take()
        };
        if let Some(t) = timer {
            t.invalidate();
            self.redraw_caret();
        }
    }

    fn redraw_caret(&self) {
        let sel = self.selection();
        if sel.length > 0 {
            return;
        }
        if let Some(lm) = self.lm_impl() {
            let r = lm.caret_rect(sel.location, self.ivars().affinity.get() == NSSelectionAffinity::Upstream);
            self.as_view().setNeedsDisplayInRect(offset(grow(r, 1.0), self.origin()));
        }
    }

    // The mouse.

    fn track_mouse(&self, event: &NSEvent) {
        if !self.has(flag::SELECTABLE) {
            // SAFETY: NSView's mouseDown:.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            return;
        }
        let view = self.as_text_view();
        if let Some(w) = view.window()
            && !edit::is_first_responder(view)
        {
            w.makeFirstResponder(Some(view));
        }
        let o = self.origin();
        let Some(lm) = self.lm_impl() else { return };
        let at = |e: &NSEvent| {
            let p = view.convertPoint_fromView(e.locationInWindow(), None);
            NSPoint::new(p.x - o.x, p.y - o.y)
        };
        let clicks = event.clickCount();
        let granularity = match clicks {
            0 | 1 => NSSelectionGranularity::SelectByCharacter,
            2 => NSSelectionGranularity::SelectByWord,
            _ => NSSelectionGranularity::SelectByParagraph,
        };
        let (index, upstream) = lm.insertion_index(at(event));
        let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
        let old = self.selection();
        // The end that stays: with Shift, the selection's end away from the
        // click; otherwise the click.
        let base =
            if shift { if index < old.location { old.location + old.length } else { old.location } } else { index };
        let whole = |r: NSRange| self.range_for_granularity(r, granularity);
        let first = if shift {
            let (a, b) = (base.min(index), base.max(index));
            whole(NSRange::new(a, b - a))
        } else if granularity == NSSelectionGranularity::SelectByCharacter {
            NSRange::new(index, 0)
        } else {
            whole(NSRange::new(index, 0))
        };
        let affinity = if upstream { NSSelectionAffinity::Upstream } else { NSSelectionAffinity::Downstream };
        self.select(first, affinity, true);
        self.ivars().granularity.set(granularity);
        let mut dragged = false;
        let mut idle = 0;
        loop {
            let until = NSDate::dateWithTimeIntervalSinceNow(1.0 / 30.0);
            let Some(window) = view.window() else { break };
            // SAFETY: the tracking mode is a constant string AppKit exports.
            let mode = unsafe { objc2_app_kit::NSEventTrackingRunLoopMode };
            let mask = NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp;
            let next = window.nextEventMatchingMask_untilDate_inMode_dequeue(mask, Some(&until), mode, true);
            let last = match next {
                Some(e) if e.r#type() == objc2_app_kit::NSEventType::LeftMouseUp => break,
                Some(e) => {
                    idle = 0;
                    dragged = true;
                    at(&e)
                }
                None => {
                    idle += 1;
                    if NSEvent::pressedMouseButtons() & 1 == 0 || idle > 300 || !dragged {
                        if NSEvent::pressedMouseButtons() & 1 == 0 || idle > 300 {
                            break;
                        }
                        continue;
                    }
                    let p = view.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
                    NSPoint::new(p.x - o.x, p.y - o.y)
                }
            };
            // Scroll toward a pointer outside what shows, then extend.
            scroll_rect_to_visible(view, NSRect::new(NSPoint::new(last.x + o.x, last.y + o.y), NSSize::new(1.0, 1.0)));
            let (to, _) = lm.insertion_index(last);
            let r = if granularity == NSSelectionGranularity::SelectByCharacter {
                let (a, b) = (base.min(to), base.max(to));
                NSRange::new(a, b - a)
            } else {
                let to_range = whole(NSRange::new(to, 0));
                let a = first.location.min(to_range.location);
                let b = (first.location + first.length).max(to_range.location + to_range.length);
                NSRange::new(a, b - a)
            };
            if r != self.selection() {
                self.select(r, NSSelectionAffinity::Downstream, true);
            }
        }
        let fin = self.selection();
        self.select(fin, self.ivars().affinity.get(), false);
        if fin.length > 0 {
            self.ivars().anchor.set(Some(if base <= fin.location { fin.location } else { fin.location + fin.length }));
        }
        // A click ends a composition: the input context commits the marked
        // text as it is with `unmarkText` (by message when a subclass
        // overrides it), as AppKit's does, and the input method starts
        // afresh.
        if self.marked_range().is_some() {
            if UNMARK.overridden(self, sel!(unmarkText)) {
                // SAFETY: unmarkText takes nothing.
                let _: () = unsafe { msg_send![view, unmarkText] };
            } else {
                super::input_client::unmark(self);
            }
            if let Some(c) = view.inputContext() {
                c.discardMarkedText();
            }
        }
        // A click on a link follows it.
        if !dragged && clicks == 1 && !shift {
            self.follow_link(index);
        }
    }

    fn follow_link(&self, index: usize) {
        let Some(storage) = self.storage() else { return };
        if index >= storage.length() {
            return;
        }
        // SAFETY: the key is a constant string AppKit exports; an index
        // inside the text.
        let link = unsafe {
            storage.attribute_atIndex_effectiveRange(objc2_app_kit::NSLinkAttributeName, index, std::ptr::null_mut())
        };
        if let Some(link) = link {
            // SAFETY: the method's own types; a subclass may override it.
            let _: () = unsafe { msg_send![self.as_text_view(), clickedOnLink: &*link, atIndex: index] };
        }
    }

    // What commands need.

    pub(crate) fn affinity(&self) -> NSSelectionAffinity {
        self.ivars().affinity.get()
    }

    pub(crate) fn goal_x(&self) -> Option<f64> {
        self.ivars().goal_x.get()
    }

    /// Move the insertion point (or, extending, the selection's moving end)
    /// for a command, keeping the vertical goal when asked.
    pub(crate) fn move_to(&self, index: usize, extend: bool, upstream: bool, keep_goal: Option<f64>) {
        let sel = self.selection();
        let affinity = if upstream { NSSelectionAffinity::Upstream } else { NSSelectionAffinity::Downstream };
        self.ivars().affinity.set(affinity);
        if extend {
            let anchor = self.ivars().anchor.get().unwrap_or(sel.location);
            let (a, b) = (anchor.min(index), anchor.max(index));
            self.set_selection_internal(NSRange::new(a, b - a), false);
            self.ivars().anchor.set(Some(anchor));
        } else {
            self.set_selection_internal(NSRange::new(index, 0), false);
            self.ivars().anchor.set(None);
        }
        self.ivars().goal_x.set(keep_goal);
        self.scroll_to(NSRange::new(index, 0));
    }

    /// Select from `anchor` to `active` for a command that extends the
    /// selection, the anchor staying put for the next such command.
    pub(crate) fn select_extended(&self, anchor: usize, active: usize, upstream: bool) {
        let affinity = if upstream { NSSelectionAffinity::Upstream } else { NSSelectionAffinity::Downstream };
        self.ivars().affinity.set(affinity);
        let (a, b) = (anchor.min(active), anchor.max(active));
        self.set_selection_internal(NSRange::new(a, b - a), false);
        self.ivars().anchor.set(Some(anchor));
        self.ivars().goal_x.set(None);
        self.scroll_to(NSRange::new(active, 0));
    }

    /// The moving end of the selection: where extending goes on from.
    pub(crate) fn moving_end(&self) -> usize {
        let sel = self.selection();
        match self.ivars().anchor.get() {
            Some(a) if a == sel.location => sel.location + sel.length,
            Some(_) => sel.location,
            None => sel.location + sel.length,
        }
    }

    pub(crate) fn edit_replace(&self, range: NSRange, text: &str, kind: Kind) -> bool {
        edit::user_replace(self, range, &NSString::from_str(text), kind)
    }

    pub(crate) fn edit_replace_ns(&self, range: NSRange, text: &NSString, kind: Kind) -> bool {
        edit::user_replace(self, range, text, kind)
    }
}

/// Whether `lm`'s class draws as Sidestep's does (no override of the
/// drawing methods), so the view can find lines by height.
fn keeps_drawing(lm: &NSLayoutManager) -> bool {
    let class = lm.class();
    let ours = <NSLayoutManagerImpl as ClassType>::class();
    std::ptr::eq(class, ours)
        || [sel!(drawGlyphsForGlyphRange:atPoint:), sel!(drawBackgroundForGlyphRange:atPoint:)].iter().all(|&s| match (
            class.instance_method(s),
            ours.instance_method(s),
        ) {
            (Some(a), Some(b)) => std::ptr::fn_addr_eq(a.implementation(), b.implementation()),
            _ => false,
        })
}

/// The stretches one of two ranges covers and the other doesn't.
fn symmetric_difference(a: Range<usize>, b: Range<usize>) -> Vec<Range<usize>> {
    if a.end <= b.start || b.end <= a.start {
        return vec![a, b];
    }
    vec![a.start.min(b.start)..a.start.max(b.start), a.end.min(b.end)..a.end.max(b.end)]
}

/// Where a selection goes when `edited` (in the text as it was) becomes
/// `added` units, as AppKit moves it: along with the text after the edit,
/// to the edit's end when the two overlap, and nowhere when it is before.
fn moved_by_edit(sel: NSRange, edited: NSRange, added: usize) -> NSRange {
    let (s, e) = (edited.location, edited.location + edited.length);
    let end = sel.location + sel.length;
    if sel.location >= e {
        NSRange::new(sel.location - edited.length + added, sel.length)
    } else if end > s || (sel.length == 0 && sel.location > s) {
        NSRange::new(s + added, 0)
    } else {
        sel
    }
}

/// Selected ranges as AppKit keeps them: inside the text, in order, those
/// that overlap or touch merged, and empty ones left out unless all are
/// (then the first in the text). Ranges wholly past the end go, unless
/// none is left.
fn normalize_ranges(mut list: Vec<NSRange>, len: usize) -> Vec<NSRange> {
    let clamp = |r: NSRange| {
        let loc = r.location.min(len);
        NSRange::new(loc, r.length.min(len - loc))
    };
    let inside: Vec<NSRange> = list.iter().filter(|r| r.location <= len).map(|&r| clamp(r)).collect();
    if inside.is_empty() {
        return list.first().map(|&r| vec![clamp(r)]).unwrap_or_default();
    }
    list = inside;
    list.sort_by_key(|r| (r.location, r.length));
    if list.iter().all(|r| r.length == 0) {
        return vec![list[0]];
    }
    let mut out: Vec<NSRange> = Vec::with_capacity(list.len());
    for r in list.into_iter().filter(|r| r.length > 0) {
        match out.last_mut() {
            Some(last) if r.location <= last.location + last.length => {
                let end = (last.location + last.length).max(r.location + r.length);
                last.length = end - last.location;
            }
            _ => out.push(r),
        }
    }
    out
}

fn movement_info(movement: isize) -> Retained<Dict> {
    // SAFETY: the key is a constant string AppKit exports.
    let key = unsafe { objc2_app_kit::NSTextMovementUserInfoKey };
    let value = objc2_foundation::NSNumber::new_isize(movement);
    NSDictionary::from_slices(&[key], &[&*value as &AnyObject])
}

pub(crate) fn offset(r: NSRect, o: NSPoint) -> NSRect {
    NSRect::new(NSPoint::new(r.origin.x + o.x, r.origin.y + o.y), r.size)
}

fn grow(r: NSRect, by: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(r.origin.x - by, r.origin.y - by),
        NSSize::new(r.size.width + 2.0 * by, r.size.height + 2.0 * by),
    )
}

/// Scroll the clip view `view` is inside so `rect` (in `view`'s points)
/// shows, moving as little as it can.
pub(crate) fn scroll_rect_to_visible(view: &NSView, rect: NSRect) {
    // The nearest clip view above, and its document (or the view below it
    // on the way up).
    let mut doc: Retained<NSView> = view.retain();
    let clip = loop {
        // SAFETY: superview takes nothing.
        let Some(sup) = (unsafe { doc.superview() }) else { return };
        if let Ok(clip) = sup.clone().downcast::<NSClipView>() {
            break clip;
        }
        doc = sup;
    };
    let r = doc.convertRect_fromView(rect, Some(view));
    let visible = clip.documentVisibleRect();
    let mut o = visible.origin;
    if r.origin.x < o.x {
        o.x = r.origin.x;
    } else if r.origin.x + r.size.width > o.x + visible.size.width {
        o.x = (r.origin.x + r.size.width - visible.size.width).min(r.origin.x);
    }
    if r.origin.y < o.y {
        o.y = r.origin.y;
    } else if r.origin.y + r.size.height > o.y + visible.size.height {
        o.y = (r.origin.y + r.size.height - visible.size.height).min(r.origin.y);
    }
    if o == visible.origin {
        return;
    }
    let target = clip.constrainBoundsRect(NSRect::new(o, clip.bounds().size));
    clip.scrollToPoint(target.origin);
    // SAFETY: superview takes nothing.
    if let Some(scroll) = unsafe { clip.superview() }.and_then(|s| s.downcast::<NSScrollView>().ok()) {
        scroll.reflectScrolledClipView(&clip);
    }
}

/// The layout manager of the text `view` shows laid text out again
/// (`idle`: in the background).
pub(crate) fn layout_changed(view: &AnyObject, idle: bool) {
    if let Some(tv) = as_impl(view) {
        tv.layout_changed_here(idle);
    }
}

/// The text storage `view` shows changed characters (see
/// [`NSTextViewImpl::storage_edited_here`]).
pub(crate) fn storage_edited(view: &AnyObject, range: NSRange, delta: isize) {
    if let Some(tv) = as_impl(view) {
        tv.storage_edited_here(range, delta);
    }
}

/// The layout manager of `view` took a new text storage: the view shows
/// and edits it from now on.
pub(crate) fn storage_replaced(view: &AnyObject, storage: Option<&NSTextStorage>) {
    if let Some(tv) = as_impl(view) {
        tv.discard_marked_text();
        tv.set_coalescing(None);
        *tv.ivars().storage.borrow_mut() = storage.map(|s| s.retain());
        tv.layout_changed_here(false);
    }
}

/// The typing attributes of `view`, if it is a text view.
pub(crate) fn typing_attributes(view: &AnyObject) -> Option<Retained<Dict>> {
    as_impl(view).map(|tv| tv.typing_attributes_dict())
}

/// `view` as a text view of Sidestep's class (or a subclass).
pub(crate) fn as_impl(view: &AnyObject) -> Option<&NSTextViewImpl> {
    // By name, as programs find it: the class is made on first use, which
    // asking `NSTextViewImpl` for it before then would collide with.
    let ours = <objc2_app_kit::NSTextView as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(view.class(), ours)
        .then(|| unsafe { &*(view as *const AnyObject).cast::<NSTextViewImpl>() })
}

/// A range in a value, for arrays of selected ranges.
pub(crate) fn value_of(r: NSRange) -> Retained<NSValue> {
    // SAFETY: an NSRange is what valueWithRange: takes.
    unsafe { NSValue::valueWithRange(r) }
}

/// The range a value holds (nothing for a value that holds none).
pub(crate) fn range_of(v: &NSValue) -> NSRange {
    // SAFETY: values in selected-range arrays hold ranges.
    unsafe { v.rangeValue() }
}

/// An index range as an `NSRange`.
pub(crate) fn ns(r: Range<usize>) -> NSRange {
    NSRange::new(r.start, r.end.saturating_sub(r.start))
}
