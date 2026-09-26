//! `NSDictionary`: an immutable map. Attribute dictionaries and other small
//! maps dominate, so entries live in a vector searched with `-isEqual:`.

use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCopying, NSUInteger, NSZone};

#[derive(Default)]
pub(crate) struct DictionaryIvars {
    entries: Vec<(Retained<AnyObject>, Retained<AnyObject>)>,
}

impl DictionaryIvars {
    fn get(&self, key: &AnyObject) -> Option<&Retained<AnyObject>> {
        let hash: NSUInteger = unsafe { msg_send![key, hash] };
        self.entries.iter().find_map(|(k, v)| {
            let k_hash: NSUInteger = unsafe { msg_send![&**k, hash] };
            let equal = k_hash == hash && unsafe { msg_send![&**k, isEqual: key] };
            equal.then_some(v)
        })
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSDictionary"]
    #[ivars = DictionaryIvars]
    pub(crate) struct NSDictionaryImpl;

    impl NSDictionaryImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(DictionaryIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithObjects:forKeys:count:))]
        fn init_with_objects(
            this: Allocated<Self>,
            objects: *mut NonNull<AnyObject>,
            keys: *mut NonNull<ProtocolObject<dyn NSCopying>>,
            count: NSUInteger,
        ) -> Retained<Self> {
            let mut ivars = DictionaryIvars::default();
            for i in 0..count {
                // SAFETY: the caller passes `count` keys and objects.
                let (key, object) = unsafe { ((*keys.add(i)).as_ref(), (*objects.add(i)).as_ref()) };
                // Keys are copied, as Foundation promises.
                let key: Retained<AnyObject> = unsafe { msg_send![key, copy] };
                match ivars.entries.iter_mut().find(|(k, _)| unsafe { msg_send![&**k, isEqual: &*key] }) {
                    Some(entry) => entry.1 = object.retain(),
                    None => ivars.entries.push((key, object.retain())),
                }
            }
            let this = this.set_ivars(ivars);
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().entries.len()
        }

        #[unsafe(method_id(objectForKey:))]
        fn object_for_key(&self, key: &AnyObject) -> Option<Retained<AnyObject>> {
            self.ivars().get(key).cloned()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSDictionaryImpl {}
);
