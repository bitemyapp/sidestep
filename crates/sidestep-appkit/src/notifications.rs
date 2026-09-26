//! Window and application notifications, and their delegates.
//!
//! Each notification's name is exported here, with the value macOS gives
//! it (its own name). A window or the application tells its delegate
//! nothing directly: as on macOS, `setDelegate:` registers the delegate
//! with the default notification center for each notification method it
//! implements (asked once, then), with the window or application as the
//! object, so every "tell the delegate" is a post, and the delegate hears
//! of it among the other observers in registration order. Posting costs
//! one atomic load when nobody observes (see
//! `sidestep_foundation::notification_center::post`).
//!
//! The center holds observers weakly, as delegates are held. Changing the
//! delegate removes the old one's registrations; a window going away
//! removes its delegate's, so a later object at the same address isn't
//! mistaken for it.

use std::cell::Cell;
use std::ffi::CStr;

use objc2::msg_send;
use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::NSString;
use sidestep_foundation::notification_center::{self, default_center};

/// Export constant strings whose values are their names, as
/// `constant_string!` does for one.
macro_rules! names {
    ($($name:ident),* $(,)?) => {$(
        #[unsafe(no_mangle)]
        pub static $name: sidestep_foundation::__private::ObjectRef = {
            static OBJ: sidestep_foundation::ConstantString = sidestep_foundation::ConstantString::new(
                &sidestep_foundation::CONSTANT_STRING_CLASS,
                sidestep_foundation::ConstStr::new(concat!(stringify!($name), "\0")),
            );
            OBJ.object_ref()
        };
    )*};
}

// Every name has its own name as its value on macOS
// (conformance/tests/appkit_events.rs checks them all).
names!(
    NSWindowDidBecomeKeyNotification,
    NSWindowDidBecomeMainNotification,
    NSWindowDidChangeBackingPropertiesNotification,
    NSWindowDidChangeOcclusionStateNotification,
    NSWindowDidChangeScreenNotification,
    NSWindowDidChangeScreenProfileNotification,
    NSWindowDidDeminiaturizeNotification,
    NSWindowDidEndLiveResizeNotification,
    NSWindowDidEndSheetNotification,
    NSWindowDidEnterFullScreenNotification,
    NSWindowDidEnterVersionBrowserNotification,
    NSWindowDidExitFullScreenNotification,
    NSWindowDidExitVersionBrowserNotification,
    NSWindowDidExposeNotification,
    NSWindowDidMiniaturizeNotification,
    NSWindowDidMoveNotification,
    NSWindowDidResignKeyNotification,
    NSWindowDidResignMainNotification,
    NSWindowDidResizeNotification,
    NSWindowDidUpdateNotification,
    NSWindowWillBeginSheetNotification,
    NSWindowWillCloseNotification,
    NSWindowWillEnterFullScreenNotification,
    NSWindowWillEnterVersionBrowserNotification,
    NSWindowWillExitFullScreenNotification,
    NSWindowWillExitVersionBrowserNotification,
    NSWindowWillMiniaturizeNotification,
    NSWindowWillMoveNotification,
    NSWindowWillStartLiveResizeNotification,
    NSApplicationDidBecomeActiveNotification,
    NSApplicationDidChangeOcclusionStateNotification,
    NSApplicationDidChangeScreenParametersNotification,
    NSApplicationDidFinishLaunchingNotification,
    NSApplicationDidHideNotification,
    NSApplicationDidResignActiveNotification,
    NSApplicationDidUnhideNotification,
    NSApplicationDidUpdateNotification,
    NSApplicationProtectedDataDidBecomeAvailableNotification,
    NSApplicationProtectedDataWillBecomeUnavailableNotification,
    NSApplicationWillBecomeActiveNotification,
    NSApplicationWillFinishLaunchingNotification,
    NSApplicationWillHideNotification,
    NSApplicationWillResignActiveNotification,
    NSApplicationWillTerminateNotification,
    NSApplicationWillUnhideNotification,
    NSApplicationWillUpdateNotification,
    NSViewFrameDidChangeNotification,
    NSViewBoundsDidChangeNotification,
    NSViewFocusDidChangeNotification,
    NSViewGlobalFrameDidChangeNotification,
    NSViewDidUpdateTrackingAreasNotification,
    NSBackingPropertyOldScaleFactorKey,
    NSBackingPropertyOldColorSpaceKey,
);

/// A name exported here, as the `&'static NSString` it is.
macro_rules! name {
    ($name:ident) => {{
        // SAFETY: the name is a constant string exported in `notifications`,
        // alive for the whole program.
        unsafe { objc2_app_kit::$name }
    }};
}
pub(crate) use name;

/// A notification a delegate may take: the delegate method's selector and
/// the notification's name.
type Entry = (&'static CStr, fn() -> &'static NSString);

macro_rules! entries {
    ($($selector:literal => $name:ident),* $(,)?) => {
        &[$(($selector, || {
            // SAFETY: the name is a constant string exported above, alive
            // for the whole program.
            unsafe { objc2_app_kit::$name }
        })),*]
    };
}

/// What a window's delegate may observe.
const WINDOW: &[Entry] = entries![
    c"windowDidBecomeKey:" => NSWindowDidBecomeKeyNotification,
    c"windowDidBecomeMain:" => NSWindowDidBecomeMainNotification,
    c"windowDidChangeBackingProperties:" => NSWindowDidChangeBackingPropertiesNotification,
    c"windowDidChangeOcclusionState:" => NSWindowDidChangeOcclusionStateNotification,
    c"windowDidChangeScreen:" => NSWindowDidChangeScreenNotification,
    c"windowDidChangeScreenProfile:" => NSWindowDidChangeScreenProfileNotification,
    c"windowDidDeminiaturize:" => NSWindowDidDeminiaturizeNotification,
    c"windowDidEndLiveResize:" => NSWindowDidEndLiveResizeNotification,
    c"windowDidEndSheet:" => NSWindowDidEndSheetNotification,
    c"windowDidEnterFullScreen:" => NSWindowDidEnterFullScreenNotification,
    c"windowDidEnterVersionBrowser:" => NSWindowDidEnterVersionBrowserNotification,
    c"windowDidExitFullScreen:" => NSWindowDidExitFullScreenNotification,
    c"windowDidExitVersionBrowser:" => NSWindowDidExitVersionBrowserNotification,
    c"windowDidExpose:" => NSWindowDidExposeNotification,
    c"windowDidMiniaturize:" => NSWindowDidMiniaturizeNotification,
    c"windowDidMove:" => NSWindowDidMoveNotification,
    c"windowDidResignKey:" => NSWindowDidResignKeyNotification,
    c"windowDidResignMain:" => NSWindowDidResignMainNotification,
    c"windowDidResize:" => NSWindowDidResizeNotification,
    c"windowDidUpdate:" => NSWindowDidUpdateNotification,
    c"windowWillBeginSheet:" => NSWindowWillBeginSheetNotification,
    c"windowWillClose:" => NSWindowWillCloseNotification,
    c"windowWillEnterFullScreen:" => NSWindowWillEnterFullScreenNotification,
    c"windowWillEnterVersionBrowser:" => NSWindowWillEnterVersionBrowserNotification,
    c"windowWillExitFullScreen:" => NSWindowWillExitFullScreenNotification,
    c"windowWillExitVersionBrowser:" => NSWindowWillExitVersionBrowserNotification,
    c"windowWillMiniaturize:" => NSWindowWillMiniaturizeNotification,
    c"windowWillMove:" => NSWindowWillMoveNotification,
    c"windowWillStartLiveResize:" => NSWindowWillStartLiveResizeNotification,
];

/// What the application's delegate may observe.
const APPLICATION: &[Entry] = entries![
    c"applicationDidBecomeActive:" => NSApplicationDidBecomeActiveNotification,
    c"applicationDidChangeOcclusionState:" => NSApplicationDidChangeOcclusionStateNotification,
    c"applicationDidChangeScreenParameters:" => NSApplicationDidChangeScreenParametersNotification,
    c"applicationDidFinishLaunching:" => NSApplicationDidFinishLaunchingNotification,
    c"applicationDidHide:" => NSApplicationDidHideNotification,
    c"applicationDidResignActive:" => NSApplicationDidResignActiveNotification,
    c"applicationDidUnhide:" => NSApplicationDidUnhideNotification,
    c"applicationDidUpdate:" => NSApplicationDidUpdateNotification,
    c"applicationProtectedDataDidBecomeAvailable:" => NSApplicationProtectedDataDidBecomeAvailableNotification,
    c"applicationProtectedDataWillBecomeUnavailable:" => NSApplicationProtectedDataWillBecomeUnavailableNotification,
    c"applicationWillBecomeActive:" => NSApplicationWillBecomeActiveNotification,
    c"applicationWillFinishLaunching:" => NSApplicationWillFinishLaunchingNotification,
    c"applicationWillHide:" => NSApplicationWillHideNotification,
    c"applicationWillResignActive:" => NSApplicationWillResignActiveNotification,
    c"applicationWillTerminate:" => NSApplicationWillTerminateNotification,
    c"applicationWillUnhide:" => NSApplicationWillUnhideNotification,
    c"applicationWillUpdate:" => NSApplicationWillUpdateNotification,
];

/// Whose delegate: a window's or the application's.
#[derive(Clone, Copy)]
pub(crate) enum Owner {
    Window,
    Application,
}

impl Owner {
    fn entries(self) -> &'static [Entry] {
        match self {
            Owner::Window => WINDOW,
            Owner::Application => APPLICATION,
        }
    }
}

/// The notifications a delegate is registered for, one bit per entry of
/// its owner's table.
#[derive(Default)]
pub(crate) struct Registered(Cell<u64>);

/// Register `delegate` for the notifications of `object` it has methods
/// for, after removing `old`'s registrations (if it is still alive).
pub(crate) fn set_delegate(
    owner: Owner,
    object: &AnyObject,
    registered: &Registered,
    old: Option<&AnyObject>,
    delegate: Option<&AnyObject>,
) {
    let center = default_center();
    let entries = owner.entries();
    let mask = registered.0.replace(0);
    if let Some(old) = old {
        for (i, (_, name)) in entries.iter().enumerate() {
            if mask & (1 << i) != 0 {
                // SAFETY: removes the old delegate's registration for the
                // name and object.
                unsafe { center.removeObserver_name_object(old, Some(name()), Some(object)) };
            }
        }
    }
    let Some(delegate) = delegate else { return };
    let mut mask = 0;
    for (i, (selector, name)) in entries.iter().enumerate() {
        let selector = Sel::register(selector);
        // SAFETY: respondsToSelector: takes a selector and returns BOOL.
        let responds: bool = unsafe { msg_send![delegate, respondsToSelector: selector] };
        if responds {
            // SAFETY: delegate notification methods take the notification;
            // the center holds the delegate weakly and compares the object
            // by identity.
            unsafe { center.addObserver_selector_name_object(delegate, selector, Some(name()), Some(object)) };
            mask |= 1 << i;
        }
    }
    registered.0.set(mask);
}

/// Post `name` with `object` on the default center, if anyone observes.
pub(crate) fn post(name: &NSString, object: &AnyObject) {
    notification_center::post(name, Some(object), None);
}

/// Post `name` with `object` and a user info dictionary.
pub(crate) fn post_with(name: &NSString, object: &AnyObject, user_info: &AnyObject) {
    notification_center::post(name, Some(object), Some(user_info));
}

/// Whether anyone observes `name` at all, to skip building user info.
pub(crate) fn observed(name: &NSString) -> bool {
    notification_center::has_observers(name)
}
