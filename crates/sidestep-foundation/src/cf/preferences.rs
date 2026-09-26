//! `CFPreferences` over `NSUserDefaults`.
//!
//! An application ID names a defaults domain: `kCFPreferencesCurrentApplication`
//! is this application's, `kCFPreferencesAnyApplication` is
//! `NSGlobalDomain`, and any other ID is a suite of that name. There is one
//! user and one host, so the user and host arguments only have to be one of
//! the constants.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSString, NSUserDefaults};

use super::types::object;

type Boolean = u8;
type CFIndex = isize;

crate::runloop::modes::exported_strings! {
    kCFPreferencesCurrentApplication, CURRENT_APPLICATION = "kCFPreferencesCurrentApplication";
    kCFPreferencesAnyApplication, ANY_APPLICATION = "kCFPreferencesAnyApplication";
    kCFPreferencesCurrentUser, CURRENT_USER = "kCFPreferencesCurrentUser";
    kCFPreferencesAnyUser, ANY_USER = "kCFPreferencesAnyUser";
    kCFPreferencesCurrentHost, CURRENT_HOST = "kCFPreferencesCurrentHost";
    kCFPreferencesAnyHost, ANY_HOST = "kCFPreferencesAnyHost";
}

/// The defaults for an application ID.
fn defaults(application: *const c_void) -> Retained<NSUserDefaults> {
    // SAFETY: the caller passes a string.
    let id = super::string::text(unsafe { object(application) }).into_owned();
    match id.as_str() {
        "kCFPreferencesCurrentApplication" => NSUserDefaults::standardUserDefaults(),
        "kCFPreferencesAnyApplication" => crate::user_defaults::with_domain("NSGlobalDomain"),
        _ if id == crate::user_defaults::application_domain() => NSUserDefaults::standardUserDefaults(),
        _ => crate::user_defaults::with_domain(&id),
    }
}

/// # Safety
///
/// `key` and `application` are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesCopyAppValue(
    key: *const c_void,
    application: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let key = unsafe { &*key.cast::<NSString>() };
    defaults(application).objectForKey(key).map_or(std::ptr::null_mut(), |v| Retained::into_raw(v).cast())
}

/// # Safety
///
/// `key` and `application` are strings; `value` is null or a property list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesSetAppValue(
    key: *const c_void,
    value: *const c_void,
    application: *const c_void,
) {
    // SAFETY: per this function's contract.
    unsafe {
        let key = &*key.cast::<NSString>();
        let value = value.cast::<AnyObject>().as_ref();
        defaults(application).setObject_forKey(value, key);
    }
}

/// # Safety
///
/// `key` and `application` are strings; `valid` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesGetAppBooleanValue(
    key: *const c_void,
    application: *const c_void,
    valid: *mut Boolean,
) -> Boolean {
    // SAFETY: per this function's contract.
    let key = unsafe { &*key.cast::<NSString>() };
    let d = defaults(application);
    let present = d.objectForKey(key).is_some() || d.stringForKey(key).is_some() || d.integerForKey(key) != 0;
    if !valid.is_null() {
        // SAFETY: per this function's contract.
        unsafe { valid.write(u8::from(present)) };
    }
    u8::from(d.boolForKey(key))
}

/// # Safety
///
/// As [`CFPreferencesGetAppBooleanValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesGetAppIntegerValue(
    key: *const c_void,
    application: *const c_void,
    valid: *mut Boolean,
) -> CFIndex {
    // SAFETY: per this function's contract.
    let key = unsafe { &*key.cast::<NSString>() };
    let d = defaults(application);
    let value = d.integerForKey(key);
    if !valid.is_null() {
        let present = d.objectForKey(key).is_some() || d.stringForKey(key).is_some() || value != 0;
        // SAFETY: per this function's contract.
        unsafe { valid.write(u8::from(present)) };
    }
    value
}

/// # Safety
///
/// `application` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesAppSynchronize(application: *const c_void) -> Boolean {
    u8::from(defaults(application).synchronize())
}

/// # Safety
///
/// `key` and `application` are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesCopyValue(
    key: *const c_void,
    application: *const c_void,
    _user: *const c_void,
    _host: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    unsafe { CFPreferencesCopyAppValue(key, application) }
}

/// # Safety
///
/// As [`CFPreferencesSetAppValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesSetValue(
    key: *const c_void,
    value: *const c_void,
    application: *const c_void,
    _user: *const c_void,
    _host: *const c_void,
) {
    // SAFETY: per this function's contract.
    unsafe { CFPreferencesSetAppValue(key, value, application) }
}

/// # Safety
///
/// `application` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFPreferencesSynchronize(
    application: *const c_void,
    _user: *const c_void,
    _host: *const c_void,
) -> Boolean {
    // SAFETY: per this function's contract.
    unsafe { CFPreferencesAppSynchronize(application) }
}
