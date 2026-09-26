//! `NSScroller`: a control with no cell, whose value (0 to 1) is how far
//! its knob is along its slot and whose knob proportion is how much of the
//! document shows. Its geometry is AppKit's, measured by
//! `conformance/tests/appkit_scroll.rs`:
//!
//! - widths by control size and style (`+scrollerWidthForControlSize:
//!   scrollerStyle:`): 17 points regular and large, 13 small and mini for
//!   legacy scrollers, 15 for small and mini overlay ones;
//! - the slot is inset 3 points from each end and from the outer edge,
//!   11 points thick for regular and large legacy scrollers and 7 for
//!   small and mini ones, 6 and 4 for overlay ones; the knob is at least
//!   20 points long (16 small, legacy) or 26 (overlay), and the pages lie
//!   on either side of it;
//! - an enabled scroller's parts are usable only while its slot is at
//!   least as long as the shortest knob; `testPart:` takes a point in
//!   window coordinates and answers only inside the slot of a scroller
//!   whose parts are usable;
//! - a scroller is vertical unless it was made wider than it is tall (a
//!   scroll view sets it either way);
//! - a new scroller is disabled and legacy; values and proportions are
//!   kept between 0 and 1, and NaN makes either 1.
//!
//! **Drawing** goes through `drawKnobSlotInRect:highlight:` and
//! `drawKnob`, which subclasses may override. Legacy scrollers draw a
//! track and a knob; overlay scrollers draw a thin knob only while shown,
//! a wider knob and a track while the pointer is over them.
//!
//! **Tracking** runs from the mouse events themselves, with no nested
//! loop: a press on the knob drags it (sending the action as the value
//! changes), a press in the slot pages toward the pointer and goes on
//! paging while held, on a timer; `hitPart` says which, until the button
//! comes up. A press that tracks is a live scroll of the scroll view the
//! scroller is in (see `scroll`).
//!
//! **Overlay scrollers** show when their scroll view scrolls or flashes
//! them and fade out about a second after the last change, on one timer
//! every overlay scroller shares, which runs only while one is fading;
//! the pointer over one keeps it shown, widened. One that isn't showing
//! lets clicks through to the document under it.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSControl, NSControlSize, NSEvent, NSResponder, NSScroller, NSScrollerKnobStyle, NSScrollerPart, NSScrollerStyle,
    NSTrackingArea, NSTrackingAreaOptions, NSUsableScrollerParts, NSView, NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSTimer};

use crate::protocol::Color;
use crate::theme;

/// Inset of the slot from the scroller's ends and its outer edge.
const INSET: f64 = 3.0;
/// How long an overlay scroller stays after the last change, and how long
/// it takes to fade.
const LINGER: Duration = Duration::from_millis(1000);
const FADE: Duration = Duration::from_millis(250);
/// Paging while the button is held: the first repeat, then the rest.
const REPEAT_DELAY: Duration = Duration::from_millis(350);
const REPEAT_EVERY: Duration = Duration::from_millis(60);

/// `+scrollerWidthForControlSize:scrollerStyle:`, as macOS answers.
fn width_for(size: NSControlSize, style: NSScrollerStyle) -> f64 {
    let small = size == NSControlSize::Small || size == NSControlSize::Mini;
    match (style == NSScrollerStyle::Overlay, small) {
        (_, false) => 17.0,
        (false, true) => 13.0,
        (true, true) => 15.0,
    }
}

/// The slot's thickness for a style and size.
fn thickness(style: NSScrollerStyle, size: NSControlSize) -> f64 {
    let small = size == NSControlSize::Small || size == NSControlSize::Mini;
    match (style == NSScrollerStyle::Overlay, small) {
        (true, false) => 6.0,
        (true, true) => 4.0,
        (false, false) => 11.0,
        (false, true) => 7.0,
    }
}

/// The shortest knob for a style and size.
fn shortest_knob(style: NSScrollerStyle, size: NSControlSize) -> f64 {
    let small = size == NSControlSize::Small || size == NSControlSize::Mini;
    match (style == NSScrollerStyle::Overlay, small) {
        (true, _) => 26.0,
        (false, false) => 20.0,
        (false, true) => 16.0,
    }
}

/// `SIDESTEP_SCROLLER_STYLE=legacy` asks for legacy scrollers everywhere;
/// overlay ones otherwise.
fn preferred() -> NSScrollerStyle {
    static STYLE: std::sync::OnceLock<NSScrollerStyle> = std::sync::OnceLock::new();
    *STYLE.get_or_init(|| match std::env::var("SIDESTEP_SCROLLER_STYLE") {
        Ok(v) if v.eq_ignore_ascii_case("legacy") => NSScrollerStyle::Legacy,
        _ => NSScrollerStyle::Overlay,
    })
}

/// A press being tracked.
#[derive(Clone, Copy)]
enum Press {
    /// Dragging the knob, held this far (points) along it from its start.
    Knob(f64),
    /// Paging toward the pointer, `along` the axis, next at `due`.
    Page { along: f64, due: Instant },
}

/// An overlay scroller's showing: how shown (0 to 1), whether the pointer
/// is over it, and when it starts to fade.
#[derive(Clone, Copy)]
struct Showing {
    alpha: f64,
    hovered: bool,
    fade_at: Option<Instant>,
}

pub(crate) struct ScrollerIvars {
    value: Cell<f64>,
    proportion: Cell<f64>,
    style: Cell<NSScrollerStyle>,
    knob_style: Cell<NSScrollerKnobStyle>,
    size: Cell<NSControlSize>,
    horizontal: Cell<bool>,
    hit: Cell<NSScrollerPart>,
    press: Cell<Option<Press>>,
    showing: Cell<Showing>,
    area: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSScroller"]
    #[ivars = ScrollerIvars]
    pub(crate) struct NSScrollerImpl;

    impl NSScrollerImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ScrollerIvars {
                value: Cell::new(0.0),
                proportion: Cell::new(0.0),
                style: Cell::new(NSScrollerStyle::Legacy),
                knob_style: Cell::new(NSScrollerKnobStyle::Default),
                size: Cell::new(NSControlSize::Regular),
                horizontal: Cell::new(frame.size.width > frame.size.height),
                hit: Cell::new(NSScrollerPart::NoPart),
                press: Cell::new(None),
                showing: Cell::new(Showing { alpha: 0.0, hovered: false, fade_at: None }),
                area: RefCell::new(None),
            });
            // SAFETY: NSControl's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            // A new scroller has nothing to scroll.
            this.control().setEnabled(false);
            // As macOS answers.
            crate::view_layout::set_redraw_policy(
                crate::views::imp(this.view()),
                NSViewLayerContentsRedrawPolicy::Never,
            );
            this
        }

        #[unsafe(method(scrollerWidthForControlSize:scrollerStyle:))]
        fn scroller_width_for_control_size_scroller_style(size: NSControlSize, style: NSScrollerStyle) -> f64 {
            width_for(size, style)
        }

        #[unsafe(method(scrollerWidthForControlSize:))]
        fn scroller_width_for_control_size(size: NSControlSize) -> f64 {
            width_for(size, NSScrollerStyle::Legacy)
        }

        #[unsafe(method(scrollerWidth))]
        fn scroller_width() -> f64 {
            width_for(NSControlSize::Regular, NSScrollerStyle::Legacy)
        }

        #[unsafe(method(preferredScrollerStyle))]
        fn preferred_scroller_style() -> NSScrollerStyle {
            preferred()
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// An overlay scroller that isn't showing lets clicks through to
        /// what's under it.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let showing = self.ivars().showing.get();
            if self.overlay() && showing.alpha <= 0.0 && !showing.hovered {
                None
            } else {
                // SAFETY: NSView's hitTest: takes a point in the superview's
                // coordinates and returns a view or nil.
                unsafe { msg_send![super(self), hitTest: point] }
            }
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(scrollerStyle))]
        fn scroller_style(&self) -> NSScrollerStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setScrollerStyle:))]
        fn set_scroller_style(&self, style: NSScrollerStyle) {
            if self.ivars().style.replace(style) != style {
                self.view().setNeedsDisplay(true);
                self.view().updateTrackingAreas();
            }
        }

        #[unsafe(method(knobStyle))]
        fn knob_style(&self) -> NSScrollerKnobStyle {
            self.ivars().knob_style.get()
        }

        #[unsafe(method(setKnobStyle:))]
        fn set_knob_style(&self, style: NSScrollerKnobStyle) {
            if self.ivars().knob_style.replace(style) != style {
                self.view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method(controlSize))]
        fn control_size(&self) -> NSControlSize {
            self.ivars().size.get()
        }

        #[unsafe(method(setControlSize:))]
        fn set_control_size(&self, size: NSControlSize) {
            if self.ivars().size.replace(size) != size {
                self.view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method(knobProportion))]
        fn knob_proportion(&self) -> f64 {
            self.ivars().proportion.get()
        }

        #[unsafe(method(setKnobProportion:))]
        fn set_knob_proportion(&self, proportion: f64) {
            let proportion = if proportion.is_nan() { 1.0 } else { proportion.clamp(0.0, 1.0) };
            let before = parts(self);
            if self.ivars().proportion.replace(proportion) != proportion {
                knob_moved(self, &before);
            }
        }

        /// The value is the scroller's own, so a new one redraws only where
        /// the knob was and is.
        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.ivars().value.get()
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            let value = if value.is_nan() { 1.0 } else { value.clamp(0.0, 1.0) };
            let before = parts(self);
            self.ivars().value.set(value);
            knob_moved(self, &before);
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            self.ivars().value.get() as f32
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            self.scroller().setDoubleValue(value as f64);
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            self.ivars().value.get() as i32
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            self.ivars().value.get() as isize
        }

        #[unsafe(method(setFloatValue:knobProportion:))]
        fn set_float_value_knob_proportion(&self, value: f32, proportion: f64) {
            let scroller = self.scroller();
            scroller.setDoubleValue(value as f64);
            scroller.setKnobProportion(proportion);
        }

        #[unsafe(method(rectForPart:))]
        fn rect_for_part(&self, part: NSScrollerPart) -> NSRect {
            rect_for(self, part)
        }

        #[unsafe(method(testPart:))]
        fn test_part(&self, point: NSPoint) -> NSScrollerPart {
            test_part(self, self.view().convertPoint_fromView(point, None))
        }

        #[unsafe(method(usableParts))]
        fn usable_parts(&self) -> NSUsableScrollerParts {
            if usable(self) {
                NSUsableScrollerParts::AllScrollerParts
            } else {
                NSUsableScrollerParts::NoScrollerParts
            }
        }

        #[unsafe(method(checkSpaceForParts))]
        fn check_space_for_parts(&self) {}

        #[unsafe(method(hitPart))]
        fn hit_part(&self) -> NSScrollerPart {
            self.ivars().hit.get()
        }

        #[unsafe(method(highlight:))]
        fn highlight(&self, _flag: bool) {}

        #[unsafe(method(drawParts))]
        fn draw_parts(&self) {}

        /// Tracks from the event that started a knob drag; the rest comes
        /// with the mouse events (no nested loop).
        #[unsafe(method(trackKnob:))]
        fn track_knob(&self, event: &NSEvent) {
            if self.ivars().press.get().is_none() {
                crate::scroll::scroller_tracking(self.view(), true);
            }
            start_knob(self, event);
        }

        #[unsafe(method(trackScrollButtons:))]
        fn track_scroll_buttons(&self, _event: &NSEvent) {}

        #[unsafe(method(drawKnobSlotInRect:highlight:))]
        fn draw_knob_slot_in_rect_highlight(&self, slot: NSRect, _highlight: bool) {
            draw_slot(self, slot);
        }

        #[unsafe(method(drawKnob))]
        fn draw_knob(&self) {
            draw_knob(self);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw(self);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            mouse_down(self, event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if let Some(Press::Knob(grab)) = self.ivars().press.get() {
                drag_knob(self, event, grab);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let tracked = self.ivars().press.take().is_some();
            self.ivars().hit.set(NSScrollerPart::NoPart);
            linger(self);
            self.view().setNeedsDisplay(true);
            if tracked {
                crate::scroll::scroller_tracking(self.view(), false);
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            hover(self, true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            hover(self, false);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            update_area(self);
        }
    }

    unsafe impl NSObjectProtocol for NSScrollerImpl {}
);

impl NSScrollerImpl {
    fn view(&self) -> &NSView {
        // SAFETY: NSScroller is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn control(&self) -> &NSControl {
        // SAFETY: NSScroller is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn scroller(&self) -> &NSScroller {
        // SAFETY: this is an NSScroller.
        unsafe { &*(self as *const Self).cast::<NSScroller>() }
    }

    fn value(&self) -> f64 {
        self.ivars().value.get()
    }

    fn overlay(&self) -> bool {
        self.ivars().style.get() == NSScrollerStyle::Overlay
    }
}

/// `scroller` as its implementation.
fn imp(scroller: &NSScroller) -> &NSScrollerImpl {
    // SAFETY: NSScroller's instances, subclasses' too, have NSScrollerImpl's
    // layout.
    unsafe { &*(scroller as *const NSScroller).cast::<NSScrollerImpl>() }
}

/// `+isCompatibleWithOverlayScrollers`: whether the receiver draws as
/// NSScroller does, which overlay scrollers need.
extern "C-unwind" fn compatible_imp(receiver: &AnyClass, _cmd: Sel) -> Bool {
    let base = NSScrollerImpl::class();
    let same = |sel: Sel| {
        let a = receiver.instance_method(sel).map(|m| m.implementation());
        let b = base.instance_method(sel).map(|m| m.implementation());
        a.zip(b).is_some_and(|(a, b)| std::ptr::fn_addr_eq(a, b))
    };
    Bool::new(same(sel!(drawRect:)) && same(sel!(drawKnob)) && same(sel!(drawKnobSlotInRect:highlight:)))
}

/// Give NSScroller's metaclass `+isCompatibleWithOverlayScrollers`, which
/// needs its receiver.
pub(crate) fn install_class_methods(class: &AnyClass) {
    let meta = class.metaclass();
    // SAFETY: the function takes the receiver and selector and returns a
    // BOOL, as the encoding says; methods are called with the C ABI objc2
    // uses for these types.
    unsafe {
        let imp: unsafe extern "C-unwind" fn() =
            std::mem::transmute(compatible_imp as extern "C-unwind" fn(&AnyClass, Sel) -> Bool);
        objc2::ffi::class_addMethod(
            (meta as *const AnyClass).cast_mut(),
            sel!(isCompatibleWithOverlayScrollers),
            imp,
            c"C@:".as_ptr(),
        );
    }
}

/// Make `scroller` vertical or horizontal, as its scroll view uses it.
pub(crate) fn set_vertical(scroller: &NSScroller, vertical: bool) {
    imp(scroller).ivars().horizontal.set(!vertical);
}

// Geometry.

/// The slot, the length the knob moves along, and the knob's start and
/// length, all along the axis: (slot start, slot length, knob start, knob
/// length); the slot's thickness and where it starts across.
struct Parts {
    start: f64,
    length: f64,
    knob: f64,
    knob_length: f64,
    across: f64,
    thick: f64,
}

fn parts(s: &NSScrollerImpl) -> Parts {
    let b = s.view().bounds();
    let horizontal = s.ivars().horizontal.get();
    let (along, across_size) = if horizontal { (b.size.width, b.size.height) } else { (b.size.height, b.size.width) };
    let (along_origin, across_origin) = if horizontal { (b.origin.x, b.origin.y) } else { (b.origin.y, b.origin.x) };
    let style = s.ivars().style.get();
    let thick = thickness(style, s.ivars().size.get());
    let start = along_origin + INSET;
    let length = along - 2.0 * INSET;
    let proportion = s.ivars().proportion.get();
    let min = shortest_knob(style, s.ivars().size.get());
    let knob_length = if proportion >= 1.0 { length } else { (length * proportion).max(min) };
    let knob = start + s.value() * (length - knob_length);
    Parts { start, length, knob, knob_length, across: across_origin + across_size - INSET - thick, thick }
}

/// The knob moved from where `before` had it: redraw the stretch of the
/// scroller it left and the one it reached, across the whole scroller
/// (a hovered knob is wider than the slot).
fn knob_moved(s: &NSScrollerImpl, before: &Parts) {
    let after = parts(s);
    if before.knob == after.knob && before.knob_length == after.knob_length {
        return;
    }
    let from = before.knob.min(after.knob);
    let to = (before.knob + before.knob_length).max(after.knob + after.knob_length);
    let b = s.view().bounds();
    let band = if s.ivars().horizontal.get() {
        NSRect::new(NSPoint::new(from, b.origin.y), NSSize::new(to - from, b.size.height))
    } else {
        NSRect::new(NSPoint::new(b.origin.x, from), NSSize::new(b.size.width, to - from))
    };
    s.view().setNeedsDisplayInRect(band);
}

/// A span along the axis as a rectangle across the slot.
fn span(s: &NSScrollerImpl, p: &Parts, from: f64, length: f64) -> NSRect {
    if s.ivars().horizontal.get() {
        NSRect::new(NSPoint::new(from, p.across), NSSize::new(length, p.thick))
    } else {
        NSRect::new(NSPoint::new(p.across, from), NSSize::new(p.thick, length))
    }
}

#[allow(deprecated)]
fn rect_for(s: &NSScrollerImpl, part: NSScrollerPart) -> NSRect {
    let p = parts(s);
    match part {
        NSScrollerPart::KnobSlot => span(s, &p, p.start, p.length),
        NSScrollerPart::Knob => span(s, &p, p.knob, p.knob_length),
        NSScrollerPart::DecrementPage => span(s, &p, p.start, p.knob - p.start),
        NSScrollerPart::IncrementPage => {
            let end = p.knob + p.knob_length;
            span(s, &p, end, p.start + p.length - end)
        }
        _ => NSRect::ZERO,
    }
}

fn inside(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x && p.y >= r.origin.y && p.x < r.origin.x + r.size.width && p.y < r.origin.y + r.size.height
}

/// Whether the scroller's parts can be used: it is enabled, and its slot
/// has room for the shortest knob.
fn usable(s: &NSScrollerImpl) -> bool {
    let p = parts(s);
    s.control().isEnabled() && p.length >= shortest_knob(s.ivars().style.get(), s.ivars().size.get())
}

/// The part at `point` (the scroller's coordinates).
fn test_part(s: &NSScrollerImpl, point: NSPoint) -> NSScrollerPart {
    if !usable(s) || !inside(rect_for(s, NSScrollerPart::KnobSlot), point) {
        return NSScrollerPart::NoPart;
    }
    let scroller = s.scroller();
    for part in [NSScrollerPart::Knob, NSScrollerPart::DecrementPage, NSScrollerPart::IncrementPage] {
        if inside(scroller.rectForPart(part), point) {
            return part;
        }
    }
    NSScrollerPart::NoPart
}

// Tracking.

/// A point's place along the axis.
fn along(s: &NSScrollerImpl, p: NSPoint) -> f64 {
    if s.ivars().horizontal.get() { p.x } else { p.y }
}

fn event_point(s: &NSScrollerImpl, event: &NSEvent) -> NSPoint {
    s.view().convertPoint_fromView(event.locationInWindow(), None)
}

fn send(s: &NSScrollerImpl) {
    let control = s.control();
    crate::controls::control::send_action(s.view(), control.action(), control.target().as_deref());
}

fn mouse_down(s: &NSScrollerImpl, event: &NSEvent) {
    let scroller = s.scroller();
    let part = scroller.testPart(event.locationInWindow());
    s.ivars().hit.set(part);
    show(s);
    match part {
        NSScrollerPart::Knob => {
            crate::scroll::scroller_tracking(s.view(), true);
            start_knob(s, event);
        }
        NSScrollerPart::DecrementPage | NSScrollerPart::IncrementPage => {
            let at = along(s, event_point(s, event));
            s.ivars().press.set(Some(Press::Page { along: at, due: Instant::now() + REPEAT_DELAY }));
            crate::scroll::scroller_tracking(s.view(), true);
            send(s);
            clock::wake(s, Instant::now() + REPEAT_DELAY);
        }
        _ => {}
    }
    s.view().setNeedsDisplay(true);
}

fn start_knob(s: &NSScrollerImpl, event: &NSEvent) {
    let p = parts(s);
    s.ivars().hit.set(NSScrollerPart::Knob);
    s.ivars().press.set(Some(Press::Knob(along(s, event_point(s, event)) - p.knob)));
}

/// Follow the pointer with the knob, held `grab` along from its start.
fn drag_knob(s: &NSScrollerImpl, event: &NSEvent, grab: f64) {
    let p = parts(s);
    let travel = p.length - p.knob_length;
    if travel <= 0.0 {
        return;
    }
    let value = ((along(s, event_point(s, event)) - grab - p.start) / travel).clamp(0.0, 1.0);
    if value != s.value() {
        s.scroller().setDoubleValue(value);
        send(s);
    }
}

/// Paging while held: another page toward the pointer, until the knob
/// reaches it.
fn page_again(s: &NSScrollerImpl, now: Instant) -> Option<Instant> {
    let Some(Press::Page { along: at, due }) = s.ivars().press.get() else { return None };
    if now < due {
        return Some(due);
    }
    let part = s.scroller().testPart(s.view().convertPoint_toView(position(s, at), None));
    if part != s.ivars().hit.get() {
        return None;
    }
    send(s);
    let next = now + REPEAT_EVERY;
    s.ivars().press.set(Some(Press::Page { along: at, due: next }));
    Some(next)
}

/// A point on the slot's middle line, `at` along the axis.
fn position(s: &NSScrollerImpl, at: f64) -> NSPoint {
    let p = parts(s);
    let across = p.across + p.thick / 2.0;
    if s.ivars().horizontal.get() { NSPoint::new(at, across) } else { NSPoint::new(across, at) }
}

// Overlay showing and fading.

/// Show an overlay scroller now, and fade it later: the scroll view
/// scrolled or flashes its scrollers.
pub(crate) fn flash(scroller: &NSScroller) {
    let s = imp(scroller);
    if s.overlay() {
        show(s);
        linger(s);
    }
}

fn show(s: &NSScrollerImpl) {
    let mut showing = s.ivars().showing.get();
    if showing.alpha < 1.0 {
        showing.alpha = 1.0;
        s.view().setNeedsDisplay(true);
    }
    showing.fade_at = None;
    s.ivars().showing.set(showing);
}

/// Start the wait before fading, unless something holds it shown.
fn linger(s: &NSScrollerImpl) {
    let mut showing = s.ivars().showing.get();
    if !s.overlay() || showing.hovered || s.ivars().press.get().is_some() {
        return;
    }
    let at = Instant::now() + LINGER;
    showing.fade_at = Some(at);
    s.ivars().showing.set(showing);
    clock::wake(s, at);
}

fn hover(s: &NSScrollerImpl, over: bool) {
    let mut showing = s.ivars().showing.get();
    if showing.hovered == over {
        return;
    }
    showing.hovered = over;
    s.ivars().showing.set(showing);
    if over {
        if s.control().isEnabled() {
            show(s);
        }
    } else {
        linger(s);
    }
    s.view().setNeedsDisplay(true);
}

/// A time came for a scroller: fade it a step, or page again. Returns
/// when it next needs the clock.
fn tick(s: &NSScrollerImpl, now: Instant) -> Option<Instant> {
    let paging = page_again(s, now);
    let mut showing = s.ivars().showing.get();
    let fading = match showing.fade_at {
        Some(at) if now >= at => {
            let done = (now - at).as_secs_f64() / FADE.as_secs_f64();
            showing.alpha = (1.0 - done).max(0.0);
            if showing.alpha <= 0.0 {
                showing.fade_at = None;
            }
            s.ivars().showing.set(showing);
            s.view().setNeedsDisplay(true);
            showing.fade_at.map(|_| now + Duration::from_millis(33))
        }
        other => other,
    };
    match (paging, fading) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The one timer overlay fades and held pages run on.
mod clock {
    use super::*;

    thread_local! {
        static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
        /// Scrollers waiting for a time, and the time.
        static WAITING: RefCell<Vec<(Weak<NSScroller>, Instant)>> = const { RefCell::new(Vec::new()) };
    }

    /// Have `s` ticked at `at`.
    pub(super) fn wake(s: &NSScrollerImpl, at: Instant) {
        WAITING.with(|w| {
            let mut w = w.borrow_mut();
            let me = s.scroller() as *const NSScroller;
            match w.iter_mut().find(|(v, _)| v.load().is_some_and(|v| std::ptr::eq(&*v, me))) {
                Some(entry) => entry.1 = entry.1.min(at),
                None => w.push((Weak::new(s.scroller()), at)),
            }
        });
        arm();
    }

    /// Set the timer for the soonest wait.
    fn arm() {
        let Some(soonest) = WAITING.with(|w| w.borrow().iter().map(|(_, at)| *at).min()) else { return };
        let seconds = soonest.saturating_duration_since(Instant::now()).as_secs_f64();
        let date = NSDate::dateWithTimeIntervalSinceNow(seconds);
        let existing = TIMER.with(|t| t.borrow().clone());
        if let Some(timer) = existing {
            timer.setFireDate(&date);
            return;
        }
        // Repeating, so it stays valid between waits: each fire sets its
        // next date.
        let block = block2::RcBlock::new(|_: NonNull<NSTimer>| fired());
        // SAFETY: the block runs on the main thread, where the timer is
        // scheduled.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1e9, true, &block) };
        timer.setFireDate(&date);
        // SAFETY: the main loop takes the timer, in the common modes so
        // scrollers fade during tracking loops too; NSRunLoopCommonModes is
        // Foundation's constant.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        TIMER.with(|t| t.replace(Some(timer)));
    }

    fn fired() {
        let now = Instant::now();
        let due: Vec<Retained<NSScroller>> = WAITING.with(|w| {
            let mut w = w.borrow_mut();
            let mut due = Vec::new();
            w.retain(|(v, at)| match v.load() {
                Some(s) if *at <= now => {
                    due.push(s);
                    false
                }
                Some(_) => true,
                None => false,
            });
            due
        });
        for scroller in due {
            if let Some(next) = tick(imp(&scroller), now) {
                WAITING.with(|w| w.borrow_mut().push((Weak::new(&*scroller), next)));
            }
        }
        let idle = WAITING.with(|w| w.borrow().is_empty());
        let timer = TIMER.with(|t| t.borrow().clone());
        match timer {
            Some(timer) if idle => timer.setFireDate(&NSDate::distantFuture()),
            _ => arm(),
        }
    }
}

/// Overlay scrollers hear when the pointer is over them.
fn update_area(s: &NSScrollerImpl) {
    let view = s.view();
    let old = s.ivars().area.take();
    if let Some(old) = &old {
        view.removeTrackingArea(old);
    }
    if !s.overlay() {
        return;
    }
    let options = NSTrackingAreaOptions::MouseEnteredAndExited
        | NSTrackingAreaOptions::ActiveInKeyWindow
        | NSTrackingAreaOptions::InVisibleRect;
    let owner: &AnyObject = view;
    // SAFETY: the scroller owns the area and removes it before it goes; no
    // user info.
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            <NSTrackingArea as objc2::AnyThread>::alloc(),
            NSRect::ZERO,
            options,
            Some(owner),
            None,
        )
    };
    view.addTrackingArea(&area);
    s.ivars().area.replace(Some(area));
}

// Drawing.

/// The legacy track's color.
pub(crate) fn track_color() -> Color {
    if theme::dark() { [0.16, 0.16, 0.16, 1.0] } else { [0.98, 0.98, 0.98, 1.0] }
}

fn knob_color(s: &NSScrollerImpl, active: bool) -> Color {
    let dark = match s.ivars().knob_style.get() {
        NSScrollerKnobStyle::Dark => false,
        NSScrollerKnobStyle::Light => true,
        _ => theme::dark(),
    };
    let alpha = if active { 0.62 } else { 0.45 };
    if dark { [1.0, 1.0, 1.0, alpha] } else { [0.0, 0.0, 0.0, alpha] }
}

/// How much an overlay scroller shows, 1 for legacy ones.
fn shown_alpha(s: &NSScrollerImpl) -> f64 {
    if s.overlay() { s.ivars().showing.get().alpha } else { 1.0 }
}

fn faded(c: Color, alpha: f64) -> Color {
    [c[0], c[1], c[2], c[3] * alpha as f32]
}

fn draw(s: &NSScrollerImpl) {
    if !theme::paint::recording() {
        return;
    }
    let alpha = shown_alpha(s);
    if alpha <= 0.0 {
        return;
    }
    let scroller = s.scroller();
    let hovered = s.ivars().showing.get().hovered;
    if !s.overlay() || hovered {
        scroller.drawKnobSlotInRect_highlight(scroller.rectForPart(NSScrollerPart::KnobSlot), false);
    }
    if usable(s) && s.ivars().proportion.get() < 1.0 {
        scroller.drawKnob();
    }
}

/// The slot: legacy scrollers' track fills the scroller, with a line on
/// its inner edge; a hovered overlay scroller's is a translucent band.
fn draw_slot(s: &NSScrollerImpl, _slot: NSRect) {
    let b = s.view().bounds();
    let palette = theme::palette();
    let alpha = shown_alpha(s);
    let track = if s.overlay() { faded(track_color(), 0.85) } else { track_color() };
    theme::paint::fill_rect(b, faded(track, alpha));
    let line = if s.ivars().horizontal.get() {
        NSRect::new(b.origin, NSSize::new(b.size.width, 1.0))
    } else {
        NSRect::new(b.origin, NSSize::new(1.0, b.size.height))
    };
    theme::paint::fill_rect(line, faded(palette.separator, alpha));
}

/// The knob: rounded, and wider over a hovered overlay scroller.
fn draw_knob(s: &NSScrollerImpl) {
    let mut r = s.scroller().rectForPart(NSScrollerPart::Knob);
    let hovered = s.ivars().showing.get().hovered;
    if s.overlay() && hovered {
        // As wide as a legacy knob.
        let wide = thickness(NSScrollerStyle::Legacy, s.ivars().size.get());
        let grow = wide - r.size.width.min(r.size.height);
        if s.ivars().horizontal.get() {
            r.origin.y -= grow;
            r.size.height += grow;
        } else {
            r.origin.x -= grow;
            r.size.width += grow;
        }
    }
    // Legacy knobs sit a point inside the slot.
    if !s.overlay() {
        r = NSRect::new(
            NSPoint::new(r.origin.x + 1.0, r.origin.y + 1.0),
            NSSize::new((r.size.width - 2.0).max(0.0), (r.size.height - 2.0).max(0.0)),
        );
    }
    let radius = r.size.width.min(r.size.height) / 2.0;
    let active = hovered || s.ivars().press.get().is_some();
    let color = faded(knob_color(s, active), shown_alpha(s));
    theme::paint::fill_round_rect(r, theme::paint::radii(radius), color);
}
