//! `NSNotification`, so far only as the argument of delegate callbacks.

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSNotification, NSString};

pub(crate) struct NotificationIvars {
    name: Retained<NSString>,
    object: Option<Retained<AnyObject>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSNotification"]
    #[ivars = NotificationIvars]
    pub(crate) struct NSNotificationImpl;

    impl NSNotificationImpl {
        #[unsafe(method_id(notificationWithName:object:))]
        fn with_name(name: &NSString, object: Option<&AnyObject>) -> Retained<Self> {
            make(name, object)
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            self.ivars().name.clone()
        }

        #[unsafe(method_id(object))]
        fn object(&self) -> Option<Retained<AnyObject>> {
            self.ivars().object.clone()
        }
    }

    unsafe impl NSObjectProtocol for NSNotificationImpl {}
);

fn make(name: &NSString, object: Option<&AnyObject>) -> Retained<NSNotificationImpl> {
    let ivars = NotificationIvars { name: name.retain(), object: object.map(|o| o.retain()) };
    let this: Allocated<NSNotificationImpl> = NSNotificationImpl::alloc();
    let this = this.set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A notification object, for frameworks that call delegates with one.
pub fn notification(name: &NSString, object: Option<&AnyObject>) -> Retained<NSNotification> {
    // SAFETY: NSNotificationImpl is the class registered as NSNotification.
    let class = <NSNotification as objc2::ClassType>::class();
    unsafe { msg_send![class, notificationWithName: name, object: object] }
}
