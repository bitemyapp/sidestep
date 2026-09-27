//! `CAAnimation` and its subclasses: `CAPropertyAnimation`,
//! `CABasicAnimation`, `CAKeyframeAnimation`, `CASpringAnimation`,
//! `CATransition` and `CAAnimationGroup`, and `CAValueFunction`.
//!
//! Every animation keeps one state ([`AnimState`]) whatever its class, in
//! the base class's ivars. Adding one to a layer freezes a copy (changing
//! it then fails, as on macOS) and turns the copy into an
//! [`AnimSpec`](super::spec::AnimSpec), with the transaction's duration
//! when it has none. `+animation` and `+animationWithKeyPath:` make an
//! instance of the class they're sent to, so they're added to the classes
//! by hand (a `define_class!` class method can't see its receiver).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_core_foundation::{CFTimeInterval, CGFloat};
use objc2_core_graphics::CGPath;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSZone};
use objc2_quartz_core::{
    CAAnimation, CAAnimationGroup, CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTimingFunction,
    CAPropertyAnimation, CASpringAnimation, CATransition, CAValueFunction,
};

use super::layer::CALayerImpl;
use super::math::{Bezier, Spring};
use super::objects;
use super::props::{KeyPath, Value};
use super::spec::{AnimKind, AnimSpec, CalcMode, Combine, Direction, Fill, Timing, TransitionKind, ValueFn};

sidestep_foundation::constant_string!(kCAFillModeForwards = "forwards");
sidestep_foundation::constant_string!(kCAFillModeBackwards = "backwards");
sidestep_foundation::constant_string!(kCAFillModeBoth = "both");
sidestep_foundation::constant_string!(kCAFillModeRemoved = "removed");
sidestep_foundation::constant_string!(kCAAnimationLinear = "linear");
sidestep_foundation::constant_string!(kCAAnimationDiscrete = "discrete");
sidestep_foundation::constant_string!(kCAAnimationPaced = "paced");
sidestep_foundation::constant_string!(kCAAnimationCubic = "cubic");
sidestep_foundation::constant_string!(kCAAnimationCubicPaced = "cubicPaced");
sidestep_foundation::constant_string!(kCAAnimationRotateAuto = "auto");
sidestep_foundation::constant_string!(kCAAnimationRotateAutoReverse = "autoReverse");
sidestep_foundation::constant_string!(kCATransitionFade = "fade");
sidestep_foundation::constant_string!(kCATransitionMoveIn = "moveIn");
sidestep_foundation::constant_string!(kCATransitionPush = "push");
sidestep_foundation::constant_string!(kCATransitionReveal = "reveal");
sidestep_foundation::constant_string!(kCATransitionFromRight = "fromRight");
sidestep_foundation::constant_string!(kCATransitionFromLeft = "fromLeft");
sidestep_foundation::constant_string!(kCATransitionFromTop = "fromTop");
sidestep_foundation::constant_string!(kCATransitionFromBottom = "fromBottom");
sidestep_foundation::constant_string!(kCAValueFunctionRotateX = "rotateX");
sidestep_foundation::constant_string!(kCAValueFunctionRotateY = "rotateY");
sidestep_foundation::constant_string!(kCAValueFunctionRotateZ = "rotateZ");
sidestep_foundation::constant_string!(kCAValueFunctionScale = "scale");
sidestep_foundation::constant_string!(kCAValueFunctionScaleX = "scaleX");
sidestep_foundation::constant_string!(kCAValueFunctionScaleY = "scaleY");
sidestep_foundation::constant_string!(kCAValueFunctionScaleZ = "scaleZ");
sidestep_foundation::constant_string!(kCAValueFunctionTranslate = "translate");
sidestep_foundation::constant_string!(kCAValueFunctionTranslateX = "translateX");
sidestep_foundation::constant_string!(kCAValueFunctionTranslateY = "translateY");
sidestep_foundation::constant_string!(kCAValueFunctionTranslateZ = "translateZ");

static NEXT_SPEC: AtomicU64 = AtomicU64::new(1);

/// Everything any animation class keeps.
#[derive(Clone)]
pub(crate) struct AnimState {
    pub timing: Timing,
    pub fill_mode: Retained<NSString>,
    pub function: Option<Retained<CAMediaTimingFunction>>,
    pub delegate: Option<Retained<AnyObject>>,
    pub removed_on_completion: bool,
    pub frozen: bool,
    pub extras: Vec<(Retained<NSString>, Retained<AnyObject>)>,
    // CAPropertyAnimation.
    pub key_path: Option<Retained<NSString>>,
    pub additive: bool,
    pub cumulative: bool,
    pub value_function: Option<Retained<CAValueFunction>>,
    // CABasicAnimation.
    pub from: Option<Retained<AnyObject>>,
    pub to: Option<Retained<AnyObject>>,
    pub by: Option<Retained<AnyObject>>,
    // CAKeyframeAnimation.
    pub values: Option<Retained<NSArray>>,
    pub path: Option<Retained<CGPath>>,
    pub key_times: Option<Retained<NSArray<NSNumber>>>,
    pub functions: Option<Retained<NSArray<CAMediaTimingFunction>>>,
    pub calculation_mode: Retained<NSString>,
    pub rotation_mode: Option<Retained<NSString>>,
    pub tension: Option<Retained<NSArray<NSNumber>>>,
    pub continuity: Option<Retained<NSArray<NSNumber>>>,
    pub bias: Option<Retained<NSArray<NSNumber>>>,
    // CASpringAnimation.
    pub mass: f64,
    pub stiffness: f64,
    pub damping: f64,
    pub velocity: f64,
    pub overdamping: bool,
    // CATransition.
    pub kind: Retained<NSString>,
    pub subtype: Option<Retained<NSString>>,
    pub start_progress: f32,
    pub end_progress: f32,
    pub filter: Option<Retained<AnyObject>>,
    // CAAnimationGroup.
    pub animations: Option<Retained<NSArray<CAAnimation>>>,
}

impl AnimState {
    fn new() -> AnimState {
        AnimState {
            timing: Timing::default(),
            fill_mode: NSString::from_str("removed"),
            function: None,
            delegate: None,
            removed_on_completion: true,
            frozen: false,
            extras: Vec::new(),
            key_path: None,
            additive: false,
            cumulative: false,
            value_function: None,
            from: None,
            to: None,
            by: None,
            values: None,
            path: None,
            key_times: None,
            functions: None,
            calculation_mode: NSString::from_str("linear"),
            rotation_mode: None,
            tension: None,
            continuity: None,
            bias: None,
            mass: 1.0,
            stiffness: 100.0,
            damping: 10.0,
            velocity: 0.0,
            overdamping: false,
            kind: NSString::from_str("fade"),
            subtype: None,
            start_progress: 0.0,
            end_progress: 1.0,
            filter: None,
            animations: None,
        }
    }
}

pub(crate) struct AnimIvars {
    state: RefCell<AnimState>,
}

fn state_of(this: &AnyObject) -> &RefCell<AnimState> {
    // SAFETY: every animation is a CAAnimationImpl (or a subclass).
    let imp: &CAAnimationImpl = unsafe { &*(this as *const AnyObject).cast::<CAAnimationImpl>() };
    &imp.ivars().state
}

fn read<R>(this: &AnyObject, f: impl FnOnce(&AnimState) -> R) -> R {
    f(&state_of(this).borrow())
}

/// Change an animation, which a frozen one refuses, as on macOS.
fn edit(this: &AnyObject, f: impl FnOnce(&mut AnimState)) {
    let mut s = state_of(this).borrow_mut();
    if s.frozen {
        drop(s);
        panic!("attempting to modify read-only animation {this:p}");
    }
    f(&mut s);
}

/// `+defaultValueForKey:`'s answers for every animation class (measured).
fn default_value(key: &str) -> Option<Retained<AnyObject>> {
    let s = |v: &str| objects::any(NSString::from_str(v));
    Some(match key {
        "removedOnCompletion" => objects::boolean(true),
        "speed" => objects::number(1.0),
        "fillMode" => s("removed"),
        "calculationMode" => s("linear"),
        "type" => s("fade"),
        _ => return None,
    })
}

fn copy_obj<T: Message>(o: Option<&T>) -> Option<Retained<T>> {
    o.map(Message::retain)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; an animation's
    // state is never borrowed while a message it sends runs.
    #[unsafe(super(NSObject))]
    #[name = "CAAnimation"]
    #[ivars = AnimIvars]
    pub(crate) struct CAAnimationImpl;

    impl CAAnimationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AnimIvars { state: RefCell::new(AnimState::new()) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(defaultValueForKey:))]
        fn default_value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            default_value(&key.to_string())
        }

        #[unsafe(method(shouldArchiveValueForKey:))]
        fn should_archive_value_for_key(&self, _key: &NSString) -> bool {
            false
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<AnyObject> {
            let class = self.class();
            // SAFETY: +alloc and -init on the animation's own class.
            let copy: Retained<AnyObject> = unsafe {
                let a: Allocated<AnyObject> = msg_send![class, alloc];
                msg_send![a, init]
            };
            let mut state = read(self, Clone::clone);
            state.frozen = false;
            *state_of(&copy).borrow_mut() = state;
            copy
        }

        #[unsafe(method_id(timingFunction))]
        fn timing_function(&self) -> Option<Retained<CAMediaTimingFunction>> {
            read(self, |s| s.function.clone())
        }

        #[unsafe(method(setTimingFunction:))]
        fn set_timing_function(&self, f: Option<&CAMediaTimingFunction>) {
            let f = copy_obj(f);
            edit(self, |s| s.function = f);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            read(self, |s| s.delegate.clone())
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, d: Option<&AnyObject>) {
            let d = copy_obj(d);
            edit(self, |s| s.delegate = d);
        }

        #[unsafe(method(isRemovedOnCompletion))]
        fn is_removed_on_completion(&self) -> bool {
            read(self, |s| s.removed_on_completion)
        }

        #[unsafe(method(setRemovedOnCompletion:))]
        fn set_removed_on_completion(&self, flag: bool) {
            edit(self, |s| s.removed_on_completion = flag);
        }

        #[unsafe(method(runActionForKey:object:arguments:))]
        fn run_action_for_key(&self, key: &NSString, object: &AnyObject, _arguments: Option<&NSDictionary>) {
            if let Some(layer) = object.downcast_ref::<CALayer>() {
                // SAFETY: CALayer's method takes an animation and a key.
                let _: () = unsafe { msg_send![layer, addAnimation: self, forKey: key] };
            }
        }

        #[unsafe(method(preferredFrameRateRange))]
        fn preferred_frame_rate_range(&self) -> objc2_quartz_core::CAFrameRateRange {
            super::transform::CAFrameRateRangeDefault
        }

        #[unsafe(method(setPreferredFrameRateRange:))]
        fn set_preferred_frame_rate_range(&self, _range: objc2_quartz_core::CAFrameRateRange) {}

        // CAMediaTiming.

        #[unsafe(method(beginTime))]
        fn begin_time(&self) -> CFTimeInterval {
            read(self, |s| s.timing.begin)
        }

        #[unsafe(method(setBeginTime:))]
        fn set_begin_time(&self, t: CFTimeInterval) {
            edit(self, |s| s.timing.begin = t);
        }

        #[unsafe(method(duration))]
        fn duration(&self) -> CFTimeInterval {
            read(self, |s| s.timing.duration)
        }

        #[unsafe(method(setDuration:))]
        fn set_duration(&self, d: CFTimeInterval) {
            edit(self, |s| s.timing.duration = d);
        }

        #[unsafe(method(speed))]
        fn speed(&self) -> f32 {
            read(self, |s| s.timing.speed) as f32
        }

        #[unsafe(method(setSpeed:))]
        fn set_speed(&self, v: f32) {
            edit(self, |s| s.timing.speed = v as f64);
        }

        #[unsafe(method(timeOffset))]
        fn time_offset(&self) -> CFTimeInterval {
            read(self, |s| s.timing.offset)
        }

        #[unsafe(method(setTimeOffset:))]
        fn set_time_offset(&self, t: CFTimeInterval) {
            edit(self, |s| s.timing.offset = t);
        }

        #[unsafe(method(repeatCount))]
        fn repeat_count(&self) -> f32 {
            read(self, |s| s.timing.repeat_count) as f32
        }

        #[unsafe(method(setRepeatCount:))]
        fn set_repeat_count(&self, c: f32) {
            edit(self, |s| s.timing.repeat_count = c as f64);
        }

        #[unsafe(method(repeatDuration))]
        fn repeat_duration(&self) -> CFTimeInterval {
            read(self, |s| s.timing.repeat_duration)
        }

        #[unsafe(method(setRepeatDuration:))]
        fn set_repeat_duration(&self, d: CFTimeInterval) {
            edit(self, |s| s.timing.repeat_duration = d);
        }

        #[unsafe(method(autoreverses))]
        fn autoreverses(&self) -> bool {
            read(self, |s| s.timing.autoreverses)
        }

        #[unsafe(method(setAutoreverses:))]
        fn set_autoreverses(&self, flag: bool) {
            edit(self, |s| s.timing.autoreverses = flag);
        }

        #[unsafe(method_id(fillMode))]
        fn fill_mode(&self) -> Retained<NSString> {
            read(self, |s| s.fill_mode.clone())
        }

        #[unsafe(method(setFillMode:))]
        fn set_fill_mode(&self, mode: &NSString) {
            let fill = Fill::named(&mode.to_string());
            let copy = objc2_foundation::NSCopying::copy(mode);
            edit(self, |s| {
                s.timing.fill = fill;
                s.fill_mode = copy;
            });
        }

        // Key-value coding.

        #[unsafe(method_id(valueForKey:))]
        fn value_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            value_for_key(self, key)
        }

        #[unsafe(method(setValue:forKey:))]
        fn set_value_for_key(&self, value: Option<&AnyObject>, key: &NSString) {
            set_value_for_key(self, value, key);
        }
    }

    unsafe impl NSObjectProtocol for CAAnimationImpl {}
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CAAnimation, NSObject))]
    #[name = "CAPropertyAnimation"]
    pub(crate) struct CAPropertyAnimationImpl;

    impl CAPropertyAnimationImpl {
        #[unsafe(method_id(keyPath))]
        fn key_path(&self) -> Option<Retained<NSString>> {
            read(self, |s| s.key_path.clone())
        }

        #[unsafe(method(setKeyPath:))]
        fn set_key_path(&self, path: Option<&NSString>) {
            let path = path.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.key_path = path);
        }

        #[unsafe(method(isAdditive))]
        fn is_additive(&self) -> bool {
            read(self, |s| s.additive)
        }

        #[unsafe(method(setAdditive:))]
        fn set_additive(&self, flag: bool) {
            edit(self, |s| s.additive = flag);
        }

        #[unsafe(method(isCumulative))]
        fn is_cumulative(&self) -> bool {
            read(self, |s| s.cumulative)
        }

        #[unsafe(method(setCumulative:))]
        fn set_cumulative(&self, flag: bool) {
            edit(self, |s| s.cumulative = flag);
        }

        #[unsafe(method_id(valueFunction))]
        fn value_function(&self) -> Option<Retained<CAValueFunction>> {
            read(self, |s| s.value_function.clone())
        }

        #[unsafe(method(setValueFunction:))]
        fn set_value_function(&self, f: Option<&CAValueFunction>) {
            let f = copy_obj(f);
            edit(self, |s| s.value_function = f);
        }
    }
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CAPropertyAnimation, CAAnimation, NSObject))]
    #[name = "CABasicAnimation"]
    pub(crate) struct CABasicAnimationImpl;

    impl CABasicAnimationImpl {
        #[unsafe(method_id(fromValue))]
        fn from_value(&self) -> Option<Retained<AnyObject>> {
            read(self, |s| s.from.clone())
        }

        #[unsafe(method(setFromValue:))]
        fn set_from_value(&self, v: Option<&AnyObject>) {
            let v = copy_obj(v);
            edit(self, |s| s.from = v);
        }

        #[unsafe(method_id(toValue))]
        fn to_value(&self) -> Option<Retained<AnyObject>> {
            read(self, |s| s.to.clone())
        }

        #[unsafe(method(setToValue:))]
        fn set_to_value(&self, v: Option<&AnyObject>) {
            let v = copy_obj(v);
            edit(self, |s| s.to = v);
        }

        #[unsafe(method_id(byValue))]
        fn by_value(&self) -> Option<Retained<AnyObject>> {
            read(self, |s| s.by.clone())
        }

        #[unsafe(method(setByValue:))]
        fn set_by_value(&self, v: Option<&AnyObject>) {
            let v = copy_obj(v);
            edit(self, |s| s.by = v);
        }
    }
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CAPropertyAnimation, CAAnimation, NSObject))]
    #[name = "CAKeyframeAnimation"]
    pub(crate) struct CAKeyframeAnimationImpl;

    impl CAKeyframeAnimationImpl {
        #[unsafe(method_id(values))]
        fn values(&self) -> Option<Retained<NSArray>> {
            read(self, |s| s.values.clone())
        }

        #[unsafe(method(setValues:))]
        fn set_values(&self, v: Option<&NSArray>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.values = v);
        }

        #[unsafe(method(path))]
        fn path(&self) -> *mut CGPath {
            match read(self, |s| s.path.clone()) {
                Some(p) => Retained::autorelease_ptr(p),
                None => std::ptr::null_mut(),
            }
        }

        #[unsafe(method(setPath:))]
        fn set_path(&self, p: Option<&CGPath>) {
            let p = p.map(|p| objects::path(objects::path_shape(p)));
            edit(self, |s| s.path = p);
        }

        #[unsafe(method_id(keyTimes))]
        fn key_times(&self) -> Option<Retained<NSArray<NSNumber>>> {
            read(self, |s| s.key_times.clone())
        }

        #[unsafe(method(setKeyTimes:))]
        fn set_key_times(&self, v: Option<&NSArray<NSNumber>>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.key_times = v);
        }

        #[unsafe(method_id(timingFunctions))]
        fn timing_functions(&self) -> Option<Retained<NSArray<CAMediaTimingFunction>>> {
            read(self, |s| s.functions.clone())
        }

        #[unsafe(method(setTimingFunctions:))]
        fn set_timing_functions(&self, v: Option<&NSArray<CAMediaTimingFunction>>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.functions = v);
        }

        #[unsafe(method_id(calculationMode))]
        fn calculation_mode(&self) -> Retained<NSString> {
            read(self, |s| s.calculation_mode.clone())
        }

        #[unsafe(method(setCalculationMode:))]
        fn set_calculation_mode(&self, mode: &NSString) {
            let copy = objc2_foundation::NSCopying::copy(mode);
            edit(self, |s| s.calculation_mode = copy);
        }

        #[unsafe(method_id(tensionValues))]
        fn tension_values(&self) -> Option<Retained<NSArray<NSNumber>>> {
            read(self, |s| s.tension.clone())
        }

        #[unsafe(method(setTensionValues:))]
        fn set_tension_values(&self, v: Option<&NSArray<NSNumber>>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.tension = v);
        }

        #[unsafe(method_id(continuityValues))]
        fn continuity_values(&self) -> Option<Retained<NSArray<NSNumber>>> {
            read(self, |s| s.continuity.clone())
        }

        #[unsafe(method(setContinuityValues:))]
        fn set_continuity_values(&self, v: Option<&NSArray<NSNumber>>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.continuity = v);
        }

        #[unsafe(method_id(biasValues))]
        fn bias_values(&self) -> Option<Retained<NSArray<NSNumber>>> {
            read(self, |s| s.bias.clone())
        }

        #[unsafe(method(setBiasValues:))]
        fn set_bias_values(&self, v: Option<&NSArray<NSNumber>>) {
            let v = v.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.bias = v);
        }

        #[unsafe(method_id(rotationMode))]
        fn rotation_mode(&self) -> Option<Retained<NSString>> {
            read(self, |s| s.rotation_mode.clone())
        }

        #[unsafe(method(setRotationMode:))]
        fn set_rotation_mode(&self, mode: Option<&NSString>) {
            let mode = mode.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.rotation_mode = mode);
        }
    }
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CABasicAnimation, CAPropertyAnimation, CAAnimation, NSObject))]
    #[name = "CASpringAnimation"]
    pub(crate) struct CASpringAnimationImpl;

    impl CASpringAnimationImpl {
        /// A spring of this perceptual duration and bounce: its duration
        /// its settling duration, overdamping allowed (measured).
        #[unsafe(method_id(initWithPerceptualDuration:bounce:))]
        fn init_with_perceptual_duration(this: Allocated<Self>, duration: CFTimeInterval, bounce: CGFloat) -> Retained<Self> {
            // SAFETY: the designated initializer.
            let this: Retained<Self> = unsafe { msg_send![this, init] };
            let d = if duration > 0.0 { duration } else { 0.5 };
            let stiffness = (2.0 * std::f64::consts::PI / d).powi(2);
            let damping = if bounce >= 0.0 {
                (1.0 - bounce) * 4.0 * std::f64::consts::PI / d
            } else {
                4.0 * std::f64::consts::PI / (d * (1.0 + bounce))
            };
            edit(&this, |s| {
                s.mass = 1.0;
                s.stiffness = stiffness;
                s.damping = damping;
                s.overdamping = true;
            });
            let settle = read(&this, spring).settling_duration();
            edit(&this, |s| s.timing.duration = settle);
            this
        }

        #[unsafe(method_id(defaultValueForKey:))]
        fn default_value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            match key.to_string().as_str() {
                "mass" => Some(objects::number(1.0)),
                "stiffness" => Some(objects::number(100.0)),
                "damping" => Some(objects::number(10.0)),
                k => default_value(k),
            }
        }

        #[unsafe(method(mass))]
        fn mass(&self) -> CGFloat {
            read(self, |s| s.mass)
        }

        /// Only a positive mass is taken, as on macOS.
        #[unsafe(method(setMass:))]
        fn set_mass(&self, v: CGFloat) {
            if v > 0.0 {
                edit(self, |s| s.mass = v);
            }
        }

        #[unsafe(method(stiffness))]
        fn stiffness(&self) -> CGFloat {
            read(self, |s| s.stiffness)
        }

        #[unsafe(method(setStiffness:))]
        fn set_stiffness(&self, v: CGFloat) {
            if v > 0.0 {
                edit(self, |s| s.stiffness = v);
            }
        }

        #[unsafe(method(damping))]
        fn damping(&self) -> CGFloat {
            read(self, |s| s.damping)
        }

        #[unsafe(method(setDamping:))]
        fn set_damping(&self, v: CGFloat) {
            if v >= 0.0 {
                edit(self, |s| s.damping = v);
            }
        }

        #[unsafe(method(initialVelocity))]
        fn initial_velocity(&self) -> CGFloat {
            read(self, |s| s.velocity)
        }

        #[unsafe(method(setInitialVelocity:))]
        fn set_initial_velocity(&self, v: CGFloat) {
            edit(self, |s| s.velocity = v);
        }

        #[unsafe(method(allowsOverdamping))]
        fn allows_overdamping(&self) -> bool {
            read(self, |s| s.overdamping)
        }

        #[unsafe(method(setAllowsOverdamping:))]
        fn set_allows_overdamping(&self, flag: bool) {
            edit(self, |s| s.overdamping = flag);
        }

        #[unsafe(method(settlingDuration))]
        fn settling_duration(&self) -> CFTimeInterval {
            read(self, spring).settling_duration()
        }

        #[unsafe(method(perceptualDuration))]
        fn perceptual_duration(&self) -> CFTimeInterval {
            read(self, |s| 2.0 * std::f64::consts::PI * (s.mass / s.stiffness).sqrt())
        }

        #[unsafe(method(bounce))]
        fn bounce(&self) -> CGFloat {
            read(self, |s| {
                let zeta = s.damping / (2.0 * (s.stiffness * s.mass).sqrt());
                if zeta <= 1.0 { 1.0 - zeta } else { 1.0 / zeta - 1.0 }
            })
        }
    }
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CAAnimation, NSObject))]
    #[name = "CATransition"]
    pub(crate) struct CATransitionImpl;

    impl CATransitionImpl {
        #[unsafe(method_id(type))]
        fn kind(&self) -> Retained<NSString> {
            read(self, |s| s.kind.clone())
        }

        #[unsafe(method(setType:))]
        fn set_kind(&self, kind: &NSString) {
            let copy = objc2_foundation::NSCopying::copy(kind);
            edit(self, |s| s.kind = copy);
        }

        #[unsafe(method_id(subtype))]
        fn subtype(&self) -> Option<Retained<NSString>> {
            read(self, |s| s.subtype.clone())
        }

        #[unsafe(method(setSubtype:))]
        fn set_subtype(&self, subtype: Option<&NSString>) {
            let subtype = subtype.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.subtype = subtype);
        }

        #[unsafe(method(startProgress))]
        fn start_progress(&self) -> f32 {
            read(self, |s| s.start_progress)
        }

        #[unsafe(method(setStartProgress:))]
        fn set_start_progress(&self, v: f32) {
            edit(self, |s| s.start_progress = v);
        }

        #[unsafe(method(endProgress))]
        fn end_progress(&self) -> f32 {
            read(self, |s| s.end_progress)
        }

        #[unsafe(method(setEndProgress:))]
        fn set_end_progress(&self, v: f32) {
            edit(self, |s| s.end_progress = v);
        }

        #[unsafe(method_id(filter))]
        fn filter(&self) -> Option<Retained<AnyObject>> {
            read(self, |s| s.filter.clone())
        }

        #[unsafe(method(setFilter:))]
        fn set_filter(&self, f: Option<&AnyObject>) {
            let f = copy_obj(f);
            edit(self, |s| s.filter = f);
        }
    }
);

define_class!(
    // SAFETY: as CAAnimation; no ivars of its own.
    #[unsafe(super(CAAnimation, NSObject))]
    #[name = "CAAnimationGroup"]
    pub(crate) struct CAAnimationGroupImpl;

    impl CAAnimationGroupImpl {
        #[unsafe(method_id(animations))]
        fn animations(&self) -> Option<Retained<NSArray<CAAnimation>>> {
            read(self, |s| s.animations.clone())
        }

        #[unsafe(method(setAnimations:))]
        fn set_animations(&self, a: Option<&NSArray<CAAnimation>>) {
            let a = a.map(objc2_foundation::NSCopying::copy);
            edit(self, |s| s.animations = a);
        }
    }
);

pub(crate) struct ValueFunctionIvars {
    name: Retained<NSString>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; immutable.
    #[unsafe(super(NSObject))]
    #[name = "CAValueFunction"]
    #[ivars = ValueFunctionIvars]
    pub(crate) struct CAValueFunctionImpl;

    impl CAValueFunctionImpl {
        #[unsafe(method_id(functionWithName:))]
        fn function_with_name(name: &NSString) -> Option<Retained<CAValueFunction>> {
            value_function(name)
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            self.ivars().name.clone()
        }
    }
);

fn value_function(name: &NSString) -> Option<Retained<CAValueFunction>> {
    ValueFn::named(&name.to_string())?;
    let this =
        CAValueFunctionImpl::alloc().set_ivars(ValueFunctionIvars { name: objc2_foundation::NSCopying::copy(name) });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CAValueFunctionImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: the class is CAValueFunction.
    Some(unsafe { Retained::cast_unchecked(this) })
}

/// `+animation` and `+animationWithKeyPath:`, which make an instance of
/// the class they are sent to.
pub(crate) fn install_class_methods(class: &AnyClass) {
    unsafe extern "C-unwind" fn animation(cls: &AnyClass, _: Sel) -> *mut CAAnimation {
        // SAFETY: +new returns a new instance of the class.
        let a: Retained<CAAnimation> = unsafe { msg_send![cls, new] };
        Retained::autorelease_return(a)
    }
    unsafe extern "C-unwind" fn with_key_path(cls: &AnyClass, _: Sel, path: Option<&NSString>) -> *mut CAAnimation {
        // SAFETY: +new returns a new instance of the class.
        let a: Retained<CAAnimation> = unsafe { msg_send![cls, new] };
        let path = path.map(objc2_foundation::NSCopying::copy);
        edit(&a, |s| s.key_path = path);
        Retained::autorelease_return(a)
    }
    // SAFETY: the implementations take the receiver, the selector and the
    // arguments the encodings say, and return an object.
    unsafe {
        super::add_class_method(
            class,
            sel!(animation),
            std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyClass, Sel) -> *mut CAAnimation, objc2::runtime::Imp>(
                animation,
            ),
            c"@@:",
        );
        if std::ptr::eq(class, CAPropertyAnimation::class()) {
            super::add_class_method(
                class,
                sel!(animationWithKeyPath:),
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyClass, Sel, Option<&NSString>) -> *mut CAAnimation,
                    objc2::runtime::Imp,
                >(with_key_path),
                c"@@:@",
            );
        }
    }
}

// Key-value coding of animations: their properties by name, anything
// else kept as given.

fn value_for_key(this: &CAAnimationImpl, key: &NSString) -> Option<Retained<AnyObject>> {
    let k = key.to_string();
    let s = read(this, Clone::clone);
    Some(match k.as_str() {
        "duration" => objects::number(s.timing.duration),
        "beginTime" => objects::number(s.timing.begin),
        "speed" => objects::number(s.timing.speed),
        "timeOffset" => objects::number(s.timing.offset),
        "repeatCount" => objects::number(s.timing.repeat_count),
        "repeatDuration" => objects::number(s.timing.repeat_duration),
        "autoreverses" => objects::boolean(s.timing.autoreverses),
        "fillMode" => objects::any(s.fill_mode),
        "removedOnCompletion" => objects::boolean(s.removed_on_completion),
        "timingFunction" => objects::any(s.function?),
        "delegate" => s.delegate?,
        "keyPath" => objects::any(s.key_path?),
        "additive" => objects::boolean(s.additive),
        "cumulative" => objects::boolean(s.cumulative),
        "fromValue" => s.from?,
        "toValue" => s.to?,
        "byValue" => s.by?,
        "values" => objects::any(s.values?),
        "keyTimes" => objects::any(s.key_times?),
        "calculationMode" => objects::any(s.calculation_mode),
        "mass" => objects::number(s.mass),
        "stiffness" => objects::number(s.stiffness),
        "damping" => objects::number(s.damping),
        "initialVelocity" => objects::number(s.velocity),
        "type" => objects::any(s.kind),
        "subtype" => objects::any(s.subtype?),
        "animations" => objects::any(s.animations?),
        _ => return s.extras.iter().rev().find(|(n, _)| n.to_string() == k).map(|(_, v)| v.clone()),
    })
}

fn set_value_for_key(this: &CAAnimationImpl, value: Option<&AnyObject>, key: &NSString) {
    let k = key.to_string();
    let number = || {
        value.map_or(0.0, |v| {
            // SAFETY: numbers answer -doubleValue.
            unsafe { msg_send![v, doubleValue] }
        })
    };
    let setter = |sel: Sel| {
        let p: *const AnyObject = value.map_or(std::ptr::null(), |v| v as *const AnyObject);
        let o: &AnyObject = this.as_ref();
        // SAFETY: the setters below take objects.
        unsafe { super::kvc::send_set(o, sel, p) };
    };
    match k.as_str() {
        "duration" => edit(this, |s| s.timing.duration = number()),
        "beginTime" => edit(this, |s| s.timing.begin = number()),
        "speed" => edit(this, |s| s.timing.speed = number()),
        "timeOffset" => edit(this, |s| s.timing.offset = number()),
        "repeatCount" => edit(this, |s| s.timing.repeat_count = number()),
        "repeatDuration" => edit(this, |s| s.timing.repeat_duration = number()),
        "autoreverses" => edit(this, |s| s.timing.autoreverses = number() != 0.0),
        "removedOnCompletion" => edit(this, |s| s.removed_on_completion = number() != 0.0),
        "additive" => edit(this, |s| s.additive = number() != 0.0),
        "cumulative" => edit(this, |s| s.cumulative = number() != 0.0),
        "fromValue" => edit(this, |s| s.from = copy_obj(value)),
        "toValue" => edit(this, |s| s.to = copy_obj(value)),
        "byValue" => edit(this, |s| s.by = copy_obj(value)),
        "delegate" => edit(this, |s| s.delegate = copy_obj(value)),
        "fillMode" | "timingFunction" | "keyPath" | "values" | "keyTimes" | "calculationMode" | "type" | "subtype"
        | "animations" => {
            if let Some(sel) = super::kvc::setter(&k) {
                setter(sel);
            }
        }
        _ => {
            // Any other key is kept, even on a frozen animation (measured).
            let value = copy_obj(value);
            let name: Retained<NSString> = objc2_foundation::NSCopying::copy(key);
            let mut s = state_of(this).borrow_mut();
            s.extras.retain(|(n, _)| n.to_string() != k);
            if let Some(v) = value {
                s.extras.push((name, v));
            }
        }
    }
}

// Freezing and specs.

fn spring(s: &AnimState) -> Spring {
    Spring {
        mass: s.mass,
        stiffness: s.stiffness,
        damping: s.damping,
        velocity: s.velocity,
        overdamping: s.overdamping,
    }
}

/// A frozen copy of `anim` for a layer to keep, with the transaction's
/// duration if it has none and its timing function if it has one.
pub(crate) fn freeze_copy(anim: &CAAnimation) -> Retained<CAAnimation> {
    // SAFETY: -copy returns a new animation of the same class.
    let copy: Retained<CAAnimation> = unsafe { msg_send![anim, copy] };
    let duration = super::transaction::animation_duration();
    let function = super::transaction::timing_function();
    let mut s = state_of(&copy).borrow_mut();
    if s.timing.duration <= 0.0 {
        s.timing.duration = duration;
    }
    if s.function.is_none() && function.is_some() {
        s.function = function;
    }
    s.frozen = true;
    drop(s);
    copy
}

/// Set a frozen animation's begin time (at the commit that starts it).
pub(crate) fn settle_begin(anim: &CAAnimation, begin: f64) {
    state_of(anim).borrow_mut().timing.begin = begin;
}

fn is_kind(obj: &AnyObject, class: &AnyClass) -> bool {
    // SAFETY: -isKindOfClass: takes a class.
    unsafe { msg_send![obj, isKindOfClass: class] }
}

fn numbers(a: &Option<Retained<NSArray<NSNumber>>>) -> Option<Vec<f64>> {
    a.as_ref().map(|a| a.iter().map(|n| n.doubleValue()).collect())
}

/// What an animation does to `layer`, frozen.
pub(crate) fn spec_of(anim: &CAAnimation, layer: &CALayerImpl) -> Option<AnimSpec> {
    let s = read(anim, Clone::clone);
    let props = layer.read(|m| m.props.clone());
    let function = s.function.as_deref().map(super::function::curve);
    let path_of = || s.key_path.as_ref().and_then(|p| KeyPath::parse(&p.to_string()));
    let like = |path: KeyPath| props.get_path(path);
    let convert =
        |o: &Option<Retained<AnyObject>>, like: &Value| o.as_deref().and_then(|o| objects::to_value(Some(o), like));
    let kind = if is_kind(anim, CAAnimationGroup::class()) {
        let children = s.animations.as_ref().map(|a| a.iter().collect::<Vec<_>>()).unwrap_or_default();
        let specs = children
            .iter()
            .filter_map(|c| {
                // Children keep their own durations (none takes the group's).
                spec_of(c, layer)
            })
            .map(std::sync::Arc::new)
            .collect();
        AnimKind::Group(specs)
    } else if is_kind(anim, CATransition::class()) {
        let kind = match s.kind.to_string().as_str() {
            "moveIn" => TransitionKind::MoveIn,
            "push" => TransitionKind::Push,
            "reveal" => TransitionKind::Reveal,
            _ => TransitionKind::Fade,
        };
        let direction = match s.subtype.as_ref().map(|t| t.to_string()).as_deref() {
            Some("fromRight") => Direction::Right,
            Some("fromTop") => Direction::Top,
            Some("fromBottom") => Direction::Bottom,
            _ => Direction::Left,
        };
        AnimKind::Transition { kind, direction, start: s.start_progress as f64, end: s.end_progress as f64 }
    } else if is_kind(anim, CAPropertyAnimation::class()) {
        match path_of().and_then(|p| Some((p, like(p)?))) {
            Some((path, like)) => {
                // Through a value function, numbers (three for a scale or
                // translation, measured) made into the transform.
                let function =
                    s.value_function.as_deref().and_then(value_fn_of).filter(|_| matches!(like, Value::Transform(_)));
                let like = match function {
                    Some(f) if f.takes_three() => Value::Numbers(Vec::new()),
                    Some(_) => Value::Number(0.0),
                    None => like,
                };
                let convert = |o: &Option<Retained<AnyObject>>| {
                    convert(o, &like).filter(|v| !matches!((function, v), (Some(_), Value::Numbers(n)) if n.len() != 3))
                };
                let how = Combine { additive: s.additive, cumulative: s.cumulative, function };
                if is_kind(anim, CASpringAnimation::class()) {
                    AnimKind::Spring {
                        path,
                        from: convert(&s.from),
                        to: convert(&s.to),
                        by: convert(&s.by),
                        spring: spring(&s),
                        how,
                    }
                } else if is_kind(anim, CABasicAnimation::class()) {
                    AnimKind::Basic { path, from: convert(&s.from), to: convert(&s.to), by: convert(&s.by), how }
                } else if is_kind(anim, CAKeyframeAnimation::class()) {
                    keyframes(&s, path, &like, how)
                } else {
                    AnimKind::None
                }
            }
            None => AnimKind::None,
        }
    } else {
        AnimKind::None
    };
    Some(AnimSpec {
        id: NEXT_SPEC.fetch_add(1, Ordering::Relaxed),
        timing: s.timing,
        function,
        removed_on_completion: s.removed_on_completion,
        kind,
    })
}

/// A keyframe animation's values and timing, frozen: key times of another
/// count than the values taken with the values' common prefix (measured;
/// discrete takes one more key time, the last segment's end), and paced
/// keyframes timed by their distances once, here.
fn keyframes(s: &AnimState, path: KeyPath, like: &Value, how: Combine) -> AnimKind {
    let mut mode = CalcMode::named(&s.calculation_mode.to_string());
    let (mut values, mut key_times) = match (&s.path, &s.values) {
        (Some(p), _) => {
            let discrete = mode == CalcMode::Discrete;
            let (mut values, at) = path_points(&objects::path_shape(p), like, !discrete);
            // Discrete, each element holds where it starts.
            if discrete && values.len() > 1 {
                values.pop();
            }
            (values, path_key_times(&at, numbers(&s.key_times), mode))
        }
        (None, Some(v)) => {
            (v.iter().filter_map(|o| objects::to_value(Some(&o), like)).collect(), numbers(&s.key_times))
        }
        _ => (Vec::new(), numbers(&s.key_times)),
    };
    let mut functions: Vec<Option<Bezier>> =
        s.functions.as_ref().map(|f| f.iter().map(|f| Some(super::function::curve(&f))).collect()).unwrap_or_default();
    if let Some(kt) = &mut key_times {
        let n = values.len();
        if kt.len() != n && !(mode == CalcMode::Discrete && kt.len() == n + 1) {
            let m = kt.len().min(n);
            kt.truncate(m);
            values.truncate(m);
        }
    }
    if mode == CalcMode::Paced && key_times.is_none() && values.len() > 1 {
        // Even speed ignores the timing functions (measured).
        key_times = Some(super::keyframe::paced_times(&values));
        mode = CalcMode::Linear;
        functions.clear();
    }
    AnimKind::Keyframe { path, values, key_times, functions, mode, how }
}

/// The value function a `CAValueFunction` is.
fn value_fn_of(f: &CAValueFunction) -> Option<ValueFn> {
    let obj: &AnyObject = f;
    let imp = obj.downcast_ref::<CAValueFunctionImpl>()?;
    ValueFn::named(&imp.ivars().name.to_string())
}

/// A keyframe path's points as values, each with where it is along the
/// path's elements (element `e`'s end is at `e`; a curve, followed by
/// flattening it finely unless `flatten` is false, has its points in
/// between). Discrete keyframes take only the elements' ends.
fn path_points(shape: &crate::coregraphics::path::Shape, like: &Value, flatten: bool) -> (Vec<Value>, Vec<f64>) {
    use kurbo::PathEl;
    if !matches!(like, Value::Point(_)) {
        return (Vec::new(), Vec::new());
    }
    let (mut out, mut at) = (Vec::new(), Vec::new());
    let mut last = kurbo::Point::ZERO;
    let mut push = |p: kurbo::Point, e: f64, out: &mut Vec<Value>| {
        out.push(Value::Point([p.x, p.y]));
        at.push(e);
    };
    let mut element = -1.0;
    for el in shape.elements() {
        match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => {
                element += 1.0;
                push(*p, element, &mut out);
                last = *p;
            }
            PathEl::QuadTo(c, p) => {
                let q = kurbo::QuadBez::new(last, *c, *p);
                let steps = if flatten { 24 } else { 1 };
                for i in 1..=steps {
                    let f = i as f64 / steps as f64;
                    push(kurbo::ParamCurve::eval(&q, f), element + f, &mut out);
                }
                element += 1.0;
                last = *p;
            }
            PathEl::CurveTo(a, b, p) => {
                let c = kurbo::CubicBez::new(last, *a, *b, *p);
                let steps = if flatten { 32 } else { 1 };
                for i in 1..=steps {
                    let f = i as f64 / steps as f64;
                    push(kurbo::ParamCurve::eval(&c, f), element + f, &mut out);
                }
                element += 1.0;
                last = *p;
            }
            PathEl::ClosePath => {}
        }
    }
    (out, at)
}

/// Key times for a path's flattened points, so each of the path's elements
/// takes its keyframe's share of the time as it would unflattened (the
/// key times given, one per element end, or equal shares); paced modes
/// space the points by distance and take none.
fn path_key_times(at: &[f64], given: Option<Vec<f64>>, mode: CalcMode) -> Option<Vec<f64>> {
    let n = at.last().copied().unwrap_or(0.0);
    if at.iter().all(|p| p.fract() == 0.0) || n <= 0.0 || matches!(mode, CalcMode::Paced | CalcMode::CubicPaced) {
        return given;
    }
    match given {
        Some(kt) if kt.len() == n as usize + 1 => Some(
            at.iter()
                .map(|p| {
                    let e = (p.floor() as usize).min(kt.len() - 2);
                    kt[e] + (kt[e + 1] - kt[e]) * (p - e as f64)
                })
                .collect(),
        ),
        None => Some(at.iter().map(|p| p / n).collect()),
        other => other,
    }
}

/// The animation an implicit change runs: from the value before it, over
/// the transaction's duration (settled when it's added), with the
/// transaction's timing function or the default one, filling backwards.
pub(crate) fn implicit_animation(key: &str, from: Option<&AnyObject>) -> Retained<CABasicAnimation> {
    crate::load_shell::<CABasicAnimation>();
    // SAFETY: +new returns a new animation.
    let a: Retained<CABasicAnimation> = unsafe { msg_send![CABasicAnimation::class(), new] };
    let function = super::transaction::timing_function().unwrap_or_else(super::function::default_function);
    edit(&a, |s| {
        s.key_path = Some(NSString::from_str(key));
        s.from = from.map(Message::retain);
        s.function = Some(function);
        s.timing.fill = Fill::Backwards;
        s.fill_mode = NSString::from_str("backwards");
    });
    a
}

/// The fade a change of the keys macOS fades runs.
pub(crate) fn implicit_transition() -> Retained<CATransition> {
    crate::load_shell::<CATransition>();
    // SAFETY: +new returns a new transition.
    let t: Retained<CATransition> = unsafe { msg_send![CATransition::class(), new] };
    if let Some(f) = super::transaction::timing_function() {
        edit(&t, |s| s.function = Some(f));
    }
    t
}

/// The delegate of an animation, if it has one.
pub(crate) fn delegate_of(anim: &CAAnimation) -> Option<Retained<AnyObject>> {
    read(anim, |s| s.delegate.clone())
}
