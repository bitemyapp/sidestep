//! `NSNull`: the one object that stands for nil inside collections.
//!
//! It lives in static memory, so `+null` costs no allocation and retains
//! and releases of it are free. `+new` and `-init` return it too.

use objc2::define_class;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2_foundation::{NSString, NSZone};
use sidestep_runtime::{ObjectRef, StaticObject};

/// The instance. NSNull has no instance variables, so the object is just
/// its class pointer.
static NULL: StaticObject<()> = StaticObject::new(&crate::NSNULL, ());

/// CoreFoundation's name for the instance.
#[unsafe(no_mangle)]
pub static kCFNull: ObjectRef = NULL.object_ref();

fn null() -> Retained<NSNullImpl> {
    // SAFETY: NULL is an immortal NSNull instance; retaining it is a no-op.
    unsafe { Retained::retain(NULL.as_object().cast()) }.expect("static object")
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSNull"]
    pub(crate) struct NSNullImpl;

    impl NSNullImpl {
        /// Not retained for the caller, and needing no autorelease: the
        /// object is immortal.
        #[unsafe(method(null))]
        fn null() -> *mut Self {
            NULL.as_object().cast()
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // The freshly allocated object is released unused.
            drop(this);
            null()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str("<null>")
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            null()
        }
    }

    unsafe impl NSObjectProtocol for NSNullImpl {}
);
