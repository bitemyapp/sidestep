//! Drawing during `drawRect:`, as the display pass and string drawing see
//! it. Nothing here touches pixels: the current graphics context
//! (`context`) records [`Op`]s in its layer's coordinates for the render
//! thread, or rasterizes them at once into a bitmap. `NSColor` lives in
//! `color`, `NSBezierPath` in `path`; what remains here is the view
//! geometry map ([`Xf`]) and the recorder that string drawing appends to,
//! whose functions are adapters over the current context.

use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::protocol::{Op, Rect};

/// Maps a view's coordinates to its layer's: `x' = x + tx`, `y' = a·y + ty`
/// with `a = ±1` (views may be flipped relative to each other).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Xf {
    pub tx: f64,
    pub a: f64,
    pub ty: f64,
}

impl Xf {
    pub const IDENTITY: Xf = Xf { tx: 0.0, a: 1.0, ty: 0.0 };

    pub fn point(&self, x: f64, y: f64) -> (f64, f64) {
        (x + self.tx, self.a * y + self.ty)
    }

    pub fn rect(&self, r: NSRect) -> Rect {
        let (x0, y0) = self.point(r.origin.x, r.origin.y);
        let (x1, y1) = self.point(r.origin.x + r.size.width, r.origin.y + r.size.height);
        Rect::new(x0.min(x1) as f32, y0.min(y1) as f32, x0.max(x1) as f32, y0.max(y1) as f32)
    }

    pub fn inverse(&self) -> Xf {
        // y = a·y' ... solved for y: y = a·(y' - ty), since a = ±1.
        Xf { tx: -self.tx, a: self.a, ty: -self.a * self.ty }
    }

    pub fn inverse_rect(&self, r: Rect) -> NSRect {
        let inv = self.inverse();
        let (x0, y0) = inv.point(r.x0 as f64, r.y0 as f64);
        let (x1, y1) = inv.point(r.x1 as f64, r.y1 as f64);
        NSRect::new(NSPoint::new(x0.min(x1), y0.min(y1)), NSSize::new((x1 - x0).abs(), (y1 - y0).abs()))
    }

    /// `outer ∘ self`: first this map, then `outer`.
    pub fn then(&self, outer: &Xf) -> Xf {
        Xf { tx: self.tx + outer.tx, a: self.a * outer.a, ty: outer.a * self.ty + outer.ty }
    }
}

/// What the current context records into, as string drawing sees it: the
/// ops so far, the CTM as the nearest translate-and-flip, and the clip's
/// bounds.
pub(crate) struct Recorder {
    pub ops: Vec<Op>,
    pub xf: Xf,
    pub clip: Rect,
    /// Text drawn before being laid out, to lay out together at the end.
    pub pending: crate::string_drawing::Pending,
    /// Ops are rasterized as they come (a bitmap context), so text is laid
    /// out at once rather than at the end of the pass.
    pub immediate: bool,
}

impl Recorder {
    pub fn new(immediate: bool) -> Recorder {
        Recorder { ops: Vec::new(), xf: Xf::IDENTITY, clip: Rect::default(), pending: Default::default(), immediate }
    }
}

/// Start recording ops for the render thread, in a context of their own.
pub(crate) fn begin_recording() {
    crate::context::begin_recording(Xf::IDENTITY, 1.0);
}

/// The ops recorded since [`begin_recording`], text laid out.
pub(crate) fn end_recording() -> Vec<Op> {
    let rec = crate::context::end_recording();
    crate::string_drawing::finish(rec.ops, rec.pending)
}

/// Whether anything is being drawn.
pub(crate) fn recording() -> bool {
    crate::context::with_state(|_| ()).is_some()
}

/// A fresh state for drawing with `xf` and `clip`, outside any view (the
/// title bar's text, tests).
pub(crate) fn set_view(xf: Xf, clip: Rect) {
    crate::context::with_state(|st| st.reset(xf, clip));
}

/// Record directly (the window background).
pub(crate) fn push(op: Op) {
    crate::context::with_state(|st| st.push(op));
}

/// Let string drawing append to the ops. Text knows only the clip's
/// bounds, so under a path clip (`addClip`) it goes in a group the clip
/// masks.
pub(crate) fn with_recorder(f: impl FnOnce(&mut Recorder)) {
    crate::context::with_state(|st| {
        let mask = st.gs.mask.clone();
        let group = mask.is_some();
        if group {
            let draw = crate::protocol::Draw {
                xf: tiny_skia::Transform::identity(),
                blend: crate::protocol::Blend::SourceOver,
                aa: true,
                clip: st.gs.clip,
                mask,
                shadow: None,
            };
            st.rec.ops.push(Op::BeginGroup { alpha: 1.0, draw });
        }
        f(&mut st.rec);
        if group {
            st.rec.ops.push(Op::EndGroup);
        }
        st.flush();
    });
}
