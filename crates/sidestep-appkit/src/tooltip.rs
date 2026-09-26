//! Tooltips: `toolTip`, `addToolTipRect:owner:userData:` and their kin,
//! and a Rust API for controls with a tooltip per part.
//!
//! As on macOS (`conformance/tests/appkit_events.rs`), each tooltip is a
//! tracking area of its view (whole-view tooltips follow the visible rect),
//! listed in `trackingAreas`; its tag is the area's address, and the owner
//! of a rectangle's tooltip isn't retained (it is held weakly here, so one
//! that goes away shows nothing rather than crashing). The areas' owner is
//! one private object that hears the pointer enter, move and leave. After
//! the pointer rests in an area for `NSInitialToolTipDelay` milliseconds
//! (1000 unless the defaults say otherwise; a tenth of that right after
//! another tooltip hid), the tooltip shows below the pointer: a small
//! borderless window, an ungrabbed popup of the window under the pointer,
//! drawing its text with the text engine in the tooltip font. It hides when
//! the pointer leaves the area, at a click or a key, and when its window
//! leaves the screen. Nothing is allocated while the pointer moves inside
//! an area with its tooltip up.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSResponder, NSStringDrawing, NSStringNSExtendedStringDrawing, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer};

use crate::views;

/// Space around the text, in points.
const PAD_X: f64 = 6.0;
const PAD_Y: f64 = 3.0;
/// Widest a tooltip grows before its text wraps.
const MAX_WIDTH: f64 = 300.0;
/// How far below the pointer a tooltip opens (a cursor's height).
const BELOW_POINTER: f64 = 20.0;
/// After a tooltip hid, another shows this soon when the pointer moves on.
const WARM: Duration = Duration::from_millis(500);

/// What a tooltip area shows.
enum Source {
    /// The view's `toolTip`.
    Whole,
    /// A rectangle's owner's `view:stringForToolTip:point:userData:`, or
    /// its description.
    Owner { owner: Weak<AnyObject>, data: *mut c_void },
    /// Text a control gave through [`add_text_rect`].
    Text(Retained<NSString>),
}

struct Entry {
    view: Weak<NSView>,
    source: Source,
}

struct Shown {
    area: usize,
    tip: Retained<NSWindow>,
    over: Weak<NSWindow>,
}

/// A tooltip waiting for the pointer to rest, or showing.
struct Hover {
    area: usize,
    window: Weak<NSWindow>,
    /// Where the pointer is, in the window.
    at: NSPoint,
}

thread_local! {
    /// Every tooltip area, by address (which is its tag).
    static AREAS: RefCell<HashMap<usize, Entry>> = RefCell::new(HashMap::new());
    static OWNER: RefCell<Option<Retained<ToolTipOwner>>> = const { RefCell::new(None) };
    static HOVER: RefCell<Option<Hover>> = const { RefCell::new(None) };
    static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    /// The tooltip on screen, the area it is for and the window it is over.
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static LAST_HIDDEN: Cell<Option<Instant>> = const { Cell::new(None) };
}

define_class!(
    // Owns every tooltip area: hears the pointer come, move and go.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepToolTips"]
    struct ToolTipOwner;

    impl ToolTipOwner {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            entered(event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            moved(event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            if let Some(area) = event.trackingArea() {
                exited(Retained::as_ptr(&area) as usize);
            }
        }
    }

    unsafe impl NSObjectProtocol for ToolTipOwner {}
);

fn owner(mtm: MainThreadMarker) -> Retained<ToolTipOwner> {
    OWNER.with(|o| {
        o.borrow_mut()
            .get_or_insert_with(|| {
                // SAFETY: NSObject's designated initializer.
                unsafe { msg_send![super(ToolTipOwner::alloc(mtm).set_ivars(())), init] }
            })
            .clone()
    })
}

/// Add a tooltip area to `view` and return its tag.
fn add(view: &NSView, rect: NSRect, whole: bool, source: Source) -> isize {
    let owner = owner(view.mtm());
    let mut options = NSTrackingAreaOptions::MouseEnteredAndExited
        | NSTrackingAreaOptions::MouseMoved
        | NSTrackingAreaOptions::ActiveAlways;
    if whole {
        options |= NSTrackingAreaOptions::InVisibleRect;
    }
    crate::load_shell::<NSTrackingArea>();
    // SAFETY: the designated initializer; the owner outlives its areas.
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), rect, options, Some(&owner), None)
    };
    let tag = Retained::as_ptr(&area) as usize;
    AREAS.with(|a| {
        let mut areas = a.borrow_mut();
        // Forget tooltips of views that went away, now and then.
        if areas.len() >= 16 && areas.len().is_power_of_two() {
            areas.retain(|_, e| e.view.load().is_some());
        }
        areas.insert(tag, Entry { view: Weak::new(view), source });
    });
    crate::tracking::add_area(views::imp(view), &area);
    tag as isize
}

/// Take the tooltip area `tag` off `view`.
fn remove(view: &NSView, tag: usize) {
    let entry = AREAS.with(|a| a.borrow_mut().remove(&tag));
    let Some(entry) = entry else { return };
    drop(entry);
    let area = views::tracking(views::imp(view))
        .borrow()
        .areas()
        .iter()
        .find(|a| Retained::as_ptr(a) as usize == tag)
        .cloned();
    if let Some(area) = area {
        crate::tracking::remove_area(views::imp(view), &area);
    }
    if HOVER.with(|h| h.borrow().as_ref().is_some_and(|h| h.area == tag)) {
        exited(tag);
    }
}

/// The tags of `view`'s tooltips, whole-view ones or not.
fn tags_of(view: &NSView, whole: Option<bool>) -> Vec<usize> {
    AREAS.with(|a| {
        a.borrow()
            .iter()
            .filter(|(_, e)| e.view.load().is_some_and(|v| std::ptr::eq(&*v, view)))
            .filter(|(_, e)| whole.is_none_or(|w| w == matches!(e.source, Source::Whole)))
            .map(|(&tag, _)| tag)
            .collect()
    })
}

/// `setToolTip:`.
pub(crate) fn set_tool_tip(view: &NSView, text: Option<&NSString>) {
    let old = views::tool_tip(views::imp(view)).replace(text.map(|t| t.copy_string()));
    drop(old);
    let has_area = !tags_of(view, Some(true)).is_empty();
    match (text.is_some(), has_area) {
        (true, false) => {
            add(view, NSRect::ZERO, true, Source::Whole);
        }
        (false, true) => {
            for tag in tags_of(view, Some(true)) {
                remove(view, tag);
            }
        }
        _ => {}
    }
}

/// `addToolTipRect:owner:userData:`.
pub(crate) fn add_tool_tip_rect(view: &NSView, rect: NSRect, owner: &AnyObject, data: *mut c_void) -> isize {
    add(view, rect, false, Source::Owner { owner: Weak::new(owner), data })
}

/// `removeToolTip:`.
pub(crate) fn remove_tool_tip(view: &NSView, tag: isize) {
    remove(view, tag as usize);
}

/// `removeAllToolTips`: the view's `toolTip` too.
pub(crate) fn remove_all(view: &NSView) {
    let old = views::tool_tip(views::imp(view)).take();
    drop(old);
    for tag in tags_of(view, None) {
        remove(view, tag);
    }
}

/// A tooltip showing `text` over `rect` of `view` (in its coordinates),
/// for controls with a tooltip per part (segments, cells). Returns a tag
/// for [`remove_text_rect`].
#[allow(dead_code)]
pub(crate) fn add_text_rect(view: &NSView, rect: NSRect, text: &NSString) -> isize {
    add(view, rect, false, Source::Text(text.copy_string()))
}

/// Take off a tooltip [`add_text_rect`] added.
#[allow(dead_code)]
pub(crate) fn remove_text_rect(view: &NSView, tag: isize) {
    remove(view, tag as usize);
}

trait CopyString {
    fn copy_string(&self) -> Retained<NSString>;
}

impl CopyString for NSString {
    fn copy_string(&self) -> Retained<NSString> {
        objc2_foundation::NSCopying::copy(self)
    }
}

// Hovering.

fn entered(event: &NSEvent) {
    let Some(area) = event.trackingArea() else { return };
    let area = Retained::as_ptr(&area) as usize;
    let Some(window) = event.window(MainThreadMarker::new().expect("tooltips belong to the main thread")) else {
        return;
    };
    let at = event.locationInWindow();
    HOVER.with(|h| h.replace(Some(Hover { area, window: Weak::new(&window), at })));
    let warm = LAST_HIDDEN.with(Cell::get).is_some_and(|t| t.elapsed() < WARM);
    let showing_other = SHOWN.with(|s| s.borrow().as_ref().is_some_and(|s| s.area != area));
    let delay = if warm || showing_other { initial_delay() / 10 } else { initial_delay() };
    schedule(delay);
}

fn moved(event: &NSEvent) {
    let Some(area) = event.trackingArea() else { return };
    let area = Retained::as_ptr(&area) as usize;
    let at = event.locationInWindow();
    let waiting = HOVER.with(|h| {
        let mut hover = h.borrow_mut();
        let hover = hover.as_mut().filter(|h| h.area == area)?;
        hover.at = at;
        Some(())
    });
    let shown = SHOWN.with(|s| s.borrow().as_ref().is_some_and(|s| s.area == area));
    // The pointer has to rest: moving starts the wait over, until the
    // tooltip is up.
    if waiting.is_some() && !shown {
        schedule(initial_delay());
    }
}

fn exited(area: usize) {
    let ours = HOVER.with(|h| {
        let mut hover = h.borrow_mut();
        let ours = hover.as_ref().is_some_and(|h| h.area == area);
        if ours {
            *hover = None;
        }
        ours
    });
    if ours {
        cancel_timer();
    }
    if SHOWN.with(|s| s.borrow().as_ref().is_some_and(|s| s.area == area)) {
        hide();
    }
}

/// A click or a key: whatever tooltip is up goes, and the one waiting
/// waits for the pointer to move on.
pub(crate) fn input() {
    if HOVER.with(|h| h.borrow().is_none()) && SHOWN.with(|s| s.borrow().is_none()) {
        return;
    }
    HOVER.with(|h| h.take());
    cancel_timer();
    hide();
}

/// `window` is leaving the screen: a tooltip over it goes first (a popup
/// can't outlive its parent).
pub(crate) fn window_leaving(window: &NSWindow) {
    let over =
        SHOWN.with(|s| s.borrow().as_ref().is_some_and(|s| s.over.load().is_some_and(|w| std::ptr::eq(&*w, window))));
    let hovering =
        HOVER.with(|h| h.borrow().as_ref().is_some_and(|h| h.window.load().is_some_and(|w| std::ptr::eq(&*w, window))));
    if hovering {
        HOVER.with(|h| h.take());
        cancel_timer();
    }
    if over {
        hide();
    }
}

/// `NSInitialToolTipDelay`, in milliseconds, from the defaults.
fn initial_delay() -> Duration {
    // SAFETY: integerForKey: takes a key and returns an integer.
    let ms: isize = unsafe {
        let defaults: Retained<AnyObject> =
            msg_send![objc2::runtime::AnyClass::get(c"NSUserDefaults").expect("Foundation"), standardUserDefaults];
        msg_send![&*defaults, integerForKey: &*NSString::from_str("NSInitialToolTipDelay")]
    };
    Duration::from_millis(if ms > 0 { ms as u64 } else { 1000 })
}

fn schedule(delay: Duration) {
    cancel_timer();
    let block = RcBlock::new(|_: NonNull<NSTimer>| {
        TIMER.with(|t| t.take());
        show();
    });
    // SAFETY: the block runs on the main thread, where the timer is
    // scheduled.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(delay.as_secs_f64(), false, &block) };
    // SAFETY: the main loop takes the timer, in the common modes so tooltips
    // show during tracking and modal loops too.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    TIMER.with(|t| t.replace(Some(timer)));
}

fn cancel_timer() {
    if let Some(timer) = TIMER.with(|t| t.take()) {
        timer.invalidate();
    }
}

/// The text the area under the pointer shows.
fn text_of(area: usize, at: NSPoint) -> Option<Retained<NSString>> {
    enum Found {
        Whole(Retained<NSView>),
        Owner(Retained<NSView>, Retained<AnyObject>, *mut c_void),
        Text(Retained<NSString>),
    }
    let found = AREAS.with(|a| {
        let areas = a.borrow();
        let entry = areas.get(&area)?;
        let view = entry.view.load()?;
        Some(match &entry.source {
            Source::Whole => Found::Whole(view),
            Source::Owner { owner, data } => Found::Owner(view, owner.load()?, *data),
            Source::Text(text) => Found::Text(text.clone()),
        })
    })?;
    match found {
        Found::Whole(view) => views::tool_tip(views::imp(&view)).borrow().clone(),
        Found::Text(text) => Some(text),
        Found::Owner(view, owner, data) => {
            let point = view.convertPoint_fromView(at, None);
            let asks = sel!(view:stringForToolTip:point:userData:);
            // SAFETY: respondsToSelector: takes a selector and returns BOOL.
            let responds: bool = unsafe { msg_send![&*owner, respondsToSelector: asks] };
            if responds {
                let tag = area as isize;
                // SAFETY: the owner's method takes the view, the tag, the
                // point and the user data, and returns a string.
                unsafe { msg_send![&*owner, view: &*view, stringForToolTip: tag, point: point, userData: data] }
            } else {
                // SAFETY: description takes nothing and returns a string.
                unsafe { msg_send![&*owner, description] }
            }
        }
    }
}

fn show() {
    let Some((area, window, at)) =
        HOVER.with(|h| h.borrow().as_ref().and_then(|h| Some((h.area, h.window.load()?, h.at))))
    else {
        return;
    };
    hide();
    let Some(text) = text_of(area, at).filter(|t| t.length() > 0) else { return };
    if !window.isVisible() {
        return;
    }
    let tip = tip_window(window.mtm(), &text);
    let anchor = NSRect::new(NSPoint::new(at.x, at.y - BELOW_POINTER), NSSize::new(1.0, BELOW_POINTER));
    crate::window::show_as_popup(&tip, &window, anchor, false);
    SHOWN.with(|s| s.replace(Some(Shown { area, tip, over: Weak::new(&window) })));
}

fn hide() {
    let shown = SHOWN.with(|s| s.take());
    if let Some(shown) = shown {
        shown.tip.orderOut(None);
        LAST_HIDDEN.with(|l| l.set(Some(Instant::now())));
    }
}

// The tooltip's window.

fn attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::toolTipsFontOfSize(0.0);
    let color = NSColor::textColor();
    // SAFETY: the attribute names are constants this crate exports.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, &color];
    NSDictionary::from_slices(&keys, &values)
}

/// Where the text goes: one line if it fits, else wrapped at the widest a
/// tooltip grows.
fn text_size(text: &NSString) -> NSSize {
    let attributes = attributes();
    // SAFETY: the dictionary maps attribute names to their values.
    let line = unsafe { text.sizeWithAttributes(Some(&attributes)) };
    if line.width <= MAX_WIDTH {
        return NSSize::new(line.width.ceil(), line.height.ceil());
    }
    let options = objc2_app_kit::NSStringDrawingOptions::UsesLineFragmentOrigin;
    // SAFETY: as above; the context may be nil.
    let bounds = unsafe {
        text.boundingRectWithSize_options_attributes_context(
            NSSize::new(MAX_WIDTH, f64::MAX),
            options,
            Some(&attributes),
            None,
        )
    };
    NSSize::new(bounds.size.width.ceil(), bounds.size.height.ceil())
}

fn tip_window(mtm: MainThreadMarker, text: &NSString) -> Retained<NSWindow> {
    let size = text_size(text);
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(size.width + 2.0 * PAD_X, size.height + 2.0 * PAD_Y));
    // SAFETY: NSWindow's designated initializer.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the tooltip keeps its window.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setIgnoresMouseEvents(true);
    let this = ToolTipView::alloc(mtm).set_ivars(text.copy_string());
    // SAFETY: NSView's designated initializer.
    let view: Retained<ToolTipView> = unsafe { msg_send![super(this), initWithFrame: frame] };
    window.setContentView(Some(&view));
    window
}

define_class!(
    // Draws a tooltip: its text on a pale ground with a thin edge.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepToolTipView"]
    #[ivars = Retained<NSString>]
    struct ToolTipView;

    impl ToolTipView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            NSColor::colorWithWhite_alpha(0.6, 1.0).setFill();
            NSBezierPath::fillRect(bounds);
            let inner = NSRect::new(
                NSPoint::new(bounds.origin.x + 1.0, bounds.origin.y + 1.0),
                NSSize::new(bounds.size.width - 2.0, bounds.size.height - 2.0),
            );
            NSColor::colorWithWhite_alpha(0.98, 1.0).setFill();
            NSBezierPath::fillRect(inner);
            let text_rect = NSRect::new(
                NSPoint::new(PAD_X, PAD_Y),
                NSSize::new(bounds.size.width - 2.0 * PAD_X, bounds.size.height - 2.0 * PAD_Y),
            );
            // SAFETY: the dictionary maps attribute names to their values.
            unsafe { self.ivars().drawInRect_withAttributes(text_rect, Some(&attributes())) };
        }
    }

    unsafe impl NSObjectProtocol for ToolTipView {}
);
