//! `-[NSObject description]` as the process's first use of `NSString`: the
//! root class, which the runtime defines, finds the string class by name.
//! Each file in tests/ is its own process.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2_foundation::NSString;

use sidestep as _;

#[test]
fn description_before_strings_are_used() {
    let obj = NSObject::new();
    let description: Option<Retained<NSString>> = unsafe { msg_send![&*obj, description] };
    let description = description.expect("a description").to_string();
    assert!(description.starts_with("<NSObject: 0x"), "{description}");
    assert!(description.ends_with('>'), "{description}");
}
