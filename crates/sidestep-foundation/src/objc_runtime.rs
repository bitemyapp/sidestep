//! The runtime lookups Foundation exports as C functions:
//! `NSClassFromString`, `NSStringFromClass` and their selector and
//! protocol counterparts.
//!
//! The strings they return are autoreleased, as on macOS. Lookups of
//! names that aren't registered return nil (a null selector for an empty
//! name).

use std::ffi::CString;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyProtocol, Sel};
use objc2_foundation::NSString;

fn name_of(string: &NSString) -> Option<CString> {
    CString::new(string.to_string()).ok()
}

fn autoreleased(text: &str) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str(text))
}

#[unsafe(no_mangle)]
pub extern "C" fn NSClassFromString(name: Option<&NSString>) -> Option<&'static AnyClass> {
    AnyClass::get(&name_of(name?)?)
}

#[unsafe(no_mangle)]
pub extern "C" fn NSStringFromClass(class: Option<&AnyClass>) -> *mut NSString {
    match class {
        Some(class) => autoreleased(&class.name().to_string_lossy()),
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn NSSelectorFromString(name: Option<&NSString>) -> Option<Sel> {
    let name = name_of(name?)?;
    (!name.as_bytes().is_empty()).then(|| Sel::register(&name))
}

#[unsafe(no_mangle)]
pub extern "C" fn NSStringFromSelector(selector: Option<Sel>) -> *mut NSString {
    match selector {
        Some(selector) => autoreleased(&selector.name().to_string_lossy()),
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn NSProtocolFromString(name: Option<&NSString>) -> Option<&'static AnyProtocol> {
    AnyProtocol::get(&name_of(name?)?)
}

#[unsafe(no_mangle)]
pub extern "C" fn NSStringFromProtocol(protocol: Option<&AnyProtocol>) -> *mut NSString {
    match protocol {
        Some(protocol) => autoreleased(&protocol.name().to_string_lossy()),
        None => std::ptr::null_mut(),
    }
}
