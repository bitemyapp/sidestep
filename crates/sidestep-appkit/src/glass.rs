//! Glass surfaces use the desktop palette and rounded clipping geometry. Optical
//! refraction is not emulated; content ownership and resizing follow NSView.
use objc2::rc::{Allocated, Retained};
use objc2::runtime::NSObject;
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSBezierPath, NSColor, NSResponder, NSView};
use objc2_foundation::{NSRect, NSSize};
use std::cell::{Cell, RefCell};
#[derive(Default)]
struct State {
    content: RefCell<Option<Retained<NSView>>>,
    style: Cell<isize>,
    radius: Cell<f64>,
    tint: RefCell<Option<Retained<NSColor>>>,
    spacing: Cell<f64>,
}
fn content(owner: &NSView, state: &State, value: Option<&NSView>) {
    let old = state.content.replace(value.map(|v| v.retain()));
    if let Some(old) = old {
        old.removeFromSuperview();
    }
    if let Some(view) = value {
        view.setFrame(owner.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        owner.addSubview(view);
    }
}
sidestep_runtime::static_class!(pub(crate) GLASS, GLASS_META = "NSGlassEffectView", || { let _ = Glass::class(); });
define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[name = "NSGlassEffectView"]
    #[ivars = State]
    struct Glass;
    impl Glass {
        #[unsafe(method_id(initWithFrame:))]
        fn init(this: Allocated<Self>, frame: NSRect) -> Retained<Self> { let this = this.set_ivars(State::default()); unsafe { msg_send![super(this), initWithFrame: frame] } }
        #[unsafe(method_id(contentView))]
        fn content(&self) -> Option<Retained<NSView>> { self.ivars().content.borrow().clone() }
        #[unsafe(method(setContentView:))]
        fn set_content(&self, view: Option<&NSView>) { content(self, self.ivars(), view); }
        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize(&self, old: NSSize) { let _: () = unsafe { msg_send![super(self), resizeSubviewsWithOldSize: old] }; let child = self.ivars().content.borrow().clone(); if let Some(view) = child { view.setFrame(self.bounds()); } }
        #[unsafe(method(style))]
        fn style(&self) -> isize { self.ivars().style.get() }
        #[unsafe(method(setStyle:))]
        fn set_style(&self, value: isize) { self.ivars().style.set(value); self.setNeedsDisplay(true); }
        #[unsafe(method(cornerRadius))]
        fn radius(&self) -> f64 { self.ivars().radius.get() }
        #[unsafe(method(setCornerRadius:))]
        fn set_radius(&self, value: f64) { self.ivars().radius.set(value); self.setNeedsDisplay(true); }
        #[unsafe(method_id(tintColor))]
        fn tint(&self) -> Option<Retained<NSColor>> { self.ivars().tint.borrow().clone() }
        #[unsafe(method(setTintColor:))]
        fn set_tint(&self, color: Option<&NSColor>) { self.ivars().tint.replace(color.map(|c| c.retain())); self.setNeedsDisplay(true); }
        #[unsafe(method(drawRect:))]
        fn draw(&self, _dirty: NSRect) {
            let color = self.ivars().tint.borrow().clone().unwrap_or_else(NSColor::windowBackgroundColor);
            let color = if self.ivars().style.get() == 1 { color.colorWithAlphaComponent(0.8) } else { color };
            color.setFill();
            let radius = self.ivars().radius.get(); NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), radius, radius).fill();
        }
    }
);
sidestep_runtime::static_class!(pub(crate) CONTAINER, CONTAINER_META = "NSGlassEffectContainerView", || { let _ = Container::class(); });
define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[name = "NSGlassEffectContainerView"]
    #[ivars = State]
    struct Container;
    impl Container {
        #[unsafe(method_id(initWithFrame:))]
        fn init(this: Allocated<Self>, frame: NSRect) -> Retained<Self> { let this = this.set_ivars(State::default()); unsafe { msg_send![super(this), initWithFrame: frame] } }
        #[unsafe(method_id(contentView))]
        fn content(&self) -> Option<Retained<NSView>> { self.ivars().content.borrow().clone() }
        #[unsafe(method(setContentView:))]
        fn set_content(&self, view: Option<&NSView>) { content(self, self.ivars(), view); }
        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize(&self, old: NSSize) { let _: () = unsafe { msg_send![super(self), resizeSubviewsWithOldSize: old] }; let child = self.ivars().content.borrow().clone(); if let Some(view) = child { view.setFrame(self.bounds()); } }
        #[unsafe(method(spacing))]
        fn spacing(&self) -> f64 { self.ivars().spacing.get() }
        #[unsafe(method(setSpacing:))]
        fn set_spacing(&self, value: f64) { self.ivars().spacing.set(value); self.setNeedsDisplay(true); }
    }
);
