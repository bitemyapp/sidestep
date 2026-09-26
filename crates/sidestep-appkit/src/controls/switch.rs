//! `NSSwitch`: an on/off toggle. Unlike most controls it has no cell: the
//! state is the control's own (`conformance/tests/controls.rs`,
//! `steppers_sliders_switches`), and it is 54 by 24 points at every
//! control size.
//!
//! A click toggles it and sends the action. Dragging moves the knob with
//! the mouse; releasing turns it to whichever side the knob is nearer,
//! sending the action if that changed the state.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSControl, NSEvent, NSEventMask, NSEventType, NSResponder, NSView};
use objc2_foundation::{NSRect, NSSize};

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

        #[unsafe(method(state))]
        fn state(&self) -> isize {
            self.ivars().state.get()
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            let state = isize::from(state != 0);
            if self.ivars().state.replace(state) != state {
                self.view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            self.ivars().state.get() as i32
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            self.ivars().state.get()
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.ivars().state.get() as f64
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
            let position = ivars.dragged.get().unwrap_or(ivars.state.get() as f64);
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

    /// Turn over and say so.
    fn toggle(&self) {
        let control = self.control();
        let next = 1 - self.ivars().state.get();
        // SAFETY: setState: takes the state.
        let _: () = unsafe { msg_send![self, setState: next] };
        // SAFETY: the switch's own action and target.
        let _ = unsafe { control.sendAction_to(control.action(), control.target().as_deref()) };
    }
}

/// Turn a switch over as the Space key does.
pub(crate) fn toggle(view: &NSView) -> bool {
    let Some(control) = control::as_control(view) else { return false };
    let target = <objc2_app_kit::NSSwitch as objc2::ClassType>::class();
    let mut class = Some((view as &AnyObject).class());
    while let Some(c) = class {
        if std::ptr::eq(c, target) {
            // SAFETY: the view's class descends from NSSwitch.
            let switch = unsafe { &*(view as *const NSView).cast::<NSSwitchImpl>() };
            if control.as_control().isEnabled() {
                switch.toggle();
            }
            return true;
        }
        class = c.superclass();
    }
    false
}

fn track_switch(switch: &NSSwitchImpl, event: &NSEvent) {
    let view = switch.view();
    let mtm = MainThreadMarker::from(view);
    let bounds = view.bounds();
    let start = control::event_point(view, event);
    let start_state = switch.ivars().state.get() as f64;
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
