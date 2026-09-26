//! The string `-[NSTextStorage string]` hands out: live, as AppKit's is.
//!
//! `_SidestepTextStorageString` is an `NSString` subclass that reads its
//! text storage's paragraph tree: `length` is a sum the tree keeps,
//! `characterAtIndex:` finds the paragraph by halving and the unit in it
//! (by index for ASCII and long paragraphs), and `getCharacters:range:` and
//! `substringWithRange:` copy only the range. Every other `NSString` method
//! works through those, as for any subclass. `UTF8String` is made once per
//! change of the text, and the one it replaces is autoreleased rather than
//! freed, so a pointer handed out earlier lives until the pool drains.
//!
//! The string points back at its storage without retaining it; when the
//! storage goes, it hands the string its text, which the string keeps.

use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class};
use objc2_foundation::{NSRange, NSString, NSUInteger, NSZone};

use super::storage::Storage;
use super::text_storage::NSTextStorageImpl;

pub(crate) struct Ivars {
    /// The storage, unretained; null once it has gone.
    owner: Cell<*const NSTextStorageImpl>,
    /// The text, once the storage has gone.
    detached: RefCell<Option<Storage>>,
    /// The last `UTF8String`, and the generation of the text it holds.
    utf8: RefCell<Option<(u64, Retained<NSString>)>>,
}

define_class!(
    #[unsafe(super(NSString, NSObject))]
    #[name = "_SidestepTextStorageString"]
    #[ivars = Ivars]
    pub(crate) struct LiveString;

    impl LiveString {
        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            self.with_text(Storage::len)
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, index: NSUInteger) -> u16 {
            self.with_text(|t| {
                if index >= t.len() {
                    panic!("-[NSString characterAtIndex:]: Range or index out of bounds");
                }
                t.unit_at(index)
            })
        }

        #[unsafe(method(getCharacters:range:))]
        fn get_characters(&self, buffer: NonNull<u16>, range: NSRange) {
            let mut units = Vec::with_capacity(range.length);
            self.with_text(|t| {
                check(range, t.len());
                t.units(range.location..range.location + range.length, &mut units);
            });
            // SAFETY: the caller passes room for `range.length` units, and
            // `units` holds that many.
            unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), buffer.as_ptr(), units.len()) };
        }

        #[unsafe(method_id(substringWithRange:))]
        fn substring_with_range(&self, range: NSRange) -> Retained<NSString> {
            let text = self.with_text(|t| {
                check(range, t.len());
                t.text(range.location..range.location + range.length)
            });
            NSString::from_str(&text)
        }

        #[unsafe(method(UTF8String))]
        fn utf8_string(&self) -> *const c_char {
            let generation = self.with_text(Storage::generation);
            let mut cache = self.ivars().utf8.borrow_mut();
            match cache.as_ref() {
                Some((g, s)) if *g == generation => return s.UTF8String(),
                _ => {}
            }
            let s = NSString::from_str(&self.with_text(Storage::string));
            let ptr = s.UTF8String();
            if let Some((_, old)) = cache.replace((generation, s)) {
                // Pointers into the old text live until the pool drains.
                let _ = Retained::autorelease_ptr(old);
            }
            ptr
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSString> {
            NSString::from_str(&self.with_text(Storage::string))
        }
    }

    unsafe impl NSObjectProtocol for LiveString {}
);

impl LiveString {
    fn with_text<R>(&self, f: impl FnOnce(&Storage) -> R) -> R {
        let owner = self.ivars().owner.get();
        if owner.is_null() {
            let detached = self.ivars().detached.borrow();
            return f(detached.as_ref().expect("sidestep: a detached string keeps its text"));
        }
        // SAFETY: the storage clears the pointer before it goes.
        let owner = unsafe { &*owner };
        f(&owner.ivars().text())
    }
}

#[track_caller]
fn check(range: NSRange, len: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        panic!("-[NSString substringWithRange:]: Range {{{}, {}}} out of bounds; string length {len}", range.location, range.length);
    }
}

/// A live string for `owner`.
pub(crate) fn new(owner: &NSTextStorageImpl) -> Retained<NSString> {
    crate::load_shell::<NSString>();
    let this = LiveString::alloc().set_ivars(Ivars {
        owner: Cell::new(owner),
        detached: RefCell::new(None),
        utf8: RefCell::new(None),
    });
    // SAFETY: NSString's initializer.
    let s: Retained<LiveString> = unsafe { objc2::msg_send![super(this), init] };
    // SAFETY: a subclass of NSString.
    unsafe { Retained::cast_unchecked(s) }
}

/// The storage behind `s` is going: `s` keeps `text` from now on.
pub(crate) fn detach(s: &NSString, text: Storage) {
    // SAFETY: only `new` makes the strings a storage keeps.
    let s = unsafe { &*(s as *const NSString).cast::<LiveString>() };
    *s.ivars().detached.borrow_mut() = Some(text);
    s.ivars().owner.set(std::ptr::null());
}
