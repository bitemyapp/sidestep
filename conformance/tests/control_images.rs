//! Images in buttons, checked on macOS and on Linux alike without a
//! window: the factories and defaults, where the image and title go for
//! every image position, bezel, control size and `imageHugsTitle` (and the
//! other scalings for images alone), and how images draw by state:
//! template images tinted, others faded when disabled, the alternate
//! image shown with the alternate contents.
//!
//! The geometry was measured on macOS 26 for thousands of cases; the
//! oracle below holds it as formulas over the title's measured size (so
//! it holds whatever fonts a system has) and the image's. Running this
//! file on macOS checks the oracle against AppKit, and on Linux checks
//! Sidestep against the oracle. As in `controls.rs`, the geometry is
//! pinned on a 2x screen and only reported on a 1x one, where AppKit
//! rounds some rects differently.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

// The square bezel names are deprecated, and programs still use them.
#![allow(deprecated)]

mod common;

use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::*;
use objc2_foundation::{NSDictionary, NSRect, NSSize, NSString};

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker));

fn size(w: f64, h: f64) -> NSSize {
    NSSize::new(w, h)
}

fn image(w: f64, h: f64) -> Retained<NSImage> {
    NSImage::initWithSize(NSImage::alloc(), size(w, h))
}

/// An image of `w` × `h` points filled with `color` (sRGB components).
fn filled(w: f64, h: f64, color: [f64; 4]) -> Retained<NSImage> {
    let image = image(w, h);
    #[allow(deprecated)]
    image.lockFocus();
    NSColor::colorWithSRGBRed_green_blue_alpha(color[0], color[1], color[2], color[3]).set();
    NSRectFill(rect(0.0, 0.0, w, h));
    #[allow(deprecated)]
    image.unlockFocus();
    image
}

fn text_size(text: &str, font_size: f64) -> NSSize {
    let font = NSFont::systemFontOfSize(font_size);
    // SAFETY: the key is a constant string.
    let key = unsafe { NSFontAttributeName };
    let attrs = NSDictionary::from_slices(&[key], &[&*font as &AnyObject]);
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attrs)) }
}

const SIZES: [NSControlSize; 4] =
    [NSControlSize::Regular, NSControlSize::Small, NSControlSize::Mini, NSControlSize::Large];
const FONT: [f64; 4] = [13.0, 11.0, 9.0, 13.0];

// The oracle: what AppKit answers, as measured.

const PUSH_HEIGHT: [f64; 4] = [24.0, 20.0, 16.0, 28.0];
const ROUND_INSET: [f64; 4] = [4.0, 3.0, 1.0, 6.0];
const TEXTURED_PAD: [f64; 4] = [8.0, 6.0, 6.0, 8.0];
const TEXTURED_HEIGHT: [f64; 4] = [20.0, 14.0, 11.0, 20.0];
const TOOLBAR_PAD: [f64; 4] = [14.0, 12.0, 10.0, 14.0];
const TOOLBAR_HEIGHT: [f64; 4] = [20.0, 16.0, 13.0, 20.0];
const GAP: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Look {
    Push,
    Flexible,
    Circular,
    SmallSquare,
    Shadowless,
    Textured,
    Toolbar,
    Disclosure,
    Borderless,
}

impl Look {
    fn bezel(self) -> NSBezelStyle {
        match self {
            Look::Push | Look::Borderless => NSBezelStyle::Push,
            Look::Flexible => NSBezelStyle::FlexiblePush,
            Look::Circular => NSBezelStyle::Circular,
            Look::SmallSquare => NSBezelStyle::SmallSquare,
            Look::Shadowless => NSBezelStyle::ShadowlessSquare,
            Look::Textured => NSBezelStyle::TexturedSquare,
            Look::Toolbar => NSBezelStyle::Toolbar,
            Look::Disclosure => NSBezelStyle::Disclosure,
        }
    }

    fn rounded(self) -> bool {
        matches!(self, Look::Push | Look::Flexible | Look::Circular)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Only,
    Left,
    Right,
    Above,
    Below,
    Overlaps,
}

struct Case {
    look: Look,
    i: usize,
    title: Option<NSSize>,
    image: NSSize,
    place: Place,
    hugs: bool,
    scaling: NSImageScaling,
}

struct Want {
    intrinsic: NSSize,
    cell: NSSize,
}

fn half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

fn rounded(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    let (x0, y0, x1, y1) = (half_up(x), half_up(y), half_up(x + w), half_up(y + h));
    rect(x0, y0, x1 - x0, y1 - y0)
}

fn fit(image: NSSize, area: NSSize, scaling: NSImageScaling) -> NSSize {
    match scaling {
        NSImageScaling::ScaleNone => image,
        NSImageScaling::ScaleAxesIndependently => area,
        _ => {
            let k = (area.width / image.width).min(area.height / image.height);
            if scaling == NSImageScaling::ScaleProportionallyDown && k >= 1.0 {
                image
            } else {
                size(image.width * k, image.height * k)
            }
        }
    }
}

fn to_height(image: NSSize, h: f64, scaling: NSImageScaling) -> NSSize {
    match scaling {
        NSImageScaling::ScaleAxesIndependently => size(image.width, h),
        NSImageScaling::ScaleNone => image,
        _ if scaling == NSImageScaling::ScaleProportionallyDown && h >= image.height => image,
        _ => size(image.width * h / image.height, h),
    }
}

fn ceil(s: NSSize) -> NSSize {
    size((s.width - 1e-9).ceil(), (s.height - 1e-9).ceil())
}

/// The square looks' insets: across for an image alone, added beside a
/// title, up and down, and the content's left and top insets alone,
/// beside and above or below.
fn square(look: Look, i: usize) -> ([f64; 3], [(f64, f64); 3]) {
    match look {
        Look::Shadowless => ([6.0, 4.0, 6.0], [(3.0, 3.0), (5.0, 3.0), (3.0, 5.0)]),
        Look::SmallSquare => ([2.0, 4.0, 4.0], [(1.0, 3.0), (3.0, 3.0), (1.0, 5.0)]),
        Look::Textured => {
            let e = TEXTURED_PAD[i] / 2.0;
            ([TEXTURED_PAD[i] - 4.0, 4.0, 0.0], [(e, 2.0), (e + 2.0, 2.0), (e, 4.0)])
        }
        Look::Toolbar => {
            ([TOOLBAR_PAD[i] - 4.0, 0.0, 0.0], [(3.0, 0.0), (TOOLBAR_PAD[i] / 2.0 - 1.0, 0.0), (3.0, 0.0)])
        }
        _ => ([0.0; 3], [(0.0, 0.0); 3]),
    }
}

/// A disclosure button's square.
const DISCLOSURE: f64 = 13.0;

fn sizes(c: &Case) -> Want {
    let i = c.i;
    if c.look == Look::Disclosure {
        let s = size(DISCLOSURE, DISCLOSURE);
        return Want { intrinsic: s, cell: s };
    }
    if c.look.rounded() {
        let (dx, dy, fixed) = match c.look {
            Look::Push => (PUSH_HEIGHT[i] / 2.0, 0.0, Some(PUSH_HEIGHT[i])),
            Look::Flexible => (PUSH_HEIGHT[i] / 2.0, ROUND_INSET[i], None),
            _ => (ROUND_INSET[i], ROUND_INSET[i], None),
        };
        let im = fixed.map_or(c.image, |h| to_height(c.image, h, c.scaling));
        let content = match (c.place, c.title) {
            (Place::Left | Place::Right, Some(t)) => size(im.width + GAP + t.width, im.height.max(t.height)),
            (Place::Above | Place::Below, Some(t)) => size(im.width.max(t.width), im.height + GAP + t.height),
            (Place::Overlaps, Some(t)) => size(im.width.max(t.width), im.height.max(t.height)),
            _ => im,
        };
        let s = ceil(size(content.width + 2.0 * dx, fixed.unwrap_or(content.height + 2.0 * dy)));
        return Want { intrinsic: s, cell: s };
    }
    let ([across, beside, down], _) = square(c.look, i);
    let im = if c.look == Look::Toolbar { to_height(c.image, TOOLBAR_HEIGHT[i] - 2.0, c.scaling) } else { c.image };
    let (w, content, vertical) = match (c.place, c.title) {
        (Place::Left | Place::Right, Some(t)) => {
            (across + beside + im.width + GAP + t.width + 4.0, im.height.max(t.height), false)
        }
        (Place::Above | Place::Below, Some(t)) => {
            (im.width.max(t.width + 4.0) + across, im.height + GAP + t.height, true)
        }
        (Place::Overlaps, Some(t)) => (im.width.max(t.width + 4.0) + across, im.height.max(t.height), false),
        _ => (im.width + across, im.height, false),
    };
    let h = match c.look {
        Look::Borderless => content,
        Look::Toolbar => TOOLBAR_HEIGHT[i],
        Look::Textured if i == 1 => TEXTURED_HEIGHT[1],
        Look::Textured if vertical => content + 3.0,
        Look::Textured => TEXTURED_HEIGHT[i].max(content - 1.0),
        _ => content + down + if vertical { 4.0 } else { 0.0 },
    };
    let extra = match c.look {
        Look::SmallSquare => (0.0, 2.0),
        Look::Textured => (4.0, 5.0),
        Look::Toolbar => (2.0, 3.0),
        _ => (0.0, 0.0),
    };
    Want { intrinsic: ceil(size(w, h)), cell: size(w + extra.0, h + extra.1) }
}

/// An image alone in the content rect (`cx`, `cy`, `cw`, `ch`): at the
/// edge its place names or in the middle; hugging, one on the right goes
/// to the left and one above or below to the middle, with an empty title
/// rect at its edge.
fn image_alone(c: &Case, cx: f64, cy: f64, cw: f64, ch: f64) -> (NSRect, NSRect) {
    let s = fit(c.image, size(cw, ch), c.scaling);
    let (mut x, mut y) = match c.place {
        Place::Left => (cx, cy + (ch - s.height) / 2.0),
        Place::Right => (cx + cw - s.width, cy + (ch - s.height) / 2.0),
        Place::Above => (cx + (cw - s.width) / 2.0, cy),
        Place::Below => (cx + (cw - s.width) / 2.0, cy + ch - s.height),
        _ => (cx + (cw - s.width) / 2.0, cy + (ch - s.height) / 2.0),
    };
    if c.hugs && matches!(c.place, Place::Above | Place::Below) {
        y = cy + (ch - s.height) / 2.0;
    }
    if c.hugs && c.place == Place::Right {
        x = cx;
    }
    let title = match c.place {
        Place::Above if c.hugs => rect(0.0, y + s.height, 0.0, 0.0),
        Place::Below if c.hugs => rect(0.0, y, 0.0, 0.0),
        _ => NSRect::ZERO,
    };
    (rect(x, y, s.width, s.height), title)
}

/// A disclosure button's rects: its square in the middle of the bounds,
/// laid out as a square bezel's content, but beside a title the image
/// fits the square less 2 points each side and the title takes the rest
/// (none if nothing's left), and above or below one there's no room for
/// the image.
fn disclosure_rects(c: &Case, bw: f64, bh: f64) -> (NSRect, NSRect, NSRect) {
    let side = DISCLOSURE;
    let (sx, sy) = ((bw - side) / 2.0, (bh - side) / 2.0);
    let band = |t: NSSize| rect(sx, (bh - t.height) / 2.0, side, t.height);
    match (c.place, c.title) {
        (Place::Left | Place::Right, Some(t)) => {
            let s = fit(c.image, size(side - 4.0, side), c.scaling);
            let left = c.place == Place::Left;
            let rest = side - 4.0 - s.width - GAP;
            // Hugging a title with no room at all: the image in the middle,
            // an empty title rect half the gap off the origin.
            let squeezed = c.hugs && rest.abs() < 1e-9;
            let x = if squeezed {
                sx + 2.0 + (side - 4.0 - s.width) / 2.0
            } else if left {
                sx + 2.0
            } else {
                sx + side - 2.0 - s.width
            };
            let title = if squeezed {
                rect(if left { GAP / 2.0 } else { -GAP / 2.0 }, 0.0, 0.0, 0.0)
            } else if rest > 0.0 {
                rect(if left { x + s.width + GAP } else { sx + 2.0 }, (bh - t.height) / 2.0, rest, t.height)
            } else {
                NSRect::ZERO
            };
            (rect(x, sy + (side - s.height) / 2.0, s.width, s.height), title, rect(sx + 2.0, sy, side - 4.0, side))
        }
        (Place::Above | Place::Below, Some(t)) => {
            let title = band(t);
            let edge = title.origin.y + if c.place == Place::Below { t.height } else { 0.0 };
            let image = if c.hugs { rect(0.0, edge, 0.0, 0.0) } else { NSRect::ZERO };
            (image, title, rect(sx, sy + 2.0, side, side - 4.0))
        }
        (_, t) => {
            let (image, quirk) = image_alone(c, sx, sy, side, side);
            let title = match t {
                Some(t) if c.place == Place::Overlaps => band(t),
                _ => quirk,
            };
            (image, title, rect(sx, sy, side, side))
        }
    }
}

/// The image, title and drawing rects in bounds `bw` × `bh`.
fn rects(c: &Case, bw: f64, bh: f64) -> (NSRect, NSRect, NSRect) {
    let i = c.i;
    let sc = c.scaling;
    let none = NSRect::ZERO;
    if c.look == Look::Disclosure {
        return disclosure_rects(c, bw, bh);
    }
    if c.look.rounded() {
        let (dx, dy, fixed) = match c.look {
            Look::Push => (PUSH_HEIGHT[i] / 2.0, 0.0, Some(PUSH_HEIGHT[i])),
            Look::Flexible => (PUSH_HEIGHT[i] / 2.0, ROUND_INSET[i], None),
            _ => (ROUND_INSET[i], ROUND_INSET[i], None),
        };
        let shift =
            if c.look == Look::Push && i == 2 && c.title.is_some() && c.place != Place::Only { 0.5 } else { 0.0 };
        let (cx, cw) = (dx, bw - 2.0 * dx);
        let (cy, ch) = fixed.map_or((dy, bh - 2.0 * dy), |h| (half_up((bh - h) / 2.0), h));
        let middle = |h: f64| if fixed.is_some() { (bh - h) / 2.0 + shift } else { cy + (ch - h) / 2.0 };
        let drawing = fixed.map_or(rect(cx, cy, cw, ch), |h| rect(cx, half_up((bh - h) / 2.0 + shift), cw, h));
        return match (c.place, c.title) {
            (Place::Left | Place::Right, Some(t)) => {
                let left = c.place == Place::Left;
                let (s, ix, tx);
                if c.hugs {
                    s = fit(c.image, size(cw - GAP - t.width, ch), sc);
                    let gx = cx + (cw - (s.width + GAP + t.width)) / 2.0;
                    (ix, tx) = if left { (gx, gx + s.width + GAP) } else { (gx + t.width + GAP, gx) };
                } else {
                    s = fit(c.image, size((cw - GAP) / 2.0, ch), sc);
                    ix = if left { cx } else { cx + cw - s.width };
                    let ax = if left { cx + s.width + GAP } else { cx };
                    tx = ax + (cw - s.width - GAP - t.width) / 2.0;
                }
                let title = rect(half_up(tx), half_up(middle(t.height)), t.width, t.height);
                (rounded(ix, middle(s.height), s.width, s.height), title, drawing)
            }
            (Place::Above | Place::Below, Some(t)) => {
                let above = c.place == Place::Above;
                let (s, iy, ty);
                if c.hugs {
                    s = fit(c.image, size(cw, ch - GAP - t.height), sc);
                    let gy = cy + (ch - (s.height + GAP + t.height)) / 2.0;
                    (iy, ty) = if above { (gy, gy + s.height + GAP) } else { (gy + t.height + GAP, gy) };
                } else {
                    s = fit(c.image, size(cw, (ch - GAP) / 2.0), sc);
                    iy = if above { cy } else { cy + ch - s.height };
                    let ay = if above { cy + s.height + GAP } else { cy };
                    ty = ay + (ch - s.height - GAP - t.height) / 2.0;
                }
                let title = rect(half_up((bw - t.width) / 2.0), half_up(ty), t.width, t.height);
                (rounded(cx + (cw - s.width) / 2.0, iy, s.width, s.height), title, drawing)
            }
            (place, t) => {
                let s = fit(c.image, size(cw, ch), sc);
                let (x, y) = match place {
                    Place::Left => (cx, middle(s.height)),
                    Place::Right => (cx + cw - s.width, middle(s.height)),
                    Place::Above => (cx + (cw - s.width) / 2.0, cy),
                    Place::Below => (cx + (cw - s.width) / 2.0, cy + ch - s.height),
                    _ => (cx + (cw - s.width) / 2.0, middle(s.height)),
                };
                let title = match t {
                    Some(t) if place == Place::Overlaps => {
                        rect(half_up((bw - t.width) / 2.0), half_up((bh - t.height) / 2.0 + shift), t.width, t.height)
                    }
                    _ => none,
                };
                (rounded(x, y, s.width, s.height), title, drawing)
            }
        };
    }
    // Square looks and borderless buttons.
    let (_, [alone, beside, vertical]) = square(c.look, i);
    let toolbar = c.look == Look::Toolbar;
    let which = match (c.place, c.title) {
        (Place::Left | Place::Right, Some(_)) => 1,
        (Place::Above | Place::Below, Some(_)) => 2,
        _ => 0,
    };
    let (lx, top) = [alone, beside, vertical][which];
    let (cy, ch) =
        if toolbar { ((bh - TOOLBAR_HEIGHT[i]) / 2.0 + 0.5, TOOLBAR_HEIGHT[i] - 2.0) } else { (top, bh - 2.0 * top) };
    let (cx, cw) = (lx, bw - 2.0 * lx);
    let drawing = if toolbar {
        rect(3.0, (bh - TOOLBAR_HEIGHT[i]) / 2.0 - 0.5, bw - 6.0, TOOLBAR_HEIGHT[i])
    } else {
        rect(cx, cy, cw, ch)
    };
    let title_y = |th: f64| (bh - th) / 2.0 - if toolbar { 1.5 } else { 0.0 };
    let alone_title = |t: NSSize| match c.look {
        Look::Shadowless => rect(3.0, title_y(t.height), bw - 6.0, t.height),
        Look::SmallSquare => rect(1.0, title_y(t.height), bw - 2.0, t.height),
        Look::Textured => rect(TEXTURED_PAD[i] / 2.0, title_y(t.height), bw - TEXTURED_PAD[i], t.height),
        Look::Toolbar => rect(TOOLBAR_PAD[i] / 2.0 - 1.0, title_y(t.height), bw - TOOLBAR_PAD[i] + 2.0, t.height),
        _ => rect(0.0, title_y(t.height), bw, t.height),
    };
    // Images not scaled are clipped by the bounds.
    let clipped = |r: NSRect| {
        if sc != NSImageScaling::ScaleNone {
            return r;
        }
        let (x0, y0) = (r.origin.x.max(0.0), r.origin.y.max(0.0));
        let (x1, y1) = ((r.origin.x + r.size.width).min(bw), (r.origin.y + r.size.height).min(bh));
        rect(x0, y0, x1 - x0, y1 - y0)
    };
    match (which, c.title) {
        (1, Some(t)) => {
            let s = fit(c.image, size(cw - GAP - t.width, ch), sc);
            let iy = cy + (ch - s.height) / 2.0;
            let left = c.place == Place::Left;
            if c.hugs {
                let gx = cx + (cw - (s.width + GAP + t.width)) / 2.0;
                let (ix, tx) = if left { (gx, gx + s.width + GAP) } else { (gx + t.width + GAP, gx) };
                (clipped(rect(ix, iy, s.width, s.height)), rect(tx, title_y(t.height), t.width, t.height), drawing)
            } else {
                let (ix, ax) = if left { (cx, cx + s.width + GAP) } else { (cx + cw - s.width, cx) };
                let band = rect(ax, title_y(t.height), cw - s.width - GAP, t.height);
                (clipped(rect(ix, iy, s.width, s.height)), band, drawing)
            }
        }
        (2, Some(t)) => {
            let s = fit(c.image, size(cw, ch - GAP - t.height), sc);
            if s.width <= 0.0 || s.height <= 0.0 {
                // No room for the image: the title alone, and, hugging, an
                // empty image rect at its edge.
                let title = alone_title(t);
                let edge = title.origin.y + if c.place == Place::Below { t.height } else { 0.0 };
                let image = if c.hugs { rect(0.0, edge, 0.0, 0.0) } else { none };
                return (image, title, drawing);
            }
            let above = c.place == Place::Above;
            let (iy, ty) = if c.hugs {
                let gy = cy + (ch - (s.height + t.height)) / 2.0;
                if above { (gy, gy + s.height) } else { (gy + t.height, gy) }
            } else {
                let (iy, ay) = if above { (cy, cy + s.height + GAP) } else { (cy + ch - s.height, cy) };
                (iy, ay + (ch - s.height - GAP - t.height) / 2.0)
            };
            (clipped(rect(cx + (cw - s.width) / 2.0, iy, s.width, s.height)), rect(cx, ty, cw, t.height), drawing)
        }
        (_, t) => {
            let (mut image, quirk) = image_alone(c, cx, cy, cw, ch);
            if toolbar && matches!(c.place, Place::Left | Place::Right) {
                let w = image.size.width;
                let e = (TOOLBAR_PAD[i] / 2.0 - 1.0).min((bw - w) / 2.0);
                image.origin.x = if c.place == Place::Left { e } else { bw - e - w };
            }
            let title = match (c.place, t) {
                (Place::Overlaps, Some(t)) => alone_title(t),
                _ => quirk,
            };
            (clipped(image), title, drawing)
        }
    }
}

fn near(a: NSRect, b: NSRect) -> bool {
    [a.origin.x - b.origin.x, a.origin.y - b.origin.y, a.size.width - b.size.width, a.size.height - b.size.height]
        .iter()
        .all(|d| d.abs() < 0.01)
}

fn near_size(a: NSSize, b: NSSize) -> bool {
    (a.width - b.width).abs() < 0.01 && (a.height - b.height).abs() < 0.01
}

/// A button showing `c`.
fn button(mtm: MainThreadMarker, c: &Case, title: &str) -> Retained<NSButton> {
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm) };
    b.setBezelStyle(c.look.bezel());
    b.setControlSize(SIZES[c.i]);
    if c.look == Look::Borderless {
        b.setBordered(false);
    }
    b.setImage(Some(&image(c.image.width, c.image.height)));
    let position = match c.place {
        Place::Only => NSCellImagePosition::ImageOnly,
        Place::Left => NSCellImagePosition::ImageLeft,
        Place::Right => NSCellImagePosition::ImageRight,
        Place::Above => NSCellImagePosition::ImageAbove,
        Place::Below => NSCellImagePosition::ImageBelow,
        Place::Overlaps => NSCellImagePosition::ImageOverlaps,
    };
    b.setImagePosition(position);
    b.setImageScaling(c.scaling);
    b.setImageHugsTitle(c.hugs);
    b
}

/// Check every case of `looks` with `scalings`, positions and titles: the
/// sizes, and the rects at the button's own size and in 160 × 60.
fn check_grid(mtm: MainThreadMarker, looks: &[Look], scalings: &[NSImageScaling], places: &[Place], titles: &[&str]) {
    let images = [size(16.0, 16.0), size(10.0, 30.0), size(40.0, 12.0), size(60.0, 60.0), size(7.0, 5.0)];
    let mut failures = Vec::new();
    let mut checked = 0;
    for &look in looks {
        for (i, font) in FONT.into_iter().enumerate() {
            for &title in titles {
                let t = (!title.is_empty()).then(|| {
                    let s = text_size(title, font);
                    size(s.width.ceil(), s.height)
                });
                for image in images {
                    for &place in places {
                        for &scaling in scalings {
                            for hugs in [false, true] {
                                if skipped(look, place, t.is_some(), hugs) {
                                    continue;
                                }
                                // Leading and trailing are left and right.
                                let c = Case { look, i, title: t, image, place, hugs, scaling };
                                let b = button(mtm, &c, title);
                                let cell = b.cell().expect("a cell");
                                let what = format!(
                                    "{look:?} size {i} {title:?} image {image:?} {place:?} {scaling:?} hugs {hugs}"
                                );
                                let want = sizes(&c);
                                checked += 1;
                                let intrinsic = b.intrinsicContentSize();
                                if !near_size(intrinsic, want.intrinsic) {
                                    failures.push(format!("{what}: intrinsic {intrinsic:?}, not {:?}", want.intrinsic));
                                    continue;
                                }
                                if !near_size(cell.cellSize(), want.cell) {
                                    failures.push(format!(
                                        "{what}: cellSize {:?}, not {:?}",
                                        cell.cellSize(),
                                        want.cell
                                    ));
                                }
                                for (bw, bh) in [(intrinsic.width, intrinsic.height), (160.0, 60.0)] {
                                    if bw < intrinsic.width || bh < intrinsic.height {
                                        continue;
                                    }
                                    let bounds = rect(0.0, 0.0, bw, bh);
                                    let got = (
                                        cell.imageRectForBounds(bounds),
                                        cell.titleRectForBounds(bounds),
                                        cell.drawingRectForBounds(bounds),
                                    );
                                    let want = rects(&c, bw, bh);
                                    for (name, g, w) in
                                        [("image", got.0, want.0), ("title", got.1, want.1), ("drawing", got.2, want.2)]
                                    {
                                        if !near(g, w) {
                                            failures.push(format!("{what} in {bw}x{bh}: {name} {g:?}, not {w:?}"));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for f in failures.iter().take(20) {
        println!("    {f}");
    }
    assert!(failures.is_empty(), "{} of {checked} buttons differ", failures.len());
}

/// The cases AppKit lays out in ways not pinned here: a push button with
/// an image above or below its title (which it squeezes into its fixed
/// height oddly), a toolbar button's image above or below (its title's
/// room) or over the title, and untitled toolbar buttons that hug.
fn skipped(look: Look, place: Place, titled: bool, hugs: bool) -> bool {
    match look {
        Look::Push => titled && matches!(place, Place::Above | Place::Below),
        Look::Toolbar => {
            matches!(place, Place::Above | Place::Below)
                || (titled && place == Place::Overlaps)
                || (hugs && !titled && matches!(place, Place::Left | Place::Right))
        }
        _ => false,
    }
}

const ALL_PLACES: [Place; 6] = [Place::Only, Place::Left, Place::Right, Place::Above, Place::Below, Place::Overlaps];

fn rounded_bezels(mtm: MainThreadMarker) {
    let looks = [Look::Push, Look::Flexible, Look::Circular];
    check_grid(mtm, &looks, &[NSImageScaling::ScaleProportionallyDown], &ALL_PLACES, &["", "Cancel", "A"]);
}

fn square_bezels(mtm: MainThreadMarker) {
    let looks = [Look::SmallSquare, Look::Shadowless, Look::Textured, Look::Toolbar];
    check_grid(mtm, &looks, &[NSImageScaling::ScaleProportionallyDown], &ALL_PLACES, &["", "Cancel", "A"]);
}

fn disclosure_buttons(mtm: MainThreadMarker) {
    check_grid(mtm, &[Look::Disclosure], &[NSImageScaling::ScaleProportionallyDown], &ALL_PLACES, &["", "Cancel", "A"]);
}

fn borderless_buttons(mtm: MainThreadMarker) {
    check_grid(mtm, &[Look::Borderless], &[NSImageScaling::ScaleProportionallyDown], &ALL_PLACES, &["", "Cancel", "A"]);
}

fn image_scalings(mtm: MainThreadMarker) {
    use NSImageScaling as S;
    let all = [S::ScaleAxesIndependently, S::ScaleNone, S::ScaleProportionallyUpOrDown];
    let looks = [Look::Push, Look::Circular, Look::Shadowless, Look::SmallSquare];
    check_grid(mtm, &looks, &all, &[Place::Only], &[""]);
    check_grid(mtm, &[Look::Flexible], &[S::ScaleAxesIndependently, S::ScaleNone], &[Place::Only], &[""]);
    check_grid(mtm, &[Look::Textured, Look::Toolbar], &[S::ScaleNone], &[Place::Only], &[""]);
}

fn leading_and_trailing(mtm: MainThreadMarker) {
    // Left to right, leading is left and trailing right.
    for (look, i) in [(Look::Push, 0), (Look::Flexible, 1), (Look::Shadowless, 2), (Look::Borderless, 3)] {
        let t = text_size("Cancel", FONT[i]);
        let title = Some(size(t.width.ceil(), t.height));
        for (position, place) in
            [(NSCellImagePosition::ImageLeading, Place::Left), (NSCellImagePosition::ImageTrailing, Place::Right)]
        {
            let c = Case {
                look,
                i,
                title,
                image: size(16.0, 16.0),
                place,
                hugs: false,
                scaling: NSImageScaling::ScaleProportionallyDown,
            };
            let b = button(mtm, &c, "Cancel");
            b.setImagePosition(position);
            let cell = b.cell().expect("a cell");
            let s = b.intrinsicContentSize();
            assert!(near_size(s, sizes(&c).intrinsic), "{look:?} {position:?}");
            let (image, title, _) = rects(&c, 160.0, 60.0);
            let bounds = rect(0.0, 0.0, 160.0, 60.0);
            assert!(near(cell.imageRectForBounds(bounds), image), "{look:?} {position:?}");
            assert!(near(cell.titleRectForBounds(bounds), title), "{look:?} {position:?}");
        }
    }
}

fn images_that_show_nowhere(mtm: MainThreadMarker) {
    // Help and push-disclosure buttons don't show images; nor does a
    // button told to show none.
    for bezel in [NSBezelStyle::HelpButton, NSBezelStyle::PushDisclosure] {
        // SAFETY: no target or action.
        let b = unsafe { NSButton::buttonWithImage_target_action(&image(16.0, 16.0), None, None, mtm) };
        b.setBezelStyle(bezel);
        let c = b.cell().expect("a cell");
        assert_eq!(b.intrinsicContentSize(), size(24.0, 24.0), "{bezel:?}");
        assert_eq!(c.imageRectForBounds(rect(0.0, 0.0, 60.0, 40.0)), NSRect::ZERO, "{bezel:?}");
    }
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("Cancel"), None, None, mtm) };
    let before = b.intrinsicContentSize();
    b.setImage(Some(&image(40.0, 40.0)));
    b.setImagePosition(NSCellImagePosition::NoImage);
    assert_eq!(b.intrinsicContentSize(), before);
    assert_eq!(b.cell().expect("a cell").imageRectForBounds(rect(0.0, 0.0, 100.0, 40.0)), NSRect::ZERO);
}

// Factories and defaults

fn factories_and_defaults(mtm: MainThreadMarker) {
    let img = image(10.0, 10.0);
    // An image button shows its image alone, sized to fit; its title is
    // the default one, not shown.
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithImage_target_action(&img, None, None, mtm) };
    assert_eq!(b.imagePosition(), NSCellImagePosition::ImageOnly);
    assert_eq!(b.title().to_string(), "Button");
    assert_eq!(b.bezelStyle(), NSBezelStyle::Automatic);
    assert_eq!(b.frame(), rect(0.0, 0.0, 34.0, 24.0));
    assert_eq!(b.imageScaling(), NSImageScaling::ScaleProportionallyDown);
    assert!(b.image().is_some_and(|i| std::ptr::eq(&*i, &*img)));
    // Titled, the image leads.
    // SAFETY: as above.
    let b =
        unsafe { NSButton::buttonWithTitle_image_target_action(&NSString::from_str("Cancel"), &img, None, None, mtm) };
    assert_eq!(b.imagePosition(), NSCellImagePosition::ImageLeading);
    let t = text_size("Cancel", 13.0);
    assert_eq!(b.frame(), rect(0.0, 0.0, 24.0 + 10.0 + GAP + t.width.ceil(), 24.0));
    assert!(!b.imageHugsTitle());
    // A button given an image with nowhere to show it shows it alone.
    let plain = NSButton::initWithFrame(mtm.alloc::<NSButton>(), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(plain.imagePosition(), NSCellImagePosition::NoImage);
    plain.setImage(Some(&img));
    assert_eq!(plain.imagePosition(), NSCellImagePosition::ImageOnly);
    assert_eq!(plain.cell().expect("a cell").r#type(), NSCellType::TextCellType);
    plain.setImage(None);
    assert_eq!(plain.imagePosition(), NSCellImagePosition::ImageOnly);
    // Buttons the factories make scale images down to fit; others don't
    // scale them. A button cell made alone has no image position, and
    // dims images when disabled.
    assert_eq!(plain.imageScaling(), NSImageScaling::ScaleNone);
    // SAFETY: no target or action.
    let check = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str("A"), None, None, mtm) };
    assert_eq!(check.imageScaling(), NSImageScaling::ScaleProportionallyDown);
    let cell = NSButtonCell::new(mtm);
    assert_eq!(cell.imageScaling(), NSImageScaling::ScaleNone);
    assert_eq!(cell.imagePosition(), NSCellImagePosition::NoImage);
    assert!(cell.imageDimsWhenDisabled());
    assert!(cell.alternateImage().is_none());
    // One made with an image shows it alone, untitled.
    let cell = NSButtonCell::initImageCell(mtm.alloc::<NSButtonCell>(), Some(&img));
    assert_eq!(cell.imagePosition(), NSCellImagePosition::ImageOnly);
    assert_eq!(cell.r#type(), NSCellType::NullCellType);
    assert_eq!(cell.title().to_string(), "");
    assert!(cell.isBordered());
    // Symbol configurations are the button's.
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(26.0, 0.0);
    b.setSymbolConfiguration(Some(&config));
    assert!(b.symbolConfiguration().is_some());
    // Sent to a subclass, each factory makes one of that subclass.
    let t = NSString::from_str("T");
    let (none, no_action): (Option<&AnyObject>, Option<Sel>) = (None, None);
    let class = MyButton::class();
    // SAFETY: the factories take these arguments and return buttons.
    let made: [Retained<NSButton>; 5] = unsafe {
        [
            msg_send![class, buttonWithTitle: &*t, target: none, action: no_action],
            msg_send![class, buttonWithImage: &*img, target: none, action: no_action],
            msg_send![class, buttonWithTitle: &*t, image: &*img, target: none, action: no_action],
            msg_send![class, checkboxWithTitle: &*t, target: none, action: no_action],
            msg_send![class, radioButtonWithTitle: &*t, target: none, action: no_action],
        ]
    };
    for b in made {
        assert!(std::ptr::eq(b.class(), class), "{:?}", b.class().name());
    }
}

define_class!(
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMyButton"]
    struct MyButton;

    unsafe impl NSObjectProtocol for MyButton {}
);

// Drawing

fn aqua() -> Retained<NSAppearance> {
    // SAFETY: the name is AppKit's constant.
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).expect("aqua")
}

fn dark_aqua() -> Retained<NSAppearance> {
    // SAFETY: the name is AppKit's constant.
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).expect("dark aqua")
}

/// The pixel at the middle of `view` drawn at 1x in `appearance`.
fn middle_in(view: &NSView, appearance: &NSAppearance) -> [u8; 4] {
    view.setAppearance(Some(appearance));
    let f = view.frame();
    pixel(&snapshot(view, 1.0), (f.size.width / 2.0) as isize, (f.size.height / 2.0) as isize)
}

/// The pixel at the middle of `button` drawn at 1x, in the light
/// appearance.
fn middle_pixel(button: &NSButton) -> [u8; 4] {
    middle_in(button, &aqua())
}

fn image_button(mtm: MainThreadMarker, image: &NSImage, kind: NSButtonType, bordered: bool) -> Retained<NSButton> {
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithImage_target_action(image, None, None, mtm) };
    b.setFrame(rect(0.0, 0.0, 40.0, 24.0));
    b.setButtonType(kind);
    b.setBordered(bordered);
    b
}

/// The pixel `color` fills with in `appearance`.
fn reference_in(mtm: MainThreadMarker, appearance: &NSAppearance, color: Retained<NSColor>) -> [u8; 4] {
    let view = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, move |_, _| {
        color.set();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    view.setAppearance(Some(appearance));
    pixel(&snapshot(&view, 1.0), 2, 2)
}

/// Premultiplied `top` over premultiplied `under`.
fn over(top: [u8; 4], under: [u8; 4]) -> [u8; 4] {
    let a = f64::from(top[3]) / 255.0;
    [0, 1, 2, 3].map(|i| (f64::from(top[i]) + f64::from(under[i]) * (1.0 - a)).round() as u8)
}

/// An opaque pixel with `step` 255ths added to each color channel.
fn shifted(p: [u8; 4], step: i32) -> [u8; 4] {
    let s = |v: u8| (i32::from(v) + step).clamp(0, 255) as u8;
    [s(p[0]), s(p[1]), s(p[2]), p[3]]
}

/// `rgb` (0 to 255, straight) at `alpha`, premultiplied.
fn premultiplied(rgb: [u8; 3], alpha: f64) -> [u8; 4] {
    let c = |v: u8| (f64::from(v) * alpha).round() as u8;
    [c(rgb[0]), c(rgb[1]), c(rgb[2]), (alpha * 255.0).round() as u8]
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 3)
}

/// A state to draw a button in: its name, the button's type, what puts
/// it in the state, and the template's color there.
type State<'a> = (&'a str, NSButtonType, &'a dyn Fn(&NSButton), [u8; 4]);

fn template_images_by_state(mtm: MainThreadMarker) {
    let template = filled(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]);
    template.setTemplate(true);
    let plain = filled(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]);
    let clear = filled(10.0, 10.0, [0.0, 0.0, 0.0, 0.0]);
    let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
    let orange = NSColor::colorWithSRGBRed_green_blue_alpha(0.6, 0.4, 0.2, 1.0);
    let push = NSButtonType::MomentaryPushIn;
    for appearance in [aqua(), dark_aqua()] {
        let dark = appearance.name().to_string().contains("Dark");
        let reference = |c: Retained<NSColor>| reference_in(mtm, &appearance, c);
        // Borderless, a template draws in the secondary label color, the
        // label color while pressed, the disabled text color when disabled,
        // and the content tint in any state (with the pressed system effect
        // while pressed).
        let shown = |image: &NSImage, setup: &dyn Fn(&NSButton)| {
            let b = image_button(mtm, image, push, false);
            setup(&b);
            middle_in(&b, &appearance)
        };
        let mut cases = vec![
            ("normal", shown(&template, &|_| {}), reference(NSColor::secondaryLabelColor())),
            ("pressed", shown(&template, &|b| b.highlight(true)), reference(NSColor::labelColor())),
            ("disabled", shown(&template, &|b| b.setEnabled(false)), reference(NSColor::disabledControlTextColor())),
            // Other images draw as they are, faded when disabled unless told
            // not.
            ("plain, tinted", shown(&plain, &|b| b.setContentTintColor(Some(&blue))), [0, 0, 0, 255]),
            ("plain, disabled", shown(&plain, &|b| b.setEnabled(false)), [0, 0, 0, 102]),
            (
                "plain, disabled, not dimmed",
                shown(&plain, &|b| {
                    b.setEnabled(false);
                    b.cell()
                        .expect("a cell")
                        .downcast_ref::<NSButtonCell>()
                        .expect("a button cell")
                        .setImageDimsWhenDisabled(false);
                }),
                [0, 0, 0, 255],
            ),
        ];
        for tint in [&blue, &orange] {
            let with = |b: &NSButton| b.setContentTintColor(Some(tint));
            cases.push(("tinted", shown(&template, &with), reference((*tint).clone())));
            let pressed = shown(&template, &|b| {
                with(b);
                b.highlight(true);
            });
            cases.push((
                "tinted, pressed",
                pressed,
                reference(tint.colorWithSystemEffect(NSColorSystemEffect::Pressed)),
            ));
            let disabled = shown(&template, &|b| {
                with(b);
                b.setEnabled(false);
            });
            cases.push(("tinted, disabled", disabled, reference(tint.colorWithAlphaComponent(0.5))));
        }
        for (what, got, want) in cases {
            assert!(close(got, want), "borderless {what}, dark {dark}: {got:?}, not {want:?}");
        }
        // Bordered, a template takes the title's color: never the content
        // tint, and white on the default button's accent.
        let bordered = |image: &NSImage, setup: &dyn Fn(&NSButton)| {
            let b = image_button(mtm, image, push, true);
            setup(&b);
            middle_in(&b, &appearance)
        };
        let normal = bordered(&template, &|_| {});
        assert_eq!(bordered(&template, &|b| b.setContentTintColor(Some(&blue))), normal);
        let default = bordered(&template, &|b| b.setKeyEquivalent(&NSString::from_str("\r")));
        assert!(close(default, [255, 255, 255, 255]), "the default button's template, dark {dark}: {default:?}");
        assert!(bordered(&template, &|b| b.setEnabled(false))[3] < normal[3], "a disabled template fades");
        assert!(close(bordered(&plain, &|_| {}), [0, 0, 0, 255]));
        // Textured and toolbar buttons tint templates with the accent, a
        // shade darker in the light appearance and lighter in the dark one,
        // more so while pressed, and faded as the disabled system effect
        // fades it when disabled; the content tint doesn't count. Shown on
        // by their bezel, they use the label's color at 70% instead. Badge
        // buttons use the secondary label color in every state. Each is
        // drawn over the button's own bezel (what a clear image shows).
        let accent = reference(NSColor::controlAccentColor());
        let (step, pressed_step) = if dark { (13, 61) } else { (-10, -46) };
        let label_rgb = if dark { [255, 255, 255] } else { [0, 0, 0] };
        for bezel in [NSBezelStyle::TexturedSquare, NSBezelStyle::Toolbar, NSBezelStyle::Badge] {
            let badge = bezel == NSBezelStyle::Badge;
            let drawn = |image: &NSImage, kind: NSButtonType, setup: &dyn Fn(&NSButton)| {
                let b = image_button(mtm, image, kind, true);
                b.setBezelStyle(bezel);
                b.setContentTintColor(Some(&orange));
                setup(&b);
                middle_in(&b, &appearance)
            };
            let secondary = reference(NSColor::secondaryLabelColor());
            let disabled_accent =
                reference(NSColor::controlAccentColor().colorWithSystemEffect(NSColorSystemEffect::Disabled));
            let on = |b: &NSButton| b.setState(1);
            let press = |b: &NSButton| b.highlight(true);
            let off = |b: &NSButton| b.setEnabled(false);
            let toggle = NSButtonType::PushOnPushOff;
            let (on_pressed, on_disabled) = if dark { (0.90, 0.25) } else { (1.0, 0.28) };
            let states: [State; 7] = [
                ("normal", push, &|_| {}, if badge { secondary } else { shifted(accent, step) }),
                ("pressed", push, &press, if badge { secondary } else { shifted(accent, pressed_step) }),
                ("disabled", push, &off, if badge { secondary } else { disabled_accent }),
                // A toggle that isn't on looks as a push button does.
                ("toggle, off", toggle, &|_| {}, if badge { secondary } else { shifted(accent, step) }),
                ("toggle, on", toggle, &on, if badge { secondary } else { premultiplied(label_rgb, 0.70) }),
                (
                    "toggle, on, pressed",
                    toggle,
                    &|b| {
                        on(b);
                        press(b);
                    },
                    if badge { secondary } else { premultiplied(label_rgb, on_pressed) },
                ),
                (
                    "toggle, on, disabled",
                    toggle,
                    &|b| {
                        on(b);
                        off(b);
                    },
                    if badge { secondary } else { premultiplied(label_rgb, on_disabled) },
                ),
            ];
            for (what, kind, setup, want) in states {
                let bezel_only = drawn(&clear, kind, setup);
                let got = drawn(&template, kind, setup);
                let want = over(want, bezel_only);
                assert!(close(got, want), "{bezel:?} {what}, dark {dark}: {got:?}, not {want:?}");
            }
        }
    }
}

fn alternate_images(mtm: MainThreadMarker) {
    let black = filled(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]);
    let red = filled(10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    // A toggle shows its alternate image when on or pressed, but not both;
    // a push button never does.
    for bordered in [true, false] {
        for (kind, shows) in
            [(NSButtonType::Toggle, [false, true, true, false]), (NSButtonType::MomentaryPushIn, [false; 4])]
        {
            for (n, (state, pressed)) in [(0, false), (1, false), (0, true), (1, true)].into_iter().enumerate() {
                let b = image_button(mtm, &black, kind, bordered);
                b.setAlternateImage(Some(&red));
                b.setState(state);
                if pressed {
                    b.highlight(true);
                }
                let got = middle_pixel(&b);
                let want = if shows[n] { [255, 0, 0, 255] } else { [0, 0, 0, 255] };
                assert!(close(got, want), "{kind:?} bordered {bordered} state {state} pressed {pressed}: {got:?}");
            }
        }
    }
}

// Accessibility

fn image_buttons_are_labelled_by_their_images(mtm: MainThreadMarker) {
    let img = image(10.0, 10.0);
    img.setAccessibilityDescription(Some(&NSString::from_str("A picture")));
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithImage_target_action(&img, None, None, mtm) };
    let cell = b.cell().expect("a cell");
    let label = |o: &AnyObject| -> Option<String> {
        // SAFETY: accessibilityLabel takes nothing and returns a string or nil.
        let l: Option<Retained<NSString>> = unsafe { msg_send![o, accessibilityLabel] };
        l.map(|l| l.to_string())
    };
    assert_eq!(label(&cell).as_deref(), Some("A picture"));
    assert_eq!(label(&b).as_deref(), Some("A picture"));
    // SAFETY: accessibilityTitle takes nothing and returns a string or nil.
    let title: Option<Retained<NSString>> = unsafe { msg_send![&*cell, accessibilityTitle] };
    assert_eq!(title.map(|t| t.to_string()).as_deref(), Some(""));
    // An image view's cell is the image element, with the image's label.
    let v = NSImageView::new(mtm);
    v.setImage(Some(&img));
    let cell = v.cell().expect("a cell");
    // SAFETY: accessibilityRole takes nothing and returns a string or nil.
    let role: Option<Retained<NSString>> = unsafe { msg_send![&*cell, accessibilityRole] };
    assert_eq!(role.map(|r| r.to_string()).as_deref(), Some("AXImage"));
    // SAFETY: isAccessibilityElement takes nothing and returns BOOL.
    let (cell_is, view_is): (bool, bool) =
        unsafe { (msg_send![&*cell, isAccessibilityElement], msg_send![&*v, isAccessibilityElement]) };
    assert!(cell_is && !view_is);
    assert_eq!(label(&cell).as_deref(), Some("A picture"));
    assert_eq!(label(&v).as_deref(), Some("A picture"));
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let tests: &[Test] = &[
        ("factories_and_defaults", factories_and_defaults),
        ("rounded_bezels", rounded_bezels),
        ("square_bezels", square_bezels),
        ("disclosure_buttons", disclosure_buttons),
        ("borderless_buttons", borderless_buttons),
        ("image_scalings", image_scalings),
        ("leading_and_trailing", leading_and_trailing),
        ("images_that_show_nowhere", images_that_show_nowhere),
        ("template_images_by_state", template_images_by_state),
        ("alternate_images", alternate_images),
        ("image_buttons_are_labelled_by_their_images", image_buttons_are_labelled_by_their_images),
    ];
    // The geometry was measured on a 2x screen; on a 1x one (CI's macOS
    // runner) AppKit rounds some of it differently, so there a difference
    // is printed, not failed (as in `controls.rs`).
    #[cfg(target_vendor = "apple")]
    let scale = NSScreen::mainScreen(mtm).map_or(1.0, |s| s.backingScaleFactor());
    #[cfg(not(target_vendor = "apple"))]
    let scale = 2.0;
    const GEOMETRY: &[&str] = &[
        "rounded_bezels",
        "square_bezels",
        "disclosure_buttons",
        "borderless_buttons",
        "image_scalings",
        "leading_and_trailing",
        "images_that_show_nowhere",
    ];
    for (name, test) in tests {
        if scale != 2.0 && GEOMETRY.contains(name) {
            let run = std::panic::AssertUnwindSafe(|| objc2::rc::autoreleasepool(|_| test(mtm)));
            match std::panic::catch_unwind(run) {
                Ok(()) => println!("test {name} ... ok"),
                Err(_) => println!("test {name} ... differs on a {scale}x screen (see above), not failed"),
            }
            continue;
        }
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
