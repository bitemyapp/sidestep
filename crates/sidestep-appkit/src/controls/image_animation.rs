//! Animated images in image views: an image whose bitmap has frames (an
//! animated GIF, `NSImageFrameCount`) steps through them while an image
//! view that `animates` shows it, in a window or not (as AppKit's does,
//! `image_views.rs`, `animation`), each frame for its
//! `NSImageCurrentFrameDuration`, as many times as `NSImageLoopCount`
//! says (0 for ever), stopping on the last frame, by setting the bitmap's
//! `NSImageCurrentFrame` as AppKit does. A one-shot timer in the main
//! loop's common modes brings each frame, so nothing runs between frames,
//! and the animation stops when the view drops the image, stops animating
//! or goes away; it goes on from the frame showing when it starts again.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{Message, msg_send};
use objc2_app_kit::{NSBitmapImageRep, NSImage, NSView};
use objc2_foundation::{NSNumber, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer};

/// One image view's animation.
pub(crate) struct Animation {
    /// The image animated.
    image: Retained<NSImage>,
    rep: Retained<NSBitmapImageRep>,
    count: usize,
    /// Times to play it all (0 for ever) and times played.
    loops: usize,
    played: usize,
    index: usize,
    timer: Option<Retained<NSTimer>>,
}

impl Drop for Animation {
    fn drop(&mut self) {
        if let Some(t) = self.timer.take() {
            t.invalidate();
        }
    }
}

/// A number property of `rep`.
fn property(rep: &NSBitmapImageRep, key: &str) -> Option<f64> {
    // SAFETY: the key is a property name; the value is a number or nil.
    let value: Option<Retained<AnyObject>> = unsafe { msg_send![rep, valueForProperty: &*NSString::from_str(key)] };
    // SAFETY: frame properties are numbers.
    value.map(|v| unsafe { msg_send![&*v, doubleValue] })
}

impl Animation {
    /// The animation `view` runs for `image`: `old` if it's for the same
    /// image, a new one if the image has frames, else none.
    pub(crate) fn follow(old: Option<Animation>, image: &NSImage, view: &NSView) -> Option<Animation> {
        if let Some(old) = old
            && std::ptr::eq(&*old.image, image)
        {
            return Some(old);
        }
        let rep = image.representations().iter().find_map(|r| r.downcast::<NSBitmapImageRep>().ok())?;
        let count = property(&rep, "NSImageFrameCount")? as usize;
        if count < 2 {
            return None;
        }
        let index = property(&rep, "NSImageCurrentFrame").unwrap_or(0.0) as usize;
        let loops = property(&rep, "NSImageLoopCount").unwrap_or(0.0) as usize;
        let mut a = Animation { image: image.retain(), rep, count, loops, played: 0, index, timer: None };
        a.arm(view);
        Some(a)
    }

    /// Set the timer for the end of the frame showing.
    fn arm(&mut self, view: &NSView) {
        let seconds = property(&self.rep, "NSImageCurrentFrameDuration").unwrap_or(0.1).max(0.01);
        let weak = Weak::new(view);
        let block = RcBlock::new(move |_: NonNull<NSTimer>| {
            if let Some(view) = weak.load() {
                step(&view);
            }
        });
        // SAFETY: the block runs on the main thread, where the timer is
        // scheduled, in the common modes (Foundation's constant), so images
        // animate during tracking loops too.
        let timer = unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(seconds, false, &block);
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            timer
        };
        if let Some(old) = self.timer.replace(timer) {
            old.invalidate();
        }
    }

    /// Show the next frame, unless the last loop just ended. True if it
    /// moved on.
    fn advance(&mut self) -> bool {
        let next = (self.index + 1) % self.count;
        if next == 0 {
            self.played += 1;
            if self.loops != 0 && self.played >= self.loops {
                self.timer = None;
                return false;
            }
        }
        self.index = next;
        let n = NSNumber::new_isize(next as isize);
        // SAFETY: the key is a property name and the value a number.
        let _: () =
            unsafe { msg_send![&*self.rep, setProperty: &*NSString::from_str("NSImageCurrentFrame"), withValue: &*n] };
        true
    }
}

/// A frame of `view`'s animation ended: show the next and wait for its end.
fn step(view: &NSView) {
    let Some(image_view) = super::image_view::as_image_view(view) else { return };
    let retained = view.retain();
    let moved = image_view.with_animation(|a| {
        let moved = a.advance();
        if moved {
            a.arm(&retained);
        }
        moved
    });
    if moved == Some(true) {
        view.setNeedsDisplay(true);
    }
}
