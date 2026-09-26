//! String drawing: the methods AppKit adds to `NSString` for drawing and
//! measuring with an attribute dictionary.
//!
//! The attributes become a [`layout::Attrs`] and the text is laid out by
//! [`layout::lay_out`], which takes a list of attribute runs, so drawing an
//! attributed string is the same call with one run per attribute range
//! ([`draw`], [`measure`]). Drawing records a glyph run op per run of
//! glyphs, fills for backgrounds before them and fills for underlines and
//! strikethroughs after them.
//!
//! The methods are defined on a helper class for their encodings, then
//! copied onto `NSString`, as a category would add them. The helper is a
//! static shell (`_SidestepStringDrawing`) whose loader does the copying,
//! so installing them is a message to it: the work runs once, under the
//! runtime's class-loading lock like every class's loader, and never under
//! a lock of its own that a loader could wait on the other way round.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackgroundColorAttributeName, NSBaselineOffsetAttributeName, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSKernAttributeName, NSLigatureAttributeName, NSParagraphStyle,
    NSParagraphStyleAttributeName, NSStrikethroughColorAttributeName, NSStrikethroughStyleAttributeName,
    NSStringDrawingContext, NSStringDrawingOptions, NSUnderlineColorAttributeName, NSUnderlineStyleAttributeName,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::font::{number, text_font};
use crate::graphics::{self, Xf, color_of, with_recorder};
use crate::paragraph::paragraph_of;
use crate::protocol::{GlyphRun, Op, Rect};
use crate::text::fonts::{self, Design, FontSpec};
use crate::text::layout::{self, Attrs, FxBuild, Options, Run, TextFont, TextLayout};

type Attributes = NSDictionary<NSString, AnyObject>;

/// Where text goes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Place {
    /// `drawAtPoint:`: the corner of the text nearest the view's origin.
    Point(NSPoint),
    /// `drawInRect:`: wrapped to the rectangle's width and clipped to it.
    /// Lines below it are clipped away, not dropped, as on macOS.
    Rect(NSRect),
    /// `drawWithRect:options:`: with `usesLineFragmentOrigin`, as `Rect`,
    /// except that `truncatesLastVisibleLine` keeps to the height and
    /// truncates the last line in it; without, one line on a baseline at
    /// the rectangle's origin.
    WithRect(NSRect, NSStringDrawingOptions),
}

/// Draw text with attribute runs into the view being drawn. Text laid out
/// before is recorded at once; other text waits for the end of the pass,
/// when it is laid out together (see `text::pool`).
pub(crate) fn draw(text: &str, attrs: &[Attrs], runs: &[Run], place: Place) {
    let opts = match place {
        Place::Point(_) => Options::UNBOUNDED,
        // An empty rectangle shows nothing.
        Place::Rect(r) if r.size.width <= 0.0 || r.size.height <= 0.0 => return,
        Place::Rect(r) => Options { width: limit(r.size.width), ..Options::UNBOUNDED },
        Place::WithRect(r, o) if !o.contains(NSStringDrawingOptions::TruncatesLastVisibleLine) => {
            Options { height: f32::INFINITY, ..options(r.size, o) }
        }
        Place::WithRect(r, o) => options(r.size, o),
    };
    if !graphics::recording() {
        return;
    }
    match layout::cached(text, attrs, runs, &opts) {
        Ok(laid) => with_recorder(|rec| {
            let (left, top, clip) = placement(rec.xf, rec.clip, place, &laid);
            emit(&mut rec.ops, &laid, left, top, clip);
        }),
        Err(hash) => with_recorder(|rec| {
            let job = rec.pending.job_for(text, attrs, runs, opts, hash);
            rec.pending.places.push(Deferred { at: rec.ops.len(), job, xf: rec.xf, clip: rec.clip, place });
        }),
    }
}

/// Text a pass drew before it was laid out: the jobs to lay out, once for
/// each distinct text however often it was drawn, and where each drawing
/// goes.
#[derive(Default)]
pub(crate) struct Pending {
    jobs: Vec<layout::Job>,
    /// The first job with each key.
    by_key: HashMap<u64, usize, FxBuild>,
    places: Vec<Deferred>,
}

impl Pending {
    /// The job that lays `text` out as asked: one already pending if the
    /// pass drew the same text before (a label repeated down a table), or
    /// a new one.
    fn job_for(&mut self, text: &str, attrs: &[Attrs], runs: &[Run], opts: Options, key: u64) -> usize {
        let found = self.by_key.get(&key).copied();
        if let Some(at) = found
            && self.jobs[at].is(text, attrs, runs, &opts)
        {
            return at;
        }
        let at = self.jobs.len();
        self.jobs.push(layout::job(text.into(), attrs.into(), runs.into(), opts, key));
        if found.is_none() {
            self.by_key.insert(key, at);
        }
        at
    }
}

/// Text drawn before it was laid out: where its ops go, its job, and what
/// placing it needs.
struct Deferred {
    at: usize,
    job: usize,
    xf: Xf,
    clip: Rect,
    place: Place,
}

/// A pass's ops, with the text it deferred laid out and put in place.
pub(crate) fn finish(ops: Vec<Op>, pending: Pending) -> Vec<Op> {
    if pending.places.is_empty() {
        return ops;
    }
    let laid = layout::lay_out_all(pending.jobs);
    let extra: usize = pending.places.iter().map(|d| laid[d.job].runs.len() + laid[d.job].fills.len()).sum();
    let mut out = Vec::with_capacity(ops.len() + extra);
    let mut ops = ops.into_iter();
    let mut next = 0;
    for d in pending.places {
        out.extend(ops.by_ref().take(d.at - next));
        next = d.at;
        let laid = &laid[d.job];
        let (left, top, clip) = placement(d.xf, d.clip, d.place, laid);
        emit(&mut out, laid, left, top, clip);
    }
    out.extend(ops);
    out
}

/// Where laid-out text goes in the layer, and what it's clipped to, given
/// the view's transform and clip.
fn placement(xf: Xf, clip: Rect, place: Place, laid: &TextLayout) -> (f32, f32, Rect) {
    // The corner of the text's box nearest the view's origin: its top in a
    // flipped view, its bottom otherwise.
    let at_point = |p: NSPoint| {
        let (x, ya) = xf.point(p.x, p.y);
        let (_, yb) = xf.point(p.x, p.y + f64::from(laid.height));
        (x as f32, ya.min(yb) as f32, clip)
    };
    let lines = NSStringDrawingOptions::UsesLineFragmentOrigin;
    match place {
        Place::Point(p) => at_point(p),
        // Without a height, the rectangle's origin is a point to draw at,
        // as for drawAtPoint:.
        Place::WithRect(r, o) if o.contains(lines) && r.size.height <= 0.0 => at_point(r.origin),
        Place::Rect(r) | Place::WithRect(r, _) if matches!(place, Place::Rect(_)) || laid_in_lines(place) => {
            let area = xf.rect(r);
            let clip = if r.size.width > 0.0 {
                clip.intersect(&area)
            } else {
                clip.intersect(&Rect { x1: f32::INFINITY, ..area })
            };
            (area.x0, area.y0, clip)
        }
        // One line on a baseline at the origin.
        Place::Rect(r) | Place::WithRect(r, _) => {
            let (x, baseline) = xf.point(r.origin.x, r.origin.y);
            (x as f32, baseline as f32 - (laid.height - laid.first_descent), clip)
        }
    }
}

fn laid_in_lines(place: Place) -> bool {
    matches!(place, Place::WithRect(_, o) if o.contains(NSStringDrawingOptions::UsesLineFragmentOrigin))
}

/// The size of text with attribute runs, as `sizeWithAttributes:` or, with
/// options, `boundingRectWithSize:options:` measures it.
pub(crate) fn measure(
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    bounds: Option<(NSSize, NSStringDrawingOptions)>,
) -> NSRect {
    let opts = bounds.map_or(Options::UNBOUNDED, |(size, o)| options(size, o));
    let laid = layout::lay_out(text, attrs, runs, &opts);
    let y = if opts.all_lines { 0.0 } else { -laid.first_descent };
    NSRect::new(NSPoint::new(0.0, f64::from(y)), NSSize::new(f64::from(laid.width), f64::from(laid.height)))
}

fn limit(extent: f64) -> f32 {
    if extent > 0.0 { extent as f32 } else { f32::INFINITY }
}

fn options(size: NSSize, o: NSStringDrawingOptions) -> Options {
    let all_lines = o.contains(NSStringDrawingOptions::UsesLineFragmentOrigin);
    Options {
        width: limit(size.width),
        // One line on a baseline takes no notice of a height, as in AppKit.
        height: if all_lines { limit(size.height) } else { f32::INFINITY },
        all_lines,
        font_leading: o.contains(NSStringDrawingOptions::UsesFontLeading),
        truncate_last: o.contains(NSStringDrawingOptions::TruncatesLastVisibleLine),
    }
}

/// Record `laid` with its top left corner at (`left`, `top`) in the layer.
fn emit(ops: &mut Vec<Op>, laid: &TextLayout, left: f32, top: f32, clip: Rect) {
    if clip.is_empty() {
        return;
    }
    let fill = |ops: &mut Vec<Op>, rect: Rect, color| {
        let rect = rect.intersect(&clip);
        if !rect.is_empty() {
            ops.push(Op::Fill { rect, color });
        }
    };
    for f in laid.fills.iter().filter(|f| f.background) {
        let [x0, y0, x1, y1] = f.rect;
        fill(ops, Rect::new(left + x0, top + y0, left + x1, top + y1), f.color);
    }
    for run in &laid.runs {
        let (x, y) = (left + run.x, top + run.y);
        // Glyphs reach at most about twice their size from the baseline.
        let reach = run.size * 2.0;
        if y - reach > clip.y1 || y + reach < clip.y0 {
            continue;
        }
        ops.push(Op::Glyphs(GlyphRun {
            font: run.font,
            size: run.size,
            x,
            y,
            glyphs: run.glyphs.clone(),
            color: run.color,
            clip,
        }));
    }
    for f in laid.fills.iter().filter(|f| !f.background) {
        // Lines are drawn crisp: whole points, at least one thick.
        let [x0, y0, x1, y1] = f.rect;
        let y = (top + y0).round();
        let thickness = (y1 - y0).round().max(1.0);
        fill(ops, Rect::new(left + x0, y, left + x1, y + thickness), f.color);
    }
}

/// The attributes string drawing understands, read from a dictionary.
/// Numbers are read with `doubleValue`, so anything that answers it will
/// do.
pub(crate) fn attrs_of(dict: Option<&Attributes>) -> Attrs {
    let mut attrs = Attrs::new(default_font());
    let Some(dict) = dict else { return attrs };
    let mut left = dict.count();
    let mut get = |key: &NSString| {
        if left == 0 {
            return None;
        }
        let value = dict.objectForKey(key);
        left -= usize::from(value.is_some());
        value
    };
    let color =
        |value: Option<Retained<AnyObject>>| value.and_then(|v| v.downcast::<NSColor>().ok()).map(|c| color_of(&c));
    let int = |value: Option<Retained<AnyObject>>| value.and_then(|v| number(&v)).map(|n| n as i64);
    // SAFETY: the keys are constants this crate exports.
    unsafe {
        if let Some(font) = get(NSFontAttributeName).and_then(|v| v.downcast::<NSFont>().ok()) {
            attrs.font = text_font(&font);
        }
        if let Some(c) = color(get(NSForegroundColorAttributeName)) {
            attrs.color = c;
        }
        if let Some(style) = get(NSParagraphStyleAttributeName).and_then(|v| v.downcast::<NSParagraphStyle>().ok()) {
            attrs.paragraph = paragraph_of(&style);
        }
        attrs.background = color(get(NSBackgroundColorAttributeName));
        attrs.underline.style = int(get(NSUnderlineStyleAttributeName)).unwrap_or(0);
        attrs.strikethrough.style = int(get(NSStrikethroughStyleAttributeName)).unwrap_or(0);
        attrs.kern = get(NSKernAttributeName).and_then(|v| number(&v)).map(|k| k as f32);
        attrs.baseline_offset = get(NSBaselineOffsetAttributeName).and_then(|v| number(&v)).unwrap_or(0.0) as f32;
        attrs.ligatures = int(get(NSLigatureAttributeName)).unwrap_or(1);
        attrs.underline.color = color(get(NSUnderlineColorAttributeName));
        attrs.strikethrough.color = color(get(NSStrikethroughColorAttributeName));
    }
    attrs
}

/// Text without a font attribute is drawn in the 12-point interface font.
fn default_font() -> TextFont {
    thread_local!(static DEFAULT: RefCell<Option<TextFont>> = const { RefCell::new(None) });
    DEFAULT.with(|d| {
        d.borrow_mut()
            .get_or_insert_with(|| TextFont {
                face: fonts::resolve(&FontSpec::system(Design::Default, 12.0)),
                size: 12.0,
                tabular_digits: false,
                features: None,
            })
            .clone()
    })
}

fn this_string<T>(this: &T) -> String {
    // SAFETY: these methods are installed on NSString and only ever run with
    // a string as the receiver.
    unsafe { &*(this as *const T).cast::<NSString>() }.to_string()
}

/// One run of attributes over all of `text`.
fn whole(text: &str) -> [Run; 1] {
    [Run { start: 0, end: text.len(), attrs: 0 }]
}

fn draw_string<T>(this: &T, attrs: Option<&Attributes>, place: Place) {
    let text = this_string(this);
    draw(&text, &[attrs_of(attrs)], &whole(&text), place);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringDrawing"]
    struct StringDrawing;

    impl StringDrawing {
        #[unsafe(method(drawAtPoint:withAttributes:))]
        fn draw_at_point(&self, point: NSPoint, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::Point(point));
        }

        #[unsafe(method(drawInRect:withAttributes:))]
        fn draw_in_rect(&self, rect: NSRect, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::Rect(rect));
        }

        #[unsafe(method(drawWithRect:options:attributes:context:))]
        fn draw_with_rect(
            &self,
            rect: NSRect,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
            _context: Option<&NSStringDrawingContext>,
        ) {
            draw_string(self, attrs, Place::WithRect(rect, options));
        }

        #[unsafe(method(drawWithRect:options:attributes:))]
        fn draw_with_rect_no_context(&self, rect: NSRect, options: NSStringDrawingOptions, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::WithRect(rect, options));
        }

        #[unsafe(method(sizeWithAttributes:))]
        fn size_with_attributes(&self, attrs: Option<&Attributes>) -> NSSize {
            let text = this_string(self);
            measure(&text, &[attrs_of(attrs)], &whole(&text), None).size
        }

        #[unsafe(method(boundingRectWithSize:options:attributes:context:))]
        fn bounding_rect(
            &self,
            size: NSSize,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
            _context: Option<&NSStringDrawingContext>,
        ) -> NSRect {
            let text = this_string(self);
            measure(&text, &[attrs_of(attrs)], &whole(&text), Some((size, options)))
        }

        #[unsafe(method(boundingRectWithSize:options:attributes:))]
        fn bounding_rect_no_context(
            &self,
            size: NSSize,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
        ) -> NSRect {
            let text = this_string(self);
            measure(&text, &[attrs_of(attrs)], &whole(&text), Some((size, options)))
        }
    }
);

/// Give `NSString` its drawing methods, if it hasn't them yet, and start
/// opening the system's fonts in the background. Safe from anywhere,
/// class loaders included: it messages the helper's shell, whose loader
/// ([`load`]) does the work.
pub(crate) fn install_string_drawing() {
    // The shell, by its symbol (`crate::STRING_DRAWING`).
    let shell = objc2::class!(_SidestepStringDrawing);
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const AnyClass = unsafe { msg_send![shell, class] };
}

/// The helper's loader: define it, then copy its methods onto NSString.
/// The runtime runs it once, holding its class-loading lock.
pub(crate) fn load() {
    fonts::prewarm();
    let helper = StringDrawing::class();
    let target = <NSString as ClassType>::class();
    for sel in [
        sel!(drawAtPoint:withAttributes:),
        sel!(drawInRect:withAttributes:),
        sel!(drawWithRect:options:attributes:context:),
        sel!(drawWithRect:options:attributes:),
        sel!(sizeWithAttributes:),
        sel!(boundingRectWithSize:options:attributes:context:),
        sel!(boundingRectWithSize:options:attributes:),
    ] {
        let method = helper.instance_method(sel).expect("helper method");
        // SAFETY: the implementation treats its receiver as an NSString,
        // and the encoding is the helper method's own.
        unsafe {
            objc2::ffi::class_addMethod(
                (target as *const AnyClass).cast_mut(),
                sel,
                method.implementation(),
                objc2::ffi::method_getTypeEncoding(method),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_objects::number;
    use crate::text::layout::Align;

    #[test]
    fn attributes_are_read_from_the_dictionary() {
        let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
        let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
        let (thick, kern, raise, none) = (number(2.0), number(1.5), number(3.0), number(0.0));
        // SAFETY: the keys are constant strings.
        let keys = unsafe {
            [
                NSForegroundColorAttributeName,
                NSBackgroundColorAttributeName,
                NSUnderlineStyleAttributeName,
                NSUnderlineColorAttributeName,
                NSStrikethroughStyleAttributeName,
                NSKernAttributeName,
                NSBaselineOffsetAttributeName,
                NSLigatureAttributeName,
            ]
        };
        let values: [&AnyObject; 8] = [&red, &blue, &thick, &blue, &thick, &kern, &raise, &none];
        let attrs = attrs_of(Some(&NSDictionary::from_slices(&keys, &values)));
        assert_eq!(attrs.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(attrs.background, Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!((attrs.underline.style, attrs.underline.color), (2, Some([0.0, 0.0, 1.0, 1.0])));
        assert_eq!((attrs.strikethrough.style, attrs.strikethrough.color), (2, None));
        assert_eq!((attrs.kern, attrs.baseline_offset, attrs.ligatures), (Some(1.5), 3.0, 0));
        let plain = attrs_of(None);
        assert_eq!((plain.color, plain.kern, plain.ligatures), ([0.0, 0.0, 0.0, 1.0], None, 1));
        assert_eq!(plain.font.size, 12.0, "text without a font is 12-point");
    }

    fn record(f: impl FnOnce()) -> Vec<Op> {
        graphics::begin_recording();
        graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, 800.0, 3000.0));
        f();
        graphics::end_recording()
    }

    fn text_at(text: &str, y: f64) {
        draw(text, &[attrs_of(None)], &whole(text), Place::Point(NSPoint::new(0.0, y)));
    }

    #[test]
    fn points_are_the_corner_nearest_the_origin() {
        let text = "corner";
        let laid = layout::lay_out(text, &[attrs_of(None)], &whole(text), &Options::UNBOUNDED);
        let baseline = laid.height - laid.first_descent;
        for (xf, top) in [
            // A flipped view: the point is the text's top left corner.
            (Xf::IDENTITY, 10.0),
            // An unflipped one, 800 points tall: its bottom left corner.
            (Xf { tx: 0.0, a: -1.0, ty: 800.0 }, 790.0 - laid.height),
        ] {
            graphics::begin_recording();
            graphics::set_view(xf, Rect::new(0.0, 0.0, 800.0, 800.0));
            draw(text, &[attrs_of(None)], &whole(text), Place::Point(NSPoint::new(5.0, 10.0)));
            let ops = graphics::end_recording();
            let Some(Op::Glyphs(run)) = ops.first() else { panic!("glyphs") };
            assert_eq!((run.x, run.y), (5.0, top + baseline));
            // drawWithRect: without usesLineFragmentOrigin puts the baseline
            // at the rectangle's origin, whichever way the view faces.
            graphics::begin_recording();
            graphics::set_view(xf, Rect::new(0.0, 0.0, 800.0, 800.0));
            let r = NSRect::new(NSPoint::new(5.0, 10.0), NSSize::new(100.0, 20.0));
            draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, NSStringDrawingOptions(0)));
            let ops = graphics::end_recording();
            let Some(Op::Glyphs(run)) = ops.first() else { panic!("glyphs") };
            assert_eq!(run.y, xf.point(0.0, 10.0).1 as f32);
        }
    }

    #[test]
    fn text_laid_out_later_keeps_its_place() {
        let fill = Op::Fill { rect: Rect::new(0.0, 0.0, 10.0, 10.0), color: [1.0, 0.0, 0.0, 1.0] };
        let ops = record(|| {
            text_at("deferred, first", 0.0);
            graphics::push(fill.clone());
            text_at("deferred, second", 100.0);
        });
        let kinds: Vec<_> = ops.iter().map(|op| matches!(op, Op::Glyphs(_))).collect();
        assert_eq!(kinds, [true, false, true]);
        let (Op::Glyphs(a), Op::Glyphs(b)) = (&ops[0], &ops[2]) else { unreachable!() };
        assert!(b.y > a.y + 90.0);
        // Drawn again, the text is found laid out and recorded at once.
        let again = record(|| text_at("deferred, first", 0.0));
        let Op::Glyphs(c) = &again[0] else { panic!("glyphs") };
        assert!(Arc::ptr_eq(&a.glyphs, &c.glyphs) && (a.x, a.y) == (c.x, c.y));
    }

    fn glyph_runs(ops: &[Op]) -> Vec<&GlyphRun> {
        ops.iter().filter_map(|op| if let Op::Glyphs(run) = op { Some(run) } else { None }).collect()
    }

    fn with_style(set: impl FnOnce(&mut layout::Paragraph)) -> Attrs {
        let mut attrs = attrs_of(None);
        set(&mut attrs.paragraph);
        attrs
    }

    /// A flipped view and an unflipped one, 800 points tall, and the clip
    /// of each.
    fn views() -> [(Xf, Rect); 2] {
        let bounds = Rect::new(0.0, 0.0, 800.0, 800.0);
        [(Xf::IDENTITY, bounds), (Xf { tx: 0.0, a: -1.0, ty: 800.0 }, bounds)]
    }

    fn record_in(xf: Xf, clip: Rect, f: impl FnOnce()) -> Vec<Op> {
        graphics::begin_recording();
        graphics::set_view(xf, clip);
        f();
        graphics::end_recording()
    }

    #[test]
    fn rectangles_align_and_clip_their_text() {
        let text = "Hello";
        let r = NSRect::new(NSPoint::new(20.0, 30.0), NSSize::new(300.0, 40.0));
        let plain = layout::lay_out(text, &[attrs_of(None)], &whole(text), &Options::UNBOUNDED);
        let baseline = plain.height - plain.first_descent;
        for (xf, bounds) in views() {
            let area = xf.rect(r);
            for (align, x) in [
                (Align::Left, 20.0),
                (Align::Natural, 20.0),
                (Align::Right, 320.0 - plain.width),
                (Align::Center, 20.0 + (300.0 - plain.width) / 2.0),
            ] {
                let a = with_style(|p| p.alignment = align);
                let ops = record_in(xf, bounds, || draw(text, std::slice::from_ref(&a), &whole(text), Place::Rect(r)));
                let runs = glyph_runs(&ops);
                assert_eq!(runs.len(), 1, "{align:?}");
                assert!((runs[0].x - x).abs() < 0.01, "{align:?}: {} vs {x}", runs[0].x);
                assert_eq!(
                    runs[0].y,
                    area.y0 + baseline,
                    "the text's top is the rectangle's, whichever way the view faces"
                );
                assert_eq!(runs[0].clip, area.intersect(&bounds), "clipped to the rectangle");
            }
        }
        // Justified lines but the last reach across the rectangle.
        let long = "The quick brown fox jumps over the lazy dog and keeps on running far away from here";
        let a = with_style(|p| p.alignment = Align::Justified);
        let ops = record_in(Xf::IDENTITY, views()[0].1, || draw(long, &[a], &whole(long), Place::Rect(r)));
        let runs = glyph_runs(&ops);
        let first_line: Vec<_> = runs.iter().filter(|run| run.y == runs[0].y).collect();
        let end = first_line.iter().map(|run| run.x + run.glyphs.last().map_or(0.0, |g| g.x)).fold(0.0, f32::max);
        assert!(end > 300.0 && end <= 320.01, "the first line is spread to the right edge ({end})");
        assert!(runs.iter().any(|run| run.y > runs[0].y), "and wraps");
    }

    #[test]
    fn lines_past_the_rectangle_are_clipped_not_dropped() {
        let line = layout::lay_out("A", &[attrs_of(None)], &whole("A"), &Options::UNBOUNDED).height;
        let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, f64::from(line) * 1.5));
        let text = "First\nSecond";
        let ops = record_in(Xf::IDENTITY, views()[0].1, || draw(text, &[attrs_of(None)], &whole(text), Place::Rect(r)));
        let runs = glyph_runs(&ops);
        assert_eq!(runs.len(), 2, "both lines are drawn");
        assert!(runs[1].y > runs[0].y && runs[1].clip.y1 == line * 1.5, "the second is cut at the rectangle");
    }

    #[test]
    fn drawing_with_line_fragments_places_like_a_rectangle() {
        let text = "Hello\nWorld";
        let line = layout::lay_out("A", &[attrs_of(None)], &whole("A"), &Options::UNBOUNDED).height;
        let lfo = NSStringDrawingOptions::UsesLineFragmentOrigin;
        let truncate = lfo | NSStringDrawingOptions::TruncatesLastVisibleLine;
        let dots = layout::lay_out("…", &[attrs_of(None)], &whole("…"), &Options::UNBOUNDED).runs[0].glyphs[0].id;
        for (xf, bounds) in views() {
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, 100.0));
            let rect = record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::Rect(r)));
            let with = record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, lfo)));
            let (rect, with) = (glyph_runs(&rect), glyph_runs(&with));
            assert_eq!(rect.len(), 2);
            assert!(rect.iter().zip(&with).all(|(a, b)| (a.x, a.y, a.clip) == (b.x, b.y, b.clip)));

            // With truncatesLastVisibleLine, only lines that fit are drawn,
            // the last with an ellipsis if text was cut off...
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, f64::from(line) * 1.5));
            let ops =
                record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, truncate)));
            let runs = glyph_runs(&ops);
            assert!(runs.iter().all(|run| run.y == runs[0].y), "one line");
            assert_eq!(runs.last().and_then(|run| run.glyphs.last()).map(|g| g.id), Some(dots));
            // ...and the first line always, clipped, however short the
            // rectangle.
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, 5.0));
            let ops =
                record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, truncate)));
            let runs = glyph_runs(&ops);
            assert!(!runs.is_empty(), "the first line is drawn");
            assert_eq!(runs[0].clip, xf.rect(r));
        }
    }

    #[test]
    fn the_same_new_text_drawn_again_is_laid_out_once() {
        let text = "a label repeated down a table";
        let before = crate::text::with_ctx(|ctx| ctx.layouts.len());
        let ops = record(|| (0..10).for_each(|i| text_at(text, f64::from(i) * 20.0)));
        let runs = glyph_runs(&ops);
        assert_eq!(runs.len(), 10);
        assert!(runs.windows(2).all(|w| Arc::ptr_eq(&w[0].glyphs, &w[1].glyphs) && w[1].y > w[0].y));
        assert_eq!(crate::text::with_ctx(|ctx| ctx.layouts.len()), before + 1, "cached once");
    }

    #[test]
    fn many_lines_are_laid_out_together_as_one_by_one() {
        let lines: Vec<String> =
            (0..40).map(|i| format!("line {i} of a page laid out on the pool's threads")).collect();
        let ops = record(|| lines.iter().enumerate().for_each(|(i, l)| text_at(l, i as f64 * 20.0)));
        assert_eq!(ops.len(), 40);
        for (op, line) in ops.iter().zip(&lines) {
            let Op::Glyphs(run) = op else { panic!("glyphs") };
            let alone = layout::lay_out(line, &[attrs_of(None)], &whole(line), &Options::UNBOUNDED);
            assert_eq!(*run.glyphs, *alone.runs[0].glyphs, "{line}");
        }
    }
}
