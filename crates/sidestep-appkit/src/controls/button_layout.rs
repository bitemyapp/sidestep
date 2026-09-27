//! Where a button's image and title go: its intrinsic size, `cellSize`,
//! and the image, title and drawing rects for any bounds, for every image
//! position, bezel, control size, `imageScaling` and `imageHugsTitle`.
//!
//! The rules are AppKit's, measured on macOS for thousands of
//! combinations (`conformance/tests/control_images.rs` checks a spread of
//! them). Bezels fall in two families.
//!
//! - Rounded bezels (push, flexible push and glass, circular, badge) keep
//!   the image and title in a content rect inset from the bounds (a push
//!   button's is its fixed height, centered). An image beside or above
//!   its title gets at most half the content rect along that axis, less
//!   the 2-point gap, and sits at its edge, the title centered in what's
//!   left; with `imageHugsTitle` the two sit together in the middle. An
//!   image alone is centered, or at the edge its position names. Rects
//!   are rounded to whole points, edge by edge, half-way values up. A push
//!   button's image is scaled to its height, and centered on the bounds
//!   up and down (half a point lower at the mini size, titled).
//! - Square bezels (small square, shadowless, textured, toolbar) and
//!   borderless buttons give the title a band, the width of what the image
//!   leaves, and let the image take all the room the title doesn't; their
//!   rects aren't rounded. An image beside the title moves the content 2
//!   points further in from the sides; one above or below, from the top
//!   and bottom. A toolbar button has no room for an image above or below
//!   a title, and shows the title alone.
//!
//! Help and push-disclosure buttons don't show images. A disclosure
//! button lays one out in its 13-point square as a square bezel lays out
//! its content, but beside a title the image takes what it needs of the
//! square less 2 points each side and the title the rest, and above or
//! below one there's no room for it. Positions are for left-to-right
//! layout: leading and trailing swap for right to left.

use objc2_app_kit::NSImageScaling;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::button::Look;
use super::{half_up, scaled_image as fit};
use crate::theme::metrics;

/// Between an image and its title.
const GAP: f64 = 2.0;

/// Where the image goes, as laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// The image alone (`NSImageOnly`, or a position with no title).
    Only,
    Left,
    Right,
    Above,
    Below,
    Overlaps,
}

/// What a button with an image lays out.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spec {
    pub look: Look,
    /// The control size, as `metrics` indexes it.
    pub size: usize,
    pub bordered: bool,
    /// The title's size (whole points wide), if it has one to show.
    pub title: Option<NSSize>,
    /// The image's size.
    pub image: NSSize,
    pub place: Place,
    pub hugs: bool,
    pub scaling: NSImageScaling,
}

/// The rects a button with an image answers for some bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rects {
    pub image: NSRect,
    pub title: NSRect,
    pub drawing: NSRect,
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// A rect with each edge rounded to whole points, half-way values up.
fn rounded(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    let (x0, y0, x1, y1) = (half_up(x), half_up(y), half_up(x + w), half_up(y + h));
    rect(x0, y0, x1 - x0, y1 - y0)
}

/// `r` cut to `bounds`.
fn clipped(r: NSRect, bounds: NSRect) -> NSRect {
    let (x0, y0) = (r.origin.x.max(bounds.origin.x), r.origin.y.max(bounds.origin.y));
    let x1 = (r.origin.x + r.size.width).min(bounds.origin.x + bounds.size.width);
    let y1 = (r.origin.y + r.size.height).min(bounds.origin.y + bounds.size.height);
    rect(x0, y0, x1 - x0, y1 - y0)
}

/// The image as a button of fixed content height `height` measures it:
/// scaled to that height as the scaling allows.
fn to_height(image: NSSize, height: f64, scaling: NSImageScaling) -> NSSize {
    if scaling == NSImageScaling::ScaleAxesIndependently {
        return NSSize::new(image.width, height);
    }
    if scaling == NSImageScaling::ScaleNone || image.height <= 0.0 {
        return image;
    }
    let k = height / image.height;
    if scaling == NSImageScaling::ScaleProportionallyDown && k >= 1.0 {
        return image;
    }
    NSSize::new(image.width * k, height)
}

/// How a look lays out an image.
enum Family {
    Rounded,
    Square,
    /// Images aren't shown (help and push-disclosure buttons).
    None,
    Disclosure,
}

fn family(spec: &Spec) -> Family {
    match spec.look {
        Look::Help | Look::PushDisclosure | Look::Check | Look::Radio => Family::None,
        Look::Disclosure => Family::Disclosure,
        _ if !spec.bordered => Family::Square,
        Look::SmallSquare | Look::ShadowlessSquare | Look::TexturedSquare | Look::Toolbar => Family::Square,
        _ => Family::Rounded,
    }
}

/// Whether a button of this look shows an image at all.
pub(crate) fn shows_image(look: Look) -> bool {
    !matches!(look, Look::Help | Look::PushDisclosure | Look::Check | Look::Radio)
}

/// The title as laid out: a badge's is two points wider.
fn title_of(spec: &Spec) -> Option<NSSize> {
    let t = spec.title?;
    let extra = if spec.look == Look::Badge && spec.bordered { 2.0 } else { 0.0 };
    Some(NSSize::new(t.width + extra, t.height))
}

/// The image's place once there's no title to go beside.
fn place_of(spec: &Spec) -> Place {
    if spec.title.is_none() && spec.place == Place::Overlaps { Place::Only } else { spec.place }
}

/// The intrinsic size (rounded up to whole points) and `cellSize`.
pub(crate) fn sizes(spec: &Spec) -> (NSSize, NSSize) {
    match family(spec) {
        Family::Rounded => {
            let s = rounded_size(spec);
            (s, s)
        }
        Family::Square => {
            let raw = square_size(spec);
            let extra = if spec.bordered {
                match spec.look {
                    Look::TexturedSquare => metrics::TEXTURED_FRAME,
                    Look::Toolbar => metrics::TOOLBAR_FRAME,
                    Look::SmallSquare => metrics::SMALL_SQUARE_FRAME,
                    _ => (0.0, 0.0),
                }
            } else {
                (0.0, 0.0)
            };
            let intrinsic = NSSize::new((raw.width - 1e-9).ceil(), (raw.height - 1e-9).ceil());
            (intrinsic, NSSize::new(raw.width + extra.0, raw.height + extra.1))
        }
        Family::Disclosure => {
            let s = NSSize::new(metrics::DISCLOSURE, metrics::DISCLOSURE);
            (s, s)
        }
        Family::None => unreachable!("buttons of this look don't lay out images"),
    }
}

/// The rects for `bounds`.
pub(crate) fn rects(spec: &Spec, bounds: NSRect) -> Rects {
    let local = rect(0.0, 0.0, bounds.size.width, bounds.size.height);
    let r = match family(spec) {
        Family::Rounded => rounded_rects(spec, local),
        Family::Square => {
            let mut r = square_rects(spec, local);
            // An image not scaled is clipped by the bounds.
            if spec.scaling == NSImageScaling::ScaleNone && r.image != NSRect::ZERO {
                r.image = clipped(r.image, local);
            }
            r
        }
        Family::Disclosure => disclosure_rects(spec, local),
        Family::None => unreachable!("buttons of this look don't lay out images"),
    };
    let moved = |a: NSRect| {
        if a == NSRect::ZERO {
            a
        } else {
            NSRect::new(NSPoint::new(a.origin.x + bounds.origin.x, a.origin.y + bounds.origin.y), a.size)
        }
    };
    Rects { image: moved(r.image), title: moved(r.title), drawing: moved(r.drawing) }
}

// Rounded bezels.

/// Horizontal and vertical insets round the content, and a fixed content
/// height (a push button's).
fn rounded_insets(spec: &Spec) -> (f64, f64, Option<f64>) {
    let i = spec.size;
    match spec.look {
        Look::Push => (metrics::PUSH_HEIGHT[i] / 2.0, 0.0, Some(metrics::PUSH_HEIGHT[i])),
        Look::FlexiblePush => (metrics::PUSH_HEIGHT[i] / 2.0, metrics::ROUND_INSET[i], None),
        Look::Circular => (metrics::ROUND_INSET[i], metrics::ROUND_INSET[i], None),
        _ => (4.0, 2.0, None),
    }
}

fn rounded_size(spec: &Spec) -> NSSize {
    let (dx, dy, fixed) = rounded_insets(spec);
    let image = fixed.map_or(spec.image, |h| to_height(spec.image, h, spec.scaling));
    let content = match (place_of(spec), title_of(spec)) {
        (Place::Left | Place::Right, Some(t)) => NSSize::new(image.width + GAP + t.width, image.height.max(t.height)),
        (Place::Above | Place::Below, Some(t)) => NSSize::new(image.width.max(t.width), image.height + GAP + t.height),
        (Place::Overlaps, Some(t)) => NSSize::new(image.width.max(t.width), image.height.max(t.height)),
        _ => image,
    };
    let w = content.width + 2.0 * dx;
    let h = fixed.unwrap_or(content.height + 2.0 * dy);
    NSSize::new((w - 1e-9).ceil(), (h - 1e-9).ceil())
}

fn rounded_rects(spec: &Spec, b: NSRect) -> Rects {
    let (dx, dy, fixed) = rounded_insets(spec);
    let (bw, bh) = (b.size.width, b.size.height);
    let t = title_of(spec);
    let place = place_of(spec);
    // A titled mini push button's content sits half a point lower.
    let shift =
        if spec.look == Look::Push && spec.size == 2 && t.is_some() && place != Place::Only { 0.5 } else { 0.0 };
    let (cx, cw) = (dx, bw - 2.0 * dx);
    let (cy, ch) = match fixed {
        Some(h) => (half_up((bh - h) / 2.0), h),
        None => (dy, bh - 2.0 * dy),
    };
    let push = fixed.is_some();
    // Centered up and down: on the bounds for a push button.
    let middle = |h: f64| if push { (bh - h) / 2.0 + shift } else { cy + (ch - h) / 2.0 };
    let drawing = match fixed {
        Some(h) => rect(cx, half_up((bh - h) / 2.0 + shift), cw, h),
        None => rect(cx, cy, cw, ch),
    };
    let sc = spec.scaling;
    let none = NSRect::ZERO;
    let (image, title) = match (place, t) {
        (Place::Left | Place::Right, Some(t)) => {
            let (w, h, ix, tx);
            if spec.hugs {
                let s = fit(spec.image, NSSize::new(cw - GAP - t.width, ch), sc);
                (w, h) = (s.width, s.height);
                let gx = cx + (cw - (w + GAP + t.width)) / 2.0;
                (ix, tx) = if place == Place::Left { (gx, gx + w + GAP) } else { (gx + t.width + GAP, gx) };
            } else {
                let s = fit(spec.image, NSSize::new((cw - GAP) / 2.0, ch), sc);
                (w, h) = (s.width, s.height);
                let ax = if place == Place::Left { cx + w + GAP } else { cx };
                ix = if place == Place::Left { cx } else { cx + cw - w };
                tx = ax + (cw - w - GAP - t.width) / 2.0;
            }
            (rounded(ix, middle(h), w, h), rect(half_up(tx), half_up(middle(t.height)), t.width, t.height))
        }
        (Place::Above | Place::Below, Some(t)) => {
            let (w, h, iy, ty);
            if spec.hugs {
                let s = fit(spec.image, NSSize::new(cw, ch - GAP - t.height), sc);
                (w, h) = (s.width, s.height);
                let gy = cy + (ch - (h + GAP + t.height)) / 2.0;
                (iy, ty) = if place == Place::Above { (gy, gy + h + GAP) } else { (gy + t.height + GAP, gy) };
            } else {
                let s = fit(spec.image, NSSize::new(cw, (ch - GAP) / 2.0), sc);
                (w, h) = (s.width, s.height);
                let ay = if place == Place::Above { cy + h + GAP } else { cy };
                iy = if place == Place::Above { cy } else { cy + ch - h };
                ty = ay + (ch - h - GAP - t.height) / 2.0;
            }
            let title = rect(half_up((bw - t.width) / 2.0), half_up(ty), t.width, t.height);
            (rounded(cx + (cw - w) / 2.0, iy, w, h), title)
        }
        _ => {
            let s = fit(spec.image, NSSize::new(cw, ch), sc);
            let (w, h) = (s.width, s.height);
            let (x, y) = match place {
                Place::Left => (cx, middle(h)),
                Place::Right => (cx + cw - w, middle(h)),
                Place::Above => (cx + (cw - w) / 2.0, cy),
                Place::Below => (cx + (cw - w) / 2.0, cy + ch - h),
                _ => (cx + (cw - w) / 2.0, middle(h)),
            };
            let title = match t {
                Some(t) if place == Place::Overlaps => {
                    rect(half_up((bw - t.width) / 2.0), half_up((bh - t.height) / 2.0 + shift), t.width, t.height)
                }
                _ => none,
            };
            (rounded(x, y, w, h), title)
        }
    };
    Rects { image, title, drawing }
}

// Square bezels and borderless buttons.

/// A square look's insets: the room an image alone adds across, what a
/// title beside an image adds, the room up and down (none for fixed
/// heights), and the content's left and top insets with the image alone,
/// beside the title and above or below it.
struct Square {
    across: f64,
    beside: f64,
    down: f64,
    alone: (f64, f64),
    side: (f64, f64),
    vertical: (f64, f64),
}

fn square(spec: &Spec) -> Square {
    let i = spec.size;
    if !spec.bordered {
        return Square {
            across: 0.0,
            beside: 0.0,
            down: 0.0,
            alone: (0.0, 0.0),
            side: (0.0, 0.0),
            vertical: (0.0, 0.0),
        };
    }
    match spec.look {
        Look::ShadowlessSquare => {
            Square { across: 6.0, beside: 4.0, down: 6.0, alone: (3.0, 3.0), side: (5.0, 3.0), vertical: (3.0, 5.0) }
        }
        Look::SmallSquare => {
            Square { across: 2.0, beside: 4.0, down: 4.0, alone: (1.0, 3.0), side: (3.0, 3.0), vertical: (1.0, 5.0) }
        }
        Look::TexturedSquare => {
            let e = metrics::TEXTURED_PAD[i] / 2.0;
            Square {
                across: metrics::TEXTURED_PAD[i] - 4.0,
                beside: 4.0,
                down: 0.0,
                alone: (e, 2.0),
                side: (e + 2.0, 2.0),
                vertical: (e, 4.0),
            }
        }
        // The toolbar button: a fixed height.
        _ => {
            let e = metrics::TOOLBAR_PAD[i] / 2.0 - 1.0;
            Square {
                across: metrics::TOOLBAR_PAD[i] - 4.0,
                beside: 0.0,
                down: 0.0,
                alone: (3.0, 0.0),
                side: (e, 0.0),
                vertical: (3.0, 0.0),
            }
        }
    }
}

/// A toolbar button's image, scaled to the room its height leaves.
fn square_image(spec: &Spec) -> NSSize {
    if spec.bordered && spec.look == Look::Toolbar {
        to_height(spec.image, metrics::TOOLBAR_HEIGHT[spec.size] - 2.0, spec.scaling)
    } else {
        spec.image
    }
}

/// Whether a toolbar button drops its image for a title above or below it.
fn drops_image(spec: &Spec) -> bool {
    spec.bordered
        && spec.look == Look::Toolbar
        && spec.title.is_some()
        && matches!(spec.place, Place::Above | Place::Below)
}

/// The square family's size, unrounded.
fn square_size(spec: &Spec) -> NSSize {
    let q = square(spec);
    let i = spec.size;
    let image = square_image(spec);
    let t = title_of(spec);
    let place = place_of(spec);
    if drops_image(spec) {
        let t = t.unwrap_or(NSSize::ZERO);
        return NSSize::new(t.width + metrics::TOOLBAR_PAD[i], metrics::TOOLBAR_HEIGHT[i]);
    }
    let (w, content, vertical) = match (place, t) {
        (Place::Left | Place::Right, Some(t)) => {
            (q.across + q.beside + image.width + GAP + t.width + 4.0, image.height.max(t.height), false)
        }
        (Place::Above | Place::Below, Some(t)) => {
            (image.width.max(t.width + 4.0) + q.across, image.height + GAP + t.height, true)
        }
        (Place::Overlaps, Some(t)) => (image.width.max(t.width + 4.0) + q.across, image.height.max(t.height), false),
        _ => (image.width + q.across, image.height, false),
    };
    let h = if !spec.bordered {
        content
    } else {
        match spec.look {
            Look::Toolbar => metrics::TOOLBAR_HEIGHT[i],
            Look::TexturedSquare if i == 1 => metrics::TEXTURED_HEIGHT[1],
            Look::TexturedSquare if vertical => content + 3.0,
            Look::TexturedSquare => metrics::TEXTURED_HEIGHT[i].max(content - 1.0),
            _ => content + q.down + if vertical { 4.0 } else { 0.0 },
        }
    };
    NSSize::new(w, h)
}

/// A title alone in a square button: a band across it.
fn square_title_alone(spec: &Spec, t: NSSize, bw: f64, bh: f64) -> NSRect {
    let i = spec.size;
    let y = (bh - t.height) / 2.0;
    if !spec.bordered {
        return rect(0.0, y, bw, t.height);
    }
    match spec.look {
        Look::ShadowlessSquare => rect(3.0, y, bw - 6.0, t.height),
        Look::SmallSquare => rect(1.0, y, bw - 2.0, t.height),
        Look::TexturedSquare => {
            let pad = metrics::TEXTURED_PAD[i];
            rect(pad / 2.0, y, bw - pad, t.height)
        }
        _ => {
            let pad = metrics::TOOLBAR_PAD[i];
            rect(pad / 2.0 - 1.0, y - 1.5, bw - pad + 2.0, t.height)
        }
    }
}

fn square_rects(spec: &Spec, b: NSRect) -> Rects {
    let (bw, bh) = (b.size.width, b.size.height);
    let q = square(spec);
    let i = spec.size;
    let t = title_of(spec);
    let place = place_of(spec);
    let toolbar = spec.bordered && spec.look == Look::Toolbar;
    let (image_size, sc) = (spec.image, spec.scaling);
    let (alone, side, vertical) = match (place, t) {
        (Place::Left | Place::Right, Some(_)) => (false, true, false),
        (Place::Above | Place::Below, Some(_)) => (false, false, true),
        _ => (true, false, false),
    };
    let (lx, top) = if side {
        q.side
    } else if vertical {
        q.vertical
    } else {
        q.alone
    };
    let (cy, ch) = if toolbar {
        let h = metrics::TOOLBAR_HEIGHT[i];
        ((bh - h) / 2.0 + 0.5, h - 2.0)
    } else {
        (top, bh - 2.0 * top)
    };
    let (cx, cw) = (lx, bw - 2.0 * lx);
    let drawing = if toolbar {
        let h = metrics::TOOLBAR_HEIGHT[i];
        rect(3.0, (bh - h) / 2.0 - 0.5, bw - 6.0, h)
    } else {
        rect(cx, cy, cw, ch)
    };
    let title_y = |th: f64| (bh - th) / 2.0 - if toolbar { 1.5 } else { 0.0 };
    let none = NSRect::ZERO;
    if drops_image(spec) {
        let t = t.unwrap_or(NSSize::ZERO);
        return Rects { image: none, title: square_title_alone(spec, t, bw, bh), drawing };
    }
    if alone {
        let (mut image, quirk) = place_alone(spec, place, rect(cx, cy, cw, ch));
        if toolbar && matches!(place, Place::Left | Place::Right) {
            let w = image.size.width;
            let e = (metrics::TOOLBAR_PAD[i] / 2.0 - 1.0).min((bw - w) / 2.0);
            image.origin.x = if place == Place::Left { e } else { bw - e - w };
        }
        let title = match (place, t) {
            (Place::Overlaps, Some(t)) => square_title_alone(spec, t, bw, bh),
            _ => quirk,
        };
        return Rects { image, title, drawing };
    }
    let t = t.unwrap_or(NSSize::ZERO);
    if side {
        let s = fit(image_size, NSSize::new(cw - GAP - t.width, ch), sc);
        let (w, h) = (s.width, s.height);
        let iy = cy + (ch - h) / 2.0;
        let (image, title) = if spec.hugs {
            let gx = cx + (cw - (w + GAP + t.width)) / 2.0;
            let (ix, tx) = if place == Place::Left { (gx, gx + w + GAP) } else { (gx + t.width + GAP, gx) };
            (rect(ix, iy, w, h), rect(tx, title_y(t.height), t.width, t.height))
        } else {
            let (ix, ax) = if place == Place::Left { (cx, cx + w + GAP) } else { (cx + cw - w, cx) };
            (rect(ix, iy, w, h), rect(ax, title_y(t.height), cw - w - GAP, t.height))
        };
        return Rects { image, title, drawing };
    }
    // Above or below the title.
    let s = fit(image_size, NSSize::new(cw, ch - GAP - t.height), sc);
    let (w, h) = (s.width, s.height);
    if w <= 0.0 || h <= 0.0 {
        // No room for the image: the title alone, and, hugging, an empty
        // image rect at its edge.
        let title = square_title_alone(spec, t, bw, bh);
        let edge = title.origin.y + if place == Place::Below { t.height } else { 0.0 };
        let image = if spec.hugs { rect(0.0, edge, 0.0, 0.0) } else { none };
        return Rects { image, title, drawing };
    }
    let ix = cx + (cw - w) / 2.0;
    let (iy, ty) = if spec.hugs {
        let gy = cy + (ch - (h + t.height)) / 2.0;
        if place == Place::Above { (gy, gy + h) } else { (gy + t.height, gy) }
    } else {
        let (iy, ay) = if place == Place::Above { (cy, cy + h + GAP) } else { (cy + ch - h, cy) };
        (iy, ay + (ch - h - GAP - t.height) / 2.0)
    };
    Rects { image: rect(ix, iy, w, h), title: rect(cx, ty, cw, t.height), drawing }
}

/// An image with no title beside it, in the content rect `c`: scaled
/// into it, at the edge its place names or else in the middle. Hugging a
/// title that isn't there, an image on the right goes to the left, and one
/// above or below to the middle, with an empty title rect at its edge
/// (the second rect), as AppKit answers.
fn place_alone(spec: &Spec, place: Place, c: NSRect) -> (NSRect, NSRect) {
    let (cx, cy, cw, ch) = (c.origin.x, c.origin.y, c.size.width, c.size.height);
    let s = fit(spec.image, c.size, spec.scaling);
    let (w, h) = (s.width, s.height);
    let (mut x, mut y) = match place {
        Place::Left => (cx, cy + (ch - h) / 2.0),
        Place::Right => (cx + cw - w, cy + (ch - h) / 2.0),
        Place::Above => (cx + (cw - w) / 2.0, cy),
        Place::Below => (cx + (cw - w) / 2.0, cy + ch - h),
        _ => (cx + (cw - w) / 2.0, cy + (ch - h) / 2.0),
    };
    if spec.hugs && matches!(place, Place::Above | Place::Below) {
        y = cy + (ch - h) / 2.0;
    }
    if spec.hugs && place == Place::Right {
        x = cx;
    }
    let title = match place {
        Place::Above if spec.hugs => rect(0.0, y + h, 0.0, 0.0),
        Place::Below if spec.hugs => rect(0.0, y, 0.0, 0.0),
        _ => NSRect::ZERO,
    };
    (rect(x, y, w, h), title)
}

// Disclosure buttons: a 13-point square in the middle of the bounds.

fn disclosure_rects(spec: &Spec, b: NSRect) -> Rects {
    let side = metrics::DISCLOSURE;
    let (bw, bh) = (b.size.width, b.size.height);
    let square = rect((bw - side) / 2.0, (bh - side) / 2.0, side, side);
    let (sx, sy) = (square.origin.x, square.origin.y);
    let place = place_of(spec);
    // A title goes across the square, centered up and down on the bounds.
    let band = |t: NSSize| rect(sx, (bh - t.height) / 2.0, side, t.height);
    match (place, title_of(spec)) {
        // Beside a title, the image goes at its edge of the square less 2
        // points each side, and the title in what it leaves, if anything.
        // (Hugging a title left no room at all, the image is in the middle
        // and the title an empty rect half the gap off the origin.)
        (Place::Left | Place::Right, Some(t)) => {
            let area = rect(sx + 2.0, sy, side - 4.0, side);
            let s = fit(spec.image, area.size, spec.scaling);
            let left = place == Place::Left;
            let rest = area.size.width - s.width - GAP;
            let squeezed = spec.hugs && rest.abs() < 1e-9;
            let x = match () {
                _ if squeezed => area.origin.x + (area.size.width - s.width) / 2.0,
                _ if left => area.origin.x,
                _ => area.origin.x + area.size.width - s.width,
            };
            let image = rect(x, sy + (side - s.height) / 2.0, s.width, s.height);
            let title = if squeezed {
                rect(if left { GAP / 2.0 } else { -GAP / 2.0 }, 0.0, 0.0, 0.0)
            } else if rest > 0.0 {
                let tx = if left { x + s.width + GAP } else { area.origin.x };
                rect(tx, (bh - t.height) / 2.0, rest, t.height)
            } else {
                NSRect::ZERO
            };
            Rects { image, title, drawing: area }
        }
        // Above or below a title, there's no room for the image: the title
        // alone, and, hugging, an empty image rect at its edge.
        (Place::Above | Place::Below, Some(t)) => {
            let title = band(t);
            let edge = title.origin.y + if place == Place::Below { t.height } else { 0.0 };
            let image = if spec.hugs { rect(0.0, edge, 0.0, 0.0) } else { NSRect::ZERO };
            Rects { image, title, drawing: rect(sx, sy + 2.0, side, side - 4.0) }
        }
        (place, t) => {
            let (image, quirk) = place_alone(spec, place, square);
            let title = match (place, t) {
                (Place::Overlaps, Some(t)) => band(t),
                _ => quirk,
            };
            Rects { image, title, drawing: square }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(look: Look, title: Option<(f64, f64)>, image: (f64, f64), place: Place) -> Spec {
        Spec {
            look,
            size: 0,
            bordered: true,
            title: title.map(|(w, h)| NSSize::new(w, h)),
            image: NSSize::new(image.0, image.1),
            place,
            hugs: false,
            scaling: NSImageScaling::ScaleProportionallyDown,
        }
    }

    // Values measured on macOS (see the module documentation).

    #[test]
    fn push_buttons_scale_images_to_their_height() {
        let s = spec(Look::Push, None, (60.0, 60.0), Place::Only);
        assert_eq!(sizes(&s).0, NSSize::new(48.0, 24.0));
        let r = rects(&s, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.drawing), (rect(68.0, 18.0, 24.0, 24.0), rect(12.0, 18.0, 136.0, 24.0)));
        // A title beside: the image gets half the content, less the gap.
        let s = spec(Look::Push, Some((9.0, 16.0)), (60.0, 60.0), Place::Left);
        assert_eq!(sizes(&s).0, NSSize::new(59.0, 24.0));
        let r = rects(&s, rect(0.0, 0.0, 59.0, 24.0));
        assert_eq!((r.image, r.title), (rect(12.0, 4.0, 17.0, 16.0), rect(34.0, 4.0, 9.0, 16.0)));
    }

    #[test]
    fn flexible_buttons_grow_with_their_images() {
        let s = spec(Look::FlexiblePush, Some((42.0, 16.0)), (10.0, 30.0), Place::Below);
        assert_eq!(sizes(&s).0, NSSize::new(66.0, 56.0));
        let r = rects(&s, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.title), (rect(76.0, 31.0, 8.0, 25.0), rect(59.0, 9.0, 42.0, 16.0)));
        let hugging = Spec { hugs: true, ..spec(Look::FlexiblePush, Some((42.0, 16.0)), (16.0, 16.0), Place::Left) };
        let r = rects(&hugging, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.title), (rect(50.0, 22.0, 16.0, 16.0), rect(68.0, 22.0, 42.0, 16.0)));
    }

    #[test]
    fn square_buttons_give_the_title_a_band() {
        let s = spec(Look::ShadowlessSquare, Some((42.0, 16.0)), (16.0, 16.0), Place::Left);
        assert_eq!(sizes(&s).0, NSSize::new(74.0, 22.0));
        let r = rects(&s, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.title), (rect(5.0, 22.0, 16.0, 16.0), rect(23.0, 22.0, 132.0, 16.0)));
        let s = spec(Look::ShadowlessSquare, Some((42.0, 16.0)), (60.0, 60.0), Place::Below);
        let r = rects(&s, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.title), (rect(64.0, 23.0, 32.0, 32.0), rect(3.0, 5.0, 154.0, 16.0)));
        let borderless = Spec { bordered: false, ..spec(Look::Push, None, (16.0, 16.0), Place::Only) };
        assert_eq!(rects(&borderless, rect(0.0, 0.0, 61.0, 33.0)).image, rect(22.5, 8.5, 16.0, 16.0));
    }

    #[test]
    fn disclosure_buttons_lay_out_in_their_square() {
        let near = |a: NSRect, b: NSRect| {
            let d = [a.origin.x - b.origin.x, a.origin.y - b.origin.y, a.size.width - b.size.width];
            d.iter().chain([a.size.height - b.size.height].iter()).all(|v| v.abs() < 1e-9)
        };
        let b = rect(0.0, 0.0, 13.0, 13.0);
        // Untitled, the image takes the square, at the edge it names.
        let r = rects(&spec(Look::Disclosure, None, (16.0, 16.0), Place::Left), b);
        assert_eq!((r.image, r.drawing), (b, b));
        let r = rects(&spec(Look::Disclosure, None, (40.0, 12.0), Place::Below), b);
        assert!(near(r.image, rect(0.0, 9.1, 13.0, 3.9)), "{:?}", r.image);
        // Beside a title, it fits the square less 2 points each side; the
        // title takes what's left.
        let r = rects(&spec(Look::Disclosure, Some((42.0, 16.0)), (10.0, 30.0), Place::Left), b);
        assert!(near(r.image, rect(2.0, 0.0, 13.0 / 3.0, 13.0)), "{:?}", r.image);
        assert!(near(r.title, rect(2.0 + 13.0 / 3.0 + 2.0, -1.5, 9.0 - 13.0 / 3.0 - 2.0, 16.0)), "{:?}", r.title);
        assert_eq!(r.drawing, rect(2.0, 0.0, 9.0, 13.0));
        // Above a title, no room for it.
        let s = Spec { hugs: true, ..spec(Look::Disclosure, Some((42.0, 16.0)), (16.0, 16.0), Place::Above) };
        let r = rects(&s, rect(0.0, 0.0, 160.0, 60.0));
        assert_eq!((r.image, r.title), (rect(0.0, 22.0, 0.0, 0.0), rect(73.5, 22.0, 13.0, 16.0)));
        assert_eq!(r.drawing, rect(73.5, 25.5, 13.0, 9.0));
    }

    #[test]
    fn toolbar_buttons_drop_an_image_above_a_title() {
        let s = spec(Look::Toolbar, Some((42.0, 16.0)), (16.0, 16.0), Place::Above);
        assert_eq!(sizes(&s).0, NSSize::new(56.0, 20.0));
        assert_eq!(rects(&s, rect(0.0, 0.0, 56.0, 20.0)).image, NSRect::ZERO);
    }
}
