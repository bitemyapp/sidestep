//! `NSLayoutManager`: a text storage's text laid out in its text
//! containers, through the text engine's line layout (`text::lines`).
//!
//! Layout goes a paragraph at a time. The layout manager keeps an entry
//! for each of the storage's paragraphs (`layout_cache`): its lines once
//! laid out, or an estimate of its height until then. An edit
//! (`processEditingForTextStorage:…`) replaces the entries of the
//! paragraphs it touched with estimates, keeping the old height where one
//! paragraph became one, and nothing else is laid out again: the lines of
//! other paragraphs are relative to their paragraph's top, which moves by
//! itself. Questions about an index lay out the paragraph that holds it,
//! and questions about a point the paragraphs there; with contiguous layout
//! (AppKit's default) the paragraphs above them first, so positions are
//! exact, and with `allowsNonContiguousLayout` the ones above keep their
//! estimates until laid out. `ensureLayout…` and `usedRectForTextContainer:`
//! lay out what they cover exactly. Text the storage hasn't cut into
//! paragraphs yet (text set whole) is estimated a stretch at a time, its
//! paragraphs alike, so setting 11 MB of text costs a few thousand
//! estimates, not 200 000. Before laying paragraphs out, the manager has the
//! storage fix their attributes where it put fixing off, and it lays out
//! again what an edit changed, not the whole paragraphs a change put off
//! fixing is widened to. What remains is laid out when the run
//! loop is idle, a few milliseconds a turn, before it would wait: only
//! for a manager whose text a view shows, on the main thread (views are
//! the main thread's, so such a manager is too); other managers lay out
//! only on demand, on whatever thread uses them.
//!
//! What layout changes, the text views showing it draw again: a paragraph
//! laid out again as tall as before is drawn again alone, one whose
//! extent changed from its top down (and the view sizes to the text
//! again). An edit of a few paragraphs a view shows lays them out at once,
//! so typing redraws only its line unless the lines below move.
//!
//! Glyphs are one per UTF-16 unit, so a glyph index is a character index;
//! the second half of a surrogate pair is a null glyph. All the text goes
//! into the first text container. A line fragment is as wide as the
//! container and as tall as its line with the line spacing after it (and
//! the paragraph spacing before the first line and after the last; the
//! text's last line has neither after it). Its used rect is the line's
//! text with the container's padding at each end, where the text is
//! (alignment and indents place it), kept inside the width the line may
//! take (so a clipped line, or spaces hanging past a wrap, reach no
//! further), from the line's top to the fragment's bottom less the
//! paragraph spacing after. The container's used rect is the union of
//! them. A text that is empty or ends in a paragraph separator ends in
//! the extra line fragment, laid out with the text view's typing
//! attributes (or the last character's).
//!
//! A storage of another class (a subclass keeping its own text) is read
//! through its primitives into a copy the layout manager keeps (a
//! `storage::Storage` of its own), edit by edit.
//!
//! Paragraphs in text blocks and tables are placed as `blocks` says: a
//! block paragraph's neighbours are read to see which blocks start and end
//! with it, and a table row's paragraphs are always laid out together.
//!
//! Layout managers work on any thread, one at a time; drawing, which only
//! views do, is the main thread's.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, Message, define_class, msg_send};
use objc2_app_kit::{
    NSFont, NSGlyphProperty, NSLayoutManager, NSTextContainer, NSTextStorage, NSTextStorageEditActions,
};
use objc2_foundation::{NSArray, NSInteger, NSPoint, NSRange, NSRect, NSSize, NSString, NSUInteger};
use sidestep_foundation::runloop::{self, Activity, Mode, ObserverId, RunLoop};

use super::attrs::{AttrTable, Dict, EMPTY};
use super::blocks::{self, Chain, Metrics, Place, RowMember};
use super::container::{self, Geometry};
use super::layout_cache::{Entry, LayoutCache, Piece};
use super::storage::{AttrId, Extent, Run, Storage};
use super::temporary::{self, Temporary};
use crate::text::layout::{Attrs, LineBreak};
use crate::text::lines::{self, Container, Line, ParagraphLines, Span, Styled};

sidestep_runtime::static_class!(pub(crate) NSLAYOUTMANAGER, NSLAYOUTMANAGER_META = "NSLayoutManager", || {
    let _ = NSLayoutManagerImpl::class();
});

/// Idle layout's share of a run-loop turn.
const IDLE_SLICE: Duration = Duration::from_millis(3);

/// Where idle layout goes among a run loop's observers: after the display
/// pass (2 000 000), so what shows is drawn first.
const IDLE_ORDER: isize = 3_000_000;

/// The layout state, borrowed only by Rust code that sends no messages.
struct State {
    cache: LayoutCache,
    /// The text engine's attributes for each of the storage's attribute
    /// ids, and whether each has been worked out (for the table's epoch).
    resolved: Vec<Attrs>,
    known: Vec<bool>,
    /// The text blocks of each attribute id's paragraph style.
    chains: Vec<Option<Chain>>,
    epoch: u64,
    geometry: Geometry,
    /// What views need to draw again since they last asked: the first
    /// paragraph whose extent changed (all below it moved), and the
    /// paragraphs laid out again as tall as before; and whether the used
    /// rect's edges moved (a view growing across follows them).
    moved: Option<usize>,
    redraw: Option<(usize, usize)>,
    widened: bool,
}

impl State {
    fn mark_moved(&mut self, p: usize) {
        self.moved = Some(self.moved.map_or(p, |m| m.min(p)));
    }

    fn mark_redraw(&mut self, p: usize) {
        self.redraw = Some(self.redraw.map_or((p, p), |(a, b)| (a.min(p), b.max(p))));
    }
}

pub(crate) struct Ivars {
    storage: RefCell<Weak<NSTextStorage>>,
    containers: RefCell<Vec<Retained<NSTextContainer>>>,
    delegate: RefCell<Weak<AnyObject>>,
    state: RefCell<State>,
    /// The text of a storage that isn't Sidestep's own, and its attributes.
    mirror: RefCell<Option<Storage>>,
    mirror_attrs: RefCell<AttrTable>,
    font_leading: Cell<bool>,
    non_contiguous: Cell<bool>,
    background_layout: Cell<bool>,
    shows_invisibles: Cell<bool>,
    shows_control: Cell<bool>,
    hyphenation: Cell<bool>,
    limits_suspicious: Cell<bool>,
    /// Lay text out as bullets, one per character (a secure field's).
    masked: Cell<bool>,
    idle: Cell<Option<(ObserverId, RunLoop)>>,
    /// Temporary attributes (see `temporary`).
    temporary: RefCell<Temporary>,
}

impl Drop for Ivars {
    fn drop(&mut self) {
        if let Some((id, rl)) = self.idle.take()
            && rl.is_current()
        {
            rl.remove_observer(id);
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLayoutManager"]
    #[ivars = Ivars]
    pub(crate) struct NSLayoutManagerImpl;

    impl NSLayoutManagerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Ivars {
                storage: RefCell::new(Weak::default()),
                containers: RefCell::new(Vec::new()),
                delegate: RefCell::new(Weak::default()),
                state: RefCell::new(State {
                    cache: LayoutCache::new(vec![Piece::one(Entry::estimate(0.0))]),
                    resolved: Vec::new(),
                    known: Vec::new(),
                    chains: Vec::new(),
                    epoch: u64::MAX,
                    geometry: Geometry::DEFAULT,
                    moved: None,
                    redraw: None,
                    widened: false,
                }),
                mirror: RefCell::new(None),
                mirror_attrs: RefCell::new(AttrTable::new()),
                font_leading: Cell::new(true),
                non_contiguous: Cell::new(false),
                background_layout: Cell::new(true),
                shows_invisibles: Cell::new(false),
                shows_control: Cell::new(false),
                hyphenation: Cell::new(false),
                limits_suspicious: Cell::new(true),
                masked: Cell::new(false),
                idle: Cell::new(None),
                temporary: RefCell::default(),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        // The text storage and the containers.

        #[unsafe(method_id(textStorage))]
        fn text_storage(&self) -> Option<Retained<NSTextStorage>> {
            self.ivars().storage.borrow().load()
        }

        #[unsafe(method(setTextStorage:))]
        fn set_text_storage(&self, storage: Option<&NSTextStorage>) {
            *self.ivars().storage.borrow_mut() = storage.map_or_else(Weak::default, Weak::new);
            self.ivars().temporary.borrow_mut().clear();
            self.rebuild();
            // The views of its containers show the new storage.
            for view in self.views() {
                super::text_view::storage_replaced(&view, storage);
            }
        }

        #[unsafe(method(replaceTextStorage:))]
        fn replace_text_storage(&self, storage: &NSTextStorage) {
            // Kept alive: the old storage may hold the only reference.
            let keep = self.retain();
            let this: &NSLayoutManager = keep.as_manager();
            let old = self.ivars().storage.borrow().load();
            if let Some(old) = old {
                old.removeLayoutManager(this);
            }
            storage.addLayoutManager(this);
        }

        #[unsafe(method_id(textContainers))]
        fn text_containers(&self) -> Retained<NSArray<NSTextContainer>> {
            NSArray::from_retained_slice(&self.ivars().containers.borrow())
        }

        #[unsafe(method(addTextContainer:))]
        fn add_text_container(&self, c: &NSTextContainer) {
            let n = self.ivars().containers.borrow().len();
            self.insert_container(c, n);
        }

        #[unsafe(method(insertTextContainer:atIndex:))]
        fn insert_text_container(&self, c: &NSTextContainer, index: NSUInteger) {
            self.insert_container(c, index);
        }

        #[unsafe(method(removeTextContainerAtIndex:))]
        fn remove_text_container_at_index(&self, index: NSUInteger) {
            let removed = {
                let mut cs = self.ivars().containers.borrow_mut();
                (index < cs.len()).then(|| cs.remove(index))
            };
            if let Some(c) = removed {
                // SAFETY: setLayoutManager: takes a layout manager or nil.
                let _: () = unsafe { msg_send![&*c, setLayoutManager: None::<&NSLayoutManager>] };
                if index == 0 {
                    self.geometry_changed();
                }
            }
        }

        #[unsafe(method(textContainerChangedGeometry:))]
        fn text_container_changed_geometry(&self, c: &NSTextContainer) {
            if self.is_first_container(c) {
                self.geometry_changed();
            }
        }

        #[unsafe(method(textContainerChangedTextView:))]
        fn text_container_changed_text_view(&self, _c: &NSTextContainer) {}

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *self.ivars().delegate.borrow_mut() = delegate.map_or_else(Weak::default, Weak::new);
        }

        // Options.

        #[unsafe(method(usesFontLeading))]
        fn uses_font_leading(&self) -> bool {
            self.ivars().font_leading.get()
        }

        #[unsafe(method(setUsesFontLeading:))]
        fn set_uses_font_leading(&self, flag: bool) {
            if self.ivars().font_leading.replace(flag) != flag {
                self.geometry_changed();
            }
        }

        #[unsafe(method(allowsNonContiguousLayout))]
        fn allows_non_contiguous_layout(&self) -> bool {
            self.ivars().non_contiguous.get()
        }

        #[unsafe(method(setAllowsNonContiguousLayout:))]
        fn set_allows_non_contiguous_layout(&self, flag: bool) {
            self.ivars().non_contiguous.set(flag);
        }

        #[unsafe(method(hasNonContiguousLayout))]
        fn has_non_contiguous_layout(&self) -> bool {
            let state = self.ivars().state.borrow();
            self.ivars().non_contiguous.get() && state.cache.next_unlaid(0, usize::MAX).is_some()
        }

        #[unsafe(method(backgroundLayoutEnabled))]
        fn background_layout_enabled(&self) -> bool {
            self.ivars().background_layout.get()
        }

        #[unsafe(method(setBackgroundLayoutEnabled:))]
        fn set_background_layout_enabled(&self, flag: bool) {
            self.ivars().background_layout.set(flag);
        }

        #[unsafe(method(showsInvisibleCharacters))]
        fn shows_invisible_characters(&self) -> bool {
            self.ivars().shows_invisibles.get()
        }

        #[unsafe(method(setShowsInvisibleCharacters:))]
        fn set_shows_invisible_characters(&self, flag: bool) {
            self.ivars().shows_invisibles.set(flag);
        }

        #[unsafe(method(showsControlCharacters))]
        fn shows_control_characters(&self) -> bool {
            self.ivars().shows_control.get()
        }

        #[unsafe(method(setShowsControlCharacters:))]
        fn set_shows_control_characters(&self, flag: bool) {
            self.ivars().shows_control.set(flag);
        }

        #[unsafe(method(usesDefaultHyphenation))]
        fn uses_default_hyphenation(&self) -> bool {
            self.ivars().hyphenation.get()
        }

        #[unsafe(method(setUsesDefaultHyphenation:))]
        fn set_uses_default_hyphenation(&self, flag: bool) {
            self.ivars().hyphenation.set(flag);
        }

        #[unsafe(method(limitsLayoutForSuspiciousContents))]
        fn limits_layout_for_suspicious_contents(&self) -> bool {
            self.ivars().limits_suspicious.get()
        }

        #[unsafe(method(setLimitsLayoutForSuspiciousContents:))]
        fn set_limits_layout_for_suspicious_contents(&self, flag: bool) {
            self.ivars().limits_suspicious.set(flag);
        }

        // Invalidation.

        #[unsafe(method(processEditingForTextStorage:edited:range:changeInLength:invalidatedRange:))]
        fn process_editing(
            &self,
            storage: &NSTextStorage,
            mask: NSTextStorageEditActions,
            range: NSRange,
            delta: NSInteger,
            _invalidated: NSRange,
        ) {
            let characters = mask.contains(NSTextStorageEditActions::EditedCharacters);
            // A change widened to whole paragraphs for its delegate is laid
            // out again as edited.
            let range = super::text_storage::edited_range(storage, range);
            self.edited(storage, range, delta, characters);
        }

        #[unsafe(method(invalidateLayoutForCharacterRange:actualCharacterRange:))]
        fn invalidate_layout_for_character_range(&self, range: NSRange, actual: *mut NSRange) {
            let r = self.invalidate(range.location..range.location + range.length);
            if !actual.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *actual = ns_range(r) };
            }
        }

        #[unsafe(method(invalidateGlyphsForCharacterRange:changeInLength:actualCharacterRange:))]
        fn invalidate_glyphs_for_character_range(&self, range: NSRange, _delta: NSInteger, actual: *mut NSRange) {
            if !actual.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *actual = range };
            }
        }

        #[unsafe(method(invalidateDisplayForCharacterRange:))]
        fn invalidate_display_for_character_range(&self, range: NSRange) {
            self.redraw_chars(range.location..range.location + range.length);
        }

        #[unsafe(method(invalidateDisplayForGlyphRange:))]
        fn invalidate_display_for_glyph_range(&self, range: NSRange) {
            self.redraw_chars(range.location..range.location + range.length);
        }

        // Layout on demand.

        #[unsafe(method(ensureGlyphsForCharacterRange:))]
        fn ensure_glyphs_for_character_range(&self, _range: NSRange) {}

        #[unsafe(method(ensureGlyphsForGlyphRange:))]
        fn ensure_glyphs_for_glyph_range(&self, _range: NSRange) {}

        #[unsafe(method(ensureLayoutForCharacterRange:))]
        fn ensure_layout_for_character_range(&self, range: NSRange) {
            let laid = self.ensure_chars(range.location..range.location + range.length);
            self.completed(laid);
        }

        #[unsafe(method(ensureLayoutForGlyphRange:))]
        fn ensure_layout_for_glyph_range(&self, range: NSRange) {
            let laid = self.ensure_chars(range.location..range.location + range.length);
            self.completed(laid);
        }

        #[unsafe(method(ensureLayoutForTextContainer:))]
        fn ensure_layout_for_text_container(&self, c: &NSTextContainer) {
            if self.is_first_container(c) {
                let laid = self.ensure_all();
                self.completed(laid);
            }
        }

        #[unsafe(method(ensureLayoutForBoundingRect:inTextContainer:))]
        fn ensure_layout_for_bounding_rect(&self, rect: NSRect, c: &NSTextContainer) {
            if self.is_first_container(c) {
                let laid = self.ensure_y(rect.origin.y, rect.origin.y + rect.size.height);
                self.completed(laid);
            }
        }

        #[unsafe(method(firstUnlaidCharacterIndex))]
        fn first_unlaid_character_index(&self) -> NSUInteger {
            self.first_unlaid()
        }

        #[unsafe(method(firstUnlaidGlyphIndex))]
        fn first_unlaid_glyph_index(&self) -> NSUInteger {
            self.first_unlaid()
        }

        #[unsafe(method(getFirstUnlaidCharacterIndex:glyphIndex:))]
        fn get_first_unlaid(&self, char_index: *mut NSUInteger, glyph_index: *mut NSUInteger) {
            let i = self.first_unlaid();
            // SAFETY: the caller passes valid pointers or null.
            unsafe {
                if !char_index.is_null() {
                    *char_index = i;
                }
                if !glyph_index.is_null() {
                    *glyph_index = i;
                }
            }
        }

        // Glyphs: one per UTF-16 unit.

        #[unsafe(method(numberOfGlyphs))]
        fn number_of_glyphs(&self) -> NSUInteger {
            self.text_len()
        }

        #[unsafe(method(isValidGlyphIndex:))]
        fn is_valid_glyph_index(&self, index: NSUInteger) -> bool {
            index < self.text_len()
        }

        #[unsafe(method(characterIndexForGlyphAtIndex:))]
        fn character_index_for_glyph_at_index(&self, index: NSUInteger) -> NSUInteger {
            index.min(self.text_len())
        }

        #[unsafe(method(glyphIndexForCharacterAtIndex:))]
        fn glyph_index_for_character_at_index(&self, index: NSUInteger) -> NSUInteger {
            index.min(self.text_len())
        }

        #[unsafe(method(propertyForGlyphAtIndex:))]
        fn property_for_glyph_at_index(&self, index: NSUInteger) -> NSGlyphProperty {
            let null = self.with_text(|t| index < t.len() && (0xDC00..0xE000).contains(&t.unit_at(index))).unwrap_or(false);
            if null { NSGlyphProperty::Null } else { NSGlyphProperty(0) }
        }

        #[unsafe(method(glyphRangeForCharacterRange:actualCharacterRange:))]
        fn glyph_range_for_character_range(&self, range: NSRange, actual: *mut NSRange) -> NSRange {
            let r = self.whole_characters(range);
            if !actual.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *actual = r };
            }
            r
        }

        #[unsafe(method(characterRangeForGlyphRange:actualGlyphRange:))]
        fn character_range_for_glyph_range(&self, range: NSRange, actual: *mut NSRange) -> NSRange {
            let r = self.whole_characters(range);
            if !actual.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *actual = r };
            }
            r
        }

        #[unsafe(method(notShownAttributeForGlyphAtIndex:))]
        fn not_shown_attribute_for_glyph_at_index(&self, index: NSUInteger) -> bool {
            self.with_text(|t| {
                index < t.len() && matches!(t.unit_at(index), 0x0A | 0x0D | 0x2029 | 0x2028)
            })
            .unwrap_or(false)
        }

        #[unsafe(method(drawsOutsideLineFragmentForGlyphAtIndex:))]
        fn draws_outside_line_fragment(&self, _index: NSUInteger) -> bool {
            false
        }

        // Containers and line fragments.

        #[unsafe(method_id(textContainerForGlyphAtIndex:effectiveRange:))]
        fn text_container_for_glyph_at_index(
            &self,
            index: NSUInteger,
            range: *mut NSRange,
        ) -> Option<Retained<NSTextContainer>> {
            self.container_for(index, range)
        }

        #[unsafe(method_id(textContainerForGlyphAtIndex:effectiveRange:withoutAdditionalLayout:))]
        fn text_container_for_glyph_at_index_without(
            &self,
            index: NSUInteger,
            range: *mut NSRange,
            _without: bool,
        ) -> Option<Retained<NSTextContainer>> {
            self.container_for(index, range)
        }

        #[unsafe(method(glyphRangeForTextContainer:))]
        fn glyph_range_for_text_container(&self, c: &NSTextContainer) -> NSRange {
            if self.is_first_container(c) { NSRange::new(0, self.text_len()) } else { NSRange::new(self.text_len(), 0) }
        }

        #[unsafe(method(usedRectForTextContainer:))]
        fn used_rect_for_text_container(&self, c: &NSTextContainer) -> NSRect {
            if !self.is_first_container(c) {
                return NSRect::ZERO;
            }
            let laid = self.ensure_all();
            self.completed(laid);
            self.used_rect()
        }

        #[unsafe(method(lineFragmentRectForGlyphAtIndex:effectiveRange:))]
        fn line_fragment_rect(&self, index: NSUInteger, range: *mut NSRange) -> NSRect {
            self.fragment(index, range, false)
        }

        #[unsafe(method(lineFragmentRectForGlyphAtIndex:effectiveRange:withoutAdditionalLayout:))]
        fn line_fragment_rect_without(&self, index: NSUInteger, range: *mut NSRange, _without: bool) -> NSRect {
            self.fragment(index, range, false)
        }

        #[unsafe(method(lineFragmentUsedRectForGlyphAtIndex:effectiveRange:))]
        fn line_fragment_used_rect(&self, index: NSUInteger, range: *mut NSRange) -> NSRect {
            self.fragment(index, range, true)
        }

        #[unsafe(method(lineFragmentUsedRectForGlyphAtIndex:effectiveRange:withoutAdditionalLayout:))]
        fn line_fragment_used_rect_without(&self, index: NSUInteger, range: *mut NSRange, _without: bool) -> NSRect {
            self.fragment(index, range, true)
        }

        #[unsafe(method(extraLineFragmentRect))]
        fn extra_line_fragment_rect(&self) -> NSRect {
            self.extra(false)
        }

        #[unsafe(method(extraLineFragmentUsedRect))]
        fn extra_line_fragment_used_rect(&self) -> NSRect {
            self.extra(true)
        }

        #[unsafe(method_id(extraLineFragmentTextContainer))]
        fn extra_line_fragment_text_container(&self) -> Option<Retained<NSTextContainer>> {
            let has = self.extra(false).size.height > 0.0;
            if has { self.ivars().containers.borrow().first().cloned() } else { None }
        }

        #[unsafe(method(locationForGlyphAtIndex:))]
        fn location_for_glyph_at_index(&self, index: NSUInteger) -> NSPoint {
            self.location(index)
        }

        #[unsafe(method(truncatedGlyphRangeInLineFragmentForGlyphAtIndex:))]
        fn truncated_glyph_range(&self, index: NSUInteger) -> NSRange {
            let r = self.line_at(index, false).and_then(|l| {
                let line = l.line();
                line.elided.clone().map(|e| (l.start + e.start as usize)..(l.start + e.end as usize))
            });
            r.map_or(NSRange::new(objc2_foundation::NSNotFound as usize, 0), ns_range)
        }

        // Geometry.

        #[unsafe(method(boundingRectForGlyphRange:inTextContainer:))]
        fn bounding_rect_for_glyph_range(&self, range: NSRange, c: &NSTextContainer) -> NSRect {
            if !self.is_first_container(c) {
                return NSRect::ZERO;
            }
            self.bounding_rect(range.location..range.location + range.length)
        }

        #[unsafe(method(glyphRangeForBoundingRect:inTextContainer:))]
        fn glyph_range_for_bounding_rect(&self, rect: NSRect, c: &NSTextContainer) -> NSRange {
            if !self.is_first_container(c) {
                return NSRange::new(0, 0);
            }
            ns_range(self.range_for_rect(rect, true))
        }

        #[unsafe(method(glyphRangeForBoundingRectWithoutAdditionalLayout:inTextContainer:))]
        fn glyph_range_for_bounding_rect_without(&self, rect: NSRect, c: &NSTextContainer) -> NSRange {
            if !self.is_first_container(c) {
                return NSRange::new(0, 0);
            }
            ns_range(self.range_for_rect(rect, false))
        }

        #[unsafe(method(glyphIndexForPoint:inTextContainer:fractionOfDistanceThroughGlyph:))]
        fn glyph_index_for_point_fraction(&self, p: NSPoint, c: &NSTextContainer, fraction: *mut f64) -> NSUInteger {
            let (index, f) = if self.is_first_container(c) { self.glyph_at(p) } else { (0, 0.0) };
            if !fraction.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *fraction = f };
            }
            index
        }

        #[unsafe(method(glyphIndexForPoint:inTextContainer:))]
        fn glyph_index_for_point(&self, p: NSPoint, c: &NSTextContainer) -> NSUInteger {
            if self.is_first_container(c) { self.glyph_at(p).0 } else { 0 }
        }

        #[unsafe(method(fractionOfDistanceThroughGlyphForPoint:inTextContainer:))]
        fn fraction_of_distance_through_glyph(&self, p: NSPoint, c: &NSTextContainer) -> f64 {
            if self.is_first_container(c) { self.glyph_at(p).1 } else { 0.0 }
        }

        #[unsafe(method(characterIndexForPoint:inTextContainer:fractionOfDistanceBetweenInsertionPoints:))]
        fn character_index_for_point(&self, p: NSPoint, c: &NSTextContainer, fraction: *mut f64) -> NSUInteger {
            let (index, f) = if self.is_first_container(c) { self.glyph_at(p) } else { (0, 0.0) };
            if !fraction.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { *fraction = f };
            }
            index
        }

        #[unsafe(method(enumerateLineFragmentsForGlyphRange:usingBlock:))]
        fn enumerate_line_fragments(
            &self,
            range: NSRange,
            block: &block2::DynBlock<dyn Fn(NSRect, NSRect, NonNull<NSTextContainer>, NSRange, NonNull<Bool>)>,
        ) {
            let Some(container) = self.ivars().containers.borrow().first().cloned() else { return };
            let fragments = self.fragments_in(range.location..range.location + range.length);
            for f in fragments {
                let mut stop = Bool::NO;
                block.call((f.rect, f.used, NonNull::from(&*container), ns_range(f.range), NonNull::from(&mut stop)));
                if stop.as_bool() {
                    break;
                }
            }
        }

        #[unsafe(method(enumerateEnclosingRectsForGlyphRange:withinSelectedGlyphRange:inTextContainer:usingBlock:))]
        fn enumerate_enclosing_rects(
            &self,
            range: NSRange,
            _selected: NSRange,
            c: &NSTextContainer,
            block: &block2::DynBlock<dyn Fn(NSRect, NonNull<Bool>)>,
        ) {
            if !self.is_first_container(c) {
                return;
            }
            for r in self.selection_rects(range.location..range.location + range.length) {
                let mut stop = Bool::NO;
                block.call((r, NonNull::from(&mut stop)));
                if stop.as_bool() {
                    break;
                }
            }
        }

        #[unsafe(method(defaultLineHeightForFont:))]
        fn default_line_height_for_font(&self, font: &NSFont) -> f64 {
            let (ascent, descent, leading) = rounded_metrics(font);
            ascent + descent + if self.ivars().font_leading.get() { leading } else { 0.0 }
        }

        #[unsafe(method(defaultBaselineOffsetForFont:))]
        fn default_baseline_offset_for_font(&self, font: &NSFont) -> f64 {
            rounded_metrics(font).0
        }

        // Text blocks.

        #[unsafe(method(layoutRectForTextBlock:glyphRange:))]
        fn layout_rect_for_text_block(&self, block: &objc2_app_kit::NSTextBlock, range: NSRange) -> NSRect {
            self.block_rects(block, range.location).map_or(NSRect::ZERO, |r| r.0)
        }

        #[unsafe(method(boundsRectForTextBlock:glyphRange:))]
        fn bounds_rect_for_text_block(&self, block: &objc2_app_kit::NSTextBlock, range: NSRange) -> NSRect {
            self.block_rects(block, range.location).map_or(NSRect::ZERO, |r| r.1)
        }

        #[unsafe(method(layoutRectForTextBlock:atIndex:effectiveRange:))]
        fn layout_rect_for_text_block_at(
            &self,
            block: &objc2_app_kit::NSTextBlock,
            index: NSUInteger,
            range: *mut NSRange,
        ) -> NSRect {
            let found = self.block_rects(block, index);
            set_range(range, found.as_ref().map(|r| r.2.clone()));
            found.map_or(NSRect::ZERO, |r| r.0)
        }

        #[unsafe(method(boundsRectForTextBlock:atIndex:effectiveRange:))]
        fn bounds_rect_for_text_block_at(
            &self,
            block: &objc2_app_kit::NSTextBlock,
            index: NSUInteger,
            range: *mut NSRange,
        ) -> NSRect {
            let found = self.block_rects(block, index);
            set_range(range, found.as_ref().map(|r| r.2.clone()));
            found.map_or(NSRect::ZERO, |r| r.1)
        }

        // Temporary attributes.

        #[unsafe(method_id(temporaryAttributesAtCharacterIndex:effectiveRange:))]
        fn temporary_attributes_at(&self, index: NSUInteger, range: *mut NSRange) -> Retained<Dict> {
            let (d, r) = self.temporary_at(index);
            write_range(range, r);
            d.unwrap_or_default()
        }

        #[unsafe(method_id(temporaryAttributesAtCharacterIndex:longestEffectiveRange:inRange:))]
        fn temporary_attributes_longest(&self, index: NSUInteger, range: *mut NSRange, limit: NSRange) -> Retained<Dict> {
            let (d, r) = self.temporary_longest(index, limit, |a, b| match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => a.isEqualToDictionary(b),
                _ => false,
            });
            write_range(range, r);
            d.unwrap_or_default()
        }

        #[unsafe(method_id(temporaryAttribute:atCharacterIndex:effectiveRange:))]
        fn temporary_attribute_at(
            &self,
            key: &NSString,
            index: NSUInteger,
            range: *mut NSRange,
        ) -> Option<Retained<AnyObject>> {
            let (d, r) = self.temporary_at(index);
            write_range(range, r);
            d.and_then(|d| d.objectForKey(key))
        }

        #[unsafe(method_id(temporaryAttribute:atCharacterIndex:longestEffectiveRange:inRange:))]
        fn temporary_attribute_longest(
            &self,
            key: &NSString,
            index: NSUInteger,
            range: *mut NSRange,
            limit: NSRange,
        ) -> Option<Retained<AnyObject>> {
            let value = |d: Option<&Dict>| d.and_then(|d| d.objectForKey(key));
            let (d, r) = self.temporary_longest(index, limit, |a, b| match (value(a), value(b)) {
                (None, None) => true,
                // SAFETY: isEqual: takes an object.
                (Some(a), Some(b)) => std::ptr::eq(&*a, &*b) || unsafe { msg_send![&*a, isEqual: &*b] },
                _ => false,
            });
            write_range(range, r);
            value(d.as_deref())
        }

        #[unsafe(method(setTemporaryAttributes:forCharacterRange:))]
        fn set_temporary_attributes(&self, attrs: &Dict, range: NSRange) {
            use objc2::Message;
            self.map_temporary(range, |_| Some(attrs.retain()));
        }

        #[unsafe(method(addTemporaryAttributes:forCharacterRange:))]
        fn add_temporary_attributes(&self, attrs: &Dict, range: NSRange) {
            self.map_temporary(range, |d| Some(temporary::added(d, attrs)));
        }

        #[unsafe(method(addTemporaryAttribute:value:forCharacterRange:))]
        fn add_temporary_attribute(&self, key: &NSString, value: &AnyObject, range: NSRange) {
            self.map_temporary(range, |d| temporary::with(d, key, Some(value)));
        }

        #[unsafe(method(removeTemporaryAttribute:forCharacterRange:))]
        fn remove_temporary_attribute(&self, key: &NSString, range: NSRange) {
            self.map_temporary(range, |d| temporary::with(d, key, None));
        }

        // Drawing.

        #[unsafe(method(drawBackgroundForGlyphRange:atPoint:))]
        fn draw_background_for_glyph_range(&self, range: NSRange, origin: NSPoint) {
            self.draw(range.location..range.location + range.length, origin, true);
        }

        #[unsafe(method(drawGlyphsForGlyphRange:atPoint:))]
        fn draw_glyphs_for_glyph_range(&self, range: NSRange, origin: NSPoint) {
            self.draw(range.location..range.location + range.length, origin, false);
        }
    }

    unsafe impl NSObjectProtocol for NSLayoutManagerImpl {}
);

/// Store `r` through an out-parameter, when there is one.
fn write_range(out: *mut NSRange, r: Range<usize>) {
    if !out.is_null() {
        // SAFETY: the caller passes a valid pointer or null.
        unsafe { *out = ns_range(r) };
    }
}

/// Store `r` (or NSNotFound) through an out-parameter, when there is one.
fn set_range(out: *mut NSRange, r: Option<Range<usize>>) {
    if !out.is_null() {
        let r = r.map_or(NSRange::new(objc2_foundation::NSNotFound as usize, 0), ns_range);
        // SAFETY: the caller passes a valid pointer or null.
        unsafe { *out = r };
    }
}

fn ns_range(r: Range<usize>) -> NSRange {
    NSRange::new(r.start, r.end.saturating_sub(r.start))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// A font's ascent, descent and leading, each rounded to whole points as
/// lines are measured.
fn rounded_metrics(font: &NSFont) -> (f64, f64, f64) {
    let f = crate::font::text_font(font);
    let m = &f.face.metrics;
    let size = f64::from(f.size);
    (
        (f64::from(m.ascent) * size).round(),
        (f64::from(-m.descent) * size).round(),
        (f64::from(m.leading) * size).round(),
    )
}

/// A line fragment: its rect, its used rect, and its glyphs.
#[derive(Clone, Debug)]
pub(crate) struct Fragment {
    pub rect: NSRect,
    pub used: NSRect,
    pub range: Range<usize>,
}

/// A laid-out line and where it is: its paragraph (number, first unit, the
/// top of its first line) and its place among the paragraph's lines.
#[derive(Clone)]
pub(crate) struct LineAt {
    pub para: usize,
    pub start: usize,
    pub top: f64,
    pub lines: Arc<ParagraphLines>,
    pub index: usize,
    /// The paragraph's leading and trailing spacing.
    pub lead: f64,
    pub trail: f64,
    /// Among text blocks: its content's left from the padding edge, and
    /// width.
    pub left: f64,
    pub width: Option<f64>,
}

impl LineAt {
    pub fn line(&self) -> &Line {
        &self.lines.lines[self.index]
    }

    /// The line's units in the text, separator included.
    pub fn range(&self) -> Range<usize> {
        let r = &self.line().range;
        self.start + r.start as usize..self.start + r.end as usize
    }

    /// Where the line's characters end: before its separator.
    pub fn content_end(&self) -> usize {
        self.start + self.line().content_end() as usize
    }

    /// The line's box: its top and height.
    pub fn line_top(&self) -> f64 {
        self.top + f64::from(self.line().top)
    }

    pub fn is_first(&self) -> bool {
        self.index == 0
    }

    pub fn is_last(&self) -> bool {
        self.index + 1 == self.lines.lines.len()
    }

    /// The line fragment's top and bottom: the line with the spacing that
    /// belongs to it.
    pub fn fragment_span(&self) -> (f64, f64) {
        let l = self.line();
        let top = self.line_top() - if self.is_first() { self.lead } else { 0.0 };
        let mut bottom = self.line_top() + f64::from(l.height);
        bottom += if self.is_last() { self.trail } else { f64::from(self.lines.spacing.line) };
        (top, bottom)
    }
}

impl NSLayoutManagerImpl {
    /// The temporary attributes at `index`, and where they are the same.
    fn temporary_at(&self, index: usize) -> (Option<Retained<Dict>>, Range<usize>) {
        let len = self.text_len();
        self.ivars().temporary.borrow().at(index, len)
    }

    /// The temporary attributes at `index`, and as far around it inside
    /// `limit` as `same` says they stay the same.
    fn temporary_longest(
        &self,
        index: usize,
        limit: NSRange,
        same: impl Fn(Option<&Dict>, Option<&Dict>) -> bool,
    ) -> (Option<Retained<Dict>>, Range<usize>) {
        let (lo, hi) = (limit.location, limit.location + limit.length);
        let (d, mut r) = self.temporary_at(index);
        while r.start > lo {
            let (other, o) = self.temporary_at(r.start - 1);
            if !same(d.as_deref(), other.as_deref()) {
                break;
            }
            r.start = o.start;
        }
        while r.end < hi {
            let (other, o) = self.temporary_at(r.end);
            if o.end <= r.end || !same(d.as_deref(), other.as_deref()) {
                break;
            }
            r.end = o.end;
        }
        let r = r.start.max(lo)..r.end.min(hi).max(r.start.max(lo));
        (d, r)
    }

    /// Rewrite the temporary attributes over `range`, and draw it again.
    fn map_temporary(&self, range: NSRange, f: impl FnMut(Option<&Dict>) -> Option<Retained<Dict>>) {
        let r = range.location..range.location + range.length;
        self.ivars().temporary.borrow_mut().map(r.clone(), f);
        self.redraw_chars(r);
    }

    /// Fill the temporary background colors of `lines`, the container's
    /// origin at `origin`.
    fn draw_temporary(&self, lines: &[LineAt], origin: NSPoint) {
        if self.ivars().temporary.borrow().is_empty() {
            return;
        }
        // SAFETY: the key is a constant string AppKit exports.
        let key = unsafe { objc2_app_kit::NSBackgroundColorAttributeName };
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else { return };
        let span = first.range().start..last.range().end;
        let runs: Vec<(Range<usize>, Retained<Dict>)> =
            self.ivars().temporary.borrow().in_range(span).map(|(r, d)| (r, d.clone())).collect();
        for (r, d) in runs {
            let Some(color) = d.objectForKey(key).and_then(|c| c.downcast::<objc2_app_kit::NSColor>().ok()) else {
                continue;
            };
            let on: Vec<LineAt> =
                lines.iter().filter(|l| l.range().start < r.end && l.range().end > r.start).cloned().collect();
            for rect in self.spans_of(&on, r) {
                super::text_view::fill(super::text_view::offset(rect, origin), &color);
            }
        }
    }

    /// Lay text out as a bullet for each character (and nothing for the
    /// rest of its units), as a secure field shows it.
    pub(crate) fn set_masked(&self, on: bool) {
        if self.ivars().masked.replace(on) != on {
            self.rebuild();
        }
    }

    fn as_manager(&self) -> &NSLayoutManager {
        // SAFETY: NSLayoutManager is this class.
        unsafe { &*(self as *const Self).cast::<NSLayoutManager>() }
    }

    fn insert_container(&self, c: &NSTextContainer, index: usize) {
        {
            let mut cs = self.ivars().containers.borrow_mut();
            let index = index.min(cs.len());
            cs.insert(index, c.retain());
        }
        // SAFETY: setLayoutManager: takes a layout manager.
        let _: () = unsafe { msg_send![c, setLayoutManager: self.as_manager()] };
        if self.is_first_container(c) {
            self.geometry_changed();
        }
    }

    fn is_first_container(&self, c: &NSTextContainer) -> bool {
        self.ivars().containers.borrow().first().is_some_and(|f| std::ptr::eq(&**f, c))
    }

    /// The first container's geometry, read now.
    fn read_geometry(&self) -> Geometry {
        let first = self.ivars().containers.borrow().first().cloned();
        first.map_or(Geometry::DEFAULT, |c| container::geometry(&c))
    }

    /// The text: the storage's own when it is Sidestep's, else the copy.
    fn with_text<R>(&self, f: impl FnOnce(&Storage) -> R) -> Option<R> {
        let storage = self.ivars().storage.borrow().load()?;
        match super::text_storage::native(&storage) {
            Some(iv) => Some(f(&iv.text())),
            None => self.ivars().mirror.borrow().as_ref().map(f),
        }
    }

    /// The attribute table of the text [`with_text`](Self::with_text)
    /// reads.
    fn with_table<R>(&self, f: impl FnOnce(&RefCell<AttrTable>) -> R) -> Option<R> {
        let storage = self.ivars().storage.borrow().load()?;
        match super::text_storage::native(&storage) {
            Some(iv) => Some(f(iv.attrs())),
            None => Some(f(&self.ivars().mirror_attrs)),
        }
    }

    pub(crate) fn text_len(&self) -> usize {
        self.with_text(Storage::len).unwrap_or(0)
    }

    /// Glyph (or character) range `r`, grown to whole characters.
    fn whole_characters(&self, r: NSRange) -> NSRange {
        let len = self.text_len();
        let (mut a, mut b) = (r.location.min(len), (r.location + r.length).min(len));
        self.with_text(|t| {
            let low = |i: usize| i < t.len() && (0xDC00..0xE000).contains(&t.unit_at(i));
            if a > 0 && low(a) {
                a -= 1;
            }
            if b > a && b < t.len() && low(b) {
                b += 1;
            }
        });
        NSRange::new(a, b - a)
    }

    // Keeping up with the storage.

    /// Lay the text out afresh: a new storage, or new geometry.
    fn rebuild(&self) {
        let storage = self.ivars().storage.borrow().load();
        if let Some(s) = &storage
            && super::text_storage::native(s).is_none()
        {
            *self.ivars().mirror.borrow_mut() = Some(mirror_of(s, &self.ivars().mirror_attrs));
        } else {
            *self.ivars().mirror.borrow_mut() = None;
        }
        let geometry = self.read_geometry();
        let n = self.with_text(Storage::paragraph_count).unwrap_or(1);
        let pieces = self.estimates(0..n, &geometry);
        {
            let mut state = self.ivars().state.borrow_mut();
            state.geometry = geometry;
            state.cache = LayoutCache::new(pieces);
            state.redraw = None;
            state.mark_moved(0);
        }
        self.laid_out_changed();
    }

    fn geometry_changed(&self) {
        self.rebuild();
    }

    /// The storage changed `range` (in the text as it is now) by `delta`
    /// units; `characters`: not only attributes.
    fn edited(&self, storage: &NSTextStorage, range: NSRange, delta: isize, characters: bool) {
        let (a, b) = (range.location, range.location + range.length);
        if super::text_storage::native(storage).is_none() {
            let old_end = (b as isize - delta).max(a as isize) as usize;
            let (text, runs) = read_range(storage, a..b);
            let table = &self.ivars().mirror_attrs;
            let ids: Vec<AttrId> = runs.iter().map(|(_, d)| super::attrs::intern(table, Some(d))).collect();
            let mut mirror = self.ivars().mirror.borrow_mut();
            let Some(m) = mirror.as_mut() else { return };
            if old_end > m.len() {
                drop(mirror);
                self.rebuild();
                return;
            }
            let runs: Vec<Run> =
                runs.iter().zip(ids).map(|((r, _), id)| Run { len: r.len() as u32, attrs: id }).collect();
            m.replace_runs(a..old_end, &text, &runs);
        }
        let Some((n_new, k0, k1)) =
            self.with_text(|t| (t.paragraph_count(), t.locate(a).para, t.locate(b.min(t.len())).para))
        else {
            return;
        };
        let n_old = self.ivars().state.borrow().cache.len();
        let old_count = (k1 + 1 - k0) as isize - (n_new as isize - n_old as isize);
        if old_count < 0 || k0 > n_old {
            self.rebuild();
            return;
        }
        let geometry = self.ivars().state.borrow().geometry;
        let mut pieces = self.estimates(k0..k1 + 1, &geometry);
        // A few paragraphs a view shows are laid out now, if they were
        // before, so that only what changed is drawn again; otherwise all
        // from them down is.
        let few = k1 + 1 - k0 <= 8;
        let was_laid = {
            let mut state = self.ivars().state.borrow_mut();
            let old_end = (k0 + old_count as usize).min(state.cache.len());
            let was_laid = few && (k0..old_end).all(|p| state.cache.get(p).is_laid());
            // One paragraph for one: keep the old extent, which is likelier
            // than an estimate.
            if old_count == 1 && pieces.len() == 1 && pieces[0].count == 1 {
                let old = state.cache.get(k0);
                pieces[0] = Piece::one(Entry { lines: None, ..old.clone() });
            }
            state.cache.splice(k0, old_count as usize, pieces);
            was_laid
        };
        let around = self.unlay_around(k0, k1);
        let eager = was_laid && around.is_none() && self.shown();
        {
            let mut state = self.ivars().state.borrow_mut();
            if !eager {
                state.mark_moved(around.unwrap_or(k0).min(k0));
            }
        }
        if eager {
            self.lay_out(k0..k1 + 1, 16);
        }
        self.laid_out_changed();
        if characters {
            let old_end = ((b as isize) - delta).max(a as isize) as usize;
            self.ivars().temporary.borrow_mut().edited(a..old_end, b - a);
            for view in self.views() {
                super::text_view::storage_edited(&view, range, delta);
            }
        }
    }

    /// Invalidate the layout of the paragraphs `range` touches; what they
    /// are, as characters.
    fn invalidate(&self, range: Range<usize>) -> Range<usize> {
        let Some((k0, k1, whole)) = self.with_text(|t| {
            let span = super::text_storage::span(t, range.clone());
            (t.locate(span.start).para, t.locate(span.end.min(t.len())).para, span)
        }) else {
            return range;
        };
        let around = self.unlay_around(k0, k1);
        {
            // The old extents stay until laid out again.
            let mut state = self.ivars().state.borrow_mut();
            for k in k0..=k1 {
                state.cache.unlay(k);
            }
            state.mark_moved(around.unwrap_or(k0).min(k0));
        }
        self.laid_out_changed();
        whole
    }

    /// After the layout changed: the views draw again what changed, the
    /// delegate hears of it, and the rest is laid out when the run loop is
    /// idle.
    fn laid_out_changed(&self) {
        self.redisplay(false);
        self.schedule_idle();
        // Loaded first: the delegate may set another in its method.
        let delegate = self.ivars().delegate.borrow().load();
        if let Some(d) = delegate
            && super::undo_text::responds(&d, objc2::sel!(layoutManagerDidInvalidateLayout:))
        {
            // SAFETY: the delegate method takes the layout manager.
            let _: () = unsafe { msg_send![&*d, layoutManagerDidInvalidateLayout: self.as_manager()] };
        }
    }

    /// Tell the text views showing the text to draw again what layout
    /// changed (sizing to the text at most every so often, if `idle`).
    fn redisplay(&self, idle: bool) {
        for v in self.views() {
            super::text_view::layout_changed(&v, idle);
        }
    }

    /// The views draw the paragraphs holding characters `range` again.
    fn redraw_chars(&self, range: Range<usize>) {
        let Some((k0, k1)) = self.with_text(|t| {
            let len = t.len();
            (t.locate(range.start.min(len)).para, t.locate(range.end.min(len)).para)
        }) else {
            return;
        };
        {
            let mut state = self.ivars().state.borrow_mut();
            state.mark_redraw(k0);
            state.mark_redraw(k1);
        }
        self.redisplay(false);
    }

    /// The text views of the containers.
    fn views(&self) -> Vec<Retained<AnyObject>> {
        let containers = self.ivars().containers.borrow().clone();
        containers
            .iter()
            .filter_map(|c| {
                // SAFETY: textView takes nothing and returns a view or nil.
                let view: Option<Retained<AnyObject>> = unsafe { msg_send![&**c, textView] };
                view
            })
            .collect()
    }

    /// Whether a view in a window shows the text.
    fn shown(&self) -> bool {
        self.views().iter().any(|v| v.downcast_ref::<objc2_app_kit::NSView>().is_some_and(|v| v.window().is_some()))
    }

    /// What the views need to draw again since they last asked, as heights
    /// in the container (to the bottom, when text moved), and whether the
    /// text's extent may have changed.
    pub(crate) fn take_damage(&self) -> Option<(f64, f64, bool)> {
        let mut state = self.ivars().state.borrow_mut();
        let (moved, redraw) = (state.moved.take(), state.redraw.take());
        let widened = std::mem::take(&mut state.widened);
        let mut out = redraw.map(|(a, b)| (state.cache.flow_top(a), state.cache.flow_top(b + 1)));
        if let Some(p) = moved {
            let y = state.cache.flow_top(p);
            out = Some((out.map_or(y, |o| o.0.min(y)), f64::INFINITY));
        }
        out.map(|(a, b)| (a, b, moved.is_some() || widened))
    }

    /// `ensureLayout…` laid text out: the delegate hears it finished, and
    /// whether all of the text is.
    fn completed(&self, laid: bool) {
        if !laid {
            return;
        }
        let delegate = self.ivars().delegate.borrow().load();
        let Some(d) = delegate else { return };
        let sel = objc2::sel!(layoutManager:didCompleteLayoutForTextContainer:atEnd:);
        if !super::undo_text::responds(&d, sel) {
            return;
        }
        let container = self.ivars().containers.borrow().first().cloned();
        let at_end = self.ivars().state.borrow().cache.next_unlaid(0, usize::MAX).is_none();
        // SAFETY: the delegate method takes the manager, a container or nil
        // and a BOOL.
        let _: () = unsafe {
            msg_send![&*d, layoutManager: self.as_manager(), didCompleteLayoutForTextContainer: container.as_deref(), atEnd: at_end]
        };
    }

    // Resolving attributes.

    /// Work out the text engine's attributes for `ids`, sending messages
    /// with nothing borrowed.
    fn resolve(&self, ids: &[AttrId]) {
        let Some(epoch) = self.with_table(|t| t.borrow().epoch()) else { return };
        {
            let mut state = self.ivars().state.borrow_mut();
            if state.epoch != epoch {
                state.epoch = epoch;
                state.known.clear();
                state.resolved.clear();
                state.chains.clear();
            }
        }
        for &id in ids {
            let known = self.ivars().state.borrow().known.get(id as usize).copied().unwrap_or(false);
            if known {
                continue;
            }
            let Some(dict) = self.with_table(|t| t.borrow().dict(id).clone()) else { return };
            let attrs = crate::string_drawing::attrs_of(Some(&dict));
            // SAFETY: the key is a constant string AppKit exports.
            let style = dict.objectForKey(unsafe { objc2_app_kit::NSParagraphStyleAttributeName });
            let chain = style
                .and_then(|s| s.downcast::<objc2_app_kit::NSParagraphStyle>().ok())
                .and_then(|s| crate::paragraph::blocks_of(&s));
            let mut state = self.ivars().state.borrow_mut();
            let i = id as usize;
            if state.resolved.len() <= i {
                let filler = attrs.clone();
                state.resolved.resize(i + 1, filler);
                state.known.resize(i + 1, false);
                state.chains.resize(i + 1, None);
            }
            state.resolved[i] = attrs;
            state.chains[i] = chain;
            state.known[i] = true;
        }
    }

    /// The extra line fragment's attributes: the text view's typing
    /// attributes, else the last character's, else the default ones.
    fn extra_attrs(&self) -> Attrs {
        let first = self.ivars().containers.borrow().first().cloned();
        let view = first.and_then(|c| {
            // SAFETY: textView takes nothing and returns a view or nil.
            let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*c, textView] };
            v
        });
        if let Some(v) = view
            && let Some(dict) = super::text_view::typing_attributes(&v)
        {
            return crate::string_drawing::attrs_of(Some(&dict));
        }
        let len = self.text_len();
        let storage = self.ivars().storage.borrow().load();
        if let Some(storage) = storage
            && len > 0
        {
            super::text_storage::ensure_fixed(&storage, len - 1..len);
        }
        let last = self.with_text(|t| (!t.is_empty()).then(|| t.attrs_at(t.len() - 1, false).0)).flatten();
        match last.and_then(|id| self.with_table(|t| t.borrow().dict(id).clone())) {
            Some(dict) => crate::string_drawing::attrs_of(Some(&dict)),
            None => crate::string_drawing::attrs_of(None),
        }
    }

    // Laying out.

    /// Estimated entries for paragraphs `paras`: a line of the paragraph's
    /// first font for each container width of text, at half an em a unit.
    /// Text the storage hasn't cut into paragraphs yet is estimated a
    /// stretch at a time, its paragraphs alike, as long as the stretch's
    /// average.
    fn estimates(&self, paras: Range<usize>, g: &Geometry) -> Vec<Piece> {
        let Some(facts) = self.with_text(|t| {
            let mut out = Vec::new();
            t.for_each_extent(paras, |e| out.push(e));
            out
        }) else {
            return Vec::new();
        };
        let attrs_of = |e: &Extent| match *e {
            Extent::One { attrs, .. } | Extent::Many { attrs, .. } => attrs.unwrap_or(EMPTY),
        };
        let mut ids: Vec<AttrId> = facts.iter().map(attrs_of).collect();
        ids.sort_unstable();
        ids.dedup();
        self.resolve(&ids);
        let state = self.ivars().state.borrow();
        let width = (g.size.width - 2.0 * g.padding).max(1.0) as f32;
        let estimate = |len: f32, id: AttrId| {
            let size = state.resolved.get(id as usize).map_or(12.0, |a| a.font.size);
            Entry::estimate(super::layout_cache::estimate_height(len, size, width))
        };
        facts
            .iter()
            .map(|e| match *e {
                Extent::One { len16, .. } => Piece::one(estimate(len16 as f32, attrs_of(e))),
                Extent::Many { count, len16, .. } => {
                    Piece { count, entry: estimate(len16 as f32 / count.max(1) as f32, attrs_of(e)) }
                }
            })
            .collect()
    }

    /// The text engine's container for the geometry.
    fn engine_container(&self, g: &Geometry) -> Container {
        let width = (g.size.width - 2.0 * g.padding).max(0.0) as f32;
        let truncation = match g.line_break {
            objc2_app_kit::NSLineBreakMode::ByTruncatingHead => Some(LineBreak::TruncateHead),
            objc2_app_kit::NSLineBreakMode::ByTruncatingTail => Some(LineBreak::TruncateTail),
            objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle => Some(LineBreak::TruncateMiddle),
            _ => None,
        };
        Container { width, max_lines: 0, truncation, font_leading: self.ivars().font_leading.get() }
    }

    /// Lay out the paragraphs among `paras` that aren't laid out, at most
    /// `limit` of them (and the rest of the table rows they are in); how
    /// many it laid out.
    fn lay_out(&self, paras: Range<usize>, limit: usize) -> usize {
        // Which need it, and the attributes they use.
        let mut wanted: Vec<usize> = {
            let state = self.ivars().state.borrow();
            let mut out = Vec::new();
            let mut from = paras.start;
            while out.len() < limit {
                match state.cache.next_unlaid(from, paras.end) {
                    Some(p) => {
                        out.push(p);
                        from = p + 1;
                    }
                    None => break,
                }
            }
            out
        };
        if wanted.is_empty() {
            return 0;
        }
        self.fix_attributes_of(&wanted);
        self.resolve_runs(&wanted);
        // Paragraphs in text blocks: their neighbours' blocks, and whole
        // table rows. (Paragraphs in none don't depend on their neighbours.)
        let mut own: Vec<Option<Chain>> = self.chains_of(&wanted);
        if own.iter().any(Option::is_some) {
            let blocky: Vec<usize> = wanted.iter().zip(&own).filter(|(_, c)| c.is_some()).map(|(&p, _)| p).collect();
            let rows: Vec<Range<usize>> = wanted
                .iter()
                .zip(&own)
                .filter(|(_, c)| c.as_ref().is_some_and(|c| blocks::row_key(c).is_some()))
                .filter_map(|(&p, _)| self.row_of(p))
                .collect();
            if !rows.is_empty() {
                wanted.extend(rows.into_iter().flatten());
                wanted.sort_unstable();
                wanted.dedup();
                self.fix_attributes_of(&wanted);
                self.resolve_runs(&wanted);
                own = self.chains_of(&wanted);
            }
            let neighbours: Vec<usize> =
                blocky.iter().chain(&wanted).flat_map(|&p| [p.saturating_sub(1), p + 1]).collect();
            self.resolve_paras(&neighbours);
        }
        let last_para = self.with_text(Storage::paragraph_count).unwrap_or(1) - 1;
        let extra = wanted.contains(&last_para).then(|| self.extra_attrs());
        let geometry = self.ivars().state.borrow().geometry;
        let container = self.engine_container(&geometry);
        let max_lines = geometry.max_lines;
        let padding = geometry.padding;
        // Across: where each paragraph's blocks put its lines.
        let chains: Vec<Chains> = wanted
            .iter()
            .zip(own)
            .map(|(&p, own)| match own {
                Some(c) => (Some(c), p.checked_sub(1).and_then(|q| self.chain(q)), self.chain(p + 1)),
                None => (None, None, None),
            })
            .collect();
        let acrosses = across_all(&wanted, &chains, padding, f64::from(container.width));
        let lines: Vec<(usize, ParagraphLines)> = self
            .with_text(|t| {
                let state = self.ivars().state.borrow();
                let mut spans: Vec<Span> = Vec::new();
                wanted
                    .iter()
                    .zip(&acrosses)
                    .map(|(&p, across)| {
                        let at = t.locate_paragraph(p);
                        let para = t.para(at);
                        spans.clear();
                        let mut start = 0u32;
                        for r in para.runs() {
                            spans.push(Span { start, end: start + r.len, attrs: r.attrs });
                            start += r.len;
                        }
                        let masked_text;
                        let text = if self.ivars().masked.get() {
                            masked_text = mask(para.text());
                            masked_text.as_str()
                        } else {
                            para.text()
                        };
                        let narrowed;
                        let container = match across {
                            Some(a) => {
                                narrowed = Container { width: a.w as f32, ..container };
                                &narrowed
                            }
                            None => &container,
                        };
                        let lines = if para.len16() == 0 {
                            let attrs = extra.clone().unwrap_or_else(|| crate::string_drawing::attrs_of(None));
                            let styled = Styled { text: "", attrs: std::slice::from_ref(&attrs), spans: &[] };
                            lines::lay_out_paragraph(styled, container, 0)
                        } else {
                            let styled = Styled { text, attrs: &state.resolved, spans: &spans };
                            lines::lay_out_paragraph(styled, container, 0)
                        };
                        (p, lines)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let metrics: Vec<Metrics> = lines
            .iter()
            .map(|(p, l)| Metrics {
                lead: if *p == 0 { 0.0 } else { f64::from(l.spacing.before) },
                height: f64::from(l.height()),
                trail: f64::from(l.spacing.line + l.spacing.after),
            })
            .collect();
        let places = place_all(&wanted, &chains, &acrosses, &metrics, padding);
        let n = lines.len();
        let content = f64::from(container.width);
        let mut state = self.ivars().state.borrow_mut();
        for (((p, lines), m), place) in lines.into_iter().zip(metrics).zip(places) {
            let (left, right) = used_x(&lines, place.as_ref(), content, padding);
            let entry = Entry {
                lead: m.lead as f32,
                height: m.height as f32,
                trail: m.trail as f32,
                left,
                right,
                lines: Some(Arc::new(lines)),
                place: place.map(Arc::new),
            };
            // What views draw again: from here down if it moved the text
            // below, else itself.
            let before = state.cache.get(p);
            let changed = (entry.extent() - before.extent()).abs() > 1e-6;
            if (entry.left, entry.right) != (before.left, before.right) {
                state.widened = true;
            }
            state.cache.set(p, entry);
            if changed {
                state.mark_moved(p);
            } else {
                state.mark_redraw(p);
            }
        }
        if max_lines > 0 {
            limit_lines(&mut state.cache, max_lines, content, padding);
        }
        n
    }

    /// Have the storage fix the attributes of paragraphs `ps` where it put
    /// fixing off, before they are laid out.
    fn fix_attributes_of(&self, ps: &[usize]) {
        let (Some(&lo), Some(&hi)) = (ps.iter().min(), ps.iter().max()) else { return };
        let Some(storage) = self.ivars().storage.borrow().load() else { return };
        let range = self.with_text(|t| {
            let (a, b) = (t.locate_paragraph(lo), t.locate_paragraph(hi));
            a.start..b.start + t.para(b).len16() as usize
        });
        if let Some(range) = range {
            super::text_storage::ensure_fixed(&storage, range);
        }
    }

    /// Resolve the attributes of every run of paragraphs `ps`.
    fn resolve_runs(&self, ps: &[usize]) {
        let ids = self.with_text(|t| {
            let mut ids: Vec<AttrId> = Vec::new();
            for &p in ps {
                let at = t.locate_paragraph(p);
                ids.extend(t.para(at).runs().iter().map(|r| r.attrs));
            }
            ids.sort_unstable();
            ids.dedup();
            ids
        });
        if let Some(ids) = ids {
            self.resolve(&ids);
        }
    }

    /// The text blocks of paragraphs `ps`, their attributes resolved.
    fn chains_of(&self, ps: &[usize]) -> Vec<Option<Chain>> {
        self.with_text(|t| {
            let state = self.ivars().state.borrow();
            let n = t.paragraph_count();
            ps.iter()
                .map(|&p| {
                    let id =
                        (p < n).then(|| t.para(t.locate_paragraph(p)).runs().first().map(|r| r.attrs)).flatten()?;
                    state.chains.get(id as usize).cloned().flatten()
                })
                .collect()
        })
        .unwrap_or_else(|| vec![None; ps.len()])
    }

    /// Resolve the attributes of paragraphs `ps` (their first runs').
    fn resolve_paras(&self, ps: &[usize]) {
        let ids = self.with_text(|t| {
            let n = t.paragraph_count();
            let mut ids: Vec<AttrId> = ps
                .iter()
                .filter(|&&p| p < n)
                .filter_map(|&p| t.para(t.locate_paragraph(p)).runs().first().map(|r| r.attrs))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        });
        if let Some(ids) = ids {
            self.resolve(&ids);
        }
    }

    /// Paragraph `p`'s text blocks, its attributes resolved.
    fn chain(&self, p: usize) -> Option<Chain> {
        self.with_text(|t| {
            if p >= t.paragraph_count() {
                return None;
            }
            let id = t.para(t.locate_paragraph(p)).runs().first()?.attrs;
            self.ivars().state.borrow().chains.get(id as usize).cloned().flatten()
        })
        .flatten()
    }

    /// The paragraphs of the table row paragraph `p` is in.
    fn row_of(&self, p: usize) -> Option<Range<usize>> {
        let chain = self.chain(p)?;
        blocks::row_key(&chain)?;
        let n = self.with_text(Storage::paragraph_count).unwrap_or(0);
        let same = |q: usize| {
            self.resolve_paras(&[q]);
            self.chain(q).is_some_and(|c| blocks::same_table(&chain, &c).1)
        };
        let mut a = p;
        while a > 0 && same(a - 1) {
            a -= 1;
        }
        let mut b = p + 1;
        while b < n && same(b) {
            b += 1;
        }
        Some(a..b)
    }

    /// After an edit of paragraphs `k0..=k1`: when blocks are about, the
    /// paragraphs around them (and the rest of their table rows) are laid
    /// out again, since which blocks start and end where may have changed;
    /// the first of them, if any.
    fn unlay_around(&self, k0: usize, k1: usize) -> Option<usize> {
        let n = self.with_text(Storage::paragraph_count).unwrap_or(0);
        if n == 0 {
            return None;
        }
        let (lo, hi) = (k0.saturating_sub(1), (k1 + 1).min(n - 1));
        let edges = [lo, k0, k1.min(n - 1), hi];
        self.resolve_paras(&edges);
        if !edges.iter().any(|&p| self.chain(p).is_some()) {
            return None;
        }
        let mut paras: Vec<usize> = edges.to_vec();
        for p in edges {
            if let Some(r) = self.row_of(p) {
                paras.extend(r);
            }
        }
        let first = paras.iter().copied().min();
        let mut state = self.ivars().state.borrow_mut();
        for p in paras {
            state.cache.unlay(p);
        }
        first
    }

    /// Whether the paragraphs holding characters `range` are laid out.
    pub(crate) fn is_laid(&self, range: Range<usize>) -> bool {
        let Some((k0, k1)) = self.with_text(|t| {
            let len = t.len();
            (t.locate(range.start.min(len)).para, t.locate(range.end.min(len)).para)
        }) else {
            return false;
        };
        let state = self.ivars().state.borrow();
        (k0..=k1).all(|p| p >= state.cache.len() || state.cache.get(p).is_laid())
    }

    /// Lay out everything; whether anything was.
    pub(crate) fn ensure_all(&self) -> bool {
        let n = self.ivars().state.borrow().cache.len();
        let mut laid = false;
        while self.lay_out(0..n, 256) > 0 {
            laid = true;
        }
        laid
    }

    /// Lay out the paragraphs holding `range` (and, with contiguous layout,
    /// all before them); whether any was.
    pub(crate) fn ensure_chars(&self, range: Range<usize>) -> bool {
        let Some((k0, k1)) = self.with_text(|t| {
            let len = t.len();
            (t.locate(range.start.min(len)).para, t.locate(range.end.min(len)).para)
        }) else {
            return false;
        };
        let from = if self.ivars().non_contiguous.get() { k0 } else { 0 };
        let mut laid = false;
        while self.lay_out(from..k1 + 1, 256) > 0 {
            laid = true;
        }
        laid
    }

    /// Lay out the paragraphs between heights `y0` and `y1` (and, with
    /// contiguous layout, all before them). Laying a paragraph out changes
    /// where the ones after it are, so this goes until those found are
    /// laid out. Whether any was.
    pub(crate) fn ensure_y(&self, y0: f64, y1: f64) -> bool {
        let mut laid = false;
        for _ in 0..16 {
            let (p0, p1) = {
                let state = self.ivars().state.borrow();
                (state.cache.para_at_y(y0), state.cache.para_at_y(y1))
            };
            let from = if self.ivars().non_contiguous.get() { p0 } else { 0 };
            if self.lay_out(from..p1 + 1, 256) == 0 {
                break;
            }
            laid = true;
        }
        laid
    }

    fn first_unlaid(&self) -> usize {
        let p = self.ivars().state.borrow().cache.next_unlaid(0, usize::MAX);
        match p {
            None => self.text_len(),
            Some(p) => self.with_text(|t| t.locate_paragraph(p).start).unwrap_or(0),
        }
    }

    // Idle layout.

    /// Lay the rest out when the main run loop is idle: only for a manager
    /// whose text a view shows, which is the main thread's as views are;
    /// others lay out only on demand, on whatever thread uses them.
    fn schedule_idle(&self) {
        let iv = self.ivars();
        if !iv.background_layout.get() || MainThreadMarker::new().is_none() || self.views().is_empty() {
            return;
        }
        let current = iv.idle.take();
        if let Some(c) = current {
            iv.idle.set(Some(c));
            return;
        }
        let rl = runloop::current();
        let weak = Weak::from_retained(&self.retain());
        let id = rl.add_observer(&[Mode::COMMON], Activity::BEFORE_WAITING | Activity::EXIT, IDLE_ORDER, move |_| {
            if let Some(this) = weak.load() {
                this.idle_slice();
            }
        });
        iv.idle.set(Some((id, rl)));
    }

    /// Lay out what remains for a few milliseconds; stop observing when
    /// nothing does (or no view shows the text any more), else make the
    /// loop come round again.
    fn idle_slice(&self) {
        if MainThreadMarker::new().is_none() || self.views().is_empty() {
            self.stop_idle();
            return;
        }
        let deadline = Instant::now() + IDLE_SLICE;
        let n = self.ivars().state.borrow().cache.len();
        loop {
            if self.lay_out(0..n, 16) == 0 {
                self.stop_idle();
                self.redisplay(false);
                self.completed(true);
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.redisplay(true);
        runloop::current().wake();
    }

    fn stop_idle(&self) {
        if let Some((id, rl)) = self.ivars().idle.take() {
            rl.remove_observer(id);
        }
    }

    // Geometry.

    /// The line holding `index` (the line before, where a wrapped line ends
    /// there, with `upstream`), its paragraph laid out.
    pub(crate) fn line_at(&self, index: usize, upstream: bool) -> Option<LineAt> {
        let (para, start) = self.with_text(|t| {
            let at = t.locate(index.min(t.len()));
            (at.para, at.start)
        })?;
        self.ensure_para(para);
        let state = self.ivars().state.borrow();
        let entry = state.cache.get(para);
        // A paragraph past a container's last line has none.
        let lines = entry.lines.clone().filter(|l| !l.lines.is_empty())?;
        let rel = (index.min(start + lines.len as usize) - start) as u32;
        // None past the lines a container's limit left.
        let line = lines.line_at(rel, upstream)?;
        Some(LineAt {
            para,
            start,
            top: state.cache.top(para),
            lead: f64::from(entry.lead),
            trail: f64::from(entry.trail),
            left: entry.place.as_ref().map_or(0.0, |p| f64::from(p.left)),
            width: entry.place.as_ref().map(|p| f64::from(p.width)),
            lines,
            index: line,
        })
    }

    fn ensure_para(&self, para: usize) {
        let from = if self.ivars().non_contiguous.get() { para } else { 0 };
        while self.lay_out(from..para + 1, 256) > 0 {}
    }

    /// Every line of paragraph `para` (laid out), in order.
    fn lines_of(&self, para: usize) -> Vec<LineAt> {
        self.ensure_para(para);
        let Some(start) = self.with_text(|t| t.locate_paragraph(para).start) else { return Vec::new() };
        let state = self.ivars().state.borrow();
        let entry = state.cache.get(para);
        let Some(lines) = entry.lines.clone() else { return Vec::new() };
        let top = state.cache.top(para);
        (0..lines.lines.len())
            .map(|index| LineAt {
                para,
                start,
                top,
                lead: f64::from(entry.lead),
                trail: f64::from(entry.trail),
                left: entry.place.as_ref().map_or(0.0, |p| f64::from(p.left)),
                width: entry.place.as_ref().map(|p| f64::from(p.width)),
                lines: lines.clone(),
                index,
            })
            .collect()
    }

    /// The lines covering characters `range`, in order (the line holding
    /// its start, for an empty range).
    pub(crate) fn lines_in(&self, range: Range<usize>) -> Vec<LineAt> {
        let Some((k0, k1)) = self.with_text(|t| {
            let len = t.len();
            (t.locate(range.start.min(len)).para, t.locate(range.end.min(len)).para)
        }) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in k0..=k1 {
            for l in self.lines_of(p) {
                let r = l.range();
                let touches = if range.is_empty() {
                    r.start <= range.start && (range.start < r.end || (l.is_last() && range.start <= r.end))
                } else {
                    r.start < range.end && r.end > range.start
                };
                if touches {
                    out.push(l);
                }
            }
        }
        out
    }

    /// The paragraphs from height `y0` to `y1` (whole table rows), laid
    /// out.
    fn paras_in_y(&self, y0: f64, y1: f64) -> Range<usize> {
        self.ensure_y(y0, y1);
        let state = self.ivars().state.borrow();
        let p0 = state.cache.row_start(state.cache.para_at_y(y0));
        p0..state.cache.para_at_y(y1) + 1
    }

    /// The lines from height `y0` to `y1`, laid out.
    pub(crate) fn lines_in_y(&self, y0: f64, y1: f64) -> Vec<LineAt> {
        let r = self.paras_in_y(y0, y1);
        let (p0, p1) = (r.start, r.end - 1);
        let mut out = Vec::new();
        for p in p0..=p1 {
            for l in self.lines_of(p) {
                let (a, b) = l.fragment_span();
                if b > y0 && a <= y1 {
                    out.push(l);
                }
            }
        }
        out
    }

    fn padding(&self) -> f64 {
        self.ivars().state.borrow().geometry.padding
    }

    fn container_width(&self) -> f64 {
        self.ivars().state.borrow().geometry.size.width
    }

    /// A line's fragment and used rects.
    fn fragment_of(&self, l: &LineAt) -> Fragment {
        let (top, mut bottom) = l.fragment_span();
        let line = l.line();
        let p = self.padding();
        let last_para = self.with_text(Storage::paragraph_count).unwrap_or(1) - 1;
        let mut used_bottom = bottom;
        if l.is_last() {
            if l.para == last_para {
                // The text's last line: no spacing after it.
                bottom = l.line_top() + f64::from(line.height);
                used_bottom = bottom;
            } else {
                used_bottom -= f64::from(l.lines.spacing.after);
            }
        }
        let content = l.width.unwrap_or(self.container_width() - 2.0 * p);
        let (x0, x1) = line_used_x(line, &l.lines, l.left, content, p);
        let used = rect(x0, l.line_top(), x1 - x0, (used_bottom - l.line_top()).max(f64::from(line.height)));
        let width = l.width.map_or(self.container_width(), |w| w + 2.0 * p);
        Fragment { rect: rect(l.left, top, width, bottom - top), used, range: l.range() }
    }

    fn fragment(&self, index: usize, range: *mut NSRange, used: bool) -> NSRect {
        let len = self.text_len();
        let found = (index < len).then(|| self.line_at(index, false)).flatten();
        let (r, out) = match found {
            Some(l) => {
                let f = self.fragment_of(&l);
                (ns_range(f.range), if used { f.used } else { f.rect })
            }
            None => (NSRange::new(0, 0), NSRect::ZERO),
        };
        if !range.is_null() {
            // SAFETY: the caller passes a valid pointer or null.
            unsafe { *range = r };
        }
        out
    }

    /// The line fragments of `range`'s lines.
    pub(crate) fn fragments_in(&self, range: Range<usize>) -> Vec<Fragment> {
        let len = self.text_len();
        let range = range.start.min(len)..range.end.min(len);
        if range.is_empty() {
            return Vec::new();
        }
        self.lines_in(range).iter().filter(|l| !l.range().is_empty()).map(|l| self.fragment_of(l)).collect()
    }

    /// The extra line fragment's rect (or its used rect), or an empty rect
    /// at the text's bottom when there is none.
    fn extra(&self, used: bool) -> NSRect {
        let has = self.with_text(|t| t.is_empty() || t.para(t.locate_paragraph(t.paragraph_count() - 1)).len16() == 0);
        if has != Some(true) {
            let h = self.ivars().state.borrow().cache.height();
            return rect(0.0, h, 0.0, 0.0);
        }
        let last = self.with_text(Storage::paragraph_count).unwrap_or(1) - 1;
        let Some(l) = self.lines_of(last).into_iter().next() else { return NSRect::ZERO };
        let f = self.fragment_of(&l);
        if used { f.used } else { f.rect }
    }

    /// The union of the used rects: the text's extent, the extra line
    /// fragment's included.
    fn used_rect(&self) -> NSRect {
        let state = self.ivars().state.borrow();
        let height = state.cache.height();
        let (left, right) = state.cache.used_x();
        let (left, right) = if left <= right { (f64::from(left), f64::from(right)) } else { (0.0, 0.0) };
        rect(left, 0.0, right - left, height)
    }

    /// Where glyph `index` sits: its left edge from the line fragment's
    /// origin, and the baseline from its top.
    fn location(&self, index: usize) -> NSPoint {
        let Some(l) = self.line_at(index, false) else { return NSPoint::ZERO };
        let line = l.line();
        let (x, _) = line.caret_x((index - l.start) as u32);
        let (top, _) = l.fragment_span();
        let x = self.padding() + f64::from(x);
        NSPoint::new(x, l.line_top() - top + f64::from(line.baseline))
    }

    /// The rects that show `range` selected (container points): the
    /// lines' spans, and on to the line's end where the range takes in a
    /// separator.
    pub(crate) fn selection_rects(&self, range: Range<usize>) -> Vec<NSRect> {
        if range.is_empty() {
            return Vec::new();
        }
        let lines = self.lines_in(range.clone());
        self.spans_of(&lines, range)
    }

    /// [`selection_rects`](Self::selection_rects) of only the lines
    /// between heights `y0` and `y1`: what a view shows of a selection,
    /// without laying out or measuring the rest of it.
    pub(crate) fn selection_rects_in(&self, range: Range<usize>, y0: f64, y1: f64) -> Vec<NSRect> {
        if range.is_empty() {
            return Vec::new();
        }
        let lines: Vec<LineAt> = self
            .lines_in_y(y0, y1)
            .into_iter()
            .filter(|l| {
                let r = l.range();
                r.start < range.end && r.end > range.start
            })
            .collect();
        self.spans_of(&lines, range)
    }

    /// The rects of `range`'s characters on its first line (its separator
    /// left out), and the part of `range` that line holds: where an input
    /// method's rect for a range, or scrolling to one, looks. Nothing after
    /// that line is laid out or measured.
    pub(crate) fn first_line_rects(&self, range: Range<usize>) -> (Vec<NSRect>, Range<usize>) {
        let Some(l) = self.line_at(range.start, false) else { return (Vec::new(), range) };
        let end = range.end.min(l.range().end).max(range.start);
        let content = range.end.min(l.content_end()).max(range.start);
        (self.spans_of(std::slice::from_ref(&l), range.start..content), range.start..end)
    }

    fn spans_of(&self, lines: &[LineAt], range: Range<usize>) -> Vec<NSRect> {
        spans_of(lines, range, self.padding(), self.container_width())
    }

    fn bounding_rect(&self, range: Range<usize>) -> NSRect {
        let rects = self.selection_rects(range.clone());
        let rects = if rects.is_empty() {
            // An empty range: the caret's place.
            self.line_at(range.start, false)
                .map(|l| {
                    let (x, _) = l.line().caret_x((range.start - l.start) as u32);
                    vec![rect(self.padding() + l.left + f64::from(x), l.line_top(), 0.0, f64::from(l.line().height))]
                })
                .unwrap_or_default()
        } else {
            rects
        };
        rects.into_iter().reduce(union).unwrap_or(NSRect::ZERO)
    }

    /// The glyphs of the lines whose fragments meet `r`.
    fn range_for_rect(&self, r: NSRect, lay_out: bool) -> Range<usize> {
        let (y0, y1) = (r.origin.y, r.origin.y + r.size.height);
        let lines = if lay_out {
            self.lines_in_y(y0, y1)
        } else {
            let (p0, p1) = {
                let state = self.ivars().state.borrow();
                (state.cache.para_at_y(y0), state.cache.para_at_y(y1))
            };
            let laid = (p0..=p1).all(|p| self.ivars().state.borrow().cache.get(p).is_laid());
            if !laid {
                return 0..0;
            }
            self.lines_in_y(y0, y1)
        };
        let mut out: Option<Range<usize>> = None;
        for l in lines {
            let lr = l.range();
            out = Some(match out {
                None => lr,
                Some(o) => o.start.min(lr.start)..o.end.max(lr.end),
            });
        }
        out.unwrap_or(0..0)
    }

    /// The line at point (`x`, `y`) (the first above the text, the last
    /// below it), laid out. In a table row, the line whose fragment's
    /// height holds `y`, the one across `x` where more do, else the nearest
    /// across; with none there, the nearest down.
    pub(crate) fn line_at_point(&self, x: f64, y: f64) -> Option<LineAt> {
        let row = self.paras_in_y(y, y);
        if row.len() <= 1 {
            let lines = self.lines_of(row.start);
            let at = lines.iter().position(|l| y < l.fragment_span().1).unwrap_or(lines.len().saturating_sub(1));
            return lines.into_iter().nth(at);
        }
        let p = self.padding();
        let lines: Vec<LineAt> = row.flat_map(|q| self.lines_of(q)).collect();
        let across = |l: &LineAt| {
            let (x0, x1) = (l.left, l.left + l.width.unwrap_or(0.0) + 2.0 * p);
            if x < x0 {
                x0 - x
            } else if x > x1 {
                x - x1
            } else {
                0.0
            }
        };
        let down = |l: &LineAt| {
            let (a, b) = l.fragment_span();
            if y < a {
                a - y
            } else if y >= b {
                y - b + f64::EPSILON
            } else {
                0.0
            }
        };
        lines.into_iter().min_by(|a, b| {
            let ka = (down(a) > 0.0, down(a), across(a));
            let kb = (down(b) > 0.0, down(b), across(b));
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// The glyph under point `p` and how far through it `p` is: past a
    /// line's end, the line's last glyph, all the way through; below the
    /// text, the last glyph.
    fn glyph_at(&self, p: NSPoint) -> (usize, f64) {
        let len = self.text_len();
        if len == 0 {
            return (0, 0.0);
        }
        // Below the text: the last glyph, all the way through.
        self.ensure_y(p.y, p.y);
        if p.y >= self.ivars().state.borrow().cache.height() {
            return (len - 1, 1.0);
        }
        let Some(l) = self.line_at_point(p.x, p.y) else { return (0, 0.0) };
        let range = l.range();
        if range.is_empty() {
            // The extra line fragment: the last glyph.
            return (len - 1, 1.0);
        }
        let line = l.line();
        let x = (p.x - self.padding() - l.left) as f32;
        let end_edge = if line.rtl { line.x } else { line.x + line.width };
        let past = if line.rtl { x < end_edge } else { x >= end_edge };
        if past {
            return (range.end - 1, 1.0);
        }
        let hit = line.hit(x);
        (l.start + hit.character as usize, f64::from(hit.fraction))
    }

    /// Where an insertion point goes for a point: the index, and whether it
    /// belongs at the end of the line above (a point past a wrapped line's
    /// end).
    pub(crate) fn insertion_index(&self, p: NSPoint) -> (usize, bool) {
        let len = self.text_len();
        let Some(l) = self.line_at_point(p.x, p.y) else { return (0, false) };
        if p.y < 0.0 && l.para == 0 && l.index == 0 {
            return (0, false);
        }
        let height = self.ivars().state.borrow().cache.height();
        if p.y >= height {
            return (len, false);
        }
        let hit = l.line().hit((p.x - self.padding() - l.left) as f32);
        ((l.start + hit.index as usize).min(len), hit.upstream)
    }

    /// The caret's rect (x, top, width 1, height) for an insertion point
    /// at `index`.
    pub(crate) fn caret_rect(&self, index: usize, upstream: bool) -> NSRect {
        let Some(l) = self.line_at(index, upstream) else { return rect(self.padding(), 0.0, 1.0, 14.0) };
        let line = l.line();
        let (x, _) = line.caret_x((index - l.start) as u32);
        rect(self.padding() + l.left + f64::from(x), l.line_top(), 1.0, f64::from(line.height))
    }

    /// The text's height, estimates included.
    pub(crate) fn height(&self) -> f64 {
        self.ivars().state.borrow().cache.height()
    }

    /// How far right the lines laid out reach, padding included.
    pub(crate) fn used_width(&self) -> f64 {
        f64::from(self.ivars().state.borrow().cache.used_x().1)
    }

    fn container_for(&self, index: usize, range: *mut NSRange) -> Option<Retained<NSTextContainer>> {
        let first = self.ivars().containers.borrow().first().cloned();
        if !range.is_null() {
            let r = if first.is_some() { NSRange::new(0, self.text_len()) } else { NSRange::new(0, 0) };
            // SAFETY: the caller passes a valid pointer or null.
            unsafe { *range = r };
        }
        let _ = index;
        first
    }

    // Drawing.

    /// Record the lines of `range` with the container's origin at `origin`
    /// in the view being drawn: their backgrounds, or their glyphs and
    /// decorations.
    fn draw(&self, range: Range<usize>, origin: NSPoint, backgrounds: bool) {
        if !crate::graphics::recording() {
            return;
        }
        let p = self.padding();
        let lines = self.lines_in(range);
        if backgrounds && let (Some(first), Some(last)) = (lines.first(), lines.last()) {
            self.draw_blocks(first.para..last.para + 1, origin);
        }
        for l in &lines {
            let left = NSPoint::new(origin.x + p + l.left, origin.y + l.line_top());
            crate::string_drawing::record_line(l.line(), left, backgrounds);
        }
        if backgrounds {
            self.draw_temporary(&lines, origin);
        }
    }

    /// Text block `block`'s layout rect and bounds, and the glyphs it holds:
    /// the paragraphs around glyph `index` in it.
    fn block_rects(&self, block: &objc2_app_kit::NSTextBlock, index: usize) -> Option<(NSRect, NSRect, Range<usize>)> {
        if index >= self.text_len() {
            return None;
        }
        let (p, n) = self.with_text(|t| (t.locate(index).para, t.paragraph_count()))?;
        let has = |q: usize| {
            self.resolve_paras(&[q]);
            self.chain(q).is_some_and(|c| c.iter().any(|b| std::ptr::eq(&**b, block)))
        };
        if !has(p) {
            return None;
        }
        let (mut s, mut e) = (p, p);
        while s > 0 && has(s - 1) {
            s -= 1;
        }
        while e + 1 < n && has(e + 1) {
            e += 1;
        }
        self.ensure_para(e);
        while self.lay_out(s..e + 1, 256) > 0 {}
        let chars = self.with_text(|t| {
            t.paragraph_range(t.locate_paragraph(s).start).start..t.paragraph_range(t.locate_paragraph(e).start).end
        })?;
        let state = self.ivars().state.borrow();
        let mut layout = None;
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for q in s..=e {
            let Some(place) = state.cache.get(q).place.clone() else { continue };
            let top = state.cache.flow_top(q);
            for sl in place.slices.iter().filter(|sl| std::ptr::eq(&*sl.block, block)) {
                if layout.is_none()
                    && let Some(lt) = sl.ltop
                {
                    let y = top + lt;
                    layout = Some(rect(sl.lx, y, sl.lw, (state.geometry.size.height - y).max(0.0)));
                }
                let (y0, y1) = (top + sl.y0, top + sl.y1);
                bounds = Some(match bounds {
                    None => (sl.x0, y0, sl.x1, y1),
                    Some((x0, a, x1, b)) => (x0.min(sl.x0), a.min(y0), x1.max(sl.x1), b.max(y1)),
                });
            }
        }
        let (x0, y0, x1, y1) = bounds?;
        Some((layout.unwrap_or(NSRect::ZERO), rect(x0, y0, x1 - x0, y1 - y0), chars))
    }

    /// Record the text blocks' backgrounds and borders between heights
    /// `y0` and `y1`, the container's origin at `origin`: what a text view
    /// draws first.
    pub(crate) fn draw_blocks_in_y(&self, y0: f64, y1: f64, origin: NSPoint) {
        if !crate::graphics::recording() {
            return;
        }
        let paras = self.paras_in_y(y0, y1);
        self.draw_blocks(paras, origin);
    }

    /// Record the boxes of the blocks paragraphs `paras` are in.
    fn draw_blocks(&self, paras: Range<usize>, origin: NSPoint) {
        let placed: Vec<(f64, Arc<Place>)> = {
            let state = self.ivars().state.borrow();
            let from = state.cache.row_start(paras.start);
            (from..paras.end.min(state.cache.len()))
                .filter_map(|q| state.cache.get(q).place.clone().map(|pl| (state.cache.flow_top(q), pl)))
                .collect()
        };
        if placed.is_empty() {
            return;
        }
        let slices: Vec<(f64, &[blocks::Slice])> = placed.iter().map(|(t, pl)| (*t, pl.slices.as_slice())).collect();
        blocks::draw_slices(&slices, origin);
    }

    /// Record the lines between heights `y0` and `y1` (container points),
    /// with the container's origin at `origin`: what a text view draws.
    pub(crate) fn draw_rect(&self, y0: f64, y1: f64, origin: NSPoint, backgrounds: bool) {
        let p = self.padding();
        let lines = self.lines_in_y(y0, y1);
        for l in &lines {
            let left = NSPoint::new(origin.x + p + l.left, origin.y + l.line_top());
            crate::string_drawing::record_line(l.line(), left, backgrounds);
        }
        if backgrounds {
            self.draw_temporary(&lines, origin);
        }
    }
}

/// The rects that show `range` selected on `lines` (container points), in a
/// container `container_width` wide with padding `pad`: the lines' spans,
/// and on to the line's end where the range takes in a separator.
pub(crate) fn spans_of(lines: &[LineAt], range: Range<usize>, pad: f64, container_width: f64) -> Vec<NSRect> {
    let mut out = Vec::new();
    let container_right = container_width - pad;
    for l in lines {
        let p = pad + l.left;
        let right = l.width.map_or(container_right, |w| p + w);
        let line = l.line();
        let (from, to) = ((range.start.max(l.start) - l.start) as u32, (range.end - l.start.min(range.end)) as u32);
        let (y, h) = (l.line_top(), f64::from(line.height));
        for (x0, x1) in line.spans(from..to) {
            out.push(rect(p + f64::from(x0), y, f64::from(x1 - x0), h));
        }
        let end = line.content_end();
        if to > end && from <= end {
            let x0 = if line.rtl { p } else { p + f64::from(line.x + line.width) };
            let x1 = if line.rtl { p + f64::from(line.x) } else { right };
            if x1 > x0 {
                out.push(rect(x0, y, x1 - x0, h));
            }
        }
    }
    out
}

/// A paragraph's text blocks, and its neighbours'.
type Chains = (Option<Chain>, Option<Chain>, Option<Chain>);

/// The runs of `paras` (numbers, in order) that are one table row each,
/// and the paragraphs outside tables: (first, last, cell level) with no
/// level for the latter; paragraphs in no blocks are left out.
fn groups(paras: &[usize], chains: &[Chains]) -> Vec<(usize, usize, Option<usize>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < paras.len() {
        let Some(chain) = &chains[i].0 else {
            i += 1;
            continue;
        };
        let Some((k, _, _)) = blocks::row_key(chain) else {
            out.push((i, i, None));
            i += 1;
            continue;
        };
        let mut j = i;
        while j + 1 < paras.len()
            && paras[j + 1] == paras[j] + 1
            && chains[j + 1].0.as_ref().is_some_and(|c| blocks::same_table(chain, c).1)
        {
            j += 1;
        }
        out.push((i, j, Some(k)));
        i = j + 1;
    }
    out
}

/// Where each paragraph's blocks put its lines across (none for one in
/// no blocks), in content `width` from the padding edge.
fn across_all(paras: &[usize], chains: &[Chains], padding: f64, width: f64) -> Vec<Option<blocks::Across>> {
    let mut out: Vec<Option<blocks::Across>> = vec![None; paras.len()];
    let plain = |_: usize, x: f64, w: f64| (x, w);
    for (i, j, level) in groups(paras, chains) {
        let chain_of = |m: usize| chains[m].0.as_deref().unwrap_or(&[]);
        let Some(k) = level else {
            out[i] = Some(blocks::across(chain_of(i), padding, width, &plain));
            continue;
        };
        // The table's enclosing rect, then the row's cells in it.
        let outer = blocks::across(&chain_of(i)[..k], padding, width, &plain);
        let mut cells: Vec<&objc2_app_kit::NSTextBlock> = Vec::new();
        for m in i..=j {
            let c: &objc2_app_kit::NSTextBlock = &chain_of(m)[k];
            if !cells.iter().any(|&d| std::ptr::eq(d, c)) {
                cells.push(c);
            }
        }
        let rects = blocks::row_columns(&cells, outer.x, outer.w);
        for (m, slot) in out.iter_mut().enumerate().take(j + 1).skip(i) {
            let own: &objc2_app_kit::NSTextBlock = &chain_of(m)[k];
            let rect = cells.iter().position(|&c| std::ptr::eq(c, own)).map_or((outer.x, outer.w), |q| rects[q]);
            let cell = |lvl: usize, x: f64, w: f64| if lvl == k { rect } else { (x, w) };
            *slot = Some(blocks::across(chain_of(m), padding, width, &cell));
        }
    }
    out
}

/// Where each paragraph goes among its blocks (none for one in no blocks).
fn place_all(
    paras: &[usize],
    chains: &[Chains],
    acrosses: &[Option<blocks::Across>],
    metrics: &[Metrics],
    padding: f64,
) -> Vec<Option<Place>> {
    let mut out: Vec<Option<Place>> = vec![None; paras.len()];
    let empty = blocks::Across::default();
    let chain_of = |c: &Option<Chain>| -> Vec<Retained<objc2_app_kit::NSTextBlock>> {
        c.as_deref().map(<[_]>::to_vec).unwrap_or_default()
    };
    for (i, j, level) in groups(paras, chains) {
        let (prev, next) = (chain_of(&chains[i].1), chain_of(&chains[j].2));
        match level {
            None => {
                let own = chains[i].0.as_deref().unwrap_or(&[]);
                let across = acrosses[i].as_ref().unwrap_or(&empty);
                out[i] = Some(blocks::place_flow(own, across, &prev, &next, metrics[i], padding));
            }
            Some(k) => {
                let members: Vec<RowMember<'_>> = (i..=j)
                    .map(|m| RowMember {
                        chain: chains[m].0.as_deref().unwrap_or(&[]),
                        across: acrosses[m].as_ref().unwrap_or(&empty),
                        metrics: metrics[m],
                    })
                    .collect();
                for (m, place) in (i..).zip(blocks::place_row(&members, k, &prev, &next, padding)) {
                    out[m] = Some(place);
                }
            }
        }
    }
    out
}

fn union(a: NSRect, b: NSRect) -> NSRect {
    let x0 = a.origin.x.min(b.origin.x);
    let y0 = a.origin.y.min(b.origin.y);
    let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    rect(x0, y0, x1 - x0, y1 - y0)
}

/// `text` as a secure field shows it: a bullet for each character, and a
/// zero-width space for each of its other UTF-16 units, so indexes stay
/// the text's. Separators stay, so paragraphs do.
fn mask(text: &str) -> String {
    use icu_segmenter::GraphemeClusterSegmenter;
    let mut out = String::with_capacity(text.len() * 3);
    let mut bounds = GraphemeClusterSegmenter::new().segment_str(text).peekable();
    let mut start = bounds.next().unwrap_or(0);
    for end in bounds {
        let cluster = &text[start..end];
        if cluster.chars().all(|c| matches!(c, '\n' | '\r' | '\u{2029}')) {
            out.push_str(cluster);
        } else {
            out.push('\u{2022}');
            let units: usize = cluster.chars().map(char::len_utf16).sum();
            for _ in 1..units {
                out.push('\u{200B}');
            }
        }
        start = end;
    }
    out
}

/// Keep at most `max` lines, over all paragraphs in order: those after
/// are cut, and paragraphs after the last line kept have none.
fn limit_lines(cache: &mut LayoutCache, max: usize, content: f64, padding: f64) {
    let mut left = max;
    for p in 0..cache.len() {
        let e = cache.get(p).clone();
        let Some(lines) = &e.lines else { continue };
        if lines.lines.len() <= left {
            left -= lines.lines.len();
            continue;
        }
        let mut cut = (**lines).clone();
        cut.lines.truncate(left);
        left = 0;
        let height = cut.height();
        let (l, r) = used_x(&cut, e.place.as_deref(), content, padding);
        cache.set(p, Entry { height, left: l, right: r, lines: Some(Arc::new(cut)), ..e });
    }
}

/// The left and right edges of a line's used rect, from the container's
/// left: its text with the padding at each end, kept within the width the
/// line may take (the paragraph's `content` width less its tail indent),
/// so a clipped line, or spaces hanging past a wrap or an alignment edge,
/// reach no further. `left` is where the paragraph's content area starts
/// (a text block's), from the padding edge.
fn line_used_x(line: &Line, lines: &ParagraphLines, left: f64, content: f64, padding: f64) -> (f64, f64) {
    let tail = lines.style.tail_indent;
    let right = if tail > 0.0 { tail.min(content) } else { content + tail };
    let (lo, hi) = (left, left + right.max(0.0) + 2.0 * padding);
    let x0 = left + f64::from(line.x);
    let x1 = x0 + f64::from(line.width) + 2.0 * padding;
    let x0 = x0.clamp(lo, hi);
    (x0, x1.clamp(x0, hi.max(x0)))
}

/// The left and right edges of a paragraph's used rects (see
/// [`line_used_x`]), as a cache entry keeps them: (infinity, 0) for none.
fn used_x(lines: &ParagraphLines, place: Option<&Place>, content: f64, padding: f64) -> (f32, f32) {
    let (left, content) = match place {
        Some(pl) => (f64::from(pl.left), f64::from(pl.width)),
        None => (0.0, content),
    };
    let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
    for line in &lines.lines {
        let (a, b) = line_used_x(line, lines, left, content, padding);
        lo = lo.min(a);
        hi = hi.max(b);
    }
    if let Some(pl) = place {
        hi = hi.max(f64::from(pl.reach) + 2.0 * padding);
    }
    (lo as f32, hi as f32)
}

/// Attribute runs: UTF-16 ranges and their dictionaries.
type Runs = Vec<(Range<usize>, Retained<Dict>)>;

/// A copy of a storage of another class, read through its primitives.
fn mirror_of(storage: &NSTextStorage, table: &RefCell<AttrTable>) -> Storage {
    let len = storage.length();
    let (text, runs) = read_range(storage, 0..len);
    let runs: Vec<Run> =
        runs.iter().map(|(r, d)| Run { len: r.len() as u32, attrs: super::attrs::intern(table, Some(d)) }).collect();
    let mut s = Storage::new();
    s.replace_runs(0..0, &text, &runs);
    s
}

/// The text and attribute runs (UTF-16 ranges from `range`'s start) of a
/// storage's `range`, through its primitives.
fn read_range(storage: &NSTextStorage, range: Range<usize>) -> (String, Runs) {
    let string: Retained<NSString> = storage.string();
    let text = string.substringWithRange(ns_range(range.clone())).to_string();
    let mut runs = Vec::new();
    let mut i = range.start;
    while i < range.end {
        let mut r = NSRange::new(0, 0);
        // SAFETY: a primitive, with a valid out-parameter.
        let d = unsafe { storage.attributesAtIndex_effectiveRange(i, &mut r) };
        let end = (r.location + r.length).min(range.end).max(i + 1);
        runs.push((i - range.start..end - range.start, d));
        i = end;
    }
    (text, runs)
}
