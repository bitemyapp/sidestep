//! `NSAlert`: a message, its buttons, and the panel that shows them.
//!
//! As on macOS (`conformance/tests/alert.rs`): a new alert has an OK button
//! until the program adds its own; buttons are push buttons tagged 1000,
//! 1001… in order, acting through the alert. The first answers Return, a
//! button titled Cancel answers Escape (even first) and one titled Don't
//! Save answers Command-D. The alert is a warning with empty texts, the
//! suppression check box is there but hidden, and `window` is a panel made
//! at once, not on screen.
//!
//! The panel is laid out (`layout`, and before it shows) the way GNOME's
//! message dialogs are, which macOS's narrow alerts resemble: the icon
//! when the program set one, the message in bold and the informative text
//! centered under it, the accessory view, the suppression check box, then
//! the buttons along the bottom, the first on the right; with three or more
//! they stack, the first on top and Cancel at the bottom. The accessory
//! view's first view that takes the keyboard has it, else the first
//! button.
//!
//! `runModal` runs the application's modal loop for the panel (over the
//! key window) and returns the tag of the button clicked;
//! `beginSheetModalForWindow:completionHandler:` attaches the panel to the
//! window as a sheet and calls the handler with the tag, keeping the alert
//! alive until then.

use std::cell::{Cell, RefCell};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSEventModifierFlags, NSFont,
    NSImage, NSModalResponse, NSPanel, NSResponder, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSCopying, NSError, NSPoint, NSRect, NSSize, NSString};

use crate::controls::cell::Styled;
use crate::text::layout::{Align, Attrs, LineBreak};
use crate::theme;

sidestep_runtime::static_class!(pub NSALERT, NSALERT_META = "NSAlert", || {
    let _ = NSAlertImpl::class();
});

/// The panel's width without an accessory view wider than this allows.
const WIDTH: f64 = 320.0;
const MARGIN: f64 = 24.0;
const ICON: f64 = 48.0;
const BUTTON_HEIGHT: f64 = 28.0;
const BUTTON_GAP: f64 = 8.0;
const MESSAGE_SIZE: f64 = 15.0;

pub(crate) struct AlertIvars {
    message: RefCell<Retained<NSString>>,
    informative: RefCell<Retained<NSString>>,
    icon: RefCell<Option<Retained<NSImage>>>,
    /// The icon the program set; the panel shows only that one.
    icon_set: Cell<bool>,
    buttons: RefCell<Vec<Retained<NSButton>>>,
    /// The OK button a new alert has until the program adds one.
    implicit: RefCell<Option<Retained<NSButton>>>,
    style: Cell<NSAlertStyle>,
    shows_help: Cell<bool>,
    help_anchor: RefCell<Option<Retained<NSString>>>,
    delegate: RefCell<Weak<AnyObject>>,
    accessory: RefCell<Option<Retained<NSView>>>,
    shows_suppression: Cell<bool>,
    suppression: RefCell<Option<Retained<NSButton>>>,
    help: RefCell<Option<Retained<NSButton>>>,
    panel: RefCell<Option<Retained<NSWindow>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSAlert"]
    #[ivars = AlertIvars]
    pub(crate) struct NSAlertImpl;

    impl NSAlertImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AlertIvars {
                message: RefCell::new(NSString::new()),
                informative: RefCell::new(NSString::new()),
                icon: RefCell::new(None),
                icon_set: Cell::new(false),
                buttons: RefCell::new(Vec::new()),
                implicit: RefCell::new(None),
                style: Cell::new(NSAlertStyle::Warning),
                shows_help: Cell::new(false),
                help_anchor: RefCell::new(None),
                delegate: RefCell::new(Weak::default()),
                accessory: RefCell::new(None),
                shows_suppression: Cell::new(false),
                suppression: RefCell::new(None),
                help: RefCell::new(None),
                panel: RefCell::new(None),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        /// The error's description, recovery suggestion and recovery
        /// options.
        #[unsafe(method_id(alertWithError:))]
        fn alert_with_error(error: &NSError) -> Retained<NSAlert> {
            let mtm = MainThreadMarker::new().expect("sidestep: alerts belong to the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&error.localizedDescription());
            if let Some(suggestion) = error.localizedRecoverySuggestion() {
                alert.setInformativeText(&suggestion);
            }
            if let Some(options) = error.localizedRecoveryOptions() {
                for option in options.iter() {
                    alert.addButtonWithTitle(&option);
                }
            }
            alert
        }

        #[unsafe(method_id(messageText))]
        fn message_text(&self) -> Retained<NSString> {
            self.ivars().message.borrow().clone()
        }

        #[unsafe(method(setMessageText:))]
        fn set_message_text(&self, text: &NSString) {
            let old = self.ivars().message.replace(text.copy());
            drop(old);
        }

        #[unsafe(method_id(informativeText))]
        fn informative_text(&self) -> Retained<NSString> {
            self.ivars().informative.borrow().clone()
        }

        #[unsafe(method(setInformativeText:))]
        fn set_informative_text(&self, text: &NSString) {
            let old = self.ivars().informative.replace(text.copy());
            drop(old);
        }

        /// The one set, else a symbol (programs have no icon of their own on
        /// Linux).
        #[unsafe(method_id(icon))]
        fn icon(&self) -> Option<Retained<NSImage>> {
            let set = self.ivars().icon.borrow().clone();
            set.or_else(|| {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("info.circle"), None)
            })
        }

        #[unsafe(method(setIcon:))]
        fn set_icon(&self, icon: Option<&NSImage>) {
            self.ivars().icon_set.set(icon.is_some());
            let old = self.ivars().icon.replace(icon.map(|i| i.retain()));
            drop(old);
        }

        /// Tagged 1000 on in order, with the key equivalents their titles
        /// call for, acting through the alert.
        #[unsafe(method_id(addButtonWithTitle:))]
        fn add_button_with_title(&self, title: &NSString) -> Retained<NSButton> {
            let index = self.ivars().buttons.borrow().len();
            let button = make_button(self, title, index);
            self.ivars().buttons.borrow_mut().push(button.clone());
            let implicit = self.ivars().implicit.take();
            drop(implicit);
            button
        }

        #[unsafe(method_id(buttons))]
        fn buttons(&self) -> Retained<NSArray<NSButton>> {
            NSArray::from_retained_slice(&shown_buttons(self))
        }

        #[unsafe(method(alertStyle))]
        fn alert_style(&self) -> NSAlertStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setAlertStyle:))]
        fn set_alert_style(&self, style: NSAlertStyle) {
            self.ivars().style.set(style);
        }

        #[unsafe(method(showsHelp))]
        fn shows_help(&self) -> bool {
            self.ivars().shows_help.get()
        }

        #[unsafe(method(setShowsHelp:))]
        fn set_shows_help(&self, flag: bool) {
            self.ivars().shows_help.set(flag);
        }

        #[unsafe(method_id(helpAnchor))]
        fn help_anchor(&self) -> Option<Retained<NSString>> {
            self.ivars().help_anchor.borrow().clone()
        }

        #[unsafe(method(setHelpAnchor:))]
        fn set_help_anchor(&self, anchor: Option<&NSString>) {
            let old = self.ivars().help_anchor.replace(anchor.map(|a| a.copy()));
            drop(old);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            let old = self.ivars().delegate.replace(delegate.map_or_else(Weak::default, Weak::new));
            drop(old);
        }

        #[unsafe(method_id(accessoryView))]
        fn accessory_view(&self) -> Option<Retained<NSView>> {
            self.ivars().accessory.borrow().clone()
        }

        #[unsafe(method(setAccessoryView:))]
        fn set_accessory_view(&self, view: Option<&NSView>) {
            let old = self.ivars().accessory.replace(view.map(|v| v.retain()));
            if let Some(old) = &old
                && view.is_none_or(|v| !std::ptr::eq(&**old, v))
            {
                old.removeFromSuperview();
            }
            drop(old);
        }

        #[unsafe(method(showsSuppressionButton))]
        fn shows_suppression_button(&self) -> bool {
            self.ivars().shows_suppression.get()
        }

        #[unsafe(method(setShowsSuppressionButton:))]
        fn set_shows_suppression_button(&self, flag: bool) {
            self.ivars().shows_suppression.set(flag);
        }

        /// A check box, off, there whether it shows or not.
        #[unsafe(method_id(suppressionButton))]
        fn suppression_button(&self) -> Option<Retained<NSButton>> {
            Some(suppression(self))
        }

        #[unsafe(method_id(window))]
        fn window(&self) -> Retained<NSWindow> {
            panel(self)
        }

        #[unsafe(method(layout))]
        fn layout(&self) {
            lay_out(self);
        }

        #[unsafe(method(runModal))]
        fn run_modal(&self) -> NSModalResponse {
            run_modal(self)
        }

        #[unsafe(method(beginSheetModalForWindow:completionHandler:))]
        fn begin_sheet_modal_for_window(&self, window: &NSWindow, handler: Option<&DynBlock<dyn Fn(NSModalResponse)>>) {
            let handler = handler.map(|h| h.copy());
            let alert = self.retain();
            begin_sheet(self, window, move |code| {
                if let Some(h) = &handler {
                    h.call((code,));
                }
                let _ = &alert;
            });
        }

        /// The old form: `didEndSelector` gets the alert, the code and the
        /// context.
        #[unsafe(method(beginSheetModalForWindow:modalDelegate:didEndSelector:contextInfo:))]
        fn begin_sheet_modal_delegate(
            &self,
            window: &NSWindow,
            delegate: Option<&AnyObject>,
            selector: Option<Sel>,
            context: *mut std::ffi::c_void,
        ) {
            let alert = self.retain();
            let delegate = delegate.map(|d| d.retain());
            let context = context as usize;
            begin_sheet(self, window, move |code| {
                if let (Some(d), Some(sel)) = (&delegate, selector) {
                    // SAFETY: the selector takes the alert, the code and the
                    // context, as the delegate promised.
                    unsafe {
                        objc2::runtime::MessageReceiver::send_message::<_, ()>(
                            &**d,
                            sel,
                            (&*alert as &AnyObject, code, context as *mut std::ffi::c_void),
                        )
                    };
                }
            });
        }

        /// A button's action: end the modal loop or the sheet with its tag.
        #[unsafe(method(buttonPressed:))]
        fn button_pressed(&self, sender: Option<&AnyObject>) {
            let code = sender.and_then(|s| s.downcast_ref::<NSButton>()).map_or(1000, |b| b.tag());
            finish(self, code);
        }

        /// The help button's action: the delegate's `alertShowHelp:`.
        #[unsafe(method(helpPressed:))]
        fn help_pressed(&self, _sender: Option<&AnyObject>) {
            if let Some(d) = self.ivars().delegate.borrow().load()
                && responds(&d, sel!(alertShowHelp:))
            {
                // SAFETY: alertShowHelp: takes the alert and returns BOOL.
                let _: bool = unsafe { msg_send![&*d, alertShowHelp: self] };
            }
        }
    }

    unsafe impl NSObjectProtocol for NSAlertImpl {}
);

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

fn as_alert(alert: &NSAlertImpl) -> &NSAlert {
    // SAFETY: NSAlertImpl is the class NSAlert names.
    unsafe { &*(alert as *const NSAlertImpl).cast::<NSAlert>() }
}

/// A push button titled `title`, the `index`th, tagged and keyed as an
/// alert's buttons are.
fn make_button(alert: &NSAlertImpl, title: &NSString, index: usize) -> Retained<NSButton> {
    let mtm = MainThreadMarker::from(alert);
    // SAFETY: the button keeps its target weakly; the alert has the action.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(title, Some(as_alert(alert)), Some(sel!(buttonPressed:)), mtm)
    };
    button.setBezelStyle(NSBezelStyle::Push);
    button.setTag(1000 + index as isize);
    let text = title.to_string();
    let (key, mask) = if text == "Cancel" {
        ("\u{1b}", NSEventModifierFlags::empty())
    } else if text == "Don't Save" || text == "Don\u{2019}t Save" {
        ("d", NSEventModifierFlags::Command)
    } else if index == 0 {
        ("\r", NSEventModifierFlags::empty())
    } else {
        ("", NSEventModifierFlags::empty())
    };
    button.setKeyEquivalent(&NSString::from_str(key));
    button.setKeyEquivalentModifierMask(mask);
    button
}

/// The buttons the alert shows: the program's, or the implicit OK.
fn shown_buttons(alert: &NSAlertImpl) -> Vec<Retained<NSButton>> {
    let buttons = alert.ivars().buttons.borrow().clone();
    if !buttons.is_empty() {
        return buttons;
    }
    let implicit = alert.ivars().implicit.borrow().clone();
    let ok = implicit.unwrap_or_else(|| {
        let ok = make_button(alert, &NSString::from_str("OK"), 0);
        alert.ivars().implicit.replace(Some(ok.clone()));
        ok
    });
    vec![ok]
}

fn suppression(alert: &NSAlertImpl) -> Retained<NSButton> {
    let made = alert.ivars().suppression.borrow().clone();
    made.unwrap_or_else(|| {
        let mtm = MainThreadMarker::from(alert);
        let title = NSString::from_str("Do not show this message again");
        // SAFETY: no target or action.
        let b = unsafe { NSButton::checkboxWithTitle_target_action(&title, None, None, mtm) };
        alert.ivars().suppression.replace(Some(b.clone()));
        b
    })
}

/// The panel, made the first time it's asked for.
fn panel(alert: &NSAlertImpl) -> Retained<NSWindow> {
    let made = alert.ivars().panel.borrow().clone();
    if let Some(p) = made {
        return p;
    }
    let mtm = MainThreadMarker::from(alert);
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 160.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Titled,
        NSBackingStoreType::Buffered,
        true,
    );
    panel.setHidesOnDeactivate(false);
    let window: Retained<NSWindow> = Retained::into_super(panel);
    let view = AlertView::new(mtm, alert);
    window.setContentView(Some(&view));
    alert.ivars().panel.replace(Some(window.clone()));
    window
}

// Layout and drawing.

fn text_attrs(size: f64, bold: bool, color: crate::protocol::Color) -> Attrs {
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    let mut a = Attrs::new(crate::font::text_font(&font));
    a.color = color;
    a.paragraph.alignment = Align::Center;
    a.paragraph.line_break = LineBreak::WordWrap;
    a
}

fn message_attrs() -> Attrs {
    text_attrs(MESSAGE_SIZE, true, theme::palette().label)
}

fn informative_attrs() -> Attrs {
    text_attrs(0.0, false, theme::palette().label)
}

/// The height of `text` set in `attrs`, wrapped to `width`.
fn text_height(text: &NSString, attrs: &Attrs, width: f64) -> f64 {
    if text.length() == 0 {
        return 0.0;
    }
    Styled::plain(text.to_string(), attrs.clone()).size(Some(width)).height.ceil()
}

/// Where the texts go, from the top of the panel's content (flipped).
#[derive(Clone, Copy, Default)]
struct Places {
    icon: Option<NSRect>,
    message: NSRect,
    informative: NSRect,
}

/// Lay the panel out for what the alert holds now.
fn lay_out(alert: &NSAlertImpl) {
    let window = panel(alert);
    let Some(content) = window.contentView() else { return };
    let Some(view) = content.downcast_ref::<AlertView>() else { return };
    let ivars = alert.ivars();
    let accessory = ivars.accessory.borrow().clone();
    let width = accessory.as_ref().map_or(WIDTH, |a| WIDTH.max(a.frame().size.width + 2.0 * MARGIN)).ceil();
    let inner = width - 2.0 * MARGIN;
    let mut y = MARGIN;
    let mut places = Places::default();
    if ivars.icon_set.get() {
        places.icon = Some(NSRect::new(NSPoint::new((width - ICON) / 2.0, y), NSSize::new(ICON, ICON)));
        y += ICON + 12.0;
    }
    let message = ivars.message.borrow().clone();
    let h = text_height(&message, &message_attrs(), inner);
    places.message = NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(inner, h));
    y += h;
    let informative = ivars.informative.borrow().clone();
    let h = text_height(&informative, &informative_attrs(), inner);
    if h > 0.0 {
        y += 8.0;
    }
    places.informative = NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(inner, h));
    y += h;
    // Whatever the view held besides the alert's parts goes.
    for sub in content.subviews().iter() {
        sub.removeFromSuperview();
    }
    if let Some(accessory) = &accessory {
        y += 16.0;
        let size = accessory.frame().size;
        accessory.setFrameOrigin(NSPoint::new(((width - size.width) / 2.0).round(), y));
        content.addSubview(accessory);
        y += size.height;
    }
    if ivars.shows_suppression.get() {
        y += 12.0;
        let check = suppression(alert);
        check.sizeToFit();
        let size = check.frame().size;
        check.setFrameOrigin(NSPoint::new(MARGIN, y));
        content.addSubview(&check);
        y += size.height;
    }
    y += 20.0;
    // The buttons: a row, the first on the right; or, three and more, a
    // stack, the first on top and Cancel at the bottom.
    let mut buttons = shown_buttons(alert);
    if buttons.len() >= 3 {
        if let Some(cancel) = buttons.iter().position(|b| b.title().to_string() == "Cancel") {
            let b = buttons.remove(cancel);
            buttons.push(b);
        }
        for b in &buttons {
            b.setFrame(NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(inner, BUTTON_HEIGHT)));
            content.addSubview(b);
            y += BUTTON_HEIGHT + BUTTON_GAP;
        }
        y -= BUTTON_GAP;
    } else {
        let n = buttons.len() as f64;
        let each = ((inner - BUTTON_GAP * (n - 1.0)) / n).floor();
        for (i, b) in buttons.iter().enumerate() {
            // The first on the right.
            let slot = n - 1.0 - i as f64;
            let x = MARGIN + slot * (each + BUTTON_GAP);
            b.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(each, BUTTON_HEIGHT)));
            content.addSubview(b);
        }
        y += BUTTON_HEIGHT;
    }
    if ivars.shows_help.get() {
        let mtm = MainThreadMarker::from(alert);
        let made = ivars.help.borrow().clone();
        let help = made.unwrap_or_else(|| {
            // SAFETY: the alert has the action; the button keeps it weakly.
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::new(),
                    Some(as_alert(alert)),
                    Some(sel!(helpPressed:)),
                    mtm,
                )
            };
            b.setBezelStyle(NSBezelStyle::HelpButton);
            ivars.help.replace(Some(b.clone()));
            b
        });
        help.setFrame(NSRect::new(NSPoint::new(MARGIN / 2.0, MARGIN / 2.0), NSSize::new(24.0, 24.0)));
        content.addSubview(&help);
    }
    y += MARGIN;
    view.ivars().places.set(places);
    window.setContentSize(NSSize::new(width, y.ceil()));
    content.setNeedsDisplay(true);
    // The accessory's first view that takes the keyboard, else the first
    // button.
    let first = accessory
        .as_deref()
        .and_then(first_key_view)
        .or_else(|| shown_buttons(alert).into_iter().next().map(|b| Retained::into_super(Retained::into_super(b))));
    window.setInitialFirstResponder(first.as_deref());
}

/// `view` or its first descendant that takes the keyboard.
fn first_key_view(view: &NSView) -> Option<Retained<NSView>> {
    if view.acceptsFirstResponder() {
        return Some(view.retain());
    }
    view.subviews().iter().find_map(|s| first_key_view(&s))
}

pub(crate) struct ViewIvars {
    alert: Weak<NSAlertImpl>,
    places: Cell<Places>,
}

define_class!(
    // The panel's content: draws the icon and the texts; the accessory
    // view, the check box and the buttons are its subviews.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepAlertView"]
    #[ivars = ViewIvars]
    pub(crate) struct AlertView;

    impl AlertView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(alert) = self.ivars().alert.load() else { return };
            let p = theme::palette();
            theme::paint::fill_rect(self.bounds(), p.window);
            let places = self.ivars().places.get();
            if let (Some(r), Some(icon)) = (places.icon, alert.ivars().icon.borrow().clone()) {
                let op = objc2_app_kit::NSCompositingOperation::SourceOver;
                icon.drawInRect_fromRect_operation_fraction(r, NSRect::ZERO, op, 1.0);
            }
            let message = alert.ivars().message.borrow().clone();
            if message.length() > 0 {
                Styled::plain(message.to_string(), message_attrs()).draw(places.message);
            }
            let informative = alert.ivars().informative.borrow().clone();
            if informative.length() > 0 {
                Styled::plain(informative.to_string(), informative_attrs()).draw(places.informative);
            }
        }
    }
);

impl AlertView {
    fn new(mtm: MainThreadMarker, alert: &NSAlertImpl) -> Retained<AlertView> {
        crate::load_shell::<NSView>();
        let this = AlertView::alloc(mtm).set_ivars(ViewIvars { alert: Weak::from(alert), places: Cell::default() });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 160.0))] }
    }
}

// Running.

fn run_modal(alert: &NSAlertImpl) -> NSModalResponse {
    lay_out(alert);
    let window = panel(alert);
    if let Some(first) = window.initialFirstResponder() {
        window.makeFirstResponder(Some(&first));
    }
    let mtm = MainThreadMarker::from(alert);
    let code = NSApplication::sharedApplication(mtm).runModalForWindow(&window);
    window.orderOut(None);
    code
}

/// Attach the panel to `parent` as a sheet; `then` gets the tag when it
/// ends.
fn begin_sheet(alert: &NSAlertImpl, parent: &NSWindow, then: impl Fn(NSModalResponse) + 'static) {
    lay_out(alert);
    let window = panel(alert);
    if let Some(first) = window.initialFirstResponder() {
        window.makeFirstResponder(Some(&first));
    }
    let sheet = window.clone();
    let handler = RcBlock::new(move |code: NSModalResponse| {
        sheet.orderOut(None);
        then(code);
    });
    parent.beginSheet_completionHandler(&window, Some(&handler));
}

/// A button ended the alert with `code`: the sheet's parent ends the sheet,
/// or the modal loop stops.
fn finish(alert: &NSAlertImpl, code: NSModalResponse) {
    let window = panel(alert);
    if let Some(parent) = window.sheetParent() {
        parent.endSheet_returnCode(&window, code);
        return;
    }
    let mtm = MainThreadMarker::from(alert);
    let app = NSApplication::sharedApplication(mtm);
    if app.modalWindow().is_some_and(|m| std::ptr::eq(&*m, &*window)) {
        app.stopModalWithCode(code);
    }
}
