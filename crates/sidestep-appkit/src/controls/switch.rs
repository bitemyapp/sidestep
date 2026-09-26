//! `NSSwitch`: an on/off toggle. Unlike most controls it has no cell: the
//! state is the control's own (`conformance/tests/controls.rs`,
//! `steppers_sliders_switches`), and it is 54 by 24 points at every
//! control size. It isn't flipped.
//!
//! The value and the state go together, as on macOS: setting the state
//! sets the value to it, and setting a value (`setIntValue:`,
//! `setObjectValue:` and the rest) keeps that value and sets the state
//! from its integer: on for any positive number, mixed for a negative one.
//!
//! A click toggles it and sends the action. Dragging moves the knob with
//! the mouse; releasing turns it to whichever side the knob is nearer,
//! sending the action if that changed the state.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSControl, NSEvent, NSEventMask, NSEventType, NSResponder, NSView};
use objc2_foundation::{NSRect, NSSize, NSString};

use super::{control, track};
use crate::theme::{self, metrics, parts};

pub(crate) struct SwitchIvars {
    state: Cell<isize>,
    /// While dragging, where the knob is, 0 (off) to 1 (on).
    dragged: Cell<Option<f64>>,
    pressed: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSwitch"]
    #[ivars = SwitchIvars]
    pub(crate) struct NSSwitchImpl;

    impl NSSwitchImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(SwitchIvars { state: Cell::new(0), dragged: Cell::new(None), pressed: Cell::new(false) });
            // SAFETY: NSControl's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let view: &NSView = self.control();
            if !super::focus::control_key_down(view, event) {
                // SAFETY: NSResponder's keyDown: passes the key on.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(state))]
        fn state(&self) -> isize {
            self.ivars().state.get()
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            let state = state.signum();
            // SAFETY: NSControl's setIntegerValue: keeps the value.
            let _: () = unsafe { msg_send![super(self), setIntegerValue: state] };
            self.show_state(state);
        }

        // The value setters keep the value and set the state from it.

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            // SAFETY: NSControl's setter, with the value.
            let _: () = unsafe { msg_send![super(self), setIntValue: value] };
            self.follow_value();
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), setIntegerValue: value] };
            self.follow_value();
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), setFloatValue: value] };
            self.follow_value();
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), setDoubleValue: value] };
            self.follow_value();
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, value: &NSString) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), setStringValue: value] };
            self.follow_value();
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, value: Option<&AnyObject>) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), setObjectValue: value] };
            self.follow_value();
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.control().isEnabled()
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            NSSize::new(metrics::SWITCH.0, metrics::SWITCH.1)
        }

        #[unsafe(method(sizeThatFits:))]
        fn size_that_fits(&self, _size: NSSize) -> NSSize {
            NSSize::new(metrics::SWITCH.0, metrics::SWITCH.1)
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, _sender: Option<&AnyObject>) {
            if self.control().isEnabled() {
                self.toggle();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if self.control().isEnabled() {
                track_switch(self, event);
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if !theme::paint::recording() {
                return;
            }
            let ivars = self.ivars();
            // Mixed draws as off.
            let position = ivars.dragged.get().unwrap_or(ivars.state.get().max(0) as f64);
            let state = parts::State { disabled: !self.control().isEnabled(), pressed: ivars.pressed.get(), ..Default::default() };
            parts::switch(theme::palette(), self.view().bounds(), position, state);
        }
    }

    unsafe impl NSObjectProtocol for NSSwitchImpl {}
);

impl NSSwitchImpl {
    fn control(&self) -> &NSControl {
        // SAFETY: NSSwitch is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn view(&self) -> &NSView {
        self.control()
    }

    /// Show `state`, redrawing if it changed.
    fn show_state(&self, state: isize) {
        if self.ivars().state.replace(state) != state {
            self.view().setNeedsDisplay(true);
        }
    }

    /// The state from the value just set: its integer's sign.
    fn follow_value(&self) {
        let value = self.control().integerValue();
        self.show_state(value.signum());
    }

    /// Turn over and say so.
    fn toggle(&self) {
        let control = self.control();
        let next = isize::from(self.ivars().state.get() != 1);
        // SAFETY: setState: takes the state.
        let _: () = unsafe { msg_send![self, setState: next] };
        // SAFETY: the switch's own action and target.
        let _ = unsafe { control.sendAction_to(control.action(), control.target().as_deref()) };
    }
}

/// Turn a switch over as the Space key does.
pub(crate) fn toggle(view: &NSView) -> bool {
    // SAFETY: NSSwitchImpl is the class NSSwitch names.
    let Some(switch) = (unsafe { super::impl_of::<objc2_app_kit::NSSwitch, NSSwitchImpl>(view) }) else {
        return false;
    };
    if switch.control().isEnabled() {
        switch.toggle();
    }
    true
}

fn track_switch(switch: &NSSwitchImpl, event: &NSEvent) {
    let view = switch.view();
    let mtm = MainThreadMarker::from(view);
    let bounds = view.bounds();
    let start = control::event_point(view, event);
    let start_state = switch.ivars().state.get().max(0) as f64;
    // The knob's travel: the track less the knob, as the painter draws it.
    let travel = (bounds.size.width - bounds.size.height).max(1.0);
    let mut moved = false;
    switch.ivars().pressed.set(true);
    view.setNeedsDisplay(true);
    while let Some(next) = track::next_event(mtm, NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged, None) {
        let at = control::event_point(view, &next);
        let dx = at.x - start.x;
        if next.r#type() == NSEventType::LeftMouseUp {
            switch.ivars().pressed.set(false);
            let position = switch.ivars().dragged.take();
            match position {
                Some(p) if moved => {
                    let on = isize::from(p >= 0.5);
                    if on != switch.ivars().state.get() {
                        switch.toggle();
                    }
                }
                _ if track::mouse_in_rect(at, bounds, view.isFlipped()) => switch.toggle(),
                _ => {}
            }
            view.setNeedsDisplay(true);
            return;
        }
        if dx.abs() > 3.0 {
            moved = true;
        }
        if moved {
            switch.ivars().dragged.set(Some((start_state + dx / travel).clamp(0.0, 1.0)));
            view.setNeedsDisplay(true);
        }
    }
    switch.ivars().pressed.set(false);
    switch.ivars().dragged.set(None);
    view.setNeedsDisplay(true);
}
