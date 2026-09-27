//! `NSTextView` in TextKit 2 mode: its layout through a text layout
//! manager, and what it does as its viewport controller's delegate.
//!
//! Measured on macOS (`conformance/tests/textkit2_view.rs`): a text view
//! made with `initWithFrame:` (or `init`, `new`, `scrollableTextView`,
//! `textViewUsingTextLayoutManager:YES`, `initUsingTextLayoutManager:YES`)
//! is TextKit 2, unless its class overrides `drawRect:`; one given a
//! container is the mode of the container's layout manager (TextKit 2 for
//! a text layout manager's, whatever the class). The window's field editor
//! is TextKit 2, the secure one TextKit 1. Asking a TextKit 2 view for its
//! `layoutManager` switches it to TextKit 1 for good: an `NSLayoutManager`
//! takes over the same container and storage, and `textLayoutManager` and
//! `textContentStorage` are nil from then on (the text layout manager
//! keeps its container and content, as on macOS).
//!
//! The view is its viewport controller's delegate. Its viewport bounds are
//! what its clip view shows, in container coordinates: across, its whole
//! width; down, from the top of what the clip view shows (not above the
//! view's bounds) for the clip view's height (below the view's bottom too,
//! as on macOS); in a window and no clip view, its visible rect; in
//! neither, as good as unbounded below its top (so laying the viewport out
//! lays the whole text out: on macOS too, 20 000 lines in 0.4 s). The
//! viewport is laid out before the view draws (`viewWillDraw`) when
//! something moved or changed; the fragments it configured are what the
//! view draws, each through `drawAtPoint:inContext:` after the view's own
//! `drawRect:` (as AppKit draws them above it, in views of their own), with
//! the caret above them. As on macOS, a fragment draws at the point zero,
//! the drawing state's origin moved to its frame's.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Message, msg_send};
use objc2_app_kit::NSTextLayoutFragment;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::layout_manager::NSTextLayoutManagerImpl;
use crate::textkit::layout_manager::{LineAt, NSLayoutManagerImpl};

/// A text view's layout: TextKit 1's layout manager or TextKit 2's.
pub(crate) enum Geo {
    One(Retained<NSLayoutManagerImpl>),
    Two(Retained<NSTextLayoutManagerImpl>),
}

impl Geo {
    pub fn line_at(&self, index: usize, upstream: bool) -> Option<LineAt> {
        match self {
            Geo::One(m) => m.line_at(index, upstream),
            Geo::Two(m) => m.line_at(index, upstream),
        }
    }

    pub fn caret_rect(&self, index: usize, upstream: bool) -> NSRect {
        match self {
            Geo::One(m) => m.caret_rect(index, upstream),
            Geo::Two(m) => m.caret_rect(index, upstream),
        }
    }

    pub fn insertion_index(&self, p: NSPoint) -> (usize, bool) {
        match self {
            Geo::One(m) => m.insertion_index(p),
            Geo::Two(m) => m.insertion_index(p),
        }
    }

    /// The text's height (TextKit 2: the bottom of what is laid out, as
    /// estimated, nothing before anything is).
    pub fn height(&self) -> f64 {
        match self {
            Geo::One(m) => m.height(),
            Geo::Two(m) => {
                let u = m.usage_bounds();
                u.origin.y + u.size.height
            }
        }
    }

    pub fn used_width(&self) -> f64 {
        match self {
            Geo::One(m) => m.used_width(),
            Geo::Two(m) => m.used_width(),
        }
    }

    pub fn lines_in_y(&self, y0: f64, y1: f64) -> Vec<LineAt> {
        match self {
            Geo::One(m) => m.lines_in_y(y0, y1),
            Geo::Two(m) => m.lines_in_y(y0, y1),
        }
    }

    pub fn selection_rects_in(&self, range: Range<usize>, y0: f64, y1: f64) -> Vec<NSRect> {
        match self {
            Geo::One(m) => m.selection_rects_in(range, y0, y1),
            Geo::Two(m) => m.selection_rects_in(range, y0, y1),
        }
    }

    pub fn first_line_rects(&self, range: Range<usize>) -> (Vec<NSRect>, Range<usize>) {
        match self {
            Geo::One(m) => m.first_line_rects(range),
            Geo::Two(m) => m.first_line_rects(range),
        }
    }

    pub fn is_laid(&self, range: Range<usize>) -> bool {
        match self {
            Geo::One(m) => m.is_laid(range),
            Geo::Two(m) => m.is_laid(range),
        }
    }

    pub fn take_damage(&self) -> Option<(f64, f64, bool)> {
        match self {
            Geo::One(m) => m.take_damage(),
            Geo::Two(m) => m.take_damage(),
        }
    }
}

/// A layout manager and its content manager, kept alive.
pub(crate) type Network = (Retained<AnyObject>, Option<Retained<AnyObject>>);

/// What a TextKit 2 text view keeps of its viewport.
#[derive(Default)]
pub(crate) struct ViewState {
    /// Laid out since the layout last changed, for these bounds.
    pub clean: Cell<Option<NSRect>>,
    /// The layout manager and content manager the view keeps alive.
    pub network: RefCell<Option<Network>>,
}

impl ViewState {
    /// Layout changed: lay the viewport out again before drawing.
    pub fn dirty(&self) {
        self.clean.set(None);
    }
}

/// The viewport bounds of a view whose bounds are `bounds`, `visible` of
/// which shows (what its clip view shows, not cut to its bounds; `None`: in
/// no clip view or window, as good as all of it below its top), with its
/// container at `origin`.
pub(crate) fn viewport_bounds(bounds: NSRect, visible: Option<NSRect>, origin: NSPoint) -> NSRect {
    let (top, bottom) = match visible {
        Some(v) => (v.origin.y.max(bounds.origin.y), v.origin.y + v.size.height),
        None => (bounds.origin.y, f64::MAX / 2.0),
    };
    NSRect::new(
        NSPoint::new(bounds.origin.x - origin.x, top - origin.y),
        NSSize::new(bounds.size.width, (bottom - top).max(0.0)),
    )
}

/// Draw `fragments` (in the view's coordinates, the container at
/// `origin`) that meet `dirty`, each through its `drawAtPoint:inContext:`
/// at the point zero, the drawing state's origin moved to its frame's
/// origin, as AppKit draws each in a surface of its own.
pub(crate) fn draw_fragments(fragments: &[Retained<NSTextLayoutFragment>], origin: NSPoint, dirty: NSRect) {
    for f in fragments {
        // SAFETY: the fragment's own geometry methods; a subclass may
        // override them.
        let (frame, surface): (NSRect, NSRect) =
            unsafe { (msg_send![&**f, layoutFragmentFrame], msg_send![&**f, renderingSurfaceBounds]) };
        let at = NSPoint::new(origin.x + frame.origin.x, origin.y + frame.origin.y);
        let reach = NSRect::new(NSPoint::new(at.x + surface.origin.x, at.y + surface.origin.y), surface.size);
        if !meets(reach, dirty) {
            continue;
        }
        let f = f.retain();
        super::draw::with_cg_context_at(at, |cg| {
            // SAFETY: the fragment's drawing method, with a context.
            let _: () = unsafe { msg_send![&*f, drawAtPoint: NSPoint::ZERO, inContext: cg] };
        });
    }
}

fn meets(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

pub(crate) use super::layout_manager::manager_of;
