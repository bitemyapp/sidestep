//! The first string made of a class may come from another class's method:
//! `mutableCopy` makes the first NSMutableString, a regular expression the
//! first NSTextCheckingResult. They must be of the classes objc2 names
//! (this file runs alone, in a process of its own, so nothing has used
//! those classes before).

use objc2::ClassType;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2_foundation::{
    NSMatchingOptions, NSMutableCopying, NSMutableString, NSRange, NSRegularExpression, NSRegularExpressionOptions,
    NSString, NSTextCheckingResult,
};

use sidestep as _;

#[test]
fn first_instances_are_of_the_named_classes() {
    let m: Retained<NSMutableString> = NSString::from_str("the start").mutableCopy();
    assert!(m.isKindOfClass(NSMutableString::class()));
    m.appendString(&NSString::from_str("!"));
    assert_eq!(m.to_string(), "the start!");
    let r = NSRegularExpression::regularExpressionWithPattern_options_error(
        &NSString::from_str("t"),
        NSRegularExpressionOptions(0),
    )
    .unwrap();
    let text = NSString::from_str("xt");
    let found = r.firstMatchInString_options_range(&text, NSMatchingOptions(0), NSRange::new(0, 2)).unwrap();
    assert!(found.isKindOfClass(NSTextCheckingResult::class()));
}
