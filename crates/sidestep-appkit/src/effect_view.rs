//! `NSVisualEffectView`: a view showing a material (a sidebar, a menu, a
//! title bar) behind its subviews.
//!
//! On macOS a material blurs and tints what's behind the window or the
//! view. Wayland has no blur a client can ask for yet, so here a material
//! is an opaque color of Sidestep's palette in the view's appearance: the
//! window's background for window-like materials, the view background for
//! menus, popovers and content, the page background for sidebars, and the
//! selection colors for `Selection`. Its settings are kept and read back
//! as AppKit's are.
// AppKit's first materials (Light, Dark, AppearanceBased) are deprecated,
// and still mean something.
#![allow(deprecated)]

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::NSObject;
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackgroundStyle, NSImage, NSResponder, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState,
};
use objc2_foundation::NSRect;

use crate::palette::{Look, System};

sidestep_runtime::static_class!(pub NSVISUALEFFECTVIEW, NSVISUALEFFECTVIEW_META = "NSVisualEffectView", || {
    let _ = NSVisualEffectViewImpl::class();
});

pub(crate) struct EffectIvars {
    material: Cell<NSVisualEffectMaterial>,
    blending: Cell<NSVisualEffectBlendingMode>,
    state: Cell<NSVisualEffectState>,
    emphasized: Cell<bool>,
    mask: RefCell<Option<Retained<NSImage>>>,
}

impl Default for EffectIvars {
    fn default() -> Self {
        EffectIvars {
            material: Cell::new(NSVisualEffectMaterial::AppearanceBased),
            blending: Cell::new(NSVisualEffectBlendingMode::BehindWindow),
            state: Cell::new(NSVisualEffectState::FollowsWindowActiveState),
            emphasized: Cell::new(false),
            mask: RefCell::new(None),
        }
    }
}

define_class!(
    // SAFETY: NSView has no subclassing requirements beyond its
    // initializer, which `initWithFrame:` calls.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSVisualEffectView"]
    #[ivars = EffectIvars]
    pub(crate) struct NSVisualEffectViewImpl;

    impl NSVisualEffectViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(EffectIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(material))]
        fn material(&self) -> NSVisualEffectMaterial {
            self.ivars().material.get()
        }

        #[unsafe(method(setMaterial:))]
        fn set_material(&self, material: NSVisualEffectMaterial) {
            if self.ivars().material.replace(material) != material {
                self.redraw();
            }
        }

        #[unsafe(method(blendingMode))]
        fn blending_mode(&self) -> NSVisualEffectBlendingMode {
            self.ivars().blending.get()
        }

        #[unsafe(method(setBlendingMode:))]
        fn set_blending_mode(&self, mode: NSVisualEffectBlendingMode) {
            self.ivars().blending.set(mode);
        }

        #[unsafe(method(state))]
        fn state(&self) -> NSVisualEffectState {
            self.ivars().state.get()
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: NSVisualEffectState) {
            self.ivars().state.set(state);
        }

        #[unsafe(method(isEmphasized))]
        fn is_emphasized(&self) -> bool {
            self.ivars().emphasized.get()
        }

        #[unsafe(method(setEmphasized:))]
        fn set_emphasized(&self, flag: bool) {
            if self.ivars().emphasized.replace(flag) != flag {
                self.redraw();
            }
        }

        #[unsafe(method_id(maskImage))]
        fn mask_image(&self) -> Option<Retained<NSImage>> {
            self.ivars().mask.borrow().clone()
        }

        #[unsafe(method(setMaskImage:))]
        fn set_mask_image(&self, image: Option<&NSImage>) {
            *self.ivars().mask.borrow_mut() = image.map(|i| i.retain());
            self.redraw();
        }

        /// Views on a selection draw emphasized content (white text).
        #[unsafe(method(interiorBackgroundStyle))]
        fn interior_background_style(&self) -> NSBackgroundStyle {
            if self.ivars().material.get() == NSVisualEffectMaterial::Selection && self.ivars().emphasized.get() {
                NSBackgroundStyle::Emphasized
            } else {
                NSBackgroundStyle::Normal
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let color = self.color();
            crate::context::with_state(|st| st.fill_rect(dirty, color, crate::protocol::Blend::Copy));
        }
    }
);

impl NSVisualEffectViewImpl {
    fn redraw(&self) {
        let view: &NSView = self;
        view.setNeedsDisplay(true);
    }

    /// The material's color in the appearance drawing is in.
    fn color(&self) -> crate::protocol::Color {
        use NSVisualEffectMaterial as M;
        let look = crate::appearance::current_look();
        let material = self.ivars().material.get();
        // The old Light and Dark materials hold their look whatever the
        // appearance.
        let look = match material {
            M::Light | M::MediumLight => Look::Light,
            M::Dark | M::UltraDark => Look::Dark,
            _ => look,
        };
        let system = match material {
            M::Sidebar | M::UnderPageBackground => System::UnderPageBackground,
            M::Menu | M::Popover | M::ToolTip | M::HUDWindow | M::Sheet | M::ContentBackground => {
                System::ControlBackground
            }
            M::Selection if self.ivars().emphasized.get() => System::SelectedContentBackground,
            M::Selection => System::UnemphasizedSelectedContentBackground,
            _ => System::WindowBackground,
        };
        let mut c = crate::palette::get(system, look);
        c[3] = 1.0;
        c
    }
}
