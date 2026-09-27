//! `NSTextLayoutFragment` and `NSTextLineFragment`: an element laid out,
//! and its lines.
//!
//! A layout fragment belongs to an element, over its whole range (or the
//! range it was made with). Its layout manager lays it out (`layout`) and
//! places it: the frame's origin is where the fragment is in the
//! container, its size what its lines take (see `layout` for how they are
//! stacked). A subclass may override `layoutFragmentFrame` (to be taller,
//! or of no height at all): the layout manager stacks the fragments below
//! by what the method says, as on macOS, and `super` answers with the
//! frame worked out here. The default `drawAtPoint:inContext:` draws the
//! lines with the fragment's frame origin at the point (through
//! `draw::with_context_state`); a subclass's is called in its place.
//!
//! Measured on macOS (`conformance/tests/textkit2.rs`): a fragment not laid
//! out is in state 0 with a zero frame; laid out, state 3. A line
//! fragment's character range is in its element's text; its typographic
//! bounds are in the layout fragment's frame; its glyph origin is the
//! baseline's height from its top; `locationForCharacterAtIndex:` is from
//! its typographic origin, at the baseline; drawing a line puts its
//! typographic origin at the point.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSTextLayoutFragment, NSTextLayoutFragmentState, NSTextLineFragment, NSTextRange};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSArray, NSAttributedString, NSDictionary, NSPoint, NSRange, NSRect, NSSize, NSString};

use super::SharedWeak;
use super::element;
use super::layout::{Laid, Para};
use super::location::{self, offset_of, span_of};
use crate::attachment::{Drawn, Engine, Setting};

sidestep_runtime::static_class!(pub(crate) NSTEXTLAYOUTFRAGMENT, NSTEXTLAYOUTFRAGMENT_META = "NSTextLayoutFragment", || {
    let _ = NSTextLayoutFragmentImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTLINEFRAGMENT, NSTEXTLINEFRAGMENT_META = "NSTextLineFragment", || {
    let _ = NSTextLineFragmentImpl::class();
});

pub(crate) struct FragmentIvars {
    element: RefCell<Option<Retained<AnyObject>>>,
    /// The range it was made with, when not its element's whole range.
    range: RefCell<Option<Retained<NSTextRange>>>,
    /// Its layout manager, through the weak reference the manager hands
    /// all its fragments.
    manager: RefCell<Option<SharedWeak>>,
    state: Cell<NSTextLayoutFragmentState>,
    /// The frame worked out here (what `super` answers).
    frame: Cell<NSRect>,
    laid: RefCell<Option<Arc<Laid>>>,
    lines: RefCell<Option<Retained<NSArray<NSTextLineFragment>>>>,
    queue: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextLayoutFragment"]
    #[ivars = FragmentIvars]
    pub(crate) struct NSTextLayoutFragmentImpl;

    impl NSTextLayoutFragmentImpl {
        #[unsafe(method_id(initWithTextElement:range:))]
        fn init_with_text_element(
            this: Allocated<Self>,
            element: &AnyObject,
            range: Option<&NSTextRange>,
        ) -> Retained<Self> {
            // A range that is the element's whole range follows it.
            let whole = range.is_none_or(|r| {
                let e = element::span(element);
                e.is_some() && span_of(r) == e
            });
            let this = this.set_ivars(FragmentIvars {
                element: RefCell::new(Some(element.retain())),
                range: RefCell::new(if whole { None } else { range.map(|r| r.retain()) }),
                manager: RefCell::new(None),
                state: Cell::new(NSTextLayoutFragmentState::None),
                frame: Cell::new(NSRect::ZERO),
                laid: RefCell::new(None),
                lines: RefCell::new(None),
                queue: RefCell::new(None),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(FragmentIvars {
                element: RefCell::new(None),
                range: RefCell::new(None),
                manager: RefCell::new(None),
                state: Cell::new(NSTextLayoutFragmentState::None),
                frame: Cell::new(NSRect::ZERO),
                laid: RefCell::new(None),
                lines: RefCell::new(None),
                queue: RefCell::new(None),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(textLayoutManager))]
        fn text_layout_manager(&self) -> Option<Retained<AnyObject>> {
            self.ivars().manager.borrow().as_ref().and_then(SharedWeak::load)
        }

        #[unsafe(method_id(textElement))]
        fn text_element(&self) -> Option<Retained<AnyObject>> {
            self.ivars().element.borrow().clone()
        }

        #[unsafe(method_id(rangeInElement))]
        fn range_in_element(&self) -> Retained<NSTextRange> {
            self.range()
        }

        #[unsafe(method_id(textLineFragments))]
        fn text_line_fragments(&self) -> Retained<NSArray<NSTextLineFragment>> {
            self.lines()
        }

        #[unsafe(method_id(textLineFragmentForVerticalOffset:requiresExactMatch:))]
        fn line_for_vertical_offset(&self, y: f64, exact: bool) -> Option<Retained<NSTextLineFragment>> {
            self.lines().iter().find(|l| {
                let b = line_ivars(l).bounds.get();
                let (top, bottom) = (b.origin.y, b.origin.y + b.size.height);
                if exact { top <= y && y < bottom } else { y < bottom }
            })
        }

        #[unsafe(method_id(textLineFragmentForTextLocation:isUpstreamAffinity:))]
        fn line_for_text_location(&self, location: &AnyObject, upstream: bool) -> Option<Retained<NSTextLineFragment>> {
            self.line_for_location(location, upstream)
        }

        #[unsafe(method_id(layoutQueue))]
        fn layout_queue(&self) -> Option<Retained<AnyObject>> {
            self.ivars().queue.borrow().clone()
        }

        #[unsafe(method(setLayoutQueue:))]
        fn set_layout_queue(&self, queue: Option<&AnyObject>) {
            *self.ivars().queue.borrow_mut() = queue.map(|q| q.retain());
        }

        #[unsafe(method(state))]
        fn state(&self) -> NSTextLayoutFragmentState {
            self.ivars().state.get()
        }

        #[unsafe(method(invalidateLayout))]
        fn invalidate_layout(&self) {
            self.ivars().state.set(NSTextLayoutFragmentState::None);
            let manager = self.ivars().manager.borrow().as_ref().and_then(SharedWeak::load);
            if let Some(m) = manager {
                super::layout_manager::fragment_invalidated(&m, self.as_object());
            }
        }

        #[unsafe(method(layoutFragmentFrame))]
        fn layout_fragment_frame(&self) -> NSRect {
            self.ivars().frame.get()
        }

        #[unsafe(method(renderingSurfaceBounds))]
        fn rendering_surface_bounds(&self) -> NSRect {
            let Some(laid) = self.ivars().laid.borrow().clone() else { return NSRect::ZERO };
            let f = self.ivars().frame.get();
            surface(&laid, f.size)
        }

        #[unsafe(method(leadingPadding))]
        fn leading_padding(&self) -> f64 {
            0.0
        }

        #[unsafe(method(trailingPadding))]
        fn trailing_padding(&self) -> f64 {
            0.0
        }

        #[unsafe(method(topMargin))]
        fn top_margin(&self) -> f64 {
            0.0
        }

        #[unsafe(method(bottomMargin))]
        fn bottom_margin(&self) -> f64 {
            0.0
        }

        #[unsafe(method(drawAtPoint:inContext:))]
        fn draw_at_point(&self, point: NSPoint, cg: &CGContext) {
            // A fragment not laid out is laid out first, as on macOS
            // (`conformance/tests/textkit2.rs`,
            // `a_fragment_draws_into_a_bitmap_context`).
            if !self.ivars().is_laid() {
                let manager = self.ivars().manager.borrow().as_ref().and_then(SharedWeak::load);
                if let (Some(m), Some((a, b))) = (manager, self.ivars().span())
                    && let Some(m) = super::layout_manager::imp(&m)
                {
                    m.ensure_range(a..b);
                }
            }
            let Some(laid) = self.ivars().laid.borrow().clone() else { return };
            let element = self.ivars().element.borrow().clone();
            let manager = self.ivars().manager.borrow().as_ref().and_then(SharedWeak::load);
            let setting = manager.as_deref().and_then(super::layout_manager::imp).map(|m| m.attachment_setting());
            let setting = setting.unwrap_or_else(|| Setting::textkit(Engine::TextKit2, None, f64::INFINITY));
            let origin = self.element_span().map_or(0, |(a, _)| a);
            let drawn = Drawing { element: element.as_deref(), setting: &setting, origin };
            super::draw::with_context_state(cg, || draw_laid(&laid, point, &drawn));
        }

        #[unsafe(method_id(textAttachmentViewProviders))]
        fn text_attachment_view_providers(&self) -> Retained<NSArray<AnyObject>> {
            NSArray::new()
        }

        /// The attachment's box at `location`, from the fragment's frame
        /// origin; none if no attachment is there.
        #[unsafe(method(frameForTextAttachmentAtLocation:))]
        fn frame_for_text_attachment(&self, location: &AnyObject) -> NSRect {
            self.attachment_frame(location).unwrap_or(NSRect::ZERO)
        }
    }

    unsafe impl NSObjectProtocol for NSTextLayoutFragmentImpl {}
);

impl NSTextLayoutFragmentImpl {
    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }

    /// Its range: the one it was made with, else its element's, which
    /// laid out it covers only as far as its text goes (a delegate's
    /// paragraph shorter than the text it stands for, as on macOS).
    fn range(&self) -> Retained<NSTextRange> {
        if let Some(r) = self.ivars().range.borrow().as_ref() {
            return r.clone();
        }
        let element = self.ivars().element.borrow().clone();
        let r = element.and_then(|e| {
            // SAFETY: elementRange takes nothing.
            let r: Option<Retained<NSTextRange>> = unsafe { msg_send![&*e, elementRange] };
            r
        });
        let Some(r) = r else { return location::range(0, 0) };
        match (span_of(&r), self.ivars().laid_len()) {
            (Some((a, b)), Some(n)) if n < b - a => location::range(a, a + n),
            _ => r,
        }
    }

    fn lines(&self) -> Retained<NSArray<NSTextLineFragment>> {
        if let Some(l) = self.ivars().lines.borrow().as_ref() {
            return l.clone();
        }
        let made = self.make_lines();
        *self.ivars().lines.borrow_mut() = Some(made.clone());
        made
    }

    fn line_for_location(&self, location: &AnyObject, upstream: bool) -> Option<Retained<NSTextLineFragment>> {
        let (a, b) = self.element_span()?;
        let o = offset_of(location)?;
        if o < a || o >= b.max(a + 1) {
            return None;
        }
        let laid = self.ivars().laid.borrow().clone()?;
        let (para, i) = laid.line_at((o - a) as u32, upstream)?;
        let index = laid.lines().position(|(p, j)| std::ptr::eq(p, para) && j == i)?;
        let lines = self.lines();
        (index < lines.count()).then(|| lines.objectAtIndex(index))
    }

    fn element_span(&self) -> Option<(usize, usize)> {
        self.ivars().span()
    }

    fn attachment_frame(&self, location: &AnyObject) -> Option<NSRect> {
        let (a, _) = self.element_span()?;
        let index = u32::try_from(offset_of(location)?.checked_sub(a)?).ok()?;
        let laid = self.ivars().laid.borrow().clone()?;
        let (para, i) = laid.line_at(index, false)?;
        let l = &para.lines.lines[i];
        let at = index.checked_sub(para.start + l.range.start)?;
        let b = l.attachments.iter().find(|b| b.index == at)?;
        let (x0, y0) = (f64::from(b.rect[0] - laid.min_x), f64::from(para.top + l.top + b.rect[1]));
        Some(NSRect::new(
            NSPoint::new(x0, y0),
            NSSize::new(f64::from(b.rect[2] - b.rect[0]), f64::from(b.rect[3] - b.rect[1])),
        ))
    }

    fn make_lines(&self) -> Retained<NSArray<NSTextLineFragment>> {
        let Some(laid) = self.ivars().laid.borrow().clone() else { return NSArray::new() };
        let element = self.ivars().element.borrow().clone();
        let lines: Vec<Retained<NSTextLineFragment>> =
            laid.lines().map(|(para, i)| new_line(&laid, para, i, element.clone())).collect();
        NSArray::from_retained_slice(&lines)
    }
}

/// A fragment's ivars, if it is Sidestep's (or a subclass's).
pub(crate) fn ivars(f: &AnyObject) -> Option<&FragmentIvars> {
    let ours = <NSTextLayoutFragment as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(f.class(), ours)
        .then(|| unsafe { &*(f as *const AnyObject).cast::<NSTextLayoutFragmentImpl>() }.ivars())
}

impl FragmentIvars {
    pub(crate) fn element(&self) -> Option<Retained<AnyObject>> {
        self.element.borrow().clone()
    }

    /// Its layout manager is the one `manager` refers to.
    pub(crate) fn set_manager(&self, manager: &SharedWeak) {
        let mut m = self.manager.borrow_mut();
        if !m.as_ref().is_some_and(|m| m.same(manager)) {
            *m = Some(manager.clone());
        }
    }

    pub(crate) fn laid(&self) -> Option<Arc<Laid>> {
        self.laid.borrow().clone()
    }

    /// Its range in offsets, as `rangeInElement` has it (see `range`).
    pub(crate) fn span(&self) -> Option<(usize, usize)> {
        if let Some(r) = self.range.borrow().as_ref() {
            return span_of(r);
        }
        let e = self.element.borrow().clone()?;
        let (a, b) = element::span(&e)?;
        Some(match self.laid_len() {
            Some(n) if n < b - a => (a, a + n),
            _ => (a, b),
        })
    }

    /// The units it laid out, while laid out.
    fn laid_len(&self) -> Option<usize> {
        let laid = self.laid.borrow();
        laid.as_ref().filter(|_| self.is_laid()).map(|l| l.len as usize)
    }

    pub(crate) fn is_laid(&self) -> bool {
        self.state.get() == NSTextLayoutFragmentState::LayoutAvailable
    }

    /// Laid out as `laid`, its frame's origin at `origin` (`x` from the
    /// container's left: the padding and the leftmost line's start).
    pub(crate) fn set_layout(&self, laid: Arc<Laid>, origin: NSPoint) {
        let size = NSSize::new(f64::from(laid.max_x - laid.min_x), f64::from(laid.height));
        self.frame.set(NSRect::new(origin, size));
        *self.laid.borrow_mut() = Some(laid);
        *self.lines.borrow_mut() = None;
        self.state.set(NSTextLayoutFragmentState::LayoutAvailable);
    }

    /// Its layout is out of date: laid out again when next needed. The
    /// frame stays until then, as on macOS.
    pub(crate) fn unlay(&self) {
        self.state.set(NSTextLayoutFragmentState::None);
    }

    /// Its layout is gone (the container changed): state 0, zero frame and
    /// no lines, as macOS shows it.
    pub(crate) fn clear(&self) {
        self.state.set(NSTextLayoutFragmentState::None);
        self.frame.set(NSRect::ZERO);
        *self.laid.borrow_mut() = None;
        *self.lines.borrow_mut() = None;
    }

    /// Moved to `y` (laid out elsewhere above it).
    pub(crate) fn set_y(&self, y: f64) {
        let mut f = self.frame.get();
        f.origin.y = y;
        self.frame.set(f);
    }

    pub(crate) fn frame(&self) -> NSRect {
        self.frame.get()
    }
}

/// A new fragment for `element` over `range`, made by Sidestep.
pub(crate) fn new_fragment(element: &AnyObject, range: Option<&NSTextRange>) -> Retained<NSTextLayoutFragment> {
    crate::load_shell::<NSTextLayoutFragment>();
    // SAFETY: the designated initializer.
    unsafe { msg_send![NSTextLayoutFragment::alloc(), initWithTextElement: element, range: range] }
}

/// Where a fragment draws, from its frame's origin: its frame and its
/// lines' ink (each line's box widened by its height across and a quarter
/// of it up and down, which takes in overhanging glyphs).
fn surface(laid: &Laid, size: NSSize) -> NSRect {
    let (mut x0, mut y0, mut x1, mut y1) = (0.0f64, 0.0f64, size.width, size.height);
    for (para, i) in laid.lines() {
        let l = &para.lines.lines[i];
        if l.width <= 0.0 {
            continue;
        }
        let h = f64::from(l.height);
        let top = f64::from(para.top + l.top);
        let left = f64::from(l.x - laid.min_x);
        x0 = x0.min(left - h);
        x1 = x1.max(left + f64::from(l.width) + h);
        y0 = y0.min(top - h / 4.0);
        y1 = y1.max(top + h + h / 4.0);
    }
    NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
}

/// What a fragment's attachments are drawn with: its element, whose text
/// they are in, what they're told, and where the element's text starts in
/// the document.
pub(crate) struct Drawing<'a> {
    pub element: Option<&'a AnyObject>,
    pub setting: &'a Setting,
    pub origin: usize,
}

/// Record the lines of `laid` with the fragment's frame origin at `point`:
/// their backgrounds, then their glyphs, then the attachments of the
/// element's text in them.
pub(crate) fn draw_laid(laid: &Laid, point: NSPoint, drawing: &Drawing<'_>) {
    let mut boxes = Vec::new();
    for backgrounds in [true, false] {
        for (para, i) in laid.lines() {
            let l = &para.lines.lines[i];
            let origin = NSPoint::new(point.x - f64::from(laid.min_x), point.y + f64::from(para.top + l.top));
            crate::string_drawing::record_line(l, origin, backgrounds);
            if !backgrounds {
                let start = (para.start + l.range.start) as usize;
                boxes.extend(
                    crate::string_drawing::line_boxes(l, origin)
                        .into_iter()
                        .map(|(rect, a)| (rect, start + a.index as usize)),
                );
            }
        }
    }
    let element = drawing.element;
    draw_attachments(boxes, || element.and_then(|e| element::text_of(e, None)), drawing.setting, drawing.origin);
}

/// Draw the attachments of `boxes` (their rects in the view's space and
/// their characters' indexes in `text`'s), told they're `origin` further
/// on in the document.
fn draw_attachments(
    boxes: Vec<(NSRect, usize)>,
    text: impl FnOnce() -> Option<Retained<NSAttributedString>>,
    setting: &Setting,
    origin: usize,
) {
    if boxes.is_empty() {
        return;
    }
    let Some(text) = text() else { return };
    let len = text.length();
    // SAFETY: the key is a constant this crate exports.
    let key = unsafe { objc2_app_kit::NSAttachmentAttributeName };
    for (rect, index) in boxes.into_iter().filter(|b| b.1 < len) {
        // SAFETY: an index inside the text, and no range asked for.
        let attributes: Retained<NSDictionary<NSString, AnyObject>> =
            unsafe { msg_send![&*text, attributesAtIndex: index, effectiveRange: std::ptr::null_mut::<NSRange>()] };
        if let Some(value) = attributes.objectForKey(key) {
            let at = Drawn { rect, index: origin + index, attributes: Some(&attributes), view: None };
            crate::attachment::draw(&value, &at, setting, None);
        }
    }
}

// Line fragments.

pub(crate) struct LineIvars {
    text: RefCell<Option<Retained<NSAttributedString>>>,
    /// The element whose text it is, read when first asked for.
    element: RefCell<Option<Retained<AnyObject>>>,
    range: Cell<NSRange>,
    bounds: Cell<NSRect>,
    glyph_origin: Cell<NSPoint>,
    /// The line laid out: its paragraph's lines, its index there, and the
    /// paragraph's start in the text.
    line: RefCell<Option<(Arc<crate::text::lines::ParagraphLines>, usize, u32)>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextLineFragment"]
    #[ivars = LineIvars]
    pub(crate) struct NSTextLineFragmentImpl;

    impl NSTextLineFragmentImpl {
        #[unsafe(method_id(initWithAttributedString:range:))]
        fn init_with_attributed_string(
            this: Allocated<Self>,
            text: &NSAttributedString,
            range: NSRange,
        ) -> Retained<Self> {
            let this = this.set_ivars(LineIvars {
                text: RefCell::new(Some(text.retain())),
                element: RefCell::new(None),
                range: Cell::new(range),
                bounds: Cell::new(NSRect::ZERO),
                glyph_origin: Cell::new(NSPoint::ZERO),
                line: RefCell::new(None),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithString:attributes:range:))]
        fn init_with_string(
            this: Allocated<Self>,
            string: &NSString,
            attributes: &NSDictionary<NSString, AnyObject>,
            range: NSRange,
        ) -> Retained<Self> {
            // SAFETY: the attributes are a dictionary of attribute names.
            let text = unsafe { NSAttributedString::new_with_attributes(string, attributes) };
            // SAFETY: the initializer above.
            unsafe { msg_send![this, initWithAttributedString: &*text, range: range] }
        }

        #[unsafe(method_id(attributedString))]
        fn attributed_string(&self) -> Retained<NSAttributedString> {
            let known = self.ivars().text.borrow().clone();
            match known {
                Some(t) => t,
                None => {
                    let element = self.ivars().element.borrow().clone();
                    let text = element.and_then(|e| element::text_of(&e, None)).unwrap_or_default();
                    *self.ivars().text.borrow_mut() = Some(text.clone());
                    text
                }
            }
        }

        #[unsafe(method(characterRange))]
        fn character_range(&self) -> NSRange {
            self.ivars().range.get()
        }

        #[unsafe(method(typographicBounds))]
        fn typographic_bounds(&self) -> NSRect {
            self.ivars().bounds.get()
        }

        #[unsafe(method(glyphOrigin))]
        fn glyph_origin(&self) -> NSPoint {
            self.ivars().glyph_origin.get()
        }

        #[unsafe(method(drawAtPoint:inContext:))]
        fn draw_at_point(&self, point: NSPoint, cg: &CGContext) {
            let line = self.ivars().line.borrow().clone();
            let Some((lines, i, base)) = line else { return };
            super::draw::with_context_state(cg, || {
                let l = &lines.lines[i];
                let origin = NSPoint::new(point.x - f64::from(l.x), point.y);
                crate::string_drawing::record_line(l, origin, true);
                crate::string_drawing::record_line(l, origin, false);
                let start = (base + l.range.start) as usize;
                let boxes = crate::string_drawing::line_boxes(l, origin)
                    .into_iter()
                    .map(|(rect, a)| (rect, start + a.index as usize))
                    .collect();
                let setting = Setting::textkit(Engine::TextKit2, None, f64::INFINITY);
                let text = || {
                    // SAFETY: attributedString takes nothing.
                    let text: Retained<NSAttributedString> = unsafe { msg_send![self, attributedString] };
                    Some(text)
                };
                draw_attachments(boxes, text, &setting, 0);
            });
        }

        #[unsafe(method(locationForCharacterAtIndex:))]
        fn location_for_character_at_index(&self, index: isize) -> NSPoint {
            let line = self.ivars().line.borrow().clone();
            let Some((lines, i, base)) = line else { return NSPoint::ZERO };
            let l = &lines.lines[i];
            let at = (index.max(0) as u32).saturating_sub(base);
            let (x, _) = l.caret_x(at);
            NSPoint::new(f64::from(x - l.x), f64::from(l.baseline))
        }

        /// The character under the point (the first left of the line,
        /// NSNotFound past its end, as on macOS).
        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, p: NSPoint) -> isize {
            let line = self.ivars().line.borrow().clone();
            let Some((lines, i, base)) = line else { return self.ivars().range.get().location as isize };
            let l = &lines.lines[i];
            let x = p.x as f32 + l.x;
            if if l.rtl { x < l.x } else { x >= l.x + l.width } {
                return objc2_foundation::NSNotFound;
            }
            (base + l.hit(x).character) as isize
        }

        #[unsafe(method(fractionOfDistanceThroughGlyphForPoint:))]
        fn fraction_of_distance(&self, p: NSPoint) -> f64 {
            let line = self.ivars().line.borrow().clone();
            let Some((lines, i, _)) = line else { return 0.0 };
            let l = &lines.lines[i];
            f64::from(l.hit(p.x as f32 + l.x).fraction)
        }
    }

    unsafe impl NSObjectProtocol for NSTextLineFragmentImpl {}
);

fn line_ivars(l: &NSTextLineFragment) -> &LineIvars {
    // SAFETY: line fragments here are all Sidestep's (made by `new_line`
    // or a program through +alloc, which is this class).
    unsafe { &*(l as *const NSTextLineFragment).cast::<NSTextLineFragmentImpl>() }.ivars()
}

/// The line fragment for line `i` of `para` of `laid`.
fn new_line(laid: &Laid, para: &Para, i: usize, element: Option<Retained<AnyObject>>) -> Retained<NSTextLineFragment> {
    let l = &para.lines.lines[i];
    let range = NSRange::new((para.start + l.range.start) as usize, l.range.len());
    let bounds = NSRect::new(
        NSPoint::new(f64::from(l.x - laid.min_x), f64::from(para.top + l.top)),
        NSSize::new(f64::from(l.width), f64::from(l.height)),
    );
    crate::load_shell::<NSTextLineFragment>();
    let this = NSTextLineFragmentImpl::alloc().set_ivars(LineIvars {
        text: RefCell::new(None),
        element: RefCell::new(element),
        range: Cell::new(range),
        bounds: Cell::new(bounds),
        glyph_origin: Cell::new(NSPoint::new(0.0, f64::from(l.baseline))),
        line: RefCell::new(Some((para.lines.clone(), i, para.start))),
    });
    // SAFETY: NSObject's initializer.
    let this: Retained<NSTextLineFragmentImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSTextLineFragment is this class.
    unsafe { Retained::cast_unchecked(this) }
}
