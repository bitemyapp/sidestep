//! `CALayer`: the model layer programs change.
//!
//! A layer keeps its properties as [`Props`] (plain data) and the objects
//! it was given (colors, contents, paths, the names of its gravity and
//! filters) beside them, so getters hand back what they were given. Every
//! change goes through [`change`]: when the layer has been committed into
//! a window's layer tree (it is *live*, as on macOS, where a layer that
//! never was has no implicit animations and no presentation layer) and
//! the transaction allows actions, the layer asks `actionForKey:` before
//! the change and runs the action after it (a default action adds an
//! implicit animation, or a fade transition for the keys macOS fades);
//! then it is marked dirty for the next commit (`transaction`).
//!
//! Geometry follows CoreAnimation as measured: `frame` is the box around
//! the transformed bounds in the superlayer, set by moving the position to
//! where the anchor falls in it and sizing the bounds; a layer's geometry
//! flipped (`geometryFlipped`) turns its own space about the middle of its
//! bounds relative to its superlayer's, which conversions between it and
//! its superlayer (hit testing among them) follow, and conversions between
//! its sublayers and it don't.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_core_foundation::{CFTimeInterval, CGAffineTransform, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGContext, CGPath};
use objc2_foundation::{NSArray, NSDictionary, NSNull, NSString, NSUInteger};
use objc2_quartz_core::{
    CAAction, CAAnimation, CAAutoresizingMask, CACornerMask, CAEdgeAntialiasingMask, CALayer, CATransform3D,
};

use super::math;
use super::objects;
use super::props::{self, Gravity, Key, KeyPath, Kind, Part, Props, Value, apply_affine, compose, invert_affine};
use super::spec::{AnimSpec, Fill, Timing};
use super::transaction;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A layer's name on both threads.
pub(crate) type LayerId = crate::protocol::CaLayerId;

/// An animation a layer holds: its key (none for one added without),
/// the frozen copy `animationForKey:` hands out, and what it does.
pub(crate) struct Added {
    pub key: Option<String>,
    pub object: Retained<CAAnimation>,
    pub spec: Arc<AnimSpec>,
    /// Its begin time was settled at a commit (it was 0 until then).
    pub started: bool,
    /// It ended and stays (it isn't removed on completion), and its
    /// delegate and group have been told: nothing waits for it any more.
    pub ended: bool,
    /// The completion groups that wait for it.
    pub groups: Vec<std::rc::Rc<transaction::Group>>,
}

/// The objects a layer was given.
pub(crate) struct Objects {
    pub background_color: Option<Retained<CGColor>>,
    pub border_color: Option<Retained<CGColor>>,
    pub shadow_color: Option<Retained<CGColor>>,
    pub contents: Option<Retained<AnyObject>>,
    pub shadow_path: Option<Retained<CGPath>>,
    pub gravity: Retained<NSString>,
    pub format: Retained<NSString>,
    pub min_filter: Retained<NSString>,
    pub mag_filter: Retained<NSString>,
    pub corner_curve: Retained<NSString>,
    pub compositing_filter: Option<Retained<AnyObject>>,
    pub filters: Option<Retained<NSArray>>,
    pub background_filters: Option<Retained<NSArray>>,
    pub fill_mode: Retained<NSString>,
}

impl Objects {
    fn new() -> Objects {
        let s = NSString::from_str;
        Objects {
            background_color: None,
            border_color: None,
            shadow_color: None,
            contents: None,
            shadow_path: None,
            gravity: s("resize"),
            format: s("RGBA8"),
            min_filter: s("linear"),
            mag_filter: s("linear"),
            corner_curve: s("circular"),
            compositing_filter: None,
            filters: None,
            background_filters: None,
            fill_mode: s("removed"),
        }
    }
}

/// What a layer displays, drawn by `display`.
#[derive(Clone, Default)]
pub(crate) enum Drawn {
    #[default]
    None,
    /// What `drawInContext:` recorded, in the layer's points from its
    /// bounds' top left, and the size and scale it was drawn at.
    Ops(Arc<Vec<crate::protocol::Op>>, [f64; 2], f64),
}

pub(crate) struct Model {
    pub props: Props,
    pub objs: Objects,
    pub timing: Timing,
    pub sublayers: Vec<Retained<CALayer>>,
    pub superlayer: Option<NonNull<CALayer>>,
    pub mask: Option<Retained<CALayer>>,
    /// The layer this one is the mask of.
    pub mask_of: Option<NonNull<CALayer>>,
    pub delegate: Weak<AnyObject>,
    pub has_delegate: bool,
    pub actions: Option<Retained<NSDictionary>>,
    pub style: Option<Retained<NSDictionary>>,
    pub name: Option<Retained<NSString>>,
    pub layout_manager: Option<Retained<AnyObject>>,
    pub anims: Vec<Added>,
    pub needs_display: bool,
    pub needs_layout: bool,
    pub needs_display_on_bounds_change: bool,
    pub draws_asynchronously: bool,
    pub should_rasterize: bool,
    pub rasterization_scale: f64,
    pub edge_aa: u32,
    pub allows_edge_aa: bool,
    pub autoresizing: u32,
    pub min_filter_bias: f32,
    pub edr: bool,
    pub dynamic_scaling: bool,
    pub headroom: f64,
    /// The keys set explicitly (so a style doesn't override them), by
    /// their index in [`Key::ALL`].
    pub explicit: u64,
    /// Values set for keys that aren't properties.
    pub extras: Vec<(Retained<NSString>, Retained<AnyObject>)>,
    pub drawn: Drawn,
    /// Counts changes of what the layer displays, for the render thread.
    pub content_version: u64,
    /// A shape or gradient layer's objects.
    pub kind_objs: super::shape::KindObjects,
    /// The properties as last committed: what the presentation layer shows
    /// under the animations (changes not yet committed don't show, as on
    /// macOS).
    pub committed: Option<Props>,
}

impl Model {
    fn new() -> Model {
        Model {
            props: Props::default(),
            objs: Objects::new(),
            // A layer lasts forever (measured).
            timing: Timing { duration: f64::INFINITY, ..Timing::default() },
            sublayers: Vec::new(),
            superlayer: None,
            mask: None,
            mask_of: None,
            delegate: Weak::default(),
            has_delegate: false,
            actions: None,
            style: None,
            name: None,
            layout_manager: None,
            anims: Vec::new(),
            needs_display: false,
            needs_layout: false,
            needs_display_on_bounds_change: false,
            draws_asynchronously: false,
            should_rasterize: false,
            rasterization_scale: 1.0,
            edge_aa: 15,
            allows_edge_aa: true,
            autoresizing: 0,
            min_filter_bias: 0.0,
            edr: false,
            dynamic_scaling: false,
            headroom: 0.0,
            explicit: 0,
            extras: Vec::new(),
            drawn: Drawn::None,
            content_version: 0,
            kind_objs: Default::default(),
            committed: None,
        }
    }
}

pub(crate) struct LayerIvars {
    pub id: LayerId,
    pub model: RefCell<Model>,
    /// Committed into a window's layer tree at least once.
    pub live: Cell<bool>,
    /// Waiting in a transaction's list of changed layers.
    pub dirty: Cell<bool>,
    /// Sent to the render thread (so it must be told when it goes).
    pub sent: Cell<bool>,
    /// The view this layer backs, if one does (see `backing`). Weak: the
    /// layer may outlive the view (a program keeps it, or a transaction's
    /// list of changed layers does until the end of the turn).
    pub view: RefCell<Weak<objc2_app_kit::NSView>>,
    /// This is a presentation layer.
    pub is_presentation: Cell<bool>,
    /// For a presentation layer: the model layer it presents. Weak: the
    /// model keeps its last presentation layer, which mustn't keep it.
    pub model_layer: RefCell<Weak<CALayer>>,
    /// The presentation layer last made, and when (media time).
    pub presentation: RefCell<Option<(f64, Retained<CALayer>)>>,
}

impl Drop for LayerIvars {
    fn drop(&mut self) {
        if self.is_presentation.get() {
            // A copy: its mask is the model's, and it has no sublayers.
            return;
        }
        let model = self.model.get_mut();
        // Sublayers that outlive this layer no longer point at it.
        for sub in &model.sublayers {
            imp(sub).ivars().model.borrow_mut().superlayer = None;
        }
        if let Some(mask) = &model.mask {
            imp(mask).ivars().model.borrow_mut().mask_of = None;
        }
        // Animations still running stop unfinished: their delegates and
        // completion blocks hear, as on macOS.
        for added in std::mem::take(&mut model.anims) {
            if !added.ended {
                transaction::stopped_later(added, false);
            }
        }
        if self.sent.get() {
            transaction::layer_gone(self.id);
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. A layer belongs to
    // one thread at a time, as Core Animation's model layers do; its state
    // is never borrowed while a message it sends runs.
    #[unsafe(super(NSObject))]
    #[name = "CALayer"]
    #[ivars = LayerIvars]
    pub(crate) struct CALayerImpl;

    impl CALayerImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ivars());
            // SAFETY: NSObject's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            class_defaults(&this);
            this
        }

        /// A subclass copies its own properties from `layer` here (the
        /// base class copies none, as on macOS).
        #[unsafe(method_id(initWithLayer:))]
        fn init_with_layer(this: Allocated<Self>, _layer: &AnyObject) -> Retained<Self> {
            let this = this.set_ivars(ivars());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(presentationLayer))]
        fn presentation_layer(&self) -> Option<Retained<CALayer>> {
            presentation(self)
        }

        #[unsafe(method_id(modelLayer))]
        fn model_layer(&self) -> Retained<CALayer> {
            self.ivars().model_layer.borrow().load().unwrap_or_else(|| as_layer(self).retain())
        }

        #[unsafe(method_id(defaultValueForKey:))]
        fn default_value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            default_value(&key.to_string())
        }

        #[unsafe(method(needsDisplayForKey:))]
        fn needs_display_for_key(_key: &NSString) -> bool {
            false
        }

        #[unsafe(method_id(defaultActionForKey:))]
        fn default_action_for_key(_key: &NSString) -> Option<Retained<ProtocolObject<dyn CAAction>>> {
            None
        }

        #[unsafe(method(cornerCurveExpansionFactor:))]
        fn corner_curve_expansion_factor(curve: &NSString) -> CGFloat {
            if curve.to_string() == "continuous" { CONTINUOUS_EXPANSION } else { 1.0 }
        }

        #[unsafe(method(shouldArchiveValueForKey:))]
        fn should_archive_value_for_key(&self, _key: &NSString) -> bool {
            true
        }

        // Geometry.

        #[unsafe(method(bounds))]
        fn bounds(&self) -> CGRect {
            rect(self.read(|m| m.props.bounds))
        }

        #[unsafe(method(setBounds:))]
        fn set_bounds(&self, bounds: CGRect) {
            // Standardized, as Core Animation keeps it (measured).
            let r = props::standardize([bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height]);
            let old = self.read(|m| m.props.bounds);
            change(self, Key::Bounds, |m| m.props.bounds = r);
            bounds_changed(self, old, r);
        }

        #[unsafe(method(position))]
        fn position(&self) -> CGPoint {
            point(self.read(|m| m.props.position))
        }

        #[unsafe(method(setPosition:))]
        fn set_position(&self, p: CGPoint) {
            change(self, Key::Position, |m| m.props.position = [p.x, p.y]);
        }

        #[unsafe(method(zPosition))]
        fn z_position(&self) -> CGFloat {
            self.read(|m| m.props.z_position)
        }

        #[unsafe(method(setZPosition:))]
        fn set_z_position(&self, z: CGFloat) {
            change(self, Key::ZPosition, |m| m.props.z_position = z);
        }

        #[unsafe(method(anchorPoint))]
        fn anchor_point(&self) -> CGPoint {
            point(self.read(|m| m.props.anchor))
        }

        #[unsafe(method(setAnchorPoint:))]
        fn set_anchor_point(&self, p: CGPoint) {
            change(self, Key::AnchorPoint, |m| m.props.anchor = [p.x, p.y]);
        }

        #[unsafe(method(anchorPointZ))]
        fn anchor_point_z(&self) -> CGFloat {
            self.read(|m| m.props.anchor_z)
        }

        #[unsafe(method(setAnchorPointZ:))]
        fn set_anchor_point_z(&self, z: CGFloat) {
            change(self, Key::AnchorPointZ, |m| m.props.anchor_z = z);
        }

        #[unsafe(method(transform))]
        fn transform(&self) -> CATransform3D {
            super::transform::from_mat(&self.read(|m| m.props.transform))
        }

        #[unsafe(method(setTransform:))]
        fn set_transform(&self, t: CATransform3D) {
            let t = super::transform::to_mat(&t);
            change(self, Key::Transform, |m| m.props.transform = t);
        }

        #[unsafe(method(affineTransform))]
        fn affine_transform(&self) -> CGAffineTransform {
            let t = self.read(|m| m.props.transform);
            CGAffineTransform { a: t[0], b: t[1], c: t[4], d: t[5], tx: t[12], ty: t[13] }
        }

        #[unsafe(method(setAffineTransform:))]
        fn set_affine_transform(&self, a: CGAffineTransform) {
            let t = super::transform::CATransform3DMakeAffineTransform(a);
            // SAFETY: setTransform: takes a CATransform3D.
            let _: () = unsafe { msg_send![self, setTransform: t] };
        }

        #[unsafe(method(frame))]
        fn frame(&self) -> CGRect {
            rect(self.read(|m| m.props.frame()))
        }

        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: CGRect) {
            let r = [frame.origin.x, frame.origin.y, frame.size.width, frame.size.height];
            let (position, bounds) = self.read(|m| m.props.set_frame(r));
            // By message, as Core Animation does, so the implicit
            // animations are the position's and the bounds'.
            // SAFETY: the setters take a point and a rectangle.
            unsafe {
                let _: () = msg_send![self, setPosition: point(position)];
                let _: () = msg_send![self, setBounds: rect(bounds)];
            }
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            self.read(|m| m.props.hidden)
        }

        #[unsafe(method(setHidden:))]
        fn set_hidden(&self, hidden: bool) {
            change(self, Key::Hidden, |m| m.props.hidden = hidden);
        }

        #[unsafe(method(isDoubleSided))]
        fn is_double_sided(&self) -> bool {
            self.read(|m| m.props.double_sided)
        }

        #[unsafe(method(setDoubleSided:))]
        fn set_double_sided(&self, flag: bool) {
            change(self, Key::DoubleSided, |m| m.props.double_sided = flag);
        }

        #[unsafe(method(isGeometryFlipped))]
        fn is_geometry_flipped(&self) -> bool {
            self.read(|m| m.props.geometry_flipped)
        }

        #[unsafe(method(setGeometryFlipped:))]
        fn set_geometry_flipped(&self, flag: bool) {
            change(self, Key::GeometryFlipped, |m| m.props.geometry_flipped = flag);
        }

        #[unsafe(method(contentsAreFlipped))]
        fn contents_are_flipped(&self) -> bool {
            flipped_on_screen(self)
        }

        #[unsafe(method(sublayerTransform))]
        fn sublayer_transform(&self) -> CATransform3D {
            super::transform::from_mat(&self.read(|m| m.props.sublayer_transform))
        }

        #[unsafe(method(setSublayerTransform:))]
        fn set_sublayer_transform(&self, t: CATransform3D) {
            let t = super::transform::to_mat(&t);
            change(self, Key::SublayerTransform, |m| m.props.sublayer_transform = t);
        }

        // The tree.

        #[unsafe(method_id(superlayer))]
        fn superlayer(&self) -> Option<Retained<CALayer>> {
            superlayer_of(self)
        }

        #[unsafe(method(removeFromSuperlayer))]
        fn remove_from_superlayer(&self) {
            remove_from_superlayer(self);
        }

        #[unsafe(method_id(sublayers))]
        fn sublayers(&self) -> Option<Retained<NSArray<CALayer>>> {
            sublayers_of(self)
        }

        #[unsafe(method(setSublayers:))]
        fn set_sublayers(&self, sublayers: Option<&NSArray<CALayer>>) {
            set_sublayers(self, sublayers.map(|l| l.to_vec()).unwrap_or_default());
        }

        #[unsafe(method(addSublayer:))]
        fn add_sublayer(&self, layer: &CALayer) {
            insert(self, layer, usize::MAX);
        }

        #[unsafe(method(insertSublayer:atIndex:))]
        fn insert_sublayer_at_index(&self, layer: &CALayer, index: std::ffi::c_uint) {
            insert(self, layer, index as usize);
        }

        /// Below a sibling; a layer that isn't one puts it last (measured).
        #[unsafe(method(insertSublayer:below:))]
        fn insert_sublayer_below(&self, layer: &CALayer, sibling: Option<&CALayer>) {
            let at = match sibling {
                Some(s) => self.index_of(s).unwrap_or(usize::MAX),
                None => 0,
            };
            insert(self, layer, at);
        }

        #[unsafe(method(insertSublayer:above:))]
        fn insert_sublayer_above(&self, layer: &CALayer, sibling: Option<&CALayer>) {
            let at = sibling.and_then(|s| self.index_of(s)).map_or(usize::MAX, |i| i + 1);
            insert(self, layer, at);
        }

        #[unsafe(method(replaceSublayer:with:))]
        fn replace_sublayer_with(&self, old: &CALayer, new: &CALayer) {
            let Some(at) = self.index_of(old) else {
                panic!("replaced layer {old:p} is not a sublayer of {:p}", as_layer(self));
            };
            if std::ptr::eq(old, new) {
                return;
            }
            replace(self, old, new, at);
        }

        #[unsafe(method_id(mask))]
        fn mask(&self) -> Option<Retained<CALayer>> {
            self.read(|m| m.mask.clone())
        }

        #[unsafe(method(setMask:))]
        fn set_mask(&self, mask: Option<&CALayer>) {
            let mask = mask.map(Message::retain);
            let old = self.read(|m| m.mask.clone());
            if old.as_deref().map(|o| o as *const CALayer) == mask.as_deref().map(|m| m as *const CALayer) {
                return;
            }
            if let Some(new) = &mask {
                check_cycle(self, new);
                // Out of its superlayer (a mask's superlayer is the layer
                // it masks, measured).
                unlink(imp(new));
                imp(new).write(|m| m.mask_of = Some(NonNull::from(as_layer(self))));
            }
            if let Some(old) = &old {
                imp(old).write(|m| m.mask_of = None);
                transaction::detached(imp(old));
            }
            let added = mask.clone();
            change(self, Key::Mask, |m| m.mask = mask);
            transaction::structure_changed(self);
            if let Some(new) = added {
                transaction::attached(imp(&new));
                run_order_action(imp(&new), "onOrderIn");
            }
        }

        #[unsafe(method(masksToBounds))]
        fn masks_to_bounds(&self) -> bool {
            self.read(|m| m.props.masks_to_bounds)
        }

        #[unsafe(method(setMasksToBounds:))]
        fn set_masks_to_bounds(&self, flag: bool) {
            change(self, Key::MasksToBounds, |m| m.props.masks_to_bounds = flag);
        }

        // Conversions and hit testing.

        #[unsafe(method(convertPoint:fromLayer:))]
        fn convert_point_from_layer(&self, p: CGPoint, layer: Option<&CALayer>) -> CGPoint {
            let m = map_between(layer.map(imp), Some(self));
            let (x, y) = apply_affine(&m, p.x, p.y);
            CGPoint::new(x, y)
        }

        #[unsafe(method(convertPoint:toLayer:))]
        fn convert_point_to_layer(&self, p: CGPoint, layer: Option<&CALayer>) -> CGPoint {
            let m = map_between(Some(self), layer.map(imp));
            let (x, y) = apply_affine(&m, p.x, p.y);
            CGPoint::new(x, y)
        }

        #[unsafe(method(convertRect:fromLayer:))]
        fn convert_rect_from_layer(&self, r: CGRect, layer: Option<&CALayer>) -> CGRect {
            map_rect(&map_between(layer.map(imp), Some(self)), r)
        }

        #[unsafe(method(convertRect:toLayer:))]
        fn convert_rect_to_layer(&self, r: CGRect, layer: Option<&CALayer>) -> CGRect {
            map_rect(&map_between(Some(self), layer.map(imp)), r)
        }

        #[unsafe(method(convertTime:fromLayer:))]
        fn convert_time_from_layer(&self, t: CFTimeInterval, layer: Option<&CALayer>) -> CFTimeInterval {
            let media = layer.map_or(Some(t), |l| to_media(imp(l), t));
            media.map_or(t, |m| local_time(self, m))
        }

        #[unsafe(method(convertTime:toLayer:))]
        fn convert_time_to_layer(&self, t: CFTimeInterval, layer: Option<&CALayer>) -> CFTimeInterval {
            // A paused layer's times are all one moment: 0 (measured).
            let Some(media) = to_media(self, t) else { return 0.0 };
            layer.map_or(media, |l| local_time(imp(l), media))
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, p: CGPoint) -> Option<Retained<CALayer>> {
            hit(self, [p.x, p.y])
        }

        #[unsafe(method(containsPoint:))]
        fn contains_point(&self, p: CGPoint) -> bool {
            let [x, y, w, h] = self.read(|m| m.props.bounds);
            p.x >= x && p.x < x + w && p.y >= y && p.y < y + h
        }

        // Contents.

        #[unsafe(method_id(contents))]
        fn contents(&self) -> Option<Retained<AnyObject>> {
            let (given, drawn) = self.read(|m| (m.objs.contents.clone(), matches!(m.drawn, Drawn::Ops(..))));
            given.or_else(|| drawn.then(|| backing_store(self)))
        }

        #[unsafe(method(setContents:))]
        fn set_contents(&self, contents: Option<&AnyObject>) {
            let contents = contents.map(Message::retain);
            change(self, Key::Contents, |m| {
                m.objs.contents = contents;
                m.drawn = Drawn::None;
                m.content_version += 1;
            });
        }

        #[unsafe(method(contentsRect))]
        fn contents_rect(&self) -> CGRect {
            rect(self.read(|m| m.props.contents_rect))
        }

        #[unsafe(method(setContentsRect:))]
        fn set_contents_rect(&self, r: CGRect) {
            let r = [r.origin.x, r.origin.y, r.size.width, r.size.height];
            change(self, Key::ContentsRect, |m| m.props.contents_rect = r);
        }

        #[unsafe(method_id(contentsGravity))]
        fn contents_gravity(&self) -> Retained<NSString> {
            self.read(|m| m.objs.gravity.clone())
        }

        #[unsafe(method(setContentsGravity:))]
        fn set_contents_gravity(&self, gravity: &NSString) {
            // An unknown name is taken as the center (measured).
            let (g, copy) = match Gravity::known(&gravity.to_string()) {
                Some(g) => (g, objc2_foundation::NSCopying::copy(gravity)),
                None => (Gravity::Center, NSString::from_str("center")),
            };
            quiet_change(self, |m| {
                m.objs.gravity = copy;
                m.props.gravity = g;
            });
        }

        #[unsafe(method(contentsScale))]
        fn contents_scale(&self) -> CGFloat {
            self.read(|m| m.props.contents_scale)
        }

        #[unsafe(method(setContentsScale:))]
        fn set_contents_scale(&self, s: CGFloat) {
            change(self, Key::ContentsScale, |m| m.props.contents_scale = s);
        }

        #[unsafe(method(contentsCenter))]
        fn contents_center(&self) -> CGRect {
            rect(self.read(|m| m.props.contents_center))
        }

        #[unsafe(method(setContentsCenter:))]
        fn set_contents_center(&self, r: CGRect) {
            let r = [r.origin.x, r.origin.y, r.size.width, r.size.height];
            change(self, Key::ContentsCenter, |m| m.props.contents_center = r);
        }

        #[unsafe(method_id(contentsFormat))]
        fn contents_format(&self) -> Retained<NSString> {
            self.read(|m| m.objs.format.clone())
        }

        #[unsafe(method(setContentsFormat:))]
        fn set_contents_format(&self, format: &NSString) {
            let copy = objc2_foundation::NSCopying::copy(format);
            quiet_change(self, |m| m.objs.format = copy);
        }

        #[unsafe(method(wantsExtendedDynamicRangeContent))]
        fn wants_edr(&self) -> bool {
            self.read(|m| m.edr)
        }

        #[unsafe(method(setWantsExtendedDynamicRangeContent:))]
        fn set_wants_edr(&self, flag: bool) {
            quiet_change(self, |m| m.edr = flag);
        }

        #[unsafe(method(contentsHeadroom))]
        fn contents_headroom(&self) -> CGFloat {
            self.read(|m| m.headroom)
        }

        #[unsafe(method(setContentsHeadroom:))]
        fn set_contents_headroom(&self, h: CGFloat) {
            quiet_change(self, |m| m.headroom = h);
        }

        #[unsafe(method(wantsDynamicContentScaling))]
        fn wants_dynamic_content_scaling(&self) -> bool {
            self.read(|m| m.dynamic_scaling)
        }

        #[unsafe(method(setWantsDynamicContentScaling:))]
        fn set_wants_dynamic_content_scaling(&self, flag: bool) {
            quiet_change(self, |m| m.dynamic_scaling = flag);
        }

        #[unsafe(method_id(minificationFilter))]
        fn minification_filter(&self) -> Retained<NSString> {
            self.read(|m| m.objs.min_filter.clone())
        }

        #[unsafe(method(setMinificationFilter:))]
        fn set_minification_filter(&self, f: &NSString) {
            let nearest = f.to_string() == "nearest";
            let copy = objc2_foundation::NSCopying::copy(f);
            quiet_change(self, |m| {
                m.objs.min_filter = copy;
                m.props.min_nearest = nearest;
            });
        }

        #[unsafe(method_id(magnificationFilter))]
        fn magnification_filter(&self) -> Retained<NSString> {
            self.read(|m| m.objs.mag_filter.clone())
        }

        #[unsafe(method(setMagnificationFilter:))]
        fn set_magnification_filter(&self, f: &NSString) {
            let nearest = f.to_string() == "nearest";
            let copy = objc2_foundation::NSCopying::copy(f);
            quiet_change(self, |m| {
                m.objs.mag_filter = copy;
                m.props.mag_nearest = nearest;
            });
        }

        #[unsafe(method(minificationFilterBias))]
        fn minification_filter_bias(&self) -> f32 {
            self.read(|m| m.min_filter_bias)
        }

        #[unsafe(method(setMinificationFilterBias:))]
        fn set_minification_filter_bias(&self, bias: f32) {
            quiet_change(self, |m| m.min_filter_bias = bias);
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.read(|m| m.props.opaque)
        }

        #[unsafe(method(setOpaque:))]
        fn set_opaque(&self, flag: bool) {
            asking_change(self, "opaque", |m| m.props.opaque = flag);
        }

        // Display.

        #[unsafe(method(display))]
        fn display(&self) {
            display(self);
        }

        #[unsafe(method(setNeedsDisplay))]
        fn set_needs_display(&self) {
            set_needs_display(self);
        }

        #[unsafe(method(setNeedsDisplayInRect:))]
        fn set_needs_display_in_rect(&self, _r: CGRect) {
            set_needs_display(self);
        }

        #[unsafe(method(needsDisplay))]
        fn needs_display(&self) -> bool {
            self.read(|m| m.needs_display)
        }

        #[unsafe(method(displayIfNeeded))]
        fn display_if_needed(&self) {
            if self.read(|m| m.needs_display) {
                // SAFETY: -display takes nothing.
                let _: () = unsafe { msg_send![self, display] };
            }
        }

        #[unsafe(method(needsDisplayOnBoundsChange))]
        fn needs_display_on_bounds_change(&self) -> bool {
            self.read(|m| m.needs_display_on_bounds_change)
        }

        #[unsafe(method(setNeedsDisplayOnBoundsChange:))]
        fn set_needs_display_on_bounds_change(&self, flag: bool) {
            asking_change(self, "needsDisplayOnBoundsChange", |m| m.needs_display_on_bounds_change = flag);
        }

        #[unsafe(method(drawsAsynchronously))]
        fn draws_asynchronously(&self) -> bool {
            self.read(|m| m.draws_asynchronously)
        }

        #[unsafe(method(setDrawsAsynchronously:))]
        fn set_draws_asynchronously(&self, flag: bool) {
            self.write(|m| m.draws_asynchronously = flag);
        }

        #[unsafe(method(drawInContext:))]
        fn draw_in_context(&self, ctx: &CGContext) {
            let Some(delegate) = self.delegate_object() else { return };
            // SAFETY: -respondsToSelector: takes a selector.
            let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(drawLayer:inContext:)] };
            if responds {
                // SAFETY: the delegate draws the layer into the context.
                let _: () = unsafe { msg_send![&*delegate, drawLayer: as_layer(self), inContext: ctx] };
            }
        }

        #[unsafe(method(renderInContext:))]
        fn render_in_context(&self, ctx: &CGContext) {
            super::render::render_in_context(as_layer(self), ctx);
        }

        #[unsafe(method(edgeAntialiasingMask))]
        fn edge_antialiasing_mask(&self) -> CAEdgeAntialiasingMask {
            CAEdgeAntialiasingMask(self.read(|m| m.edge_aa))
        }

        #[unsafe(method(setEdgeAntialiasingMask:))]
        fn set_edge_antialiasing_mask(&self, mask: CAEdgeAntialiasingMask) {
            // Four edges' bits (measured).
            self.write(|m| m.edge_aa = mask.0 & 15);
        }

        #[unsafe(method(allowsEdgeAntialiasing))]
        fn allows_edge_antialiasing(&self) -> bool {
            self.read(|m| m.allows_edge_aa)
        }

        #[unsafe(method(setAllowsEdgeAntialiasing:))]
        fn set_allows_edge_antialiasing(&self, flag: bool) {
            self.write(|m| m.allows_edge_aa = flag);
        }

        // Appearance.

        #[unsafe(method(backgroundColor))]
        fn background_color(&self) -> *mut CGColor {
            autoreleased(self.read(|m| m.objs.background_color.clone()))
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&CGColor>) {
            let rgba = color.map(objects::cg_rgba);
            let color = color.map(Message::retain);
            change(self, Key::BackgroundColor, |m| {
                m.objs.background_color = color;
                m.props.background_color = rgba;
            });
        }

        #[unsafe(method(cornerRadius))]
        fn corner_radius(&self) -> CGFloat {
            self.read(|m| m.props.corner_radius)
        }

        #[unsafe(method(setCornerRadius:))]
        fn set_corner_radius(&self, r: CGFloat) {
            change(self, Key::CornerRadius, |m| m.props.corner_radius = r);
        }

        #[unsafe(method(maskedCorners))]
        fn masked_corners(&self) -> CACornerMask {
            CACornerMask(self.read(|m| m.props.masked_corners) as NSUInteger)
        }

        #[unsafe(method(setMaskedCorners:))]
        fn set_masked_corners(&self, mask: CACornerMask) {
            let bits = (mask.0 & 15) as u8;
            quiet_change(self, |m| m.props.masked_corners = bits);
            run_transition(self, "maskedCorners");
        }

        #[unsafe(method_id(cornerCurve))]
        fn corner_curve(&self) -> Retained<NSString> {
            self.read(|m| m.objs.corner_curve.clone())
        }

        #[unsafe(method(setCornerCurve:))]
        fn set_corner_curve(&self, curve: &NSString) {
            let continuous = curve.to_string() == "continuous";
            // An unknown name is taken as circular (measured).
            let copy = if continuous || curve.to_string() == "circular" {
                objc2_foundation::NSCopying::copy(curve)
            } else {
                NSString::from_str("circular")
            };
            asking_change(self, "cornerCurve", |m| {
                m.objs.corner_curve = copy;
                m.props.continuous_corners = continuous;
            });
        }

        #[unsafe(method(borderWidth))]
        fn border_width(&self) -> CGFloat {
            self.read(|m| m.props.border_width)
        }

        #[unsafe(method(setBorderWidth:))]
        fn set_border_width(&self, w: CGFloat) {
            change(self, Key::BorderWidth, |m| m.props.border_width = w);
        }

        #[unsafe(method(borderColor))]
        fn border_color(&self) -> *mut CGColor {
            let (given, rgba) = self.read(|m| (m.objs.border_color.clone(), m.props.border_color));
            autoreleased(given.or_else(|| rgba.map(|_| black())))
        }

        #[unsafe(method(setBorderColor:))]
        fn set_border_color(&self, color: Option<&CGColor>) {
            let rgba = color.map(objects::cg_rgba);
            let color = color.map(Message::retain);
            change(self, Key::BorderColor, |m| {
                m.objs.border_color = color;
                m.props.border_color = rgba;
            });
        }

        #[unsafe(method(opacity))]
        fn opacity(&self) -> f32 {
            self.read(|m| m.props.opacity) as f32
        }

        #[unsafe(method(setOpacity:))]
        fn set_opacity(&self, opacity: f32) {
            change(self, Key::Opacity, |m| m.props.opacity = opacity as f64);
        }

        #[unsafe(method(allowsGroupOpacity))]
        fn allows_group_opacity(&self) -> bool {
            self.read(|m| m.props.group_opacity)
        }

        #[unsafe(method(setAllowsGroupOpacity:))]
        fn set_allows_group_opacity(&self, flag: bool) {
            asking_change(self, "allowsGroupOpacity", |m| m.props.group_opacity = flag);
        }

        #[unsafe(method_id(compositingFilter))]
        fn compositing_filter(&self) -> Option<Retained<AnyObject>> {
            self.read(|m| m.objs.compositing_filter.clone())
        }

        #[unsafe(method(setCompositingFilter:))]
        fn set_compositing_filter(&self, f: Option<&AnyObject>) {
            let f = f.map(Message::retain);
            change(self, Key::CompositingFilter, |m| m.objs.compositing_filter = f);
        }

        #[unsafe(method_id(filters))]
        fn filters(&self) -> Option<Retained<NSArray>> {
            self.read(|m| m.objs.filters.clone())
        }

        #[unsafe(method(setFilters:))]
        fn set_filters(&self, f: Option<&NSArray>) {
            let f = f.map(objc2_foundation::NSCopying::copy);
            change(self, Key::Filters, |m| m.objs.filters = f);
        }

        #[unsafe(method_id(backgroundFilters))]
        fn background_filters(&self) -> Option<Retained<NSArray>> {
            self.read(|m| m.objs.background_filters.clone())
        }

        #[unsafe(method(setBackgroundFilters:))]
        fn set_background_filters(&self, f: Option<&NSArray>) {
            let f = f.map(objc2_foundation::NSCopying::copy);
            change(self, Key::BackgroundFilters, |m| m.objs.background_filters = f);
        }

        #[unsafe(method(shouldRasterize))]
        fn should_rasterize(&self) -> bool {
            self.read(|m| m.should_rasterize)
        }

        #[unsafe(method(setShouldRasterize:))]
        fn set_should_rasterize(&self, flag: bool) {
            quiet_change(self, |m| m.should_rasterize = flag);
        }

        #[unsafe(method(rasterizationScale))]
        fn rasterization_scale(&self) -> CGFloat {
            self.read(|m| m.rasterization_scale)
        }

        #[unsafe(method(setRasterizationScale:))]
        fn set_rasterization_scale(&self, s: CGFloat) {
            quiet_change(self, |m| m.rasterization_scale = s);
        }

        #[unsafe(method(shadowColor))]
        fn shadow_color(&self) -> *mut CGColor {
            let (given, rgba) = self.read(|m| (m.objs.shadow_color.clone(), m.props.shadow_color));
            autoreleased(given.or_else(|| rgba.map(|_| black())))
        }

        #[unsafe(method(setShadowColor:))]
        fn set_shadow_color(&self, color: Option<&CGColor>) {
            let rgba = color.map(objects::cg_rgba);
            let color = color.map(Message::retain);
            change(self, Key::ShadowColor, |m| {
                m.objs.shadow_color = color;
                m.props.shadow_color = rgba;
            });
        }

        #[unsafe(method(shadowOpacity))]
        fn shadow_opacity(&self) -> f32 {
            self.read(|m| m.props.shadow_opacity) as f32
        }

        #[unsafe(method(setShadowOpacity:))]
        fn set_shadow_opacity(&self, o: f32) {
            change(self, Key::ShadowOpacity, |m| m.props.shadow_opacity = o as f64);
        }

        #[unsafe(method(shadowOffset))]
        fn shadow_offset(&self) -> CGSize {
            let [w, h] = self.read(|m| m.props.shadow_offset);
            CGSize::new(w, h)
        }

        #[unsafe(method(setShadowOffset:))]
        fn set_shadow_offset(&self, s: CGSize) {
            change(self, Key::ShadowOffset, |m| m.props.shadow_offset = [s.width, s.height]);
        }

        #[unsafe(method(shadowRadius))]
        fn shadow_radius(&self) -> CGFloat {
            self.read(|m| m.props.shadow_radius)
        }

        #[unsafe(method(setShadowRadius:))]
        fn set_shadow_radius(&self, r: CGFloat) {
            change(self, Key::ShadowRadius, |m| m.props.shadow_radius = r);
        }

        #[unsafe(method(shadowPath))]
        fn shadow_path(&self) -> *mut CGPath {
            autoreleased(self.read(|m| m.objs.shadow_path.clone()))
        }

        #[unsafe(method(setShadowPath:))]
        fn set_shadow_path(&self, path: Option<&CGPath>) {
            let shape = path.map(objects::path_shape);
            let path = path.map(|p| objects::path(objects::path_shape(p)));
            change(self, Key::ShadowPath, |m| {
                m.objs.shadow_path = path;
                m.props.shadow_path = shape;
            });
        }

        // Layout.

        #[unsafe(method(autoresizingMask))]
        fn autoresizing_mask(&self) -> CAAutoresizingMask {
            CAAutoresizingMask(self.read(|m| m.autoresizing))
        }

        #[unsafe(method(setAutoresizingMask:))]
        fn set_autoresizing_mask(&self, mask: CAAutoresizingMask) {
            self.write(|m| m.autoresizing = mask.0);
        }

        #[unsafe(method_id(layoutManager))]
        fn layout_manager(&self) -> Option<Retained<AnyObject>> {
            self.read(|m| m.layout_manager.clone())
        }

        #[unsafe(method(setLayoutManager:))]
        fn set_layout_manager(&self, manager: Option<&AnyObject>) {
            let manager = manager.map(Message::retain);
            self.write(|m| m.layout_manager = manager);
        }

        #[unsafe(method(preferredFrameSize))]
        fn preferred_frame_size(&self) -> CGSize {
            if let Some(manager) = self.read(|m| m.layout_manager.clone()) {
                // SAFETY: -respondsToSelector: takes a selector.
                let responds: bool = unsafe { msg_send![&*manager, respondsToSelector: sel!(preferredSizeOfLayer:)] };
                if responds {
                    // SAFETY: the layout manager's method returns a size.
                    return unsafe { msg_send![&*manager, preferredSizeOfLayer: as_layer(self)] };
                }
            }
            let [_, _, w, h] = self.read(|m| m.props.bounds);
            CGSize::new(w, h)
        }

        #[unsafe(method(setNeedsLayout))]
        fn set_needs_layout(&self) {
            self.write(|m| m.needs_layout = true);
            transaction::touch();
        }

        #[unsafe(method(needsLayout))]
        fn needs_layout(&self) -> bool {
            self.read(|m| m.needs_layout)
        }

        #[unsafe(method(layoutIfNeeded))]
        fn layout_if_needed(&self) {
            layout_if_needed(self);
        }

        #[unsafe(method(layoutSublayers))]
        fn layout_sublayers(&self) {
            if let Some(manager) = self.read(|m| m.layout_manager.clone()) {
                // SAFETY: -respondsToSelector: takes a selector.
                let responds: bool = unsafe { msg_send![&*manager, respondsToSelector: sel!(layoutSublayersOfLayer:)] };
                if responds {
                    // SAFETY: the layout manager lays the layer out.
                    let _: () = unsafe { msg_send![&*manager, layoutSublayersOfLayer: as_layer(self)] };
                    return;
                }
            }
            if let Some(delegate) = self.delegate_object() {
                // SAFETY: as above.
                let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(layoutSublayersOfLayer:)] };
                if responds {
                    // SAFETY: the delegate lays the layer out.
                    let _: () = unsafe { msg_send![&*delegate, layoutSublayersOfLayer: as_layer(self)] };
                }
            }
        }

        #[unsafe(method(resizeSublayersWithOldSize:))]
        fn resize_sublayers_with_old_size(&self, old: CGSize) {
            for sub in self.read(|m| m.sublayers.clone()) {
                // SAFETY: the method takes a size.
                let _: () = unsafe { msg_send![&*sub, resizeWithOldSuperlayerSize: old] };
            }
        }

        #[unsafe(method(resizeWithOldSuperlayerSize:))]
        fn resize_with_old_superlayer_size(&self, old: CGSize) {
            autoresize(self, old);
        }

        // Actions and animations.

        #[unsafe(method_id(actionForKey:))]
        fn action_for_key(&self, key: &NSString) -> Option<Retained<ProtocolObject<dyn CAAction>>> {
            action_for_key(self, key).map(|a| {
                // SAFETY: an action conforms to CAAction by answering its
                // one method.
                unsafe { Retained::cast_unchecked(a) }
            })
        }

        #[unsafe(method_id(actions))]
        fn actions(&self) -> Option<Retained<NSDictionary>> {
            self.read(|m| m.actions.clone())
        }

        #[unsafe(method(setActions:))]
        fn set_actions(&self, actions: Option<&NSDictionary>) {
            let actions = actions.map(objc2_foundation::NSCopying::copy);
            self.write(|m| m.actions = actions);
        }

        #[unsafe(method(addAnimation:forKey:))]
        fn add_animation_for_key(&self, anim: &CAAnimation, key: Option<&NSString>) {
            add_animation(self, anim, key.map(|k| k.to_string()));
        }

        #[unsafe(method(removeAllAnimations))]
        fn remove_all_animations(&self) {
            let gone: Vec<Added> = self.write(|m| std::mem::take(&mut m.anims));
            transaction::animations_removed(self, gone);
        }

        #[unsafe(method(removeAnimationForKey:))]
        fn remove_animation_for_key(&self, key: &NSString) {
            let key = key.to_string();
            let gone: Vec<Added> = self.write(|m| {
                let (gone, kept) = std::mem::take(&mut m.anims).into_iter().partition(|a| a.key.as_deref() == Some(&key));
                m.anims = kept;
                gone
            });
            transaction::animations_removed(self, gone);
        }

        /// A presentation layer answers its model's (measured).
        #[unsafe(method_id(animationKeys))]
        fn animation_keys(&self) -> Option<Retained<NSArray<NSString>>> {
            let model = model_of(self);
            let keys: Vec<Retained<NSString>> = imp(&model)
                .read(|m| m.anims.iter().filter_map(|a| a.key.as_deref().map(NSString::from_str)).collect());
            (!keys.is_empty()).then(|| NSArray::from_retained_slice(&keys))
        }

        #[unsafe(method_id(animationForKey:))]
        fn animation_for_key(&self, key: &NSString) -> Option<Retained<CAAnimation>> {
            let key = key.to_string();
            let model = model_of(self);
            imp(&model)
                .read(|m| m.anims.iter().rev().find(|a| a.key.as_deref() == Some(&key)).map(|a| a.object.clone()))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            self.read(|m| m.name.clone())
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            let name = name.map(objc2_foundation::NSCopying::copy);
            self.write(|m| m.name = name);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.delegate_object()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            self.write(|m| {
                m.delegate = super::objects::weak_of(delegate);
                m.has_delegate = delegate.is_some();
            });
        }

        #[unsafe(method_id(style))]
        fn style(&self) -> Option<Retained<NSDictionary>> {
            self.read(|m| m.style.clone())
        }

        #[unsafe(method(setStyle:))]
        fn set_style(&self, style: Option<&NSDictionary>) {
            let style = style.map(objc2_foundation::NSCopying::copy);
            asking_change(self, "style", |m| m.style = style);
            apply_style(self);
        }

        // CAMediaTiming.

        #[unsafe(method(beginTime))]
        fn begin_time(&self) -> CFTimeInterval {
            self.read(|m| m.timing.begin)
        }

        #[unsafe(method(setBeginTime:))]
        fn set_begin_time(&self, t: CFTimeInterval) {
            quiet_change(self, |m| {
                m.timing.begin = t;
                m.props.time.begin = t;
            });
        }

        #[unsafe(method(duration))]
        fn duration(&self) -> CFTimeInterval {
            self.read(|m| m.timing.duration)
        }

        #[unsafe(method(setDuration:))]
        fn set_duration(&self, d: CFTimeInterval) {
            quiet_change(self, |m| m.timing.duration = d);
        }

        #[unsafe(method(speed))]
        fn speed(&self) -> f32 {
            self.read(|m| m.timing.speed) as f32
        }

        #[unsafe(method(setSpeed:))]
        fn set_speed(&self, s: f32) {
            asking_change(self, "speed", |m| {
                m.timing.speed = s as f64;
                m.props.time.speed = s as f64;
            });
            transaction::timing_changed(self);
        }

        #[unsafe(method(timeOffset))]
        fn time_offset(&self) -> CFTimeInterval {
            self.read(|m| m.timing.offset)
        }

        #[unsafe(method(setTimeOffset:))]
        fn set_time_offset(&self, t: CFTimeInterval) {
            quiet_change(self, |m| {
                m.timing.offset = t;
                m.props.time.offset = t;
            });
            transaction::timing_changed(self);
        }

        #[unsafe(method(repeatCount))]
        fn repeat_count(&self) -> f32 {
            self.read(|m| m.timing.repeat_count) as f32
        }

        #[unsafe(method(setRepeatCount:))]
        fn set_repeat_count(&self, c: f32) {
            quiet_change(self, |m| m.timing.repeat_count = c as f64);
        }

        #[unsafe(method(repeatDuration))]
        fn repeat_duration(&self) -> CFTimeInterval {
            self.read(|m| m.timing.repeat_duration)
        }

        #[unsafe(method(setRepeatDuration:))]
        fn set_repeat_duration(&self, d: CFTimeInterval) {
            quiet_change(self, |m| m.timing.repeat_duration = d);
        }

        #[unsafe(method(autoreverses))]
        fn autoreverses(&self) -> bool {
            self.read(|m| m.timing.autoreverses)
        }

        #[unsafe(method(setAutoreverses:))]
        fn set_autoreverses(&self, flag: bool) {
            quiet_change(self, |m| m.timing.autoreverses = flag);
        }

        #[unsafe(method_id(fillMode))]
        fn fill_mode(&self) -> Retained<NSString> {
            self.read(|m| m.objs.fill_mode.clone())
        }

        #[unsafe(method(setFillMode:))]
        fn set_fill_mode(&self, mode: &NSString) {
            let fill = Fill::named(&mode.to_string());
            let copy = objc2_foundation::NSCopying::copy(mode);
            quiet_change(self, |m| {
                m.timing.fill = fill;
                m.objs.fill_mode = copy;
            });
        }

        // Key-value coding.

        #[unsafe(method_id(valueForKey:))]
        fn value_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            super::kvc::value_for_key(self, &key.to_string())
        }

        #[unsafe(method(setValue:forKey:))]
        fn set_value_for_key(&self, value: Option<&AnyObject>, key: &NSString) {
            super::kvc::set_value_for_key(self, value, key);
        }

        #[unsafe(method_id(valueForKeyPath:))]
        fn value_for_key_path(&self, path: &NSString) -> Option<Retained<AnyObject>> {
            super::kvc::value_for_key_path(self, &path.to_string())
        }

        #[unsafe(method(setValue:forKeyPath:))]
        fn set_value_for_key_path(&self, value: Option<&AnyObject>, path: &NSString) {
            super::kvc::set_value_for_key_path(self, value, &path.to_string());
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let class = as_layer(self).class().name().to_str().unwrap_or("CALayer").to_owned();
            NSString::from_str(&format!("<{class}: {:p}>", self))
        }
    }

    unsafe impl NSObjectProtocol for CALayerImpl {}
);

/// A layer's superlayer (a mask's is the layer it masks, measured); a
/// presentation layer's is its model's superlayer's presentation layer.
fn superlayer_of(layer: &CALayerImpl) -> Option<Retained<CALayer>> {
    if layer.ivars().is_presentation.get() {
        let model = layer.ivars().model_layer.borrow().load()?;
        // SAFETY: a superlayer clears its sublayers' links as it goes.
        return imp(&model).read(|m| m.superlayer.or(m.mask_of)).and_then(|s| presentation(imp(unsafe { s.as_ref() })));
    }
    // SAFETY: as above.
    layer.read(|m| m.superlayer.or(m.mask_of)).map(|s| unsafe { s.as_ref() }.retain())
}

/// The model layer of a presentation layer (itself for a model layer, or
/// for a presentation layer whose model is gone).
fn model_of(layer: &CALayerImpl) -> Retained<CALayer> {
    let model = if layer.ivars().is_presentation.get() { layer.ivars().model_layer.borrow().load() } else { None };
    model.unwrap_or_else(|| as_layer(layer).retain())
}

/// A layer's sublayers (none for an empty list); a presentation layer's
/// are its model's sublayers' presentation layers.
fn sublayers_of(layer: &CALayerImpl) -> Option<Retained<NSArray<CALayer>>> {
    if layer.ivars().is_presentation.get() {
        let model = layer.ivars().model_layer.borrow().load()?;
        let subs = imp(&model).read(|m| m.sublayers.clone());
        if subs.is_empty() {
            return None;
        }
        let presented: Vec<Retained<CALayer>> =
            subs.iter().map(|s| presentation(imp(s)).unwrap_or_else(|| s.clone())).collect();
        return Some(NSArray::from_retained_slice(&presented));
    }
    let subs = layer.read(|m| m.sublayers.clone());
    (!subs.is_empty()).then(|| NSArray::from_retained_slice(&subs))
}

/// Expansion of a continuous corner beyond its radius (measured:
/// `+cornerCurveExpansionFactor:`).
pub(crate) const CONTINUOUS_EXPANSION: f64 = 1.528665;

fn ivars() -> LayerIvars {
    LayerIvars {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        model: RefCell::new(Model::new()),
        live: Cell::new(false),
        dirty: Cell::new(false),
        sent: Cell::new(false),
        view: RefCell::new(Weak::default()),
        is_presentation: Cell::new(false),
        model_layer: RefCell::new(Weak::default()),
        presentation: RefCell::new(None),
    }
}

/// Give `CALayer` its `+layer`, which makes an instance of the class it
/// was sent to (a `define_class!` class method can't see its receiver).
pub(crate) fn install_class_methods(class: &AnyClass) {
    unsafe extern "C-unwind" fn layer(cls: &AnyClass, _: Sel) -> *mut CALayer {
        // SAFETY: +new returns a new instance of the class.
        let l: Retained<CALayer> = unsafe { msg_send![cls, new] };
        Retained::autorelease_return(l)
    }
    // SAFETY: the implementation takes the receiver and selector and
    // returns an object, as the encoding says.
    unsafe {
        super::add_class_method(
            class,
            sel!(layer),
            std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyClass, Sel) -> *mut CALayer, objc2::runtime::Imp>(
                layer,
            ),
            c"@@:",
        );
    }
}

/// A new layer's defaults from its class, when a subclass overrides
/// `+defaultValueForKey:`, and its style.
fn class_defaults(this: &CALayerImpl) {
    let class = as_layer(this).class();
    if std::ptr::eq(class, CALayer::class()) || own_defaults(class) {
        return;
    }
    for (name, key) in Key::ALL {
        let nskey = NSString::from_str(name);
        // SAFETY: the class method takes a key and returns an object.
        let v: Option<Retained<AnyObject>> = unsafe { msg_send![class, defaultValueForKey: &*nskey] };
        let Some(v) = v else { continue };
        if default_value(name).is_some_and(|d| {
            // SAFETY: -isEqual: takes an object.
            unsafe { msg_send![&*d, isEqual: &*v] }
        }) {
            continue;
        }
        apply_object(this, key, Some(&v), false);
    }
}

pub(crate) fn imp(layer: &CALayer) -> &CALayerImpl {
    // SAFETY: every CALayer is a CALayerImpl (Sidestep's class, or a
    // subclass of it).
    unsafe { &*(layer as *const CALayer).cast::<CALayerImpl>() }
}

pub(crate) fn as_layer(this: &CALayerImpl) -> &CALayer {
    // SAFETY: CALayerImpl is what CALayer names.
    unsafe { &*(this as *const CALayerImpl).cast::<CALayer>() }
}

impl CALayerImpl {
    pub(crate) fn id(&self) -> LayerId {
        self.ivars().id
    }

    pub(crate) fn read<R>(&self, f: impl FnOnce(&Model) -> R) -> R {
        f(&self.ivars().model.borrow())
    }

    pub(crate) fn write<R>(&self, f: impl FnOnce(&mut Model) -> R) -> R {
        f(&mut self.ivars().model.borrow_mut())
    }

    pub(crate) fn is_live(&self) -> bool {
        self.ivars().live.get()
    }

    pub(crate) fn delegate_object(&self) -> Option<Retained<AnyObject>> {
        self.read(|m| if m.has_delegate { m.delegate.load() } else { None })
    }

    fn index_of(&self, layer: &CALayer) -> Option<usize> {
        self.read(|m| m.sublayers.iter().position(|s| std::ptr::eq(&**s, layer)))
    }

    /// The view this layer backs, while it lives.
    pub(crate) fn view(&self) -> Option<Retained<objc2_app_kit::NSView>> {
        self.ivars().view.borrow().load()
    }

    /// Whether a view backs this layer.
    pub(crate) fn has_view(&self) -> bool {
        self.view().is_some()
    }

    pub(crate) fn set_view(&self, view: Option<&objc2_app_kit::NSView>) {
        *self.ivars().view.borrow_mut() = super::objects::weak_of(view);
    }

    pub(crate) fn is_presentation(&self) -> bool {
        self.ivars().is_presentation.get()
    }
}

fn rect(r: [f64; 4]) -> CGRect {
    CGRect::new(CGPoint::new(r[0], r[1]), CGSize::new(r[2], r[3]))
}

fn point(p: [f64; 2]) -> CGPoint {
    CGPoint::new(p[0], p[1])
}

/// Hand out an object its owner keeps, as a getter returning a CF type
/// does (the layer keeps it alive).
pub(crate) fn autoreleased<T: Message>(object: Option<Retained<T>>) -> *mut T {
    match object {
        Some(o) => Retained::autorelease_ptr(o),
        None => std::ptr::null_mut(),
    }
}

/// Opaque black, the default border and shadow color.
fn black() -> Retained<CGColor> {
    let imp = crate::coregraphics::color::srgb_color([0.0, 0.0, 0.0, 1.0]);
    // SAFETY: CGColorImpl is what CGColor names.
    unsafe { Retained::cast_unchecked(imp) }
}

/// `+defaultValueForKey:`'s answers (measured): the defaults of the keys
/// macOS gives one, nil for the others.
pub(crate) fn default_value(key: &str) -> Option<Retained<AnyObject>> {
    let d = Props::default();
    let s = |v: &str| objects::any(NSString::from_str(v));
    Some(match key {
        "opacity" | "speed" => objects::number(1.0),
        "duration" => objects::number(f64::INFINITY),
        "anchorPoint" => objects::to_object(&Value::Point(d.anchor))?,
        "shadowOffset" => objects::to_object(&Value::Size(d.shadow_offset))?,
        "shadowRadius" => objects::number(3.0),
        "shadowColor" | "borderColor" => objects::any(black()),
        "contentsScale" | "rasterizationScale" => objects::number(1.0),
        "contentsHeadroom" => objects::number(0.0),
        "hidden" | "masksToBounds" | "needsDisplayOnBoundsChange" | "geometryFlipped" | "opaque" => {
            objects::boolean(false)
        }
        "contentsRect" | "contentsCenter" => objects::to_object(&Value::Rect([0.0, 0.0, 1.0, 1.0]))?,
        "doubleSided" | "allowsEdgeAntialiasing" | "allowsGroupOpacity" => objects::boolean(true),
        "cornerCurve" => s("circular"),
        "contentsGravity" => s("resize"),
        "contentsFormat" => s("RGBA8"),
        "fillMode" => s("removed"),
        "minificationFilter" | "magnificationFilter" => s("linear"),
        "edgeAntialiasingMask" | "maskedCorners" => objects::number(15.0),
        _ => return None,
    })
}

/// Whether `class`'s `+defaultValueForKey:` is one of Sidestep's own
/// (CALayer's, or its shape and gradient layers'), whose values every new
/// layer already has.
fn own_defaults(class: &AnyClass) -> bool {
    let sel = sel!(defaultValueForKey:);
    let imp_of = |c: &AnyClass| c.metaclass().instance_method(sel).map(|m| m.implementation() as usize);
    let theirs = imp_of(class);
    [CALayer::class(), objc2_quartz_core::CAShapeLayer::class(), objc2_quartz_core::CAGradientLayer::class()]
        .iter()
        .any(|c| imp_of(c) == theirs)
}

// Changes.

/// Change `key` on `layer`: ask for its action first (when the layer is
/// live and actions aren't disabled), change it, mark it, run the action.
pub(crate) fn change(layer: &CALayerImpl, key: Key, apply: impl FnOnce(&mut Model)) {
    read_only_check(layer);
    let action = if layer.is_live() && !transaction::disable_actions() {
        let name = NSString::from_str(key.name());
        // SAFETY: -actionForKey: takes a key and returns an action or nil.
        let a: Option<Retained<AnyObject>> = unsafe { msg_send![layer, actionForKey: &*name] };
        a.map(|a| (name, a))
    } else {
        None
    };
    let bit = key_bit(key);
    layer.write(|m| {
        apply(m);
        m.explicit |= bit;
    });
    transaction::mark_dirty(layer);
    // `+needsDisplayForKey:` (subclasses redraw for their own keys).
    let class = as_layer(layer).class();
    if !std::ptr::eq(class, CALayer::class()) {
        let name = NSString::from_str(key.name());
        // SAFETY: the class method takes a key and returns BOOL.
        let redraw: bool = unsafe { msg_send![class, needsDisplayForKey: &*name] };
        if redraw {
            set_needs_display(layer);
        }
    }
    if let Some((name, action)) = action {
        run_action(layer, &action, &name);
    }
}

/// Change a key that has no default action, asking for its action as
/// macOS does (a delegate's or the actions dictionary's runs).
fn asking_change(layer: &CALayerImpl, key: &str, apply: impl FnOnce(&mut Model)) {
    read_only_check(layer);
    let name = NSString::from_str(key);
    let action = if layer.is_live() && !transaction::disable_actions() {
        // SAFETY: -actionForKey: takes a key and returns an action or nil.
        let a: Option<Retained<AnyObject>> = unsafe { msg_send![layer, actionForKey: &*name] };
        a
    } else {
        None
    };
    quiet_change(layer, apply);
    if let Some(action) = action {
        run_action(layer, &action, &name);
    }
}

/// Change something that has no action.
pub(crate) fn quiet_change(layer: &CALayerImpl, apply: impl FnOnce(&mut Model)) {
    read_only_check(layer);
    layer.write(apply);
    transaction::mark_dirty(layer);
}

/// A presentation layer can't be changed (raises on macOS).
fn read_only_check(layer: &CALayerImpl) {
    if layer.is_presentation() {
        panic!("attempting to modify read-only layer {:p}", as_layer(layer));
    }
}

fn key_bit(key: Key) -> u64 {
    Key::ALL.iter().position(|(_, k)| *k == key).map_or(0, |i| 1 << i)
}

/// Run a key's action for a change made without going through `change`
/// (a change of what's drawn, or of the tree).
pub(crate) fn run_transition(layer: &CALayerImpl, key: &str) {
    if !layer.is_live() || transaction::disable_actions() {
        return;
    }
    let name = NSString::from_str(key);
    // SAFETY: -actionForKey: takes a key and returns an action or nil.
    let a: Option<Retained<AnyObject>> = unsafe { msg_send![layer, actionForKey: &*name] };
    if let Some(a) = a {
        run_action(layer, &a, &name);
    }
}

fn run_action(layer: &CALayerImpl, action: &AnyObject, key: &NSString) {
    // SAFETY: -respondsToSelector: takes a selector.
    let responds: bool = unsafe { msg_send![action, respondsToSelector: sel!(runActionForKey:object:arguments:)] };
    if responds {
        let arguments: Option<&NSDictionary> = None;
        // SAFETY: CAAction's method.
        let _: () = unsafe { msg_send![action, runActionForKey: key, object: as_layer(layer), arguments: arguments] };
    }
}

/// `actionForKey:`: the delegate's, then the layer's `actions`, then its
/// style's, then its class's, then the default for the key (an implicit
/// animation, or a fade transition for the keys macOS fades), where
/// `NSNull` found anywhere means none.
fn action_for_key(layer: &CALayerImpl, key: &NSString) -> Option<Retained<AnyObject>> {
    let null = |o: &AnyObject| -> bool {
        // SAFETY: -isKindOfClass: takes a class.
        unsafe { msg_send![o, isKindOfClass: NSNull::class()] }
    };
    if let Some(delegate) = layer.delegate_object() {
        // SAFETY: -respondsToSelector: takes a selector.
        let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(actionForLayer:forKey:)] };
        if responds {
            // SAFETY: the delegate's method returns an action, NSNull or nil.
            let a: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*delegate, actionForLayer: as_layer(layer), forKey: key] };
            if let Some(a) = a {
                return (!null(&a)).then_some(a);
            }
        }
    }
    let from_dict = |d: &NSDictionary| -> Option<Retained<AnyObject>> {
        // SAFETY: -objectForKey: takes a key.
        unsafe { msg_send![d, objectForKey: key] }
    };
    if let Some(actions) = layer.read(|m| m.actions.clone())
        && let Some(a) = from_dict(&actions)
    {
        return (!null(&a)).then_some(a);
    }
    let mut style = layer.read(|m| m.style.clone());
    while let Some(s) = style {
        let k = NSString::from_str("actions");
        // SAFETY: -objectForKey: takes a key.
        let actions: Option<Retained<AnyObject>> = unsafe { msg_send![&*s, objectForKey: &*k] };
        if let Some(actions) = actions
            && let Some(d) = actions.downcast_ref::<NSDictionary>()
            && let Some(a) = from_dict(d)
        {
            return (!null(&a)).then_some(a);
        }
        let k = NSString::from_str("style");
        // SAFETY: as above.
        let next: Option<Retained<AnyObject>> = unsafe { msg_send![&*s, objectForKey: &*k] };
        style = next.and_then(|n| n.downcast::<NSDictionary>().ok());
    }
    let class = as_layer(layer).class();
    // SAFETY: the class method takes a key and returns an action or nil.
    let a: Option<Retained<AnyObject>> = unsafe { msg_send![class, defaultActionForKey: key] };
    if let Some(a) = a {
        return (!null(&a)).then_some(a);
    }
    if !layer.is_live() {
        return None;
    }
    default_action(layer, &key.to_string())
}

/// The keys whose change fades (a default `CATransition`), measured.
const FADED: [&str; 9] = [
    "hidden",
    "geometryFlipped",
    "masksToBounds",
    "mask",
    "sublayers",
    "filters",
    "backgroundFilters",
    "compositingFilter",
    "maskedCorners",
];

/// The keys with a default implicit animation, measured.
const IMPLICIT: [&str; 20] = [
    "bounds",
    "position",
    "zPosition",
    "anchorPoint",
    "anchorPointZ",
    "transform",
    "sublayerTransform",
    "contents",
    "contentsRect",
    "contentsCenter",
    "contentsScale",
    "cornerRadius",
    "borderWidth",
    "borderColor",
    "opacity",
    "shadowColor",
    "shadowOpacity",
    "shadowOffset",
    "shadowRadius",
    "backgroundColor",
];

fn default_action(layer: &CALayerImpl, key: &str) -> Option<Retained<AnyObject>> {
    if FADED.contains(&key) {
        return Some(objects::any(super::animation::implicit_transition()));
    }
    let shape_keys =
        ["path", "fillColor", "strokeColor", "strokeStart", "strokeEnd", "lineWidth", "miterLimit", "lineDashPhase"];
    let gradient_keys = ["colors", "locations", "startPoint", "endPoint"];
    let kind_ok = match layer.read(|m| m.props.kind.clone()) {
        Kind::Shape(_) => shape_keys.contains(&key),
        Kind::Gradient(_) => gradient_keys.contains(&key),
        Kind::Plain => false,
    };
    let transform_part = key.starts_with("transform.") || key.starts_with("sublayerTransform.");
    if !IMPLICIT.contains(&key) && !kind_ok && !transform_part {
        return None;
    }
    // The value before the change, as the presentation shows it.
    let from = if key == "contents" {
        layer.read(|m| m.objs.contents.clone())
    } else {
        let path = KeyPath::parse(key)?;
        let value = current_value(layer, path)?;
        match &value {
            // Nothing to animate from.
            Value::Color(None) | Value::Path(None) => return None,
            _ => {}
        }
        Some(object_for(layer, path, &value)?)
    };
    Some(objects::any(super::animation::implicit_animation(key, from.as_deref())))
}

/// The value a key path shows now: the presentation's while animations
/// change it, else the model's.
fn current_value(layer: &CALayerImpl, path: KeyPath) -> Option<Value> {
    let animated = layer.read(|m| m.anims.iter().any(|a| a.spec.touches(path.key)));
    if animated && let Some(p) = presented_props(layer) {
        return p.get_path(path);
    }
    layer.read(|m| m.props.get_path(path))
}

/// The object for a value at a key path: what the layer was given where
/// the value is the model's, else a new one.
fn object_for(layer: &CALayerImpl, path: KeyPath, value: &Value) -> Option<Retained<AnyObject>> {
    if path.part == Part::Whole {
        let given = layer.read(|m| {
            let same = m.props.get(path.key).as_ref() == Some(value);
            if !same {
                return None;
            }
            match path.key {
                Key::BackgroundColor => m.objs.background_color.clone().map(objects::any),
                Key::BorderColor => m.objs.border_color.clone().map(objects::any),
                Key::ShadowColor => m.objs.shadow_color.clone().map(objects::any),
                Key::ShadowPath => m.objs.shadow_path.clone().map(objects::any),
                _ => None,
            }
        });
        if given.is_some() {
            return given;
        }
    }
    objects::to_object(value)
}

/// Set a key from an object (key-value coding, a style, a class's
/// defaults), converting it to the key's kind; `explicit` marks it set.
pub(crate) fn apply_object(layer: &CALayerImpl, key: Key, value: Option<&AnyObject>, explicit: bool) {
    let obj = || value.map(Message::retain);
    // Keys with setters of their own go through them, so subclasses see
    // the change.
    let name = key.name();
    let Some(sel) = super::kvc::setter(name) else { return };
    let like = layer.read(|m| m.props.get(key));
    match (key, like) {
        (Key::Contents, _) => {
            // SAFETY: setContents: takes an object.
            let _: () = unsafe { msg_send![layer, setContents: value] };
        }
        (Key::Mask, _) => {
            let mask = value.and_then(|v| v.downcast_ref::<CALayer>());
            // SAFETY: setMask: takes a layer.
            let _: () = unsafe { msg_send![layer, setMask: mask] };
        }
        (Key::Filters | Key::BackgroundFilters, _) => {
            let a = value.and_then(|v| v.downcast_ref::<NSArray>());
            if key == Key::Filters {
                // SAFETY: setFilters: takes an array.
                let _: () = unsafe { msg_send![layer, setFilters: a] };
            } else {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![layer, setBackgroundFilters: a] };
            }
        }
        (Key::CompositingFilter, _) => {
            // SAFETY: setCompositingFilter: takes an object.
            let _: () = unsafe { msg_send![layer, setCompositingFilter: value] };
        }
        (Key::Sublayers, _) => {
            // SAFETY: an array of layers, as setSublayers: takes.
            let a = value
                .and_then(|v| v.downcast_ref::<NSArray>())
                .map(|a| unsafe { &*(a as *const NSArray).cast::<NSArray<CALayer>>() });
            // SAFETY: setSublayers: takes an array of layers.
            let _: () = unsafe { msg_send![layer, setSublayers: a] };
        }
        (_, Some(like)) => {
            let Some(v) = objects::to_value(value, &like) else { return };
            super::kvc::send_setter(layer, sel, key, &v, obj());
        }
        (_, None) => {}
    }
    if !explicit {
        let bit = key_bit(key);
        layer.write(|m| m.explicit &= !bit);
    }
}

/// Apply a style's values to the keys not set explicitly.
fn apply_style(layer: &CALayerImpl) {
    let mut style = layer.read(|m| m.style.clone());
    let mut seen = 0u64;
    while let Some(s) = style {
        for (name, key) in Key::ALL {
            let bit = key_bit(key);
            if seen & bit != 0 || layer.read(|m| m.explicit & bit != 0) {
                continue;
            }
            let k = NSString::from_str(name);
            // SAFETY: -objectForKey: takes a key.
            let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*s, objectForKey: &*k] };
            if let Some(v) = v {
                seen |= bit;
                let forced = transaction::force_actions_off();
                apply_object(layer, key, Some(&v), false);
                transaction::restore_forced(forced);
            }
        }
        let k = NSString::from_str("style");
        // SAFETY: as above.
        let next: Option<Retained<AnyObject>> = unsafe { msg_send![&*s, objectForKey: &*k] };
        style = next.and_then(|n| n.downcast::<NSDictionary>().ok());
    }
}

/// The bounds changed: layout, and display where the layer asks.
fn bounds_changed(layer: &CALayerImpl, old: [f64; 4], new: [f64; 4]) {
    if old[2..] != new[2..] {
        let redraw = layer.write(|m| {
            m.needs_layout = true;
            m.needs_display_on_bounds_change
        });
        if redraw {
            set_needs_display(layer);
        }
        // Sublayers that resize with this one.
        let subs = layer.read(|m| m.sublayers.clone());
        if subs.iter().any(|s| imp(s).read(|m| m.autoresizing != 0)) {
            let size = CGSize::new(old[2], old[3]);
            // SAFETY: the method takes a size.
            let _: () = unsafe { msg_send![layer, resizeSublayersWithOldSize: size] };
        }
    }
}

/// `resizeWithOldSuperlayerSize:`: follow the superlayer by the
/// autoresizing mask, sharing the change among the flexible parts.
fn autoresize(layer: &CALayerImpl, old: CGSize) {
    let mask = layer.read(|m| m.autoresizing);
    if mask == 0 {
        return;
    }
    let Some(sup) = layer.read(|m| m.superlayer) else { return };
    // SAFETY: a superlayer clears the link as it goes.
    let new = imp(unsafe { sup.as_ref() }).read(|m| m.props.bounds);
    let frame = layer.read(|m| m.props.frame());
    let share = |min: bool, size: bool, max: bool, pos: f64, len: f64, old_len: f64, new_len: f64| {
        let delta = new_len - old_len;
        let parts = [min, size, max].iter().filter(|b| **b).count() as f64;
        if parts == 0.0 {
            return (pos, len);
        }
        let each = delta / parts;
        (pos + if min { each } else { 0.0 }, len + if size { each } else { 0.0 })
    };
    let (x, w) = share(mask & 1 != 0, mask & 2 != 0, mask & 4 != 0, frame[0], frame[2], old.width, new[2]);
    let (y, h) = share(mask & 8 != 0, mask & 16 != 0, mask & 32 != 0, frame[1], frame[3], old.height, new[3]);
    let r = rect([x, y, w, h]);
    // SAFETY: setFrame: takes a rectangle.
    let _: () = unsafe { msg_send![layer, setFrame: r] };
}

fn layout_if_needed(layer: &CALayerImpl) {
    // Up to the topmost ancestor needing layout, then down.
    let needs = layer.write(|m| std::mem::replace(&mut m.needs_layout, false));
    if needs {
        // SAFETY: -layoutSublayers takes nothing.
        let _: () = unsafe { msg_send![layer, layoutSublayers] };
        // Then its `onLayout` action (measured).
        run_order_action(layer, "onLayout");
    }
    for sub in layer.read(|m| m.sublayers.clone()) {
        layout_if_needed(imp(&sub));
    }
}

pub(crate) fn set_needs_display(layer: &CALayerImpl) {
    layer.write(|m| m.needs_display = true);
    transaction::mark_dirty(layer);
    if let Some(view) = layer.view() {
        crate::quartzcore::backing::layer_needs_display(&view);
    }
}

/// `display`: a view's layer displays through its view (`updateLayer`, or
/// its drawing redrawn into its canvas), as AppKit's backing layers do;
/// another layer through its delegate's `displayLayer:`, or
/// `drawInContext:` recorded into its backing store.
fn display(layer: &CALayerImpl) {
    layer.write(|m| m.needs_display = false);
    if let Some(view) = layer.view() {
        super::backing::display_view_layer(&view);
        return;
    }
    if let Some(delegate) = layer.delegate_object() {
        // SAFETY: -respondsToSelector: takes a selector.
        let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(displayLayer:)] };
        if responds {
            // SAFETY: the delegate's method takes the layer.
            let _: () = unsafe { msg_send![&*delegate, displayLayer: as_layer(layer)] };
            return;
        }
        // SAFETY: as above.
        let will: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(layerWillDraw:)] };
        if will {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![&*delegate, layerWillDraw: as_layer(layer)] };
        }
    }
    let drawn = super::render::record_draw_in_context(layer);
    layer.write(|m| {
        m.drawn = drawn;
        m.objs.contents = None;
        m.content_version += 1;
    });
    transaction::mark_dirty(layer);
}

/// The object `contents` returns for a layer's backing store.
fn backing_store(layer: &CALayerImpl) -> Retained<AnyObject> {
    super::render::backing_store_object(layer)
}

// The tree.

/// Raise (panic) if putting `child` under `parent` would make a cycle:
/// `child` is `parent` or one of its ancestors (as macOS raises).
fn check_cycle(parent: &CALayerImpl, child: &CALayer) {
    let mut cur = Some(as_layer(parent).retain());
    while let Some(l) = cur {
        if std::ptr::eq(&*l, child) {
            panic!("layer {child:p} is a part of cycle in its layer tree");
        }
        // SAFETY: a superlayer clears the link as it goes.
        cur = imp(&l).read(|m| m.superlayer.or(m.mask_of)).map(|s| unsafe { s.as_ref() }.retain());
    }
}

/// Ask a layer that joined or left a tree for its `onOrderIn` or
/// `onOrderOut` action and run it (as macOS asks, live or not).
fn run_order_action(layer: &CALayerImpl, key: &str) {
    if transaction::disable_actions() {
        return;
    }
    let name = NSString::from_str(key);
    // SAFETY: -actionForKey: takes a key and returns an action or nil.
    let a: Option<Retained<AnyObject>> = unsafe { msg_send![layer, actionForKey: &*name] };
    if let Some(a) = a {
        run_action(layer, &a, &name);
    }
}

/// Put `layer` into `parent`'s sublayers at `at` (clamped), taking it out
/// of wherever it was, with the actions macOS asks for: the superlayers'
/// `sublayers` transitions, and the layer's `onOrderIn` when it joins a
/// new superlayer (a reordering asks only the superlayer, measured).
fn insert(parent: &CALayerImpl, layer: &CALayer, at: usize) {
    check_cycle(parent, layer);
    let child = imp(layer);
    let keep = layer.retain();
    let old = link(parent, child, at);
    let moved = old.as_ref().is_none_or(|o| !std::ptr::eq(&**o, parent));
    if let Some(old) = old.filter(|_| moved) {
        run_transition(&old, "sublayers");
    }
    run_transition(parent, "sublayers");
    if moved {
        run_order_action(child, "onOrderIn");
    }
    drop(keep);
}

/// Link `child` into `parent`'s sublayers at `at` with no actions, taking
/// it out of its superlayer (or the layer it masks) first; returns the
/// superlayer it had.
fn link(parent: &CALayerImpl, child: &CALayerImpl, at: usize) -> Option<Retained<CALayerImpl>> {
    let keep = as_layer(child).retain();
    let old = unlink(child);
    parent.write(|m| {
        let at = at.min(m.sublayers.len());
        m.sublayers.insert(at, keep);
        m.needs_layout = true;
    });
    child.write(|m| m.superlayer = Some(NonNull::from(as_layer(parent))));
    transaction::structure_changed(parent);
    transaction::attached(child);
    old
}

/// Take `layer` out of its superlayer (or stop it masking), asking for
/// the actions as `removeFromSuperlayer` does: the superlayer's
/// `sublayers` transition, then the layer's `onOrderOut`.
pub(crate) fn remove_from_superlayer(layer: &CALayerImpl) {
    let keep = as_layer(layer).retain();
    if let Some(parent) = unlink(layer) {
        transaction::detached(layer);
        run_transition(&parent, "sublayers");
        run_order_action(layer, "onOrderOut");
    }
    drop(keep);
}

/// Take `layer` out of its superlayer's sublayers, or out of the layer it
/// masks, with no actions; returns the superlayer (or masked layer). The
/// caller holds `layer`.
fn unlink(layer: &CALayerImpl) -> Option<Retained<CALayerImpl>> {
    if let Some(owner) = layer.read(|m| m.mask_of) {
        // SAFETY: the owner clears the link as it goes.
        let owner = imp(unsafe { owner.as_ref() }).retain();
        let gone = owner.write(|m| m.mask.take());
        layer.write(|m| m.mask_of = None);
        transaction::structure_changed(&owner);
        drop(gone);
        return Some(owner);
    }
    let sup = layer.read(|m| m.superlayer)?;
    // SAFETY: the superlayer clears the link as it goes.
    let parent = imp(unsafe { sup.as_ref() }).retain();
    let gone = parent.write(|m| {
        let at = m.sublayers.iter().position(|s| std::ptr::eq(imp(s), layer))?;
        m.needs_layout = true;
        Some(m.sublayers.remove(at))
    });
    layer.write(|m| m.superlayer = None);
    transaction::structure_changed(&parent);
    // Released after the links are right: dealloc runs program code.
    drop(gone);
    Some(parent)
}

/// `replaceSublayer:with:`: `new` where `old` was, asking the superlayer
/// for its transition once, then `old` for `onOrderOut` and `new` for
/// `onOrderIn` (measured).
fn replace(parent: &CALayerImpl, old: &CALayer, new: &CALayer, at: usize) {
    check_cycle(parent, new);
    let (keep_old, keep_new) = (old.retain(), new.retain());
    unlink(imp(old));
    transaction::detached(imp(old));
    link(parent, imp(new), at);
    run_transition(parent, "sublayers");
    run_order_action(imp(old), "onOrderOut");
    run_order_action(imp(new), "onOrderIn");
    drop((keep_old, keep_new));
}

/// `setSublayers:`: the new list in place of the old, asking the
/// superlayer for its transition once, the layers leaving for
/// `onOrderOut` and those joining for `onOrderIn`.
fn set_sublayers(parent: &CALayerImpl, list: Vec<Retained<CALayer>>) {
    let old = parent.read(|m| m.sublayers.clone());
    for s in &old {
        unlink(imp(s));
    }
    for s in &list {
        check_cycle(parent, s);
        link(parent, imp(s), usize::MAX);
    }
    if old.is_empty() && list.is_empty() {
        return;
    }
    let within = |l: &CALayer, v: &[Retained<CALayer>]| v.iter().any(|x| std::ptr::eq(&**x, l));
    for s in old.iter().filter(|s| !within(s, &list)) {
        transaction::detached(imp(s));
    }
    run_transition(parent, "sublayers");
    for s in old.iter().filter(|s| !within(s, &list)) {
        run_order_action(imp(s), "onOrderOut");
    }
    for s in list.iter().filter(|s| !within(s, &old)) {
        run_order_action(imp(s), "onOrderIn");
    }
}

/// The layer drawn topmost at `p` (in `layer`'s superlayer's space): its
/// sublayers first, the last drawn first, then itself.
fn hit(layer: &CALayerImpl, p: [f64; 2]) -> Option<Retained<CALayer>> {
    let (props, subs) = layer.read(|m| (m.props.clone(), m.sublayers.clone()));
    if props.hidden || props.opacity <= 0.0 {
        return None;
    }
    let to_self = invert_affine(&props.to_superlayer())?;
    let (x, y) = apply_affine(&to_self, p[0], p[1]);
    let [bx, by, w, h] = props.bounds;
    let inside = x >= bx && x < bx + w && y >= by && y < by + h;
    if props.masks_to_bounds && !inside {
        return None;
    }
    // Nothing shows outside a mask layer's bounds (measured).
    if let Some(mask) = layer.read(|m| m.mask.clone()) {
        let mp = imp(&mask).read(|m| m.props.clone());
        let [mx, my, mw, mh] = mp.bounds;
        let within = invert_affine(&mp.to_superlayer())
            .map(|inv| apply_affine(&inv, x, y))
            .is_some_and(|(u, v)| u >= mx && u < mx + mw && v >= my && v < my + mh);
        if !within {
            return None;
        }
    }
    // Sublayers see the point through the sublayer transform.
    let (sx, sy) = sublayer_point(&props, x, y);
    let mut order: Vec<&Retained<CALayer>> = subs.iter().collect();
    order.sort_by(|a, b| {
        let (za, zb) = (imp(a).read(|m| m.props.z_position), imp(b).read(|m| m.props.z_position));
        za.partial_cmp(&zb).unwrap_or(std::cmp::Ordering::Equal)
    });
    for sub in order.iter().rev() {
        if let Some(h) = hit(imp(sub), [sx, sy]) {
            return Some(h);
        }
    }
    // SAFETY: -containsPoint: takes a point.
    let contains: bool = unsafe { msg_send![layer, containsPoint: CGPoint::new(x, y)] };
    contains.then(|| as_layer(layer).retain())
}

/// A point in a layer's space as its sublayers' positions see it: through
/// the inverse of its sublayer transform (about its anchor point).
fn sublayer_point(props: &Props, x: f64, y: f64) -> (f64, f64) {
    match props.sublayer_map() {
        Some(m) => invert_affine(&m).map_or((x, y), |inv| apply_affine(&inv, x, y)),
        None => (x, y),
    }
}

/// The map from `layer`'s space to its superlayer's, sublayer transform
/// included.
fn up(layer: &CALayerImpl) -> [f64; 6] {
    let m = layer.read(|m| m.props.to_superlayer());
    let Some(sup) = layer.read(|m| m.superlayer) else { return m };
    // SAFETY: the superlayer clears the link as it goes.
    let parent = imp(unsafe { sup.as_ref() });
    match parent.read(|m| m.props.sublayer_map()) {
        Some(st) => compose(&m, &st),
        None => m,
    }
}

/// The map from `layer`'s space to its root's superlayer space (the
/// window's, for a view's layer tree).
fn to_root(layer: &CALayerImpl) -> [f64; 6] {
    let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut cur = Some(layer.retain());
    while let Some(l) = cur {
        m = compose(&m, &up(&l));
        // SAFETY: a superlayer clears the link as it goes.
        cur = l.read(|m| m.superlayer).map(|s| imp(unsafe { s.as_ref() }).retain());
    }
    m
}

/// The map from `from`'s space to `to`'s (none standing for the root's
/// superlayer space).
fn map_between(from: Option<&CALayerImpl>, to: Option<&CALayerImpl>) -> [f64; 6] {
    let a = from.map_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], to_root);
    let b = to.map_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], to_root);
    // Layers in different trees convert through their roots as though
    // the roots shared a space, as Core Animation does.
    compose(&a, &invert_affine(&b).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]))
}

fn map_rect(m: &[f64; 6], r: CGRect) -> CGRect {
    let corners = [
        (r.origin.x, r.origin.y),
        (r.origin.x + r.size.width, r.origin.y),
        (r.origin.x, r.origin.y + r.size.height),
        (r.origin.x + r.size.width, r.origin.y + r.size.height),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        let (px, py) = apply_affine(m, x, y);
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    CGRect::new(CGPoint::new(x0, y0), CGSize::new(x1 - x0, y1 - y0))
}

/// Whether a layer's contents show upside down relative to the root's
/// space (`contentsAreFlipped`): its own flip and its ancestors' taken
/// together, and, for a tree a view hosts, its superview's (the space the
/// tree is placed in runs down the window there).
pub(crate) fn flipped_on_screen(layer: &CALayerImpl) -> bool {
    let mut flipped = false;
    let mut cur = as_layer(layer).retain();
    loop {
        flipped ^= imp(&cur).read(|m| m.props.geometry_flipped);
        // SAFETY: a superlayer clears the link as it goes.
        match imp(&cur).read(|m| m.superlayer.or(m.mask_of)) {
            Some(s) => cur = unsafe { s.as_ref() }.retain(),
            None => break,
        }
    }
    flipped ^ super::backing::host_down(&cur)
}

/// The layer's own time for the media time `t`, through its ancestors'
/// timings.
pub(crate) fn local_time(layer: &CALayerImpl, t: f64) -> f64 {
    let mut chain = Vec::new();
    let mut cur = Some(layer.retain());
    while let Some(l) = cur {
        chain.push(l.read(|m| m.props.time));
        // SAFETY: a superlayer clears the link as it goes.
        cur = l.read(|m| m.superlayer).map(|s| imp(unsafe { s.as_ref() }).retain());
    }
    chain.iter().rev().fold(t, |t, time| time.local(t))
}

/// Whether the layer's time stands still: it or an ancestor has a speed
/// of 0.
pub(crate) fn time_frozen(layer: &CALayerImpl) -> bool {
    let mut cur = Some(layer.retain());
    while let Some(l) = cur {
        if l.read(|m| m.props.time.speed == 0.0) {
            return true;
        }
        // SAFETY: a superlayer clears the link as it goes.
        cur = l.read(|m| m.superlayer).map(|s| imp(unsafe { s.as_ref() }).retain());
    }
    false
}

/// The media time for the layer's own time `t` (none when an ancestor, or
/// it, is paused).
pub(crate) fn to_media(layer: &CALayerImpl, t: f64) -> Option<f64> {
    let mut t = t;
    let mut cur = Some(layer.retain());
    while let Some(l) = cur {
        t = l.read(|m| m.props.time).parent(t)?;
        // SAFETY: a superlayer clears the link as it goes.
        cur = l.read(|m| m.superlayer).map(|s| imp(unsafe { s.as_ref() }).retain());
    }
    Some(t)
}

// Animations.

fn add_animation(layer: &CALayerImpl, anim: &CAAnimation, key: Option<String>) {
    // A frozen copy (so later changes to the program's object don't count,
    // as on macOS), with the transaction's duration if it has none.
    let copy = super::animation::freeze_copy(anim);
    let Some(spec) = super::animation::spec_of(&copy, layer) else { return };
    // A transition goes under `kCATransition` whatever its key (measured).
    let key = if spec.is_transition() { Some("transition".to_owned()) } else { key };
    let groups = transaction::current_groups();
    let added = Added { key: key.clone(), object: copy, spec: Arc::new(spec), started: false, ended: false, groups };
    let gone: Vec<Added> = layer.write(|m| {
        let gone = match &key {
            Some(k) => {
                let (gone, kept) = std::mem::take(&mut m.anims).into_iter().partition(|a| a.key.as_deref() == Some(k));
                m.anims = kept;
                gone
            }
            None => Vec::new(),
        };
        m.anims.push(added);
        gone
    });
    transaction::animations_removed(layer, gone);
    transaction::animation_added(layer);
}

/// Whether any of the layer's animations change what it shows.
pub(crate) fn animating(layer: &CALayerImpl) -> bool {
    layer.read(|m| m.anims.iter().any(|a| !a.ended))
}

/// The layer's properties as its animations show them now, over what was
/// last committed (the model's, for a layer never committed).
pub(crate) fn presented_props(layer: &CALayerImpl) -> Option<Props> {
    let (mut props, specs) = layer.read(|m| {
        (
            m.committed.clone().unwrap_or_else(|| m.props.clone()),
            m.anims.iter().filter(|a| a.started).map(|a| a.spec.clone()).collect::<Vec<_>>(),
        )
    });
    let t = local_time(layer, math::media_now());
    super::spec::present(&mut props, &specs, t);
    Some(props)
}

/// `presentationLayer`: a copy of the layer as it shows now (made by the
/// class's `initWithLayer:`), whose tree links lead to presentation
/// layers; none for a layer never committed.
fn presentation(layer: &CALayerImpl) -> Option<Retained<CALayer>> {
    // A presentation layer presents itself.
    if layer.is_presentation() {
        return Some(as_layer(layer).retain());
    }
    if !layer.is_live() {
        return None;
    }
    let now = math::media_now();
    if let Some((at, p)) = layer.ivars().presentation.borrow().as_ref()
        && (now - at).abs() < 0.001
    {
        return Some(p.clone());
    }
    let props = presented_props(layer)?;
    let class = as_layer(layer).class();
    // SAFETY: +alloc on the layer's class, and its initWithLayer:.
    let copy: Retained<CALayer> = unsafe {
        let a: Allocated<CALayer> = msg_send![class, alloc];
        msg_send![a, initWithLayer: as_layer(layer)]
    };
    let c = imp(&copy);
    let objs = layer.read(|m| Objects {
        background_color: presented_color(&m.objs.background_color, m.props.background_color, props.background_color),
        border_color: presented_color(&m.objs.border_color, m.props.border_color, props.border_color),
        shadow_color: presented_color(&m.objs.shadow_color, m.props.shadow_color, props.shadow_color),
        contents: m.objs.contents.clone(),
        shadow_path: if props.shadow_path == m.props.shadow_path {
            m.objs.shadow_path.clone()
        } else {
            props.shadow_path.clone().map(objects::path)
        },
        gravity: m.objs.gravity.clone(),
        format: m.objs.format.clone(),
        min_filter: m.objs.min_filter.clone(),
        mag_filter: m.objs.mag_filter.clone(),
        corner_curve: m.objs.corner_curve.clone(),
        compositing_filter: m.objs.compositing_filter.clone(),
        filters: m.objs.filters.clone(),
        background_filters: m.objs.background_filters.clone(),
        fill_mode: m.objs.fill_mode.clone(),
    });
    let (timing, name, mask, drawn, extras, kind_objs, model_props) = layer.read(|m| {
        (
            m.timing,
            m.name.clone(),
            m.mask.clone(),
            m.drawn.clone(),
            m.extras.clone(),
            m.kind_objs.clone(),
            m.props.clone(),
        )
    });
    c.write(|m| {
        m.props = props;
        m.objs = objs;
        m.timing = timing;
        m.name = name;
        m.mask = mask;
        m.drawn = drawn;
        m.extras = extras;
        m.kind_objs = kind_objs;
    });
    super::shape::present_objects(c, &model_props);
    *c.ivars().model_layer.borrow_mut() = Weak::new(as_layer(layer));
    c.ivars().is_presentation.set(true);
    c.ivars().live.set(true);
    *layer.ivars().presentation.borrow_mut() = Some((now, copy.clone()));
    Some(copy)
}

fn presented_color(
    given: &Option<Retained<CGColor>>,
    model: Option<props::Rgba>,
    shown: Option<props::Rgba>,
) -> Option<Retained<CGColor>> {
    if shown == model { given.clone().or_else(|| shown.map(|_| black())) } else { shown.map(objects::color) }
}

/// Every layer in `layer`'s tree, breadth first, itself first (each once:
/// the tree can't have cycles, `insert` and `setMask:` refuse them, but a
/// layer is never listed twice regardless).
pub(crate) fn tree(layer: &CALayer) -> Vec<Retained<CALayer>> {
    let mut out = vec![layer.retain()];
    let mut seen: std::collections::HashSet<*const CALayer> = std::collections::HashSet::from([layer as *const _]);
    let mut i = 0;
    while i < out.len() {
        let (subs, mask) = imp(&out[i]).read(|m| (m.sublayers.clone(), m.mask.clone()));
        for l in subs.into_iter().chain(mask) {
            if seen.insert(&*l as *const CALayer) {
                out.push(l);
            }
        }
        i += 1;
    }
    out
}

/// The root of `layer`'s tree.
pub(crate) fn root_of(layer: &CALayerImpl) -> Retained<CALayer> {
    let mut cur = as_layer(layer).retain();
    loop {
        let next = imp(&cur).read(|m| m.superlayer.or(m.mask_of));
        match next {
            // SAFETY: a superlayer clears the link as it goes.
            Some(s) => cur = unsafe { s.as_ref() }.retain(),
            None => return cur,
        }
    }
}
