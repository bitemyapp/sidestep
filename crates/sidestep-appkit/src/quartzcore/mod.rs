//! QuartzCore (Core Animation): the classes and functions
//! objc2-quartz-core declares, on Sidestep's drawing machinery.
//!
//! Core Animation has two halves, and so does this module. The *model* is
//! the layers programs change on their own thread: [`layer`] (`CALayer`,
//! its geometry, properties, tree, actions and presentation layer),
//! [`animation`] (`CAAnimation` and its subclasses, frozen as
//! [`spec::AnimSpec`] when added), [`function`] (`CAMediaTimingFunction`),
//! [`transaction`] (`CATransaction`, and the commit that sends layer
//! changes to the render thread), [`shape`] (`CAShapeLayer`,
//! `CAGradientLayer`), [`display_link`] (`CADisplayLink`), [`transform`]
//! (`CATransform3D…`, `CACurrentMediaTime`) and [`kvc`] (key-value coding
//! of layers). The *render* half is [`tree`]: the render thread's copy of
//! the committed trees, animated there from the same timing math
//! ([`spec`], [`keyframe`], [`math`]) the main thread uses for
//! `presentationLayer`, and composited as ops ([`render`]) where a
//! window's display pass placed them. [`backing`] is AppKit's side:
//! layer-backed views, their layers and their canvases.
//!
//! Everything a layer draws with is plain data ([`props::Props`]), so the
//! same values serve the model, the presentation and the render thread.

pub(crate) mod animation;
pub(crate) mod backing;
pub(crate) mod display_link;
pub(crate) mod function;
pub(crate) use sidestep_engine::ca::keyframe;
pub(crate) mod kvc;
pub(crate) mod layer;
pub(crate) use sidestep_engine::ca::math;
pub(crate) mod objects;
pub(crate) use sidestep_engine::ca::props;
pub(crate) mod render;
pub(crate) mod shape;
pub(crate) use sidestep_engine::ca::spec;
pub(crate) mod transaction;
pub(crate) mod transform;
pub(crate) mod tree;

use objc2::ClassType;
use objc2::runtime::{AnyClass, Imp, Sel};

sidestep_runtime::static_class!(pub CALAYER, CALAYER_META = "CALayer", || {
    layer::install_class_methods(layer::CALayerImpl::class());
});

sidestep_runtime::static_class!(pub CASHAPELAYER, CASHAPELAYER_META = "CAShapeLayer", || {
    let _ = shape::CAShapeLayerImpl::class();
});

sidestep_runtime::static_class!(pub CAGRADIENTLAYER, CAGRADIENTLAYER_META = "CAGradientLayer", || {
    let _ = shape::CAGradientLayerImpl::class();
});

sidestep_runtime::static_class!(pub CAANIMATION, CAANIMATION_META = "CAAnimation", || {
    animation::install_class_methods(animation::CAAnimationImpl::class());
});

sidestep_runtime::static_class!(pub CAPROPERTYANIMATION, CAPROPERTYANIMATION_META = "CAPropertyAnimation", || {
    animation::install_class_methods(animation::CAPropertyAnimationImpl::class());
});

sidestep_runtime::static_class!(pub CABASICANIMATION, CABASICANIMATION_META = "CABasicAnimation", || {
    let _ = animation::CABasicAnimationImpl::class();
});

sidestep_runtime::static_class!(pub CAKEYFRAMEANIMATION, CAKEYFRAMEANIMATION_META = "CAKeyframeAnimation", || {
    let _ = animation::CAKeyframeAnimationImpl::class();
});

sidestep_runtime::static_class!(pub CASPRINGANIMATION, CASPRINGANIMATION_META = "CASpringAnimation", || {
    let _ = animation::CASpringAnimationImpl::class();
});

sidestep_runtime::static_class!(pub CATRANSITION, CATRANSITION_META = "CATransition", || {
    let _ = animation::CATransitionImpl::class();
});

sidestep_runtime::static_class!(pub CAANIMATIONGROUP, CAANIMATIONGROUP_META = "CAAnimationGroup", || {
    let _ = animation::CAAnimationGroupImpl::class();
});

sidestep_runtime::static_class!(pub CAVALUEFUNCTION, CAVALUEFUNCTION_META = "CAValueFunction", || {
    let _ = animation::CAValueFunctionImpl::class();
});

sidestep_runtime::static_class!(pub CAMEDIATIMINGFUNCTION, CAMEDIATIMINGFUNCTION_META = "CAMediaTimingFunction", || {
    let _ = function::CAMediaTimingFunctionImpl::class();
});

sidestep_runtime::static_class!(pub CATRANSACTION, CATRANSACTION_META = "CATransaction", || {
    let _ = transaction::CATransactionImpl::class();
});

sidestep_runtime::static_class!(pub CADISPLAYLINK, CADISPLAYLINK_META = "CADisplayLink", || {
    let _ = display_link::CADisplayLinkImpl::class();
});

// The layer constants, with Core Animation's values (measured).
sidestep_foundation::constant_string!(kCAGravityCenter = "center");
sidestep_foundation::constant_string!(kCAGravityTop = "top");
sidestep_foundation::constant_string!(kCAGravityBottom = "bottom");
sidestep_foundation::constant_string!(kCAGravityLeft = "left");
sidestep_foundation::constant_string!(kCAGravityRight = "right");
sidestep_foundation::constant_string!(kCAGravityTopLeft = "topLeft");
sidestep_foundation::constant_string!(kCAGravityTopRight = "topRight");
sidestep_foundation::constant_string!(kCAGravityBottomLeft = "bottomLeft");
sidestep_foundation::constant_string!(kCAGravityBottomRight = "bottomRight");
sidestep_foundation::constant_string!(kCAGravityResize = "resize");
sidestep_foundation::constant_string!(kCAGravityResizeAspect = "resizeAspect");
sidestep_foundation::constant_string!(kCAGravityResizeAspectFill = "resizeAspectFill");
sidestep_foundation::constant_string!(kCAContentsFormatRGBA8Uint = "RGBA8");
sidestep_foundation::constant_string!(kCAContentsFormatRGBA16Float = "RGBAh");
sidestep_foundation::constant_string!(kCAContentsFormatGray8Uint = "Gray8");
sidestep_foundation::constant_string!(kCAContentsFormatAutomatic = "Automatic");
sidestep_foundation::constant_string!(kCAFilterNearest = "nearest");
sidestep_foundation::constant_string!(kCAFilterLinear = "linear");
sidestep_foundation::constant_string!(kCAFilterTrilinear = "trilinear");
sidestep_foundation::constant_string!(kCACornerCurveCircular = "circular");
sidestep_foundation::constant_string!(kCACornerCurveContinuous = "continuous");
sidestep_foundation::constant_string!(kCAOnOrderIn = "onOrderIn");
sidestep_foundation::constant_string!(kCAOnOrderOut = "onOrderOut");
sidestep_foundation::constant_string!(kCATransition = "transition");
sidestep_foundation::constant_string!(CAToneMapModeAutomatic = "automatic");
sidestep_foundation::constant_string!(CAToneMapModeNever = "never");
sidestep_foundation::constant_string!(CAToneMapModeIfSupported = "ifSupported");
sidestep_foundation::constant_string!(CADynamicRangeAutomatic = "automatic");
sidestep_foundation::constant_string!(CADynamicRangeStandard = "standard");
sidestep_foundation::constant_string!(CADynamicRangeConstrainedHigh = "constrainedHigh");
sidestep_foundation::constant_string!(CADynamicRangeHigh = "high");

/// Add a class method by hand (one that must see its receiver).
///
/// # Safety
///
/// `imp` takes the receiver, the selector and the arguments `types`
/// describes, and returns what it describes.
pub(crate) unsafe fn add_class_method(class: &AnyClass, sel: Sel, imp: Imp, types: &std::ffi::CStr) {
    let meta = (class.metaclass() as *const AnyClass).cast_mut();
    // SAFETY: as the caller promises; the class is being loaded.
    let added = unsafe { objc2::ffi::class_addMethod(meta, sel, imp, types.as_ptr()) };
    if !added.as_bool() {
        // SAFETY: as above.
        unsafe { objc2::ffi::class_replaceMethod(meta, sel, imp, types.as_ptr()) };
    }
}
