//! `NSTextStorage`: a mutable attributed string that tells its layout
//! managers what changed.
//!
//! The text lives in a paragraph tree (`storage`) and the attribute
//! dictionaries in a table of their own (`attrs`), each in its own
//! `RefCell`, borrowed only while Rust code reads or writes them, never
//! while a message goes out. The class answers the four primitives of an
//! attributed string from them, and the non-primitive methods that edits
//! and layout lean on (`length`, adding and removing attributes, replacing
//! with an attributed string) directly too. A subclass that brings its own
//! storage (overriding the primitives, as syntax highlighters do) gets
//! `NSMutableAttributedString`'s generic methods instead, which go through
//! its primitives, as on macOS.
//!
//! Editing follows AppKit, as measured on macOS
//! (`conformance/tests/text_storage.rs`): each primitive edit calls
//! `edited:range:changeInLength:`, which gathers the change (the mask, the
//! edited range in the text as it is now, and the change in length) and,
//! outside `beginEditing`/`endEditing`, calls `processEditing`. That posts
//! `NSTextStorageWillProcessEditingNotification` and asks the delegate's
//! `textStorage:willProcessEditing:…`, fixes attributes over the edited
//! paragraphs (text without a font gets the default one, 12-point
//! Helvetica, and a paragraph takes its first character's paragraph
//! style throughout), posts `NSTextStorageDidProcessEditingNotification`,
//! tells the delegate's `textStorage:didProcessEditing:…`, and then sends
//! each layout manager `processEditingForTextStorage:…`. Edits made
//! meanwhile (the fixing, or a delegate's) join the change being processed;
//! an edit made while the layout managers hear of it (a text view's
//! delegate, told the selection moved, may make one) is processed as a
//! change of its own, at once. A storage made with text has its
//! attributes fixed from the start, quietly.
//!
//! Fixing is lazy, as AppKit's is (`fixesAttributesLazily`, measured in
//! `conformance/tests/text_fixing.rs`): an edit of 65 536 units or more,
//! or any edit while text is left to fix, isn't fixed as it is processed.
//! The edit, through the end of its last paragraph, joins the range left
//! to fix (one range, covering all of it), and the change the delegate and
//! the layout managers hear of covers its paragraphs whole (the layout
//! managers lay out again only what was edited, which they ask for, with
//! what a delegate edited when told the change was processed). The text
//! is fixed when something asks for its attributes
//! (`attributesAtIndex:effectiveRange:`, `attribute:atIndex:…`, the
//! longest effective ranges, and what goes through them) or the layout
//! manager lays it out, quietly, a stretch at a time: from the start of
//! what is left through the index asked about (65 536 units at least) when
//! that is nearer the start, otherwise from the index's paragraph to the
//! end. Fixing notes in the text what it fixed, so text fixed before and
//! unchanged since isn't read again when a stretch covering it is fixed
//! (typing where text is left to fix puts the range back at the edit). A
//! subclass with text of its own fixes at once unless it says it fixes
//! lazily; then `ensureAttributesAreFixedInRange:` is its to call, as on
//! macOS.
//!
//! Longest effective ranges are found by walking the runs out from the
//! index, each way, until attributes differ or the limit is reached, and
//! clipped to the limit as AppKit clips them (measured in
//! `conformance/tests/text_storage.rs`: a limit past the text is no error).
//!
//! The scripting accessors (`font`, `foregroundColor` and their setters)
//! read the first character's attribute and set one over all the text.
//!
//! Text storages may live on any thread (no main-thread state is used),
//! one thread at a time, as Foundation's mutable objects do.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSFont, NSFontAttributeName, NSLayoutManager, NSParagraphStyleAttributeName, NSTextStorage,
    NSTextStorageEditActions,
};
use objc2_foundation::{
    NSAttributedString, NSInteger, NSMutableAttributedString, NSNotFound, NSRange, NSString, NSUInteger,
};

use super::attrs::{self, AttrTable, Dict, EMPTY};
use super::storage::{AttrId, Run, Storage};

/// Edits this long (in UTF-16 units) or longer have their attributes fixed
/// lazily, as on macOS (where 65 535 are fixed as the edit is processed and
/// 65 536 aren't).
const LAZY: usize = 65_536;

/// The least a lazy fix from the start of what is left to fix covers.
const FIX_STEP: usize = 65_536;

sidestep_runtime::static_class!(pub(crate) NSTEXTSTORAGE, NSTEXTSTORAGE_META = "NSTextStorage", || {
    let _ = NSTextStorageImpl::class();
});

sidestep_foundation::constant_string!(
    NSTextStorageWillProcessEditingNotification = "NSTextStorageWillProcessEditingNotification"
);
sidestep_foundation::constant_string!(
    NSTextStorageDidProcessEditingNotification = "NSTextStorageDidProcessEditingNotification"
);

/// The change gathered since the last `processEditing`.
#[derive(Clone, Copy, Debug)]
struct Pending {
    mask: NSUInteger,
    /// The edited range in the text as it is now; its location is
    /// `NSNotFound` when nothing is pending (the length stays as it was,
    /// as on macOS).
    range: NSRange,
    delta: NSInteger,
}

impl Pending {
    const NONE: Pending = Pending { mask: 0, range: NSRange { location: NSNotFound as usize, length: 0 }, delta: 0 };

    /// Take in an edit that replaced `old` (in the text as it was before
    /// it) with `old.length + delta` units.
    fn add(&mut self, mask: NSUInteger, old: NSRange, delta: NSInteger) {
        let (s, e) = (old.location, old.location + old.length);
        let new_end = (e as isize + delta).max(s as isize) as usize;
        if self.mask == 0 {
            *self = Pending { mask, range: NSRange::new(s, new_end - s), delta };
            return;
        }
        // The range gathered so far, moved by this edit: ends inside the
        // edited range go to its edges.
        let map = |x: usize, inside: usize| {
            if x >= e {
                (x as isize + delta) as usize
            } else if x > s {
                inside
            } else {
                x
            }
        };
        let (a, b) = (self.range.location, self.range.location + self.range.length);
        let (a, b) = (map(a, s), map(b, new_end));
        let (a, b) = (a.min(s), b.max(new_end));
        self.mask |= mask;
        self.range = NSRange::new(a, b - a);
        self.delta += delta;
    }
}

pub(crate) struct Ivars {
    text: RefCell<Storage>,
    attrs: RefCell<AttrTable>,
    pending: Cell<Pending>,
    editing: Cell<usize>,
    processing: Cell<bool>,
    /// The change the layout managers are hearing of: what `editedRange`
    /// and the rest answer meanwhile.
    delivering: Cell<Option<Pending>>,
    delegate: RefCell<Weak<AnyObject>>,
    managers: RefCell<Vec<Retained<NSLayoutManager>>>,
    /// The live string `string` hands out, made when first asked for.
    string: RefCell<Option<Retained<NSString>>>,
    /// Text whose attributes are left to fix, as one range covering it all.
    invalid: Cell<Option<(usize, usize)>>,
    /// Fixing lazily: the edits the fixing makes are no change to process.
    fixing: Cell<bool>,
    /// A change widened to whole paragraphs (its fixing put off) as it was
    /// edited, from the widening until the layout managers have heard of
    /// it: edits made meanwhile (a delegate's, told the change was
    /// processed) join it.
    unwidened: Cell<Option<Pending>>,
}

impl Ivars {
    fn new() -> Ivars {
        Ivars {
            text: RefCell::new(Storage::new()),
            attrs: RefCell::new(AttrTable::new()),
            pending: Cell::new(Pending::NONE),
            editing: Cell::new(0),
            processing: Cell::new(false),
            delivering: Cell::new(None),
            delegate: RefCell::new(Weak::default()),
            managers: RefCell::new(Vec::new()),
            string: RefCell::new(None),
            invalid: Cell::new(None),
            fixing: Cell::new(false),
            unwidened: Cell::new(None),
        }
    }

    /// The text, for reading by Rust code that sends no messages while it
    /// holds it.
    pub(crate) fn text(&self) -> std::cell::Ref<'_, Storage> {
        self.text.borrow()
    }

    pub(crate) fn attrs(&self) -> &RefCell<AttrTable> {
        &self.attrs
    }

    fn len(&self) -> usize {
        self.text.borrow().len()
    }

    /// The change `editedMask` and the rest describe: the one gathering, or
    /// the one the layout managers are hearing of.
    fn current(&self) -> Pending {
        let p = self.pending.get();
        if p.mask != 0 { p } else { self.delivering.get().unwrap_or(p) }
    }

    /// The attributes new text at `range` takes: those of the first unit
    /// it replaces, or for an insertion, of the unit before it (the first
    /// unit's, at the start).
    fn inherited(&self, range: NSRange) -> AttrId {
        let text = self.text.borrow();
        let len = text.len();
        let at = if range.length > 0 {
            range.location
        } else if range.location > 0 {
            range.location - 1
        } else if len > 0 {
            0
        } else {
            return EMPTY;
        };
        text.attrs_at(at, false).0
    }
}

impl Drop for Ivars {
    fn drop(&mut self) {
        if let Some(s) = self.string.get_mut().take() {
            super::string_proxy::detach(&s, std::mem::take(self.text.get_mut()));
        }
    }
}

define_class!(
    #[unsafe(super(NSMutableAttributedString, NSAttributedString, NSObject))]
    #[name = "NSTextStorage"]
    #[ivars = Ivars]
    pub(crate) struct NSTextStorageImpl;

    impl NSTextStorageImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Ivars::new());
            // SAFETY: NSMutableAttributedString's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            // SAFETY: the designated initializer, sent so a subclass's runs.
            let this: Retained<Self> = unsafe { msg_send![this, init] };
            sidestep_foundation::with_str(string, |text| {
                *this.ivars().text.borrow_mut() = Storage::with_text(text, EMPTY);
            });
            fix_all_quietly(&this);
            this
        }

        #[unsafe(method_id(initWithString:attributes:))]
        fn init_with_string_attributes(this: Allocated<Self>, string: &NSString, attrs: Option<&Dict>) -> Retained<Self> {
            // SAFETY: the designated initializer, sent so a subclass's runs.
            let this: Retained<Self> = unsafe { msg_send![this, init] };
            let id = attrs::intern(&this.ivars().attrs, attrs);
            sidestep_foundation::with_str(string, |text| {
                *this.ivars().text.borrow_mut() = Storage::with_text(text, id);
            });
            fix_all_quietly(&this);
            this
        }

        #[unsafe(method_id(initWithAttributedString:))]
        fn init_with_attributed_string(this: Allocated<Self>, other: &NSAttributedString) -> Retained<Self> {
            // SAFETY: the designated initializer, sent so a subclass's runs.
            let this: Retained<Self> = unsafe { msg_send![this, init] };
            let iv = this.ivars();
            sidestep_foundation::with_runs(other, |text, refs| {
                let runs = intern_runs(&iv.attrs, refs);
                iv.text.borrow_mut().replace_runs(0..0, text, &runs);
            });
            fix_all_quietly(&this);
            this
        }

        // The primitives.

        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.live_string()
        }

        /// Neither retained nor autoreleased: the attribute table keeps the
        /// dictionary, and callers retain it before anything can change.
        #[unsafe(method(attributesAtIndex:effectiveRange:))]
        fn attributes_at_index(&self, index: NSUInteger, range: *mut NSRange) -> *mut Dict {
            let iv = self.ivars();
            check_index("attributesAtIndex:effectiveRange:", index, iv.len());
            ensure_at(self, index);
            let (id, run) = iv.text.borrow().attrs_at(index, !range.is_null());
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = ns_range(run) };
            }
            Retained::as_ptr(iv.attrs.borrow().dict(id)).cast_mut()
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            let iv = self.ivars();
            check("replaceCharactersInRange:withString:", range, iv.len());
            let id = iv.inherited(range);
            let added = sidestep_foundation::with_str(string, |text| {
                iv.text.borrow_mut().replace(range.location..range.location + range.length, text, id);
                super::storage::utf16_len(text) as isize
            });
            self.edited(NSTextStorageEditActions::EditedCharacters, range, added - range.length as isize);
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&Dict>, range: NSRange) {
            let iv = self.ivars();
            check("setAttributes:range:", range, iv.len());
            let id = attrs::intern(&iv.attrs, attrs);
            iv.text.borrow_mut().set_attrs(range.location..range.location + range.length, id);
            self.edited(NSTextStorageEditActions::EditedAttributes, range, 0);
        }

        // What an attributed string does beyond the primitives, from the
        // tree directly when the receiver keeps it.

        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            match native(self) {
                Some(iv) => iv.len(),
                // SAFETY: NSAttributedString's length, over the primitives.
                None => unsafe { msg_send![super(self), length] },
            }
        }

        #[unsafe(method_id(attribute:atIndex:effectiveRange:))]
        fn attribute_at_index(
            &self,
            name: &NSString,
            index: NSUInteger,
            range: *mut NSRange,
        ) -> Option<Retained<AnyObject>> {
            let Some(iv) = native(self) else {
                // SAFETY: NSAttributedString's method, over the primitives.
                return unsafe { msg_send![super(self), attribute: name, atIndex: index, effectiveRange: range] };
            };
            check_index("attribute:atIndex:effectiveRange:", index, iv.len());
            ensure_at(self, index);
            let (id, run) = iv.text.borrow().attrs_at(index, !range.is_null());
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = ns_range(run) };
            }
            let dict = iv.attrs.borrow().dict(id).clone();
            dict.objectForKey(name)
        }

        /// The longest range is read as the text is, fixing only what the
        /// index needs, as on macOS (where a run beyond it left to fix
        /// ends the range).
        #[unsafe(method(attributesAtIndex:longestEffectiveRange:inRange:))]
        fn attributes_longest(&self, index: NSUInteger, range: *mut NSRange, limit: NSRange) -> *mut Dict {
            let Some(iv) = native(self) else {
                // SAFETY: NSAttributedString's method, over the primitives.
                return unsafe {
                    msg_send![super(self), attributesAtIndex: index, longestEffectiveRange: range, inRange: limit]
                };
            };
            check_index("attributesAtIndex:longestEffectiveRange:inRange:", index, iv.len());
            ensure_at(self, index);
            let id = iv.text.borrow().attrs_at(index, false).0;
            if !range.is_null() {
                // Equal attributes are one dictionary in the table.
                let r = self.longest(index, limit, |other| other == id);
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = r };
            }
            Retained::as_ptr(iv.attrs.borrow().dict(id)).cast_mut()
        }

        #[unsafe(method_id(attribute:atIndex:longestEffectiveRange:inRange:))]
        fn attribute_longest(
            &self,
            name: &NSString,
            index: NSUInteger,
            range: *mut NSRange,
            limit: NSRange,
        ) -> Option<Retained<AnyObject>> {
            let Some(iv) = native(self) else {
                // SAFETY: NSAttributedString's method, over the primitives.
                return unsafe {
                    msg_send![super(self), attribute: name, atIndex: index, longestEffectiveRange: range, inRange: limit]
                };
            };
            check_index("attribute:atIndex:longestEffectiveRange:inRange:", index, iv.len());
            ensure_at(self, index);
            let id = iv.text.borrow().attrs_at(index, false).0;
            let value = iv.attrs.borrow().dict(id).objectForKey(name);
            if !range.is_null() {
                let r = self.longest(index, limit, |other| {
                    let dict = iv.attrs.borrow().dict(other).clone();
                    same_value(dict.objectForKey(name).as_deref(), value.as_deref())
                });
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = r };
            }
            value
        }

        #[unsafe(method(addAttribute:value:range:))]
        fn add_attribute(&self, name: &NSString, value: &AnyObject, range: NSRange) {
            if native(self).is_none() {
                // SAFETY: NSMutableAttributedString's method, over the
                // primitives.
                return unsafe { msg_send![super(self), addAttribute: name, value: value, range: range] };
            }
            self.map_attrs("addAttribute:value:range:", range, |d| attrs::with_value(d, name, Some(value)));
        }

        #[unsafe(method(addAttributes:range:))]
        fn add_attributes(&self, extra: &Dict, range: NSRange) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), addAttributes: extra, range: range] };
            }
            self.map_attrs("addAttributes:range:", range, |d| attrs::merged(d, extra));
        }

        #[unsafe(method(removeAttribute:range:))]
        fn remove_attribute(&self, name: &NSString, range: NSRange) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), removeAttribute: name, range: range] };
            }
            self.map_attrs("removeAttribute:range:", range, |d| attrs::with_value(d, name, None));
        }

        #[unsafe(method(replaceCharactersInRange:withAttributedString:))]
        fn replace_with_attributed(&self, range: NSRange, other: &NSAttributedString) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), replaceCharactersInRange: range, withAttributedString: other] };
            }
            self.replace_attributed(range, other);
        }

        #[unsafe(method(insertAttributedString:atIndex:))]
        fn insert_attributed(&self, other: &NSAttributedString, index: NSUInteger) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), insertAttributedString: other, atIndex: index] };
            }
            if index > self.ivars().len() {
                check_index("insertAttributedString:atIndex:", index, self.ivars().len());
            }
            self.replace_attributed(NSRange::new(index, 0), other);
        }

        #[unsafe(method(appendAttributedString:))]
        fn append_attributed(&self, other: &NSAttributedString) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), appendAttributedString: other] };
            }
            self.replace_attributed(NSRange::new(self.ivars().len(), 0), other);
        }

        #[unsafe(method(setAttributedString:))]
        fn set_attributed_string(&self, other: &NSAttributedString) {
            if native(self).is_none() {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), setAttributedString: other] };
            }
            self.replace_attributed(NSRange::new(0, self.ivars().len()), other);
        }

        #[unsafe(method(beginEditing))]
        fn begin_editing(&self) {
            let iv = self.ivars();
            iv.editing.set(iv.editing.get() + 1);
        }

        #[unsafe(method(endEditing))]
        fn end_editing(&self) {
            let iv = self.ivars();
            let depth = iv.editing.get();
            if depth == 0 {
                return;
            }
            iv.editing.set(depth - 1);
            if depth == 1 && iv.pending.get().mask != 0 && !iv.processing.get() {
                // SAFETY: processEditing takes nothing.
                let _: () = unsafe { msg_send![self, processEditing] };
            }
        }

        // Editing.

        #[unsafe(method(edited:range:changeInLength:))]
        fn edited_range(&self, mask: NSTextStorageEditActions, range: NSRange, delta: NSInteger) {
            let iv = self.ivars();
            if iv.fixing.get() {
                return;
            }
            if mask.contains(NSTextStorageEditActions::EditedCharacters) || delta != 0 {
                shift_invalid(iv, range, delta);
            }
            let mut pending = iv.pending.get();
            pending.add(mask.0, range, delta);
            iv.pending.set(pending);
            if iv.processing.get()
                && let Some(mut edited) = iv.unwidened.get()
            {
                edited.add(mask.0, range, delta);
                iv.unwidened.set(Some(edited));
            }
            if iv.editing.get() == 0 && !iv.processing.get() {
                // SAFETY: processEditing takes nothing.
                let _: () = unsafe { msg_send![self, processEditing] };
            }
        }

        #[unsafe(method(processEditing))]
        fn process_editing(&self) {
            process_editing(self);
        }

        #[unsafe(method(editedMask))]
        fn edited_mask(&self) -> NSTextStorageEditActions {
            NSTextStorageEditActions(self.ivars().current().mask)
        }

        #[unsafe(method(editedRange))]
        fn edited_range_value(&self) -> NSRange {
            self.ivars().current().range
        }

        #[unsafe(method(changeInLength))]
        fn change_in_length(&self) -> NSInteger {
            self.ivars().current().delta
        }

        // Scripting: the first character's font and color, set over all
        // the text.

        #[unsafe(method_id(font))]
        fn font(&self) -> Option<Retained<NSFont>> {
            // SAFETY: the key is a constant string AppKit exports.
            self.first_attribute(unsafe { NSFontAttributeName }).and_then(|f| f.downcast::<NSFont>().ok())
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: Option<&NSFont>) {
            // SAFETY: as above.
            self.set_everywhere(unsafe { NSFontAttributeName }, font.map(|f| f.as_ref()));
        }

        #[unsafe(method_id(foregroundColor))]
        fn foreground_color(&self) -> Option<Retained<objc2_app_kit::NSColor>> {
            // SAFETY: as above.
            let key = unsafe { objc2_app_kit::NSForegroundColorAttributeName };
            self.first_attribute(key).and_then(|c| c.downcast::<objc2_app_kit::NSColor>().ok())
        }

        #[unsafe(method(setForegroundColor:))]
        fn set_foreground_color(&self, color: Option<&objc2_app_kit::NSColor>) {
            // SAFETY: as above.
            let key = unsafe { objc2_app_kit::NSForegroundColorAttributeName };
            self.set_everywhere(key, color.map(|c| c.as_ref()));
        }

        /// Sidestep's own text is fixed lazily, as AppKit's concrete
        /// storage's is; a subclass with text of its own isn't unless it
        /// says so, as on macOS.
        #[unsafe(method(fixesAttributesLazily))]
        fn fixes_attributes_lazily(&self) -> bool {
            native(self).is_some()
        }

        #[unsafe(method(invalidateAttributesInRange:))]
        fn invalidate_attributes_in_range(&self, range: NSRange) {
            invalidate(self, range);
        }

        #[unsafe(method(ensureAttributesAreFixedInRange:))]
        fn ensure_attributes_are_fixed(&self, range: NSRange) {
            ensure(self, range.location..range.location + range.length);
        }

        #[unsafe(method(fixAttributesInRange:))]
        fn fix_attributes_in_range(&self, range: NSRange) {
            fix_attributes(self, range);
        }

        // The delegate and the layout managers.

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *self.ivars().delegate.borrow_mut() = delegate.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method_id(layoutManagers))]
        fn layout_managers(&self) -> Retained<objc2_foundation::NSArray<NSLayoutManager>> {
            let managers = self.ivars().managers.borrow().clone();
            objc2_foundation::NSArray::from_retained_slice(&managers)
        }

        #[unsafe(method(addLayoutManager:))]
        fn add_layout_manager(&self, manager: &NSLayoutManager) {
            {
                let mut managers = self.ivars().managers.borrow_mut();
                if managers.iter().any(|m| std::ptr::eq(&**m, manager)) {
                    return;
                }
                managers.push(manager.retain());
            }
            // SAFETY: setTextStorage: takes a text storage; this is one.
            let _: () = unsafe { msg_send![manager, setTextStorage: self.as_storage()] };
        }

        #[unsafe(method(removeLayoutManager:))]
        fn remove_layout_manager(&self, manager: &NSLayoutManager) {
            let removed = {
                let mut managers = self.ivars().managers.borrow_mut();
                let at = managers.iter().position(|m| std::ptr::eq(&**m, manager));
                at.map(|i| managers.remove(i))
            };
            if let Some(m) = removed {
                // SAFETY: setTextStorage: takes a text storage or nil.
                let _: () = unsafe { msg_send![&*m, setTextStorage: None::<&NSTextStorage>] };
            }
        }

        #[unsafe(method_id(textStorageObserver))]
        fn text_storage_observer(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setTextStorageObserver:))]
        fn set_text_storage_observer(&self, _observer: Option<&AnyObject>) {}
    }

    unsafe impl NSObjectProtocol for NSTextStorageImpl {}
);

impl NSTextStorageImpl {
    /// Attribute `key` of the first character, by message (a subclass may
    /// keep its own text); none in empty text.
    fn first_attribute(&self, key: &NSString) -> Option<Retained<AnyObject>> {
        // SAFETY: length takes nothing.
        let len: NSUInteger = unsafe { msg_send![self, length] };
        if len == 0 {
            return None;
        }
        // SAFETY: an index inside the text, and no range asked for.
        unsafe { msg_send![self, attribute: key, atIndex: 0usize, effectiveRange: std::ptr::null_mut::<NSRange>()] }
    }

    /// Set attribute `key` over all the text (take it away, for none).
    fn set_everywhere(&self, key: &NSString, value: Option<&AnyObject>) {
        // SAFETY: length takes nothing.
        let len: NSUInteger = unsafe { msg_send![self, length] };
        if len == 0 {
            return;
        }
        let all = NSRange::new(0, len);
        match value {
            // SAFETY: the value is an object for the attribute.
            Some(v) => unsafe { msg_send![self, addAttribute: key, value: v, range: all] },
            // SAFETY: the range is the text's.
            None => unsafe { msg_send![self, removeAttribute: key, range: all] },
        }
    }

    /// The live string, made when first asked for.
    fn live_string(&self) -> Retained<NSString> {
        let iv = self.ivars();
        let known = iv.string.borrow().clone();
        known.unwrap_or_else(|| {
            let s = super::string_proxy::new(self);
            *iv.string.borrow_mut() = Some(s.clone());
            s
        })
    }

    fn as_storage(&self) -> &NSTextStorage {
        // SAFETY: NSTextStorage is this class.
        unsafe { &*(self as *const Self).cast::<NSTextStorage>() }
    }

    /// The longest range around `index` whose runs have attributes `alike`
    /// takes as those at the index, within `limit`, read as the text is:
    /// the runs are walked out from the index each way until one that
    /// differs or the limit. What is found is clipped to the limit as on
    /// macOS (measured): to nothing, (0, 0), when it only touches the limit
    /// or misses it; and a limit's end wraps, as AppKit's does, rather than
    /// being checked.
    fn longest(&self, index: usize, limit: NSRange, mut alike: impl FnMut(AttrId) -> bool) -> NSRange {
        let iv = self.ivars();
        let limit_end = limit.location.wrapping_add(limit.length);
        let (id, run) = iv.text.borrow().attrs_at(index, false);
        // Whether each set of attributes met is alike: a walk stops at one
        // not asked about yet, asked about with the text let go, and goes
        // on from there.
        let mut known: Vec<(AttrId, bool)> = vec![(id, true)];
        let known_as = |known: &[(AttrId, bool)], id| known.iter().find(|k| k.0 == id).map(|k| k.1);
        let (mut start, mut end) = (run.start, run.end);
        while end < limit_end {
            let mut ask = None;
            iv.text.borrow().runs_after(end, |r, id| match known_as(&known, id) {
                Some(true) => {
                    end = r.end;
                    end < limit_end
                }
                Some(false) => false,
                None => {
                    ask = Some(id);
                    false
                }
            });
            let Some(id) = ask else { break };
            known.push((id, alike(id)));
        }
        while start > limit.location {
            let mut ask = None;
            iv.text.borrow().runs_before(start, |r, id| match known_as(&known, id) {
                Some(true) => {
                    start = r.start;
                    start > limit.location
                }
                Some(false) => false,
                None => {
                    ask = Some(id);
                    false
                }
            });
            let Some(id) = ask else { break };
            known.push((id, alike(id)));
        }
        sidestep_foundation::clip_to_limit(start..end, limit)
    }

    /// `edited:range:changeInLength:`, sent so a subclass sees it.
    fn edited(&self, mask: NSTextStorageEditActions, range: NSRange, delta: isize) {
        // SAFETY: the method's own types.
        let _: () = unsafe { msg_send![self, edited: mask, range: range, changeInLength: delta] };
    }

    /// Give each run in `range` the attributes `f` makes of its own, each
    /// distinct dictionary once.
    fn map_attrs(&self, method: &str, range: NSRange, f: impl FnMut(&Dict) -> Retained<Dict>) {
        let iv = self.ivars();
        check(method, range, iv.len());
        if range.length == 0 {
            return;
        }
        let r = range.location..range.location + range.length;
        let mut runs: Vec<(Range<usize>, AttrId)> = Vec::new();
        iv.text.borrow().for_each_run(r, |r, id| runs.push((r, id)));
        let map = attrs::map_each(&iv.attrs, runs.iter().map(|r| r.1), f);
        {
            let mut text = iv.text.borrow_mut();
            for (r, id) in runs {
                let new = map.iter().find(|m| m.0 == id).map_or(id, |m| m.1);
                if new != id {
                    text.set_attrs(r, new);
                }
            }
        }
        self.compact_if_wanted();
        self.edited(NSTextStorageEditActions::EditedAttributes, range, 0);
    }

    /// `replaceCharactersInRange:withAttributedString:` on the tree: the
    /// text and its runs in one edit.
    fn replace_attributed(&self, range: NSRange, other: &NSAttributedString) {
        let iv = self.ivars();
        check("replaceCharactersInRange:withAttributedString:", range, iv.len());
        // The argument's text and runs are read before anything changes (it
        // may be this very storage, whose text is then read as a copy), and
        // go in as they are.
        let added = sidestep_foundation::with_runs(other, |text, refs| {
            let runs = intern_runs(&iv.attrs, refs);
            iv.text.borrow_mut().replace_runs(range.location..range.location + range.length, text, &runs);
            runs.iter().map(|r| r.len as usize).sum::<usize>()
        });
        self.compact_if_wanted();
        let mask = if added > 0 {
            NSTextStorageEditActions::EditedCharacters | NSTextStorageEditActions::EditedAttributes
        } else {
            NSTextStorageEditActions::EditedCharacters
        };
        self.edited(mask, range, added as isize - range.length as isize);
    }

    /// Renumber the attribute table once it has grown well past what the
    /// text uses.
    fn compact_if_wanted(&self) {
        let iv = self.ivars();
        if !iv.attrs.borrow().wants_compaction() {
            return;
        }
        let mut used = vec![false; iv.attrs.borrow().len()];
        iv.text.borrow().for_each_attrs(|id| used[id as usize] = true);
        let map = iv.attrs.borrow_mut().compact(&used);
        iv.text.borrow_mut().remap(|id| map[id as usize]);
    }
}

/// The receiver's own storage, if its class answers the primitives with
/// this class's implementations: exactly NSTextStorage, or a subclass
/// that leaves them alone.
pub(crate) fn native(obj: &AnyObject) -> Option<&Ivars> {
    let class = obj.class();
    let ours = NSTextStorageImpl::class();
    let keeps = std::ptr::eq(class, ours) || keeps_primitives(class, ours);
    // SAFETY: a class that keeps the primitives is NSTextStorage or a
    // subclass, which has this class's instance variables.
    keeps.then(|| unsafe { &*(obj as *const AnyObject).cast::<NSTextStorageImpl>() }.ivars())
}

/// Whether `class` is a subclass of `ours` that answers the primitives
/// with `ours`'s methods. Remembered for the last class asked about on
/// each thread.
fn keeps_primitives(class: &AnyClass, ours: &AnyClass) -> bool {
    thread_local!(static LAST: Cell<(usize, bool)> = const { Cell::new((0, false)) });
    let key = class as *const AnyClass as usize;
    let (last, answer) = LAST.with(Cell::get);
    if last == key {
        return answer;
    }
    let primitives: [Sel; 4] = [
        sel!(string),
        sel!(attributesAtIndex:effectiveRange:),
        sel!(replaceCharactersInRange:withString:),
        sel!(setAttributes:range:),
    ];
    let answer = crate::textkit::is_kind(class, ours)
        && primitives.iter().all(|&s| match (class.instance_method(s), ours.instance_method(s)) {
            (Some(a), Some(b)) => std::ptr::fn_addr_eq(a.implementation(), b.implementation()),
            _ => false,
        });
    LAST.with(|c| c.set((key, answer)));
    answer
}

/// An attributed string's runs, their attributes interned in `table`:
/// each dictionary is looked for among the last few seen, by address and
/// then by `isEqual:` (runs made one at a time, as a highlighter makes them,
/// often hold equal dictionaries that are different objects), before the
/// table.
fn intern_runs(table: &RefCell<AttrTable>, refs: &[sidestep_foundation::RunRef]) -> Vec<Run> {
    const RECENT: usize = 4;
    let mut recent: Vec<(Retained<Dict>, AttrId)> = Vec::with_capacity(RECENT);
    let mut out: Vec<Run> = Vec::with_capacity(refs.len());
    for r in refs {
        let d = &r.attrs;
        let known = recent.iter().position(|(k, _)| Retained::as_ptr(k) == Retained::as_ptr(d)).or_else(|| {
            recent.iter().position(|(k, _)| {
                // SAFETY: isEqual: takes an object.
                unsafe { msg_send![&**k, isEqual: &**d] }
            })
        });
        let id = match known {
            Some(i) => {
                let hit = recent.remove(i);
                let id = hit.1;
                recent.push((d.clone(), id));
                id
            }
            None => {
                let id = attrs::intern(table, Some(d));
                if recent.len() == RECENT {
                    recent.remove(0);
                }
                recent.push((d.clone(), id));
                id
            }
        };
        let len = r.utf16.len() as u32;
        match out.last_mut() {
            Some(last) if last.attrs == id => last.len += len,
            _ if len == 0 => {}
            _ => out.push(Run { len, attrs: id }),
        }
    }
    out
}

fn ns_range(r: Range<usize>) -> NSRange {
    NSRange::new(r.start, r.end - r.start)
}

#[track_caller]
fn check(method: &str, range: NSRange, len: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        panic!(
            "-[NSTextStorage {method}]: Range {{{}, {}}} out of bounds; string length {len}",
            range.location, range.length
        );
    }
}

#[track_caller]
fn check_index(method: &str, i: usize, len: usize) {
    if i >= len {
        panic!("-[NSTextStorage {method}]: index {i} out of bounds; string length {len}");
    }
}

/// Send `sel` to the delegate, if it has it, with the storage and the
/// change.
fn tell_delegate(this: &NSTextStorageImpl, sel: Sel, p: Pending) {
    let Some(delegate) = this.ivars().delegate.borrow().load() else { return };
    // SAFETY: respondsToSelector: takes a selector.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel] };
    if responds {
        let args = (this.as_storage(), NSTextStorageEditActions(p.mask), p.range, p.delta);
        // SAFETY: the delegate methods take the storage, the mask, the range
        // and the change in length, and return nothing.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*delegate, sel, args) };
    }
}

fn process_editing(this: &NSTextStorageImpl) {
    let iv = this.ivars();
    if iv.processing.replace(true) {
        return;
    }
    // SAFETY: the names are constant strings this module exports.
    let (will, did) = unsafe {
        (
            objc2_app_kit::NSTextStorageWillProcessEditingNotification,
            objc2_app_kit::NSTextStorageDidProcessEditingNotification,
        )
    };
    // An outer change the layout managers are hearing of keeps its own.
    let outer_unwidened = iv.unwidened.replace(None);
    let object: &AnyObject = this;
    sidestep_foundation::notification_center::post(will, Some(object), None);
    tell_delegate(this, sel!(textStorage:willProcessEditing:range:changeInLength:), iv.pending.get());
    let p = iv.pending.get();
    if p.mask != 0 && p.range.location != NSNotFound as usize {
        // SAFETY: invalidateAttributesInRange: takes a range; a subclass
        // may override it.
        let _: () = unsafe { msg_send![this, invalidateAttributesInRange: p.range] };
        // Fixing put off: the change covers its paragraphs whole.
        if iv.invalid.get().is_some() {
            let whole = paragraph_span(this, p.range);
            let mut now = iv.pending.get();
            let (a, b) = (
                now.range.location.min(whole.location),
                (now.range.location + now.range.length).max(whole.location + whole.length),
            );
            if (a, b - a) != (now.range.location, now.range.length) {
                iv.unwidened.set(Some(now));
            }
            now.range = NSRange::new(a, b - a);
            iv.pending.set(now);
        }
    }
    sidestep_foundation::notification_center::post(did, Some(object), None);
    tell_delegate(this, sel!(textStorage:didProcessEditing:range:changeInLength:), iv.pending.get());
    let p = iv.pending.get();
    let managers = iv.managers.borrow().clone();
    // The layout managers hear of the change as one gone by: an edit made
    // meanwhile starts a change of its own, processed at once.
    iv.pending.set(Pending { range: NSRange::new(NSNotFound as usize, p.range.length), ..Pending::NONE });
    iv.processing.set(false);
    if p.mask != 0 {
        let outer = iv.delivering.replace(Some(p));
        let invalidated = p.range;
        for m in &managers {
            m.processEditingForTextStorage_edited_range_changeInLength_invalidatedRange(
                this.as_storage(),
                NSTextStorageEditActions(p.mask),
                p.range,
                p.delta,
                invalidated,
            );
        }
        iv.delivering.set(outer);
    }
    iv.unwidened.set(outer_unwidened);
}

/// The range a change the layout managers are hearing of edited, before it
/// was widened to whole paragraphs (with what was edited after the
/// widening): what they lay out again, and move selections and temporary
/// attributes by. `range` is the change's.
pub(crate) fn edited_range(storage: &NSTextStorage, range: NSRange) -> NSRange {
    let obj: &AnyObject = storage;
    native(obj).and_then(|iv| iv.unwidened.get()).map_or(range, |p| p.range)
}

/// The paragraphs `range` touches, whole.
fn paragraph_span(this: &NSTextStorageImpl, range: NSRange) -> NSRange {
    match native(this) {
        Some(iv) => {
            let text = iv.text.borrow();
            let r = span(&text, range.location..range.location + range.length);
            ns_range(r)
        }
        None => {
            // SAFETY: the primitive.
            let string: Retained<NSString> = unsafe { msg_send![this, string] };
            let len = string.length();
            let r = NSRange::new(range.location.min(len), range.length.min(len - range.location.min(len)));
            string.paragraphRangeForRange(r)
        }
    }
}

/// The paragraphs of `text` that `range` touches, whole.
pub(crate) fn span(text: &Storage, range: Range<usize>) -> Range<usize> {
    let len = text.len();
    let start = text.paragraph_range(range.start.min(len)).start;
    let end = if range.end > range.start {
        text.paragraph_range((range.end - 1).min(len)).end
    } else {
        text.paragraph_range(range.start.min(len)).end
    };
    start..end.max(start)
}

/// The font text without one gets: 12-point Helvetica, as on macOS.
fn default_font() -> Retained<NSFont> {
    thread_local!(static FONT: RefCell<Option<Retained<NSFont>>> = const { RefCell::new(None) });
    FONT.with(|f| {
        f.borrow_mut()
            .get_or_insert_with(|| {
                NSFont::fontWithName_size(&NSString::from_str("Helvetica"), 12.0)
                    .unwrap_or_else(|| NSFont::systemFontOfSize(12.0))
            })
            .clone()
    })
}

/// Fix the attributes of the paragraphs `range` touches: a font where
/// there is none, and each paragraph's first paragraph style throughout.
fn fix_attributes(this: &NSTextStorageImpl, range: NSRange) {
    let r = range.location..range.location + range.length;
    match native(this) {
        Some(iv) => fix_native(this, iv, r.clone(), true, true),
        None => fix_generic(this, range),
    }
    // What is left to fix, less what was fixed at either end of it.
    let iv = this.ivars();
    if let Some((s, e)) = iv.invalid.get() {
        let whole = paragraph_span(this, range);
        let (a, b) = (whole.location, whole.location + whole.length);
        let left = if a <= s && b >= e {
            None
        } else if a <= s && b > s {
            Some((b, e))
        } else if b >= e && a < e {
            Some((s, a))
        } else {
            Some((s, e))
        };
        iv.invalid.set(left);
    }
}

/// Fix all of a new storage's text, telling no one: nothing has been
/// edited. Long text is left to fix lazily, as on macOS.
fn fix_all_quietly(this: &NSTextStorageImpl) {
    if let Some(iv) = native(this) {
        let len = iv.len();
        if len >= LAZY {
            iv.invalid.set(Some((0, len)));
        } else {
            fix_native(this, iv, 0..len, true, false);
        }
    }
}

/// The union of what is left to fix and `range`.
fn hull(invalid: Option<(usize, usize)>, range: Range<usize>) -> (usize, usize) {
    match invalid {
        Some((a, b)) => (a.min(range.start), b.max(range.end)),
        None => (range.start, range.end),
    }
}

/// An edit replaced `old` (in the text before it) with `old.length + delta`
/// units: what is left to fix moves with the text after it, and an end
/// inside the edit goes to the edit's edge.
fn shift_invalid(iv: &Ivars, old: NSRange, delta: isize) {
    let Some((a, b)) = iv.invalid.get() else { return };
    let (s, e) = (old.location, old.location + old.length);
    let new_end = (e as isize + delta).max(s as isize) as usize;
    let map = |x: usize, inside: usize| {
        if x >= e {
            (x as isize + delta) as usize
        } else if x > s {
            inside
        } else {
            x
        }
    };
    let (a, b) = (map(a, s), map(b, new_end));
    iv.invalid.set((a < b).then_some((a, b)));
}

/// `invalidateAttributesInRange:`: Sidestep's own text is fixed at once
/// unless the edit is long or text is left to fix already, when the range
/// through the end of its last paragraph joins what is left (measured: the
/// fixing that follows starts at the edit, not its paragraph); a
/// subclass's is fixed at once, through `fixAttributesInRange:`, unless it
/// fixes lazily.
fn invalidate(this: &NSTextStorageImpl, range: NSRange) {
    let iv = this.ivars();
    let r = range.location..range.location + range.length;
    let lazy = match native(this) {
        Some(_) => range.length >= LAZY || iv.invalid.get().is_some(),
        // SAFETY: fixesAttributesLazily takes nothing.
        None => unsafe { msg_send![this, fixesAttributesLazily] },
    };
    if lazy {
        let whole = paragraph_span(this, range);
        let end = whole.location + whole.length;
        iv.invalid.set(Some(hull(iv.invalid.get(), range.location.min(end)..end)));
    } else if let Some(iv) = native(this) {
        fix_native(this, iv, r, true, true);
    } else {
        // SAFETY: fixAttributesInRange: takes a range; the subclass may
        // override it.
        let _: () = unsafe { msg_send![this, fixAttributesInRange: range] };
    }
}

/// Fix the attributes left to fix that index `i` needs, before they are
/// read.
fn ensure_at(this: &NSTextStorageImpl, i: usize) {
    if this.ivars().invalid.get().is_some() {
        ensure(this, i..i + 1);
    }
}

/// Fix what `range` needs of the text left to fix, a stretch at a time,
/// as AppKit does (measured): asked about nearer the start of what is left
/// than its end, from that start through the range, 65 536 units at least;
/// otherwise from the paragraph holding the range's start to the end.
fn ensure(this: &NSTextStorageImpl, range: Range<usize>) {
    let iv = this.ivars();
    let Some((s, e)) = iv.invalid.get() else { return };
    let end = range.end.max(range.start + 1);
    if end <= s || range.start >= e {
        return;
    }
    let q = range.start.max(s);
    let fix = if q - s < e - q {
        s..end.max(s + FIX_STEP).min(e)
    } else {
        paragraph_span(this, NSRange::new(q, 0)).location.max(s)..e
    };
    iv.invalid.set(if fix.start > s { Some((s, fix.start)) } else { (fix.end < e).then_some((fix.end, e)) });
    match native(this) {
        Some(iv) => fix_native(this, iv, fix, false, false),
        None => {
            // The fixing's edits are no change to process.
            let outer = iv.fixing.replace(true);
            // SAFETY: fixAttributesInRange: takes a range; the subclass may
            // override it.
            let _: () = unsafe { msg_send![this, fixAttributesInRange: ns_range(fix)] };
            iv.fixing.set(outer);
        }
    }
}

/// Fix the attributes of `range` of `storage` left to fix: the layout
/// manager's call before it lays text out.
pub(crate) fn ensure_fixed(storage: &NSTextStorage, range: Range<usize>) {
    let obj: &AnyObject = storage;
    if native(obj).is_some_and(|iv| iv.invalid.get().is_some()) {
        // SAFETY: a storage that keeps the primitives is this class or a
        // subclass.
        let this = unsafe { &*(obj as *const AnyObject).cast::<NSTextStorageImpl>() };
        ensure(this, range);
    }
}

/// What fixing needs to know of a dictionary.
struct Facts {
    has_font: bool,
    style: Option<Retained<AnyObject>>,
}

fn facts(dict: &Dict) -> Facts {
    // SAFETY: the keys are constant strings AppKit exports.
    let (font, style) = unsafe { (NSFontAttributeName, NSParagraphStyleAttributeName) };
    Facts { has_font: dict.objectForKey(font).is_some(), style: dict.objectForKey(style) }
}

/// Whether two attribute values are equal (or both missing).
fn same_value(a: Option<&AnyObject>, b: Option<&AnyObject>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            // SAFETY: isEqual: takes an object.
            std::ptr::eq(a, b) || unsafe { msg_send![a, isEqual: b] }
        }
        _ => false,
    }
}

/// The dictionary a run should have: `dict` with a font if it has none and
/// with `style` as its paragraph style.
fn fixed(dict: &Dict, f: &Facts, style: Option<&AnyObject>) -> Option<Retained<Dict>> {
    let restyle = !same_value(f.style.as_deref(), style);
    if f.has_font && !restyle {
        return None;
    }
    // SAFETY: the keys are constant strings AppKit exports.
    let (font_key, style_key) = unsafe { (NSFontAttributeName, NSParagraphStyleAttributeName) };
    let mut out = dict.retain();
    if restyle {
        out = attrs::with_value(&out, style_key, style);
    }
    if !f.has_font {
        let font = default_font();
        out = attrs::with_value(&out, font_key, Some(&font));
    }
    Some(out)
}

/// Fix the attributes of `range` (with `whole`, of the paragraphs it
/// touches, whole): a font where there is none, and each paragraph's first
/// paragraph style throughout. `edit`: tell of the change as an edit (when
/// processing one).
fn fix_native(this: &NSTextStorageImpl, iv: &Ivars, range: Range<usize>, whole: bool, edit: bool) {
    // The runs, each with its paragraph's first attributes, read with no
    // message out (a stretch of paragraphs with one set of attributes
    // throughout as one run).
    let mut runs: Vec<(Range<usize>, AttrId, AttrId)> = Vec::new();
    let range = {
        let text = iv.text.borrow();
        let r = if whole { span(&text, range) } else { range };
        text.for_each_run_with_first(r.clone(), |r, id, first| runs.push((r, id, first)));
        r
    };
    if runs.is_empty() {
        return;
    }
    // What each distinct dictionary holds, found with the text let go.
    let mut known: Vec<(AttrId, Retained<Dict>, Facts)> = Vec::new();
    for &(_, id, first) in &runs {
        for id in [id, first] {
            if !known.iter().any(|k| k.0 == id) {
                let dict = iv.attrs.borrow().dict(id).clone();
                let f = facts(&dict);
                known.push((id, dict, f));
            }
        }
    }
    let find = |id: AttrId| known.iter().find(|k| k.0 == id).expect("found above");
    let mut changes: Vec<(Range<usize>, AttrId)> = Vec::new();
    // Each pair's new attributes, and whether its paragraph style changed.
    let mut memo: Vec<((AttrId, AttrId), AttrId, bool)> = Vec::new();
    let mut restyled_to = 0;
    for (r, id, first) in runs {
        let (target, restyled) = if let Some(&(_, new, restyled)) = memo.iter().find(|m| m.0 == (id, first)) {
            (new, restyled)
        } else {
            let (_, dict, f) = find(id);
            let style = find(first).2.style.clone();
            let restyled = !same_value(f.style.as_deref(), style.as_deref());
            let new = match fixed(dict, f, style.as_deref()) {
                Some(d) => attrs::intern(&iv.attrs, Some(&d)),
                None => id,
            };
            memo.push(((id, first), new, restyled));
            (new, restyled)
        };
        if target != id {
            if restyled {
                restyled_to = restyled_to.max(r.end);
            }
            match changes.last_mut() {
                Some(last) if last.1 == target && last.0.end == r.start => last.0.end = r.end,
                _ => changes.push((r, target)),
            }
        }
    }
    if changes.is_empty() {
        iv.text.borrow_mut().mark_fixed(range);
        return;
    }
    let (lo, mut hi) = (changes[0].0.start, changes[changes.len() - 1].0.end);
    // A paragraph given its first style again is edited through its end,
    // as on macOS.
    if restyled_to > 0 {
        hi = hi.max(iv.text.borrow().paragraph_range(restyled_to - 1).end);
    }
    {
        let mut text = iv.text.borrow_mut();
        for (r, id) in changes {
            text.set_attrs(r, id);
        }
        // Fixed now, so fixing again needn't read it (until it changes).
        text.mark_fixed(range);
    }
    this.compact_if_wanted();
    if edit {
        this.edited(NSTextStorageEditActions::EditedAttributes, NSRange::new(lo, hi - lo), 0);
    }
}

/// Fixing for a subclass with storage of its own, through its primitives.
fn fix_generic(this: &NSTextStorageImpl, range: NSRange) {
    let whole = paragraph_span(this, range);
    // SAFETY: the primitive.
    let string: Retained<NSString> = unsafe { msg_send![this, string] };
    let mut at = whole.location;
    let end = whole.location + whole.length;
    while at < end {
        let para = string.paragraphRangeForRange(NSRange::new(at, 0));
        let para_end = (para.location + para.length).min(end).max(at + 1);
        let mut r = NSRange::new(0, 0);
        let out: *mut NSRange = &mut r;
        // SAFETY: the primitive, with a valid out-parameter.
        let first: Retained<Dict> = unsafe { msg_send![this, attributesAtIndex: at, effectiveRange: out] };
        let style = facts(&first).style;
        let mut i = at;
        while i < para_end {
            // SAFETY: as above.
            let dict: Retained<Dict> = unsafe { msg_send![this, attributesAtIndex: i, effectiveRange: out] };
            let run_end = (r.location + r.length).min(para_end);
            if let Some(new) = fixed(&dict, &facts(&dict), style.as_deref()) {
                // SAFETY: the primitive.
                let _: () = unsafe { msg_send![this, setAttributes: &*new, range: NSRange::new(i, run_end - i)] };
            }
            i = run_end;
        }
        at = para_end;
    }
}
