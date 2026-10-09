//! Compositing on the main thread: `displayIfNeeded`'s drawing, layers'
//! contents objects as pixels, and `renderInContext:`, around the
//! compositing both threads share (`sidestep_engine::ca::render`).

use std::sync::Arc;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_quartz_core::CALayer;

pub(crate) use sidestep_engine::ca::render::*;

use super::layer::{self, CALayerImpl, Drawn, imp};
use super::math;
use crate::protocol::{ClipPath, Op, Rect};
use crate::raster::images::{ImageData, Pixels};

/// The map from a rectangle of a space (x, y, width, height) to points
/// from its top left: `down` when the space's y runs down.
pub(crate) fn top_left(r: [f64; 4], down: bool) -> crate::graphics::Xf {
    let [x, y, _, h] = r;
    if down {
        crate::graphics::Xf { tx: -x, a: 1.0, ty: -y }
    } else {
        crate::graphics::Xf { tx: -x, a: -1.0, ty: y + h }
    }
}

/// `display`'s drawing: the layer's `drawInContext:` into a context of its
/// bounds at its contents scale (flipped where its contents show flipped,
/// as Core Animation's context is), kept as the pixels it made.
pub(crate) fn record_draw_in_context(layer: &CALayerImpl) -> Drawn {
    let (bounds, scale) = layer.read(|m| (m.props.bounds, m.props.contents_scale.max(0.01)));
    let [_, _, w, h] = bounds;
    let (pw, ph) = ((w * scale).ceil().max(0.0) as u32, (h * scale).ceil().max(0.0) as u32);
    if pw == 0 || ph == 0 {
        return Drawn::None;
    }
    // Layer space to the backing store's points (top-left origin).
    let base = top_left(bounds, layer::flipped_on_screen(layer));
    crate::context::begin_recording(base, scale);
    let clip = Rect::new(0.0, 0.0, w as f32, h as f32);
    crate::context::with_state(|st| st.reset(base, clip));
    if let Some(ctx) = crate::context::current() {
        // SAFETY: -CGContext returns the context's CGContext.
        let cg: *mut CGContext = unsafe { msg_send![&*ctx, CGContext] };
        if let Some(cg) = std::ptr::NonNull::new(cg) {
            // SAFETY: the context lives while it's current; drawInContext:
            // takes it.
            let _: () = unsafe { msg_send![layer, drawInContext: cg.as_ref()] };
        }
    }
    let rec = crate::context::end_recording();
    Drawn::Ops(Arc::new(rec.ops), [w, h], scale)
}

/// The image a drawn layer's backing store is (`contents`, after
/// `display`): a CGImage of its pixels.
pub(crate) fn backing_store_object(layer: &CALayerImpl) -> Retained<AnyObject> {
    let image = drawn_image(layer);
    match image.and_then(|(img, _)| cg_image_of(&img)) {
        Some(cg) => super::objects::any(cg),
        None => super::objects::any(objc2_foundation::NSNull::null()),
    }
}

/// A drawn layer's pixels, made the first time they're asked for.
pub(crate) fn drawn_image(layer: &CALayerImpl) -> Option<(Arc<ImageData>, [f64; 2])> {
    let drawn = layer.read(|m| m.drawn.clone());
    match drawn {
        Drawn::Ops(ops, [w, h], scale) => CACHE.with(|c| {
            // The cache holds the ops it was made from, so they can't be
            // freed and others made at their address.
            if let Some((k, img)) = c.borrow().as_ref()
                && Arc::ptr_eq(k, &ops)
            {
                return Some((img.clone(), [w, h]));
            }
            let img = rasterize(&ops, (w * scale).ceil() as u32, (h * scale).ceil() as u32, scale)?;
            *c.borrow_mut() = Some((ops, img.clone()));
            Some((img, [w, h]))
        }),
        Drawn::None => None,
    }
}

/// A backing store rasterized, and the ops it was made from.
type Rasterized = (Arc<Vec<Op>>, Arc<ImageData>);

thread_local! {
    /// The last backing store rasterized.
    static CACHE: std::cell::RefCell<Option<Rasterized>> = const { std::cell::RefCell::new(None) };
}

fn cg_image_of(img: &ImageData) -> Option<Retained<CGImage>> {
    let Pixels::Rgba(bytes) = &img.pixels else { return None };
    let space = crate::coregraphics::color::srgb();
    let cg = crate::coregraphics::image::from_rgba(img.width as usize, img.height as usize, bytes.clone(), space)?;
    // SAFETY: CGImageImpl is what CGImage names.
    Some(unsafe { Retained::cast_unchecked(cg) })
}

/// The pixels of a layer's contents object: a CGImage, an NSImage, or a
/// backing store it drew; and their size in points.
pub(crate) fn contents_image(layer: &CALayerImpl) -> Option<(Arc<ImageData>, [f64; 2])> {
    let (contents, scale) = layer.read(|m| (m.objs.contents.clone(), m.props.contents_scale.max(0.01)));
    let Some(obj) = contents else { return drawn_image(layer) };
    image_of_object(&obj, scale)
}

/// The pixels of an image object and its size in points at `scale`.
pub(crate) fn image_of_object(obj: &AnyObject, scale: f64) -> Option<(Arc<ImageData>, [f64; 2])> {
    if let Some(cg) = obj.downcast_ref::<crate::coregraphics::image::CGImageImpl>() {
        let data = cg.pixels()?;
        let size = [data.width as f64 / scale, data.height as f64 / scale];
        return Some((data, size));
    }
    if let Some(image) = obj.downcast_ref::<objc2_app_kit::NSImage>() {
        // The image's best representation for its size.
        // SAFETY: -size returns the image's size in points.
        let size: objc2_foundation::NSSize = unsafe { msg_send![image, size] };
        let mut rect = objc2_foundation::NSRect::new(objc2_foundation::NSPoint::ZERO, size);
        let none: Option<&AnyObject> = None;
        // SAFETY: the method takes a rectangle pointer, a context and hints.
        let cg: *mut CGImage =
            unsafe { msg_send![image, CGImageForProposedRect: &mut rect, context: none, hints: none] };
        let cg = std::ptr::NonNull::new(cg)?;
        // SAFETY: the image keeps its CGImage alive.
        let cgi = crate::coregraphics::image::image_imp(unsafe { cg.as_ref() });
        let data = cgi.pixels()?;
        return Some((data, [size.width, size.height]));
    }
    None
}

// The model tree as nodes, for renderInContext:.

/// `layer` and its sublayers as nodes, at their model values (as
/// `renderInContext:` draws them, leaving animations out).
pub(crate) fn model_node(layer: &CALayer) -> Node {
    let li = imp(layer);
    let (props, subs, mask) = li.read(|m| (m.props.clone(), m.sublayers.clone(), m.mask.clone()));
    let content = match li.view() {
        Some(view) => super::backing::view_content(&view, &props).unwrap_or(NodeContent::None),
        None => match contents_image(li) {
            Some((image, size)) => NodeContent::Image { image, size },
            None => NodeContent::None,
        },
    };
    Node {
        props,
        content,
        children: subs.iter().map(|s| model_node(s)).collect(),
        mask: mask.map(|m| Box::new(model_node(&m))),
        transition: None,
    }
}

/// `renderInContext:`: the layer (in its own space, which is the
/// context's user space) and its sublayers, at their model values.
pub(crate) fn render_in_context(layer: &CALayer, ctx: &CGContext) {
    // Displays owed first, as Core Animation's does.
    for l in layer::tree(layer) {
        if imp(&l).read(|m| m.needs_display) && imp(&l).view().is_none() {
            // SAFETY: -displayIfNeeded takes nothing.
            let _: () = unsafe { msg_send![&*l, displayIfNeeded] };
        }
    }
    let mut node = model_node(layer);
    // The layer's own geometry: drawn where its bounds are in its own
    // space, so its position, transform and flip don't place it; its flip
    // still turns its contents (and its sublayers') over (measured).
    let props = &mut node.props;
    let [bx, by, w, h] = props.bounds;
    props.position = [bx + props.anchor[0] * w, by + props.anchor[1] * h];
    props.transform = math::IDENTITY;
    props.geometry_flipped = false;
    let flipped = layer::flipped_on_screen(imp(layer));
    crate::coregraphics::context::with_state(ctx, |st| {
        let [a, b, c, d, e, f] = st.gs.ctm.as_coeffs();
        let base = [a, b, c, d, e, f];
        let clip =
            Clip { rect: st.gs.clip, paths: st.gs.mask.as_deref().map(<[ClipPath]>::to_vec).unwrap_or_default() };
        let mut ops = Vec::new();
        // User space runs up where the CTM turns it over.
        let down = (d > 0.0) ^ flipped;
        emit(&node, &base, down, &clip, &mut ops);
        // All at once: a bitmap context draws what it has each time no
        // transparency layer of its own is open, and the tree's groups
        // aren't its.
        for mut op in ops {
            st.adjust(&mut op);
            st.rec.ops.push(op);
        }
        st.flush();
    });
}
