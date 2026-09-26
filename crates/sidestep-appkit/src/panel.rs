//! `NSPanel`: a window for auxiliary controls, as far as Wayland allows.
//!
//! As on macOS (`conformance/tests/appkit_events.rs`): a panel is never
//! main, isn't released when closed, hides when the application stops being
//! active, and floats when it has the utility style. Wayland has no window
//! levels, so a floating panel is shown over the main window, which
//! compositors keep it above. Hiding on deactivation happens only while
//! some other window of the program stays on screen: on a desktop without a
//! dock, a program whose every window hid couldn't be brought back. A panel
//! that works when modal takes input during modal loops. One that becomes
//! key only if needed doesn't take key status when the compositor gives it
//! the keyboard (keys go on to the key window, as a borderless window's do)
//! until a click lands in a view that needs it to
//! (`needsPanelToBecomeKey`).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::NSObjectProtocol;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSFloatingWindowLevel, NSNormalWindowLevel, NSPanel, NSResponder, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::NSRect;

pub(crate) struct PanelIvars {
    floating: Cell<bool>,
    key_only_if_needed: Cell<bool>,
    works_when_modal: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSWindow, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSPanel"]
    #[ivars = PanelIvars]
    pub(crate) struct NSPanelImpl;

    impl NSPanelImpl {
        #[unsafe(method_id(initWithContentRect:styleMask:backing:defer:))]
        fn init(
            this: Allocated<Self>,
            rect: NSRect,
            style: NSWindowStyleMask,
            backing: NSBackingStoreType,
            defer: bool,
        ) -> Retained<Self> {
            let this = this.set_ivars(PanelIvars {
                floating: Cell::new(false),
                key_only_if_needed: Cell::new(false),
                works_when_modal: Cell::new(false),
            });
            // SAFETY: NSWindow's designated initializer.
            let this: Retained<Self> =
                unsafe { msg_send![super(this), initWithContentRect: rect, styleMask: style, backing: backing, defer: defer] };
            let window: &NSWindow = &this;
            window.setHidesOnDeactivate(true);
            // SAFETY: a panel's owner keeps it, so it isn't released on
            // close, as on macOS.
            unsafe { window.setReleasedWhenClosed(false) };
            if style.contains(NSWindowStyleMask::UtilityWindow) {
                set_floating(&this, true);
            }
            this
        }

        #[unsafe(method(isFloatingPanel))]
        fn is_floating_panel(&self) -> bool {
            self.ivars().floating.get()
        }

        #[unsafe(method(setFloatingPanel:))]
        fn set_floating_panel(&self, flag: bool) {
            set_floating(self, flag);
        }

        #[unsafe(method(becomesKeyOnlyIfNeeded))]
        fn becomes_key_only_if_needed(&self) -> bool {
            self.ivars().key_only_if_needed.get()
        }

        #[unsafe(method(setBecomesKeyOnlyIfNeeded:))]
        fn set_becomes_key_only_if_needed(&self, flag: bool) {
            self.ivars().key_only_if_needed.set(flag);
        }

        #[unsafe(method(worksWhenModal))]
        fn works_when_modal(&self) -> bool {
            self.ivars().works_when_modal.get()
        }

        #[unsafe(method(setWorksWhenModal:))]
        fn set_works_when_modal(&self, flag: bool) {
            self.ivars().works_when_modal.set(flag);
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for NSPanelImpl {}
);

fn set_floating(panel: &NSPanelImpl, flag: bool) {
    panel.ivars().floating.set(flag);
    let window: &NSWindow = panel;
    window.setLevel(if flag { NSFloatingWindowLevel } else { NSNormalWindowLevel });
}

fn as_panel(window: &NSWindow) -> Option<&NSPanelImpl> {
    window.downcast_ref::<NSPanel>().map(|p| {
        // SAFETY: every NSPanel is an NSPanelImpl.
        unsafe { &*(p as *const NSPanel).cast::<NSPanelImpl>() }
    })
}

/// A floating panel about to be shown goes over the main window.
pub(crate) fn showing(window: &NSWindow) {
    let Some(panel) = as_panel(window) else { return };
    let imp = crate::window::imp(window);
    if panel.ivars().floating.get() && imp.transient().is_none() {
        let main = crate::app::main_window().filter(|m| !std::ptr::eq(&**m, window));
        if let Some(main) = main {
            imp.set_transient(Some(&main));
        }
    }
}

/// Whether the compositor giving `window` the keyboard makes it key: not
/// for a panel that becomes key only when needed.
pub(crate) fn takes_key_on_focus(window: &NSWindow) -> bool {
    as_panel(window).is_none_or(|p| !p.ivars().key_only_if_needed.get())
}

/// A click in `view` of a panel that becomes key only when needed makes it
/// key if the view needs it to.
pub(crate) fn clicked(window: &NSWindow, view: &NSView) {
    if window.isKeyWindow() || takes_key_on_focus(window) || !window.canBecomeKeyWindow() {
        return;
    }
    if view.needsPanelToBecomeKey() {
        crate::app::make_key(window);
    }
}

thread_local! {
    /// Windows hidden when the application stopped being active, to show
    /// again when it is.
    static HIDDEN: RefCell<Vec<Weak<NSWindow>>> = const { RefCell::new(Vec::new()) };
}

/// The application became active or stopped being active: windows that
/// hide on deactivation hide or come back.
pub(crate) fn activity_changed(active: bool, windows: &[Retained<NSWindow>]) {
    if active {
        let hidden = HIDDEN.with(|h| std::mem::take(&mut *h.borrow_mut()));
        for window in hidden.iter().filter_map(Weak::load) {
            window.orderFront(None);
        }
        return;
    }
    let (hiding, staying): (Vec<_>, Vec<_>) = windows.iter().partition(|w| w.hidesOnDeactivate());
    if hiding.is_empty() || staying.is_empty() {
        return;
    }
    for window in hiding {
        window.orderOut(None);
        HIDDEN.with(|h| h.borrow_mut().push(Weak::new(window)));
    }
}
