//! `NSAttributedString` and `NSMutableAttributedString`.
//!
//! Both classes share one set of instance variables: the text (a
//! `_SidestepString` for immutable strings, the live `_SidestepAttributedText`
//! backing store for mutable ones, see `backing`), the attribute runs
//! (`runs`) and a `beginEditing` depth. NSMutableAttributedString is a
//! subclass with no instance variables of its own.
//!
//! The primitives read and write that storage. Everything else is written
//! once in `generic`, over the primitives, so subclasses that bring their
//! own storage (as an NSTextStorage does) work as they do on macOS.
//!
//! Immutable attributed strings may be read from any thread: their runs are
//! frozen once initialized and read without the `RefCell`'s borrow count.
//! Mutable ones follow Foundation's single-thread contract, which the
//! `RefCell` enforces.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ops::{Deref, Range};
use std::ptr::NonNull;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringEnumerationOptions, NSDictionary, NSMutableAttributedString, NSMutableString,
    NSRange, NSString, NSUInteger, NSZone,
};

pub(crate) mod backing;
mod dicts;
mod generic;
mod runs;

use generic::{Recv, check, check_index};
use runs::RunList;

/// An attribute dictionary.
pub(crate) type Dict = NSDictionary<NSString, AnyObject>;

sidestep_runtime::static_class!(pub(crate) NSATTRIBUTEDSTRING, NSATTRIBUTEDSTRING_META = "NSAttributedString", || {
    let _ = NSAttributedStringImpl::class();
});

sidestep_runtime::static_class!(
    pub(crate) NSMUTABLEATTRIBUTEDSTRING,
    NSMUTABLEATTRIBUTEDSTRING_META = "NSMutableAttributedString",
    || {
        let _ = NSMutableAttributedStringImpl::class();
    }
);

pub(crate) struct AttrIvars {
    /// Never replaced; a mutable string edits it in place.
    text: Retained<NSString>,
    runs: RefCell<RunList>,
    /// Set for instances of exactly NSAttributedString, whose runs never
    /// change after initialization.
    frozen: bool,
    editing: Cell<usize>,
}

/// Runs being read: straight from a frozen string, or through the cell.
pub(crate) enum RunsGuard<'a> {
    Frozen(&'a RunList),
    Cell(Ref<'a, RunList>),
}

impl Deref for RunsGuard<'_> {
    type Target = RunList;

    fn deref(&self) -> &RunList {
        match self {
            RunsGuard::Frozen(r) => r,
            RunsGuard::Cell(r) => r,
        }
    }
}

impl AttrIvars {
    pub(crate) fn runs(&self) -> RunsGuard<'_> {
        if self.frozen {
            // SAFETY: a frozen string's runs were written during
            // initialization, before the object was shared, and never change
            // again, so reading them without the borrow count is sound on
            // any thread.
            RunsGuard::Frozen(unsafe { &*self.runs.as_ptr() })
        } else {
            RunsGuard::Cell(self.runs.borrow())
        }
    }

    pub(crate) fn runs_mut(&self) -> RefMut<'_, RunList> {
        assert!(!self.frozen, "sidestep: an immutable attributed string was edited");
        self.runs.borrow_mut()
    }
}

impl Drop for AttrIvars {
    fn drop(&mut self) {
        backing::set_owner(&self.text, std::ptr::null());
    }
}

/// The storage of an instance of NSAttributedString or a subclass that was
/// initialized by it.
///
/// # Safety
/// `obj` must be such an instance.
pub(crate) unsafe fn ivars_of(obj: &AnyObject) -> &AttrIvars {
    // SAFETY: guaranteed by the caller.
    unsafe { &*(obj as *const AnyObject).cast::<NSAttributedStringImpl>() }.ivars()
}

/// What `initWithSidestepParts:` takes: the storage of a new string.
struct Parts {
    text: Retained<NSString>,
    runs: RunList,
}

/// Whether `class` is `of` or a subclass of it.
pub(crate) fn is_kind(class: &AnyClass, of: &AnyClass) -> bool {
    let mut c = Some(class);
    while let Some(k) = c {
        if std::ptr::eq(k, of) {
            return true;
        }
        c = k.superclass();
    }
    false
}

fn is_mutable_class(obj: &AnyObject) -> bool {
    is_kind(obj.class(), NSMutableAttributedString::class())
}

/// The text a new attributed string stores for `string`: an immutable copy,
/// or a backing store of its own for a mutable one.
fn stored_text(string: &NSString, mutable: bool) -> Retained<NSString> {
    if mutable {
        backing::new_text(string)
    } else {
        // SAFETY: -copy of a string is an immutable string.
        unsafe { msg_send![string, copy] }
    }
}

/// Set up a new attributed string's storage.
fn finish(this: Allocated<NSAttributedStringImpl>, parts: Parts) -> Retained<NSAttributedStringImpl> {
    // SAFETY: a freshly allocated object.
    let obj = unsafe { &*Allocated::as_ptr(&this).cast::<AnyObject>() };
    let frozen = std::ptr::eq(obj.class(), NSAttributedString::class());
    let this =
        this.set_ivars(AttrIvars { text: parts.text, runs: RefCell::new(parts.runs), frozen, editing: Cell::new(0) });
    // SAFETY: NSObject's initializer.
    let this: Retained<NSAttributedStringImpl> = unsafe { msg_send![super(this), init] };
    let iv = this.ivars();
    backing::set_owner(&iv.text, Retained::as_ptr(&this).cast());
    this
}

/// A new attributed string of `class` (NSAttributedString or
/// NSMutableAttributedString) with the given storage.
fn new_with<T: ClassType + AnyThread>(parts: Parts) -> Retained<T> {
    let mut parts = Some(parts);
    let ptr: *mut Option<Parts> = &mut parts;
    // SAFETY: the private initializer takes the parts it's pointed at.
    unsafe { msg_send![T::alloc(), initWithSidestepParts: ptr.cast::<std::ffi::c_void>()] }
}

/// A new immutable attributed string.
fn new_immutable(text: Retained<NSString>, runs: RunList) -> Retained<NSAttributedString> {
    new_with(Parts { text, runs })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSAttributedString"]
    #[ivars = AttrIvars]
    pub(crate) struct NSAttributedStringImpl;

    impl NSAttributedStringImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let mutable = is_mutable(&this);
            finish(this, Parts { text: stored_text(&crate::string::empty(), mutable), runs: RunList::default() })
        }

        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let mutable = is_mutable(&this);
            let text = stored_text(string, mutable);
            let len = text.length();
            finish(this, Parts { text, runs: RunList::single(len, dicts::empty()) })
        }

        #[unsafe(method_id(initWithString:attributes:))]
        fn init_with_string_attributes(this: Allocated<Self>, string: &NSString, attrs: Option<&Dict>) -> Retained<Self> {
            let mutable = is_mutable(&this);
            let text = stored_text(string, mutable);
            let len = text.length();
            finish(this, Parts { text, runs: RunList::single(len, dicts::copied(attrs)) })
        }

        #[unsafe(method_id(initWithAttributedString:))]
        fn init_with_attributed_string(this: Allocated<Self>, other: &NSAttributedString) -> Retained<Self> {
            let mutable = is_mutable(&this);
            let src = Recv::new(other);
            let runs = src.runs_of(NSRange::new(0, src.len()));
            let text = stored_text(&src.string(), mutable);
            finish(this, Parts { text, runs })
        }

        /// Sidestep's own initializer: `parts` points to an
        /// `Option<Parts>`, which it takes.
        #[unsafe(method_id(initWithSidestepParts:))]
        fn init_with_parts(this: Allocated<Self>, parts: *mut std::ffi::c_void) -> Retained<Self> {
            // SAFETY: only `new_with` sends this, with a valid pointer.
            let parts = unsafe { &mut *parts.cast::<Option<Parts>>() }.take().expect("sidestep: parts");
            let mutable = is_mutable(&this);
            let text = if mutable { backing::new_text(&parts.text) } else { parts.text };
            finish(this, Parts { text, runs: parts.runs })
        }

        // The primitives for reading.

        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.ivars().text.clone()
        }

        /// Neither retained nor autoreleased: the run keeps the dictionary
        /// alive, and callers (objc2's bindings, ARC) retain the result
        /// before anything can change the string.
        #[unsafe(method(attributesAtIndex:effectiveRange:))]
        fn attributes_at_index(&self, index: NSUInteger, range: *mut NSRange) -> *mut Dict {
            let runs = self.ivars().runs();
            check_index("attributesAtIndex:effectiveRange:", index, runs.len());
            let run = runs.at(index);
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = NSRange::new(run.start, run.len) };
            }
            Retained::as_ptr(&run.attrs).cast_mut()
        }

        // Everything else.

        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            let r = Recv::new(self);
            match r.native() {
                Some(iv) => iv.runs().len(),
                None => r.string().length(),
            }
        }

        #[unsafe(method_id(attribute:atIndex:effectiveRange:))]
        fn attribute_at_index(&self, name: &NSString, index: NSUInteger, range: *mut NSRange) -> Option<Retained<AnyObject>> {
            let r = Recv::new(self);
            check_index("attribute:atIndex:effectiveRange:", index, r.len());
            let (attrs, run) = r.attrs_at(index);
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = run };
            }
            attrs.objectForKey(name)
        }

        #[unsafe(method_id(attributesAtIndex:longestEffectiveRange:inRange:))]
        fn attributes_longest(&self, index: NSUInteger, range: *mut NSRange, limit: NSRange) -> Retained<Dict> {
            let r = Recv::new(self);
            check_index("attributesAtIndex:longestEffectiveRange:inRange:", index, r.len());
            check("attributesAtIndex:longestEffectiveRange:inRange:", limit, r.len());
            let (attrs, longest) = r.longest(index, limit);
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = longest };
            }
            attrs
        }

        #[unsafe(method_id(attribute:atIndex:longestEffectiveRange:inRange:))]
        fn attribute_longest(
            &self,
            name: &NSString,
            index: NSUInteger,
            range: *mut NSRange,
            limit: NSRange,
        ) -> Option<Retained<AnyObject>> {
            let r = Recv::new(self);
            check_index("attribute:atIndex:longestEffectiveRange:inRange:", index, r.len());
            check("attribute:atIndex:longestEffectiveRange:inRange:", limit, r.len());
            let (value, longest) = r.longest_value(name, index, limit);
            if !range.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *range = longest };
            }
            value
        }

        #[unsafe(method_id(attributedSubstringFromRange:))]
        fn attributed_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let r = Recv::new(self);
            check("attributedSubstringFromRange:", range, r.len());
            let runs = r.runs_of(range);
            let text = r.string().substringWithRange(range);
            new_immutable(text, runs)
        }

        #[unsafe(method(isEqualToAttributedString:))]
        fn is_equal_to_attributed_string(&self, other: &NSAttributedString) -> bool {
            Recv::new(self).equals(&Recv::new(other))
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            match other.and_then(|o| o.downcast_ref::<NSAttributedString>()) {
                Some(o) => Recv::new(self).equals(&Recv::new(o)),
                None => false,
            }
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            Recv::new(self).string().hash()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSAttributedString> {
            let r = Recv::new(self);
            match r.native() {
                // An immutable string is its own copy.
                Some(iv) if iv.frozen => {
                    // SAFETY: the receiver is an attributed string.
                    unsafe { Retained::cast_unchecked(self.retain()) }
                }
                _ => {
                    // SAFETY: -copy of a string is an immutable string.
                    let text: Retained<NSString> = unsafe { msg_send![&*r.string(), copy] };
                    new_immutable(text, r.runs_of(NSRange::new(0, r.len())))
                }
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableAttributedString> {
            let r = Recv::new(self);
            new_with(Parts { text: r.string(), runs: r.runs_of(NSRange::new(0, r.len())) })
        }

        #[unsafe(method(enumerateAttributesInRange:options:usingBlock:))]
        fn enumerate_attributes(
            &self,
            range: NSRange,
            options: NSAttributedStringEnumerationOptions,
            block: &DynBlock<dyn Fn(NonNull<Dict>, NSRange, NonNull<Bool>)>,
        ) {
            let r = Recv::new(self);
            check("enumerateAttributesInRange:options:usingBlock:", range, r.len());
            let longest = !options.contains(NSAttributedStringEnumerationOptions::LongestEffectiveRangeNotRequired);
            enumerate(&r, range, options, |i, limit| {
                let (attrs, found) = if longest {
                    r.longest(i, limit)
                } else {
                    let (attrs, run) = r.attrs_at(i);
                    let start = run.location.max(limit.location);
                    (attrs, NSRange::new(start, run.end().min(limit.end()) - start))
                };
                let mut stop = Bool::NO;
                block.call((NonNull::from(&*attrs), found, NonNull::from(&mut stop)));
                (found, stop.as_bool())
            });
        }

        #[unsafe(method(enumerateAttribute:inRange:options:usingBlock:))]
        fn enumerate_attribute(
            &self,
            name: &NSString,
            range: NSRange,
            options: NSAttributedStringEnumerationOptions,
            block: &DynBlock<dyn Fn(*mut AnyObject, NSRange, NonNull<Bool>)>,
        ) {
            let r = Recv::new(self);
            check("enumerateAttribute:inRange:options:usingBlock:", range, r.len());
            let longest = !options.contains(NSAttributedStringEnumerationOptions::LongestEffectiveRangeNotRequired);
            enumerate(&r, range, options, |i, limit| {
                let (value, found) = if longest {
                    r.longest_value(name, i, limit)
                } else {
                    let (attrs, run) = r.attrs_at(i);
                    let start = run.location.max(limit.location);
                    (attrs.objectForKey(name), NSRange::new(start, run.end().min(limit.end()) - start))
                };
                let ptr = value.as_ref().map_or(std::ptr::null_mut(), |v| Retained::as_ptr(v).cast_mut());
                let mut stop = Bool::NO;
                block.call((ptr, found, NonNull::from(&mut stop)));
                (found, stop.as_bool())
            });
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let r = Recv::new(self);
            let text = r.string();
            let out = NSMutableString::new();
            let mut i = 0;
            while i < r.len() {
                let (attrs, run) = r.attrs_at(i);
                let start = run.location.max(i);
                out.appendString(&text.substringWithRange(NSRange::new(start, run.end() - start)));
                // SAFETY: every object answers -description.
                let d: Retained<NSString> = unsafe { msg_send![&*attrs, description] };
                out.appendString(&NSString::from_str("{"));
                out.appendString(&d);
                out.appendString(&NSString::from_str("}"));
                i = run.end();
            }
            // SAFETY: an NSMutableString is an NSString.
            unsafe { Retained::cast_unchecked(out) }
        }
    }

    unsafe impl NSObjectProtocol for NSAttributedStringImpl {}
);

fn is_mutable(this: &Allocated<NSAttributedStringImpl>) -> bool {
    // SAFETY: a freshly allocated object.
    is_mutable_class(unsafe { &*Allocated::as_ptr(this).cast::<AnyObject>() })
}

/// Walk `range` forwards or backwards: `step(i, limit)` reports the range
/// found at unit `i` (clipped to `limit`, what is left of `range`) after
/// calling the block, and whether the block asked to stop.
///
/// The block may edit the string inside the range it was passed, even
/// changing its length, as on macOS: the walk goes on after that range, and
/// the end of `range` moves by as many units as the edit added or removed.
/// Going backwards, an edit moves nothing still to visit.
fn enumerate(
    r: &Recv,
    range: NSRange,
    options: NSAttributedStringEnumerationOptions,
    mut step: impl FnMut(usize, NSRange) -> (NSRange, bool),
) {
    let reverse = options.contains(NSAttributedStringEnumerationOptions::Reverse);
    // The text's length, and where `range` ends in it, as of the last step.
    let (mut len, mut end) = (r.len(), range.end());
    let limit = |len: usize, end: usize| NSRange::new(range.location, end.min(len).saturating_sub(range.location));
    // After a step: how far the text grew or shrank, moving `end` with it.
    let moved = |len: &mut usize, end: &mut usize| -> isize {
        let now = r.len();
        let delta = now as isize - *len as isize;
        *len = now;
        *end = end.saturating_add_signed(delta).max(range.location);
        delta
    };
    if reverse {
        let mut i = end.min(len);
        while i > range.location {
            let (found, stop) = step(i - 1, limit(len, end));
            if stop {
                break;
            }
            moved(&mut len, &mut end);
            i = found.location.max(range.location).min(end.min(len));
        }
    } else {
        let mut i = range.location;
        while i < end.min(len) {
            let (found, stop) = step(i, limit(len, end));
            if stop {
                break;
            }
            // The found range holds `i`, so what is left always shrinks.
            let delta = moved(&mut len, &mut end);
            i = found.end().saturating_add_signed(delta).max(found.location);
        }
    }
}

define_class!(
    #[unsafe(super(NSAttributedString, NSObject))]
    #[name = "NSMutableAttributedString"]
    pub(crate) struct NSMutableAttributedStringImpl;

    impl NSMutableAttributedStringImpl {
        // The primitives for writing.

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            // SAFETY: an instance of this class.
            generic::native_replace(unsafe { ivars_of(self) }, range, string);
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&Dict>, range: NSRange) {
            // SAFETY: an instance of this class.
            generic::native_set(unsafe { ivars_of(self) }, range, attrs);
        }

        // Everything else.

        #[unsafe(method_id(mutableString))]
        fn mutable_string(&self) -> Retained<NSMutableString> {
            let r = Recv::new(self);
            match r.native() {
                // SAFETY: a mutable attributed string's text is its
                // backing store, an NSMutableString.
                Some(iv) if backing_is_live(self) => unsafe { Retained::cast_unchecked(iv.text.clone()) },
                _ => backing::proxy(self),
            }
        }

        #[unsafe(method(addAttribute:value:range:))]
        fn add_attribute(&self, name: &NSString, value: &AnyObject, range: NSRange) {
            Recv::new(self).map_attrs("addAttribute:value:range:", range, |d| dicts::with(d, name, value));
        }

        #[unsafe(method(addAttributes:range:))]
        fn add_attributes(&self, attrs: &Dict, range: NSRange) {
            Recv::new(self).map_attrs("addAttributes:range:", range, |d| dicts::merged(d, attrs));
        }

        #[unsafe(method(removeAttribute:range:))]
        fn remove_attribute(&self, name: &NSString, range: NSRange) {
            Recv::new(self).map_attrs("removeAttribute:range:", range, |d| dicts::without(d, name));
        }

        #[unsafe(method(replaceCharactersInRange:withAttributedString:))]
        fn replace_with_attributed(&self, range: NSRange, other: &NSAttributedString) {
            Recv::new(self).replace_attributed(range, other);
        }

        #[unsafe(method(insertAttributedString:atIndex:))]
        fn insert_attributed(&self, other: &NSAttributedString, index: NSUInteger) {
            let r = Recv::new(self);
            if index > r.len() {
                check_index("insertAttributedString:atIndex:", index, r.len());
            }
            r.replace_attributed(NSRange::new(index, 0), other);
        }

        #[unsafe(method(appendAttributedString:))]
        fn append_attributed(&self, other: &NSAttributedString) {
            let r = Recv::new(self);
            r.replace_attributed(NSRange::new(r.len(), 0), other);
        }

        #[unsafe(method(deleteCharactersInRange:))]
        fn delete_characters(&self, range: NSRange) {
            Recv::new(self).replace_chars(range, &crate::string::empty());
        }

        #[unsafe(method(setAttributedString:))]
        fn set_attributed_string(&self, other: &NSAttributedString) {
            let r = Recv::new(self);
            r.replace_attributed(NSRange::new(0, r.len()), other);
        }

        #[unsafe(method(beginEditing))]
        fn begin_editing(&self) {
            if let Some(iv) = Recv::new(self).native() {
                iv.editing.set(iv.editing.get() + 1);
            }
        }

        #[unsafe(method(endEditing))]
        fn end_editing(&self) {
            if let Some(iv) = Recv::new(self).native() {
                iv.editing.set(iv.editing.get().saturating_sub(1));
            }
        }
    }
);

/// Whether edits to this string's backing text reach the attribute runs
/// through Sidestep's own primitives.
fn backing_is_live(obj: &AnyObject) -> bool {
    let r = Recv::new(obj);
    r.native().is_some() && {
        // A subclass that overrides the writing primitives needs the proxy,
        // so it sees edits made through the mutable string.
        let (class, ours) = (obj.class(), NSMutableAttributedString::class());
        std::ptr::eq(class, ours)
            || crate::string::install::keeps(
                class,
                ours,
                &[objc2::sel!(replaceCharactersInRange:withString:), objc2::sel!(setAttributes:range:)],
            )
    }
}

/// A run of an attributed string, for the text engine: its range in UTF-16
/// units and in bytes of the UTF-8 text, and its attributes.
#[doc(hidden)]
pub struct RunRef {
    pub utf16: Range<usize>,
    pub utf8: Range<usize>,
    pub attrs: Retained<NSDictionary<NSString, AnyObject>>,
}

/// Run `f` on an attributed string's text and runs. The text is UTF-8 (a
/// lone surrogate becomes U+FFFD, which keeps byte ranges consistent); the
/// runs are snapshotted, so no borrow is held while `f` runs.
#[doc(hidden)]
pub fn with_runs<R>(s: &NSAttributedString, f: impl FnOnce(&str, &[RunRef]) -> R) -> R {
    let r = Recv::new(s);
    let len = r.len();
    let runs = r.runs_of(NSRange::new(0, len));
    let string = r.string();
    crate::string::with_str(&string, |text| {
        // Map run boundaries to bytes in one pass over the text.
        let mut out = Vec::with_capacity(runs.runs().len());
        let (mut unit, mut byte) = (0, 0);
        let mut chars = text.char_indices().peekable();
        for run in runs.runs() {
            let start_byte = byte;
            while unit < run.end() {
                match chars.next() {
                    Some((_, c)) => {
                        unit += c.len_utf16();
                        byte = chars.peek().map_or(text.len(), |&(b, _)| b);
                    }
                    None => break,
                }
            }
            out.push(RunRef { utf16: run.start..run.end(), utf8: start_byte..byte, attrs: run.attrs.clone() });
        }
        f(text, &out)
    })
}
