//! Introspection as a process's very first contact with a class. Each file
//! in tests/ is its own process, so nothing here can be warmed up by another
//! test.

use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{ClassType, ProtocolType, sel};
use objc2_foundation::NSString;

use sidestep as _;

#[test]
fn introspection_before_first_message() {
    let proto = <dyn NSObjectProtocol>::protocol().unwrap();
    assert!(NSObject::class().conforms_to(proto));
    assert!(NSString::class().instance_method(sel!(length)).is_some());
    assert_eq!(NSString::class().superclass(), Some(NSObject::class()));
    // `responds_to` asks about instances; +alloc belongs to the metaclass.
    assert!(!NSString::class().responds_to(sel!(alloc)));
    assert!(NSString::class().metaclass().responds_to(sel!(alloc)));
}
