//! Methods Foundation adds to `NSObject` (forwarding through
//! `NSInvocation`) are there from the process's first contact with
//! `NSObject`, before any Foundation class has been used. Each file in
//! tests/ is its own process.

use objc2::runtime::NSObject;
use objc2::{ClassType, sel};

use sidestep as _;

#[test]
fn foundation_methods_are_on_nsobject_before_first_use() {
    let cls = NSObject::class();
    assert!(cls.instance_method(sel!(methodSignatureForSelector:)).is_some());
    assert!(cls.instance_method(sel!(forwardInvocation:)).is_some());
    assert!(cls.class_method(sel!(instanceMethodSignatureForSelector:)).is_some());
    // Instance methods of the root class are class methods too.
    assert!(cls.class_method(sel!(methodSignatureForSelector:)).is_some());
}
