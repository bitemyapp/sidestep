//! Stand-ins for Foundation classes Sidestep doesn't have yet, for tests:
//! AppKit reads numbers with `doubleValue` and arrays with `count` and
//! `objectAtIndex:`, so these answer those.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestNumber"]
    #[ivars = f64]
    pub(crate) struct Number;

    impl Number {
        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            *self.ivars()
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestArray"]
    #[ivars = Vec<Retained<AnyObject>>]
    pub(crate) struct Array;

    impl Array {
        #[unsafe(method(count))]
        fn count(&self) -> usize {
            self.ivars().len()
        }

        #[unsafe(method_id(objectAtIndex:))]
        fn object_at_index(&self, index: usize) -> Retained<AnyObject> {
            self.ivars()[index].clone()
        }
    }
);

pub(crate) fn number(value: f64) -> Retained<Number> {
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(Number::alloc().set_ivars(value)), init] }
}

pub(crate) fn array(items: Vec<Retained<AnyObject>>) -> Retained<Array> {
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(Array::alloc().set_ivars(items)), init] }
}
