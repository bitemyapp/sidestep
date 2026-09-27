//! `NSCharacterSet` and `NSMutableCharacterSet`.
//!
//! A set is one of the predefined Unicode classes, a list of ranges, or the
//! inverse of a set. Membership is by code point (`longCharacterIsMember:`);
//! `characterIsMember:` asks about a single UTF-16 unit, so a surrogate is a
//! member only if the set holds that surrogate code point itself. The
//! predefined sets follow Foundation's documented definitions (general
//! categories plus a few listed characters) over ICU4X's Unicode data, and
//! are immortal singletons. Mutable sets turn into ranges on their first
//! change.
//!
//! Sidestep's string methods read a native set directly through
//! [`membership`]; any other class is asked with `longCharacterIsMember:`.

use std::cell::{Ref, RefCell};
use std::ffi::c_void;
use std::sync::OnceLock;

use icu_normalizer::properties::{CanonicalDecompositionBorrowed, Decomposed};
use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{NSCharacterSet, NSData, NSMutableCharacterSet, NSRange, NSString, NSUInteger, NSZone};

use crate::string::view::view;
use crate::string::wtf8;

sidestep_runtime::static_class!(pub(crate) NSCHARACTERSET, NSCHARACTERSET_META = "NSCharacterSet", || {
    let _ = NSCharacterSetImpl::class();
});

sidestep_runtime::static_class!(
    pub(crate) NSMUTABLECHARACTERSET,
    NSMUTABLECHARACTERSET_META = "NSMutableCharacterSet",
    || {
        let _ = NSMutableCharacterSetImpl::class();
    }
);

const MAX: u32 = 0x10_FFFF;

/// The predefined sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Control,
    Whitespace,
    WhitespaceAndNewline,
    DecimalDigit,
    Letter,
    Lowercase,
    Uppercase,
    NonBase,
    Alphanumeric,
    Decomposable,
    Illegal,
    Punctuation,
    Capitalized,
    Symbol,
    Newline,
    UrlFragment,
    UrlHost,
    UrlPassword,
    UrlPath,
    UrlQuery,
    UrlUser,
}

impl Kind {
    const ALL: [Kind; 21] = [
        Kind::Control,
        Kind::Whitespace,
        Kind::WhitespaceAndNewline,
        Kind::DecimalDigit,
        Kind::Letter,
        Kind::Lowercase,
        Kind::Uppercase,
        Kind::NonBase,
        Kind::Alphanumeric,
        Kind::Decomposable,
        Kind::Illegal,
        Kind::Punctuation,
        Kind::Capitalized,
        Kind::Symbol,
        Kind::Newline,
        Kind::UrlFragment,
        Kind::UrlHost,
        Kind::UrlPassword,
        Kind::UrlPath,
        Kind::UrlQuery,
        Kind::UrlUser,
    ];

    /// The general categories the set is made of, if it is made of some.
    fn group(self) -> Option<GeneralCategoryGroup> {
        use GeneralCategoryGroup as G;
        Some(match self {
            Kind::Control => G::Control.union(G::Format),
            Kind::Whitespace => G::SpaceSeparator,
            Kind::WhitespaceAndNewline => G::Separator,
            Kind::DecimalDigit => G::DecimalNumber,
            Kind::Letter => G::Letter.union(G::Mark),
            Kind::Lowercase => G::LowercaseLetter,
            Kind::Uppercase => G::UppercaseLetter.union(G::TitlecaseLetter),
            Kind::NonBase => G::Mark,
            Kind::Alphanumeric => G::Letter.union(G::Mark).union(G::Number),
            Kind::Illegal => G::Unassigned,
            Kind::Punctuation => G::Punctuation,
            Kind::Capitalized => G::TitlecaseLetter,
            Kind::Symbol => G::Symbol,
            _ => return None,
        })
    }

    /// Characters the set holds beyond its categories.
    fn extra(self) -> &'static [(u32, u32)] {
        match self {
            // Tab, and U+200B, which Foundation still counts as a space.
            Kind::Whitespace => &[(0x09, 0x09), (0x200B, 0x200B)],
            Kind::WhitespaceAndNewline => &[(0x09, 0x0D), (0x85, 0x85), (0x200B, 0x200B)],
            Kind::Newline => &[(0x0A, 0x0D), (0x85, 0x85), (0x2028, 0x2029)],
            _ => &[],
        }
    }

    /// The ASCII characters of the URL component sets (they have no
    /// others): unreserved characters, sub-delimiters, and what each
    /// component also allows.
    fn url_ascii(self) -> Option<&'static [u8]> {
        const BASE: &[u8] = b"!$&'()*+,-.0123456789;=ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~";
        Some(match self {
            Kind::UrlFragment | Kind::UrlQuery => {
                b"!$&'()*+,-./0123456789:;=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~"
            }
            Kind::UrlHost => b"!$&'()*+,-.0123456789:;=ABCDEFGHIJKLMNOPQRSTUVWXYZ[]_abcdefghijklmnopqrstuvwxyz~",
            Kind::UrlPath => b"!$&'()*+,-./0123456789:;=@ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~",
            Kind::UrlPassword | Kind::UrlUser => BASE,
            _ => return None,
        })
    }

    #[inline]
    fn contains(self, c: u32) -> bool {
        if c < 0x80 {
            let bits = self.ascii_bits();
            return bits[(c >> 6) as usize] & (1 << (c & 63)) != 0;
        }
        self.contains_slow(c)
    }

    /// The set's ASCII members as a bitmap, worked out once for every set.
    fn ascii_bits(self) -> &'static [u64; 2] {
        static BITS: OnceLock<[[u64; 2]; 21]> = OnceLock::new();
        let all = BITS.get_or_init(|| {
            Kind::ALL.map(|k| {
                let mut bits = [0u64; 2];
                for c in (0..0x80).filter(|&c| k.contains_slow(c)) {
                    bits[(c >> 6) as usize] |= 1 << (c & 63);
                }
                bits
            })
        });
        &all[self as usize]
    }

    fn contains_slow(self, c: u32) -> bool {
        if c > MAX {
            return false;
        }
        if let Some(ascii) = self.url_ascii() {
            return c < 0x80 && ascii.contains(&(c as u8));
        }
        if self.extra().iter().any(|&(lo, hi)| (lo..=hi).contains(&c)) {
            return true;
        }
        match self {
            Kind::Newline => false,
            Kind::Decomposable => char::from_u32(c)
                .is_some_and(|ch| !matches!(CanonicalDecompositionBorrowed::new().decompose(ch), Decomposed::Default)),
            _ => {
                let gc = CodePointMapData::<GeneralCategory>::new().get32(c);
                self.group().is_some_and(|g| g.contains(gc))
            }
        }
    }

    /// The set as ranges.
    fn ranges(self) -> RangeSet {
        let mut set = RangeSet::default();
        if let Some(ascii) = self.url_ascii() {
            for &b in ascii {
                set.insert(u32::from(b), u32::from(b));
            }
            return set;
        }
        for &(lo, hi) in self.extra() {
            set.insert(lo, hi);
        }
        match self {
            Kind::Newline => {}
            Kind::Decomposable => {
                let mut c = 0;
                while c <= MAX {
                    if self.contains(c) {
                        let lo = c;
                        while c < MAX && self.contains(c + 1) {
                            c += 1;
                        }
                        set.insert(lo, c);
                    }
                    c += 1;
                }
            }
            _ => {
                if let Some(group) = self.group() {
                    for r in CodePointMapData::<GeneralCategory>::new().iter_ranges_for_group(group) {
                        set.insert(*r.start(), *r.end());
                    }
                }
            }
        }
        set
    }
}

/// Sorted, disjoint, non-adjacent inclusive ranges of code points, with a
/// bitmap of the ASCII members.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct RangeSet {
    ranges: Vec<(u32, u32)>,
    ascii: [u64; 2],
}

impl RangeSet {
    fn refresh_ascii(&mut self) {
        self.ascii = [0; 2];
        for &(lo, hi) in &self.ranges {
            if lo >= 0x80 {
                break;
            }
            for c in lo..=hi.min(0x7F) {
                self.ascii[(c >> 6) as usize] |= 1 << (c & 63);
            }
        }
    }

    #[inline]
    fn contains(&self, c: u32) -> bool {
        if c < 0x80 {
            return self.ascii[(c >> 6) as usize] & (1 << (c & 63)) != 0;
        }
        let k = self.ranges.partition_point(|&(_, hi)| hi < c);
        self.ranges.get(k).is_some_and(|&(lo, _)| lo <= c)
    }

    fn insert(&mut self, lo: u32, hi: u32) {
        if lo > hi {
            return;
        }
        // Everything overlapping or touching [lo, hi] merges into it.
        let a = self.ranges.partition_point(|&(_, h)| h.saturating_add(1) < lo);
        let b = self.ranges.partition_point(|&(l, _)| l <= hi.saturating_add(1));
        let (mut nlo, mut nhi) = (lo, hi);
        if a < b {
            nlo = nlo.min(self.ranges[a].0);
            nhi = nhi.max(self.ranges[b - 1].1);
        }
        self.ranges.splice(a..b, [(nlo, nhi)]);
        self.refresh_ascii();
    }

    fn remove(&mut self, lo: u32, hi: u32) {
        if lo > hi {
            return;
        }
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        for &(l, h) in &self.ranges {
            if h < lo || l > hi {
                out.push((l, h));
                continue;
            }
            if l < lo {
                out.push((l, lo - 1));
            }
            if h > hi {
                out.push((hi + 1, h));
            }
        }
        self.ranges = out;
        self.refresh_ascii();
    }

    fn inverted(&self) -> RangeSet {
        let mut out = RangeSet::default();
        let mut next = 0u32;
        for &(lo, hi) in &self.ranges {
            if lo > next {
                out.ranges.push((next, lo - 1));
            }
            next = hi.saturating_add(1);
        }
        if next <= MAX {
            out.ranges.push((next, MAX));
        }
        out.refresh_ascii();
        out
    }

    fn intersection(&self, other: &RangeSet) -> RangeSet {
        let mut out = RangeSet::default();
        let (mut i, mut j) = (0, 0);
        while i < self.ranges.len() && j < other.ranges.len() {
            let (a, b) = (self.ranges[i], other.ranges[j]);
            let (lo, hi) = (a.0.max(b.0), a.1.min(b.1));
            if lo <= hi {
                out.ranges.push((lo, hi));
            }
            if a.1 < b.1 { i += 1 } else { j += 1 }
        }
        out.refresh_ascii();
        out
    }
}

/// A character set's contents.
#[derive(Clone)]
pub(crate) enum CharSet {
    Predefined(Kind),
    Ranges(RangeSet),
    Inverted(Box<CharSet>),
}

impl CharSet {
    #[inline]
    pub(crate) fn contains(&self, c: u32) -> bool {
        match self {
            CharSet::Ranges(r) => r.contains(c),
            CharSet::Predefined(k) => k.contains(c),
            CharSet::Inverted(s) => c <= MAX && !s.contains(c),
        }
    }

    fn to_ranges(&self) -> RangeSet {
        match self {
            CharSet::Ranges(r) => r.clone(),
            CharSet::Predefined(k) => k.ranges(),
            CharSet::Inverted(s) => s.to_ranges().inverted(),
        }
    }

    fn from_text(s: &AnyObject) -> CharSet {
        let v = view(s);
        let mut set = RangeSet::default();
        for (_, c) in wtf8::code_points(v.text().bytes) {
            set.insert(c, c);
        }
        CharSet::Ranges(set)
    }
}

pub(crate) struct SetIvars {
    set: RefCell<CharSet>,
    /// Set for immutable sets, which are read without the cell's borrow
    /// count, from any thread.
    frozen: bool,
}

impl SetIvars {
    fn get(&self) -> SetRef<'_> {
        if self.frozen {
            // SAFETY: a frozen set never changes after initialization, which
            // happened before the object was shared.
            SetRef::Frozen(unsafe { &*self.set.as_ptr() })
        } else {
            SetRef::Cell(self.set.borrow())
        }
    }

    fn edit(&self, f: impl FnOnce(&mut RangeSet)) {
        assert!(!self.frozen, "sidestep: an immutable character set was changed");
        let mut set = self.set.borrow_mut();
        let mut ranges = set.to_ranges();
        f(&mut ranges);
        *set = CharSet::Ranges(ranges);
    }
}

pub(crate) enum SetRef<'a> {
    Frozen(&'a CharSet),
    Cell(Ref<'a, CharSet>),
}

impl std::ops::Deref for SetRef<'_> {
    type Target = CharSet;

    fn deref(&self) -> &CharSet {
        match self {
            SetRef::Frozen(s) => s,
            SetRef::Cell(s) => s,
        }
    }
}

/// How string code tests membership in a set.
pub(crate) enum Membership<'a> {
    Native(SetRef<'a>),
    Foreign(&'a AnyObject),
}

impl Membership<'_> {
    #[inline]
    pub(crate) fn contains(&self, c: u32) -> bool {
        match self {
            Membership::Native(s) => s.contains(c),
            // SAFETY: every character set answers -longCharacterIsMember:.
            Membership::Foreign(obj) => unsafe { msg_send![*obj, longCharacterIsMember: c] },
        }
    }
}

fn class_of(obj: &AnyObject) -> *const sidestep_runtime::Class {
    // SAFETY: every object starts with its class pointer.
    unsafe { *(obj as *const AnyObject).cast::<*const sidestep_runtime::Class>() }
}

fn ivars(obj: &AnyObject) -> Option<&SetIvars> {
    let class = class_of(obj);
    let native = std::ptr::eq(class, &NSCHARACTERSET)
        || std::ptr::eq(class, &NSMUTABLECHARACTERSET)
        || keeps_storage(obj.class());
    // SAFETY: an instance of NSCharacterSet or of a subclass that keeps its
    // storage and membership test.
    native.then(|| unsafe { &*(obj as *const AnyObject).cast::<NSCharacterSetImpl>() }.ivars())
}

#[cold]
fn keeps_storage(class: &AnyClass) -> bool {
    crate::string::install::keeps(class, NSCharacterSet::class(), &[sel!(longCharacterIsMember:)])
}

/// How to test membership in `set`.
pub(crate) fn membership(set: &AnyObject) -> Membership<'_> {
    match ivars(set) {
        Some(iv) => Membership::Native(iv.get()),
        None => Membership::Foreign(set),
    }
}

/// A new set of `class` (a character set class) holding `set`.
fn make(class: &AnyClass, set: CharSet) -> Retained<AnyObject> {
    let mut set = Some(set);
    let ptr: *mut Option<CharSet> = &mut set;
    // SAFETY: the private initializer takes the set it's pointed at.
    unsafe {
        let obj: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![obj, initWithSidestepCharSet: ptr.cast::<c_void>()]
    }
}

fn immutable(set: CharSet) -> Retained<NSCharacterSet> {
    // SAFETY: an instance of NSCharacterSet.
    unsafe { Retained::cast_unchecked(make(NSCharacterSet::class(), set)) }
}

fn mutable(set: CharSet) -> Retained<NSMutableCharacterSet> {
    // SAFETY: an instance of NSMutableCharacterSet.
    unsafe { Retained::cast_unchecked(make(NSMutableCharacterSet::class(), set)) }
}

/// The set of a range of code points.
fn from_range(range: NSRange) -> CharSet {
    let mut set = RangeSet::default();
    if range.length > 0 && range.location <= MAX as usize {
        let hi = range.location.saturating_add(range.length - 1).min(MAX as usize) as u32;
        set.insert(range.location as u32, hi);
    }
    CharSet::Ranges(set)
}

/// The shared instance of a predefined set.
fn predefined(kind: Kind) -> Retained<NSCharacterSet> {
    static SHARED: [OnceLock<usize>; 21] = [const { OnceLock::new() }; 21];
    let slot = &SHARED[Kind::ALL.iter().position(|&k| k == kind).expect("a kind")];
    let ptr = *slot.get_or_init(|| Retained::into_raw(immutable(CharSet::Predefined(kind))) as usize);
    // SAFETY: the shared sets are never released, and immutable sets may be
    // used from any thread.
    unsafe { Retained::retain(ptr as *mut NSCharacterSet) }.expect("non-null")
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSCharacterSet"]
    #[ivars = SetIvars]
    pub(crate) struct NSCharacterSetImpl;

    impl NSCharacterSetImpl {
        // The class methods. NSMutableCharacterSet has its own, which build
        // mutable sets.

        #[unsafe(method_id(controlCharacterSet))]
        fn control() -> Retained<NSCharacterSet> {
            predefined(Kind::Control)
        }

        #[unsafe(method_id(whitespaceCharacterSet))]
        fn whitespace() -> Retained<NSCharacterSet> {
            predefined(Kind::Whitespace)
        }

        #[unsafe(method_id(whitespaceAndNewlineCharacterSet))]
        fn whitespace_and_newline() -> Retained<NSCharacterSet> {
            predefined(Kind::WhitespaceAndNewline)
        }

        #[unsafe(method_id(decimalDigitCharacterSet))]
        fn decimal_digit() -> Retained<NSCharacterSet> {
            predefined(Kind::DecimalDigit)
        }

        #[unsafe(method_id(letterCharacterSet))]
        fn letter() -> Retained<NSCharacterSet> {
            predefined(Kind::Letter)
        }

        #[unsafe(method_id(lowercaseLetterCharacterSet))]
        fn lowercase() -> Retained<NSCharacterSet> {
            predefined(Kind::Lowercase)
        }

        #[unsafe(method_id(uppercaseLetterCharacterSet))]
        fn uppercase() -> Retained<NSCharacterSet> {
            predefined(Kind::Uppercase)
        }

        #[unsafe(method_id(nonBaseCharacterSet))]
        fn non_base() -> Retained<NSCharacterSet> {
            predefined(Kind::NonBase)
        }

        #[unsafe(method_id(alphanumericCharacterSet))]
        fn alphanumeric() -> Retained<NSCharacterSet> {
            predefined(Kind::Alphanumeric)
        }

        #[unsafe(method_id(decomposableCharacterSet))]
        fn decomposable() -> Retained<NSCharacterSet> {
            predefined(Kind::Decomposable)
        }

        #[unsafe(method_id(illegalCharacterSet))]
        fn illegal() -> Retained<NSCharacterSet> {
            predefined(Kind::Illegal)
        }

        #[unsafe(method_id(punctuationCharacterSet))]
        fn punctuation() -> Retained<NSCharacterSet> {
            predefined(Kind::Punctuation)
        }

        #[unsafe(method_id(capitalizedLetterCharacterSet))]
        fn capitalized() -> Retained<NSCharacterSet> {
            predefined(Kind::Capitalized)
        }

        #[unsafe(method_id(symbolCharacterSet))]
        fn symbol() -> Retained<NSCharacterSet> {
            predefined(Kind::Symbol)
        }

        #[unsafe(method_id(newlineCharacterSet))]
        fn newline() -> Retained<NSCharacterSet> {
            predefined(Kind::Newline)
        }

        #[unsafe(method_id(URLFragmentAllowedCharacterSet))]
        fn url_fragment() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlFragment)
        }

        #[unsafe(method_id(URLHostAllowedCharacterSet))]
        fn url_host() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlHost)
        }

        #[unsafe(method_id(URLPasswordAllowedCharacterSet))]
        fn url_password() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlPassword)
        }

        #[unsafe(method_id(URLPathAllowedCharacterSet))]
        fn url_path() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlPath)
        }

        #[unsafe(method_id(URLQueryAllowedCharacterSet))]
        fn url_query() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlQuery)
        }

        #[unsafe(method_id(URLUserAllowedCharacterSet))]
        fn url_user() -> Retained<NSCharacterSet> {
            predefined(Kind::UrlUser)
        }

        #[unsafe(method_id(characterSetWithRange:))]
        fn with_range(range: NSRange) -> Retained<NSCharacterSet> {
            immutable(from_range(range))
        }

        #[unsafe(method_id(characterSetWithCharactersInString:))]
        fn with_characters_in_string(string: &NSString) -> Retained<NSCharacterSet> {
            immutable(CharSet::from_text(string))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            finish(this, CharSet::Ranges(RangeSet::default()))
        }

        /// Sidestep's own initializer: `set` points to an
        /// `Option<CharSet>`, which it takes.
        #[unsafe(method_id(initWithSidestepCharSet:))]
        fn init_with_set(this: Allocated<Self>, set: *mut c_void) -> Retained<Self> {
            // SAFETY: only `make` sends this, with a valid pointer.
            let set = unsafe { &mut *set.cast::<Option<CharSet>>() }.take().expect("sidestep: a set");
            finish(this, set)
        }

        #[unsafe(method(characterIsMember:))]
        fn character_is_member(&self, c: u16) -> bool {
            self.ivars().get().contains(u32::from(c))
        }

        #[unsafe(method(longCharacterIsMember:))]
        fn long_character_is_member(&self, c: u32) -> bool {
            self.ivars().get().contains(c)
        }

        #[unsafe(method_id(invertedSet))]
        fn inverted_set(&self) -> Retained<NSCharacterSet> {
            let set = self.ivars().get().clone();
            immutable(match set {
                CharSet::Inverted(inner) => *inner,
                other => CharSet::Inverted(Box::new(other)),
            })
        }

        #[unsafe(method(isSupersetOfSet:))]
        fn is_superset_of_set(&self, other: &NSCharacterSet) -> bool {
            let mine = self.ivars().get().to_ranges();
            let theirs = match membership(other) {
                Membership::Native(s) => s.to_ranges(),
                Membership::Foreign(o) => foreign_ranges(o),
            };
            theirs.intersection(&mine) == theirs
        }

        #[unsafe(method_id(bitmapRepresentation))]
        fn bitmap_representation(&self) -> Retained<NSData> {
            NSData::with_bytes(&bitmap(self))
        }

        #[unsafe(method_id(characterSetWithBitmapRepresentation:))]
        fn with_bitmap_representation(data: &NSData) -> Retained<NSCharacterSet> {
            // SAFETY: an instance of NSCharacterSet.
            unsafe { Retained::cast_unchecked(with_bitmap(NSCharacterSet::class(), data)) }
        }

        #[unsafe(method(hasMemberInPlane:))]
        fn has_member_in_plane(&self, plane: u8) -> bool {
            let (lo, hi) = (u32::from(plane) << 16, (u32::from(plane) << 16) | 0xFFFF);
            let set = self.ivars().get();
            match &*set {
                CharSet::Ranges(r) => r.ranges.iter().any(|&(l, h)| l <= hi && h >= lo),
                other => (lo..=hi.min(MAX)).any(|c| other.contains(c)),
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSCharacterSet> {
            let iv = self.ivars();
            if iv.frozen && std::ptr::eq(class_of(self), &NSCHARACTERSET) {
                // SAFETY: an immutable set is its own copy.
                unsafe { Retained::cast_unchecked(objc2::Message::retain(self)) }
            } else {
                immutable(iv.get().clone())
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableCharacterSet> {
            mutable(self.ivars().get().clone())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            match other.and_then(|o| o.downcast_ref::<NSCharacterSet>()) {
                Some(o) => {
                    let theirs = match membership(o) {
                        Membership::Native(s) => s.to_ranges(),
                        Membership::Foreign(o) => foreign_ranges(o),
                    };
                    self.ivars().get().to_ranges().ranges == theirs.ranges
                }
                None => false,
            }
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            // Equal sets hash equally: hash the ASCII members only.
            let set = self.ivars().get();
            (0..0x80u32).filter(|&c| set.contains(c)).fold(0x345, |h: usize, c| h.rotate_left(5) ^ c as usize)
        }
    }

    unsafe impl NSObjectProtocol for NSCharacterSetImpl {}
);

fn finish(this: Allocated<NSCharacterSetImpl>, set: CharSet) -> Retained<NSCharacterSetImpl> {
    // SAFETY: a freshly allocated object.
    let obj = unsafe { &*Allocated::as_ptr(&this).cast::<AnyObject>() };
    let frozen = !crate::attributed::is_kind(obj.class(), NSMutableCharacterSet::class());
    let this = this.set_ivars(SetIvars { set: RefCell::new(set), frozen });
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![super(this), init] }
}

/// The bytes of a bitmap for a plane: bit `c & 7` of byte `(c & 0xFFFF) >> 3`
/// for each member `c` in it.
const PLANE_BYTES: usize = 8192;

/// `-bitmapRepresentation`: the Basic Multilingual Plane's bitmap, then,
/// for each other plane with members, its number and its bitmap.
pub(crate) fn bitmap(set: &AnyObject) -> Vec<u8> {
    let ranges = ranges_of(set);
    let mut planes: Vec<Vec<u8>> = vec![vec![0; PLANE_BYTES]; 17];
    for &(lo, hi) in &ranges.ranges {
        for c in lo..=hi.min(MAX) {
            planes[(c >> 16) as usize][((c & 0xFFFF) >> 3) as usize] |= 1 << (c & 7);
        }
    }
    let mut out = std::mem::take(&mut planes[0]);
    for (plane, bits) in planes.iter().enumerate().skip(1) {
        if bits.iter().any(|&b| b != 0) {
            out.push(plane as u8);
            out.extend_from_slice(bits);
        }
    }
    out
}

/// The set a bitmap from [`bitmap`] describes; a shorter first plane
/// leaves the rest of it empty.
pub(crate) fn from_bitmap(bytes: &[u8]) -> CharSet {
    let mut set = RangeSet::default();
    let mut add_plane = |plane: u32, bits: &[u8]| {
        let mut run: Option<u32> = None;
        for c in 0..(bits.len() as u32 * 8) {
            let member = bits[(c >> 3) as usize] & (1 << (c & 7)) != 0;
            match (member, run) {
                (true, None) => run = Some(c),
                (false, Some(start)) => {
                    set.insert((plane << 16) | start, (plane << 16) | (c - 1));
                    run = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run {
            set.insert((plane << 16) | start, (plane << 16) | (bits.len() as u32 * 8 - 1));
        }
    };
    let (first, mut rest) = bytes.split_at(bytes.len().min(PLANE_BYTES));
    add_plane(0, first);
    while let Some((&plane, after)) = rest.split_first() {
        let (bits, next) = after.split_at(after.len().min(PLANE_BYTES));
        if (1..=16).contains(&plane) {
            add_plane(u32::from(plane), bits);
        }
        rest = next;
    }
    CharSet::Ranges(set)
}

/// A new set of `class` holding what a bitmap describes, for
/// `+characterSetWithBitmapRepresentation:`.
pub(crate) fn with_bitmap(class: &AnyClass, data: &NSData) -> Retained<AnyObject> {
    // SAFETY: nothing changes the data while it is read.
    let bytes = unsafe { crate::data::bytes(data) };
    make(class, from_bitmap(bytes))
}

/// The members of a set of another class, asked one by one.
#[cold]
fn foreign_ranges(set: &AnyObject) -> RangeSet {
    let m = Membership::Foreign(set);
    let mut out = RangeSet::default();
    let mut c = 0;
    while c <= MAX {
        if m.contains(c) {
            let lo = c;
            while c < MAX && m.contains(c + 1) {
                c += 1;
            }
            out.ranges.push((lo, c));
        }
        c += 1;
    }
    out.refresh_ascii();
    out
}

define_class!(
    #[unsafe(super(NSCharacterSet, NSObject))]
    #[name = "NSMutableCharacterSet"]
    pub(crate) struct NSMutableCharacterSetImpl;

    impl NSMutableCharacterSetImpl {
        #[unsafe(method_id(controlCharacterSet))]
        fn control() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Control))
        }

        #[unsafe(method_id(whitespaceCharacterSet))]
        fn whitespace() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Whitespace))
        }

        #[unsafe(method_id(whitespaceAndNewlineCharacterSet))]
        fn whitespace_and_newline() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::WhitespaceAndNewline))
        }

        #[unsafe(method_id(decimalDigitCharacterSet))]
        fn decimal_digit() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::DecimalDigit))
        }

        #[unsafe(method_id(letterCharacterSet))]
        fn letter() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Letter))
        }

        #[unsafe(method_id(lowercaseLetterCharacterSet))]
        fn lowercase() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Lowercase))
        }

        #[unsafe(method_id(uppercaseLetterCharacterSet))]
        fn uppercase() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Uppercase))
        }

        #[unsafe(method_id(nonBaseCharacterSet))]
        fn non_base() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::NonBase))
        }

        #[unsafe(method_id(alphanumericCharacterSet))]
        fn alphanumeric() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Alphanumeric))
        }

        #[unsafe(method_id(decomposableCharacterSet))]
        fn decomposable() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Decomposable))
        }

        #[unsafe(method_id(illegalCharacterSet))]
        fn illegal() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Illegal))
        }

        #[unsafe(method_id(punctuationCharacterSet))]
        fn punctuation() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Punctuation))
        }

        #[unsafe(method_id(capitalizedLetterCharacterSet))]
        fn capitalized() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Capitalized))
        }

        #[unsafe(method_id(symbolCharacterSet))]
        fn symbol() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Symbol))
        }

        #[unsafe(method_id(newlineCharacterSet))]
        fn newline() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::Newline))
        }

        #[unsafe(method_id(URLFragmentAllowedCharacterSet))]
        fn url_fragment() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlFragment))
        }

        #[unsafe(method_id(URLHostAllowedCharacterSet))]
        fn url_host() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlHost))
        }

        #[unsafe(method_id(URLPasswordAllowedCharacterSet))]
        fn url_password() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlPassword))
        }

        #[unsafe(method_id(URLPathAllowedCharacterSet))]
        fn url_path() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlPath))
        }

        #[unsafe(method_id(URLQueryAllowedCharacterSet))]
        fn url_query() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlQuery))
        }

        #[unsafe(method_id(URLUserAllowedCharacterSet))]
        fn url_user() -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::Predefined(Kind::UrlUser))
        }

        #[unsafe(method_id(characterSetWithRange:))]
        fn with_range(range: NSRange) -> Retained<NSMutableCharacterSet> {
            mutable(from_range(range))
        }

        #[unsafe(method_id(characterSetWithBitmapRepresentation:))]
        fn with_bitmap_representation(data: &NSData) -> Retained<NSMutableCharacterSet> {
            // SAFETY: an instance of NSMutableCharacterSet.
            unsafe { Retained::cast_unchecked(with_bitmap(NSMutableCharacterSet::class(), data)) }
        }

        #[unsafe(method_id(characterSetWithCharactersInString:))]
        fn with_characters_in_string(string: &NSString) -> Retained<NSMutableCharacterSet> {
            mutable(CharSet::from_text(string))
        }

        #[unsafe(method(addCharactersInRange:))]
        fn add_characters_in_range(&self, range: NSRange) {
            if range.length > 0 {
                let hi = range.location.saturating_add(range.length - 1).min(MAX as usize) as u32;
                storage(self).edit(|s| s.insert(range.location as u32, hi));
            }
        }

        #[unsafe(method(removeCharactersInRange:))]
        fn remove_characters_in_range(&self, range: NSRange) {
            if range.length > 0 {
                let hi = range.location.saturating_add(range.length - 1).min(MAX as usize) as u32;
                storage(self).edit(|s| s.remove(range.location as u32, hi));
            }
        }

        #[unsafe(method(addCharactersInString:))]
        fn add_characters_in_string(&self, string: &NSString) {
            let CharSet::Ranges(add) = CharSet::from_text(string) else { unreachable!() };
            storage(self).edit(|s| {
                for &(lo, hi) in &add.ranges {
                    s.insert(lo, hi);
                }
            });
        }

        #[unsafe(method(removeCharactersInString:))]
        fn remove_characters_in_string(&self, string: &NSString) {
            let CharSet::Ranges(remove) = CharSet::from_text(string) else { unreachable!() };
            storage(self).edit(|s| {
                for &(lo, hi) in &remove.ranges {
                    s.remove(lo, hi);
                }
            });
        }

        #[unsafe(method(formUnionWithCharacterSet:))]
        fn form_union(&self, other: &NSCharacterSet) {
            let theirs = ranges_of(other);
            storage(self).edit(|s| {
                for &(lo, hi) in &theirs.ranges {
                    s.insert(lo, hi);
                }
            });
        }

        #[unsafe(method(formIntersectionWithCharacterSet:))]
        fn form_intersection(&self, other: &NSCharacterSet) {
            let theirs = ranges_of(other);
            storage(self).edit(|s| *s = s.intersection(&theirs));
        }

        #[unsafe(method(invert))]
        fn invert(&self) {
            storage(self).edit(|s| *s = s.inverted());
        }
    }
);

fn storage(obj: &AnyObject) -> &SetIvars {
    // SAFETY: only NSMutableCharacterSet's own methods call this.
    unsafe { &*(obj as *const AnyObject).cast::<NSCharacterSetImpl>() }.ivars()
}

/// A set's members as ranges, copied so no borrow is held.
fn ranges_of(set: &AnyObject) -> RangeSet {
    match membership(set) {
        Membership::Native(s) => s.to_ranges(),
        Membership::Foreign(o) => foreign_ranges(o),
    }
}
