//! `NSNotification`: a name, an object and an optional user-info
//! dictionary. Immutable; the object and user info are retained. Two
//! notifications are equal when their names are equal strings, their
//! objects are the same object and their user infos are equal, and a
//! notification hashes as its name, as on macOS.

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCopying, NSNotification, NSString, NSUInteger, NSZone};

pub(crate) struct NotificationIvars {
    name: Retained<NSString>,
    object: Option<Retained<AnyObject>>,
    user_info: Option<Retained<AnyObject>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSNotification"]
    #[ivars = NotificationIvars]
    pub(crate) struct NSNotificationImpl;

    impl NSNotificationImpl {
        #[unsafe(method_id(initWithName:object:userInfo:))]
        fn init_with_name(
            this: Allocated<Self>,
            name: &NSString,
            object: Option<&AnyObject>,
            user_info: Option<&AnyObject>,
        ) -> Retained<Self> {
            let this = this.set_ivars(ivars(name, object, user_info));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(notificationWithName:object:))]
        fn with_name(name: &NSString, object: Option<&AnyObject>) -> Retained<Self> {
            make(name, object, None)
        }

        #[unsafe(method_id(notificationWithName:object:userInfo:))]
        fn with_name_user_info(
            name: &NSString,
            object: Option<&AnyObject>,
            user_info: Option<&AnyObject>,
        ) -> Retained<Self> {
            make(name, object, user_info)
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            self.ivars().name.clone()
        }

        #[unsafe(method_id(object))]
        fn object(&self) -> Option<Retained<AnyObject>> {
            self.ivars().object.clone()
        }

        #[unsafe(method_id(userInfo))]
        fn user_info(&self) -> Option<Retained<AnyObject>> {
            self.ivars().user_info.clone()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            equal(self.ivars(), other)
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.ivars().name.hash()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let ours = self.ivars();
            let mut text = format!("NSConcreteNotification {self:p} {{name = {}", ours.name);
            if let Some(object) = &ours.object {
                // SAFETY: -description returns a string.
                let described: Retained<NSString> = unsafe { msg_send![&**object, description] };
                text.push_str(&format!("; object = {described}"));
            }
            text.push('}');
            NSString::from_str(&text)
        }
    }

    unsafe impl NSObjectProtocol for NSNotificationImpl {}
);

/// Same name, same object, equal user info.
fn equal(ours: &NotificationIvars, other: Option<&AnyObject>) -> bool {
    let Some(other) = other.and_then(|o| o.downcast_ref::<NSNotification>()) else { return false };
    let same_object = match (&ours.object, other.object()) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(&**a, &*b),
        _ => false,
    };
    let same_info = match (&ours.user_info, other.userInfo()) {
        (None, None) => true,
        // SAFETY: -isEqual: takes an object and returns BOOL.
        (Some(a), Some(b)) => unsafe { msg_send![&**a, isEqual: &*b] },
        _ => false,
    };
    same_object && same_info && ours.name.isEqualToString(&other.name())
}

fn ivars(name: &NSString, object: Option<&AnyObject>, user_info: Option<&AnyObject>) -> NotificationIvars {
    NotificationIvars {
        name: name.copy(),
        object: object.map(|o| o.retain()),
        user_info: user_info.map(|u| u.retain()),
    }
}

/// Only called from the class's own methods, so the class is loaded.
fn make(name: &NSString, object: Option<&AnyObject>, user_info: Option<&AnyObject>) -> Retained<NSNotificationImpl> {
    let this = NSNotificationImpl::alloc().set_ivars(ivars(name, object, user_info));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A notification object, for frameworks that call delegates with one.
pub fn notification(name: &NSString, object: Option<&AnyObject>) -> Retained<NSNotification> {
    notification_with(name, object, None)
}

/// A notification with user info.
pub(crate) fn notification_with(
    name: &NSString,
    object: Option<&AnyObject>,
    user_info: Option<&AnyObject>,
) -> Retained<NSNotification> {
    let class = <NSNotification as objc2::ClassType>::class();
    // SAFETY: the class method takes a name, an object and a dictionary.
    unsafe { msg_send![class, notificationWithName: name, object: object, userInfo: user_info] }
}
