//! `NSTextLayoutManager`: a content manager's elements laid out in a text
//! container as layout fragments, a fragment an element, through the
//! paragraph engine TextKit 1 uses (`layout`).
//!
//! **Where fragments are.** The layout manager keeps an index over the
//! document (`Seg`s in a `seq::Seq`): stretches of text it knows
//! only by an estimate of their height, and elements with their fragments,
//! laid out or not. A fragment's place is the height of what comes before
//! it, estimates included, so laying out only what is asked for (the
//! viewport, a range, a point) places it where the rest's estimates say,
//! as TextKit 2 does on macOS (non-contiguous layout; `usageBoundsForTextContainer`
//! is estimated for what isn't laid out). Estimates are TextKit 1's (a
//! line of the paragraph's font per container width of text, at half an
//! em a character), scaled by how the fragments laid out so far compared
//! with their estimates, so the document's height settles as more of it
//! is laid out. Text a Sidestep storage hasn't cut into paragraphs yet is
//! estimated a stretch at a time, as TextKit 1 does.
//!
//! **Elements.** A stretch known only by its estimate turns into elements
//! when something needs them: the layout manager asks the content manager
//! for the elements from an offset (its own content storage directly, else
//! `enumerateTextElementsFromLocation:options:usingBlock:`, so a
//! subclass's elements, grouping paragraphs or not, are what is laid out),
//! and asks the delegate's `textLayoutManager:textLayoutFragmentForLocation:inTextElement:`
//! for each one's fragment (else makes an `NSTextLayoutFragment`). Edits
//! (from the content storage) and `invalidateLayoutForRange:` turn what they
//! touch back into stretches, keeping their heights as estimates; an
//! element the content manager hands out again keeps its fragment (the
//! same object, laid out again), as on macOS.
//!
//! **Laying out** a fragment reads its element's text (a content storage
//! paragraph's straight from the storage's paragraph tree, anything else
//! through its attributed string), lays it out, and then asks the fragment
//! its `layoutFragmentFrame`, so a subclass's frame decides where the next
//! fragment goes. All of this happens on the thread that asks, like
//! TextKit 1's layout on demand; the viewport controller lays out only what
//! its bounds cover.
//!
//! **Where lines are** is where their fragment says it is: a line's place
//! for carets, selections, segments and hit tests is its fragment's
//! `layoutFragmentFrame` (a subclass's own, moved or padded), as drawing
//! puts it. A point in a fragment's frame below its lines (a subclass's
//! padding) finds the end of its last line, as selection navigation does
//! on macOS. An empty document has one fragment, its extra line fragment,
//! laid out when a caret, a range, the viewport or a size asks for it.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSTextContainer, NSTextContentManagerEnumerationOptions, NSTextElement, NSTextLayoutFragment,
    NSTextLayoutFragmentEnumerationOptions, NSTextLayoutManager, NSTextLayoutManagerSegmentOptions,
    NSTextLayoutManagerSegmentType, NSTextRange, NSTextSelection, NSTextSelectionNavigation,
    NSTextSelectionNavigationLayoutOrientation, NSTextSelectionNavigationWritingDirection,
    NSTextViewportLayoutController,
};
use objc2_foundation::{NSArray, NSAttributedString, NSInteger, NSPoint, NSRange, NSRect, NSSize, NSString};

use super::SharedWeak;
use super::content::{self, NSTextContentStorageImpl};
use super::element;
use super::fragment::{self, FragmentIvars};
use super::layout::{self, Geometry, Laid, Resolved};
use super::location::{self, offset_of, span_of};
use super::seq::{Item, Metrics, Pos, Seq};
use crate::text::layout::Attrs;
use crate::textkit::attrs::Dict;
use crate::textkit::layout_manager::LineAt;
use crate::textkit::temporary::{self, Temporary};

sidestep_runtime::static_class!(pub(crate) NSTEXTLAYOUTMANAGER, NSTEXTLAYOUTMANAGER_META = "NSTextLayoutManager", || {
    let _ = NSTextLayoutManagerImpl::class();
});

/// Elements asked for at once when a stretch turns into elements.
const BATCH: usize = 16;

/// What comes before a fragment this near the document's start is laid
/// out before it, so short texts are laid out contiguously and their
/// positions are exact (TextKit 2 on macOS lays out from the top down to
/// a viewport near it too).
const CONTIGUOUS_PREFIX: usize = 16 * 1024;

/// Text an edit brings in beyond this many units is estimated afresh, a
/// paragraph (or a stretch of the storage) at a time, as new text is.
const LONG_EDIT: usize = 64 * 1024;

/// Fragments of elements laid out again kept for the elements' return.
const RECYCLED_MAX: usize = 4096;

/// A rendering attributes validator: a block taking the manager and a
/// fragment.
type Validator = dyn Fn(NonNull<NSTextLayoutManager>, NonNull<NSTextLayoutFragment>);

/// An element and its fragment in the index.
pub(crate) struct Slot {
    element: Retained<AnyObject>,
    fragment: Retained<NSTextLayoutFragment>,
    laid: bool,
    /// The frame's left and right edges, once laid out.
    left: f64,
    right: f64,
}

/// A stretch of the document: an element with its fragment, or text known
/// only by its height. The height is `fixed` (laid out, or kept from an
/// earlier layout) plus `raw` times the estimate factor.
pub(crate) struct Seg {
    len: usize,
    fixed: f64,
    raw: f64,
    slot: Option<Box<Slot>>,
}

impl Item for Seg {
    fn len(&self) -> usize {
        self.len
    }

    fn metrics(&self) -> Metrics {
        let laid = self.slot.as_ref().is_some_and(|s| s.laid);
        let (left, right) = match &self.slot {
            Some(s) if s.laid => (s.left, s.right),
            _ => (f64::INFINITY, f64::NEG_INFINITY),
        };
        Metrics { fixed: self.fixed, raw: self.raw, laid: usize::from(laid), left, right }
    }
}

impl Seg {
    fn unknown(len: usize, raw: f64) -> Seg {
        Seg { len, fixed: 0.0, raw, slot: None }
    }
}

/// What the views need to draw again since they last asked.
#[derive(Default)]
struct Damage {
    /// From this height down, everything moved.
    moved: Option<f64>,
    /// A stretch laid out again in place.
    redraw: Option<(f64, f64)>,
    /// The extent may have changed.
    resized: bool,
}

impl Damage {
    fn mark_moved(&mut self, y: f64) {
        self.moved = Some(self.moved.map_or(y, |m| m.min(y)));
        self.resized = true;
    }

    fn mark_redraw(&mut self, y0: f64, y1: f64) {
        self.redraw = Some(self.redraw.map_or((y0, y1), |(a, b)| (a.min(y0), b.max(y1))));
    }
}

struct State {
    index: Seq<Seg>,
    /// Estimates are scaled by this: laid-out heights over their estimates.
    factor: f64,
    measured: (f64, f64),
    geometry: Geometry,
    resolved: Resolved,
    /// Fragments of elements laid out again, by element, for their return.
    recycled: HashMap<usize, Retained<NSTextLayoutFragment>>,
    /// The empty document's extra line fragment.
    extra: Option<Retained<NSTextLayoutFragment>>,
    damage: Damage,
}

pub(crate) struct Ivars {
    content: RefCell<Weak<AnyObject>>,
    container: RefCell<Option<Retained<NSTextContainer>>>,
    delegate: RefCell<Weak<AnyObject>>,
    viewport: RefCell<Option<Retained<NSTextViewportLayoutController>>>,
    selections: RefCell<Option<Retained<NSArray<NSTextSelection>>>>,
    navigation: RefCell<Option<Retained<NSTextSelectionNavigation>>>,
    font_leading: Cell<bool>,
    suspicious: Cell<bool>,
    hyphenation: Cell<bool>,
    natural_alignment: Cell<bool>,
    queue: RefCell<Option<Retained<AnyObject>>>,
    validator: RefCell<Option<block2::RcBlock<Validator>>>,
    rendering: RefCell<Temporary>,
    /// Whether the usage bounds take in the estimate of what isn't laid
    /// out (a view sizing to the document asked for it).
    estimated: Cell<bool>,
    /// Whether an edit, an invalidation or new geometry changed layout
    /// since a view last asked (it keeps text in place on screen only
    /// when layout alone moved it).
    touched: Cell<bool>,
    /// The weak reference to itself its fragments share.
    me: OnceCell<SharedWeak>,
    state: RefCell<State>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextLayoutManager"]
    #[ivars = Ivars]
    pub(crate) struct NSTextLayoutManagerImpl;

    impl NSTextLayoutManagerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Ivars {
                content: RefCell::new(Weak::default()),
                container: RefCell::new(None),
                delegate: RefCell::new(Weak::default()),
                viewport: RefCell::new(None),
                selections: RefCell::new(None),
                navigation: RefCell::new(None),
                font_leading: Cell::new(true),
                suspicious: Cell::new(true),
                hyphenation: Cell::new(false),
                natural_alignment: Cell::new(false),
                queue: RefCell::new(None),
                validator: RefCell::new(None),
                rendering: RefCell::default(),
                estimated: Cell::new(false),
                touched: Cell::new(false),
                me: OnceCell::new(),
                state: RefCell::new(State {
                    index: Seq::new(vec![Seg::unknown(0, 0.0)]),
                    factor: 1.0,
                    measured: (0.0, 0.0),
                    geometry: Geometry { width: f32::INFINITY, padding: 5.0, font_leading: true },
                    resolved: Resolved::default(),
                    recycled: HashMap::new(),
                    extra: None,
                    damage: Damage::default(),
                }),
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

        #[unsafe(method(usesFontLeading))]
        fn uses_font_leading(&self) -> bool {
            self.ivars().font_leading.get()
        }

        #[unsafe(method(setUsesFontLeading:))]
        fn set_uses_font_leading(&self, on: bool) {
            if self.ivars().font_leading.replace(on) != on {
                self.geometry_changed();
            }
        }

        #[unsafe(method(limitsLayoutForSuspiciousContents))]
        fn limits_layout_for_suspicious_contents(&self) -> bool {
            self.ivars().suspicious.get()
        }

        #[unsafe(method(setLimitsLayoutForSuspiciousContents:))]
        fn set_limits_layout_for_suspicious_contents(&self, on: bool) {
            self.ivars().suspicious.set(on);
        }

        #[unsafe(method(usesHyphenation))]
        fn uses_hyphenation(&self) -> bool {
            self.ivars().hyphenation.get()
        }

        #[unsafe(method(setUsesHyphenation:))]
        fn set_uses_hyphenation(&self, on: bool) {
            self.ivars().hyphenation.set(on);
        }

        #[unsafe(method(resolvesNaturalAlignmentWithBaseWritingDirection))]
        fn resolves_natural_alignment(&self) -> bool {
            self.ivars().natural_alignment.get()
        }

        #[unsafe(method(setResolvesNaturalAlignmentWithBaseWritingDirection:))]
        fn set_resolves_natural_alignment(&self, on: bool) {
            self.ivars().natural_alignment.set(on);
        }

        #[unsafe(method_id(textContentManager))]
        fn text_content_manager(&self) -> Option<Retained<AnyObject>> {
            self.ivars().content.borrow().load()
        }

        #[unsafe(method(replaceTextContentManager:))]
        fn replace_text_content_manager(&self, manager: &AnyObject) {
            let keep = self.retain();
            let old = self.ivars().content.borrow().load();
            let me = keep.as_manager();
            if let Some(old) = old {
                // SAFETY: the content manager's own method.
                let _: () = unsafe { msg_send![&*old, removeTextLayoutManager: me] };
            }
            // SAFETY: the content manager's own method.
            let _: () = unsafe { msg_send![manager, addTextLayoutManager: me] };
        }

        #[unsafe(method_id(textContainer))]
        fn text_container(&self) -> Option<Retained<NSTextContainer>> {
            self.ivars().container.borrow().clone()
        }

        #[unsafe(method(setTextContainer:))]
        fn set_text_container(&self, container: Option<&NSTextContainer>) {
            let old = self.ivars().container.replace(container.map(|c| c.retain()));
            if let Some(old) = old
                && container.is_none_or(|c| !std::ptr::eq(c, &*old))
            {
                crate::textkit::container::set_text_layout_manager(&old, None);
            }
            if let Some(c) = container {
                crate::textkit::container::set_text_layout_manager(c, Some(self.as_manager()));
            }
            self.geometry_changed();
        }

        #[unsafe(method(usageBoundsForTextContainer))]
        fn usage_bounds_for_text_container(&self) -> NSRect {
            self.usage_bounds()
        }

        /// None until there is a container, as on macOS.
        #[unsafe(method_id(textViewportLayoutController))]
        fn text_viewport_layout_controller(&self) -> Option<Retained<NSTextViewportLayoutController>> {
            self.viewport_controller()
        }

        #[unsafe(method_id(layoutQueue))]
        fn layout_queue(&self) -> Option<Retained<AnyObject>> {
            self.ivars().queue.borrow().clone()
        }

        #[unsafe(method(setLayoutQueue:))]
        fn set_layout_queue(&self, queue: Option<&AnyObject>) {
            *self.ivars().queue.borrow_mut() = queue.map(|q| q.retain());
        }

        #[unsafe(method(ensureLayoutForRange:))]
        fn ensure_layout_for_range(&self, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.ensure_range(a..b);
                self.settle();
                self.laid_for_program();
            }
        }

        #[unsafe(method(ensureLayoutForBounds:))]
        fn ensure_layout_for_bounds(&self, bounds: NSRect) {
            self.ensure_y(bounds.origin.y, bounds.origin.y + bounds.size.height);
            self.settle();
            self.laid_for_program();
        }

        #[unsafe(method(invalidateLayoutForRange:))]
        fn invalidate_layout_for_range(&self, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.invalidate(a..b);
            }
        }

        #[unsafe(method_id(textLayoutFragmentForPosition:))]
        fn text_layout_fragment_for_position(&self, p: NSPoint) -> Option<Retained<NSTextLayoutFragment>> {
            self.fragment_at_point(p)
        }

        #[unsafe(method_id(textLayoutFragmentForLocation:))]
        fn text_layout_fragment_for_location(&self, location: &AnyObject) -> Option<Retained<NSTextLayoutFragment>> {
            self.fragment_for_location(location)
        }

        #[unsafe(method_id(enumerateTextLayoutFragmentsFromLocation:options:usingBlock:))]
        fn enumerate_text_layout_fragments(
            &self,
            from: Option<&AnyObject>,
            options: NSTextLayoutFragmentEnumerationOptions,
            block: &DynBlock<dyn Fn(NonNull<NSTextLayoutFragment>) -> Bool>,
        ) -> Option<Retained<AnyObject>> {
            match from.map(offset_of) {
                Some(None) => None,
                from => {
                    let end = self.enumerate(from.flatten(), options, |f| block.call((NonNull::from(f),)).as_bool());
                    if options.contains(NSTextLayoutFragmentEnumerationOptions::EnsuresLayout) {
                        self.laid_for_program();
                    }
                    end.map(location::location)
                }
            }
        }

        #[unsafe(method_id(textSelections))]
        fn text_selections(&self) -> Retained<NSArray<NSTextSelection>> {
            self.ivars().selections.borrow().clone().unwrap_or_default()
        }

        #[unsafe(method(setTextSelections:))]
        fn set_text_selections(&self, selections: &NSArray<NSTextSelection>) {
            // SAFETY: -copy of an array is an array.
            let copy: Retained<NSArray<NSTextSelection>> = unsafe { msg_send![selections, copy] };
            *self.ivars().selections.borrow_mut() = Some(copy);
        }

        #[unsafe(method_id(textSelectionNavigation))]
        fn text_selection_navigation(&self) -> Retained<NSTextSelectionNavigation> {
            self.navigation()
        }

        #[unsafe(method(setTextSelectionNavigation:))]
        fn set_text_selection_navigation(&self, navigation: &NSTextSelectionNavigation) {
            *self.ivars().navigation.borrow_mut() = Some(navigation.retain());
        }

        // Rendering attributes: kept over ranges, as TextKit 1's temporary
        // attributes are, moved by edits and enumerated; not drawn yet, and
        // the validator is kept but not called (docs/roadmap.md).

        #[unsafe(method(enumerateRenderingAttributesFromLocation:reverse:usingBlock:))]
        fn enumerate_rendering_attributes(
            &self,
            from: &AnyObject,
            reverse: bool,
            block: &DynBlock<dyn Fn(NonNull<NSTextLayoutManager>, NonNull<Dict>, NonNull<NSTextRange>) -> Bool>,
        ) {
            let Some(o) = offset_of(from) else { return };
            let len = self.content_len();
            // From the location on (or back), the run it is in cut there.
            let within = if reverse { 0..o.min(len) } else { o.min(len)..len };
            let mut runs: Vec<(Range<usize>, Retained<Dict>)> = self
                .ivars()
                .rendering
                .borrow()
                .in_range(within)
                .map(|(r, d)| (r, d.clone()))
                .collect();
            if reverse {
                runs.reverse();
            }
            for (r, d) in runs {
                let range = location::range(r.start, r.end);
                let go = block.call((NonNull::from(self.as_manager()), NonNull::from(&*d), NonNull::from(&*range)));
                if !go.as_bool() {
                    break;
                }
            }
        }

        #[unsafe(method(setRenderingAttributes:forTextRange:))]
        fn set_rendering_attributes(&self, attrs: &Dict, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.ivars().rendering.borrow_mut().map(a..b, |_| Some(attrs.retain()));
                self.redraw_range(a..b);
            }
        }

        #[unsafe(method(addRenderingAttribute:value:forTextRange:))]
        fn add_rendering_attribute(&self, key: &NSString, value: Option<&AnyObject>, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.ivars().rendering.borrow_mut().map(a..b, |d| temporary::with(d, key, value));
                self.redraw_range(a..b);
            }
        }

        #[unsafe(method(removeRenderingAttribute:forTextRange:))]
        fn remove_rendering_attribute(&self, key: &NSString, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.ivars().rendering.borrow_mut().map(a..b, |d| temporary::with(d, key, None));
                self.redraw_range(a..b);
            }
        }

        #[unsafe(method(invalidateRenderingAttributesForTextRange:))]
        fn invalidate_rendering_attributes(&self, range: &NSTextRange) {
            if let Some((a, b)) = span_of(range) {
                self.redraw_range(a..b);
            }
        }

        #[unsafe(method(renderingAttributesValidator))]
        fn rendering_attributes_validator(
            &self,
        ) -> *mut DynBlock<dyn Fn(NonNull<NSTextLayoutManager>, NonNull<NSTextLayoutFragment>)> {
            self.ivars().validator.borrow().as_ref().map_or(std::ptr::null_mut(), |b| {
                let r: &DynBlock<dyn Fn(NonNull<NSTextLayoutManager>, NonNull<NSTextLayoutFragment>)> = b;
                r as *const _ as *mut _
            })
        }

        #[unsafe(method(setRenderingAttributesValidator:))]
        fn set_rendering_attributes_validator(
            &self,
            block: Option<&DynBlock<dyn Fn(NonNull<NSTextLayoutManager>, NonNull<NSTextLayoutFragment>)>>,
        ) {
            *self.ivars().validator.borrow_mut() = block.map(|b| b.copy());
        }

        #[unsafe(method_id(renderingAttributesForLink:atLocation:))]
        fn rendering_attributes_for_link(&self, _link: &AnyObject, _at: &AnyObject) -> Retained<Dict> {
            link_attributes()
        }

        #[unsafe(method(enumerateTextSegmentsInRange:type:options:usingBlock:))]
        fn enumerate_text_segments(
            &self,
            range: &NSTextRange,
            kind: NSTextLayoutManagerSegmentType,
            options: NSTextLayoutManagerSegmentOptions,
            block: &DynBlock<dyn Fn(*mut NSTextRange, NSRect, f64, NonNull<NSTextContainer>) -> Bool>,
        ) {
            let Some((a, b)) = span_of(range) else { return };
            let Some(container) = self.ivars().container.borrow().clone() else { return };
            let with_range = !options.contains(NSTextLayoutManagerSegmentOptions::RangeNotRequired);
            for seg in self.segments(a..b, kind, options) {
                let r = with_range.then(|| location::range(seg.0.start, seg.0.end));
                let r = r.as_ref().map_or(std::ptr::null_mut(), |r| Retained::as_ptr(r).cast_mut());
                let go = block.call((r, seg.1, seg.2, NonNull::from(&*container)));
                if !go.as_bool() {
                    break;
                }
            }
        }

        #[unsafe(method(replaceContentsInRange:withTextElements:))]
        fn replace_contents_with_elements(&self, range: &NSTextRange, elements: &NSArray<NSTextElement>) {
            let content = self.ivars().content.borrow().load();
            if let Some(c) = content {
                // SAFETY: the content manager's own method.
                let _: () = unsafe { msg_send![&*c, replaceContentsInRange: range, withTextElements: elements] };
            }
        }

        #[unsafe(method(replaceContentsInRange:withAttributedString:))]
        fn replace_contents_with_string(&self, range: &NSTextRange, text: &NSAttributedString) {
            let content = self.ivars().content.borrow().load();
            let Some(c) = content else { return };
            let Some(cs) = content::as_storage(&c) else { return };
            let (Some((a, b)), Some(storage)) = (span_of(range), cs.text_storage_now()) else { return };
            storage.replaceCharactersInRange_withAttributedString(NSRange::new(a, b - a), text);
        }

        // NSTextSelectionDataSource.

        #[unsafe(method_id(documentRange))]
        fn document_range(&self) -> Retained<NSTextRange> {
            location::range(0, self.content_len())
        }

        #[unsafe(method(enumerateSubstringsFromLocation:options:usingBlock:))]
        fn enumerate_substrings(
            &self,
            from: &AnyObject,
            options: objc2_foundation::NSStringEnumerationOptions,
            block: &DynBlock<dyn Fn(*mut NSString, NonNull<NSTextRange>, *mut NSTextRange, NonNull<Bool>)>,
        ) {
            super::selection::enumerate_substrings(self, from, options, block);
        }

        #[unsafe(method_id(textRangeForSelectionGranularity:enclosingLocation:))]
        fn text_range_for_selection_granularity(
            &self,
            granularity: objc2_app_kit::NSTextSelectionGranularity,
            at: &AnyObject,
        ) -> Option<Retained<NSTextRange>> {
            offset_of(at)
                .and_then(|o| super::selection::granular_range(self, o, granularity))
                .map(|r| location::range(r.start, r.end))
        }

        #[unsafe(method_id(locationFromLocation:withOffset:))]
        fn location_from_location(&self, from: &AnyObject, offset: NSInteger) -> Option<Retained<AnyObject>> {
            offset_of(from).and_then(|o| {
                let o = o as isize + offset;
                (0..=self.content_len() as isize).contains(&o).then(|| location::location(o as usize))
            })
        }

        #[unsafe(method(offsetFromLocation:toLocation:))]
        fn offset_from_location(&self, from: &AnyObject, to: &AnyObject) -> NSInteger {
            match (offset_of(from), offset_of(to)) {
                (Some(a), Some(b)) => b as isize - a as isize,
                _ => 0,
            }
        }

        #[unsafe(method(baseWritingDirectionAtLocation:))]
        fn base_writing_direction_at_location(&self, at: &AnyObject) -> NSTextSelectionNavigationWritingDirection {
            type Direction = NSTextSelectionNavigationWritingDirection;
            let rtl = offset_of(at).and_then(|o| self.line_at(o, false)).is_some_and(|l| l.lines.rtl);
            if rtl { Direction::RightToLeft } else { Direction::LeftToRight }
        }

        #[unsafe(method(enumerateCaretOffsetsInLineFragmentAtLocation:usingBlock:))]
        fn enumerate_caret_offsets(
            &self,
            at: &AnyObject,
            block: &DynBlock<dyn Fn(f64, NonNull<AnyObject>, Bool, NonNull<Bool>)>,
        ) {
            let Some(o) = offset_of(at) else { return };
            let Some(l) = self.line_at(o, false) else { return };
            let line = l.line();
            let left = self.padding() + l.left;
            for i in l.range().start..=l.content_end() {
                let (x, _) = line.caret_x((i - l.start) as u32);
                let loc = location::location(i);
                let mut stop = Bool::NO;
                block.call((left + f64::from(x), NonNull::from(&*loc), Bool::YES, NonNull::from(&mut stop)));
                if stop.as_bool() {
                    break;
                }
            }
        }

        #[unsafe(method_id(lineFragmentRangeForPoint:inContainerAtLocation:))]
        fn line_fragment_range_for_point(&self, p: NSPoint, _at: &AnyObject) -> Option<Retained<NSTextRange>> {
            self.line_at_point(p.x, p.y).map(|l| {
                let r = l.range();
                location::range(r.start, r.end)
            })
        }

        #[unsafe(method(enumerateContainerBoundariesFromLocation:reverse:usingBlock:))]
        fn enumerate_container_boundaries(
            &self,
            _from: &AnyObject,
            reverse: bool,
            block: &DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<Bool>)>,
        ) {
            let edge = location::location(if reverse { 0 } else { self.content_len() });
            let mut stop = Bool::NO;
            block.call((NonNull::from(&*edge), NonNull::from(&mut stop)));
        }

        #[unsafe(method(textLayoutOrientationAtLocation:))]
        fn text_layout_orientation_at_location(&self, _at: &AnyObject) -> NSTextSelectionNavigationLayoutOrientation {
            NSTextSelectionNavigationLayoutOrientation::Horizontal
        }
    }

    unsafe impl NSObjectProtocol for NSTextLayoutManagerImpl {}
);

/// The attributes links are drawn with: blue, underlined.
fn link_attributes() -> Retained<Dict> {
    let color = objc2_app_kit::NSColor::linkColor();
    let underline = objc2_foundation::NSNumber::new_isize(1);
    // SAFETY: the keys are constant strings AppKit exports.
    let keys = unsafe { [objc2_app_kit::NSForegroundColorAttributeName, objc2_app_kit::NSUnderlineStyleAttributeName] };
    objc2_foundation::NSDictionary::from_slices(&keys, &[&*color as &AnyObject, &*underline as &AnyObject])
}

impl NSTextLayoutManagerImpl {
    pub(crate) fn as_manager(&self) -> &NSTextLayoutManager {
        // SAFETY: NSTextLayoutManager is this class.
        unsafe { &*(self as *const Self).cast::<NSTextLayoutManager>() }
    }

    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }

    /// The weak reference to itself its fragments share.
    fn shared(&self) -> SharedWeak {
        self.ivars().me.get_or_init(|| SharedWeak::new(self.as_object())).clone()
    }

    fn content(&self) -> Option<Retained<AnyObject>> {
        self.ivars().content.borrow().load()
    }

    /// The viewport controller, made once there is a container.
    pub(crate) fn viewport_controller(&self) -> Option<Retained<NSTextViewportLayoutController>> {
        if let Some(v) = self.ivars().viewport.borrow().as_ref() {
            return Some(v.clone());
        }
        self.ivars().container.borrow().as_ref()?;
        let v = super::viewport::new_controller(self.as_manager());
        *self.ivars().viewport.borrow_mut() = Some(v.clone());
        Some(v)
    }

    /// A program had text laid out: a view shows what that moved (and
    /// doesn't hold its text in place against it).
    fn laid_for_program(&self) {
        let moved = {
            let st = self.ivars().state.borrow();
            st.damage.moved.is_some() || st.damage.redraw.is_some()
        };
        if moved {
            self.changed();
        }
    }

    /// Whether anything but layout changed where text is since the last
    /// call.
    pub(crate) fn take_touched(&self) -> bool {
        self.ivars().touched.replace(false)
    }

    /// Take in the estimate of the rest in the usage bounds from now on.
    pub(crate) fn estimate_document(&self) {
        self.ivars().estimated.set(true);
    }

    /// The fragment holding `location`: none at the document's end, nor
    /// past what a laid-out fragment covers (a delegate's paragraph shorter
    /// than the text it stands for), as on macOS.
    fn fragment_for_location(&self, location: &AnyObject) -> Option<Retained<NSTextLayoutFragment>> {
        let o = offset_of(location)?;
        if o >= self.content_len() {
            return None;
        }
        let (_, f) = self.materialize(o)?;
        match fragment::ivars(&f).and_then(|iv| iv.span()) {
            Some((_, end)) if o >= end => None,
            _ => Some(f),
        }
    }

    fn navigation(&self) -> Retained<NSTextSelectionNavigation> {
        if let Some(n) = self.ivars().navigation.borrow().as_ref() {
            return n.clone();
        }
        let n = super::selection::new_navigation(self.as_object());
        *self.ivars().navigation.borrow_mut() = Some(n.clone());
        n
    }

    /// The document's length in UTF-16 units: the content storage's text,
    /// else the content manager's document range.
    pub(crate) fn content_len(&self) -> usize {
        let Some(c) = self.content() else { return 0 };
        if let Some(cs) = content::as_storage(&c) {
            return cs.text_len();
        }
        // SAFETY: documentRange takes nothing and returns a range.
        let doc: Retained<NSTextRange> = unsafe { msg_send![&*c, documentRange] };
        span_of(&doc).map_or(0, |(_, b)| b)
    }

    pub(crate) fn padding(&self) -> f64 {
        self.ivars().state.borrow().geometry.padding
    }

    /// The container's width, as lines take it (as good as unbounded, as
    /// TextKit 1 takes it, for none).
    fn container_width(&self) -> f64 {
        let g = self.ivars().state.borrow().geometry;
        if g.width.is_finite() { f64::from(g.width) + 2.0 * g.padding } else { 1.0e7 }
    }

    /// The container's text view, if any, while it lays out through this
    /// manager (not once it became TextKit 1).
    fn view(&self) -> Option<Retained<AnyObject>> {
        let c = self.ivars().container.borrow().clone()?;
        let mine =
            crate::textkit::container::text_layout_manager(&c).is_some_and(|m| std::ptr::eq(&*m, self.as_object()));
        if !mine {
            return None;
        }
        // SAFETY: textView takes nothing and returns a view or nil.
        let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*c, textView] };
        v
    }

    // Keeping up with the content and the container.

    /// Read the container's geometry and lay everything out afresh.
    pub(crate) fn geometry_changed(&self) {
        let g = {
            let c = self.ivars().container.borrow().clone();
            let geo =
                c.map_or(crate::textkit::container::Geometry::DEFAULT, |c| crate::textkit::container::geometry(&c));
            let width = if geo.size.width <= 0.0 || geo.size.width >= 1.0e7 {
                f32::INFINITY
            } else {
                (geo.size.width - 2.0 * geo.padding).max(0.0) as f32
            };
            Geometry { width, padding: geo.padding, font_leading: self.ivars().font_leading.get() }
        };
        {
            let mut st = self.ivars().state.borrow_mut();
            if st.geometry == g {
                return;
            }
            st.geometry = g;
        }
        self.relayout_all();
    }

    /// Every fragment's layout is gone (new geometry): they keep their
    /// elements, and heights go back to estimates, worked out in one walk
    /// over the text (a live resize does this at every step).
    fn relayout_all(&self) {
        let len = self.content_len();
        let mut lens: Vec<usize> = Vec::new();
        {
            let mut st = self.ivars().state.borrow_mut();
            if st.index.len() != len {
                drop(st);
                self.rebuild();
                return;
            }
            st.index.for_each(|_, seg| lens.push(seg.len));
            st.extra = None;
        }
        let raws = match self.raw_estimates(&lens) {
            Some(r) => r,
            None => {
                let mut start = 0;
                lens.iter()
                    .map(|&n| {
                        start += n;
                        self.raw_estimate(start - n..start)
                    })
                    .collect()
            }
        };
        let mut st = self.ivars().state.borrow_mut();
        let mut i = 0;
        st.index.for_each_mut(|seg| {
            if let Some(slot) = &mut seg.slot {
                if let Some(iv) = fragment::ivars(&slot.fragment) {
                    iv.clear();
                }
                slot.laid = false;
            }
            seg.fixed = 0.0;
            seg.raw = raws[i];
            i += 1;
        });
        st.damage.mark_moved(0.0);
        drop(st);
        self.changed();
    }

    /// The estimates of consecutive stretches `lens` long, from the start,
    /// in one walk over a Sidestep storage's paragraphs (a stretch of the
    /// storage not yet cut into paragraphs shares its estimate out by
    /// length; an empty last paragraph adds nothing, as `raw_estimate`
    /// has it); `None` for other content.
    fn raw_estimates(&self, lens: &[usize]) -> Option<Vec<f64>> {
        self.with_native(|text, table, st| {
            let mut out = vec![0.0; lens.len()];
            if lens.is_empty() {
                return out;
            }
            let (mut item, mut item_start, mut item_end) = (0usize, 0usize, lens[0]);
            let mut at = 0usize;
            let mut sizes = FontSizes::default();
            text.for_each_extent(0..text.paragraph_count(), |e| {
                let (len, height) = sizes.estimate(&e, st, table);
                if len == 0 {
                    return;
                }
                let end = at + len;
                loop {
                    while item_end <= at && item + 1 < lens.len() {
                        item += 1;
                        item_start = item_end;
                        item_end += lens[item];
                    }
                    let overlap = end.min(item_end).saturating_sub(at.max(item_start));
                    out[item] += height * overlap as f64 / len as f64;
                    if item_end >= end || item + 1 >= lens.len() {
                        break;
                    }
                    at = item_end;
                }
                at = end;
            });
            out
        })
    }

    /// A new document (or none): estimates for all of it.
    pub(crate) fn rebuild(&self) {
        let segs = self.estimated_segments();
        let mut st = self.ivars().state.borrow_mut();
        let old = std::mem::replace(&mut st.index, Seq::new(segs));
        st.extra = None;
        st.recycled.clear();
        st.damage.mark_moved(0.0);
        drop(st);
        drop(old);
        self.changed();
    }

    /// The document as stretches known only by estimates: a Sidestep
    /// storage's paragraphs a few dozen at a time (a raw stretch of it
    /// whole), else one stretch.
    fn estimated_segments(&self) -> Vec<Seg> {
        let len = self.content_len();
        if len == 0 {
            return vec![Seg::unknown(0, 0.0)];
        }
        let facts = self.with_native(|text, table, st| {
            let mut out: Vec<Seg> = Vec::new();
            let mut run = (0usize, 0.0f64, 0usize);
            let mut sizes = FontSizes::default();
            text.for_each_extent(0..text.paragraph_count(), |e| {
                let one = matches!(e, crate::textkit::storage::Extent::One { .. });
                let (len, height) = sizes.estimate(&e, st, table);
                if !one && run.2 > 0 {
                    out.push(Seg::unknown(run.0, run.1));
                    run = (0, 0.0, 0);
                }
                run = (run.0 + len, run.1 + height, run.2 + 1);
                if !one || run.2 >= 64 {
                    out.push(Seg::unknown(run.0, run.1));
                    run = (0, 0.0, 0);
                }
            });
            if run.2 > 0 {
                out.push(Seg::unknown(run.0, run.1));
            }
            out
        });
        match facts {
            Some(segs) if !segs.is_empty() && segs.iter().map(|s| s.len).sum::<usize>() == len => segs,
            _ => vec![Seg::unknown(len, self.raw_estimate(0..len))],
        }
    }

    /// Run `f` with a Sidestep storage's paragraph tree and attribute table
    /// (and the state), when the content is a content storage over one.
    fn with_native<R>(
        &self,
        f: impl FnOnce(&crate::textkit::storage::Storage, &crate::textkit::attrs::AttrTable, &mut State) -> R,
    ) -> Option<R> {
        let c = self.content()?;
        let cs = content::as_storage(&c)?;
        let storage = cs.text_storage_now()?;
        let obj: &AnyObject = &storage;
        let iv = crate::textkit::text_storage::native(obj)?;
        let text = iv.text();
        let table = iv.attrs().borrow();
        let mut st = self.ivars().state.borrow_mut();
        Some(f(&text, &table, &mut st))
    }

    /// The estimate of `range`'s height, before scaling.
    fn raw_estimate(&self, range: Range<usize>) -> f64 {
        if range.is_empty() {
            return 0.0;
        }
        let native = self.with_native(|text, table, st| {
            let len = text.len();
            let (a, b) = (range.start.min(len), range.end.min(len));
            if a >= b {
                return 0.0;
            }
            let (p0, p1) = (text.locate(a).para, text.locate(b - 1).para);
            let mut total = 0.0;
            let mut sizes = FontSizes::default();
            text.for_each_extent(p0..p1 + 1, |e| total += sizes.estimate(&e, st, table).1);
            total
        });
        native.unwrap_or_else(|| {
            let width = self.ivars().state.borrow().geometry.width;
            estimate(range.len() as f64, 12.0, width)
        })
    }

    /// The content storage (or the content manager) changed the elements
    /// over `range` (in the document as it is now), `exact` of it by
    /// `delta` units.
    fn content_edited(&self, range: Range<usize>, exact: Range<usize>, delta: isize, characters: bool) {
        let len = self.content_len();
        let old_len = (len as isize - delta).max(0) as usize;
        let old_end = ((range.end as isize - delta).max(range.start as isize) as usize).min(old_len);
        if characters {
            // Rendering attributes move with the text around the edit.
            let exact_old_end = ((exact.end as isize - delta).max(exact.start as isize) as usize).min(old_len);
            self.ivars().rendering.borrow_mut().edited(exact.start..exact_old_end, exact.len());
        }
        let replaced = {
            let mut st = self.ivars().state.borrow_mut();
            st.extra = None;
            if st.index.len() != old_len {
                None
            } else {
                let (first, count, end) = st.index.cover(range.start.min(old_len), old_end);
                let (mut fixed, mut raw, mut kept) = (0.0, 0.0, true);
                let mut p = first;
                for i in 0..count {
                    let s = st.index.get(p);
                    fixed += s.fixed;
                    raw += s.raw;
                    kept &= s.slot.is_some() || s.fixed > 0.0;
                    if i + 1 < count {
                        p = st.index.next(p).expect("counted");
                    }
                }
                let top = first.top(st.factor);
                let new_len = ((end - first.start) as isize + delta).max(0) as usize;
                Some((first, count, new_len, top, count == 1 && kept, fixed, raw))
            }
        };
        let Some((first, count, new_len, top, keep, fixed, raw)) = replaced else {
            self.rebuild();
            self.edited_views(range, delta, characters);
            return;
        };
        // A long stretch of new text is estimated a paragraph (or a stretch
        // of the storage) at a time again, as a new text is.
        if new_len > LONG_EDIT {
            self.rebuild();
            self.edited_views(range, delta, characters);
            return;
        }
        // One element for one: its old height, likelier than an estimate.
        let seg = if keep && fixed + raw > 0.0 {
            Seg { len: new_len, fixed, raw, slot: None }
        } else {
            let est = self.raw_estimate(first.start..first.start + new_len);
            Seg::unknown(new_len, est)
        };
        {
            let mut st = self.ivars().state.borrow_mut();
            let p = st.index.locate(first.start);
            // Located again: the estimate above sent no messages, but be
            // sure the index didn't move.
            let p = if p.start == first.start { p } else { first };
            let height = seg_height(&seg, st.factor);
            st.index.splice(p, count, vec![seg]);
            // One element for one keeps its height until laid out again:
            // only it is drawn again (laying it out tells if more moved).
            if keep {
                st.damage.mark_redraw(top, top + height);
            } else {
                st.damage.mark_moved(top);
            }
        }
        self.changed();
        self.edited_views(range, delta, characters);
    }

    fn edited_views(&self, range: Range<usize>, delta: isize, characters: bool) {
        if characters && let Some(v) = self.view() {
            crate::textkit::text_view::storage_edited(&v, NSRange::new(range.start, range.len()), delta);
        }
    }

    /// Layout changed: the views draw again what changed, and the viewport
    /// is laid out again before they do.
    fn changed(&self) {
        self.ivars().touched.set(true);
        if let Some(v) = self.view() {
            crate::textkit::text_view::layout_changed(&v, false);
        }
    }

    /// The views draw `range` again.
    fn redraw_range(&self, range: Range<usize>) {
        {
            let mut st = self.ivars().state.borrow_mut();
            let f = st.factor;
            let a = st.index.locate(range.start);
            let b = st.index.locate(range.end);
            let bottom = b.top(f) + seg_height(st.index.get(b), f);
            st.damage.mark_redraw(a.top(f), bottom);
        }
        self.changed();
    }

    /// Lay out again what `range` touches: its elements go back to
    /// stretches keeping their heights, and their fragments wait for them.
    fn invalidate(&self, range: Range<usize>) {
        let len = self.content_len();
        let mut st = self.ivars().state.borrow_mut();
        if st.index.len() != len {
            drop(st);
            self.rebuild();
            return;
        }
        st.extra = None;
        let f = st.factor;
        let mut p = st.index.locate(range.start.min(len));
        let top = p.top(f);
        loop {
            let mut taken = None;
            if st.index.get(p).slot.is_some() {
                st.index.update(p, |seg| {
                    seg.fixed += f * seg.raw;
                    seg.raw = 0.0;
                    taken = seg.slot.take();
                });
            }
            if let Some(slot) = taken {
                if let Some(iv) = fragment::ivars(&slot.fragment) {
                    iv.unlay();
                }
                let key = Retained::as_ptr(&slot.element) as usize;
                if st.recycled.len() >= RECYCLED_MAX {
                    st.recycled.clear();
                }
                st.recycled.insert(key, slot.fragment);
            }
            let end = p.start + st.index.get(p).len;
            if end >= range.end {
                break;
            }
            match st.index.next(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        st.damage.mark_moved(top);
        drop(st);
        self.changed();
    }

    /// A fragment was told to invalidate its layout.
    fn fragment_unlaid(&self, fragment: &AnyObject) {
        let Some(e) = fragment::ivars(fragment).and_then(FragmentIvars::element) else { return };
        let Some((a, b)) = element::span(&e) else { return };
        self.invalidate(a..b.max(a + 1));
    }

    // Elements.

    /// The element and fragment at offset `o` (the last one at the
    /// document's end), made from the content manager's elements if the
    /// index only knows the stretch there; its position.
    fn materialize(&self, o: usize) -> Option<(Pos, Retained<NSTextLayoutFragment>)> {
        let len = self.content_len();
        if len == 0 {
            return None;
        }
        let o = o.min(len);
        // Each round places a batch of elements from the one holding `o`;
        // the first places it unless the content manager's elements don't
        // tile the text there (a subclass's), when a few more rounds give
        // it the chance to, and then there is none.
        for _ in 0..4 {
            {
                let mut st = self.ivars().state.borrow_mut();
                if st.index.len() != len {
                    drop(st);
                    self.rebuild();
                    continue;
                }
                let p = st.index.locate(o);
                if let Some(slot) = &st.index.get(p).slot {
                    return Some((p, slot.fragment.clone()));
                }
            }
            // Ask for the elements from the one holding `o` (the last one's,
            // at the end).
            let from = if o >= len { len - 1 } else { o };
            let elements = self.fetch_elements(from, BATCH);
            if elements.is_empty() {
                return None;
            }
            for (e, r) in elements {
                self.place_element(e, r);
            }
        }
        let mut st = self.ivars().state.borrow_mut();
        let p = st.index.locate(o);
        st.index.get(p).slot.as_ref().map(|s| (p, s.fragment.clone()))
    }

    /// Up to `n` elements from the one holding offset `from`, with their
    /// ranges, from the content manager.
    fn fetch_elements(&self, from: usize, n: usize) -> Vec<(Retained<AnyObject>, Range<usize>)> {
        let Some(c) = self.content() else { return Vec::new() };
        let mut out: Vec<(Retained<AnyObject>, Range<usize>)> = Vec::new();
        if let Some(cs) = content::as_storage(&c)
            && content::enumerates_natively(cs)
        {
            cs.elements(Some(from), false, |e, r| {
                out.push((e.retain(), r));
                out.len() < n
            });
            return out;
        }
        let found = RefCell::new(Vec::new());
        let block = block2::RcBlock::new(|e: NonNull<NSTextElement>| -> Bool {
            // SAFETY: the element enumerated is alive for the call.
            let e: &AnyObject = unsafe { e.as_ref() };
            if let Some((a, b)) = element::span(e) {
                found.borrow_mut().push((e.retain(), a..b));
            }
            Bool::new(found.borrow().len() < n)
        });
        let loc = location::location(from);
        let options = NSTextContentManagerEnumerationOptions::None;
        // SAFETY: the method's own types; a subclass may override it.
        let _: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*c, enumerateTextElementsFromLocation: &*loc, options: options, usingBlock: &*block] };
        drop(block);
        out = found.into_inner();
        // An element shorter than the text it stands for (a delegate's
        // paragraph) reaches the next one's start, so the index tiles the
        // text.
        for i in 1..out.len() {
            let next = out[i].1.start;
            let prev = &mut out[i - 1].1;
            if prev.end < next {
                prev.end = next;
            }
        }
        out
    }

    /// Put `element` over `range` into the index: the stretches and
    /// elements it overlaps give way (an element already there with the
    /// same range stays).
    fn place_element(&self, element: Retained<AnyObject>, range: Range<usize>) {
        let key = Retained::as_ptr(&element) as usize;
        // Already there?
        let reuse = {
            let mut st = self.ivars().state.borrow_mut();
            let len = st.index.len();
            if range.end > len || range.start > range.end {
                return;
            }
            let p = st.index.locate(range.start);
            let seg = st.index.get(p);
            if p.start == range.start
                && seg.len == range.len()
                && seg.slot.as_ref().is_some_and(|s| Retained::as_ptr(&s.element) as usize == key)
            {
                return;
            }
            // Its fragment, if it had one.
            let mut found = st.recycled.remove(&key);
            if found.is_none() {
                let mut q = Some(p);
                while let Some(qq) = q {
                    if qq.start >= range.end.max(range.start + 1) {
                        break;
                    }
                    if let Some(s) = &st.index.get(qq).slot
                        && Retained::as_ptr(&s.element) as usize == key
                    {
                        found = Some(s.fragment.clone());
                        break;
                    }
                    q = st.index.next(qq);
                }
            }
            found
        };
        let fragment = match reuse {
            Some(f) => f,
            None => self.new_fragment(&element, range.start),
        };
        if let Some(iv) = fragment::ivars(&fragment) {
            iv.set_manager(&self.shared());
            iv.unlay();
        }
        // Heights of what it replaces: the part of each overlapped item it
        // covers.
        let (fixed_share, raw_share, edges) = {
            let mut st = self.ivars().state.borrow_mut();
            let first = st.index.locate(range.start);
            let mut p = first;
            let (mut fixed, mut raw) = (0.0, 0.0);
            let mut count = 0;
            let mut end;
            loop {
                let seg = st.index.get(p);
                let (s, e) = (p.start, p.start + seg.len);
                let overlap = e.min(range.end).saturating_sub(s.max(range.start));
                if seg.len > 0 {
                    let share = overlap as f64 / seg.len as f64;
                    fixed += seg.fixed * share;
                    if seg.fixed <= 0.0 {
                        raw += seg.raw * share;
                    }
                }
                count += 1;
                end = e;
                if e >= range.end {
                    break;
                }
                match st.index.next(p) {
                    Some(n) => p = n,
                    None => break,
                }
            }
            (fixed, raw, (first, count, first.start, end))
        };
        let (first, count, s0, e0) = edges;
        // The stretches left before and after it, estimated again.
        let before = (s0 < range.start).then(|| self.remainder(first, s0..range.start));
        let after_pos = {
            let mut st = self.ivars().state.borrow_mut();
            st.index.locate(e0.saturating_sub(1).max(s0))
        };
        let after = (e0 > range.end).then(|| self.remainder(after_pos, range.end..e0));
        let raw = if fixed_share > 0.0 { raw_share } else { self.raw_estimate(range.clone()) };
        let mut items = Vec::with_capacity(3);
        if let Some(b) = before {
            items.push(b);
        }
        items.push(Seg {
            len: range.len(),
            fixed: fixed_share,
            raw,
            slot: Some(Box::new(Slot { element, fragment, laid: false, left: 0.0, right: 0.0 })),
        });
        if let Some(a) = after {
            items.push(a);
        }
        let mut st = self.ivars().state.borrow_mut();
        let p = st.index.locate(s0);
        let p = if p.start == s0 { p } else { first };
        // Elements it replaces keep their fragments for their return.
        let mut q = p;
        for i in 0..count {
            let seg = st.index.get(q);
            if let Some(s) = &seg.slot {
                let k = Retained::as_ptr(&s.element) as usize;
                let f = s.fragment.clone();
                if k != key {
                    if st.recycled.len() >= RECYCLED_MAX {
                        st.recycled.clear();
                    }
                    st.recycled.insert(k, f);
                }
            }
            if i + 1 < count {
                match st.index.next(q) {
                    Some(n) => q = n,
                    None => break,
                }
            }
        }
        st.index.splice(p, count, items);
    }

    /// A stretch over `range`, part of the item at `p`, with its share of
    /// the item's height.
    fn remainder(&self, p: Pos, range: Range<usize>) -> Seg {
        let (len, fixed, raw) = {
            let st = self.ivars().state.borrow();
            let seg = st.index.get(p);
            (seg.len, seg.fixed, seg.raw)
        };
        let share = if len > 0 { range.len() as f64 / len as f64 } else { 0.0 };
        if fixed > 0.0 && raw <= 0.0 {
            return Seg { len: range.len(), fixed: fixed * share, raw: 0.0, slot: None };
        }
        let est = self.raw_estimate(range.clone());
        Seg { len: range.len(), fixed: fixed * share, raw: if est > 0.0 { est } else { raw * share }, slot: None }
    }

    /// The fragment for `element`, starting at `start`: the delegate's, else
    /// a plain one.
    fn new_fragment(&self, element: &AnyObject, start: usize) -> Retained<NSTextLayoutFragment> {
        let delegate = self.ivars().delegate.borrow().load();
        let sel: Sel = sel!(textLayoutManager:textLayoutFragmentForLocation:inTextElement:);
        if let Some(d) = delegate
            && crate::textkit::responds(&d, sel)
        {
            let (me, loc) = (self.as_manager(), location::location(start));
            // SAFETY: the delegate method takes the manager, a location and
            // an element and returns a fragment.
            let f: Option<Retained<NSTextLayoutFragment>> = unsafe {
                msg_send![&*d, textLayoutManager: me, textLayoutFragmentForLocation: &*loc, inTextElement: element]
            };
            if let Some(f) = f {
                return f;
            }
        }
        fragment::new_fragment(element, None)
    }

    // Laying out.

    /// Lay out the fragment at `p` if it isn't: returns its (possibly
    /// new) height. Near the document's start, what comes before it is
    /// laid out first, so a short text's positions are exact.
    fn lay_out_at(&self, p: Pos) -> Option<f64> {
        let start = p.start;
        if start > 0 && start <= CONTIGUOUS_PREFIX && !self.is_laid(0..start) {
            let mut o = 0;
            while o < start {
                let Some((q, _)) = self.materialize(o) else { break };
                if q.start >= start {
                    break;
                }
                self.lay_out_start(q.start);
                let end = {
                    let mut st = self.ivars().state.borrow_mut();
                    let r = st.index.locate(q.start);
                    r.start + st.index.get(r).len
                };
                if end <= o {
                    break;
                }
                o = end;
            }
        }
        self.lay_out_start(start)
    }

    /// Lay out the fragment starting at `start` if it isn't.
    fn lay_out_start(&self, start: usize) -> Option<f64> {
        let p = self.pos_of(start);
        let (element, fragment, start, len, was, estimate_raw) = {
            let st = self.ivars().state.borrow();
            let seg = st.index.get(p);
            let slot = seg.slot.as_ref()?;
            if slot.laid && fragment::ivars(&slot.fragment).is_some_and(FragmentIvars::is_laid) {
                return Some(seg_height(seg, st.factor));
            }
            (slot.element.clone(), slot.fragment.clone(), p.start, seg.len, seg_height(seg, st.factor), seg.raw)
        };
        let doc_len = self.content_len();
        // Its own text: the element's range, where the index gave it more
        // (a subclass's elements leaving a gap after it).
        let text = match element::span(&element) {
            Some((a, b)) if a == start && b < start + len => a..b,
            _ => start..start + len,
        };
        let laid = self.lay_out_element(&element, text, start == 0, start + len >= doc_len)?;
        let top = {
            let mut st = self.ivars().state.borrow_mut();
            let q = st.index.locate(start);
            q.top(st.factor)
        };
        let pad = self.padding();
        let origin = NSPoint::new(pad + f64::from(laid.min_x), top);
        if let Some(iv) = fragment::ivars(&fragment) {
            iv.set_layout(Arc::new(laid), origin);
        }
        // What the fragment says its frame is decides what comes below.
        // SAFETY: layoutFragmentFrame takes nothing; a subclass may
        // override it.
        let frame: NSRect = unsafe { msg_send![&*fragment, layoutFragmentFrame] };
        let height = frame.size.height.max(0.0);
        let mut st = self.ivars().state.borrow_mut();
        let q = st.index.locate(start);
        if q.start != start || st.index.get(q).slot.as_ref().is_none_or(|s| !std::ptr::eq(&*s.fragment, &*fragment)) {
            return Some(height);
        }
        if estimate_raw > 0.0 {
            st.measured.0 += estimate_raw;
            st.measured.1 += height;
        }
        st.index.update(q, |seg| {
            seg.fixed = height;
            seg.raw = 0.0;
            if let Some(slot) = &mut seg.slot {
                slot.laid = true;
                slot.left = frame.origin.x;
                slot.right = frame.origin.x + frame.size.width;
            }
        });
        if (height - was).abs() > 1e-6 {
            st.damage.mark_moved(top);
        } else {
            st.damage.mark_redraw(top, top + height);
        }
        Some(height)
    }

    /// Lay out `element`'s text over `range` of the document.
    fn lay_out_element(&self, element: &AnyObject, range: Range<usize>, first: bool, last: bool) -> Option<Laid> {
        let text = self.element_text(element, range.clone());
        let open = last && ends_in_separator(element, &text);
        let extra = if open { self.extra_attrs(&text) } else { None };
        let mut st = self.ivars().state.borrow_mut();
        let g = st.geometry;
        let laid = layout::lay_out(&text, &st.resolved.attrs, &g, (first, last, open), extra.as_ref());
        st.resolved.trim();
        Some(laid)
    }

    /// An element's text: straight from the storage for a content storage's
    /// own paragraph, else its attributed string.
    fn element_text(&self, element: &AnyObject, range: Range<usize>) -> layout::Text {
        let content = self.content();
        let storage_paragraph = element::ivars(element).and_then(|iv| iv.live.get()).is_some_and(|l| l.default)
            && element::given_text(element).is_none();
        if storage_paragraph
            && let Some(c) = &content
            && let Some(cs) = content::as_storage(c)
            && let Some(storage) = cs.text_storage_now()
        {
            let obj: &AnyObject = &storage;
            if crate::textkit::text_storage::native(obj).is_some() {
                let mut resolved = std::mem::take(&mut self.ivars().state.borrow_mut().resolved);
                let t = layout::storage_text(&storage, range.clone(), &mut resolved);
                self.ivars().state.borrow_mut().resolved = resolved;
                if let Some(t) = t {
                    return t;
                }
            }
        }
        let text = element::text_of(element, content.as_deref());
        let mut resolved = std::mem::take(&mut self.ivars().state.borrow_mut().resolved);
        let t = match &text {
            Some(a) => layout::attributed_text(a, &mut resolved),
            None => layout::Text { text: String::new(), spans: Vec::new() },
        };
        self.ivars().state.borrow_mut().resolved = resolved;
        t
    }

    /// The attributes of the empty line after a final separator: the text
    /// view's typing attributes, else the last character's.
    fn extra_attrs(&self, text: &layout::Text) -> Option<Attrs> {
        if let Some(v) = self.view()
            && let Some(dict) = crate::textkit::text_view::typing_attributes(&v)
        {
            return Some(crate::string_drawing::attrs_of(Some(&dict)));
        }
        let st = self.ivars().state.borrow();
        text.spans.last().and_then(|s| st.resolved.attrs.get(s.attrs as usize)).cloned()
    }

    /// Settle the estimate factor after a round of layout.
    fn settle(&self) {
        let mut st = self.ivars().state.borrow_mut();
        let (raw, act) = st.measured;
        if raw > 0.0 {
            let f = (act / raw).clamp(0.25, 4.0);
            if (f - st.factor).abs() / st.factor > 0.02 {
                st.factor = f;
                st.damage.mark_moved(0.0);
            }
        }
    }

    /// Lay out the fragments covering `range` (the one holding its start,
    /// for an empty range; the extra line fragment, in an empty document).
    pub(crate) fn ensure_range(&self, range: Range<usize>) {
        let len = self.content_len();
        if len == 0 {
            self.extra_fragment();
            return;
        }
        let mut o = range.start.min(len);
        loop {
            let Some((p, _)) = self.materialize(o) else { return };
            self.lay_out_at(p);
            // Laying out what comes before it (near the start) splices the
            // index before `p`: where it ends is looked up again.
            let end = self.end_of(p.start);
            if end >= range.end || end >= len || end <= o {
                return;
            }
            o = end;
        }
    }

    /// Where the item starting at `start` ends.
    fn end_of(&self, start: usize) -> usize {
        let mut st = self.ivars().state.borrow_mut();
        let q = st.index.locate(start);
        q.start + st.index.get(q).len
    }

    /// Lay out what `sizeToFit` measures: the text near the start (all of
    /// a short text, so its height is exact; a long one's rest keeps its
    /// estimate), or an empty text's extra line fragment.
    pub(crate) fn lay_out_to_size(&self) {
        let len = self.content_len();
        self.ensure_range(0..len.min(CONTIGUOUS_PREFIX));
        self.settle();
    }

    /// Lay out the fragments from height `y0` to `y1`, placed by what is
    /// above them.
    pub(crate) fn ensure_y(&self, y0: f64, y1: f64) {
        let len = self.content_len();
        if len == 0 {
            self.extra_fragment();
            return;
        }
        // Laying fragments out changes their heights, which moves what the
        // bounds meet: again until a round lays nothing out. Each round lays
        // out at least a fragment, so this ends; the cap only bounds a
        // subclass whose frames keep changing (what the last round laid out
        // stays laid out).
        for _ in 0..64 {
            let mut changed = false;
            let mut o = self.offset_at_y(y0);
            loop {
                let (p, laid) = {
                    let Some((p, f)) = self.materialize(o) else { return };
                    let laid = fragment::ivars(&f).is_some_and(FragmentIvars::is_laid)
                        && self.ivars().state.borrow().index.get(p).slot.as_ref().is_some_and(|s| s.laid);
                    (p, laid)
                };
                if !laid {
                    self.lay_out_at(p);
                    changed = true;
                }
                let (top, end) = {
                    let mut st = self.ivars().state.borrow_mut();
                    let q = st.index.locate(p.start);
                    let seg = st.index.get(q);
                    (q.top(st.factor) + seg_height(seg, st.factor), q.start + seg.len)
                };
                if top >= y1 || end >= len || end <= o {
                    break;
                }
                o = end;
            }
            if !changed {
                break;
            }
        }
    }

    /// The offset at height `y`: in a stretch known only by its estimate,
    /// as far through it as `y` is.
    fn offset_at_y(&self, y: f64) -> usize {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        let p = st.index.locate_y(y, f);
        let seg = st.index.get(p);
        if seg.slot.is_some() || seg.len == 0 {
            return p.start;
        }
        let h = seg_height(seg, f);
        let through = if h > 0.0 { ((y - p.top(f)) / h).clamp(0.0, 1.0) } else { 0.0 };
        p.start + ((seg.len as f64 * through) as usize).min(seg.len.saturating_sub(1))
    }

    /// The start of the fragment at height `y` (made if it isn't).
    pub(crate) fn offset_at_top(&self, y: f64) -> usize {
        let o = self.offset_at_y(y.max(0.0));
        self.materialize(o).map_or(o, |(p, _)| p.start)
    }

    /// Call `f` with fragments from `from` (as enumerating does); where it
    /// ended.
    pub(crate) fn enumerate(
        &self,
        from: Option<usize>,
        options: NSTextLayoutFragmentEnumerationOptions,
        mut f: impl FnMut(&NSTextLayoutFragment) -> bool,
    ) -> Option<usize> {
        let len = self.content_len();
        if options.contains(NSTextLayoutFragmentEnumerationOptions::EstimatesSize) {
            self.estimate_document();
        }
        let ensure = options.contains(NSTextLayoutFragmentEnumerationOptions::EnsuresLayout);
        let reverse = options.contains(NSTextLayoutFragmentEnumerationOptions::Reverse);
        if len == 0 {
            if options.contains(NSTextLayoutFragmentEnumerationOptions::EnsuresExtraLineFragment) {
                let extra = self.extra_fragment();
                f(&extra);
            }
            return None;
        }
        let result = if reverse {
            let mut at = from.unwrap_or(len).min(len);
            let mut edge = at;
            loop {
                if at == 0 {
                    break;
                }
                let Some((p, frag)) = self.materialize(at - 1) else { break };
                if ensure {
                    self.lay_out_at(p);
                }
                let start = self.place_frame(p.start);
                edge = start;
                if !f(&frag) || start == 0 {
                    break;
                }
                at = start;
            }
            Some(edge)
        } else {
            let mut at = from.unwrap_or(0).min(len);
            let mut edge = at;
            while at < len {
                let Some((p, frag)) = self.materialize(at) else { break };
                if ensure {
                    self.lay_out_at(p);
                }
                self.place_frame(p.start);
                let end = {
                    let mut st = self.ivars().state.borrow_mut();
                    let q = st.index.locate(p.start);
                    q.start + st.index.get(q).len
                };
                edge = end;
                if !f(&frag) || end <= at {
                    break;
                }
                at = end;
            }
            Some(edge)
        };
        if ensure {
            self.settle();
        }
        result
    }

    /// Put the fragment starting at `start` where the index says it is (a
    /// laid-out one moves with what changed above it); its start.
    fn place_frame(&self, start: usize) -> usize {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        let p = st.index.locate(start);
        let top = p.top(f);
        if let Some(slot) = &st.index.get(p).slot
            && slot.laid
            && let Some(iv) = fragment::ivars(&slot.fragment)
            && (iv.frame().origin.y - top).abs() > 1e-9
        {
            iv.set_y(top);
        }
        p.start
    }

    /// The empty document's extra line fragment, laid out.
    fn extra_fragment(&self) -> Retained<NSTextLayoutFragment> {
        if let Some(f) = self.ivars().state.borrow().extra.clone() {
            return f;
        }
        let p = element::storage_paragraph();
        let content = self.content();
        if let Some(c) = &content {
            let live = element::Live { start: 0, len: 0, stamp: u64::MAX, sep: 0, default: false };
            element::set_live(&p, &content::shared(c), live);
        }
        let frag = fragment::new_fragment(&p, None);
        let text = layout::Text { text: String::new(), spans: Vec::new() };
        let extra = self.extra_attrs(&text).unwrap_or_else(|| crate::string_drawing::attrs_of(None));
        let laid = {
            let st = self.ivars().state.borrow();
            let g = st.geometry;
            let container = crate::text::lines::Container {
                width: g.width,
                max_lines: 0,
                truncation: None,
                font_leading: g.font_leading,
            };
            let empty = crate::text::lines::Styled { text: "", attrs: std::slice::from_ref(&extra), spans: &[] };
            let lines = crate::text::lines::lay_out_paragraph(empty, &container, 0);
            let height = lines.height();
            let min_x = lines.lines.first().map_or(0.0, |l| l.x);
            Laid {
                paras: vec![layout::Para { start: 0, top: 0.0, lines: Arc::new(lines), lead: 0.0, trail: 0.0 }],
                min_x,
                max_x: min_x,
                height,
                len: 0,
            }
        };
        let pad = self.padding();
        if let Some(iv) = fragment::ivars(&frag) {
            iv.set_manager(&self.shared());
            iv.set_layout(Arc::new(laid.clone()), NSPoint::new(pad + f64::from(laid.min_x), 0.0));
        }
        self.ivars().state.borrow_mut().extra = Some(frag.clone());
        frag
    }

    /// The empty document's extra line fragment, for a viewport: made and
    /// laid out when the document is empty.
    pub(crate) fn empty_document_fragment(&self) -> Option<Retained<NSTextLayoutFragment>> {
        (self.content_len() == 0).then(|| self.extra_fragment())
    }

    /// The text view's typing attributes changed: the empty document's
    /// extra line fragment takes them when next laid out.
    pub(crate) fn typing_changed(&self) {
        if self.content_len() != 0 {
            return;
        }
        let had = self.ivars().state.borrow_mut().extra.take().is_some();
        if had {
            self.ivars().state.borrow_mut().damage.mark_moved(0.0);
            self.changed();
        }
    }

    /// The empty document's line: its extra line fragment's.
    fn extra_line(&self) -> Option<LineAt> {
        let f = self.extra_fragment();
        let laid = fragment::ivars(&f)?.laid()?;
        let place = self.frame_place(&f, &laid);
        let para = laid.paras.first()?;
        Some(line_of(0, place, para, 0))
    }

    // Questions about where text is.

    /// The union of the frames laid out, down to the bottom of the document
    /// as estimated: zero when nothing is laid out.
    pub(crate) fn usage_bounds(&self) -> NSRect {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        let (_, total) = st.index.total();
        if total.laid == 0 {
            // The empty document's extra line, when it has been laid out.
            if let Some(extra) = st.extra.clone()
                && let Some(iv) = fragment::ivars(&extra)
            {
                return iv.frame();
            }
            return NSRect::ZERO;
        }
        // From the first laid-out fragment's top to the last one's bottom,
        // or to the document's estimated bottom when that was asked for.
        let laid = |m: &Metrics| m.laid > 0;
        let top = st.index.first_where(laid).map_or(0.0, |p| p.top(f));
        let last = st.index.last_where(laid).map_or(0.0, |p| p.top(f) + seg_height(st.index.get(p), f));
        let bottom = if self.ivars().estimated.get() { last.max(total.height(f)) } else { last };
        NSRect::new(
            NSPoint::new(total.left, top),
            NSSize::new((total.right - total.left).max(0.0), (bottom - top).max(0.0)),
        )
    }

    /// The document's height, estimates included.
    pub(crate) fn height(&self) -> f64 {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        st.index.total().1.height(f)
    }

    /// The laid-out fragment holding point `p` (the first above the text);
    /// none below it.
    fn fragment_at_point(&self, p: NSPoint) -> Option<Retained<NSTextLayoutFragment>> {
        let len = self.content_len();
        if len == 0 {
            return None;
        }
        let y = p.y.max(0.0);
        if y >= self.height() {
            return None;
        }
        self.ensure_y(y, y);
        let o = self.offset_at_y(y);
        self.materialize(o).map(|(_, f)| f)
    }

    /// The lines of the fragment at `p`, with where they are.
    fn lines_of_fragment(&self, p: Pos) -> Vec<LineAt> {
        let (frag, start) = {
            let st = self.ivars().state.borrow();
            let Some(slot) = &st.index.get(p).slot else { return Vec::new() };
            (slot.fragment.clone(), p.start)
        };
        let Some(iv) = fragment::ivars(&frag) else { return Vec::new() };
        let Some(laid) = iv.laid() else { return Vec::new() };
        self.place_frame(start);
        let place = self.frame_place(&frag, &laid);
        laid.lines().map(|(para, i)| line_of(start, place, para, i)).collect()
    }

    /// Where a laid-out fragment puts its lines: the top of the frame it
    /// reports (a subclass's own) and how far across that frame is from
    /// where its lines are laid out (the padding and its leftmost line's
    /// start).
    fn frame_place(&self, frag: &NSTextLayoutFragment, laid: &Laid) -> (f64, f64) {
        // SAFETY: layoutFragmentFrame takes nothing; a subclass may
        // override it.
        let frame: NSRect = unsafe { msg_send![frag, layoutFragmentFrame] };
        (frame.origin.y, frame.origin.x - (self.padding() + f64::from(laid.min_x)))
    }

    /// The line holding `index` (the line before, where a wrapped line ends
    /// there, with `upstream`), its fragment laid out; an empty document's
    /// extra line.
    pub(crate) fn line_at(&self, index: usize, upstream: bool) -> Option<LineAt> {
        let len = self.content_len();
        if len == 0 {
            return self.extra_line();
        }
        let index = index.min(len);
        let (p, frag) = self.materialize(index)?;
        self.lay_out_at(p);
        let laid = fragment::ivars(&frag)?.laid()?;
        self.place_frame(p.start);
        let place = self.frame_place(&frag, &laid);
        let (para, i) = laid.line_at((index - p.start.min(index)) as u32, upstream)?;
        Some(line_of(p.start, place, para, i))
    }

    /// The top of the fragment holding offset `o`, as the index has it
    /// (estimated, for one not laid out), made if it isn't; nothing is
    /// laid out.
    pub(crate) fn estimated_top(&self, o: usize) -> f64 {
        let start = self.materialize(o).map_or(o, |(p, _)| p.start);
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        st.index.locate(start).top(f)
    }

    /// The top of the item holding offset `o`, and where it starts.
    pub(crate) fn anchor_at(&self, o: usize) -> (usize, f64) {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        let p = st.index.locate(o);
        (p.start, p.top(f))
    }

    /// The first laid-out fragment from height `y0` to `y1`: where it
    /// starts and its top (what a view keeps in place as layout above it
    /// changes).
    pub(crate) fn first_laid_in(&self, y0: f64, y1: f64) -> Option<(usize, f64)> {
        let mut st = self.ivars().state.borrow_mut();
        let f = st.factor;
        let mut p = st.index.locate_y(y0, f);
        loop {
            let top = p.top(f);
            if top > y1 {
                return None;
            }
            if st.index.get(p).slot.as_ref().is_some_and(|s| s.laid) {
                return Some((p.start, top));
            }
            p = st.index.next(p)?;
        }
    }

    fn pos_of(&self, start: usize) -> Pos {
        self.ivars().state.borrow_mut().index.locate(start)
    }

    /// The lines between heights `y0` and `y1`, laid out.
    pub(crate) fn lines_in_y(&self, y0: f64, y1: f64) -> Vec<LineAt> {
        self.ensure_y(y0, y1);
        let len = self.content_len();
        let mut out = Vec::new();
        if len == 0 {
            return out;
        }
        let mut p = {
            let mut st = self.ivars().state.borrow_mut();
            let f = st.factor;
            st.index.locate_y(y0, f)
        };
        loop {
            let (top, is_el) = {
                let st = self.ivars().state.borrow();
                (p.top(st.factor), st.index.get(p).slot.is_some())
            };
            if top > y1 {
                break;
            }
            if is_el {
                for l in self.lines_of_fragment(p) {
                    let (a, b) = l.fragment_span();
                    if b > y0 && a <= y1 {
                        out.push(l);
                    }
                }
            }
            let next = self.ivars().state.borrow_mut().index.next(p);
            match next {
                Some(n) => p = n,
                None => break,
            }
        }
        out
    }

    /// The line at point (`x`, `y`): the first above the text, the last
    /// below it.
    pub(crate) fn line_at_point(&self, _x: f64, y: f64) -> Option<LineAt> {
        self.line_at_y(y).map(|(l, _)| l)
    }

    /// The line at height `y`, and whether `y` is below it: the line of
    /// the fragment there holding `y`; where none does (a subclass's frame
    /// taller than its lines), its last line above `y` (and `y` is below
    /// it), or its first; the first line above the text, the document's
    /// last below it.
    fn line_at_y(&self, y: f64) -> Option<(LineAt, bool)> {
        let len = self.content_len();
        if len == 0 {
            return self.extra_line().map(|l| (l, false));
        }
        if y >= self.height() {
            return self.line_at(len, false).map(|l| (l, false));
        }
        let y = y.max(0.0);
        self.ensure_y(y, y);
        let (p, _) = self.materialize(self.offset_at_y(y))?;
        let lines = self.lines_of_fragment(self.pos_of(p.start));
        if let Some(l) = lines.iter().find(|l| {
            let (a, b) = l.fragment_span();
            a <= y && y < b
        }) {
            return Some((l.clone(), false));
        }
        match lines.iter().rev().find(|l| l.fragment_span().0 <= y) {
            Some(l) => Some((l.clone(), true)),
            None => lines
                .into_iter()
                .next()
                .map(|l| (l, false))
                .or_else(|| self.line_at(p.start, false).map(|l| (l, false))),
        }
    }

    /// Where an insertion point goes for a point: the index, and whether it
    /// belongs at the end of the line above. Below the lines of the
    /// fragment the point is in, the end of its last line.
    pub(crate) fn insertion_index(&self, p: NSPoint) -> (usize, bool) {
        let len = self.content_len();
        if len == 0 {
            return (0, false);
        }
        if p.y >= self.height() {
            return (len, false);
        }
        let Some((l, below)) = self.line_at_y(p.y) else { return (0, false) };
        if p.y < 0.0 && l.start == 0 && l.index == 0 {
            return (0, false);
        }
        if below {
            return (l.content_end(), false);
        }
        let hit = l.line().hit((p.x - self.padding() - l.left) as f32);
        ((l.start + hit.index as usize).min(len), hit.upstream)
    }

    /// The caret's rect for an insertion point at `index`.
    pub(crate) fn caret_rect(&self, index: usize, upstream: bool) -> NSRect {
        let Some(l) = self.line_at(index, upstream) else {
            // No content manager: nothing to lay out.
            return NSRect::new(NSPoint::new(self.padding(), 0.0), NSSize::new(1.0, 0.0));
        };
        let line = l.line();
        let (x, _) = line.caret_x((index.min(l.start + l.lines.len as usize) - l.start) as u32);
        NSRect::new(
            NSPoint::new(self.padding() + l.left + f64::from(x), l.line_top()),
            NSSize::new(1.0, f64::from(line.height)),
        )
    }

    /// The rects that show `range` selected, only the lines between `y0`
    /// and `y1`.
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
        crate::textkit::layout_manager::spans_of(&lines, range, self.padding(), self.container_width())
    }

    /// The rects of `range`'s characters on its first line, and the part of
    /// `range` that line holds.
    pub(crate) fn first_line_rects(&self, range: Range<usize>) -> (Vec<NSRect>, Range<usize>) {
        let Some(l) = self.line_at(range.start, false) else { return (Vec::new(), range) };
        let end = range.end.min(l.range().end).max(range.start);
        let content = range.end.min(l.content_end()).max(range.start);
        let rects = crate::textkit::layout_manager::spans_of(
            std::slice::from_ref(&l),
            range.start..content,
            self.padding(),
            self.container_width(),
        );
        (rects, range.start..end)
    }

    /// Whether the fragments holding `range` are laid out.
    pub(crate) fn is_laid(&self, range: Range<usize>) -> bool {
        let mut st = self.ivars().state.borrow_mut();
        let len = st.index.len();
        let mut p = st.index.locate(range.start.min(len));
        loop {
            if !st.index.get(p).slot.as_ref().is_some_and(|s| s.laid) {
                return false;
            }
            let end = p.start + st.index.get(p).len;
            if end >= range.end {
                return true;
            }
            match st.index.next(p) {
                Some(n) => p = n,
                None => return true,
            }
        }
    }

    /// How far right the frames laid out reach.
    pub(crate) fn used_width(&self) -> f64 {
        let r = self.usage_bounds();
        (r.origin.x + r.size.width).max(0.0)
    }

    /// What the views need to draw again since they last asked (container
    /// heights, to the bottom when text moved), and whether the extent may
    /// have changed.
    pub(crate) fn take_damage(&self) -> Option<(f64, f64, bool)> {
        let mut st = self.ivars().state.borrow_mut();
        let d = std::mem::take(&mut st.damage);
        let mut out = d.redraw;
        if let Some(y) = d.moved {
            out = Some((out.map_or(y, |o| o.0.min(y)), f64::INFINITY));
        }
        out.map(|(a, b)| (a, b, d.resized))
    }

    /// The segments of `range` of `kind` with `options`: (range, frame,
    /// baseline from the frame's line top) in container coordinates, one
    /// per line the range meets (a caret's, for an empty range). As
    /// measured on macOS (`conformance/tests/textkit2.rs`):
    ///
    /// - A selection or highlight segment stops at the line's trailing
    ///   edge (the container's width less its padding), reaches it where
    ///   the range goes on past the line or takes in its separator, starts
    ///   at the leading edge on every line but the first, and starts down
    ///   where the one before it ends (no gaps between lines).
    /// - `HeadSegmentExtended`: every line's segment but the first starts
    ///   at the leading edge.
    /// - `TailSegmentExtended`: segments the range goes on past reach the
    ///   trailing edge, the last one too where the range reaches its line's
    ///   end; where it stops short of the end of a paragraph's last line,
    ///   an empty segment at that line's end reaches from its text's end to
    ///   the edge.
    /// - `MiddleFragmentsExcluded`: only the first and last lines' segments
    ///   (a selection's last reaching up to the first's); with it, a last
    ///   segment after another reaches the trailing edge only where it takes
    ///   in a separator.
    fn segments(
        &self,
        range: Range<usize>,
        kind: NSTextLayoutManagerSegmentType,
        options: NSTextLayoutManagerSegmentOptions,
    ) -> Vec<(Range<usize>, NSRect, f64)> {
        type Options = NSTextLayoutManagerSegmentOptions;
        let pad = self.padding();
        let (lead_edge, trail_edge) = (pad, self.container_width() - pad);
        if range.is_empty() {
            let upstream = options.contains(Options::UpstreamAffinity);
            let Some(l) = self.line_at(range.start, upstream) else { return Vec::new() };
            let line = l.line();
            let at = range.start.clamp(l.start, l.start + l.lines.len as usize) - l.start;
            let (x, _) = line.caret_x(at as u32);
            let x = pad + l.left + f64::from(x);
            let r = NSRect::new(NSPoint::new(x, l.line_top()), NSSize::new(0.0, f64::from(line.height)));
            return vec![(range.clone(), r, f64::from(line.baseline))];
        }
        let selection = kind != NSTextLayoutManagerSegmentType::Standard;
        let head_extended = selection || options.contains(Options::HeadSegmentExtended);
        let tail_extended = options.contains(Options::TailSegmentExtended);
        self.ensure_range(range.clone());

        /// A line's segment, and what extending it needs of the line.
        struct Piece {
            range: Range<usize>,
            x0: f64,
            x1: f64,
            y0: f64,
            y1: f64,
            baseline: f64,
            rtl: bool,
            line_end: usize,
            /// It takes in the line's separator.
            separator: bool,
            /// Where the line's text ends, when it is its paragraph's last.
            para_end: Option<f64>,
        }
        let mut pieces: Vec<Piece> = Vec::new();
        let len = self.content_len();
        let mut o = range.start;
        while o < range.end.min(len) {
            let Some((p, _)) = self.materialize(o) else { break };
            let end = self.end_of(p.start);
            for l in self.lines_of_fragment(self.pos_of(p.start)) {
                let lr = l.range();
                let (a, b) = (lr.start.max(range.start), lr.end.min(range.end));
                if a >= b {
                    continue;
                }
                let line = l.line();
                let left = pad + l.left;
                let spans = line.spans((a - l.start) as u32..(b - l.start) as u32);
                let (x0, x1) =
                    spans.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |acc, s| (acc.0.min(s.0), acc.1.max(s.1)));
                let (mut x0, mut x1) =
                    if x0 <= x1 { (left + f64::from(x0), left + f64::from(x1)) } else { (left, left) };
                let separator = b > l.content_end() && range.end > l.content_end();
                if selection {
                    if separator {
                        // Taking in the separator: on to the far edge.
                        if line.rtl {
                            x0 = lead_edge;
                        } else {
                            x1 = trail_edge;
                        }
                    }
                    x1 = x1.min(trail_edge).max(x0);
                }
                let top = l.line_top();
                let para_end = l.is_last().then(|| {
                    let (x, _) = line.caret_x((l.content_end() - l.start) as u32);
                    left + f64::from(x)
                });
                pieces.push(Piece {
                    range: a..b,
                    x0,
                    x1,
                    y0: top,
                    y1: top + f64::from(line.height),
                    baseline: f64::from(line.baseline),
                    rtl: line.rtl,
                    line_end: lr.end,
                    separator,
                    para_end,
                });
            }
            if end <= o {
                break;
            }
            o = end;
        }
        let middle_excluded = options.contains(Options::MiddleFragmentsExcluded);
        if middle_excluded && pieces.len() > 2 {
            let last = pieces.pop().expect("more than two");
            pieces.truncate(1);
            pieces.push(last);
        }
        let n = pieces.len();
        let mut extra = None;
        for i in 0..n {
            let prev_bottom = (i > 0).then(|| pieces[i - 1].y1);
            let s = &mut pieces[i];
            // Toward the edges, whichever way the line runs.
            let (lead, trail) = if s.rtl { (trail_edge, lead_edge) } else { (lead_edge, trail_edge) };
            let reach = |x0: &mut f64, x1: &mut f64, edge: f64| {
                if edge <= *x0 {
                    *x0 = edge;
                } else {
                    *x1 = x1.max(edge);
                }
            };
            if i > 0 && head_extended {
                reach(&mut s.x0, &mut s.x1, lead);
            }
            let last = i + 1 == n;
            if !last && (selection || tail_extended) {
                reach(&mut s.x0, &mut s.x1, trail);
            }
            if let Some(y) = prev_bottom.filter(|_| selection) {
                s.y0 = y.min(s.y0);
            }
            if last && tail_extended {
                if range.end >= s.line_end {
                    if !middle_excluded || n == 1 || s.separator {
                        reach(&mut s.x0, &mut s.x1, trail);
                    }
                } else if let Some(x) = s.para_end {
                    let (x0, x1) = if s.rtl { (trail, x) } else { (x, trail) };
                    let r = NSRect::new(NSPoint::new(x0, s.y0), NSSize::new((x1 - x0).max(0.0), s.y1 - s.y0));
                    extra = Some((s.line_end..s.line_end, r, s.baseline));
                }
            }
        }
        let mut out: Vec<(Range<usize>, NSRect, f64)> = pieces
            .into_iter()
            .map(|s| {
                let r = NSRect::new(NSPoint::new(s.x0, s.y0), NSSize::new(s.x1 - s.x0, s.y1 - s.y0));
                (s.range, r, s.baseline)
            })
            .collect();
        out.extend(extra);
        out
    }
}

/// Line `i` of `para` of the fragment starting at `start`, placed as
/// `frame_place` says (its frame's top, and how far across it is), as
/// TextKit 1's layout manager describes a line.
fn line_of(start: usize, (top, left): (f64, f64), para: &layout::Para, i: usize) -> LineAt {
    LineAt {
        para: 0,
        start: start + para.start as usize,
        top: top + f64::from(para.top),
        lines: para.lines.clone(),
        index: i,
        lead: f64::from(para.lead),
        trail: f64::from(para.trail),
        left,
        width: None,
    }
}

/// An item's height, estimates scaled.
fn seg_height(seg: &Seg, factor: f64) -> f64 {
    seg.fixed + factor * seg.raw
}

/// The estimate of a paragraph `len` units long in a font `size` points
/// tall, lines `width` wide: TextKit 1's.
fn estimate(len: f64, size: f64, width: f32) -> f64 {
    f64::from(crate::textkit::layout_cache::estimate_height(len as f32, size as f32, width))
}

/// The font size of attribute id `attrs` of a storage (12 for none).
fn font_size(resolved: &mut Resolved, table: &crate::textkit::attrs::AttrTable, attrs: Option<u32>) -> f64 {
    match attrs {
        Some(id) if (id as usize) < table.len() => {
            let i = resolved.index(Some(table.dict(id)));
            resolved.attrs.get(i as usize).map_or(12.0, |a| f64::from(a.font.size))
        }
        _ => 12.0,
    }
}

/// Font sizes of a storage's attribute ids, the last one remembered (runs
/// of paragraphs share their attributes).
#[derive(Default)]
struct FontSizes {
    last: Option<(Option<u32>, f64)>,
}

impl FontSizes {
    fn get(&mut self, resolved: &mut Resolved, table: &crate::textkit::attrs::AttrTable, attrs: Option<u32>) -> f64 {
        match self.last {
            Some((id, size)) if id == attrs => size,
            _ => {
                let size = font_size(resolved, table, attrs);
                self.last = Some((attrs, size));
                size
            }
        }
    }

    /// An extent of a storage's paragraphs: its length and its estimated
    /// height (before scaling), in the state's geometry.
    fn estimate(
        &mut self,
        e: &crate::textkit::storage::Extent,
        st: &mut State,
        table: &crate::textkit::attrs::AttrTable,
    ) -> (usize, f64) {
        let width = st.geometry.width;
        match *e {
            crate::textkit::storage::Extent::One { len16, attrs } => {
                let size = self.get(&mut st.resolved, table, attrs);
                (len16 as usize, estimate(f64::from(len16), size, width))
            }
            crate::textkit::storage::Extent::Many { count, len16, attrs } => {
                let size = self.get(&mut st.resolved, table, attrs);
                (len16, count as f64 * estimate(len16 as f64 / count.max(1) as f64, size, width))
            }
        }
    }
}

/// Whether an element's text ends in a paragraph separator: a content
/// storage's paragraph knows, a program's paragraph is asked
/// (`paragraphSeparatorRange`, as AppKit asks), anything else's text is
/// looked at.
fn ends_in_separator(e: &AnyObject, text: &layout::Text) -> bool {
    if let Some(live) = element::ivars(e).and_then(|iv| iv.live.get())
        && live.default
    {
        return live.sep > 0;
    }
    if crate::textkit::responds(e, sel!(paragraphSeparatorRange)) {
        // SAFETY: paragraphSeparatorRange takes nothing and returns a range
        // or nil.
        let r: Option<Retained<NSTextRange>> = unsafe { msg_send![e, paragraphSeparatorRange] };
        if let Some((a, b)) = r.as_deref().and_then(span_of) {
            return b > a;
        }
    }
    layout::ends_in_separator(&text.text)
}

/// A layout manager of Sidestep's (or a subclass).
pub(crate) fn imp(m: &AnyObject) -> Option<&NSTextLayoutManagerImpl> {
    let ours = <NSTextLayoutManager as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(m.class(), ours)
        .then(|| unsafe { &*(m as *const AnyObject).cast::<NSTextLayoutManagerImpl>() })
}

/// A layout manager of Sidestep's (or a subclass), kept.
pub(crate) fn manager_of(m: &AnyObject) -> Option<Retained<NSTextLayoutManagerImpl>> {
    imp(m)?;
    // SAFETY: checked to be Sidestep's layout manager (or a subclass).
    Some(unsafe { Retained::cast_unchecked(m.retain()) })
}

/// `manager` now lays out `content` (or nothing).
pub(crate) fn attach_content(manager: &NSTextLayoutManager, content: Option<&AnyObject>) {
    if let Some(m) = imp(manager) {
        *m.ivars().content.borrow_mut() = content.map_or_else(Weak::default, Weak::new);
        m.rebuild();
    }
}

/// The content manager of `manager` changed `range` by `delta` units.
pub(crate) fn content_changed(
    manager: &NSTextLayoutManager,
    range: Range<usize>,
    exact: Range<usize>,
    delta: isize,
    characters: bool,
) {
    if let Some(m) = imp(manager) {
        m.content_edited(range, exact, delta, characters);
    }
}

/// The content storage of `manager` has a new text storage.
pub(crate) fn content_replaced(manager: &NSTextLayoutManager) {
    if let Some(m) = imp(manager) {
        m.rebuild();
        if let Some(v) = m.view() {
            let storage =
                m.content().and_then(|c| content::as_storage(&c).and_then(NSTextContentStorageImpl::text_storage_now));
            crate::textkit::text_view::storage_replaced(&v, storage.as_deref());
        }
    }
}

/// A fragment of `manager` asked to be laid out again.
pub(crate) fn fragment_invalidated(manager: &AnyObject, fragment: &AnyObject) {
    if let Some(m) = imp(manager) {
        m.fragment_unlaid(fragment);
    }
}

/// `manager`'s container changed its geometry.
pub(crate) fn container_changed(manager: &AnyObject) {
    if let Some(m) = imp(manager) {
        m.geometry_changed();
    }
}
