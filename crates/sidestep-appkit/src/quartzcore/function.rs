//! `CAMediaTimingFunction`: a cubic Bézier curve from (0, 0) to (1, 1).
//!
//! The named functions are shared instances, one per name, with the
//! control points macOS reports for them (`getControlPointAtIndex:values:`,
//! measured): linear (0, 0) and (1, 1); ease-in (0.42, 0) and (1, 1);
//! ease-out (0, 0) and (0.58, 1); ease-in-ease-out (0.42, 0) and (0.58, 1);
//! default (0.25, 0.1) and (0.25, 1).

use std::ffi::c_float;
use std::sync::OnceLock;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::NSString;
use objc2_quartz_core::CAMediaTimingFunction;

use super::math::Bezier;

sidestep_foundation::constant_string!(kCAMediaTimingFunctionLinear = "linear");
sidestep_foundation::constant_string!(kCAMediaTimingFunctionEaseIn = "easeIn");
sidestep_foundation::constant_string!(kCAMediaTimingFunctionEaseOut = "easeOut");
sidestep_foundation::constant_string!(kCAMediaTimingFunctionEaseInEaseOut = "easeInEaseOut");
sidestep_foundation::constant_string!(kCAMediaTimingFunctionDefault = "default");

/// The named curves and their control points.
const NAMED: [(&str, Bezier); 5] = [
    ("linear", Bezier { c1: [0.0, 0.0], c2: [1.0, 1.0] }),
    ("easeIn", Bezier { c1: [0.42, 0.0], c2: [1.0, 1.0] }),
    ("easeOut", Bezier { c1: [0.0, 0.0], c2: [0.58, 1.0] }),
    ("easeInEaseOut", Bezier { c1: [0.42, 0.0], c2: [0.58, 1.0] }),
    ("default", Bezier { c1: [0.25, 0.1], c2: [0.25, 1.0] }),
];

pub(crate) struct FunctionIvars {
    curve: Bezier,
    /// The name of a shared named function.
    name: Option<&'static str>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a function never
    // changes after it's made.
    #[unsafe(super(NSObject))]
    #[name = "CAMediaTimingFunction"]
    #[ivars = FunctionIvars]
    pub(crate) struct CAMediaTimingFunctionImpl;

    impl CAMediaTimingFunctionImpl {
        /// An unknown name raises, as on macOS.
        #[unsafe(method_id(functionWithName:))]
        fn function_with_name(name: &NSString) -> Retained<CAMediaTimingFunction> {
            let name = name.to_string();
            named(&name).unwrap_or_else(|| panic!("unknown timing function name: {name}"))
        }

        #[unsafe(method_id(functionWithControlPoints::::))]
        fn function_with_control_points(
            c1x: c_float,
            c1y: c_float,
            c2x: c_float,
            c2y: c_float,
        ) -> Retained<CAMediaTimingFunction> {
            new(Bezier { c1: [c1x as f64, c1y as f64], c2: [c2x as f64, c2y as f64] }, None)
        }

        #[unsafe(method_id(initWithControlPoints::::))]
        fn init_with_control_points(
            this: Allocated<Self>,
            c1x: c_float,
            c1y: c_float,
            c2x: c_float,
            c2y: c_float,
        ) -> Retained<Self> {
            let curve = Bezier { c1: [c1x as f64, c1y as f64], c2: [c2x as f64, c2y as f64] };
            let this = this.set_ivars(FunctionIvars { curve, name: None });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(FunctionIvars { curve: NAMED[0].1, name: None });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        /// # Safety
        ///
        /// `values` points at two writable floats.
        #[unsafe(method(getControlPointAtIndex:values:))]
        unsafe fn get_control_point(&self, index: usize, values: *mut c_float) {
            let c = self.ivars().curve;
            let point = match index {
                0 => [0.0, 0.0],
                1 => c.c1,
                2 => c.c2,
                3 => [1.0, 1.0],
                _ => panic!("-[CAMediaTimingFunction getControlPointAtIndex:values:]: index {index} out of range"),
            };
            if !values.is_null() {
                // SAFETY: as the caller promises.
                unsafe {
                    values.write(point[0] as c_float);
                    values.add(1).write(point[1] as c_float);
                }
            }
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            match self.ivars().name {
                Some(name) => NSString::from_str(name),
                None => {
                    let c = self.ivars().curve;
                    NSString::from_str(&format!("{} {} {} {}", c.c1[0], c.c1[1], c.c2[0], c.c2[1]))
                }
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut objc2_foundation::NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    // Equal only to itself, as on macOS.
    unsafe impl NSObjectProtocol for CAMediaTimingFunctionImpl {}
);

fn new(curve: Bezier, name: Option<&'static str>) -> Retained<CAMediaTimingFunction> {
    crate::load_shell::<CAMediaTimingFunction>();
    let this = CAMediaTimingFunctionImpl::alloc().set_ivars(FunctionIvars { curve, name });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CAMediaTimingFunctionImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: the class is CAMediaTimingFunction.
    unsafe { Retained::cast_unchecked(this) }
}

/// The shared function of a name, or `None` for an unknown name.
pub(crate) fn named(name: &str) -> Option<Retained<CAMediaTimingFunction>> {
    static SHARED: OnceLock<[usize; 5]> = OnceLock::new();
    let at = NAMED.iter().position(|(n, _)| *n == name)?;
    let all =
        SHARED.get_or_init(|| std::array::from_fn(|i| Retained::into_raw(new(NAMED[i].1, Some(NAMED[i].0))) as usize));
    // SAFETY: the shared functions are never released; this is a new
    // reference to one.
    unsafe { Retained::retain(all[at] as *mut CAMediaTimingFunction) }
}

/// The curve of a function: Sidestep's own keep theirs; any other object
/// counts as linear.
pub(crate) fn curve(f: &CAMediaTimingFunction) -> Bezier {
    let obj: &AnyObject = f;
    if let Some(imp) = obj.downcast_ref::<CAMediaTimingFunctionImpl>() {
        return imp.ivars().curve;
    }
    Bezier::LINEAR
}

/// The default function of an implicit animation.
pub(crate) fn default_function() -> Retained<CAMediaTimingFunction> {
    named("default").expect("the default function")
}
