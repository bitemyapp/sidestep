//! `NSTextContentManager` and `NSTextContentStorage`: a document as
//! elements, for layout managers to lay out.
//!
//! A content storage presents a text storage's paragraphs as
//! `NSTextParagraph`s, made when first asked for and kept, so the same
//! paragraph is the same element (and keeps its layout fragment) until an
//! edit touches it. It is its storage's `textStorageObserver`: each change
//! the storage processes drops the elements of the paragraphs it touched
//! (attributes too: a paragraph's text is its attributes' as well), moves
//! the ones after it, and tells the layout managers.
//!
//! The kept elements are a sequence of paragraphs and gaps over the text
//! (`seq`), so finding the element at an offset, or dropping and moving
//! them after an edit, costs what the edit touched, not the document. An
//! element's own range is brought up to date when asked for, from a log
//! of the edits since it last was (so an edit doesn't visit every element
//! after it); every few hundred edits the kept elements are brought up to
//! date all at once and the log starts again.
//!
//! The delegate's `textContentStorage:textParagraphWithRange:` is asked
//! for each paragraph once, when its element is made, with the
//! paragraph's range, separator included; a paragraph it returns becomes
//! the element over that range. `textContentManager:shouldEnumerateTextElement:options:`
//! is asked of each element enumerated.
//!
//! Enumerating, as measured on macOS (`conformance/tests/textkit2.rs`):
//! forward from a location starts with the element holding it (none from
//! the document's end), and returns the end of the last element given to
//! the block (the one it stopped on included), or the location when there
//! was none, nil in an empty document; in reverse, from the element before
//! the location (holding the unit before it), returning the start of the
//! last element given; in reverse from nil, nothing, returning the start.

use std::cell::{Cell, OnceCell, RefCell};
use std::ops::Range;
use std::ptr::NonNull;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSTextContentManager, NSTextContentManagerEnumerationOptions, NSTextContentStorage, NSTextElement,
    NSTextLayoutManager, NSTextParagraph, NSTextRange, NSTextStorage, NSTextStorageEditActions,
};
use objc2_foundation::{NSArray, NSAttributedString, NSError, NSInteger, NSMutableAttributedString, NSRange, NSString};

use super::SharedWeak;
use super::element::{self, ElementIvars, Live};
use super::location::{self, offset_of, span_of};
use super::seq::{Item, Seq};

sidestep_runtime::static_class!(pub(crate) NSTEXTCONTENTMANAGER, NSTEXTCONTENTMANAGER_META = "NSTextContentManager", || {
    let _ = NSTextContentManagerImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTCONTENTSTORAGE, NSTEXTCONTENTSTORAGE_META = "NSTextContentStorage", || {
    let _ = NSTextContentStorageImpl::class();
});

// Posted on macOS when a storage meets an attribute TextKit 2 doesn't
// support; the name exists here for programs that observe it (nothing
// posts it: every attribute is kept).
sidestep_foundation::constant_string!(
    NSTextContentStorageUnsupportedAttributeAddedNotification =
        "NSTextContentStorageUnsupportedAttributeAddedNotification"
);

/// Edits the log keeps before the kept elements are brought up to date at
/// once.
const LOG_MAX: usize = 256;

pub(crate) struct ManagerIvars {
    managers: RefCell<Vec<Retained<NSTextLayoutManager>>>,
    primary: RefCell<Weak<NSTextLayoutManager>>,
    delegate: RefCell<Weak<AnyObject>>,
    transactions: Cell<usize>,
    sync_managers: Cell<bool>,
    sync_store: Cell<bool>,
    /// The weak reference to itself its elements share.
    me: OnceCell<SharedWeak>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextContentManager"]
    #[ivars = ManagerIvars]
    pub(crate) struct NSTextContentManagerImpl;

    impl NSTextContentManagerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ManagerIvars {
                managers: RefCell::new(Vec::new()),
                primary: RefCell::new(Weak::default()),
                delegate: RefCell::new(Weak::default()),
                transactions: Cell::new(0),
                sync_managers: Cell::new(true),
                sync_store: Cell::new(true),
                me: OnceCell::new(),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *self.ivars().delegate.borrow_mut() = delegate.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method_id(textLayoutManagers))]
        fn text_layout_managers(&self) -> Retained<NSArray<NSTextLayoutManager>> {
            NSArray::from_retained_slice(&self.ivars().managers.borrow())
        }

        #[unsafe(method(addTextLayoutManager:))]
        fn add_text_layout_manager(&self, manager: &NSTextLayoutManager) {
            {
                let mut ms = self.ivars().managers.borrow_mut();
                if ms.iter().any(|m| std::ptr::eq(&**m, manager)) {
                    return;
                }
                ms.push(manager.retain());
            }
            super::layout_manager::attach_content(manager, Some(self.as_object()));
        }

        #[unsafe(method(removeTextLayoutManager:))]
        fn remove_text_layout_manager(&self, manager: &NSTextLayoutManager) {
            let removed = {
                let mut ms = self.ivars().managers.borrow_mut();
                ms.iter().position(|m| std::ptr::eq(&**m, manager)).map(|i| ms.remove(i))
            };
            if let Some(m) = removed {
                let primary = self.ivars().primary.borrow().load();
                if primary.is_some_and(|p| std::ptr::eq(&*p, &*m)) {
                    *self.ivars().primary.borrow_mut() = Weak::default();
                }
                super::layout_manager::attach_content(&m, None);
            }
        }

        #[unsafe(method_id(primaryTextLayoutManager))]
        fn primary_text_layout_manager(&self) -> Option<Retained<NSTextLayoutManager>> {
            self.ivars().primary.borrow().load()
        }

        #[unsafe(method(setPrimaryTextLayoutManager:))]
        fn set_primary_text_layout_manager(&self, manager: Option<&NSTextLayoutManager>) {
            *self.ivars().primary.borrow_mut() = manager.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method(synchronizeTextLayoutManagers:))]
        fn synchronize_text_layout_managers(&self, completion: Option<&DynBlock<dyn Fn(*mut NSError)>>) {
            if let Some(c) = completion {
                c.call((std::ptr::null_mut(),));
            }
        }

        #[unsafe(method(synchronizeToBackingStore:))]
        fn synchronize_to_backing_store(&self, completion: Option<&DynBlock<dyn Fn(*mut NSError)>>) {
            if let Some(c) = completion {
                c.call((std::ptr::null_mut(),));
            }
        }

        /// The elements enumerated from the range's start that start inside
        /// it, up to the first that doesn't (the first too, as on macOS: a
        /// range starting inside an element finds nothing, an empty one
        /// nothing).
        #[unsafe(method_id(textElementsForRange:))]
        fn text_elements_for_range(&self, range: &NSTextRange) -> Retained<NSArray<NSTextElement>> {
            let found = RefCell::new(Vec::<Retained<NSTextElement>>::new());
            // SAFETY: location takes nothing.
            let start: Retained<AnyObject> = unsafe { msg_send![range, location] };
            let block = block2::RcBlock::new(|e: NonNull<NSTextElement>| -> Bool {
                // SAFETY: the element enumerated is alive for the call.
                let e = unsafe { e.as_ref() };
                // SAFETY: elementRange takes nothing.
                let r: Option<Retained<NSTextRange>> = unsafe { msg_send![e, elementRange] };
                let inside = r.is_some_and(|r| {
                    // SAFETY: location takes nothing.
                    let s: Retained<AnyObject> = unsafe { msg_send![&*r, location] };
                    // SAFETY: containsLocation: takes a location.
                    unsafe { msg_send![range, containsLocation: &*s] }
                });
                if !inside {
                    return Bool::NO;
                }
                found.borrow_mut().push(e.retain());
                Bool::YES
            });
            let (from, opts) = (&*start, NSTextContentManagerEnumerationOptions::None);
            let each: &DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool> = &block;
            // SAFETY: the method's own types; a subclass may override it.
            let _: Option<Retained<AnyObject>> =
                unsafe { msg_send![self, enumerateTextElementsFromLocation: from, options: opts, usingBlock: each] };
            drop(block);
            NSArray::from_retained_slice(&found.into_inner())
        }

        #[unsafe(method(hasEditingTransaction))]
        fn has_editing_transaction(&self) -> bool {
            self.ivars().transactions.get() > 0
        }

        #[unsafe(method(performEditingTransactionUsingBlock:))]
        fn perform_editing_transaction(&self, block: &DynBlock<dyn Fn()>) {
            let _t = Transaction::begin(&self.ivars().transactions);
            block.call(());
        }

        #[unsafe(method(recordEditActionInRange:newTextRange:))]
        fn record_edit_action(&self, _old: &NSTextRange, new: &NSTextRange) {
            // The layout managers lay out again what changed.
            if let Some((a, b)) = span_of(new) {
                self.tell_managers(a..b, a..b, 0, true);
            }
        }

        #[unsafe(method(automaticallySynchronizesTextLayoutManagers))]
        fn automatically_synchronizes_text_layout_managers(&self) -> bool {
            self.ivars().sync_managers.get()
        }

        #[unsafe(method(setAutomaticallySynchronizesTextLayoutManagers:))]
        fn set_automatically_synchronizes_text_layout_managers(&self, on: bool) {
            self.ivars().sync_managers.set(on);
        }

        #[unsafe(method(automaticallySynchronizesToBackingStore))]
        fn automatically_synchronizes_to_backing_store(&self) -> bool {
            self.ivars().sync_store.get()
        }

        #[unsafe(method(setAutomaticallySynchronizesToBackingStore:))]
        fn set_automatically_synchronizes_to_backing_store(&self, on: bool) {
            self.ivars().sync_store.set(on);
        }

        // NSTextElementProvider: an empty document, for a manager of no
        // text of its own (subclasses bring theirs).

        #[unsafe(method_id(documentRange))]
        fn document_range(&self) -> Retained<NSTextRange> {
            location::range(0, 0)
        }

        #[unsafe(method_id(enumerateTextElementsFromLocation:options:usingBlock:))]
        fn enumerate_text_elements(
            &self,
            _from: Option<&AnyObject>,
            _options: NSTextContentManagerEnumerationOptions,
            _block: &DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool>,
        ) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(replaceContentsInRange:withTextElements:))]
        fn replace_contents(&self, _range: &NSTextRange, _elements: Option<&NSArray<NSTextElement>>) {}

        #[unsafe(method_id(locationFromLocation:withOffset:))]
        fn location_from_location(&self, from: &AnyObject, offset: NSInteger) -> Option<Retained<AnyObject>> {
            self.offset_location(from, offset)
        }

        #[unsafe(method(offsetFromLocation:toLocation:))]
        fn offset_from_location(&self, from: &AnyObject, to: &AnyObject) -> NSInteger {
            match (offset_of(from), offset_of(to)) {
                (Some(a), Some(b)) => b as isize - a as isize,
                _ => 0,
            }
        }

        #[unsafe(method_id(adjustedRangeFromRange:forEditingTextSelection:))]
        fn adjusted_range(&self, range: &NSTextRange, _editing: bool) -> Option<Retained<NSTextRange>> {
            Some(range.retain())
        }
    }

    unsafe impl NSObjectProtocol for NSTextContentManagerImpl {}
);

/// An editing transaction, open until dropped (a block that unwinds
/// closes it too).
struct Transaction<'a>(&'a Cell<usize>);

impl<'a> Transaction<'a> {
    fn begin(count: &'a Cell<usize>) -> Transaction<'a> {
        count.set(count.get() + 1);
        Transaction(count)
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl NSTextContentManagerImpl {
    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }

    /// The weak reference to itself its elements share.
    fn shared(&self) -> SharedWeak {
        self.ivars().me.get_or_init(|| SharedWeak::new(self.as_object())).clone()
    }

    /// `from` moved by `offset`, inside the document.
    fn offset_location(&self, from: &AnyObject, offset: NSInteger) -> Option<Retained<AnyObject>> {
        // SAFETY: documentRange takes nothing (a subclass says what its
        // document is).
        let doc: Retained<NSTextRange> = unsafe { msg_send![self, documentRange] };
        let (_, len) = span_of(&doc)?;
        let o = offset_of(from)? as isize + offset;
        (0..=len as isize).contains(&o).then(|| location::location(o as usize))
    }

    fn managers(&self) -> Vec<Retained<NSTextLayoutManager>> {
        self.ivars().managers.borrow().clone()
    }

    /// Tell the layout managers the elements over `range` (in the text as
    /// it is now) changed, `exact` of it by `delta` units.
    fn tell_managers(&self, range: Range<usize>, exact: Range<usize>, delta: isize, characters: bool) {
        for m in self.managers() {
            super::layout_manager::content_changed(&m, range.clone(), exact.clone(), delta, characters);
        }
    }
}

/// A kept element, or a gap between them.
enum Slot {
    Gap(usize),
    Element(usize, Retained<AnyObject>),
}

impl Item for Slot {
    fn len(&self) -> usize {
        match self {
            Slot::Gap(n) | Slot::Element(n, _) => *n,
        }
    }
}

/// An edit the kept elements' ranges haven't caught up with.
#[derive(Clone, Copy)]
struct Edit {
    stamp: u64,
    old_end: usize,
    delta: isize,
}

struct Cache {
    slots: Seq<Slot>,
    /// The edit generation, and the edits since `floor`.
    generation: u64,
    log: Vec<Edit>,
}

impl Cache {
    fn new(len: usize) -> Cache {
        Cache { slots: Seq::new(vec![Slot::Gap(len)]), generation: 0, log: Vec::new() }
    }
}

pub(crate) struct StorageIvars {
    storage: RefCell<Option<Retained<NSTextStorage>>>,
    cache: RefCell<Cache>,
    list_markers: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSTextContentManager, NSObject))]
    #[name = "NSTextContentStorage"]
    #[ivars = StorageIvars]
    pub(crate) struct NSTextContentStorageImpl;

    impl NSTextContentStorageImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(StorageIvars {
                storage: RefCell::new(None),
                cache: RefCell::new(Cache::new(0)),
                list_markers: Cell::new(false),
            });
            // SAFETY: NSTextContentManager's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            // A storage of its own, as AppKit's has.
            crate::load_shell::<NSTextStorage>();
            let storage = NSTextStorage::new();
            // SAFETY: the setter below, with a storage.
            let _: () = unsafe { msg_send![&*this, setTextStorage: &*storage] };
            this
        }

        #[unsafe(method_id(textStorage))]
        fn text_storage(&self) -> Option<Retained<NSTextStorage>> {
            self.ivars().storage.borrow().clone()
        }

        #[unsafe(method(setTextStorage:))]
        fn set_text_storage(&self, storage: Option<&NSTextStorage>) {
            let old = self.ivars().storage.replace(storage.map(|s| s.retain()));
            let me = self.as_object();
            if let Some(old) = old {
                // SAFETY: the storage's getter takes nothing.
                let observer: Option<Retained<AnyObject>> = unsafe { msg_send![&*old, textStorageObserver] };
                if observer.is_some_and(|o| std::ptr::eq(&*o, me)) {
                    // SAFETY: the storage's setter takes an observer or nil.
                    let _: () = unsafe { msg_send![&*old, setTextStorageObserver: None::<&AnyObject>] };
                }
            }
            if let Some(s) = storage {
                // SAFETY: the storage's setter takes an observer.
                let _: () = unsafe { msg_send![s, setTextStorageObserver: me] };
            }
            let len = self.text_len();
            self.forget_elements(len);
            for m in self.as_manager().managers() {
                super::layout_manager::content_replaced(&m);
            }
        }

        #[unsafe(method_id(attributedString))]
        fn attributed_string(&self) -> Option<Retained<NSAttributedString>> {
            self.ivars().storage.borrow().clone().map(Retained::into_super).map(Retained::into_super)
        }

        #[unsafe(method(setAttributedString:))]
        fn set_attributed_string(&self, text: Option<&NSAttributedString>) {
            let storage = self.ivars().storage.borrow().clone();
            if let Some(s) = storage {
                let empty;
                let text = match text {
                    Some(t) => t,
                    None => {
                        empty = NSAttributedString::new();
                        &empty
                    }
                };
                s.setAttributedString(text);
            }
        }

        #[unsafe(method_id(attributedStringForTextElement:))]
        fn attributed_string_for_text_element(&self, e: &NSTextElement) -> Option<Retained<NSAttributedString>> {
            self.text_of_element(e)
        }

        /// A paragraph of the text, of no content manager or range yet (as
        /// on macOS).
        #[unsafe(method_id(textElementForAttributedString:))]
        fn text_element_for_attributed_string(&self, text: &NSAttributedString) -> Option<Retained<NSTextElement>> {
            Some(Retained::into_super(element::paragraph_with(Some(text))))
        }

        #[unsafe(method(includesTextListMarkers))]
        fn includes_text_list_markers(&self) -> bool {
            self.ivars().list_markers.get()
        }

        #[unsafe(method(setIncludesTextListMarkers:))]
        fn set_includes_text_list_markers(&self, on: bool) {
            self.ivars().list_markers.set(on);
        }

        #[unsafe(method_id(documentRange))]
        fn document_range(&self) -> Retained<NSTextRange> {
            location::range(0, self.text_len())
        }

        #[unsafe(method_id(enumerateTextElementsFromLocation:options:usingBlock:))]
        fn enumerate_text_elements(
            &self,
            from: Option<&AnyObject>,
            options: NSTextContentManagerEnumerationOptions,
            block: &DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool>,
        ) -> Option<Retained<AnyObject>> {
            self.enumerate_from(from, options, block)
        }

        #[unsafe(method(replaceContentsInRange:withTextElements:))]
        fn replace_contents(&self, range: &NSTextRange, elements: Option<&NSArray<NSTextElement>>) {
            let Some((a, b)) = span_of(range) else { return };
            let Some(storage) = self.ivars().storage.borrow().clone() else { return };
            let text = NSMutableAttributedString::new();
            for e in elements.iter().flat_map(|a| a.iter()) {
                if let Some(t) = self.text_of_element(&e) {
                    text.appendAttributedString(&t);
                }
            }
            storage.replaceCharactersInRange_withAttributedString(NSRange::new(a, b - a), &text);
        }

        #[unsafe(method_id(locationFromLocation:withOffset:))]
        fn location_from_location(&self, from: &AnyObject, offset: NSInteger) -> Option<Retained<AnyObject>> {
            offset_of(from).and_then(|o| {
                let o = o as isize + offset;
                (0..=self.text_len() as isize).contains(&o).then(|| location::location(o as usize))
            })
        }

        #[unsafe(method(offsetFromLocation:toLocation:))]
        fn offset_from_location(&self, from: &AnyObject, to: &AnyObject) -> NSInteger {
            match (offset_of(from), offset_of(to)) {
                (Some(a), Some(b)) => b as isize - a as isize,
                _ => 0,
            }
        }


        // NSTextStorageObserving.

        #[unsafe(method(processEditingForTextStorage:edited:range:changeInLength:invalidatedRange:))]
        fn process_editing(
            &self,
            storage: &NSTextStorage,
            mask: NSTextStorageEditActions,
            range: NSRange,
            delta: NSInteger,
            _invalidated: NSRange,
        ) {
            let ours = self.ivars().storage.borrow().as_ref().is_some_and(|s| std::ptr::eq(&**s, storage));
            if !ours {
                return;
            }
            let characters = mask.contains(NSTextStorageEditActions::EditedCharacters);
            let exact = range.location..range.location + range.length;
            let changed = self.edited(exact.clone(), delta);
            self.as_manager().tell_managers(changed, exact, delta, characters);
        }

        #[unsafe(method(performEditingTransactionForTextStorage:usingBlock:))]
        fn perform_editing_transaction_for_text_storage(&self, _storage: &NSTextStorage, block: &DynBlock<dyn Fn()>) {
            block.call(());
        }
    }

    unsafe impl NSObjectProtocol for NSTextContentStorageImpl {}
);

impl NSTextContentStorageImpl {
    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }

    fn as_manager(&self) -> &NSTextContentManagerImpl {
        // SAFETY: a content storage is a content manager of Sidestep's.
        unsafe { &*(self as *const Self).cast::<NSTextContentManagerImpl>() }
    }

    fn storage(&self) -> Option<Retained<NSTextStorage>> {
        self.ivars().storage.borrow().clone()
    }

    /// An element's text: a paragraph's own, else the storage's over its
    /// range.
    fn text_of_element(&self, e: &NSTextElement) -> Option<Retained<NSAttributedString>> {
        if e.downcast_ref::<NSTextParagraph>().is_some() {
            // SAFETY: attributedString takes nothing.
            let t: Option<Retained<NSAttributedString>> = unsafe { msg_send![e, attributedString] };
            return t;
        }
        let (a, b) = element::span(e)?;
        let storage = self.ivars().storage.borrow().clone()?;
        Some(storage.attributedSubstringFromRange(NSRange::new(a, b - a)))
    }

    /// Enumerating elements, as the method does.
    fn enumerate_from(
        &self,
        from: Option<&AnyObject>,
        options: NSTextContentManagerEnumerationOptions,
        block: &DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool>,
    ) -> Option<Retained<AnyObject>> {
        let reverse = options.contains(NSTextContentManagerEnumerationOptions::Reverse);
        let from = match from {
            Some(l) => Some(offset_of(l)?),
            None => None,
        };
        let end = self.elements(from, reverse, |e, _| {
            // SAFETY: an element, alive for the call.
            let e = unsafe { &*(e as *const AnyObject).cast::<NSTextElement>() };
            block.call((NonNull::from(e),)).as_bool()
        })?;
        Some(location::location(end))
    }

    /// The text's length in UTF-16 units.
    pub(crate) fn text_len(&self) -> usize {
        match self.storage() {
            Some(s) => match native(&s) {
                Some(iv) => iv.text().len(),
                None => s.length(),
            },
            None => 0,
        }
    }

    /// Drop every kept element: a new text.
    fn forget_elements(&self, len: usize) {
        let old = std::mem::replace(&mut *self.ivars().cache.borrow_mut(), Cache::new(len));
        freeze_all(old);
    }

    /// The storage changed `range` (in the text as it is now) by `delta`
    /// units: drop the elements of the paragraphs it touched and move the
    /// ones after them. The paragraphs touched, as they are now.
    fn edited(&self, range: Range<usize>, delta: isize) -> Range<usize> {
        let len = self.text_len();
        let whole = self.paragraphs_touching(range.start.min(len)..range.end.min(len));
        let old_len = (len as isize - delta).max(0) as usize;
        let old_whole = whole.start..((whole.end as isize - delta).max(whole.start as isize) as usize).min(old_len);
        let dropped = {
            let mut cache = self.ivars().cache.borrow_mut();
            let slots = &mut cache.slots;
            if slots.len() != old_len {
                // Out of step (a storage replaced under us): start again.
                None
            } else {
                // The slots the old paragraphs were in (the one holding the
                // start, for an insertion), and gaps either side, replaced
                // by one gap.
                let (mut first, mut count, mut end) = slots.cover(old_whole.start, old_whole.end);
                let mut gone = Vec::new();
                let mut p = first;
                for i in 0..count {
                    if let Slot::Element(_, e) = slots.get(p) {
                        gone.push(e.clone());
                    }
                    if i + 1 < count {
                        p = slots.next(p).expect("counted");
                    }
                }
                let last = p;
                let edit_end = end;
                if let Some(q) = slots.prev(first)
                    && matches!(slots.get(q), Slot::Gap(_))
                {
                    first = q;
                    count += 1;
                }
                if let Some(n) = slots.next(last)
                    && let Slot::Gap(len) = slots.get(n)
                {
                    end += len;
                    count += 1;
                }
                let new_len = ((end - first.start) as isize + delta).max(0) as usize;
                slots.splice(first, count, vec![Slot::Gap(new_len)]);
                if delta != 0 {
                    cache.generation += 1;
                    let stamp = cache.generation;
                    cache.log.push(Edit { stamp, old_end: edit_end, delta });
                }
                Some(gone)
            }
        };
        match dropped {
            Some(gone) => {
                for e in gone {
                    freeze(&e);
                }
                if self.ivars().cache.borrow().log.len() > LOG_MAX {
                    self.catch_up();
                }
            }
            None => self.forget_elements(len),
        }
        whole
    }

    /// Bring every kept element's range up to date and start the log again.
    fn catch_up(&self) {
        let mut cache = self.ivars().cache.borrow_mut();
        let generation = cache.generation;
        cache.slots.for_each(|start, slot| {
            if let Slot::Element(_, e) = slot
                && let Some(iv) = element::ivars(e)
                && let Some(live) = iv.live.get()
            {
                iv.live.set(Some(Live { start, stamp: generation, ..live }));
            }
        });
        cache.log.clear();
    }

    /// The paragraphs `range` touches, whole: those holding its units, or
    /// the one holding its start for an empty range.
    fn paragraphs_touching(&self, range: Range<usize>) -> Range<usize> {
        let Some(storage) = self.storage() else { return range };
        match native(&storage) {
            Some(iv) => crate::textkit::text_storage::span(&iv.text(), range),
            None => {
                let s: Retained<NSString> = storage.string();
                let r = s.paragraphRangeForRange(NSRange::new(range.start, range.len()));
                r.location..r.location + r.length
            }
        }
    }

    /// The paragraph holding `o` (the last one at the text's end), and its
    /// separator's length.
    fn paragraph_at(&self, o: usize) -> (Range<usize>, u8) {
        let Some(storage) = self.storage() else { return (0..0, 0) };
        match native(&storage) {
            Some(iv) => {
                let text = iv.text();
                let r = text.paragraph_range(o.min(text.len()));
                let sep = separator_in(&text, r.clone());
                (r, sep)
            }
            None => {
                let s: Retained<NSString> = storage.string();
                let len = s.length();
                let r = s.paragraphRangeForRange(NSRange::new(o.min(len), 0));
                let range = r.location..r.location + r.length;
                let sub = s.substringWithRange(r);
                (range, element::separator_len(&sub) as u8)
            }
        }
    }

    /// Call `f` with elements from `from` on (or back), as enumerating
    /// does, and return where enumeration ended; `f` returns whether to go
    /// on. `None` for an empty document enumerated forward.
    pub(crate) fn elements(
        &self,
        from: Option<usize>,
        reverse: bool,
        mut f: impl FnMut(&AnyObject, Range<usize>) -> bool,
    ) -> Option<usize> {
        let len = self.text_len();
        let should = self.should_enumerate();
        if reverse {
            let Some(mut at) = from else { return Some(0) };
            at = at.min(len);
            let mut edge = at;
            while at > 0 {
                let (e, r) = self.element_at(at - 1);
                edge = r.start;
                let go =
                    if should.as_ref().is_none_or(|d| self.ask_should(d, &e, true)) { f(&e, r.clone()) } else { true };
                if !go || r.start == 0 {
                    break;
                }
                at = r.start;
            }
            return Some(edge);
        }
        if len == 0 {
            return None;
        }
        let mut at = from.unwrap_or(0).min(len);
        let mut edge = at;
        while at < len {
            let (e, r) = self.element_at(at);
            edge = r.end;
            let go =
                if should.as_ref().is_none_or(|d| self.ask_should(d, &e, false)) { f(&e, r.clone()) } else { true };
            if !go || r.end <= at {
                break;
            }
            at = r.end;
        }
        Some(edge)
    }

    /// The delegate, if it says which elements to enumerate.
    fn should_enumerate(&self) -> Option<Retained<AnyObject>> {
        let d = self.as_manager().ivars().delegate.borrow().load()?;
        let sel: Sel = sel!(textContentManager:shouldEnumerateTextElement:options:);
        crate::textkit::responds(&d, sel).then_some(d)
    }

    fn ask_should(&self, d: &AnyObject, e: &AnyObject, reverse: bool) -> bool {
        let options = if reverse {
            NSTextContentManagerEnumerationOptions::Reverse
        } else {
            NSTextContentManagerEnumerationOptions::None
        };
        // SAFETY: the delegate method takes the manager, an element and the
        // options, and returns BOOL.
        unsafe { msg_send![d, textContentManager: self.as_object(), shouldEnumerateTextElement: e, options: options] }
    }

    /// The element holding offset `o` (< the length), made now if it isn't
    /// kept, and its range: the paragraph's (a delegate's shorter
    /// paragraph's too, as on macOS; its fragment covers less once laid
    /// out).
    pub(crate) fn element_at(&self, o: usize) -> (Retained<AnyObject>, Range<usize>) {
        {
            let mut cache = self.ivars().cache.borrow_mut();
            let p = cache.slots.locate(o);
            if let Slot::Element(len, e) = cache.slots.get(p) {
                return (e.clone(), p.start..p.start + len);
            }
        }
        let (range, sep) = self.paragraph_at(o);
        let e = self.make_element(range.clone());
        let default = is_default(&e);
        let mut cache = self.ivars().cache.borrow_mut();
        let generation = cache.generation;
        let slots = &mut cache.slots;
        let p = slots.locate(range.start);
        // The gap holding the paragraph, cut around it (an element kept
        // meanwhile, the delegate's doing, stays: this one is handed out
        // unkept, as one out of step with the text is).
        let (gs, glen) = (p.start, slots.get(p).len());
        let ge = gs + glen;
        if matches!(slots.get(p), Slot::Gap(_)) && gs <= range.start && range.end <= ge {
            let mut parts = Vec::with_capacity(3);
            if range.start > gs {
                parts.push(Slot::Gap(range.start - gs));
            }
            parts.push(Slot::Element(range.len(), e.clone()));
            if ge > range.end {
                parts.push(Slot::Gap(ge - range.end));
            }
            slots.splice(p, 1, parts);
        }
        drop(cache);
        let live = Live { start: range.start, len: range.len(), stamp: generation, sep, default };
        element::set_live(&e, &self.as_manager().shared(), live);
        (e, range)
    }

    /// A new element for the paragraph over `range`: the delegate's, else
    /// one over the storage's text.
    fn make_element(&self, range: Range<usize>) -> Retained<AnyObject> {
        let delegate = self.as_manager().ivars().delegate.borrow().load();
        let sel: Sel = sel!(textContentStorage:textParagraphWithRange:);
        if let Some(d) = delegate
            && crate::textkit::responds(&d, sel)
        {
            let ns = NSRange::new(range.start, range.len());
            // SAFETY: the delegate method takes the storage and a range and
            // returns a paragraph or nil.
            let p: Option<Retained<NSTextParagraph>> =
                unsafe { msg_send![&*d, textContentStorage: self.as_object(), textParagraphWithRange: ns] };
            if let Some(p) = p {
                return p.into();
            }
        }
        element::storage_paragraph().into()
    }

    /// Bring a kept element's range up to date from the log.
    fn refresh_element(&self, iv: &ElementIvars) {
        let Some(mut live) = iv.live.get() else { return };
        let cache = self.ivars().cache.borrow();
        if live.stamp >= cache.generation {
            return;
        }
        let from = cache.log.partition_point(|e| e.stamp <= live.stamp);
        for e in &cache.log[from..] {
            if live.start >= e.old_end {
                live.start = (live.start as isize + e.delta).max(0) as usize;
            }
        }
        live.stamp = cache.generation;
        iv.live.set(Some(live));
    }

    /// The storage's text over a default element.
    fn text_for(&self, iv: &ElementIvars) -> Option<Retained<NSAttributedString>> {
        self.refresh_element(iv);
        let live = iv.live.get()?;
        let storage = self.storage()?;
        let len = self.text_len();
        let (a, b) = (live.start.min(len), (live.start + live.len).min(len));
        Some(storage.attributedSubstringFromRange(NSRange::new(a, b - a)))
    }
}

/// Whether `e` is Sidestep's paragraph over the storage's text (not a
/// delegate's).
fn is_default(e: &AnyObject) -> bool {
    element::ivars(e).is_some_and(|iv| iv.live.get().is_none_or(|l| l.default)) && element::given_text(e).is_none()
}

/// An element no longer kept keeps the range it last had, as a set one.
fn freeze(e: &AnyObject) {
    if let Some(iv) = element::ivars(e) {
        // SAFETY: elementRange takes nothing; the element is Sidestep's.
        let r: Option<Retained<NSTextRange>> = unsafe { msg_send![e, elementRange] };
        iv.live.set(None);
        // SAFETY: the element's setter.
        let _: () = unsafe { msg_send![e, setElementRange: r.as_deref()] };
    }
}

fn freeze_all(cache: Cache) {
    let mut gone = Vec::new();
    cache.slots.for_each(|_, slot| {
        if let Slot::Element(_, e) = slot {
            gone.push(e.clone());
        }
    });
    drop(cache);
    for e in gone {
        freeze(&e);
    }
}

/// Separator units ending the paragraph over `r` of `text`.
fn separator_in(text: &crate::textkit::storage::Storage, r: Range<usize>) -> u8 {
    if r.is_empty() {
        return 0;
    }
    element::separator_units(text.unit_at(r.end - 1), (r.len() >= 2).then(|| text.unit_at(r.end - 2)))
}

/// The native text of a storage, if Sidestep keeps it.
fn native(s: &NSTextStorage) -> Option<&crate::textkit::text_storage::Ivars> {
    let obj: &AnyObject = s;
    crate::textkit::text_storage::native(obj)
}

/// A content storage of Sidestep's (or a subclass), from a content manager.
/// (The classes are named through their shells, which loads them as the
/// runtime expects, never through `define_class!` types.)
pub(crate) fn as_storage(m: &AnyObject) -> Option<&NSTextContentStorageImpl> {
    let ours = <NSTextContentStorage as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(m.class(), ours)
        .then(|| unsafe { &*(m as *const AnyObject).cast::<NSTextContentStorageImpl>() })
}

/// The weak reference to `manager` its elements share: a content manager
/// of Sidestep's hands out its own; another object gets one of its own.
pub(crate) fn shared(manager: &AnyObject) -> SharedWeak {
    let ours = <NSTextContentManager as ClassType>::class();
    if crate::textkit::is_kind(manager.class(), ours) {
        // SAFETY: an instance of the class or a subclass.
        let m = unsafe { &*(manager as *const AnyObject).cast::<NSTextContentManagerImpl>() };
        return m.shared();
    }
    SharedWeak::new(manager)
}

/// Bring an element's range up to date, if its manager is a content
/// storage keeping it.
pub(crate) fn refresh(manager: &AnyObject, iv: &ElementIvars) {
    if let Some(cs) = as_storage(manager) {
        cs.refresh_element(iv);
    }
}

/// The storage's text over a default element of `manager`.
pub(crate) fn text_of(manager: &AnyObject, iv: &ElementIvars) -> Option<Retained<NSAttributedString>> {
    as_storage(manager)?.text_for(iv)
}

/// Whether a content storage's class enumerates as Sidestep's does (no
/// override), so its layout managers can ask for elements directly.
pub(crate) fn enumerates_natively(cs: &NSTextContentStorageImpl) -> bool {
    let ours = <NSTextContentStorage as ClassType>::class();
    let sel: Sel = sel!(enumerateTextElementsFromLocation:options:usingBlock:);
    crate::textkit::same_method(cs.as_object().class(), ours, sel)
}

impl NSTextContentStorageImpl {
    /// The storage, for the layout managers.
    pub(crate) fn text_storage_now(&self) -> Option<Retained<NSTextStorage>> {
        self.storage()
    }
}
