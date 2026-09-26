//! `NSScreen`: the compositor's outputs.
//!
//! The render thread publishes a snapshot of the outputs (see
//! `backend::outputs`) as they come, change and go, and the main thread
//! turns it into screens when the program next asks. A screen object stays
//! the same for its output and is updated in place, as on macOS. The first
//! screen is the output at (0, 0) of the compositor's space, else the
//! top-left-most one; its frame's origin is (0, 0), and the others' frames
//! are placed around it with y going up, as AppKit counts. The first time
//! a program asks, before any window, the render thread is started and the
//! main thread waits for the outputs (a round trip or two with the
//! compositor, bounded by `FIRST_SNAPSHOT`). Without a Wayland display
//! there are no screens.
//!
//! Wayland doesn't tell a client where its windows are, only which outputs
//! they're on: a window's `screen` is the output it entered last that it's
//! still on, and a window not on screen gets the main screen, the key
//! window's (else the first). `visibleFrame` is the frame less the space
//! the desktop's panels take, known from the size limit the compositor
//! last gave a window there (xdg_toplevel's configure bounds), with the
//! panels assumed at the top; before any, it's the frame.
//! `backingScaleFactor` is the output's scale: fractional where its mode
//! over its logical size (xdg-output) shows one, or a window there was
//! told one, else wl_output's whole number.
//!
//! When the outputs change, the application's delegate gets
//! `applicationDidChangeScreenParameters:`, whether or not the program has
//! asked about screens yet, and a window whose screen changed has its
//! delegate told `windowDidChangeScreen:`; both are posted to the default
//! notification center too.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSScreen, NSWindow};
use objc2_foundation::{NSArray, NSDictionary, NSEdgeInsets, NSNumber, NSPoint, NSRect, NSSize, NSString, NSValue};
use sidestep_foundation::notification_center::post;

use crate::protocol::{ToRender, WindowId};
use crate::window::{self, NSWindowImpl};

/// How long the first question about screens waits for the compositor to
/// describe its outputs.
const FIRST_SNAPSHOT: Duration = Duration::from_secs(1);

/// An output, as the render thread publishes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    /// The wl_output global's name, which stays the same while it's there.
    pub id: u32,
    pub name: String,
    /// Where it is in the compositor's space, in logical points (y down).
    pub rect: (i32, i32, i32, i32),
    pub scale: f64,
    pub refresh_mhz: i32,
    /// The largest window size there that leaves the desktop's panels
    /// uncovered, once a window was told it.
    pub work_area: Option<(u32, u32)>,
}

#[derive(Default)]
struct Published {
    /// The render thread has described every output it knew of at start.
    settled: bool,
    /// Counts snapshots, so the main thread knows when to update.
    generation: u64,
    /// Some snapshot since the first was a change (the program is told of
    /// changes even if it never asked about the screens before them).
    changed: bool,
    outputs: Vec<Output>,
    /// The main thread asked the render thread to start for them.
    asked: Option<Instant>,
}

static PUBLISHED: Mutex<Published> =
    Mutex::new(Published { settled: false, generation: 0, changed: false, outputs: Vec::new(), asked: None });
static ARRIVED: Condvar = Condvar::new();

fn published() -> std::sync::MutexGuard<'static, Published> {
    PUBLISHED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The render thread's side: the outputs are now these; `news` unless
/// they're the first, or the same again.
pub(crate) fn publish(outputs: Vec<Output>, news: bool) {
    let mut p = published();
    p.settled = true;
    p.generation += 1;
    p.changed |= news;
    p.outputs = outputs;
    drop(p);
    ARRIVED.notify_all();
}

/// The latest snapshot if it's newer than snapshot `seen`, starting the
/// render thread and waiting for its first one if need be.
fn snapshot(seen: u64) -> Option<(u64, Vec<Output>)> {
    let mut p = published();
    if !p.settled {
        let display = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|v| std::env::var_os(v).is_some());
        if !display {
            return None;
        }
        let asked = *p.asked.get_or_insert_with(Instant::now);
        drop(p);
        crate::app::send(ToRender::PublishOutputs);
        p = published();
        while !p.settled {
            let left = (asked + FIRST_SNAPSHOT).saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            p = ARRIVED.wait_timeout(p, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
    (p.generation != seen).then(|| (p.generation, p.outputs.clone()))
}

/// A screen's state, from its output.
#[derive(Clone, Copy, Default)]
struct Geometry {
    frame: NSRect,
    visible: NSRect,
    scale: f64,
    refresh_mhz: i32,
}

pub(crate) struct ScreenIvars {
    output: u32,
    geometry: Cell<Geometry>,
    name: RefCell<Retained<NSString>>,
}

thread_local! {
    /// The screens, the first one first, and the snapshot they're from.
    static SCREENS: RefCell<(u64, Vec<Retained<NSScreen>>)> = const { RefCell::new((0, Vec::new())) };
    /// The outputs each window on screen is on, in the order it entered
    /// them, by its showing (see `window_closed`).
    static WINDOW_OUTPUTS: RefCell<HashMap<WindowId, Vec<u32>>> = RefCell::new(HashMap::new());
}

sidestep_runtime::static_class!(pub NSSCREEN, NSSCREEN_META = "NSScreen", || {
    let _ = NSScreenImpl::class();
});

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSScreen"]
    #[ivars = ScreenIvars]
    pub(crate) struct NSScreenImpl;

    impl NSScreenImpl {
        #[unsafe(method_id(screens))]
        fn screens() -> Retained<NSArray<NSScreen>> {
            NSArray::from_retained_slice(&screens())
        }

        #[unsafe(method_id(mainScreen))]
        fn main_screen() -> Option<Retained<NSScreen>> {
            main_screen()
        }

        #[unsafe(method_id(deepestScreen))]
        fn deepest_screen() -> Option<Retained<NSScreen>> {
            screens().into_iter().next()
        }

        #[unsafe(method(screensHaveSeparateSpaces))]
        fn screens_have_separate_spaces() -> bool {
            false
        }

        #[unsafe(method(frame))]
        fn frame(&self) -> NSRect {
            self.ivars().geometry.get().frame
        }

        #[unsafe(method(visibleFrame))]
        fn visible_frame(&self) -> NSRect {
            self.ivars().geometry.get().visible
        }

        #[unsafe(method(backingScaleFactor))]
        fn backing_scale_factor(&self) -> f64 {
            self.ivars().geometry.get().scale
        }

        #[unsafe(method_id(localizedName))]
        fn localized_name(&self) -> Retained<NSString> {
            self.ivars().name.borrow().clone()
        }

        #[unsafe(method(maximumFramesPerSecond))]
        fn maximum_frames_per_second(&self) -> isize {
            match self.ivars().geometry.get().refresh_mhz {
                0 => 60,
                mhz => ((mhz + 500) / 1000) as isize,
            }
        }

        #[unsafe(method(safeAreaInsets))]
        fn safe_area_insets(&self) -> NSEdgeInsets {
            NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
        }

        #[unsafe(method(convertRectToBacking:))]
        fn convert_rect_to_backing(&self, r: NSRect) -> NSRect {
            scaled(r, self.ivars().geometry.get().scale)
        }

        #[unsafe(method(convertRectFromBacking:))]
        fn convert_rect_from_backing(&self, r: NSRect) -> NSRect {
            scaled(r, 1.0 / self.ivars().geometry.get().scale)
        }

        #[unsafe(method(backingAlignedRect:options:))]
        fn backing_aligned_rect_options(&self, r: NSRect, options: u64) -> NSRect {
            aligned(r, options, self.ivars().geometry.get().scale)
        }

        #[unsafe(method_id(deviceDescription))]
        fn device_description(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
            self.description()
        }
    }

    unsafe impl NSObjectProtocol for NSScreenImpl {}
);

fn imp(screen: &NSScreen) -> &NSScreenImpl {
    // SAFETY: NSScreen is NSScreenImpl's class.
    unsafe { &*(screen as *const NSScreen).cast::<NSScreenImpl>() }
}

impl NSScreenImpl {
    fn description(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
        let g = self.ivars().geometry.get();
        let keys = ["NSDeviceIsScreen", "NSDeviceColorSpaceName", "NSDeviceBitsPerSample", "NSScreenNumber"]
            .into_iter()
            .chain(["NSDeviceResolution", "NSDeviceSize"])
            .map(NSString::from_str)
            .collect::<Vec<_>>();
        let dpi = 72.0 * g.scale;
        let object = |o: Retained<NSObject>| -> Retained<AnyObject> { Retained::into_super(o) };
        let values: Vec<Retained<AnyObject>> = vec![
            object(Retained::into_super(NSString::from_str("YES"))),
            object(Retained::into_super(NSString::from_str("NSCalibratedRGBColorSpace"))),
            object(Retained::into_super(Retained::into_super(NSNumber::new_isize(8)))),
            object(Retained::into_super(Retained::into_super(NSNumber::new_u32(self.ivars().output)))),
            object(Retained::into_super(NSValue::new(NSSize::new(dpi, dpi)))),
            object(Retained::into_super(NSValue::new(g.frame.size))),
        ];
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
        NSDictionary::from_slices(&keys, &values)
    }
}

fn scaled(r: NSRect, s: f64) -> NSRect {
    NSRect::new(NSPoint::new(r.origin.x * s, r.origin.y * s), NSSize::new(r.size.width * s, r.size.height * s))
}

/// `backingAlignedRect:options:`: the edges and lengths the options name
/// on whole backing pixels, each rounded as they ask: inward (a min edge
/// up, a max edge or a length down), outward (the other way) or to the
/// nearest, halfway going up (below zero too), except on y in a flipped
/// rectangle (`NSAlignRectFlipped`), where it goes down. Per axis, two of
/// the min edge, max edge and length are given; the third follows from
/// them.
fn aligned(r: NSRect, options: u64, scale: f64) -> NSRect {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let bit = |n: u32| options & (1 << n) != 0;
    let flipped = bit(63);
    // Inward bits from 0, outward from 8, nearest from 16; within each, min
    // x, min y, max x, max y, width, height (so y's are the odd ones).
    let round = |v: f64, which: u32, min_edge: bool| -> Option<f64> {
        let px = v * s;
        let r = if bit(which) {
            if min_edge { px.ceil() } else { px.floor() }
        } else if bit(which + 8) {
            if min_edge { px.floor() } else { px.ceil() }
        } else if bit(which + 16) {
            if flipped && which % 2 == 1 { (px - 0.5).ceil() } else { (px + 0.5).floor() }
        } else {
            return None;
        };
        Some(r / s)
    };
    // An axis from its min edge, max edge and length options.
    let axis = |min: f64, length: f64, which: u32| -> (f64, f64) {
        let lo = round(min, which, true);
        let hi = round(min + length, which + 2, false);
        let len = round(length, which + 4, false);
        match (lo, hi, len) {
            (Some(lo), Some(hi), _) => (lo, hi - lo),
            (Some(lo), None, len) => (lo, len.unwrap_or(length)),
            (None, Some(hi), Some(len)) => (hi - len, len),
            (None, Some(hi), None) => (min, hi - min),
            (None, None, len) => (min, len.unwrap_or(length)),
        }
    };
    let (x, width) = axis(r.origin.x, r.size.width, 0);
    let (y, height) = axis(r.origin.y, r.size.height, 1);
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// The first screen: the output at (0, 0), else the top-left-most.
fn first(outputs: &[Output]) -> Option<&Output> {
    outputs
        .iter()
        .find(|o| (o.rect.0, o.rect.1) == (0, 0))
        .or_else(|| outputs.iter().min_by_key(|o| (o.rect.1, o.rect.0)))
}

/// An output's frame and visible frame, y up, against the first screen.
fn frames(output: &Output, first: &Output) -> (NSRect, NSRect) {
    let (x, y, w, h) = output.rect;
    let (fx, fy, _, fh) = first.rect;
    let bottom = (fy + fh) - (y + h);
    let frame = NSRect::new(NSPoint::new((x - fx) as f64, bottom as f64), NSSize::new(w as f64, h as f64));
    let visible = match output.work_area {
        Some((bw, bh)) => NSRect::new(
            frame.origin,
            NSSize::new((bw as f64).min(frame.size.width), (bh as f64).min(frame.size.height)),
        ),
        None => frame,
    };
    (frame, visible)
}

/// The screens, the first one first, brought up to date with the outputs.
fn screens() -> Vec<Retained<NSScreen>> {
    let Some(mtm) = MainThreadMarker::new() else { return Vec::new() };
    let (seen, old) = SCREENS.with(|s| s.borrow().clone());
    let Some((generation, outputs)) = snapshot(seen) else { return old };
    let mut ordered: Vec<&Output> = outputs.iter().collect();
    if let Some(first) = first(&outputs) {
        ordered.sort_by_key(|o| o.id != first.id);
    }
    let fresh: Vec<Retained<NSScreen>> = ordered
        .iter()
        .map(|output| {
            let screen = old
                .iter()
                .find(|s| imp(s).ivars().output == output.id)
                .cloned()
                .unwrap_or_else(|| new_screen(mtm, output.id));
            let (frame, visible) = frames(output, first(&outputs).unwrap_or(output));
            let ivars = imp(&screen).ivars();
            ivars.geometry.set(Geometry { frame, visible, scale: output.scale, refresh_mhz: output.refresh_mhz });
            ivars.name.replace(NSString::from_str(&output.name));
            screen
        })
        .collect();
    let replaced = SCREENS.with(|s| std::mem::replace(&mut *s.borrow_mut(), (generation, fresh.clone())));
    // Released outside the borrow.
    drop(replaced);
    fresh
}

fn new_screen(mtm: MainThreadMarker, output: u32) -> Retained<NSScreen> {
    crate::load_shell::<NSScreen>();
    let this = NSScreenImpl::alloc(mtm).set_ivars(ScreenIvars {
        output,
        geometry: Cell::new(Geometry::default()),
        name: RefCell::new(NSString::new()),
    });
    // SAFETY: NSObject's designated initializer.
    let screen: Retained<NSScreenImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSScreenImpl is the class NSScreen names.
    unsafe { Retained::cast_unchecked(screen) }
}

/// `mainScreen`: the key window's screen, else the first.
fn main_screen() -> Option<Retained<NSScreen>> {
    let screens = screens();
    crate::app::key_window()
        .and_then(|w| entered_screen(window::imp(&w), &screens))
        .or_else(|| screens.first().cloned())
}

/// The screen of the output the window entered last and is still on.
fn entered_screen(window: &NSWindowImpl, screens: &[Retained<NSScreen>]) -> Option<Retained<NSScreen>> {
    let id = WINDOW_OUTPUTS.with(|w| w.borrow().get(&window.id()).and_then(|o| o.last().copied()))?;
    screens.iter().find(|s| imp(s).ivars().output == id).cloned()
}

/// `-[NSWindow screen]`.
pub(crate) fn window_screen(window: &NSWindowImpl) -> Option<Retained<NSScreen>> {
    entered_screen(window, &screens()).or_else(main_screen)
}

/// The window left the screen: what it was on is forgotten, and until it's
/// back, it's on the main screen.
pub(crate) fn window_closed(window: &NSWindow) {
    let id = window::imp(window).id();
    let gone = WINDOW_OUTPUTS.with(|w| w.borrow_mut().remove(&id));
    drop(gone);
}

/// The outputs the render thread says a window is on changed.
pub(crate) fn window_outputs(window: &NSWindow, outputs: Vec<u32>) {
    let id = window::imp(window).id();
    let last = outputs.last().copied();
    let before = WINDOW_OUTPUTS.with(|w| w.borrow_mut().insert(id, outputs)).and_then(|o| o.last().copied());
    if before.is_some() && before != last {
        tell_window(window, "NSWindowDidChangeScreenNotification");
    }
}

/// The outputs changed.
pub(crate) fn changed(mtm: MainThreadMarker) {
    let seen = SCREENS.with(|s| s.borrow().0);
    let before: Vec<(u32, Geometry)> =
        SCREENS.with(|s| s.borrow().1.iter().map(|s| (imp(s).ivars().output, imp(s).ivars().geometry.get())).collect());
    let after: Vec<(u32, Geometry)> =
        screens().iter().map(|s| (imp(s).ivars().output, imp(s).ivars().geometry.get())).collect();
    let same = if seen == 0 {
        // The program hasn't asked about screens before: the first snapshot
        // is where they start, and a change is news only if one came since.
        !published().changed
    } else {
        before.len() == after.len()
            && before
                .iter()
                .zip(&after)
                .all(|((a, g), (b, h))| a == b && g.frame == h.frame && g.visible == h.visible && g.scale == h.scale)
    };
    if same {
        return;
    }
    let app = NSApplication::sharedApplication(mtm);
    // The application's delegate hears of it as an observer.
    let name = NSString::from_str("NSApplicationDidChangeScreenParametersNotification");
    post(&name, Some(app.as_ref()), None);
}

fn tell_window(window: &NSWindow, name: &str) {
    // The window's delegate hears of it as an observer (setDelegate:
    // registers it for the notifications it implements).
    post(&NSString::from_str(name), Some(window.as_ref()), None);
}

define_class!(
    // Holds NSWindow's screen methods beyond `screen` itself, which the
    // `SidestepScreens` category adds to NSWindow.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowScreens"]
    struct WindowScreens;

    impl WindowScreens {
        /// The screen with the deepest color, which is the window's own.
        #[unsafe(method_id(deepestScreen))]
        fn deepest_screen(&self) -> Option<Retained<NSScreen>> {
            // SAFETY: the category adds the method to NSWindow, so `self` is
            // a window.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            window_screen(window::imp(window))
        }
    }
);

// NSWindow's screen methods.
sidestep_runtime::category!("NSWindow"(SidestepScreens), |category| {
    // SAFETY: the helper's method treats its receiver as an NSWindow.
    unsafe { category.add_methods_of(WindowScreens::class()) };
});

sidestep_foundation::constant_string!(NSDeviceIsScreen = "NSDeviceIsScreen");
sidestep_foundation::constant_string!(NSDeviceIsPrinter = "NSDeviceIsPrinter");
sidestep_foundation::constant_string!(NSDeviceColorSpaceName = "NSDeviceColorSpaceName");
sidestep_foundation::constant_string!(NSDeviceBitsPerSample = "NSDeviceBitsPerSample");
sidestep_foundation::constant_string!(NSDeviceResolution = "NSDeviceResolution");
sidestep_foundation::constant_string!(NSDeviceSize = "NSDeviceSize");
// Posted when a screen's color space changes, which Wayland doesn't say.
sidestep_foundation::constant_string!(
    NSScreenColorSpaceDidChangeNotification = "NSScreenColorSpaceDidChangeNotification"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn output(id: u32, rect: (i32, i32, i32, i32)) -> Output {
        Output { id, name: String::new(), rect, scale: 1.0, refresh_mhz: 0, work_area: None }
    }

    fn r(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    #[test]
    fn frames_are_flipped_against_the_first_screen() {
        let outputs = [output(7, (1280, 0, 1920, 1080)), output(3, (0, 0, 1280, 800))];
        let first = first(&outputs).unwrap();
        assert_eq!(first.id, 3);
        assert_eq!(frames(&outputs[1], first).0, r(0.0, 0.0, 1280.0, 800.0));
        // Beside it, top-aligned: its bottom is lower.
        assert_eq!(frames(&outputs[0], first).0, r(1280.0, -280.0, 1920.0, 1080.0));
        // Below it.
        let below = output(9, (0, 800, 1280, 800));
        assert_eq!(frames(&below, first).0, r(0.0, -800.0, 1280.0, 800.0));
        // Without an output at (0, 0), the top-left-most is first.
        let apart = [output(1, (100, 50, 800, 600)), output(2, (900, 0, 800, 600))];
        assert_eq!(super::first(&apart).unwrap().id, 2);
    }

    #[test]
    fn visible_frames_leave_the_panels_out() {
        let mut o = output(1, (0, 0, 1280, 800));
        o.work_area = Some((1280, 768));
        let (frame, visible) = frames(&o, &o.clone());
        assert_eq!(frame, r(0.0, 0.0, 1280.0, 800.0));
        assert_eq!(visible, r(0.0, 0.0, 1280.0, 768.0));
    }

    #[test]
    fn backing_alignment() {
        // As macOS aligns at scale 2.
        let rect = r(0.3, 0.6, 10.2, 10.7);
        // Halfway, nearest goes up, below zero too; flipped, y goes down.
        let nearest_all = (1 << 16) | (1 << 17) | (1 << 18) | (1 << 19);
        assert_eq!(aligned(r(-0.25, -0.25, 10.5, 10.5), nearest_all, 2.0), r(0.0, 0.0, 10.5, 10.5));
        assert_eq!(aligned(r(0.25, 0.25, 10.5, 10.5), nearest_all | (1 << 63), 2.0), r(0.5, 0.0, 10.5, 10.5));
        let outward = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11);
        assert_eq!(aligned(rect, outward, 2.0), r(0.0, 0.5, 10.5, 11.0));
        let nearest = (1 << 16) | (1 << 17) | (1 << 18) | (1 << 19);
        assert_eq!(aligned(rect, nearest, 2.0), r(0.5, 0.5, 10.0, 11.0));
        // Min x nearest and width inward; min y nearest and height outward.
        let sizes = (1 << 16) | (1 << 4) | (1 << 17) | (1 << 13);
        assert_eq!(aligned(rect, sizes, 2.0), r(0.5, 0.5, 10.0, 11.0));
        // Max x nearest and width outward: the origin follows.
        let from_max = (1 << 18) | (1 << 12);
        assert_eq!(aligned(rect, from_max, 2.0), r(0.0, 0.6, 10.5, 10.7));
    }
}
