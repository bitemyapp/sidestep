//! The string `-[NSTextStorage string]` hands out: live, as AppKit's is.
//!
//! `_SidestepTextStorageString` is an `NSString` subclass that reads its
//! text storage's paragraph tree: `length` is a sum the tree keeps,
//! `characterAtIndex:` finds the paragraph by halving and the unit in it
//! (by index for ASCII and long paragraphs), and `getCharacters:range:` and
//! `substringWithRange:` copy only the range. What programs ask of a text
//! storage's string as the user types, about the text near an edit, is
//! answered from the tree too, costing what that text costs:
//! `paragraphRangeForRange:` and `lineRangeForRange:` (and their
//! `get…Start:end:contentsEnd:forRange:` forms), which read only the
//! paragraphs the range touches (Foundation's and the tree's paragraphs
//! end at the same separators, and a line never crosses one),
//! `rangeOfComposedCharacterSequenceAtIndex:`, which reads a few units
//! around the index, and `hasPrefix:`/`hasSuffix:`, which read the ends.
//! Every other `NSString` method works through the primitives, as for any
//! subclass: it reads the whole text. `UTF8String` is made once per change
//! of the text, and the one it replaces is autoreleased rather than freed,
//! so a pointer handed out earlier lives until the pool drains.
//!
//! The string points back at its storage without retaining it; when the
//! storage goes, it hands the string its text, which the string keeps.

use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
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
                check("getCharacters:range:", range, t.len());
                t.units(range.location..range.location + range.length, &mut units);
            });
            // SAFETY: the caller passes room for `range.length` units, and
            // `units` holds that many.
            unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), buffer.as_ptr(), units.len()) };
        }

        #[unsafe(method_id(substringWithRange:))]
        fn substring_with_range(&self, range: NSRange) -> Retained<NSString> {
            let text = self.with_text(|t| {
                check("substringWithRange:", range, t.len());
                t.text(range.location..range.location + range.length)
            });
            NSString::from_str(&text)
        }

        #[unsafe(method(paragraphRangeForRange:))]
        fn paragraph_range_for_range(&self, range: NSRange) -> NSRange {
            let (s, _, e) = self.paragraphs(range, "paragraphRangeForRange:");
            NSRange::new(s, e - s)
        }

        #[unsafe(method(getParagraphStart:end:contentsEnd:forRange:))]
        fn get_paragraph_start(
            &self,
            start: *mut NSUInteger,
            end: *mut NSUInteger,
            contents_end: *mut NSUInteger,
            range: NSRange,
        ) {
            let found = self.paragraphs(range, "getParagraphStart:end:contentsEnd:forRange:");
            write(start, end, contents_end, found);
        }

        #[unsafe(method(lineRangeForRange:))]
        fn line_range_for_range(&self, range: NSRange) -> NSRange {
            let (s, _, e) = self.lines(range, "lineRangeForRange:");
            NSRange::new(s, e - s)
        }

        #[unsafe(method(getLineStart:end:contentsEnd:forRange:))]
        fn get_line_start(
            &self,
            start: *mut NSUInteger,
            end: *mut NSUInteger,
            contents_end: *mut NSUInteger,
            range: NSRange,
        ) {
            let found = self.lines(range, "getLineStart:end:contentsEnd:forRange:");
            write(start, end, contents_end, found);
        }

        #[unsafe(method(rangeOfComposedCharacterSequenceAtIndex:))]
        fn range_of_composed_character_sequence(&self, index: NSUInteger) -> NSRange {
            // A few units around the index, asked of a string of them;
            // Foundation's answer for the whole text when a sequence might
            // reach past them.
            const AROUND: usize = 32;
            let (from, to, len, window) = self.with_text(|t| {
                let len = t.len();
                if index >= len {
                    check_index(index, len);
                }
                let (mut from, mut to) = (index.saturating_sub(AROUND), (index + AROUND).min(len));
                // Whole characters: no surrogate pair split at either end.
                let low = |i: usize| (0xDC00..0xE000).contains(&t.unit_at(i));
                if from > 0 && low(from) {
                    from -= 1;
                }
                if to < len && low(to) {
                    to += 1;
                }
                (from, to, len, (from > 0 || to < len).then(|| t.text(from..to)))
            });
            let Some(window) = window else {
                // SAFETY: NSString's method, over the primitives.
                return unsafe { msg_send![super(self), rangeOfComposedCharacterSequenceAtIndex: index] };
            };
            let piece = NSString::from_str(&window);
            let r = piece.rangeOfComposedCharacterSequenceAtIndex(index - from);
            let at_edge = (r.location == 0 && from > 0) || (r.location + r.length == to - from && to < len);
            if at_edge {
                // SAFETY: as above.
                return unsafe { msg_send![super(self), rangeOfComposedCharacterSequenceAtIndex: index] };
            }
            NSRange::new(r.location + from, r.length)
        }

        #[unsafe(method(hasPrefix:))]
        fn has_prefix(&self, prefix: &NSString) -> bool {
            self.ends_with(prefix, true)
        }

        #[unsafe(method(hasSuffix:))]
        fn has_suffix(&self, suffix: &NSString) -> bool {
            self.ends_with(suffix, false)
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
    /// The paragraphs `range` touches: start, contents end, end.
    fn paragraphs(&self, range: NSRange, method: &str) -> (usize, usize, usize) {
        self.with_text(|t| {
            check(method, range, t.len());
            let r = super::text_storage::span(t, range.location..range.location + range.length);
            (r.start, contents_end(t, r.clone()), r.end)
        })
    }

    /// The lines `range` touches: the paragraphs', unless one of them holds
    /// a line separator (U+2028) or a next-line (U+0085), in which case a
    /// string of those paragraphs is asked.
    fn lines(&self, range: NSRange, method: &str) -> (usize, usize, usize) {
        let (paras, split) = self.with_text(|t| {
            check(method, range, t.len());
            let r = super::text_storage::span(t, range.location..range.location + range.length);
            let mut split = None;
            let mut breaks = false;
            t.for_each_slice(r.clone(), |s| breaks |= s.contains(['\u{85}', '\u{2028}']));
            if breaks {
                split = Some(t.text(r.clone()));
            }
            ((r.start, contents_end(t, r.clone()), r.end), split)
        });
        let Some(text) = split else { return paras };
        let piece = NSString::from_str(&text);
        let (mut s, mut e, mut c) = (0, 0, 0);
        let local = NSRange::new(range.location - paras.0, range.length);
        // SAFETY: valid pointers, and a range inside the piece.
        unsafe { piece.getLineStart_end_contentsEnd_forRange(&mut s, &mut e, &mut c, local) };
        (paras.0 + s, paras.0 + c, paras.0 + e)
    }

    /// Whether the text starts (`start`) or ends with `other`'s units, as a
    /// literal comparison does. An empty `other` is Foundation's to answer.
    fn ends_with(&self, other: &NSString, start: bool) -> bool {
        let n = other.length();
        if n == 0 {
            // SAFETY: NSString's methods, over the primitives.
            return if start {
                unsafe { msg_send![super(self), hasPrefix: other] }
            } else {
                unsafe { msg_send![super(self), hasSuffix: other] }
            };
        }
        let mut theirs = vec![0u16; n];
        // SAFETY: room for `n` units.
        unsafe { other.getCharacters_range(std::ptr::NonNull::new(theirs.as_mut_ptr()).unwrap(), NSRange::new(0, n)) };
        self.with_text(|t| {
            let len = t.len();
            if n > len {
                return false;
            }
            let mut ours = Vec::with_capacity(n);
            let r = if start { 0..n } else { len - n..len };
            t.units(r, &mut ours);
            ours == theirs
        })
    }

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
fn check(method: &str, range: NSRange, len: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        panic!(
            "-[NSString {method}]: Range {{{}, {}}} out of bounds; string length {len}",
            range.location, range.length
        );
    }
}

#[track_caller]
fn check_index(index: usize, len: usize) {
    panic!("-[NSString rangeOfComposedCharacterSequenceAtIndex:]: Index {index} out of bounds; string length {len}");
}

/// Where the paragraphs `r` end before their last separator.
fn contents_end(t: &Storage, r: std::ops::Range<usize>) -> usize {
    if r.is_empty() {
        return r.end;
    }
    match t.unit_at(r.end - 1) {
        0x0A if r.end - 1 > r.start && t.unit_at(r.end - 2) == 0x0D => r.end - 2,
        0x0A | 0x0D | 0x2029 => r.end - 1,
        _ => r.end,
    }
}

fn write(start: *mut NSUInteger, end: *mut NSUInteger, contents_end: *mut NSUInteger, found: (usize, usize, usize)) {
    for (ptr, value) in [(start, found.0), (end, found.2), (contents_end, found.1)] {
        if !ptr.is_null() {
            // SAFETY: the caller passes valid pointers or null.
            unsafe { *ptr = value };
        }
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
