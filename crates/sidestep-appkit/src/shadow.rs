//! `NSShadow`: an offset, a blur radius and a color, which `set` puts in
//! the graphics state for everything drawn after it.
//!
//! As in AppKit, the offset is in the base coordinates of the window or
//! bitmap, not the view's: a positive height moves the shadow up on the
//! screen whether or not the view is flipped or transformed, and offset
//! and blur are points, so they grow with the display's scale. The
//! rasterizer (`raster::effects`) draws each op's shadow under it: its
//! coverage, blurred like a Gaussian whose deviation is half the blur
//! radius, moved and tinted.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSColor, NSShadow};
use objc2_foundation::{NSCopying, NSSize, NSZone};

use crate::protocol::ShadowSpec;

sidestep_runtime::static_class!(pub NSSHADOW, NSSHADOW_META = "NSShadow", || {
    let _ = NSShadowImpl::class();
});

pub(crate) struct ShadowIvars {
    offset: Cell<NSSize>,
    blur: Cell<f64>,
    color: RefCell<Option<Retained<NSColor>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a shadow is used by
    // one thread at a time.
    #[unsafe(super(NSObject))]
    #[name = "NSShadow"]
    #[ivars = ShadowIvars]
    pub(crate) struct NSShadowImpl;

    impl NSShadowImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // AppKit's default: black at a third.
            let color = NSColor::colorWithCalibratedRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0 / 3.0);
            let this = this.set_ivars(ShadowIvars {
                offset: Cell::new(NSSize::ZERO),
                blur: Cell::new(0.0),
                color: RefCell::new(Some(color)),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(shadowOffset))]
        fn shadow_offset(&self) -> NSSize {
            self.ivars().offset.get()
        }

        #[unsafe(method(setShadowOffset:))]
        fn set_shadow_offset(&self, offset: NSSize) {
            self.ivars().offset.set(offset);
        }

        #[unsafe(method(shadowBlurRadius))]
        fn shadow_blur_radius(&self) -> f64 {
            self.ivars().blur.get()
        }

        #[unsafe(method(setShadowBlurRadius:))]
        fn set_shadow_blur_radius(&self, radius: f64) {
            // Kept as given, as AppKit keeps it; below zero draws unblurred.
            self.ivars().blur.set(radius);
        }

        #[unsafe(method_id(shadowColor))]
        fn shadow_color(&self) -> Option<Retained<NSColor>> {
            self.ivars().color.borrow().clone()
        }

        #[unsafe(method(setShadowColor:))]
        fn set_shadow_color(&self, color: Option<&NSColor>) {
            *self.ivars().color.borrow_mut() = color.map(|c| c.retain());
        }

        #[unsafe(method(set))]
        fn set(&self) {
            // The color resolves now, in the drawing appearance of the
            // moment, as a color's `set` does.
            let color = self.ivars().color.borrow().clone();
            let color = color.map(|c| crate::color::resolve(&c)).filter(|c| c[3] > 0.0);
            let offset = self.ivars().offset.get();
            let spec = color.map(|color| {
                // Layer points run down; the offset's height runs up.
                Arc::new(ShadowSpec {
                    dx: offset.width as f32,
                    dy: -offset.height as f32,
                    blur: self.ivars().blur.get().max(0.0) as f32,
                    color,
                    only: false,
                })
            });
            crate::context::with_state(|st| st.gs.shadow = spec);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSShadow> {
            crate::load_shell::<NSShadow>();
            let copy = Self::alloc().set_ivars(ShadowIvars {
                offset: Cell::new(self.ivars().offset.get()),
                blur: Cell::new(self.ivars().blur.get()),
                color: RefCell::new(self.ivars().color.borrow().clone()),
            });
            // SAFETY: NSObject's designated initializer.
            let copy: Retained<Self> = unsafe { msg_send![super(copy), init] };
            // SAFETY: NSShadowImpl is the class NSShadow names.
            unsafe { Retained::cast_unchecked(copy) }
        }
    }

    unsafe impl NSObjectProtocol for NSShadowImpl {}

    unsafe impl NSCopying for NSShadowImpl {}
);
