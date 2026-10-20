//! Mouse-driven pan recognition, with weak view/target references.
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSGestureRecognizer, NSView};
use objc2_foundation::NSPoint;
use std::cell::{Cell, RefCell};
sidestep_runtime::static_class!(pub(crate) GESTURE, GESTURE_META = "NSGestureRecognizer", || { let _ = Gesture::class(); });
sidestep_runtime::static_class!(pub(crate) PAN, PAN_META = "NSPanGestureRecognizer", || { let _ = Pan::class(); });
struct State {
    target: RefCell<Option<Weak<AnyObject>>>,
    action: Cell<Option<Sel>>,
    view: RefCell<Option<Weak<NSView>>>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    enabled: Cell<bool>,
    state: Cell<isize>,
    start: Cell<NSPoint>,
    current: Cell<NSPoint>,
    previous: Cell<NSPoint>,
    time: Cell<f64>,
    velocity: Cell<NSPoint>,
    translation: Cell<NSPoint>,
    button_mask: Cell<usize>,
    delays: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSGestureRecognizer"]
    #[ivars = State]
    struct Gesture;
    impl Gesture {
        #[unsafe(method_id(initWithTarget:action:))]
        fn init(this: Allocated<Self>, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<Self> {
            let this=this.set_ivars(State { target: RefCell::new(target.map(Weak::new)), action: Cell::new(action), view: RefCell::new(None), delegate: RefCell::new(None), enabled: Cell::new(true), state: Cell::new(0), start: Cell::new(NSPoint::ZERO), current: Cell::new(NSPoint::ZERO), previous: Cell::new(NSPoint::ZERO), time: Cell::new(0.0), velocity: Cell::new(NSPoint::ZERO), translation: Cell::new(NSPoint::ZERO), button_mask: Cell::new(1), delays: Cell::new(false) }); unsafe { msg_send![super(this), init] }
        }
        #[unsafe(method_id(init))]
        fn init_default(this: Allocated<Self>) -> Retained<Self> { unsafe { msg_send![this, initWithTarget: None::<&AnyObject>, action: None::<Sel>] } }
        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> { self.ivars().target.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) { self.ivars().target.replace(target.map(Weak::new)); }
        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> { self.ivars().action.get() }
        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) { self.ivars().action.set(action); }
        #[unsafe(method_id(view))]
        fn view(&self) -> Option<Retained<NSView>> { self.ivars().view.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> { self.ivars().delegate.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) { self.ivars().delegate.replace(delegate.map(Weak::new)); }
        #[unsafe(method(isEnabled))]
        fn enabled(&self) -> bool { self.ivars().enabled.get() }
        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, value: bool) { self.ivars().enabled.set(value); if !value && matches!(self.ivars().state.get(),1|2) { transition(self,4); } }
        #[unsafe(method(state))]
        fn state(&self) -> isize { self.ivars().state.get() }
        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) { transition(self,state); }
        #[unsafe(method(locationInView:))]
        fn location(&self, view: Option<&NSView>) -> NSPoint { convert(self.ivars().current.get(),view) }
        #[unsafe(method(reset))]
        fn reset(&self) { self.ivars().state.set(0); self.ivars().translation.set(NSPoint::ZERO); self.ivars().velocity.set(NSPoint::ZERO); }
        #[unsafe(method(delaysPrimaryMouseButtonEvents))]
        fn delays(&self) -> bool { self.ivars().delays.get() }
        #[unsafe(method(setDelaysPrimaryMouseButtonEvents:))]
        fn set_delays(&self, value: bool) { self.ivars().delays.set(value); }
    }
    unsafe impl NSObjectProtocol for Gesture {}
);
define_class!(
    #[unsafe(super(NSGestureRecognizer, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSPanGestureRecognizer"]
    struct Pan;
    impl Pan {
        #[unsafe(method(translationInView:))]
        fn translation(&self, view: Option<&NSView>) -> NSPoint { vector(imp(self).ivars().translation.get(), view) }
        #[unsafe(method(setTranslation:inView:))]
        fn set_translation(&self, translation: NSPoint, view: Option<&NSView>) { let translation=if let Some(view)=view { let a=view.convertPoint_toView(translation,None); let b=view.convertPoint_toView(NSPoint::ZERO,None); NSPoint::new(a.x-b.x,a.y-b.y) } else { translation }; imp(self).ivars().translation.set(translation); }
        #[unsafe(method(velocityInView:))]
        fn velocity(&self, view: Option<&NSView>) -> NSPoint { vector(imp(self).ivars().velocity.get(),view) }
        #[unsafe(method(buttonMask))]
        fn buttons(&self) -> usize { imp(self).ivars().button_mask.get() }
        #[unsafe(method(setButtonMask:))]
        fn set_buttons(&self, mask: usize) { imp(self).ivars().button_mask.set(mask); }
    }
    unsafe impl NSObjectProtocol for Pan {}
);
fn imp(gesture: &NSGestureRecognizer) -> &Gesture {
    unsafe { &*(gesture as *const NSGestureRecognizer).cast() }
}
fn convert(point: NSPoint, view: Option<&NSView>) -> NSPoint {
    view.map_or(point, |v| v.convertPoint_fromView(point, None))
}
fn vector(point: NSPoint, view: Option<&NSView>) -> NSPoint {
    let a = convert(point, view);
    let b = convert(NSPoint::ZERO, view);
    NSPoint::new(a.x - b.x, a.y - b.y)
}
pub(crate) fn attach(gesture: &NSGestureRecognizer, view: Option<&NSView>) {
    imp(gesture).ivars().view.replace(view.map(Weak::new));
}
fn transition(gesture: &Gesture, state: isize) {
    gesture.ivars().state.set(state);
    if state == 0 || state == 5 {
        return;
    }
    let target = gesture.ivars().target.borrow().as_ref().and_then(Weak::load);
    let action = gesture.ivars().action.get();
    if let Some(action) = action {
        unsafe {
            NSApplication::sharedApplication(gesture.mtm()).sendAction_to_from(action, target.as_deref(), Some(gesture))
        };
    }
}
pub(crate) fn event(view: &NSView, event: &NSEvent) {
    let mut current = Some(view.retain());
    while let Some(view) = current {
        for gesture in view.gestureRecognizers() {
            if !gesture.isKindOfClass(objc2_app_kit::NSPanGestureRecognizer::class()) {
                continue;
            }
            let g = imp(&gesture);
            let s = g.ivars();
            if !s.enabled.get() {
                continue;
            }
            if s.button_mask.get() & (1usize << event.buttonNumber().clamp(0, 31)) == 0 {
                continue;
            }
            let point = event.locationInWindow();
            let previous = s.current.replace(point);
            s.previous.set(previous);
            let time = event.timestamp();
            let elapsed = time - s.time.replace(time);
            if matches!(
                event.r#type(),
                NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown
            ) {
                s.state.set(0);
                s.start.set(point);
                s.translation.set(NSPoint::ZERO);
                s.velocity.set(NSPoint::ZERO);
            } else if matches!(
                event.r#type(),
                NSEventType::LeftMouseDragged | NSEventType::RightMouseDragged | NSEventType::OtherMouseDragged
            ) && matches!(s.state.get(), 0..=2)
            {
                let old = s.translation.get();
                s.translation.set(NSPoint::new(old.x + point.x - previous.x, old.y + point.y - previous.y));
                if elapsed > 0.0 {
                    s.velocity.set(NSPoint::new((point.x - previous.x) / elapsed, (point.y - previous.y) / elapsed));
                }
                if s.state.get() == 0 {
                    let start = s.start.get();
                    if (point.x - start.x).hypot(point.y - start.y) >= 3.0 {
                        transition(g, 1);
                    }
                } else {
                    transition(g, 2);
                }
            } else if matches!(
                event.r#type(),
                NSEventType::LeftMouseUp | NSEventType::RightMouseUp | NSEventType::OtherMouseUp
            ) {
                if matches!(s.state.get(), 1 | 2) {
                    transition(g, 3);
                } else {
                    s.state.set(5);
                }
            }
        }
        current = unsafe { view.superview() };
    }
}
