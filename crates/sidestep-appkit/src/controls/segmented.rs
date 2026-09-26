//! `NSSegmentedControl` and `NSSegmentedCell`: a row of segments, one or
//! several of which are selected.
//!
//! What's observable follows AppKit (`conformance/tests/controls.rs`,
//! `segmented_controls`): a segment is its width if one was set, else its
//! label in the 13-point system font plus 20 points (18, 14 and 24 at the
//! small, mini and large sizes), or 24 points with no label; one point
//! divides segments; the control is as tall as its control size says.
//! Selecting a segment in select-one mode deselects the others;
//! deselecting one leaves `selectedSegment` pointing at it; in select-any
//! mode `selectedSegment` is the last one chosen; -1 deselects all.
//!
//! A press highlights the segment under the pointer, and a release on the
//! same segment chooses it: select-one selects it, select-any toggles it,
//! momentary selects it for the action and then deselects it. Disabled
//! segments can't be chosen. Wider frames share the extra room as the
//! distribution says.
//!
//! The rounded and automatic styles draw as a linked group on one trough,
//! the selected segments in the accent; the separated style draws each
//! segment as a pill of its own.
//!
//! The segments' natural widths are measured once per change to the cell
//! (its generation) and shared by sizing, drawing and tracking; a press
//! lays the segments out once and hit-tests every drag against that.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSActionCell, NSBackgroundStyle, NSCell, NSCellHitResult, NSColor, NSControl, NSEvent, NSEventMask, NSEventType,
    NSFont, NSImageScaling, NSResponder, NSSegmentDistribution, NSSegmentStyle, NSSegmentSwitchTracking,
    NSSegmentedCell, NSSegmentedControl, NSTextAlignment, NSView,
};
use objc2_foundation::{NSCopying, NSPoint, NSRect, NSSize, NSString, NSZone};

use super::cell::{Flags, Styled, attrs, imp as cell_imp};
use super::{control, track};
use crate::theme::{self, metrics, parts};

/// One segment's settings.
#[derive(Clone)]
struct Segment {
    label: Option<Retained<NSString>>,
    image: Option<Retained<AnyObject>>,
    image_scaling: NSImageScaling,
    width: f64,
    menu: Option<Retained<AnyObject>>,
    selected: bool,
    enabled: bool,
    tool_tip: Option<Retained<NSString>>,
    tag: isize,
    menu_indicator: bool,
    alignment: NSTextAlignment,
}

impl Default for Segment {
    fn default() -> Self {
        Segment {
            label: None,
            image: None,
            image_scaling: NSImageScaling::ScaleProportionallyDown,
            width: 0.0,
            menu: None,
            selected: false,
            enabled: true,
            tool_tip: None,
            tag: 0,
            menu_indicator: false,
            alignment: NSTextAlignment::Center,
        }
    }
}

pub(crate) struct SegmentedIvars {
    segments: RefCell<Vec<Segment>>,
    selected: Cell<isize>,
    tracking: Cell<NSSegmentSwitchTracking>,
    style: Cell<NSSegmentStyle>,
    distribution: Cell<NSSegmentDistribution>,
    bezel_color: RefCell<Option<Retained<NSColor>>>,
    spring_loaded: Cell<bool>,
    /// The segment being pressed, while the mouse is down.
    pressed: Cell<Option<usize>>,
    /// The segment the keyboard is on.
    key_segment: Cell<usize>,
    /// The segments' natural widths, and the cell generation they were
    /// measured at.
    natural: RefCell<Option<(u32, Rc<[f64]>)>>,
}

define_class!(
    #[unsafe(super(NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSegmentedCell"]
    #[ivars = SegmentedIvars]
    pub(crate) struct NSSegmentedCellImpl;

    impl NSSegmentedCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(SegmentedIvars {
                segments: RefCell::new(Vec::new()),
                selected: Cell::new(-1),
                tracking: Cell::new(NSSegmentSwitchTracking::SelectOne),
                style: Cell::new(NSSegmentStyle::Automatic),
                distribution: Cell::new(NSSegmentDistribution::Fill),
                bezel_color: RefCell::new(None),
                spring_loaded: Cell::new(false),
                pressed: Cell::new(None),
                key_segment: Cell::new(0),
                natural: RefCell::new(None),
            });
            // SAFETY: NSActionCell's initializer.
            unsafe { msg_send![super(this), initTextCell: string] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, _image: Option<&AnyObject>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method(segmentCount))]
        fn segment_count(&self) -> isize {
            self.ivars().segments.borrow().len() as isize
        }

        #[unsafe(method(setSegmentCount:))]
        fn set_segment_count(&self, count: isize) {
            let count = count.max(0) as usize;
            self.ivars().segments.borrow_mut().resize_with(count, Segment::default);
            if self.ivars().selected.get() >= count as isize {
                self.ivars().selected.set(-1);
            }
            self.changed();
        }

        #[unsafe(method(selectedSegment))]
        fn selected_segment(&self) -> isize {
            self.ivars().selected.get()
        }

        #[unsafe(method(setSelectedSegment:))]
        fn set_selected_segment(&self, segment: isize) {
            select_segment(self, segment);
        }

        #[unsafe(method(selectSegmentWithTag:))]
        fn select_segment_with_tag(&self, tag: isize) -> bool {
            let found = self.ivars().segments.borrow().iter().position(|s| s.tag == tag);
            match found {
                Some(i) => {
                    select_segment(self, i as isize);
                    true
                }
                None => false,
            }
        }

        #[unsafe(method(makeNextSegmentKey))]
        fn make_next_segment_key(&self) {
            let n = self.ivars().segments.borrow().len();
            if n > 0 {
                self.ivars().key_segment.set((self.ivars().key_segment.get() + 1) % n);
            }
            self.redraw();
        }

        #[unsafe(method(makePreviousSegmentKey))]
        fn make_previous_segment_key(&self) {
            let n = self.ivars().segments.borrow().len();
            if n > 0 {
                self.ivars().key_segment.set((self.ivars().key_segment.get() + n - 1) % n);
            }
            self.redraw();
        }

        #[unsafe(method(trackingMode))]
        fn tracking_mode(&self) -> NSSegmentSwitchTracking {
            self.ivars().tracking.get()
        }

        #[unsafe(method(setTrackingMode:))]
        fn set_tracking_mode(&self, mode: NSSegmentSwitchTracking) {
            self.ivars().tracking.set(mode);
        }

        #[unsafe(method(segmentStyle))]
        fn segment_style(&self) -> NSSegmentStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setSegmentStyle:))]
        fn set_segment_style(&self, style: NSSegmentStyle) {
            self.ivars().style.set(style);
            self.redraw();
        }

        // Per segment.

        #[unsafe(method(setWidth:forSegment:))]
        fn set_width(&self, width: f64, segment: isize) {
            self.edit(segment, |s| s.width = width);
            self.changed();
        }

        #[unsafe(method(widthForSegment:))]
        fn width_for_segment(&self, segment: isize) -> f64 {
            self.read(segment, |s| s.width)
        }

        #[unsafe(method(setImage:forSegment:))]
        fn set_image(&self, image: Option<&AnyObject>, segment: isize) {
            self.edit(segment, |s| s.image = image.map(|i| i.retain()));
            self.changed();
        }

        #[unsafe(method_id(imageForSegment:))]
        fn image_for_segment(&self, segment: isize) -> Option<Retained<AnyObject>> {
            self.read(segment, |s| s.image.clone())
        }

        #[unsafe(method(setImageScaling:forSegment:))]
        fn set_image_scaling(&self, scaling: NSImageScaling, segment: isize) {
            self.edit(segment, |s| s.image_scaling = scaling);
        }

        #[unsafe(method(imageScalingForSegment:))]
        fn image_scaling_for_segment(&self, segment: isize) -> NSImageScaling {
            self.read(segment, |s| s.image_scaling)
        }

        #[unsafe(method(setLabel:forSegment:))]
        fn set_label(&self, label: &NSString, segment: isize) {
            let label = label.copy();
            self.edit(segment, |s| s.label = Some(label));
            self.changed();
        }

        #[unsafe(method_id(labelForSegment:))]
        fn label_for_segment(&self, segment: isize) -> Option<Retained<NSString>> {
            self.read(segment, |s| s.label.clone())
        }

        #[unsafe(method(setSelected:forSegment:))]
        fn set_selected(&self, selected: bool, segment: isize) {
            set_selected(self, selected, segment);
        }

        #[unsafe(method(isSelectedForSegment:))]
        fn is_selected_for_segment(&self, segment: isize) -> bool {
            self.read(segment, |s| s.selected)
        }

        #[unsafe(method(setEnabled:forSegment:))]
        fn set_enabled_for_segment(&self, enabled: bool, segment: isize) {
            self.edit(segment, |s| s.enabled = enabled);
            self.redraw();
        }

        #[unsafe(method(isEnabledForSegment:))]
        fn is_enabled_for_segment(&self, segment: isize) -> bool {
            self.read(segment, |s| s.enabled)
        }

        #[unsafe(method(setMenu:forSegment:))]
        fn set_menu(&self, menu: Option<&AnyObject>, segment: isize) {
            self.edit(segment, |s| s.menu = menu.map(|m| m.retain()));
        }

        #[unsafe(method_id(menuForSegment:))]
        fn menu_for_segment(&self, segment: isize) -> Option<Retained<AnyObject>> {
            self.read(segment, |s| s.menu.clone())
        }

        #[unsafe(method(setToolTip:forSegment:))]
        fn set_tool_tip(&self, tip: Option<&NSString>, segment: isize) {
            let tip = tip.map(|t| t.copy());
            self.edit(segment, |s| s.tool_tip = tip);
        }

        #[unsafe(method_id(toolTipForSegment:))]
        fn tool_tip_for_segment(&self, segment: isize) -> Option<Retained<NSString>> {
            self.read(segment, |s| s.tool_tip.clone())
        }

        #[unsafe(method(setTag:forSegment:))]
        fn set_tag(&self, tag: isize, segment: isize) {
            self.edit(segment, |s| s.tag = tag);
        }

        #[unsafe(method(tagForSegment:))]
        fn tag_for_segment(&self, segment: isize) -> isize {
            self.read(segment, |s| s.tag)
        }

        #[unsafe(method(setShowsMenuIndicator:forSegment:))]
        fn set_shows_menu_indicator(&self, flag: bool, segment: isize) {
            self.edit(segment, |s| s.menu_indicator = flag);
        }

        #[unsafe(method(showsMenuIndicatorForSegment:))]
        fn shows_menu_indicator_for_segment(&self, segment: isize) -> bool {
            self.read(segment, |s| s.menu_indicator)
        }

        #[unsafe(method(setAlignment:forSegment:))]
        fn set_alignment_for_segment(&self, alignment: NSTextAlignment, segment: isize) {
            self.edit(segment, |s| s.alignment = alignment);
            self.redraw();
        }

        #[unsafe(method(alignmentForSegment:))]
        fn alignment_for_segment(&self, segment: isize) -> NSTextAlignment {
            self.read(segment, |s| s.alignment)
        }

        #[unsafe(method(interiorBackgroundStyleForSegment:))]
        fn interior_background_style_for_segment(&self, segment: isize) -> NSBackgroundStyle {
            if self.read(segment, |s| s.selected) { NSBackgroundStyle::Emphasized } else { NSBackgroundStyle::Normal }
        }

        // Geometry and drawing.

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            natural_size(self)
        }

        #[unsafe(method(cellSizeForBounds:))]
        fn cell_size_for_bounds(&self, _bounds: NSRect) -> NSSize {
            natural_size(self)
        }

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            draw(self, frame, view);
        }

        #[unsafe(method(drawSegment:inFrame:withView:))]
        fn draw_segment(&self, segment: isize, frame: NSRect, view: &NSView) {
            draw_label(self, segment as usize, frame, view);
        }

        /// Segments track their own clicks (`hit_tests`).
        #[unsafe(method(hitTestForEvent:inRect:ofView:))]
        fn hit_test_for_event(&self, _event: &NSEvent, _frame: NSRect, _view: &NSView) -> NSCellHitResult {
            NSCellHitResult::None
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSActionCell's copyWithZone: returns a cell of this
            // class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            // SAFETY: the copy is an instance of the receiver's class.
            let theirs = unsafe { &*(Retained::as_ptr(&copy).cast::<NSSegmentedCellImpl>()) };
            let (mine, copied) = (self.ivars(), theirs.ivars());
            copied.segments.replace(mine.segments.borrow().clone());
            copied.selected.set(mine.selected.get());
            copied.tracking.set(mine.tracking.get());
            copied.style.set(mine.style.get());
            copied.distribution.set(mine.distribution.get());
            copied.bezel_color.replace(mine.bezel_color.borrow().clone());
            copied.spring_loaded.set(mine.spring_loaded.get());
            copied.key_segment.set(mine.key_segment.get());
            copy
        }
    }

    unsafe impl NSObjectProtocol for NSSegmentedCellImpl {}
);

impl NSSegmentedCellImpl {
    fn as_cell(&self) -> &NSCell {
        // SAFETY: NSSegmentedCell is a subclass of NSCell.
        unsafe { &*(self as *const Self).cast::<NSCell>() }
    }

    fn edit(&self, segment: isize, f: impl FnOnce(&mut Segment)) {
        let mut segments = self.ivars().segments.borrow_mut();
        let len = segments.len();
        match usize::try_from(segment).ok().and_then(|i| segments.get_mut(i)) {
            Some(s) => f(s),
            None => panic!("-[NSSegmentedCell]: segment {segment} out of range; segment count {len}"),
        }
    }

    fn read<R>(&self, segment: isize, f: impl FnOnce(&Segment) -> R) -> R {
        let segments = self.ivars().segments.borrow();
        let len = segments.len();
        match usize::try_from(segment).ok().and_then(|i| segments.get(i)) {
            Some(s) => f(s),
            None => panic!("-[NSSegmentedCell]: segment {segment} out of range; segment count {len}"),
        }
    }

    /// Sizes changed: the control forgets its size and redraws.
    fn changed(&self) {
        super::cell::changed(cell_imp(self.as_cell()));
    }

    fn redraw(&self) {
        if let Some(view) = cell_imp(self.as_cell()).view() {
            view.setNeedsDisplay(true);
        }
    }
}

/// Any segmented cell as the implementation.
fn segmented_cell(cell: &NSCell) -> Option<&NSSegmentedCellImpl> {
    // SAFETY: NSSegmentedCellImpl is the class NSSegmentedCell names.
    unsafe { super::impl_of::<NSSegmentedCell, NSSegmentedCellImpl>(cell) }
}

/// `setSelectedSegment:`.
fn select_segment(cell: &NSSegmentedCellImpl, segment: isize) {
    {
        let mut segments = cell.ivars().segments.borrow_mut();
        let any = cell.ivars().tracking.get() == NSSegmentSwitchTracking::SelectAny;
        for (i, s) in segments.iter_mut().enumerate() {
            if segment < 0 || !any {
                s.selected = i as isize == segment;
            } else if i as isize == segment {
                s.selected = true;
            }
        }
    }
    let count = cell.ivars().segments.borrow().len() as isize;
    cell.ivars().selected.set(if segment < count { segment.max(-1) } else { -1 });
    cell.redraw();
}

/// `setSelected:forSegment:`: selecting one in select-one mode deselects
/// the others; deselecting leaves `selectedSegment` alone.
fn set_selected(cell: &NSSegmentedCellImpl, selected: bool, segment: isize) {
    let one = cell.ivars().tracking.get() != NSSegmentSwitchTracking::SelectAny;
    cell.edit(segment, |s| s.selected = selected);
    if selected {
        if one {
            let mut segments = cell.ivars().segments.borrow_mut();
            for (i, s) in segments.iter_mut().enumerate() {
                s.selected = i as isize == segment;
            }
        }
        cell.ivars().selected.set(segment);
    }
    cell.redraw();
}

/// Each segment's natural width, measured once per change to the cell.
fn natural_widths(cell: &NSSegmentedCellImpl) -> Rc<[f64]> {
    let base = cell_imp(cell.as_cell());
    let generation = super::cell::generation(base);
    if let Some((at, widths)) = &*cell.ivars().natural.borrow()
        && *at == generation
    {
        return widths.clone();
    }
    let pad = metrics::SEGMENT_PADDING[base.control_size_index()];
    let font = segment_font(cell);
    // The widths set and the labels, taken out of the borrow to measure.
    let segments: Vec<(f64, Option<Retained<NSString>>)> =
        cell.ivars().segments.borrow().iter().map(|s| (s.width, s.label.clone())).collect();
    let widths: Rc<[f64]> = segments
        .iter()
        .map(|(width, label)| {
            if *width > 0.0 {
                return *width;
            }
            match label {
                Some(l) if l.length() > 0 => label_styled(cell, l, &font, [0.0; 4]).size(None).width.ceil() + pad,
                _ => metrics::SEGMENT_EMPTY,
            }
        })
        .collect();
    cell.ivars().natural.replace(Some((generation, widths.clone())));
    widths
}

fn natural_size(cell: &NSSegmentedCellImpl) -> NSSize {
    let widths = natural_widths(cell);
    if widths.is_empty() {
        return NSSize::ZERO;
    }
    let total: f64 = widths.iter().sum::<f64>() + metrics::SEGMENT_DIVIDER * (widths.len() - 1) as f64;
    NSSize::new(total, metrics::SEGMENT_HEIGHT[cell_imp(cell.as_cell()).control_size_index()])
}

/// The font labels are measured and drawn in: the one set, else the
/// 13-point system font.
fn segment_font(cell: &NSSegmentedCellImpl) -> Retained<NSFont> {
    cell_imp(cell.as_cell()).font_set().unwrap_or_else(|| NSFont::systemFontOfSize(13.0))
}

fn label_styled(cell: &NSSegmentedCellImpl, label: &NSString, font: &NSFont, color: crate::protocol::Color) -> Styled {
    let mut a = attrs(cell_imp(cell.as_cell()), font, color);
    a.paragraph.alignment = crate::text::layout::Align::Center;
    a.paragraph.line_break = crate::text::layout::LineBreak::TruncateTail;
    Styled::plain(label.to_string(), a)
}

/// Each segment's rect in `bounds`, sharing any extra width as the
/// distribution says.
fn segment_rects(cell: &NSSegmentedCellImpl, bounds: NSRect) -> Vec<NSRect> {
    let natural = natural_widths(cell);
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    let dividers = metrics::SEGMENT_DIVIDER * (n - 1) as f64;
    let available = (bounds.size.width - dividers).max(0.0);
    let total: f64 = natural.iter().sum();
    let fixed: Vec<bool> = cell.ivars().segments.borrow().iter().map(|s| s.width > 0.0).collect();
    let widths: Vec<f64> = match cell.ivars().distribution.get() {
        NSSegmentDistribution::Fit => natural.to_vec(),
        NSSegmentDistribution::FillEqually => vec![available / n as f64; n],
        NSSegmentDistribution::FillProportionally if total > 0.0 => {
            natural.iter().map(|w| w * available / total).collect()
        }
        _ => {
            // Fill: extra room goes equally to segments without a set width.
            let flexible = fixed.iter().filter(|f| !**f).count();
            let extra = available - total;
            natural
                .iter()
                .zip(&fixed)
                .map(|(w, f)| if !f && flexible > 0 && extra > 0.0 { w + extra / flexible as f64 } else { *w })
                .collect()
        }
    };
    let mut x = bounds.origin.x;
    widths
        .iter()
        .map(|w| {
            let r = NSRect::new(NSPoint::new(x, bounds.origin.y), NSSize::new(*w, bounds.size.height));
            x += w + metrics::SEGMENT_DIVIDER;
            r
        })
        .collect()
}

/// The segment at `p` (view coordinates) of the segments laid out in
/// `rects`, if any.
fn segment_at(rects: &[NSRect], p: NSPoint, flipped: bool) -> Option<usize> {
    rects.iter().position(|r| {
        let wide = NSRect::new(r.origin, NSSize::new(r.size.width + metrics::SEGMENT_DIVIDER, r.size.height));
        track::mouse_in_rect(p, wide, flipped)
    })
}

/// Choose `segment` as a click does, per the tracking mode, and send the
/// action.
pub(crate) fn choose(cell: &NSSegmentedCellImpl, segment: usize, view: &NSView) {
    let cell_ref = cell.as_cell();
    match cell.ivars().tracking.get() {
        NSSegmentSwitchTracking::SelectAny => {
            let on = !cell.read(segment as isize, |s| s.selected);
            set_selected(cell, on, segment as isize);
            if !on {
                cell.ivars().selected.set(segment as isize);
            }
            track::send_cell_action(cell_ref, view);
        }
        NSSegmentSwitchTracking::Momentary | NSSegmentSwitchTracking::MomentaryAccelerator => {
            select_segment(cell, segment as isize);
            track::send_cell_action(cell_ref, view);
            select_segment(cell, -1);
        }
        _ => {
            select_segment(cell, segment as isize);
            track::send_cell_action(cell_ref, view);
        }
    }
    cell.ivars().key_segment.set(segment);
}

/// A press: highlight the segment under the pointer while it stays there;
/// choose it on a release there.
fn track_segments(control: &NSControl, cell: &NSSegmentedCellImpl, event: &NSEvent) {
    let view: &NSView = control;
    let mtm = MainThreadMarker::from(control);
    let bounds = view.bounds();
    let flipped = view.isFlipped();
    let start = control::event_point(view, event);
    // Laid out once: the bounds don't change while the mouse is down.
    let rects = segment_rects(cell, bounds);
    // A press between segments or on a disabled one takes the click and
    // chooses nothing.
    let segment = segment_at(&rects, start, flipped).filter(|&i| cell.read(i as isize, |s| s.enabled));
    let Some(segment) = segment else {
        while let Some(next) = track::next_event(mtm, NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged, None) {
            if next.r#type() == NSEventType::LeftMouseUp {
                break;
            }
        }
        return;
    };
    cell.ivars().pressed.set(Some(segment));
    cell.redraw();
    loop {
        let Some(next) = track::next_event(mtm, NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged, None) else {
            break;
        };
        let at = control::event_point(view, &next);
        let over = segment_at(&rects, at, flipped) == Some(segment);
        if next.r#type() == NSEventType::LeftMouseUp {
            cell.ivars().pressed.set(None);
            if over {
                choose(cell, segment, view);
            }
            break;
        }
        let pressed = over.then_some(segment);
        if cell.ivars().pressed.replace(pressed) != pressed {
            cell.redraw();
        }
    }
    cell.ivars().pressed.set(None);
    cell.redraw();
}

fn draw(cell: &NSSegmentedCellImpl, frame: NSRect, view: &NSView) {
    if !theme::paint::recording() {
        return;
    }
    let p = theme::palette();
    let base = cell_imp(cell.as_cell());
    let disabled = !base.has(Flags::ENABLED);
    let rects = segment_rects(cell, frame);
    // What drawing needs of each segment, out of the borrow: drawing calls
    // drawSegment:inFrame:withView:, which subclasses override.
    let segments: Vec<(bool, bool)> = cell.ivars().segments.borrow().iter().map(|s| (s.selected, s.enabled)).collect();
    let tint = cell.ivars().bezel_color.borrow().clone();
    let tint = tint.as_deref().map(theme::color_of);
    let pressed = cell.ivars().pressed.get();
    let separated = cell.ivars().style.get() == NSSegmentStyle::Separated;
    let height = metrics::SEGMENT_HEIGHT[base.control_size_index()].min(frame.size.height);
    let y = frame.origin.y + ((frame.size.height - height) / 2.0).floor();
    let band = |r: &NSRect| NSRect::new(NSPoint::new(r.origin.x, y), NSSize::new(r.size.width, height));
    if !separated && let (Some(first), Some(last)) = (rects.first(), rects.last()) {
        let whole = NSRect::new(
            NSPoint::new(first.origin.x, y),
            NSSize::new(last.origin.x + last.size.width - first.origin.x, height),
        );
        parts::segment_trough(p, whole, parts::State { disabled, ..Default::default() });
    }
    let n = rects.len();
    for (i, (r, &(selected, enabled))) in rects.iter().zip(&segments).enumerate() {
        let state = parts::State { disabled: disabled || !enabled, pressed: pressed == Some(i), ..Default::default() };
        let r = band(r);
        if separated {
            parts::separated_segment(p, r, selected, tint, state);
        } else {
            let radius = [
                if i == 0 { parts::RADIUS } else { 0.0 },
                if i + 1 == n { parts::RADIUS } else { 0.0 },
                if i + 1 == n { parts::RADIUS } else { 0.0 },
                if i == 0 { parts::RADIUS } else { 0.0 },
            ];
            parts::segment(p, r, radius, selected, tint, state);
            // A divider between two segments neither of which is selected.
            if i + 1 < n && !selected && !segments[i + 1].0 {
                let x = r.origin.x + r.size.width;
                let line = NSRect::new(
                    NSPoint::new(x, r.origin.y + 5.0),
                    NSSize::new(metrics::SEGMENT_DIVIDER, (r.size.height - 10.0).max(0.0)),
                );
                theme::paint::fill_rect(line, p.separator);
            }
        }
        // SAFETY: drawSegment:inFrame:withView: takes an index, a rect and
        // a view; subclasses override it.
        let _: () = unsafe { msg_send![cell, drawSegment: i as isize, inFrame: r, withView: view] };
    }
}

fn draw_label(cell: &NSSegmentedCellImpl, segment: usize, frame: NSRect, _view: &NSView) {
    let segments = cell.ivars().segments.borrow();
    let Some(s) = segments.get(segment) else { return };
    let Some(label) = s.label.clone() else { return };
    drop(segments);
    let p = theme::palette();
    let base = cell_imp(cell.as_cell());
    let selected = cell.read(segment as isize, |s| s.selected);
    let enabled = base.has(Flags::ENABLED) && cell.read(segment as isize, |s| s.enabled);
    let mut color = if selected { p.accent_text_on } else { p.label };
    if !enabled {
        color = theme::palette::dimmed(color);
    }
    let font = segment_font(cell);
    let styled = label_styled(cell, &label, &font, color);
    let h = styled.size(None).height;
    let r = NSRect::new(
        NSPoint::new(frame.origin.x + 4.0, frame.origin.y + ((frame.size.height - h) / 2.0).floor()),
        NSSize::new((frame.size.width - 8.0).max(0.0), h),
    );
    styled.draw(r);
}

// NSSegmentedControl

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSegmentedControl"]
    pub(crate) struct NSSegmentedControlImpl;

    impl NSSegmentedControlImpl {
        #[unsafe(method_id(segmentedControlWithLabels:trackingMode:target:action:))]
        fn with_labels(labels: &AnyObject, mode: NSSegmentSwitchTracking, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSSegmentedControl> {
            let items = crate::font::array_items(labels);
            let control = factory(items.len(), mode, target, action);
            for (i, label) in items.iter().enumerate() {
                if let Some(label) = label.downcast_ref::<NSString>() {
                    control.setLabel_forSegment(label, i as isize);
                }
            }
            control.sizeToFit();
            control
        }

        #[unsafe(method_id(segmentedControlWithImages:trackingMode:target:action:))]
        fn with_images(images: &AnyObject, mode: NSSegmentSwitchTracking, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSSegmentedControl> {
            let items = crate::font::array_items(images);
            let control = factory(items.len(), mode, target, action);
            for (i, image) in items.iter().enumerate() {
                // SAFETY: setImage:forSegment: takes an image and an index.
                let _: () = unsafe { msg_send![&*control, setImage: &**image, forSegment: i as isize] };
            }
            control.sizeToFit();
            control
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let view: &NSView = self.control();
            if !super::focus::control_key_down(view, event) {
                // SAFETY: NSResponder's keyDown: passes the key on.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(segmentCount))]
        fn segment_count(&self) -> isize {
            self.cell_or(0, |c| c.segmentCount())
        }

        #[unsafe(method(setSegmentCount:))]
        fn set_segment_count(&self, count: isize) {
            self.with(|c| c.setSegmentCount(count));
        }

        #[unsafe(method(selectedSegment))]
        fn selected_segment(&self) -> isize {
            self.cell_or(-1, |c| c.selectedSegment())
        }

        #[unsafe(method(setSelectedSegment:))]
        fn set_selected_segment(&self, segment: isize) {
            self.with(|c| c.setSelectedSegment(segment));
        }

        #[unsafe(method(indexOfSelectedItem))]
        fn index_of_selected_item(&self) -> isize {
            self.cell_or(-1, |c| c.selectedSegment())
        }

        #[unsafe(method(doubleValueForSelectedSegment))]
        fn double_value_for_selected_segment(&self) -> f64 {
            0.0
        }

        #[unsafe(method(selectSegmentWithTag:))]
        fn select_segment_with_tag(&self, tag: isize) -> bool {
            self.cell_or(false, |c| c.selectSegmentWithTag(tag))
        }

        #[unsafe(method(setWidth:forSegment:))]
        fn set_width(&self, width: f64, segment: isize) {
            self.with(|c| c.setWidth_forSegment(width, segment));
        }

        #[unsafe(method(widthForSegment:))]
        fn width_for_segment(&self, segment: isize) -> f64 {
            self.cell_or(0.0, |c| c.widthForSegment(segment))
        }

        #[unsafe(method(setImage:forSegment:))]
        fn set_image(&self, image: Option<&AnyObject>, segment: isize) {
            // SAFETY: the cell's method takes an image or nil.
            self.with(|c| unsafe { msg_send![c, setImage: image, forSegment: segment] });
        }

        #[unsafe(method_id(imageForSegment:))]
        fn image_for_segment(&self, segment: isize) -> Option<Retained<AnyObject>> {
            // SAFETY: the cell's method returns an image or nil.
            self.cell_or(None, |c| unsafe { msg_send![c, imageForSegment: segment] })
        }

        #[unsafe(method(setImageScaling:forSegment:))]
        fn set_image_scaling(&self, scaling: NSImageScaling, segment: isize) {
            self.with(|c| c.setImageScaling_forSegment(scaling, segment));
        }

        #[unsafe(method(imageScalingForSegment:))]
        fn image_scaling_for_segment(&self, segment: isize) -> NSImageScaling {
            self.cell_or(NSImageScaling::ScaleProportionallyDown, |c| c.imageScalingForSegment(segment))
        }

        #[unsafe(method(setLabel:forSegment:))]
        fn set_label(&self, label: &NSString, segment: isize) {
            self.with(|c| c.setLabel_forSegment(label, segment));
        }

        #[unsafe(method_id(labelForSegment:))]
        fn label_for_segment(&self, segment: isize) -> Option<Retained<NSString>> {
            self.cell_or(None, |c| c.labelForSegment(segment))
        }

        #[unsafe(method(setMenu:forSegment:))]
        fn set_menu(&self, menu: Option<&AnyObject>, segment: isize) {
            // SAFETY: the cell's method takes a menu or nil.
            self.with(|c| unsafe { msg_send![c, setMenu: menu, forSegment: segment] });
        }

        #[unsafe(method_id(menuForSegment:))]
        fn menu_for_segment(&self, segment: isize) -> Option<Retained<AnyObject>> {
            // SAFETY: the cell's method returns a menu or nil.
            self.cell_or(None, |c| unsafe { msg_send![c, menuForSegment: segment] })
        }

        #[unsafe(method(setSelected:forSegment:))]
        fn set_selected(&self, selected: bool, segment: isize) {
            self.with(|c| c.setSelected_forSegment(selected, segment));
        }

        #[unsafe(method(isSelectedForSegment:))]
        fn is_selected_for_segment(&self, segment: isize) -> bool {
            self.cell_or(false, |c| c.isSelectedForSegment(segment))
        }

        #[unsafe(method(setEnabled:forSegment:))]
        fn set_enabled_for_segment(&self, enabled: bool, segment: isize) {
            self.with(|c| c.setEnabled_forSegment(enabled, segment));
        }

        #[unsafe(method(isEnabledForSegment:))]
        fn is_enabled_for_segment(&self, segment: isize) -> bool {
            self.cell_or(false, |c| c.isEnabledForSegment(segment))
        }

        #[unsafe(method(setToolTip:forSegment:))]
        fn set_tool_tip(&self, tip: Option<&NSString>, segment: isize) {
            self.with(|c| c.setToolTip_forSegment(tip, segment));
        }

        #[unsafe(method_id(toolTipForSegment:))]
        fn tool_tip_for_segment(&self, segment: isize) -> Option<Retained<NSString>> {
            self.cell_or(None, |c| c.toolTipForSegment(segment))
        }

        #[unsafe(method(setTag:forSegment:))]
        fn set_tag(&self, tag: isize, segment: isize) {
            self.with(|c| c.setTag_forSegment(tag, segment));
        }

        #[unsafe(method(tagForSegment:))]
        fn tag_for_segment(&self, segment: isize) -> isize {
            self.cell_or(0, |c| c.tagForSegment(segment))
        }

        #[unsafe(method(setShowsMenuIndicator:forSegment:))]
        fn set_shows_menu_indicator(&self, flag: bool, segment: isize) {
            // SAFETY: the cell's method takes a BOOL and an index.
            self.with(|c| unsafe { msg_send![c, setShowsMenuIndicator: flag, forSegment: segment] });
        }

        #[unsafe(method(showsMenuIndicatorForSegment:))]
        fn shows_menu_indicator_for_segment(&self, segment: isize) -> bool {
            // SAFETY: the cell's method takes an index and returns BOOL.
            self.cell_or(false, |c| unsafe { msg_send![c, showsMenuIndicatorForSegment: segment] })
        }

        #[unsafe(method(setAlignment:forSegment:))]
        fn set_alignment_for_segment(&self, alignment: NSTextAlignment, segment: isize) {
            // SAFETY: the cell's method takes an alignment and an index.
            self.with(|c| unsafe { msg_send![c, setAlignment: alignment, forSegment: segment] });
        }

        #[unsafe(method(alignmentForSegment:))]
        fn alignment_for_segment(&self, segment: isize) -> NSTextAlignment {
            // SAFETY: the cell's method takes an index.
            self.cell_or(NSTextAlignment::Center, |c| unsafe { msg_send![c, alignmentForSegment: segment] })
        }

        #[unsafe(method(segmentStyle))]
        fn segment_style(&self) -> NSSegmentStyle {
            self.cell_or(NSSegmentStyle::Automatic, |c| c.segmentStyle())
        }

        #[unsafe(method(setSegmentStyle:))]
        fn set_segment_style(&self, style: NSSegmentStyle) {
            self.with(|c| c.setSegmentStyle(style));
        }

        #[unsafe(method(trackingMode))]
        fn tracking_mode(&self) -> NSSegmentSwitchTracking {
            self.cell_or(NSSegmentSwitchTracking::SelectOne, |c| c.trackingMode())
        }

        #[unsafe(method(setTrackingMode:))]
        fn set_tracking_mode(&self, mode: NSSegmentSwitchTracking) {
            self.with(|c| c.setTrackingMode(mode));
        }

        #[unsafe(method(segmentDistribution))]
        fn segment_distribution(&self) -> NSSegmentDistribution {
            self.imp_cell().map_or(NSSegmentDistribution::Fill, |c| c.ivars().distribution.get())
        }

        #[unsafe(method(setSegmentDistribution:))]
        fn set_segment_distribution(&self, distribution: NSSegmentDistribution) {
            if let Some(cell) = self.control().cell()
                && let Some(c) = segmented_cell(&cell)
            {
                c.ivars().distribution.set(distribution);
                c.redraw();
            }
        }

        #[unsafe(method_id(selectedSegmentBezelColor))]
        fn selected_segment_bezel_color(&self) -> Option<Retained<NSColor>> {
            self.imp_cell().and_then(|c| c.ivars().bezel_color.borrow().clone())
        }

        #[unsafe(method(setSelectedSegmentBezelColor:))]
        fn set_selected_segment_bezel_color(&self, color: Option<&NSColor>) {
            if let Some(cell) = self.control().cell()
                && let Some(c) = segmented_cell(&cell)
            {
                c.ivars().bezel_color.replace(color.map(|c| c.retain()));
                c.redraw();
            }
        }

        #[unsafe(method(isSpringLoaded))]
        fn is_spring_loaded(&self) -> bool {
            self.imp_cell().is_some_and(|c| c.ivars().spring_loaded.get())
        }

        #[unsafe(method(setSpringLoaded:))]
        fn set_spring_loaded(&self, flag: bool) {
            if let Some(cell) = self.control().cell()
                && let Some(c) = segmented_cell(&cell)
            {
                c.ivars().spring_loaded.set(flag);
            }
        }

        #[unsafe(method(borderShape))]
        fn border_shape(&self) -> objc2_app_kit::NSControlBorderShape {
            objc2_app_kit::NSControlBorderShape::Automatic
        }

        #[unsafe(method(setBorderShape:))]
        fn set_border_shape(&self, _shape: objc2_app_kit::NSControlBorderShape) {}

        #[unsafe(method(compressWithPrioritizedCompressionOptions:))]
        fn compress(&self, _options: &AnyObject) {}

        #[unsafe(method(minimumSizeWithPrioritizedCompressionOptions:))]
        fn minimum_size(&self, _options: &AnyObject) -> NSSize {
            // SAFETY: intrinsicContentSize takes nothing and returns a size.
            unsafe { msg_send![self, intrinsicContentSize] }
        }

        #[unsafe(method_id(activeCompressionOptions))]
        fn active_compression_options(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            control::cached_intrinsic(self.control(), || self.control().cell().map_or(NSSize::ZERO, |c| c.cellSize()))
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let control = self.control();
            if !control.isEnabled() {
                return;
            }
            if let Some(cell) = control.cell()
                && let Some(c) = segmented_cell(&cell)
            {
                track_segments(control, c, event);
            }
        }
    }
);

impl NSSegmentedControlImpl {
    fn control(&self) -> &NSControl {
        // SAFETY: NSSegmentedControl is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn cell(&self) -> Option<Retained<NSSegmentedCell>> {
        let cell = self.control().cell()?;
        segmented_cell(&cell)?;
        // SAFETY: checked just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn imp_cell(&self) -> Option<SegmentedRef> {
        self.control().cell().filter(|c| segmented_cell(c).is_some()).map(SegmentedRef)
    }

    fn with(&self, f: impl FnOnce(&NSSegmentedCell)) {
        if let Some(c) = self.cell() {
            f(&c);
        }
    }

    fn cell_or<R>(&self, default: R, f: impl FnOnce(&NSSegmentedCell) -> R) -> R {
        self.cell().map_or(default, |c| f(&c))
    }
}

/// A retained segmented cell, reached as the implementation.
struct SegmentedRef(Retained<NSCell>);

impl std::ops::Deref for SegmentedRef {
    type Target = NSSegmentedCellImpl;

    fn deref(&self) -> &NSSegmentedCellImpl {
        segmented_cell(&self.0).expect("a segmented cell")
    }
}

/// The factories' common part: `count` segments tagged by index.
fn factory(
    count: usize,
    mode: NSSegmentSwitchTracking,
    target: Option<&AnyObject>,
    action: Option<Sel>,
) -> Retained<NSSegmentedControl> {
    let mtm = MainThreadMarker::new().expect("sidestep: AppKit's controls belong to the main thread");
    let control = NSSegmentedControl::initWithFrame(NSSegmentedControl::alloc(mtm), NSRect::ZERO);
    control.setSegmentCount(count as isize);
    for i in 0..count {
        control.setTag_forSegment(i as isize, i as isize);
    }
    control.setTrackingMode(mode);
    // SAFETY: the control keeps the target weakly; any selector may be an
    // action.
    unsafe {
        control.setTarget(target);
        control.setAction(action);
    }
    control
}

/// The segmented control's keyboard: the arrows move to the next or
/// previous enabled segment and choose it. True if the key was used.
pub(crate) fn arrow(control: &NSControl, forward: bool) -> bool {
    let Some(cell) = control.cell() else { return false };
    let Some(c) = segmented_cell(&cell) else { return false };
    let n = c.ivars().segments.borrow().len();
    if n == 0 {
        return false;
    }
    let current = match c.ivars().selected.get() {
        s if s >= 0 => s as usize,
        _ => c.ivars().key_segment.get().min(n - 1),
    };
    let mut i = current;
    for _ in 0..n {
        i = if forward { (i + 1) % n } else { (i + n - 1) % n };
        if c.read(i as isize, |s| s.enabled) {
            let view: &NSView = control;
            choose(c, i, view);
            return true;
        }
    }
    false
}
