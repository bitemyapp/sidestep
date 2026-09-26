//! `NSStepper` and `NSStepperCell`: two arrows that step a value up and
//! down between limits.
//!
//! Values stay between the limits as they're set, and the limits pull
//! the value along when they move past it (`conformance/tests/
//! controls.rs`, `steppers_sliders_switches`); stepping past a limit wraps
//! to the other when the value wraps, and stops there otherwise. Holding
//! an arrow steps again after half a second, then ten times a second, as
//! long as the mouse stays on that arrow (autorepeat, sending the action
//! each step). The upper arrow steps up; the stepper isn't flipped.
//! `moveUp:` and `moveDown:` (the arrow keys, through key bindings) step
//! too, and `performClick:` steps the way the last step went (down, at
//! first), as on macOS. Each sends the action.

use std::cell::Cell;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSActionCell, NSCell, NSCellHitResult, NSControl, NSEvent, NSEventMask, NSEventType, NSResponder, NSStepperCell,
    NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSZone};

use super::cell::{Flags, imp as cell_imp};
use super::value::Value;
use super::{control, track};
use crate::theme::{self, metrics, parts};

pub(crate) struct StepperIvars {
    min: Cell<f64>,
    max: Cell<f64>,
    increment: Cell<f64>,
    wraps: Cell<bool>,
    autorepeat: Cell<bool>,
    /// The arrow held down: true for the upper one.
    pressed: Cell<Option<bool>>,
    /// The way the last step went, which `performClick:` goes again: up
    /// or, at first, down.
    last_up: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSStepperCell"]
    #[ivars = StepperIvars]
    pub(crate) struct NSStepperCellImpl;

    impl NSStepperCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(StepperIvars {
                min: Cell::new(0.0),
                max: Cell::new(59.0),
                increment: Cell::new(1.0),
                wraps: Cell::new(true),
                autorepeat: Cell::new(true),
                pressed: Cell::new(None),
                last_up: Cell::new(false),
            });
            // SAFETY: NSActionCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: string] };
            let base = cell_imp(as_cell(&this));
            base.replace_value(Value::Double(0.0));
            // Continuous: acting on the press and periodically after
            // (`cell_state`).
            base.set_action_mask(NSEventMask::LeftMouseDown.0 | NSEventMask::Periodic.0);
            this
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, _sender: Option<&AnyObject>) {
            let cell = as_cell(self);
            if !cell_imp(cell).has(Flags::ENABLED) {
                return;
            }
            step(cell, self.ivars().last_up.get());
            if let Some(view) = cell_imp(cell).view() {
                track::send_cell_action(cell, &view);
            }
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
            let theirs = unsafe { &*(Retained::as_ptr(&copy).cast::<NSStepperCellImpl>()) };
            let (mine, copied) = (self.ivars(), theirs.ivars());
            copied.min.set(mine.min.get());
            copied.max.set(mine.max.get());
            copied.increment.set(mine.increment.get());
            copied.wraps.set(mine.wraps.get());
            copied.autorepeat.set(mine.autorepeat.get());
            copy
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, _image: Option<&AnyObject>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method(minValue))]
        fn min_value(&self) -> f64 {
            self.ivars().min.get()
        }

        #[unsafe(method(setMinValue:))]
        fn set_min_value(&self, value: f64) {
            if self.ivars().min.replace(value) != value {
                store(self, as_cell(self).doubleValue());
            }
        }

        #[unsafe(method(maxValue))]
        fn max_value(&self) -> f64 {
            self.ivars().max.get()
        }

        #[unsafe(method(setMaxValue:))]
        fn set_max_value(&self, value: f64) {
            if self.ivars().max.replace(value) != value {
                store(self, as_cell(self).doubleValue());
            }
        }

        #[unsafe(method(increment))]
        fn increment(&self) -> f64 {
            self.ivars().increment.get()
        }

        #[unsafe(method(setIncrement:))]
        fn set_increment(&self, value: f64) {
            self.ivars().increment.set(value);
        }

        #[unsafe(method(valueWraps))]
        fn value_wraps(&self) -> bool {
            self.ivars().wraps.get()
        }

        #[unsafe(method(setValueWraps:))]
        fn set_value_wraps(&self, flag: bool) {
            self.ivars().wraps.set(flag);
        }

        #[unsafe(method(autorepeat))]
        fn autorepeat(&self) -> bool {
            self.ivars().autorepeat.get()
        }

        #[unsafe(method(setAutorepeat:))]
        fn set_autorepeat(&self, flag: bool) {
            self.ivars().autorepeat.set(flag);
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

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            NSSize::new(metrics::STEPPER.0, metrics::STEPPER.1)
        }

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            if !theme::paint::recording() {
                return;
            }
            let base = cell_imp(as_cell(self));
            let state = parts::State { disabled: !base.has(Flags::ENABLED), ..Default::default() };
            let axis = parts::Axis { flipped: view.isFlipped() };
            let body = parts::centered_square(frame, frame.size.width.min(frame.size.height));
            let body = NSRect::new(NSPoint::new(body.origin.x, frame.origin.y), NSSize::new(body.size.width, frame.size.height));
            parts::stepper(theme::palette(), body, axis, self.ivars().pressed.get(), state);
        }
    }

    unsafe impl NSObjectProtocol for NSStepperCellImpl {}
);

fn as_cell(cell: &NSStepperCellImpl) -> &NSCell {
    // SAFETY: NSStepperCell is a subclass of NSCell.
    unsafe { &*(cell as *const NSStepperCellImpl).cast::<NSCell>() }
}

/// Store a value, kept between the limits. A stepper doesn't show its
/// value, so storing one redraws nothing.
fn store(cell: &NSStepperCellImpl, value: f64) {
    let (min, max) = (cell.ivars().min.get(), cell.ivars().max.get());
    let value = if value > max {
        max
    } else if value < min {
        min
    } else {
        value
    };
    let base = cell_imp(as_cell(cell));
    if !matches!(base.value(), Value::Double(v) if v == value) {
        base.replace_value(Value::Double(value));
    }
}

fn stepper_cell(cell: &NSCell) -> Option<&NSStepperCellImpl> {
    // SAFETY: NSStepperCellImpl is the class NSStepperCell names.
    unsafe { super::impl_of::<NSStepperCell, NSStepperCellImpl>(cell) }
}

/// Step once, up or down, wrapping or stopping at the limits.
fn step(cell: &NSCell, up: bool) {
    let Some(c) = stepper_cell(cell) else { return };
    c.ivars().last_up.set(up);
    let (min, max, inc) = (c.ivars().min.get(), c.ivars().max.get(), c.ivars().increment.get());
    let value = cell.doubleValue();
    let next = if up { value + inc } else { value - inc };
    let next = if next > max {
        if c.ivars().wraps.get() { min } else { max }
    } else if next < min {
        if c.ivars().wraps.get() { max } else { min }
    } else {
        next
    };
    cell.setDoubleValue(next);
}

/// Whether `p` is on the upper arrow of a stepper in `view`.
fn upper(view: &NSView, p: NSPoint) -> bool {
    let b = view.bounds();
    let mid = b.origin.y + b.size.height / 2.0;
    if view.isFlipped() { p.y < mid } else { p.y >= mid }
}

/// A press on an arrow: step, then step again while held on it.
fn track_stepper(control: &NSControl, cell: &NSCell, c: &NSStepperCellImpl, event: &NSEvent) {
    let view: &NSView = control;
    let mtm = MainThreadMarker::from(control);
    let at = control::event_point(view, event);
    if !track::mouse_in_rect(at, view.bounds(), view.isFlipped()) {
        return;
    }
    let up = upper(view, at);
    c.ivars().pressed.set(Some(up));
    step(cell, up);
    track::send_cell_action(cell, view);
    let (delay, interval) = metrics::STEPPER_REPEAT;
    let mut tick = c.ivars().autorepeat.get().then(|| Instant::now() + Duration::from_secs_f64(delay));
    let mut over = true;
    loop {
        let next = track::next_event(mtm, NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged, tick);
        let Some(next) = next else {
            // A repeat, while the mouse stays on the arrow.
            if let Some(t) = tick.filter(|_| track::on_screen(mtm)) {
                tick = Some(t.max(Instant::now()) + Duration::from_secs_f64(interval));
                if over {
                    step(cell, up);
                    track::send_cell_action(cell, view);
                }
                continue;
            }
            break;
        };
        if next.r#type() == NSEventType::LeftMouseUp {
            break;
        }
        let at = control::event_point(view, &next);
        let now_over = track::mouse_in_rect(at, view.bounds(), view.isFlipped()) && upper(view, at) == up;
        if now_over != over {
            over = now_over;
            c.ivars().pressed.set(over.then_some(up));
            view.setNeedsDisplay(true);
        }
    }
    c.ivars().pressed.set(None);
    view.setNeedsDisplay(true);
}

// NSStepper

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSStepper"]
    pub(crate) struct NSStepperImpl;

    impl NSStepperImpl {
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

        #[unsafe(method(increment))]
        fn increment(&self) -> f64 {
            self.cell_or(0.0, |c| c.increment())
        }

        #[unsafe(method(setIncrement:))]
        fn set_increment(&self, value: f64) {
            self.with(|c| c.setIncrement(value));
        }

        #[unsafe(method(valueWraps))]
        fn value_wraps(&self) -> bool {
            self.cell_or(false, |c| c.valueWraps())
        }

        #[unsafe(method(setValueWraps:))]
        fn set_value_wraps(&self, flag: bool) {
            self.with(|c| c.setValueWraps(flag));
        }

        #[unsafe(method(autorepeat))]
        fn autorepeat(&self) -> bool {
            self.cell_or(false, |c| c.autorepeat())
        }

        #[unsafe(method(setAutorepeat:))]
        fn set_autorepeat(&self, flag: bool) {
            self.with(|c| c.setAutorepeat(flag));
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            NSSize::new(metrics::STEPPER.0, metrics::STEPPER.1)
        }

        // The keys, through the methods key bindings send.

        #[unsafe(method(moveUp:))]
        fn move_up(&self, _sender: Option<&AnyObject>) {
            key_step(self.control(), true);
        }

        #[unsafe(method(moveDown:))]
        fn move_down(&self, _sender: Option<&AnyObject>) {
            key_step(self.control(), false);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let control = self.control();
            if !control.isEnabled() {
                return;
            }
            if let Some(cell) = control.cell()
                && let Some(c) = stepper_cell(&cell)
            {
                track_stepper(control, &cell, c, event);
            }
        }
    }
);

/// A step from the keyboard: step and send the action.
fn key_step(control: &NSControl, up: bool) {
    let Some(cell) = control.cell() else { return };
    if !control.isEnabled() {
        return;
    }
    step(&cell, up);
    let view: &NSView = control;
    track::send_cell_action(&cell, view);
}

impl NSStepperImpl {
    fn control(&self) -> &NSControl {
        // SAFETY: NSStepper is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn cell(&self) -> Option<Retained<NSStepperCell>> {
        let cell = self.control().cell()?;
        stepper_cell(&cell)?;
        // SAFETY: checked just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn with(&self, f: impl FnOnce(&NSStepperCell)) {
        if let Some(c) = self.cell() {
            f(&c);
        }
    }

    fn cell_or<R>(&self, default: R, f: impl FnOnce(&NSStepperCell) -> R) -> R {
        self.cell().map_or(default, |c| f(&c))
    }
}
