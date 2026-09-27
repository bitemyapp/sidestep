//! `NSTextSelection` and `NSTextSelectionNavigation`, and the text
//! questions a layout manager answers as a selection's data source.
//!
//! A selection is ranges with an affinity and a granularity. Navigation
//! works through its data source: Sidestep's layout manager answers from
//! its layout directly; another data source through the protocol's methods.
//! Measured on macOS (`conformance/tests/textkit2.rs`): a click (not
//! extending) is a caret at the insertion point, downstream, by character;
//! below the lines of the fragment it is in (a subclass's padding), at its
//! last line's end; in an empty document, nothing.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::ptr::NonNull;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSTextRange, NSTextSelection, NSTextSelectionAffinity, NSTextSelectionGranularity, NSTextSelectionNavigation,
    NSTextSelectionNavigationDestination, NSTextSelectionNavigationDirection, NSTextSelectionNavigationModifier,
    NSTextSelectionNavigationWritingDirection,
};
use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSRect, NSString, NSStringEnumerationOptions};

use super::layout_manager::NSTextLayoutManagerImpl;
use super::location::{self, offset_of, span_of};
use crate::textkit::attrs::Dict;

sidestep_runtime::static_class!(pub(crate) NSTEXTSELECTION, NSTEXTSELECTION_META = "NSTextSelection", || {
    let _ = NSTextSelectionImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTSELECTIONNAVIGATION, NSTEXTSELECTIONNAVIGATION_META = "NSTextSelectionNavigation", || {
    let _ = NSTextSelectionNavigationImpl::class();
});

pub(crate) struct SelectionIvars {
    ranges: RefCell<Retained<NSArray<NSTextRange>>>,
    affinity: Cell<NSTextSelectionAffinity>,
    granularity: Cell<NSTextSelectionGranularity>,
    anchor: Cell<f64>,
    logical: Cell<bool>,
    secondary: RefCell<Option<Retained<AnyObject>>>,
    typing: RefCell<Retained<Dict>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextSelection"]
    #[ivars = SelectionIvars]
    pub(crate) struct NSTextSelectionImpl;

    impl NSTextSelectionImpl {
        #[unsafe(method_id(initWithRanges:affinity:granularity:))]
        fn init_with_ranges(
            this: Allocated<Self>,
            ranges: &NSArray<NSTextRange>,
            affinity: NSTextSelectionAffinity,
            granularity: NSTextSelectionGranularity,
        ) -> Retained<Self> {
            // SAFETY: -copy of an array is an array.
            let ranges: Retained<NSArray<NSTextRange>> = unsafe { msg_send![ranges, copy] };
            let this = this.set_ivars(SelectionIvars {
                ranges: RefCell::new(ranges),
                affinity: Cell::new(affinity),
                granularity: Cell::new(granularity),
                anchor: Cell::new(0.0),
                logical: Cell::new(false),
                secondary: RefCell::new(None),
                typing: RefCell::new(NSDictionary::new()),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithRange:affinity:granularity:))]
        fn init_with_range(
            this: Allocated<Self>,
            range: &NSTextRange,
            affinity: NSTextSelectionAffinity,
            granularity: NSTextSelectionGranularity,
        ) -> Retained<Self> {
            let ranges = NSArray::from_slice(&[range]);
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithRanges: &*ranges, affinity: affinity, granularity: granularity] }
        }

        #[unsafe(method_id(initWithLocation:affinity:))]
        fn init_with_location(
            this: Allocated<Self>,
            at: &AnyObject,
            affinity: NSTextSelectionAffinity,
        ) -> Retained<Self> {
            crate::load_shell::<NSTextRange>();
            // SAFETY: NSTextRange's initializer with a location.
            let r: Retained<NSTextRange> = unsafe { msg_send![NSTextRange::alloc(), initWithLocation: at] };
            let granularity = NSTextSelectionGranularity::Character;
            // SAFETY: the initializer above.
            unsafe { msg_send![this, initWithRange: &*r, affinity: affinity, granularity: granularity] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let ranges = NSArray::<NSTextRange>::new();
            let (affinity, granularity) = (NSTextSelectionAffinity::Downstream, NSTextSelectionGranularity::Character);
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithRanges: &*ranges, affinity: affinity, granularity: granularity] }
        }

        #[unsafe(method_id(textRanges))]
        fn text_ranges(&self) -> Retained<NSArray<NSTextRange>> {
            self.ivars().ranges.borrow().clone()
        }

        #[unsafe(method(granularity))]
        fn granularity(&self) -> NSTextSelectionGranularity {
            self.ivars().granularity.get()
        }

        #[unsafe(method(affinity))]
        fn affinity(&self) -> NSTextSelectionAffinity {
            self.ivars().affinity.get()
        }

        #[unsafe(method(isTransient))]
        fn is_transient(&self) -> bool {
            false
        }

        #[unsafe(method(anchorPositionOffset))]
        fn anchor_position_offset(&self) -> f64 {
            self.ivars().anchor.get()
        }

        #[unsafe(method(setAnchorPositionOffset:))]
        fn set_anchor_position_offset(&self, offset: f64) {
            self.ivars().anchor.set(offset);
        }

        #[unsafe(method(isLogical))]
        fn is_logical(&self) -> bool {
            self.ivars().logical.get()
        }

        #[unsafe(method(setLogical:))]
        fn set_logical(&self, on: bool) {
            self.ivars().logical.set(on);
        }

        #[unsafe(method_id(secondarySelectionLocation))]
        fn secondary_selection_location(&self) -> Option<Retained<AnyObject>> {
            self.ivars().secondary.borrow().clone()
        }

        #[unsafe(method(setSecondarySelectionLocation:))]
        fn set_secondary_selection_location(&self, at: Option<&AnyObject>) {
            *self.ivars().secondary.borrow_mut() = at.map(|a| a.retain());
        }

        #[unsafe(method_id(typingAttributes))]
        fn typing_attributes(&self) -> Retained<Dict> {
            self.ivars().typing.borrow().clone()
        }

        #[unsafe(method(setTypingAttributes:))]
        fn set_typing_attributes(&self, attrs: &Dict) {
            // SAFETY: -copy of a dictionary is a dictionary.
            let copy: Retained<Dict> = unsafe { msg_send![attrs, copy] };
            *self.ivars().typing.borrow_mut() = copy;
        }

        #[unsafe(method_id(textSelectionWithTextRanges:))]
        fn text_selection_with_text_ranges(&self, ranges: &NSArray<NSTextRange>) -> Retained<NSTextSelection> {
            let s = new_selection(ranges, self.ivars().affinity.get(), self.ivars().granularity.get());
            if let Some(iv) = ivars(&s) {
                iv.typing.replace(self.ivars().typing.borrow().clone());
                iv.logical.set(self.ivars().logical.get());
                iv.anchor.set(self.ivars().anchor.get());
            }
            s
        }
    }

    unsafe impl NSObjectProtocol for NSTextSelectionImpl {}
);

fn ivars(s: &NSTextSelection) -> Option<&SelectionIvars> {
    let ours = <NSTextSelection as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(s.class(), ours)
        .then(|| unsafe { &*(s as *const NSTextSelection).cast::<NSTextSelectionImpl>() }.ivars())
}

/// A new selection of `ranges`.
pub(crate) fn new_selection(
    ranges: &NSArray<NSTextRange>,
    affinity: NSTextSelectionAffinity,
    granularity: NSTextSelectionGranularity,
) -> Retained<NSTextSelection> {
    crate::load_shell::<NSTextSelection>();
    // SAFETY: the designated initializer.
    unsafe { msg_send![NSTextSelection::alloc(), initWithRanges: ranges, affinity: affinity, granularity: granularity] }
}

/// A selection of the one range `a..b`.
pub(crate) fn selection_of(
    a: usize,
    b: usize,
    affinity: NSTextSelectionAffinity,
    granularity: NSTextSelectionGranularity,
) -> Retained<NSTextSelection> {
    let r = location::range(a, b);
    new_selection(&NSArray::from_slice(&[&*r]), affinity, granularity)
}

pub(crate) struct NavigationIvars {
    source: RefCell<Weak<AnyObject>>,
    non_contiguous: Cell<bool>,
    rotates: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextSelectionNavigation"]
    #[ivars = NavigationIvars]
    pub(crate) struct NSTextSelectionNavigationImpl;

    impl NSTextSelectionNavigationImpl {
        #[unsafe(method_id(initWithDataSource:))]
        fn init_with_data_source(this: Allocated<Self>, source: &AnyObject) -> Retained<Self> {
            let this = this.set_ivars(NavigationIvars {
                source: RefCell::new(Weak::new(source)),
                non_contiguous: Cell::new(false),
                rotates: Cell::new(true),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(textSelectionDataSource))]
        fn text_selection_data_source(&self) -> Option<Retained<AnyObject>> {
            self.ivars().source.borrow().load()
        }

        #[unsafe(method(allowsNonContiguousRanges))]
        fn allows_non_contiguous_ranges(&self) -> bool {
            self.ivars().non_contiguous.get()
        }

        #[unsafe(method(setAllowsNonContiguousRanges:))]
        fn set_allows_non_contiguous_ranges(&self, on: bool) {
            self.ivars().non_contiguous.set(on);
        }

        #[unsafe(method(rotatesCoordinateSystemForLayoutOrientation))]
        fn rotates(&self) -> bool {
            self.ivars().rotates.get()
        }

        #[unsafe(method(setRotatesCoordinateSystemForLayoutOrientation:))]
        fn set_rotates(&self, on: bool) {
            self.ivars().rotates.set(on);
        }

        #[unsafe(method(flushLayoutCache))]
        fn flush_layout_cache(&self) {}

        #[unsafe(method_id(destinationSelectionForTextSelection:direction:destination:extending:confined:))]
        fn destination_selection(
            &self,
            selection: &NSTextSelection,
            direction: NSTextSelectionNavigationDirection,
            destination: NSTextSelectionNavigationDestination,
            extending: bool,
            _confined: bool,
        ) -> Option<Retained<NSTextSelection>> {
            self.destination(selection, direction, destination, extending)
        }

        #[unsafe(method_id(textSelectionsInteractingAtPoint:inContainerAtLocation:anchors:modifiers:selecting:bounds:))]
        fn text_selections_interacting(
            &self,
            point: NSPoint,
            _container: &AnyObject,
            anchors: &NSArray<NSTextSelection>,
            modifiers: NSTextSelectionNavigationModifier,
            _selecting: bool,
            _bounds: NSRect,
        ) -> Retained<NSArray<NSTextSelection>> {
            self.interacting(point, anchors, modifiers)
        }

        #[unsafe(method_id(textSelectionForSelectionGranularity:enclosingTextSelection:))]
        fn text_selection_for_granularity(
            &self,
            granularity: NSTextSelectionGranularity,
            selection: &NSTextSelection,
        ) -> Retained<NSTextSelection> {
            self.enclosing(granularity, selection)
        }

        #[unsafe(method_id(textSelectionForSelectionGranularity:enclosingPoint:inContainerAtLocation:))]
        fn text_selection_for_granularity_at_point(
            &self,
            granularity: NSTextSelectionGranularity,
            point: NSPoint,
            _container: &AnyObject,
        ) -> Option<Retained<NSTextSelection>> {
            self.enclosing_point(granularity, point)
        }

        #[unsafe(method_id(resolvedInsertionLocationForTextSelection:writingDirection:))]
        fn resolved_insertion_location(
            &self,
            selection: &NSTextSelection,
            direction: NSTextSelectionNavigationWritingDirection,
        ) -> Option<Retained<AnyObject>> {
            selection_span(selection).map(|(a, b)| {
                let at = if direction == NSTextSelectionNavigationWritingDirection::RightToLeft { b } else { a };
                location::location(if a == b { a } else { at })
            })
        }

        #[unsafe(method_id(deletionRangesForTextSelection:direction:destination:allowsDecomposition:))]
        fn deletion_ranges(
            &self,
            selection: &NSTextSelection,
            direction: NSTextSelectionNavigationDirection,
            destination: NSTextSelectionNavigationDestination,
            _decompose: bool,
        ) -> Retained<NSArray<NSTextRange>> {
            self.deletions(selection, direction, destination)
        }
    }

    unsafe impl NSObjectProtocol for NSTextSelectionNavigationImpl {}
);

impl NSTextSelectionNavigationImpl {
    /// Where a move takes `selection`, as measured on macOS
    /// (`conformance/tests/textkit2.rs`): up and down go a line by
    /// character, and like forward and back by anything else; a caret
    /// moves from where it is, a selection collapses to its edge by
    /// character and moves from its edge otherwise (from its start up and
    /// down); extending by character moves the selection's end from its
    /// start, by anything larger its end forward or its start back; a move
    /// to a line's end, and a larger extension back, are upstream.
    fn destination(
        &self,
        selection: &NSTextSelection,
        direction: NSTextSelectionNavigationDirection,
        destination: NSTextSelectionNavigationDestination,
        extending: bool,
    ) -> Option<Retained<NSTextSelection>> {
        type Dir = NSTextSelectionNavigationDirection;
        type Dest = NSTextSelectionNavigationDestination;
        let tlm = self.manager()?;
        let (a, b) = selection_span(selection)?;
        let forward = matches!(direction, Dir::Forward | Dir::Right | Dir::Down);
        let by_char = destination == Dest::Character;
        let vertical = by_char && matches!(direction, Dir::Up | Dir::Down);
        let go = |from: usize| {
            if vertical { vertical_move(&tlm, from, forward) } else { move_by(&tlm, from, forward, destination) }
        };
        let upstream = NSTextSelectionAffinity::Upstream;
        let downstream = NSTextSelectionAffinity::Downstream;
        let (s, e, affinity) = if extending {
            if by_char {
                // The end moves; the start holds.
                let to = go(b);
                (a.min(to), a.max(to), downstream)
            } else if forward {
                (a, go(b).max(a), downstream)
            } else {
                (go(a).min(b), b, upstream)
            }
        } else if a != b && by_char && !vertical {
            // A selection collapses to its edge.
            let edge = if forward { b } else { a };
            (edge, edge, downstream)
        } else {
            let to = go(if vertical || !forward { a } else { b });
            let line_end = destination == Dest::Line && forward;
            (to, to, if line_end { upstream } else { downstream })
        };
        Some(selection_of(s, e, affinity, NSTextSelectionGranularity::Character))
    }

    fn interacting(
        &self,
        point: NSPoint,
        anchors: &NSArray<NSTextSelection>,
        modifiers: NSTextSelectionNavigationModifier,
    ) -> Retained<NSArray<NSTextSelection>> {
        if self.manager().is_some_and(|m| m.content_len() == 0) {
            return NSArray::new();
        }
        let Some(at) = self.location_at(point) else { return NSArray::new() };
        let extend = modifiers.contains(NSTextSelectionNavigationModifier::Extend);
        let anchor = anchors.firstObject().and_then(|s| selection_span(&s));
        let s = match (extend, anchor) {
            (true, Some((a, b))) => {
                let (s, e) = if at < a { (at, b) } else { (a, at.max(b)) };
                selection_of(s, e, NSTextSelectionAffinity::Downstream, NSTextSelectionGranularity::Character)
            }
            _ => selection_of(at, at, NSTextSelectionAffinity::Downstream, NSTextSelectionGranularity::Character),
        };
        NSArray::from_retained_slice(&[s])
    }

    fn enclosing(
        &self,
        granularity: NSTextSelectionGranularity,
        selection: &NSTextSelection,
    ) -> Retained<NSTextSelection> {
        let Some(tlm) = self.manager() else { return selection.retain() };
        let Some((a, b)) = selection_span(selection) else { return selection.retain() };
        let first = granular_range(&tlm, a, granularity).unwrap_or(a..a);
        let last = if b > a { granular_range(&tlm, b - 1, granularity).unwrap_or(b..b) } else { first.clone() };
        selection_of(first.start.min(a), last.end.max(b), selection.affinity(), granularity)
    }

    fn enclosing_point(
        &self,
        granularity: NSTextSelectionGranularity,
        point: NSPoint,
    ) -> Option<Retained<NSTextSelection>> {
        let tlm = self.manager()?;
        let at = self.location_at(point)?;
        let r = granular_range(&tlm, at, granularity)?;
        Some(selection_of(r.start, r.end, NSTextSelectionAffinity::Downstream, granularity))
    }

    fn deletions(
        &self,
        selection: &NSTextSelection,
        direction: NSTextSelectionNavigationDirection,
        destination: NSTextSelectionNavigationDestination,
    ) -> Retained<NSArray<NSTextRange>> {
        let Some((a, b)) = selection_span(selection) else { return NSArray::new() };
        if a != b {
            return selection.textRanges();
        }
        let Some(tlm) = self.manager() else { return NSArray::new() };
        let forward = matches!(
            direction,
            NSTextSelectionNavigationDirection::Forward | NSTextSelectionNavigationDirection::Right
        );
        let to = move_by(&tlm, a, forward, destination);
        let (s, e) = (a.min(to), a.max(to));
        if s == e {
            return NSArray::new();
        }
        let r = location::range(s, e);
        NSArray::from_retained_slice(&[r])
    }
}

impl NSTextSelectionNavigationImpl {
    /// The data source as Sidestep's layout manager.
    fn manager(&self) -> Option<Retained<NSTextLayoutManagerImpl>> {
        let s = self.ivars().source.borrow().load()?;
        super::layout_manager::manager_of(&s)
    }

    /// Where an insertion point goes for `point` (container coordinates).
    fn location_at(&self, point: NSPoint) -> Option<usize> {
        if let Some(tlm) = self.manager() {
            return Some(tlm.insertion_index(point).0);
        }
        // Another data source: the nearest caret on the line at the point.
        let source = self.ivars().source.borrow().load()?;
        let zero = location::location(0);
        // SAFETY: the data source's method takes a point and a location.
        let line: Option<Retained<NSTextRange>> =
            unsafe { msg_send![&*source, lineFragmentRangeForPoint: point, inContainerAtLocation: &*zero] };
        let line = line?;
        // SAFETY: location takes nothing.
        let start: Retained<AnyObject> = unsafe { msg_send![&*line, location] };
        let best = Cell::new((f64::INFINITY, offset_of(&start).unwrap_or(0)));
        let block = block2::RcBlock::new(|x: f64, at: NonNull<AnyObject>, _leading: Bool, _stop: NonNull<Bool>| {
            // SAFETY: the location given is alive for the call.
            let o = offset_of(unsafe { at.as_ref() }).unwrap_or(0);
            let d = (x - point.x).abs();
            if d < best.get().0 {
                best.set((d, o));
            }
        });
        // SAFETY: the data source's method takes a location and a block.
        let _: () =
            unsafe { msg_send![&*source, enumerateCaretOffsetsInLineFragmentAtLocation: &*start, usingBlock: &*block] };
        Some(best.get().1)
    }
}

/// A navigation over `source`.
pub(crate) fn new_navigation(source: &AnyObject) -> Retained<NSTextSelectionNavigation> {
    crate::load_shell::<NSTextSelectionNavigation>();
    // SAFETY: the designated initializer.
    unsafe { msg_send![NSTextSelectionNavigation::alloc(), initWithDataSource: source] }
}

/// A selection's ranges together, in offsets.
fn selection_span(s: &NSTextSelection) -> Option<(usize, usize)> {
    let ranges = s.textRanges();
    let mut out: Option<(usize, usize)> = None;
    for r in ranges.iter() {
        let (a, b) = span_of(&r)?;
        out = Some(out.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
    }
    out
}

/// The text storage of `tlm`'s content storage.
fn storage_of(tlm: &NSTextLayoutManagerImpl) -> Option<Retained<objc2_app_kit::NSTextStorage>> {
    // SAFETY: textContentManager takes nothing.
    let c: Option<Retained<AnyObject>> = unsafe { msg_send![tlm, textContentManager] };
    let c = c?;
    super::content::as_storage(&c)?.text_storage_now()
}

/// Where a move from `from` by `destination` ends.
fn move_by(
    tlm: &NSTextLayoutManagerImpl,
    from: usize,
    forward: bool,
    destination: NSTextSelectionNavigationDestination,
) -> usize {
    let Some(ts) = storage_of(tlm) else { return from };
    let len = crate::textkit::selection::len(&ts);
    use crate::textkit::selection as sel;
    match destination {
        NSTextSelectionNavigationDestination::Character => {
            if forward {
                sel::next_char(&ts, from)
            } else {
                sel::prev_char(&ts, from)
            }
        }
        NSTextSelectionNavigationDestination::Word => {
            if forward {
                sel::word_end_after(&ts, from)
            } else {
                sel::word_start_before(&ts, from)
            }
        }
        NSTextSelectionNavigationDestination::Line => match tlm.line_at(from, false) {
            Some(l) => {
                if forward {
                    l.content_end()
                } else {
                    l.range().start
                }
            }
            None => from,
        },
        NSTextSelectionNavigationDestination::Paragraph | NSTextSelectionNavigationDestination::Sentence => {
            let p = sel::paragraph(&ts, from.min(len));
            if forward { sel::content_end(&ts, p) } else { p.start }
        }
        _ => {
            if forward {
                len
            } else {
                0
            }
        }
    }
}

/// Up or down a line from `from`, keeping its x: the next line (the
/// first of the next fragment, past a fragment's padding) or the one
/// before; past the first or last line, the text's start or end.
fn vertical_move(tlm: &NSTextLayoutManagerImpl, from: usize, down: bool) -> usize {
    let len = tlm.content_len();
    let caret = tlm.caret_rect(from, false);
    let Some(line) = tlm.line_at(from, false) else { return from };
    let r = line.range();
    let to = if down {
        match tlm.line_at(r.end.min(len), false) {
            // The last line: nothing below it.
            Some(l) if l.range() == r => return len,
            l => l,
        }
    } else if r.start == 0 {
        return 0;
    } else {
        tlm.line_at(r.start - 1, false)
    };
    let Some(to) = to else { return from };
    let hit = to.line().hit((caret.origin.x - tlm.padding() - to.left) as f32);
    (to.start + hit.index as usize).min(len)
}

/// The range of `granularity` around offset `at`.
pub(crate) fn granular_range(
    tlm: &NSTextLayoutManagerImpl,
    at: usize,
    granularity: NSTextSelectionGranularity,
) -> Option<Range<usize>> {
    let ts = storage_of(tlm)?;
    let len = crate::textkit::selection::len(&ts);
    let at = at.min(len);
    use crate::textkit::selection as sel;
    Some(match granularity {
        NSTextSelectionGranularity::Word => sel::word_at(&ts, at),
        NSTextSelectionGranularity::Paragraph | NSTextSelectionGranularity::Sentence => sel::paragraph(&ts, at),
        NSTextSelectionGranularity::Line => tlm.line_at(at, false).map_or(at..at, |l| l.range()),
        _ => {
            if at >= len {
                at..at
            } else {
                at..sel::next_char(&ts, at)
            }
        }
    })
}

/// The block `enumerateSubstringsFromLocation:options:usingBlock:` takes.
pub(crate) type SubstringBlock = dyn Fn(*mut NSString, NonNull<NSTextRange>, *mut NSTextRange, NonNull<Bool>);

/// `enumerateSubstringsFromLocation:options:usingBlock:` over the layout
/// manager's text, through `NSString`'s enumeration.
pub(crate) fn enumerate_substrings(
    tlm: &NSTextLayoutManagerImpl,
    from: &AnyObject,
    options: NSStringEnumerationOptions,
    block: &DynBlock<SubstringBlock>,
) {
    let (Some(o), Some(ts)) = (offset_of(from), storage_of(tlm)) else { return };
    let string: Retained<NSString> = ts.string();
    let len = string.length();
    let o = o.min(len);
    let reverse = options.contains(NSStringEnumerationOptions::Reverse);
    let range = if reverse { objc2_foundation::NSRange::new(0, o) } else { objc2_foundation::NSRange::new(o, len - o) };
    let inner = block2::RcBlock::new(
        |s: *mut NSString, r: objc2_foundation::NSRange, enclosing: objc2_foundation::NSRange, stop: NonNull<Bool>| {
            let tr = location::range(r.location, r.location + r.length);
            let er = location::range(enclosing.location, enclosing.location + enclosing.length);
            block.call((s, NonNull::from(&*tr), Retained::as_ptr(&er).cast_mut(), stop));
        },
    );
    // SAFETY: NSString's enumeration, with a block of its type.
    let _: () =
        unsafe { msg_send![&*string, enumerateSubstringsInRange: range, options: options, usingBlock: &*inner] };
}
