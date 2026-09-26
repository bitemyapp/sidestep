//! `NSSlider` and `NSSliderCell`: a knob on a track, for a value between
//! limits.
//!
//! What's observable follows AppKit (`conformance/tests/controls.rs`,
//! `steppers_sliders_switches`): values stay between the limits, which
//! pull the value along when they move past it (and on a tick mark, when
//! only tick values are allowed); tick marks spread over the track's whole
//! length, 2 points square, the first and last centered on its ends, and
//! a lone one in the middle, worth the middle value; a point finds a tick
//! only within a point of it; a slider taller than it is wide is vertical
//! unless told otherwise; the knob is 20 points long at the regular size.
//!
//! A slider cell sends its action as its `sendActionOn:` mask says, which
//! starts as the mouse going down, dragging and going up: a continuous
//! slider is one whose mask has the drag (`setContinuous:NO` leaves only
//! the release), so `sendActionOn:NSEventMaskLeftMouseUp` makes a slider
//! that acts once, on release. A press on the track moves the knob's
//! center there and follows the mouse until it's released, anywhere. The
//! track fills with the accent up to the knob.
//!
//! The keys work it through the action methods a key binding sends:
//! `moveRight:` and `moveUp:` step up by a twentieth of the range (the
//! alternate increment, if set; the next tick, when only ticks are
//! allowed), `moveLeft:` and `moveDown:` step down, and `pageUp:` and
//! `pageDown:` go to the ends.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSActionCell, NSCell, NSCellHitResult, NSColor, NSControl, NSEvent, NSEventMask, NSEventType, NSFont, NSResponder,
    NSSlider, NSSliderCell, NSSliderType, NSTickMarkPosition, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSZone};

use super::cell::{Flags, imp as cell_imp};
use super::value::Value;
use super::{control, track};
use crate::theme::{self, metrics, parts};

pub(crate) struct SliderIvars {
    min: Cell<f64>,
    max: Cell<f64>,
    alt_increment: Cell<f64>,
    ticks: Cell<isize>,
    tick_position: Cell<NSTickMarkPosition>,
    ticks_only: Cell<bool>,
    /// Set by the program; otherwise the frame decides.
    vertical: Cell<Option<bool>>,
    knob: Cell<Option<f64>>,
    kind: Cell<NSSliderType>,
    track_fill: std::cell::RefCell<Option<Retained<NSColor>>>,
    neutral: Cell<f64>,
    pressed: Cell<bool>,
}

/// The mask a slider cell starts with: the mouse going down, dragging and
/// going up (`cell_state`).
const SLIDER_MASK: u64 = NSEventMask::LeftMouseDown.0 | NSEventMask::LeftMouseUp.0 | NSEventMask::LeftMouseDragged.0;
/// The bits `setContinuous:` adds and takes away; the drag says whether
/// the slider is continuous.
const CONTINUOUS_BITS: u64 = NSEventMask::LeftMouseDown.0 | NSEventMask::LeftMouseDragged.0;

define_class!(
    #[unsafe(super(NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSliderCell"]
    #[ivars = SliderIvars]
    pub(crate) struct NSSliderCellImpl;

    impl NSSliderCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(SliderIvars {
                min: Cell::new(0.0),
                max: Cell::new(1.0),
                alt_increment: Cell::new(0.0),
                ticks: Cell::new(0),
                tick_position: Cell::new(NSTickMarkPosition::Below),
                ticks_only: Cell::new(false),
                vertical: Cell::new(None),
                knob: Cell::new(None),
                kind: Cell::new(NSSliderType::Linear),
                track_fill: std::cell::RefCell::new(None),
                neutral: Cell::new(0.0),
                pressed: Cell::new(false),
            });
            // SAFETY: NSActionCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: string] };
            let base = cell_imp(as_cell(&this));
            base.replace_value(Value::Double(0.0));
            base.set_action_mask(SLIDER_MASK);
            this
        }

        #[unsafe(method(isContinuous))]
        fn is_continuous(&self) -> bool {
            cell_imp(as_cell(self)).action_mask() & NSEventMask::LeftMouseDragged.0 != 0
        }

        #[unsafe(method(setContinuous:))]
        fn set_continuous(&self, flag: bool) {
            cell_imp(as_cell(self)).set_mask_bits(CONTINUOUS_BITS, flag);
        }

        #[unsafe(method(hitTestForEvent:inRect:ofView:))]
        fn hit_test_for_event(&self, event: &NSEvent, frame: NSRect, view: &NSView) -> NSCellHitResult {
            track::hit_in_frame(event, frame, view)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSActionCell's copyWithZone: returns a cell of this
            // class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            // SAFETY: the copy is an instance of the receiver's class.
            let theirs = unsafe { &*(Retained::as_ptr(&copy).cast::<NSSliderCellImpl>()) };
            let (mine, copied) = (self.ivars(), theirs.ivars());
            copied.min.set(mine.min.get());
            copied.max.set(mine.max.get());
            copied.alt_increment.set(mine.alt_increment.get());
            copied.ticks.set(mine.ticks.get());
            copied.tick_position.set(mine.tick_position.get());
            copied.ticks_only.set(mine.ticks_only.get());
            copied.vertical.set(mine.vertical.get());
            copied.knob.set(mine.knob.get());
            copied.kind.set(mine.kind.get());
            copied.track_fill.replace(mine.track_fill.borrow().clone());
            copied.neutral.set(mine.neutral.get());
            copy
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, _image: Option<&AnyObject>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method(prefersTrackingUntilMouseUp))]
        fn prefers_tracking_until_mouse_up() -> bool {
            true
        }

        #[unsafe(method(minValue))]
        fn min_value(&self) -> f64 {
            self.ivars().min.get()
        }

        #[unsafe(method(setMinValue:))]
        fn set_min_value(&self, value: f64) {
            if self.ivars().min.replace(value) != value {
                // The value comes along if the limit moved past it.
                let v = as_cell(self).doubleValue();
                store(self, v);
                self.redraw();
            }
        }

        #[unsafe(method(maxValue))]
        fn max_value(&self) -> f64 {
            self.ivars().max.get()
        }

        #[unsafe(method(setMaxValue:))]
        fn set_max_value(&self, value: f64) {
            if self.ivars().max.replace(value) != value {
                let v = as_cell(self).doubleValue();
                store(self, v);
                self.redraw();
            }
        }

        #[unsafe(method(altIncrementValue))]
        fn alt_increment_value(&self) -> f64 {
            self.ivars().alt_increment.get()
        }

        #[unsafe(method(setAltIncrementValue:))]
        fn set_alt_increment_value(&self, value: f64) {
            self.ivars().alt_increment.set(value);
        }

        #[unsafe(method(sliderType))]
        fn slider_type(&self) -> NSSliderType {
            self.ivars().kind.get()
        }

        #[unsafe(method(setSliderType:))]
        fn set_slider_type(&self, kind: NSSliderType) {
            self.ivars().kind.set(kind);
            self.redraw();
        }

        #[unsafe(method(isVertical))]
        fn is_vertical(&self) -> bool {
            vertical(self)
        }

        #[unsafe(method(setVertical:))]
        fn set_vertical(&self, flag: bool) {
            self.ivars().vertical.set(Some(flag));
            self.redraw();
        }

        #[unsafe(method(knobThickness))]
        fn knob_thickness(&self) -> f64 {
            knob(self)
        }

        #[unsafe(method(setKnobThickness:))]
        fn set_knob_thickness(&self, thickness: f64) {
            self.ivars().knob.set(Some(thickness));
            self.redraw();
        }

        #[unsafe(method(trackRect))]
        fn track_rect(&self) -> NSRect {
            track_rect(self, bounds(self))
        }

        #[unsafe(method(knobRectFlipped:))]
        fn knob_rect_flipped(&self, _flipped: bool) -> NSRect {
            knob_rect(self, bounds(self))
        }

        #[unsafe(method(barRectFlipped:))]
        fn bar_rect_flipped(&self, _flipped: bool) -> NSRect {
            bar_rect(self, bounds(self))
        }

        #[unsafe(method(numberOfTickMarks))]
        fn number_of_tick_marks(&self) -> isize {
            self.ivars().ticks.get()
        }

        #[unsafe(method(setNumberOfTickMarks:))]
        fn set_number_of_tick_marks(&self, count: isize) {
            self.ivars().ticks.set(count.max(0));
            self.redraw();
        }

        #[unsafe(method(tickMarkPosition))]
        fn tick_mark_position(&self) -> NSTickMarkPosition {
            self.ivars().tick_position.get()
        }

        #[unsafe(method(setTickMarkPosition:))]
        fn set_tick_mark_position(&self, position: NSTickMarkPosition) {
            self.ivars().tick_position.set(position);
            self.redraw();
        }

        #[unsafe(method(allowsTickMarkValuesOnly))]
        fn allows_tick_mark_values_only(&self) -> bool {
            self.ivars().ticks_only.get()
        }

        #[unsafe(method(setAllowsTickMarkValuesOnly:))]
        fn set_allows_tick_mark_values_only(&self, flag: bool) {
            self.ivars().ticks_only.set(flag);
        }

        #[unsafe(method(tickMarkValueAtIndex:))]
        fn tick_mark_value_at_index(&self, index: isize) -> f64 {
            tick_value(self, index)
        }

        #[unsafe(method(rectOfTickMarkAtIndex:))]
        fn rect_of_tick_mark_at_index(&self, index: isize) -> NSRect {
            tick_rect(self, bounds(self), index)
        }

        #[unsafe(method(indexOfTickMarkAtPoint:))]
        fn index_of_tick_mark_at_point(&self, point: NSPoint) -> isize {
            tick_at(self, bounds(self), point).map_or(NOT_FOUND, |i| i as isize)
        }

        #[unsafe(method(closestTickMarkValueToValue:))]
        fn closest_tick_mark_value_to_value(&self, value: f64) -> f64 {
            closest_tick(self, value)
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            store(self, value);
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            store(self, f64::from(value));
        }

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            store(self, f64::from(value));
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            store(self, value as f64);
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, object: Option<&AnyObject>) {
            store(self, Value::from_object(object).double());
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, string: &NSString) {
            store(self, string.doubleValue());
        }

        // Titles belong to AppKit's old titled sliders; they're kept.

        #[unsafe(method_id(titleCell))]
        fn title_cell(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setTitleCell:))]
        fn set_title_cell(&self, _cell: Option<&NSCell>) {}

        #[unsafe(method_id(titleColor))]
        fn title_color(&self) -> Option<Retained<NSColor>> {
            None
        }

        #[unsafe(method(setTitleColor:))]
        fn set_title_color(&self, _color: Option<&NSColor>) {}

        #[unsafe(method_id(titleFont))]
        fn title_font(&self) -> Option<Retained<NSFont>> {
            None
        }

        #[unsafe(method(setTitleFont:))]
        fn set_title_font(&self, _font: Option<&NSFont>) {}

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            let t = metrics::SLIDER[cell_imp(as_cell(self)).control_size_index()];
            if vertical(self) { NSSize::new(t, metrics::UNBOUNDED_CELL) } else { NSSize::new(metrics::UNBOUNDED_CELL, t) }
        }

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, _view: &NSView) {
            draw(self, frame);
        }

        #[unsafe(method(drawKnob))]
        fn draw_knob(&self) {}

        #[unsafe(method(drawKnob:))]
        fn draw_knob_in(&self, rect: NSRect) {
            let base = cell_imp(as_cell(self));
            let state = parts::State { disabled: !base.has(Flags::ENABLED), pressed: self.ivars().pressed.get(), ..Default::default() };
            parts::knob(theme::palette(), rect, state);
        }

        #[unsafe(method(drawBarInside:flipped:))]
        fn draw_bar_inside(&self, _rect: NSRect, _flipped: bool) {}

        #[unsafe(method(drawTickMarks))]
        fn draw_tick_marks(&self) {}
    }

    unsafe impl NSObjectProtocol for NSSliderCellImpl {}
);

impl NSSliderCellImpl {
    fn redraw(&self) {
        if let Some(view) = cell_imp(as_cell(self)).view() {
            view.setNeedsDisplay(true);
        }
    }
}

/// `NSNotFound`.
const NOT_FOUND: isize = isize::MAX;

fn as_cell(cell: &NSSliderCellImpl) -> &NSCell {
    // SAFETY: NSSliderCell is a subclass of NSCell.
    unsafe { &*(cell as *const NSSliderCellImpl).cast::<NSCell>() }
}

fn slider_cell(cell: &NSCell) -> Option<&NSSliderCellImpl> {
    // SAFETY: NSSliderCellImpl is the class NSSliderCell names.
    unsafe { super::impl_of::<NSSliderCell, NSSliderCellImpl>(cell) }
}

/// The bounds of the view showing the cell.
fn bounds(cell: &NSSliderCellImpl) -> NSRect {
    cell_imp(as_cell(cell)).view().map_or(NSRect::ZERO, |v| v.bounds())
}

fn vertical(cell: &NSSliderCellImpl) -> bool {
    cell.ivars().vertical.get().unwrap_or_else(|| {
        let b = bounds(cell);
        b.size.height > b.size.width
    })
}

fn knob(cell: &NSSliderCellImpl) -> f64 {
    cell.ivars().knob.get().unwrap_or(metrics::KNOB[cell_imp(as_cell(cell)).control_size_index()])
}

/// The track's length and where it starts, along its axis.
fn span(cell: &NSSliderCellImpl, b: NSRect) -> (f64, f64) {
    if vertical(cell) { (b.origin.y, b.size.height) } else { (b.origin.x, b.size.width) }
}

fn fraction(cell: &NSSliderCellImpl) -> f64 {
    let (min, max) = (cell.ivars().min.get(), cell.ivars().max.get());
    let v = as_cell(cell).doubleValue();
    if max > min { ((v - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 }
}

fn knob_rect(cell: &NSSliderCellImpl, b: NSRect) -> NSRect {
    let k = knob(cell);
    let t = metrics::SLIDER[cell_imp(as_cell(cell)).control_size_index()];
    let f = fraction(cell);
    if vertical(cell) {
        // The minimum is at the bottom of the screen.
        let flipped = cell_imp(as_cell(cell)).view().is_some_and(|v| v.isFlipped());
        let travel = (b.size.height - k).max(0.0);
        let y = if flipped { b.origin.y + (1.0 - f) * travel } else { b.origin.y + f * travel };
        NSRect::new(NSPoint::new(b.origin.x + ((b.size.width - t) / 2.0).round(), y), NSSize::new(t, k))
    } else {
        let x = b.origin.x + f * (b.size.width - k).max(0.0);
        NSRect::new(NSPoint::new(x, b.origin.y + ((b.size.height - t) / 2.0).round()), NSSize::new(k, t))
    }
}

/// `trackRect`: the bounds, moved across by half the room the slider's
/// thickness leaves (`steppers_sliders_switches`).
fn track_rect(cell: &NSSliderCellImpl, b: NSRect) -> NSRect {
    let t = metrics::SLIDER[cell_imp(as_cell(cell)).control_size_index()];
    if vertical(cell) {
        NSRect::new(NSPoint::new(b.origin.x + (b.size.width - t) / 2.0, b.origin.y), b.size)
    } else {
        NSRect::new(NSPoint::new(b.origin.x, b.origin.y + (b.size.height - t) / 2.0), b.size)
    }
}

fn bar_rect(cell: &NSSliderCellImpl, b: NSRect) -> NSRect {
    let bar = metrics::SLIDER_BAR;
    if vertical(cell) {
        NSRect::new(
            NSPoint::new(b.origin.x + ((b.size.width - bar) / 2.0).round(), b.origin.y),
            NSSize::new(bar, b.size.height),
        )
    } else {
        NSRect::new(
            NSPoint::new(b.origin.x, b.origin.y + ((b.size.height - bar) / 2.0).round()),
            NSSize::new(b.size.width, bar),
        )
    }
}

fn tick_value(cell: &NSSliderCellImpl, index: isize) -> f64 {
    let n = cell.ivars().ticks.get();
    let (min, max) = (cell.ivars().min.get(), cell.ivars().max.get());
    if n <= 1 {
        // A lone tick is in the middle.
        return (min + max) / 2.0;
    }
    min + (max - min) * index as f64 / (n - 1) as f64
}

/// Where tick `index` sits along the track.
fn tick_offset(cell: &NSSliderCellImpl, b: NSRect, index: isize) -> f64 {
    let n = cell.ivars().ticks.get();
    let (start, len) = span(cell, b);
    if n <= 1 { start + len / 2.0 } else { start + len * index as f64 / (n - 1) as f64 }
}

fn tick_rect(cell: &NSSliderCellImpl, b: NSRect, index: isize) -> NSRect {
    let at = tick_offset(cell, b, index);
    if vertical(cell) {
        NSRect::new(NSPoint::new(b.origin.x + ((b.size.width - 2.0) / 2.0).round(), at - 1.0), NSSize::new(2.0, 2.0))
    } else {
        NSRect::new(NSPoint::new(at - 1.0, b.origin.y + ((b.size.height - 2.0) / 2.0).round()), NSSize::new(2.0, 2.0))
    }
}

/// The tick at `p`: one whose rect, grown by a point all round, holds
/// it (the near edges in, the far ones out).
fn tick_at(cell: &NSSliderCellImpl, b: NSRect, p: NSPoint) -> Option<usize> {
    let n = cell.ivars().ticks.get().max(0) as usize;
    (0..n).find(|&i| {
        let r = tick_rect(cell, b, i as isize);
        let (x0, y0) = (r.origin.x - 1.0, r.origin.y - 1.0);
        let (x1, y1) = (r.origin.x + r.size.width + 1.0, r.origin.y + r.size.height + 1.0);
        p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1
    })
}

fn closest_tick(cell: &NSSliderCellImpl, value: f64) -> f64 {
    let n = cell.ivars().ticks.get();
    if n == 0 {
        return value;
    }
    (0..n).map(|i| tick_value(cell, i)).min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs())).unwrap_or(value)
}

/// Store a value: between the limits, and on a tick when only those are
/// allowed.
fn store(cell: &NSSliderCellImpl, value: f64) {
    let (min, max) = (cell.ivars().min.get(), cell.ivars().max.get());
    let mut value = if value > max {
        max
    } else if value < min {
        min
    } else {
        value
    };
    if cell.ivars().ticks_only.get() {
        value = closest_tick(cell, value);
    }
    let base = cell_imp(as_cell(cell));
    let same = matches!(base.value(), Value::Double(v) if v == value);
    if !same {
        base.replace_value(Value::Double(value));
        cell.redraw();
    }
}

/// The value for the knob's center at `p` (view coordinates).
fn value_at(cell: &NSSliderCellImpl, view: &NSView, p: NSPoint) -> f64 {
    let b = view.bounds();
    let k = knob(cell);
    let (min, max) = (cell.ivars().min.get(), cell.ivars().max.get());
    let f = if vertical(cell) {
        let travel = (b.size.height - k).max(1.0);
        let from_bottom = if view.isFlipped() { b.origin.y + b.size.height - p.y } else { p.y - b.origin.y };
        (from_bottom - k / 2.0) / travel
    } else {
        (p.x - b.origin.x - k / 2.0) / (b.size.width - k).max(1.0)
    };
    min + f.clamp(0.0, 1.0) * (max - min)
}

/// A press: move the knob to the mouse and follow it until the release,
/// sending the action as the mask says: on the press and each drag that
/// changes the value, and on the release (always, for a slider that
/// doesn't act while dragged).
fn track_slider(control: &NSControl, cell: &NSCell, c: &NSSliderCellImpl, event: &NSEvent) {
    let view: &NSView = control;
    let mtm = MainThreadMarker::from(control);
    let mask = cell_imp(cell).action_mask();
    let on = |bit: NSEventMask| mask & bit.0 != 0;
    let continuous = on(NSEventMask::LeftMouseDragged);
    c.ivars().pressed.set(true);
    let mut last = cell.doubleValue();
    let mut move_to = |p: NSPoint| {
        cell.setDoubleValue(value_at(c, view, p));
        let now = cell.doubleValue();
        let changed = now != last;
        last = now;
        changed
    };
    if move_to(control::event_point(view, event)) && on(NSEventMask::LeftMouseDown) {
        track::send_cell_action(cell, view);
    }
    while let Some(next) = track::next_event(mtm, NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged, None) {
        let changed = move_to(control::event_point(view, &next));
        if next.r#type() == NSEventType::LeftMouseUp {
            if on(NSEventMask::LeftMouseUp) && (changed || !continuous) {
                track::send_cell_action(cell, view);
            }
            break;
        }
        if changed && continuous {
            track::send_cell_action(cell, view);
        }
    }
    c.ivars().pressed.set(false);
    view.setNeedsDisplay(true);
}

/// What the slider's keys do: a step up or down, or a jump to an end.
#[derive(Clone, Copy)]
enum Move {
    Step(bool),
    End(bool),
}

/// Move the value as the action methods do (see the module
/// documentation) and send the action.
fn key_move(control: &NSControl, how: Move) {
    let Some(cell) = control.cell() else { return };
    let Some(c) = slider_cell(&cell) else { return };
    if !control.isEnabled() {
        return;
    }
    let (min, max) = (c.ivars().min.get(), c.ivars().max.get());
    let value = match how {
        Move::End(up) => {
            if up {
                max
            } else {
                min
            }
        }
        Move::Step(up) => {
            let n = c.ivars().ticks.get();
            let delta = if c.ivars().ticks_only.get() && n > 1 {
                (max - min) / (n - 1) as f64
            } else if c.ivars().alt_increment.get() > 0.0 {
                c.ivars().alt_increment.get()
            } else {
                (max - min) / 20.0
            };
            cell.doubleValue() + if up { delta } else { -delta }
        }
    };
    cell.setDoubleValue(value);
    let view: &NSView = control;
    track::send_cell_action(&cell, view);
}

fn draw(cell: &NSSliderCellImpl, frame: NSRect) {
    if !theme::paint::recording() {
        return;
    }
    let p = theme::palette();
    let base = cell_imp(as_cell(cell));
    let state =
        parts::State { disabled: !base.has(Flags::ENABLED), pressed: cell.ivars().pressed.get(), ..Default::default() };
    let bar = bar_rect(cell, frame);
    let k = knob_rect(cell, frame);
    let vertical = vertical(cell);
    let flipped = base.view().is_some_and(|v| v.isFlipped());
    let fill = cell.ivars().track_fill.borrow().clone();
    let fill = fill.as_deref().map(theme::color_of);
    // The accent runs from the minimum to the knob's center.
    if vertical {
        let center = k.origin.y + k.size.height / 2.0;
        let (lo, hi) = if flipped { (center, bar.origin.y + bar.size.height) } else { (bar.origin.y, center) };
        parts::slider_track(p, bar, true, None, None, state);
        let filled = NSRect::new(NSPoint::new(bar.origin.x, lo), NSSize::new(bar.size.width, (hi - lo).max(0.0)));
        theme::paint::fill_round_rect(
            filled,
            theme::paint::radii(bar.size.width / 2.0),
            if state.disabled { theme::palette::dimmed(fill.unwrap_or(p.accent)) } else { fill.unwrap_or(p.accent) },
        );
    } else {
        parts::slider_track(p, bar, false, Some(k.origin.x + k.size.width / 2.0), fill, state);
    }
    // Ticks sit on the track: light on the accent, dark off it.
    let center = if vertical { k.origin.y + k.size.height / 2.0 } else { k.origin.x + k.size.width / 2.0 };
    for i in 0..cell.ivars().ticks.get() {
        let r = tick_rect(cell, frame, i);
        let at = if vertical { r.origin.y + r.size.height / 2.0 } else { r.origin.x + r.size.width / 2.0 };
        let on_fill = if vertical && flipped { at >= center } else { at <= center };
        parts::tick(p, r, on_fill, state);
    }
    // SAFETY: drawKnob: takes the knob's rect; subclasses override it.
    let _: () = unsafe { msg_send![cell, drawKnob: k] };
}

// NSSlider

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSlider"]
    pub(crate) struct NSSliderImpl;

    impl NSSliderImpl {
        #[unsafe(method_id(sliderWithTarget:action:))]
        fn slider_with_target(target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSSlider> {
            make_slider(0.0, 0.0, 1.0, target, action)
        }

        #[unsafe(method_id(sliderWithValue:minValue:maxValue:target:action:))]
        fn slider_with_value(value: f64, min: f64, max: f64, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSSlider> {
            make_slider(value, min, max, target, action)
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
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

        #[unsafe(method(minValue))]
        fn min_value(&self) -> f64 {
            self.cell_or(0.0, |c| c.minValue())
        }

        #[unsafe(method(setMinValue:))]
        fn set_min_value(&self, value: f64) {
            self.with(|c| c.setMinValue(value));
        }

        #[unsafe(method(maxValue))]
        fn max_value(&self) -> f64 {
            self.cell_or(0.0, |c| c.maxValue())
        }

        #[unsafe(method(setMaxValue:))]
        fn set_max_value(&self, value: f64) {
            self.with(|c| c.setMaxValue(value));
        }

        #[unsafe(method(neutralValue))]
        fn neutral_value(&self) -> f64 {
            self.slider_imp(|c| c.ivars().neutral.get()).unwrap_or(0.0)
        }

        #[unsafe(method(setNeutralValue:))]
        fn set_neutral_value(&self, value: f64) {
            self.slider_imp(|c| c.ivars().neutral.set(value));
        }

        // The keys, through the methods key bindings send.

        #[unsafe(method(moveRight:))]
        fn move_right(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::Step(true));
        }

        #[unsafe(method(moveUp:))]
        fn move_up(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::Step(true));
        }

        #[unsafe(method(moveLeft:))]
        fn move_left(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::Step(false));
        }

        #[unsafe(method(moveDown:))]
        fn move_down(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::Step(false));
        }

        #[unsafe(method(pageUp:))]
        fn page_up(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::End(true));
        }

        #[unsafe(method(pageDown:))]
        fn page_down(&self, _sender: Option<&AnyObject>) {
            key_move(self.control(), Move::End(false));
        }

        #[unsafe(method(altIncrementValue))]
        fn alt_increment_value(&self) -> f64 {
            self.cell_or(0.0, |c| c.altIncrementValue())
        }

        #[unsafe(method(setAltIncrementValue:))]
        fn set_alt_increment_value(&self, value: f64) {
            self.with(|c| c.setAltIncrementValue(value));
        }

        #[unsafe(method(sliderType))]
        fn slider_type(&self) -> NSSliderType {
            self.cell_or(NSSliderType::Linear, |c| c.sliderType())
        }

        #[unsafe(method(setSliderType:))]
        fn set_slider_type(&self, kind: NSSliderType) {
            self.with(|c| c.setSliderType(kind));
        }

        #[unsafe(method(isVertical))]
        fn is_vertical(&self) -> bool {
            self.cell_or(false, |c| c.isVertical())
        }

        #[unsafe(method(setVertical:))]
        fn set_vertical(&self, flag: bool) {
            self.with(|c| c.setVertical(flag));
        }

        #[unsafe(method(knobThickness))]
        fn knob_thickness(&self) -> f64 {
            self.cell_or(0.0, |c| c.knobThickness())
        }

        #[unsafe(method(setKnobThickness:))]
        fn set_knob_thickness(&self, thickness: f64) {
            // SAFETY: setKnobThickness: takes a CGFloat (the binding marks it
            // deprecated, but programs still send it).
            self.with(|c| unsafe { msg_send![c, setKnobThickness: thickness] });
        }

        #[unsafe(method_id(trackFillColor))]
        fn track_fill_color(&self) -> Option<Retained<NSColor>> {
            self.slider_imp(|c| c.ivars().track_fill.borrow().clone()).flatten()
        }

        #[unsafe(method(setTrackFillColor:))]
        fn set_track_fill_color(&self, color: Option<&NSColor>) {
            self.slider_imp(|c| {
                c.ivars().track_fill.replace(color.map(objc2::Message::retain));
                c.redraw();
            });
        }

        #[unsafe(method(tintProminence))]
        fn tint_prominence(&self) -> isize {
            0
        }

        #[unsafe(method(setTintProminence:))]
        fn set_tint_prominence(&self, _prominence: isize) {}

        #[unsafe(method(numberOfTickMarks))]
        fn number_of_tick_marks(&self) -> isize {
            self.cell_or(0, |c| c.numberOfTickMarks())
        }

        #[unsafe(method(setNumberOfTickMarks:))]
        fn set_number_of_tick_marks(&self, count: isize) {
            self.with(|c| c.setNumberOfTickMarks(count));
        }

        #[unsafe(method(tickMarkPosition))]
        fn tick_mark_position(&self) -> NSTickMarkPosition {
            self.cell_or(NSTickMarkPosition::Below, |c| c.tickMarkPosition())
        }

        #[unsafe(method(setTickMarkPosition:))]
        fn set_tick_mark_position(&self, position: NSTickMarkPosition) {
            self.with(|c| c.setTickMarkPosition(position));
        }

        #[unsafe(method(allowsTickMarkValuesOnly))]
        fn allows_tick_mark_values_only(&self) -> bool {
            self.cell_or(false, |c| c.allowsTickMarkValuesOnly())
        }

        #[unsafe(method(setAllowsTickMarkValuesOnly:))]
        fn set_allows_tick_mark_values_only(&self, flag: bool) {
            self.with(|c| c.setAllowsTickMarkValuesOnly(flag));
        }

        #[unsafe(method(tickMarkValueAtIndex:))]
        fn tick_mark_value_at_index(&self, index: isize) -> f64 {
            self.cell_or(0.0, |c| c.tickMarkValueAtIndex(index))
        }

        #[unsafe(method(rectOfTickMarkAtIndex:))]
        fn rect_of_tick_mark_at_index(&self, index: isize) -> NSRect {
            self.cell_or(NSRect::ZERO, |c| c.rectOfTickMarkAtIndex(index))
        }

        #[unsafe(method(indexOfTickMarkAtPoint:))]
        fn index_of_tick_mark_at_point(&self, point: NSPoint) -> isize {
            self.cell_or(NOT_FOUND, |c| c.indexOfTickMarkAtPoint(point))
        }

        #[unsafe(method(closestTickMarkValueToValue:))]
        fn closest_tick_mark_value_to_value(&self, value: f64) -> f64 {
            self.cell_or(value, |c| c.closestTickMarkValueToValue(value))
        }

        #[unsafe(method_id(titleCell))]
        fn title_cell(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setTitleCell:))]
        fn set_title_cell(&self, _cell: Option<&NSCell>) {}

        #[unsafe(method_id(titleColor))]
        fn title_color(&self) -> Option<Retained<NSColor>> {
            None
        }

        #[unsafe(method(setTitleColor:))]
        fn set_title_color(&self, _color: Option<&NSColor>) {}

        #[unsafe(method_id(titleFont))]
        fn title_font(&self) -> Option<Retained<NSFont>> {
            None
        }

        #[unsafe(method(setTitleFont:))]
        fn set_title_font(&self, _font: Option<&NSFont>) {}

        #[unsafe(method_id(title))]
        fn title(&self) -> Option<Retained<NSString>> {
            None
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, _title: Option<&NSString>) {}

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, _image: Option<&AnyObject>) {}

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            let none = control::NO_METRIC;
            let Some(c) = self.cell() else { return NSSize::new(none, none) };
            let t = metrics::SLIDER[cell_imp(&c).control_size_index()];
            if c.isVertical() { NSSize::new(t, none) } else { NSSize::new(none, t) }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let control = self.control();
            if !control.isEnabled() {
                return;
            }
            if let Some(cell) = control.cell()
                && let Some(c) = slider_cell(&cell)
            {
                track_slider(control, &cell, c, event);
            }
        }
    }
);

impl NSSliderImpl {
    fn control(&self) -> &NSControl {
        // SAFETY: NSSlider is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn cell(&self) -> Option<Retained<NSSliderCell>> {
        let cell = self.control().cell()?;
        slider_cell(&cell)?;
        // SAFETY: checked just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn with(&self, f: impl FnOnce(&NSSliderCell)) {
        if let Some(c) = self.cell() {
            f(&c);
        }
    }

    fn cell_or<R>(&self, default: R, f: impl FnOnce(&NSSliderCell) -> R) -> R {
        self.cell().map_or(default, |c| f(&c))
    }

    fn slider_imp<R>(&self, f: impl FnOnce(&NSSliderCellImpl) -> R) -> Option<R> {
        let cell = self.control().cell()?;
        slider_cell(&cell).map(f)
    }
}

/// The factories: a regular slider 100 points long, sized to fit across.
fn make_slider(value: f64, min: f64, max: f64, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSSlider> {
    let mtm = MainThreadMarker::new().expect("sidestep: AppKit's controls belong to the main thread");
    let height = metrics::SLIDER[0];
    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(100.0, height)));
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(value);
    // SAFETY: the slider keeps the target weakly; any selector may be an
    // action.
    unsafe {
        slider.setTarget(target);
        slider.setAction(action);
    }
    slider
}
